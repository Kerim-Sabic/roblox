use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use nectarpilot_contracts::{
    ActionOutcome, ActionResult, Command, CommandEnvelope, DaemonEvent, EventEnvelope, Profile,
    RunSnapshot, RunState, StartMode,
};
use parking_lot::{Mutex, RwLock};
use serde_json::json;
use thiserror::Error;
use tokio::{
    sync::{broadcast, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    crash_guard::CrashLoopGuard,
    persistence::{SqliteStore, StoreError},
    reconnect::{ReconnectOutcome, ReconnectPolicy, run_bounded_reconnect},
    scheduler::{ScheduleError, TaskKey, TaskPermit, TaskScheduler},
};

#[derive(Debug, Clone)]
pub struct TaskContext {
    cancellation: CancellationToken,
    paused: watch::Receiver<bool>,
}

impl TaskContext {
    #[must_use]
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    /// Cooperative checkpoint used before every platform input operation.
    pub async fn checkpoint(&mut self) -> Result<(), AutomationError> {
        loop {
            if self.cancellation.is_cancelled() {
                return Err(AutomationError::Cancelled);
            }
            if !*self.paused.borrow() {
                return Ok(());
            }
            tokio::select! {
                () = self.cancellation.cancelled() => return Err(AutomationError::Cancelled),
                changed = self.paused.changed() => {
                    if changed.is_err() {
                        return Err(AutomationError::Cancelled);
                    }
                }
            }
        }
    }
}

#[async_trait]
pub trait AutomationBackend: Send + Sync + 'static {
    async fn preflight(&self, profile: &Profile, mode: StartMode) -> Result<(), AutomationError>;

    async fn execute(&self, profile: &Profile, context: TaskContext) -> ActionResult;

    async fn reconnect_attempt(
        &self,
        profile: &Profile,
        attempt: u8,
    ) -> Result<(), AutomationError>;

    /// Must be idempotent. It is invoked on normal completion, cancellation,
    /// panic recovery, and emergency stop.
    async fn release_all_inputs(&self) -> Result<(), AutomationError>;
}

pub struct AutomationEngine<B: AutomationBackend> {
    inner: Arc<EngineInner<B>>,
}

impl<B: AutomationBackend> Clone for AutomationEngine<B> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

struct EngineInner<B: AutomationBackend> {
    backend: Arc<B>,
    store: Arc<SqliteStore>,
    state: RwLock<RunState>,
    run_id: RwLock<Uuid>,
    profile_id: RwLock<Option<Uuid>>,
    active_task: RwLock<Option<String>>,
    sequence: AtomicU64,
    events: broadcast::Sender<EventEnvelope>,
    cancellation: Mutex<Option<CancellationToken>>,
    pause_sender: Mutex<Option<watch::Sender<bool>>>,
    worker: tokio::sync::Mutex<Option<JoinHandle<()>>>,
    scheduler: TaskScheduler,
    reconnect_policy: RwLock<ReconnectPolicy>,
    crash_guard: Mutex<CrashLoopGuard>,
    safe_mode: AtomicBool,
}

impl<B: AutomationBackend> AutomationEngine<B> {
    pub fn new(backend: Arc<B>, store: Arc<SqliteStore>) -> Result<Self, AutomationError> {
        let (events, _) = broadcast::channel(512);
        let mut crash_guard = CrashLoopGuard::default();
        if let Some(serialized) = store.runtime_value("daemon_crash_timestamps")? {
            let timestamps: Vec<DateTime<Utc>> =
                serde_json::from_str(&serialized).unwrap_or_default();
            for timestamp in timestamps {
                crash_guard.record(timestamp);
            }
        }
        let safe_mode = crash_guard.is_tripped()
            || store
                .runtime_value("safe_mode")?
                .is_some_and(|value| value == "true");
        Ok(Self {
            inner: Arc::new(EngineInner {
                backend,
                store,
                state: RwLock::new(RunState::Idle),
                run_id: RwLock::new(Uuid::now_v7()),
                profile_id: RwLock::new(None),
                active_task: RwLock::new(None),
                sequence: AtomicU64::new(0),
                events,
                cancellation: Mutex::new(None),
                pause_sender: Mutex::new(None),
                worker: tokio::sync::Mutex::new(None),
                scheduler: TaskScheduler::default(),
                reconnect_policy: RwLock::new(ReconnectPolicy::default()),
                crash_guard: Mutex::new(crash_guard),
                safe_mode: AtomicBool::new(safe_mode),
            }),
        })
    }

    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<EventEnvelope> {
        self.inner.events.subscribe()
    }

