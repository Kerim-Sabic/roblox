use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::FutureExt;
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
    /// Creates a runnable context for embedders and deterministic task tests.
    #[must_use]
    pub fn unpaused(cancellation: CancellationToken) -> Self {
        let (_sender, receiver) = watch::channel(false);
        Self {
            cancellation,
            paused: receiver,
        }
    }

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

/// Daemon-owned boundary for the explicit legacy `AutoHotkey` compatibility
/// bridge. It is intentionally separate from [`AutomationBackend`]: normal
/// native backends never gain arbitrary-script execution capability merely by
/// implementing automation input.
#[async_trait]
pub trait LegacyExecutionPort: Send + Sync + 'static {
    /// Verifies an allowlisted asset, its exact consent digest, and all policy
    /// preconditions before a compatibility worker is allowed to start.
    async fn preflight(
        &self,
        profile: &Profile,
        script_id: &str,
        approved_sha256: &str,
    ) -> Result<(), AutomationError>;

    /// Runs one already-preflighted compatibility asset. Implementations must
    /// observe [`TaskContext::cancellation_token`] and contain the child.
    async fn execute(
        &self,
        profile: &Profile,
        script_id: &str,
        approved_sha256: &str,
        context: TaskContext,
    ) -> ActionResult;

    /// Must be idempotent and terminate only the exact compatibility child or
    /// job owned by this port.
    async fn cancel(&self) -> Result<(), AutomationError>;
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
    legacy_port: RwLock<Option<Arc<dyn LegacyExecutionPort>>>,
    reconnect_policy: RwLock<ReconnectPolicy>,
    crash_guard: Mutex<CrashLoopGuard>,
    safe_mode: AtomicBool,
}