    #[must_use]
    pub fn state(&self) -> RunState {
        *self.inner.state.read()
    }

    #[must_use]
    pub fn snapshot(&self) -> RunSnapshot {
        RunSnapshot {
            run_id: *self.inner.run_id.read(),
            profile_id: self.inner.profile_id.read().unwrap_or_else(Uuid::nil),
            state: self.state(),
            safe_mode: self.inner.safe_mode.load(Ordering::SeqCst),
            active_task: self.inner.active_task.read().clone(),
            last_sequence: self.inner.sequence.load(Ordering::SeqCst),
        }
    }

    pub fn set_reconnect_policy(&self, policy: ReconnectPolicy) {
        *self.inner.reconnect_policy.write() = policy;
    }

    pub async fn handle_command(&self, envelope: CommandEnvelope) -> Result<(), AutomationError> {
        if let Err(error) = envelope.validate_version() {
            let reason = error.to_string();
            self.emit(DaemonEvent::CommandRejected {
                request_id: envelope.request_id,
                reason: reason.clone(),
            });
            return Err(AutomationError::InvalidCommand(reason));
        }

        let request_id = envelope.request_id;
        let result = self.dispatch(envelope).await;
        match &result {
            Ok(()) => self.emit(DaemonEvent::CommandAccepted { request_id }),
            Err(error) => self.emit(DaemonEvent::CommandRejected {
                request_id,
                reason: error.to_string(),
            }),
        }
        result
    }

    async fn dispatch(&self, envelope: CommandEnvelope) -> Result<(), AutomationError> {
        match envelope.command {
            Command::Start { mode } => self.start(envelope.profile_id, mode).await,
            Command::Pause => self.pause(),
            Command::Resume => self.resume(),
            Command::Stop => self.stop(false).await,
            Command::EmergencyStop | Command::ShutdownDaemon => self.stop(true).await,
            Command::GetSnapshot => {
                self.emit(DaemonEvent::Snapshot(self.snapshot()));
                Ok(())
            }
            Command::SaveProfile { profile } => {
                if envelope.profile_id != profile.id {
                    return Err(AutomationError::InvalidCommand(
                        "envelope profile_id does not match profile document".into(),
                    ));
                }
                self.inner.store.save_profile(&profile)?;
                self.emit(DaemonEvent::ProfileSaved {
                    profile_id: profile.id,
                });
                Ok(())
            }
            Command::DeleteProfile => {
                self.inner.store.delete_profile(envelope.profile_id)?;
                self.emit(DaemonEvent::ProfileDeleted {
                    profile_id: envelope.profile_id,
                });
                Ok(())
            }
            Command::ExportProfile => {
                let json = self.inner.store.export_profile_json(envelope.profile_id)?;
                self.emit(DaemonEvent::ProfileExported {
                    profile_id: envelope.profile_id,
                    json,
                });
                Ok(())
            }
            Command::AcknowledgeAttention => {
                if !matches!(self.state(), RunState::NeedsAttention | RunState::Faulted) {
                    return Err(AutomationError::InvalidState {
                        current: self.state(),
                        command: "acknowledge_attention",
                    });
                }
                self.inner.safe_mode.store(false, Ordering::SeqCst);
                self.inner.store.set_runtime_value("safe_mode", "false")?;
                self.transition(RunState::Idle, "attention acknowledged");
                Ok(())
            }
        }
    }

    async fn start(&self, profile_id: Uuid, mode: StartMode) -> Result<(), AutomationError> {
        if self.inner.safe_mode.load(Ordering::SeqCst) {
            return Err(AutomationError::SafeMode);
        }
        if self.state() != RunState::Idle {
            return Err(AutomationError::InvalidState {
                current: self.state(),
                command: "start",
            });
        }
        let profile = self
            .inner
            .store
            .load_profile(profile_id)?
            .ok_or(StoreError::ProfileNotFound(profile_id))?;
        let permit = self.inner.scheduler.acquire(TaskKey {
            profile_id,
            task_name: "automation_run".into(),
        })?;

        let mut worker = self.inner.worker.lock().await;
        if worker.as_ref().is_some_and(|handle| !handle.is_finished()) {
            return Err(AutomationError::WorkerAlreadyRunning);
        }
        worker.take();

        let cancellation = CancellationToken::new();
        let (pause_sender, pause_receiver) = watch::channel(false);
        *self.inner.cancellation.lock() = Some(cancellation.clone());
        *self.inner.pause_sender.lock() = Some(pause_sender);
        *self.inner.profile_id.write() = Some(profile_id);
        *self.inner.run_id.write() = Uuid::now_v7();
        *self.inner.active_task.write() = Some("preflight".into());
        self.transition(RunState::Preflight, "start requested");

        let engine = self.clone();
        let context = TaskContext {
            cancellation,
            paused: pause_receiver,
        };
        *worker = Some(tokio::spawn(async move {
            engine.run_worker(profile, mode, context, permit).await;
        }));
        Ok(())
    }

    fn pause(&self) -> Result<(), AutomationError> {
        if self.state() != RunState::Running {
            return Err(AutomationError::InvalidState {
                current: self.state(),
                command: "pause",
            });
        }
        let sender = self
            .inner
            .pause_sender
            .lock()
            .clone()
            .ok_or(AutomationError::WorkerNotRunning)?;
        sender
            .send(true)
            .map_err(|_| AutomationError::WorkerNotRunning)?;
        self.transition(RunState::Paused, "pause requested");
        Ok(())
    }

    fn resume(&self) -> Result<(), AutomationError> {
        if self.state() != RunState::Paused {
            return Err(AutomationError::InvalidState {
                current: self.state(),
                command: "resume",
            });
        }
        let sender = self
            .inner
            .pause_sender
            .lock()
            .clone()
            .ok_or(AutomationError::WorkerNotRunning)?;
        sender
            .send(false)
            .map_err(|_| AutomationError::WorkerNotRunning)?;
        self.transition(RunState::Running, "resume requested");
        Ok(())
    }

    async fn stop(&self, emergency: bool) -> Result<(), AutomationError> {
        if let Some(cancellation) = self.inner.cancellation.lock().as_ref() {
            cancellation.cancel();
        }
        if self.state() != RunState::Idle {
            self.transition(
                RunState::Stopping,
                if emergency {
                    "emergency stop"
                } else {
                    "stop requested"
                },
            );
        }
        if emergency {
            self.inner.backend.release_all_inputs().await?;
        }
        if self.inner.cancellation.lock().is_none() {
            self.transition(RunState::Idle, "nothing was running");
        }
        Ok(())
    }

    async fn run_worker(
        &self,
        profile: Profile,
        mode: StartMode,
        context: TaskContext,
        _permit: TaskPermit,
    ) {
        let started_at = Utc::now();
        let preflight = tokio::select! {
            () = context.cancellation.cancelled() => Err(AutomationError::Cancelled),
            result = self.inner.backend.preflight(&profile, mode) => result,
        };
        if let Err(error) = preflight {
            let cancelled = matches!(error, AutomationError::Cancelled);
            self.emit(DaemonEvent::ActionCompleted(action_result(
                "preflight",
                if cancelled {
                    ActionOutcome::Cancelled
                } else {
                    ActionOutcome::Failed
                },
                started_at,
                error.to_string(),
            )));
            let _ = self.inner.backend.release_all_inputs().await;
            self.finish_worker(
                if cancelled {
                    RunState::Idle
                } else {
                    RunState::Faulted
                },
                "preflight ended",
            );
            return;
        }

        *self.inner.active_task.write() = Some("automation".into());
        self.transition(RunState::Running, "preflight passed");
        let result = tokio::select! {
            () = context.cancellation.cancelled() => action_result(
                "automation",
                ActionOutcome::Cancelled,
                started_at,
                "cancelled",
            ),
            result = self.inner.backend.execute(&profile, context.clone()) => result,
        };
        self.emit(DaemonEvent::ActionCompleted(result.clone()));

        let mut final_state = match result.outcome {
            ActionOutcome::Succeeded | ActionOutcome::Skipped | ActionOutcome::Cancelled => {
                RunState::Idle
            }
            ActionOutcome::NeedsAttention => RunState::NeedsAttention,
            ActionOutcome::Failed => RunState::Faulted,
        };
        let mut reason = result.message.clone();

        if result.outcome == ActionOutcome::Failed
            && profile.automation.reconnect_enabled
            && !context.cancellation.is_cancelled()
        {
            *self.inner.active_task.write() = Some("reconnect".into());
            self.transition(RunState::Recovering, "automation failed; reconnecting");
            let policy = self.inner.reconnect_policy.read().clone();
            let engine = self.clone();
            let backend = Arc::clone(&self.inner.backend);
            let reconnect = run_bounded_reconnect(
                &policy,
                &context.cancellation,
                |attempt| {
                    let backend = Arc::clone(&backend);
                    let profile = profile.clone();
                    async move {
                        backend
                            .reconnect_attempt(&profile, attempt)
                            .await
                            .map_err(|error| error.to_string())
                    }
                },
                move |progress| engine.emit(DaemonEvent::ReconnectProgress(progress)),
            )
            .await;
            match reconnect {
                ReconnectOutcome::Succeeded { attempt } => {
                    final_state = RunState::Idle;
                    reason = format!("reconnected on attempt {attempt}");
                }
                ReconnectOutcome::Cancelled { .. } => {
                    final_state = RunState::Idle;
                    reason = "reconnect cancelled".into();
                }
                ReconnectOutcome::Exhausted { attempts, .. } => {
                    final_state = RunState::NeedsAttention;
                    reason = format!("reconnect exhausted after {attempts} attempts");
                }
                ReconnectOutcome::DeadlineExceeded { attempts } => {
                    final_state = RunState::NeedsAttention;
                    reason = format!("reconnect deadline exceeded after {attempts} attempts");
                }
            }
        }

        if let Err(error) = self.inner.backend.release_all_inputs().await {
            final_state = RunState::Faulted;
            reason = format!("failed to release inputs: {error}");
        }
        self.finish_worker(final_state, &reason);
    }