impl<B: AutomationBackend> AutomationEngine<B> {
    pub fn new(backend: Arc<B>, store: Arc<SqliteStore>) -> Result<Self, AutomationError> {
        let (events, _) = broadcast::channel(512);
        let selected_profile_id = initialize_profile_selection(&store)?;
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
                profile_id: RwLock::new(Some(selected_profile_id)),
                active_task: RwLock::new(None),
                sequence: AtomicU64::new(0),
                events,
                cancellation: Mutex::new(None),
                pause_sender: Mutex::new(None),
                worker: tokio::sync::Mutex::new(None),
                scheduler: TaskScheduler::default(),
                legacy_port: RwLock::new(None),
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

    /// Installs the daemon-owned legacy bridge. The port is optional so core
    /// tests and native-only deployments remain incapable of script execution.
    pub fn install_legacy_port(&self, port: Arc<dyn LegacyExecutionPort>) {
        *self.inner.legacy_port.write() = Some(port);
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
        let shutdown_requested = matches!(&envelope.command, Command::ShutdownDaemon);
        let result = self.dispatch(envelope).await;
        match &result {
            Ok(()) => {
                self.emit(DaemonEvent::CommandAccepted { request_id });
                if shutdown_requested {
                    self.emit(DaemonEvent::ShutdownReady { request_id });
                }
            }
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
            Command::GetProfiles => {
                self.emit(DaemonEvent::Profiles {
                    profiles: self.inner.store.list_profiles()?,
                    selected_profile_id: self.inner.profile_id.read().unwrap_or_else(Uuid::nil),
                });
                Ok(())
            }
            Command::SelectProfile => self.select_profile(envelope.profile_id),
            Command::StartLegacy {
                script_id,
                approved_sha256,
            } => {
                self.start_legacy(envelope.profile_id, script_id, approved_sha256)
                    .await
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
                if self.state() != RunState::Idle {
                    return Err(AutomationError::InvalidState {
                        current: self.state(),
                        command: "delete_profile",
                    });
                }
                if self.inner.profile_id.read().as_ref() == Some(&envelope.profile_id) {
                    return Err(AutomationError::InvalidCommand(
                        "select a different profile before deleting this profile".into(),
                    ));
                }
                if self.inner.store.list_profiles()?.len() <= 1 {
                    return Err(AutomationError::InvalidCommand(
                        "the safe default/last profile cannot be deleted".into(),
                    ));
                }
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

    fn select_profile(&self, profile_id: Uuid) -> Result<(), AutomationError> {
        if self.state() != RunState::Idle {
            return Err(AutomationError::InvalidState {
                current: self.state(),
                command: "select_profile",
            });
        }
        self.inner
            .store
            .load_profile(profile_id)?
            .ok_or(StoreError::ProfileNotFound(profile_id))?;
        self.inner
            .store
            .set_runtime_value("selected_profile_id", &profile_id.to_string())?;
        *self.inner.profile_id.write() = Some(profile_id);
        self.emit(DaemonEvent::ProfileSelected { profile_id });
        self.emit(DaemonEvent::Snapshot(self.snapshot()));
        Ok(())
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
        self.inner
            .store
            .set_runtime_value("selected_profile_id", &profile_id.to_string())?;
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

    async fn start_legacy(
        &self,
        profile_id: Uuid,
        script_id: String,
        approved_sha256: String,
    ) -> Result<(), AutomationError> {
        validate_legacy_reference(&script_id, &approved_sha256)?;
        if self.inner.safe_mode.load(Ordering::SeqCst) {
            return Err(AutomationError::SafeMode);
        }
        if self.state() != RunState::Idle {
            return Err(AutomationError::InvalidState {
                current: self.state(),
                command: "start_legacy",
            });
        }
        let port = self
            .inner
            .legacy_port
            .read()
            .clone()
            .ok_or(AutomationError::LegacyUnavailable)?;
        let profile = self
            .inner
            .store
            .load_profile(profile_id)?
            .ok_or(StoreError::ProfileNotFound(profile_id))?;
        self.inner
            .store
            .set_runtime_value("selected_profile_id", &profile_id.to_string())?;
        let permit = self.inner.scheduler.acquire(TaskKey {
            profile_id,
            task_name: "legacy_compatibility_run".into(),
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
        *self.inner.active_task.write() = Some(format!("legacy-preflight:{script_id}"));
        self.transition(RunState::Preflight, "legacy compatibility start requested");

        let engine = self.clone();
        let context = TaskContext {
            cancellation,
            paused: pause_receiver,
        };
        *worker = Some(tokio::spawn(async move {
            engine
                .run_legacy_worker(profile, script_id, approved_sha256, port, context, permit)
                .await;
        }));
        Ok(())
    }

    fn pause(&self) -> Result<(), AutomationError> {
        if self
            .inner
            .active_task
            .read()
            .as_deref()
            .is_some_and(|task| task.starts_with("legacy:"))
        {
            return Err(AutomationError::InvalidCommand(
                "legacy compatibility scripts cannot pause safely; stop them instead".into(),
            ));
        }
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
        let legacy_running = self
            .inner
            .active_task
            .read()
            .as_deref()
            .is_some_and(|task| task.starts_with("legacy"));
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
        let legacy_port = legacy_running
            .then(|| self.inner.legacy_port.read().clone())
            .flatten();
        if let Some(port) = legacy_port {
            port.cancel().await?;
        }
        if self.inner.cancellation.lock().is_none() {
            self.transition(RunState::Idle, "nothing was running");
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)] // Linear lifecycle and cleanup ordering is safety-critical.
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
            result = std::panic::AssertUnwindSafe(self.inner.backend.preflight(&profile, mode)).catch_unwind() => {
                result.unwrap_or_else(|_| Err(AutomationError::Backend("preflight worker panicked".into())))
            },
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
            result = std::panic::AssertUnwindSafe(self.inner.backend.execute(&profile, context.clone())).catch_unwind() => {
                result.unwrap_or_else(|_| action_result(
                    "automation",
                    ActionOutcome::Failed,
                    started_at,
                    "automation worker panicked",
                ))
            },
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

    #[allow(clippy::too_many_lines)] // Compatibility lifecycle mirrors native cleanup explicitly.
    async fn run_legacy_worker(
        &self,
        profile: Profile,
        script_id: String,
        approved_sha256: String,
        port: Arc<dyn LegacyExecutionPort>,
        context: TaskContext,
        _permit: TaskPermit,
    ) {
        let started_at = Utc::now();
        let preflight = tokio::select! {
            () = context.cancellation.cancelled() => Err(AutomationError::Cancelled),
            result = std::panic::AssertUnwindSafe(port.preflight(&profile, &script_id, &approved_sha256)).catch_unwind() => {
                result.unwrap_or_else(|_| Err(AutomationError::Backend("legacy preflight worker panicked".into())))
            },
        };
        if let Err(error) = preflight {
            let cancelled = matches!(error, AutomationError::Cancelled);
            self.emit(DaemonEvent::ActionCompleted(action_result(
                &format!("legacy_preflight:{script_id}"),
                if cancelled {
                    ActionOutcome::Cancelled
                } else {
                    ActionOutcome::Failed
                },
                started_at,
                error.to_string(),
            )));
            let _ = port.cancel().await;
            let _ = self.inner.backend.release_all_inputs().await;
            self.finish_worker(
                if cancelled {
                    RunState::Idle
                } else {
                    RunState::Faulted
                },
                "legacy preflight ended",
            );
            return;
        }

        *self.inner.active_task.write() = Some(format!("legacy:{script_id}"));
        self.transition(RunState::Running, "legacy compatibility preflight passed");
        let action = format!("legacy:{script_id}");
        let result = tokio::select! {
            () = context.cancellation.cancelled() => action_result(
                &action,
                ActionOutcome::Cancelled,
                started_at,
                "legacy compatibility run cancelled",
            ),
            result = std::panic::AssertUnwindSafe(port.execute(&profile, &script_id, &approved_sha256, context.clone())).catch_unwind() => {
                result.unwrap_or_else(|_| action_result(
                    &action,
                    ActionOutcome::Failed,
                    started_at,
                    "legacy compatibility worker panicked",
                ))
            },
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
        if let Err(error) = port.cancel().await {
            final_state = RunState::Faulted;
            reason = format!("legacy compatibility cleanup failed: {error}");
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

fn initialize_profile_selection(store: &SqliteStore) -> Result<Uuid, AutomationError> {
    let mut profiles = store.list_profiles()?;
    if profiles.is_empty() {
        let profile = Profile::new("Default (Safe)");
        store.save_profile(&profile)?;
        profiles.push(profile);
    }
    let selected = store
        .runtime_value("selected_profile_id")?
        .and_then(|value| Uuid::parse_str(&value).ok())
        .filter(|id| profiles.iter().any(|profile| profile.id == *id))
        .unwrap_or(profiles[0].id);
    store.set_runtime_value("selected_profile_id", &selected.to_string())?;
    Ok(selected)
}

fn validate_legacy_reference(
    script_id: &str,
    approved_sha256: &str,
) -> Result<(), AutomationError> {
    if !script_id.starts_with("legacy:")
        || script_id.len() > 128
        || script_id.len() <= "legacy:".len()
        || script_id.chars().any(char::is_control)
    {
        return Err(AutomationError::InvalidCommand(
            "legacy script identifier is invalid".into(),
        ));
    }
    let digest = approved_sha256
        .strip_prefix("sha256:")
        .unwrap_or(approved_sha256);
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(AutomationError::InvalidCommand(
            "legacy script requires a complete SHA-256 consent digest".into(),
        ));
    }
    Ok(())
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
    #[error("legacy compatibility execution is unavailable in this daemon")]
    LegacyUnavailable,
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
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use async_trait::async_trait;
    use chrono::Utc;
    use nectarpilot_contracts::{
        ActionOutcome, ActionResult, Command, CommandEnvelope, DaemonEvent, Profile, RunState,
        StartMode,
    };
    use tempfile::tempdir;

    use crate::{persistence::SqliteStore, reconnect::ReconnectPolicy};

    use super::{
        AutomationBackend, AutomationEngine, AutomationError, LegacyExecutionPort, MockBackend,
        TaskContext,
    };

    struct PanicBackend {
        releases: AtomicUsize,
    }

    struct MockLegacyPort {
        starts: AtomicUsize,
        cancels: AtomicUsize,
    }

    #[async_trait]
    impl LegacyExecutionPort for MockLegacyPort {
        async fn preflight(
            &self,
            _profile: &Profile,
            _script_id: &str,
            _approved_sha256: &str,
        ) -> Result<(), AutomationError> {
            Ok(())
        }

        async fn execute(
            &self,
            _profile: &Profile,
            script_id: &str,
            _approved_sha256: &str,
            context: TaskContext,
        ) -> ActionResult {
            self.starts.fetch_add(1, Ordering::SeqCst);
            let started_at = Utc::now();
            let cancellation = context.cancellation_token();
            tokio::select! {
                () = cancellation.cancelled() => super::action_result(
                    &format!("legacy:{script_id}"),
                    ActionOutcome::Cancelled,
                    started_at,
                    "fixture legacy run cancelled",
                ),
                () = tokio::time::sleep(Duration::from_secs(30)) => super::action_result(
                    &format!("legacy:{script_id}"),
                    ActionOutcome::Succeeded,
                    started_at,
                    "fixture legacy run completed",
                ),
            }
        }

        async fn cancel(&self) -> Result<(), AutomationError> {
            self.cancels.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[async_trait]
    impl AutomationBackend for PanicBackend {
        async fn preflight(
            &self,
            _profile: &Profile,
            _mode: StartMode,
        ) -> Result<(), AutomationError> {
            Ok(())
        }

        async fn execute(&self, _profile: &Profile, _context: TaskContext) -> ActionResult {
            panic!("intentional worker panic fixture")
        }

        async fn reconnect_attempt(
            &self,
            _profile: &Profile,
            _attempt: u8,
        ) -> Result<(), AutomationError> {
            Err(AutomationError::Backend("disabled in fixture".into()))
        }

        async fn release_all_inputs(&self) -> Result<(), AutomationError> {
            self.releases.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

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

    async fn wait_for_state<B: AutomationBackend>(engine: &AutomationEngine<B>, target: RunState) {
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
    async fn legacy_command_requires_an_installed_daemon_port() {
        let directory = tempdir().expect("temp directory");
        let store =
            Arc::new(SqliteStore::open(directory.path().join("db.sqlite3")).expect("store"));
        let profile = Profile::new("test");
        store.save_profile(&profile).expect("profile");
        let engine =
            AutomationEngine::new(Arc::new(MockBackend::default()), store).expect("engine");

        let error = engine
            .handle_command(CommandEnvelope::new(
                profile.id,
                Command::StartLegacy {
                    script_id: "legacy:route:paths/gtf-sunflower.ahk".into(),
                    approved_sha256: "a".repeat(64),
                },
            ))
            .await
            .expect_err("missing legacy port must fail closed");
        assert!(matches!(error, AutomationError::LegacyUnavailable));
    }

    #[tokio::test]
    async fn stopping_legacy_run_cancels_exact_legacy_port_and_releases_inputs() {
        let directory = tempdir().expect("temp directory");
        let store =
            Arc::new(SqliteStore::open(directory.path().join("db.sqlite3")).expect("store"));
        let profile = Profile::new("test");
        store.save_profile(&profile).expect("profile");
        let backend = Arc::new(MockBackend::default());
        let engine = AutomationEngine::new(Arc::clone(&backend), store).expect("engine");
        let legacy = Arc::new(MockLegacyPort {
            starts: AtomicUsize::new(0),
            cancels: AtomicUsize::new(0),
        });
        engine.install_legacy_port(Arc::clone(&legacy) as Arc<dyn LegacyExecutionPort>);

        engine
            .handle_command(CommandEnvelope::new(
                profile.id,
                Command::StartLegacy {
                    script_id: "legacy:route:paths/gtf-sunflower.ahk".into(),
                    approved_sha256: "a".repeat(64),
                },
            ))
            .await
            .expect("start legacy");
        wait_for_state(&engine, RunState::Running).await;
        engine
            .handle_command(CommandEnvelope::new(profile.id, Command::EmergencyStop))
            .await
            .expect("emergency stop");
        wait_for_state(&engine, RunState::Idle).await;
        assert_eq!(legacy.starts.load(Ordering::SeqCst), 1);
        assert!(legacy.cancels.load(Ordering::SeqCst) >= 1);
        assert!(backend.release_count() >= 1);
    }

    #[tokio::test]
    async fn worker_panic_still_releases_inputs_and_faults() {
        let directory = tempdir().expect("temp directory");
        let store =
            Arc::new(SqliteStore::open(directory.path().join("db.sqlite3")).expect("store"));
        let mut profile = Profile::new("panic test");
        profile.automation.reconnect_enabled = false;
        store.save_profile(&profile).expect("profile");
        let backend = Arc::new(PanicBackend {
            releases: AtomicUsize::new(0),
        });
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
        wait_for_state(&engine, RunState::Faulted).await;
        assert_eq!(backend.releases.load(Ordering::SeqCst), 1);
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
        let mut events = engine.subscribe();
        let command = CommandEnvelope::new(profile.id, Command::ShutdownDaemon);
        let request_id = command.request_id;
        engine
            .handle_command(command)
            .await
            .expect("daemon shutdown");
        wait_for_state(&engine, RunState::Idle).await;
        assert!(backend.release_count() >= 1);
        let shutdown_ready = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let event = events.recv().await.expect("shutdown event");
                if let DaemonEvent::ShutdownReady {
                    request_id: received,
                } = event.event
                {
                    break received;
                }
            }
        })
        .await
        .expect("shutdown-ready timeout");
        assert_eq!(shutdown_ready, request_id);
    }

    #[tokio::test]
    async fn empty_store_gets_one_persisted_safe_default_profile() {
        let directory = tempdir().expect("temp directory");
        let store =
            Arc::new(SqliteStore::open(directory.path().join("db.sqlite3")).expect("store"));
        let engine = AutomationEngine::new(Arc::new(MockBackend::default()), Arc::clone(&store))
            .expect("engine");
        let selected = engine.snapshot().profile_id;
        assert!(!selected.is_nil());
        let profile = store
            .load_profile(selected)
            .expect("load default")
            .expect("default profile");
        assert_eq!(profile.name, "Default (Safe)");
        assert!(!profile.automation.gathering_enabled);
        assert_eq!(profile.safety.item_budgets.dice, 0);
        assert!(!profile.discord.enabled);

        let mut events = engine.subscribe();
        engine
            .handle_command(CommandEnvelope::new(selected, Command::GetProfiles))
            .await
            .expect("get profiles");
        let (profiles, returned_selected) = loop {
            let event = events.recv().await.expect("profiles event");
            if let DaemonEvent::Profiles {
                profiles,
                selected_profile_id,
            } = event.event
            {
                break (profiles, selected_profile_id);
            }
        };
        assert_eq!(returned_selected, selected);
        assert_eq!(profiles, vec![profile]);
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