    fn finish_worker(&self, final_state: RunState, reason: &str) {
        *self.inner.active_task.write() = None;
        *self.inner.cancellation.lock() = None;
        *self.inner.pause_sender.lock() = None;
        self.transition(final_state, reason);
    }

    fn transition(&self, current: RunState, reason: &str) {
        let previous = {
            let mut state = self.inner.state.write();
            let previous = *state;
            *state = current;
            previous
        };
        if previous != current {
            self.emit(DaemonEvent::StateChanged {
                previous,
                current,
                reason: reason.into(),
            });
        }
    }

    fn emit(&self, event: DaemonEvent) {
        let sequence = self.inner.sequence.fetch_add(1, Ordering::SeqCst) + 1;
        let envelope = EventEnvelope::new(sequence, *self.inner.run_id.read(), event);
        if let Err(error) = self.inner.store.append_event(&envelope) {
            tracing::error!(%error, sequence, "failed to persist daemon event");
        }
        tracing::info!(sequence, event = ?envelope.event, "daemon event");
        let _ = self.inner.events.send(envelope);
    }

    /// Called by the daemon supervisor when the prior process exited uncleanly.
    /// Crash history is persisted so the third crash survives process restarts.
    pub fn record_daemon_crash(&self, occurred_at: DateTime<Utc>) -> Result<bool, AutomationError> {
        let (tripped, count, timestamps) = {
            let mut guard = self.inner.crash_guard.lock();
            let tripped = guard.record(occurred_at);
            (tripped, guard.recent_count(), guard.timestamps())
        };
        self.inner.store.set_runtime_value(
            "daemon_crash_timestamps",
            &serde_json::to_string(&timestamps)?,
        )?;
        if tripped {
            self.inner.safe_mode.store(true, Ordering::SeqCst);
            self.inner.store.set_runtime_value("safe_mode", "true")?;
            self.emit(DaemonEvent::SafeModeEntered {
                crash_count: count,
                window_seconds: 10 * 60,
            });
        }
        Ok(tripped)
    }
}

fn action_result(
    action: &str,
    outcome: ActionOutcome,
    started_at: DateTime<Utc>,
    message: impl Into<String>,
) -> ActionResult {
    ActionResult {
        action: action.into(),
        outcome,
        started_at,
        finished_at: Utc::now(),
        message: message.into(),
        details: json!({}),
    }
}

#[derive(Debug, Clone, Error)]
pub enum AutomationError {
    #[error("operation was cancelled")]
    Cancelled,
    #[error("preflight failed: {0}")]
    Preflight(String),
    #[error("automation backend failed: {0}")]
    Backend(String),
    #[error("invalid command: {0}")]
    InvalidCommand(String),
    #[error("cannot run {command} while state is {current:?}")]
    InvalidState {
        current: RunState,
        command: &'static str,
    },
    #[error("safe mode is active; acknowledge diagnostics before starting")]
    SafeMode,
    #[error("automation worker is already running")]
    WorkerAlreadyRunning,
    #[error("automation worker is not running")]
    WorkerNotRunning,
    #[error("persistence error: {0}")]
    Store(String),
    #[error("scheduler error: {0}")]
    Scheduler(String),
    #[error("serialization error: {0}")]
    Serialization(String),
}

impl From<StoreError> for AutomationError {
    fn from(error: StoreError) -> Self {
        Self::Store(error.to_string())
    }
}

impl From<ScheduleError> for AutomationError {
    fn from(error: ScheduleError) -> Self {
        Self::Scheduler(error.to_string())
    }
}

impl From<serde_json::Error> for AutomationError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialization(error.to_string())
    }
}

/// Deterministic backend used by the desktop mock mode and core tests.
#[derive(Debug)]
pub struct MockBackend {
    preflight_error: Mutex<Option<AutomationError>>,
    normal_mode_block: Mutex<Option<String>>,
    outcome: Mutex<ActionOutcome>,
    reconnect_failures_remaining: AtomicUsize,
    releases: AtomicUsize,
    execution_delay: RwLock<Duration>,
}

impl Default for MockBackend {
    fn default() -> Self {
        Self {
            preflight_error: Mutex::new(None),
            normal_mode_block: Mutex::new(None),
            outcome: Mutex::new(ActionOutcome::Succeeded),
            reconnect_failures_remaining: AtomicUsize::new(0),
            releases: AtomicUsize::new(0),
            execution_delay: RwLock::new(Duration::from_millis(5)),
        }
    }
}

impl MockBackend {
    /// Prevents a test backend from being mistaken for live automation.
    /// Dry-run and diagnostics modes remain available for UI integration.
    pub fn block_normal_mode(&self, reason: impl Into<String>) {
        *self.normal_mode_block.lock() = Some(reason.into());
    }

    pub fn set_outcome(&self, outcome: ActionOutcome) {
        *self.outcome.lock() = outcome;
    }

    pub fn set_reconnect_failures(&self, failures: usize) {
        self.reconnect_failures_remaining
            .store(failures, Ordering::SeqCst);
    }

    pub fn set_execution_delay(&self, delay: Duration) {
        *self.execution_delay.write() = delay;
    }

    #[must_use]
    pub fn release_count(&self) -> usize {
        self.releases.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl AutomationBackend for MockBackend {
    async fn preflight(&self, _profile: &Profile, mode: StartMode) -> Result<(), AutomationError> {
        if mode == StartMode::Normal
            && let Some(reason) = self.normal_mode_block.lock().clone()
        {
            return Err(AutomationError::Backend(reason));
        }
        self.preflight_error.lock().clone().map_or(Ok(()), Err)
    }

    async fn execute(&self, _profile: &Profile, mut context: TaskContext) -> ActionResult {
        let started_at = Utc::now();
        if context.checkpoint().await.is_err() {
            return action_result(
                "mock_automation",
                ActionOutcome::Cancelled,
                started_at,
                "cancelled",
            );
        }
        tokio::select! {
            () = context.cancellation.cancelled() => action_result(
                "mock_automation",
                ActionOutcome::Cancelled,
                started_at,
                "cancelled",
            ),
            () = tokio::time::sleep(*self.execution_delay.read()) => action_result(
                "mock_automation",
                *self.outcome.lock(),
                started_at,
                "mock execution finished",
            ),
        }
    }

    async fn reconnect_attempt(
        &self,
        _profile: &Profile,
        _attempt: u8,
    ) -> Result<(), AutomationError> {
        self.reconnect_failures_remaining
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .map_or(Ok(()), |_| {
                Err(AutomationError::Backend("mock reconnect failure".into()))
            })
    }

    async fn release_all_inputs(&self) -> Result<(), AutomationError> {
        self.releases.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use chrono::Utc;
    use nectarpilot_contracts::{
        ActionOutcome, Command, CommandEnvelope, Profile, RunState, StartMode,
    };
    use tempfile::tempdir;

    use crate::{persistence::SqliteStore, reconnect::ReconnectPolicy};

    use super::{AutomationEngine, MockBackend};

    #[tokio::test]
    async fn production_guard_blocks_mock_normal_mode_only() {
        let backend = MockBackend::default();
        backend.block_normal_mode("live backend unavailable");
        let profile = Profile::new("guarded");

        assert!(
            super::AutomationBackend::preflight(&backend, &profile, StartMode::Normal)
                .await
                .is_err()
        );
        assert!(
            super::AutomationBackend::preflight(&backend, &profile, StartMode::DryRun)
                .await
                .is_ok()
        );
    }

    async fn wait_for_state(engine: &AutomationEngine<MockBackend>, target: RunState) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if engine.state() == target {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("state transition timed out");
    }

    #[tokio::test]
    async fn emergency_stop_releases_inputs() {
        let directory = tempdir().expect("temp directory");
        let store =
            Arc::new(SqliteStore::open(directory.path().join("db.sqlite3")).expect("store"));
        let profile = Profile::new("test");
        store.save_profile(&profile).expect("profile");
        let backend = Arc::new(MockBackend::default());
        backend.set_execution_delay(Duration::from_secs(30));
        let engine = AutomationEngine::new(Arc::clone(&backend), store).expect("engine");

        engine
            .handle_command(CommandEnvelope::new(
                profile.id,
                Command::Start {
                    mode: StartMode::Normal,
                },
            ))
            .await
            .expect("start");
        wait_for_state(&engine, RunState::Running).await;
        engine
            .handle_command(CommandEnvelope::new(profile.id, Command::EmergencyStop))
            .await
            .expect("emergency stop");
        wait_for_state(&engine, RunState::Idle).await;
        assert!(backend.release_count() >= 1);
    }

    #[tokio::test]
    async fn daemon_shutdown_releases_inputs_before_exit() {
        let directory = tempdir().expect("temp directory");
        let store =
            Arc::new(SqliteStore::open(directory.path().join("db.sqlite3")).expect("store"));
        let profile = Profile::new("test");
        store.save_profile(&profile).expect("profile");
        let backend = Arc::new(MockBackend::default());
        backend.set_execution_delay(Duration::from_secs(30));
        let engine = AutomationEngine::new(Arc::clone(&backend), store).expect("engine");

        engine
            .handle_command(CommandEnvelope::new(
                profile.id,
                Command::Start {
                    mode: StartMode::Normal,
                },
            ))
            .await
            .expect("start");
        wait_for_state(&engine, RunState::Running).await;
        engine
            .handle_command(CommandEnvelope::new(profile.id, Command::ShutdownDaemon))
            .await
            .expect("daemon shutdown");
        wait_for_state(&engine, RunState::Idle).await;
        assert!(backend.release_count() >= 1);
    }

    #[tokio::test]
    async fn reconnect_exhaustion_enters_needs_attention() {
        let directory = tempdir().expect("temp directory");
        let store =
            Arc::new(SqliteStore::open(directory.path().join("db.sqlite3")).expect("store"));
        let profile = Profile::new("test");
        store.save_profile(&profile).expect("profile");
        let backend = Arc::new(MockBackend::default());
        backend.set_outcome(ActionOutcome::Failed);
        backend.set_reconnect_failures(5);
        let engine = AutomationEngine::new(backend, store).expect("engine");
        engine.set_reconnect_policy(ReconnectPolicy::new(
            5,
            Duration::from_secs(1),
            vec![Duration::ZERO],
        ));

        engine
            .handle_command(CommandEnvelope::new(
                profile.id,
                Command::Start {
                    mode: StartMode::Normal,
                },
            ))
            .await
            .expect("start");
        wait_for_state(&engine, RunState::NeedsAttention).await;
    }

    #[test]
    fn crash_loop_safe_mode_survives_engine_restart() {
        let directory = tempdir().expect("temp directory");
        let store = Arc::new(
            SqliteStore::open(directory.path().join("db.sqlite3")).expect("persistent store"),
        );
        let now = Utc::now();
        {
            let engine =
                AutomationEngine::new(Arc::new(MockBackend::default()), Arc::clone(&store))
                    .expect("engine");
            assert!(!engine.record_daemon_crash(now).expect("first crash"));
            assert!(!engine.record_daemon_crash(now).expect("second crash"));
            assert!(engine.record_daemon_crash(now).expect("third crash"));
        }

        let restarted = AutomationEngine::new(Arc::new(MockBackend::default()), store)
            .expect("restarted engine");
        assert!(restarted.snapshot().safe_mode);
    }
}
