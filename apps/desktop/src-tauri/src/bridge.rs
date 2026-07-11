use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    path::{Path, PathBuf},
    process::{Child, Command as ProcessCommand, Stdio},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use chrono::Utc;
use nectarpilot_contracts::{
    Command, CommandEnvelope, DaemonEvent, DiscordPermissions, EventEnvelope, FeatureFlags,
    FieldRotation, PROTOCOL_VERSION, Profile, RunSnapshot, RunState, ValuableItemBudgets,
};
use nectarpilot_core::transport::NamedPipeSpec;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{Mutex as AsyncMutex, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[cfg(windows)]
use nectarpilot_core::transport::{CommandSender, connect_named_pipe, daemon_client};
#[cfg(windows)]
use tokio::{io::WriteHalf, net::windows::named_pipe::NamedPipeClient};

const EVENT_NAME: &str = "nectarpilot:event";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(8);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const SNAPSHOT_INTERVAL: Duration = Duration::from_secs(2);
const RECONNECT_DELAYS: [Duration; 7] = [
    Duration::from_millis(50),
    Duration::from_millis(100),
    Duration::from_millis(250),
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
];
const MAX_PROJECTED_EVENTS: usize = 100;

#[cfg(windows)]
type PipeCommandSender = CommandSender<WriteHalf<NamedPipeClient>>;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchReceipt {
    request_id: Uuid,
    accepted_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TimelineProjection {
    id: String,
    timestamp: chrono::DateTime<Utc>,
    title: String,
    detail: String,
    tone: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LogProjection {
    id: String,
    timestamp: chrono::DateTime<Utc>,
    level: &'static str,
    component: String,
    message: String,
}

#[derive(Default)]
struct BridgeCache {
    connected: bool,
    last_transport_error: Option<String>,
    run: Option<RunSnapshot>,
    selected_profile_id: Option<Uuid>,
    run_state_reason: Option<String>,
    profiles: BTreeMap<Uuid, Profile>,
    timeline: VecDeque<TimelineProjection>,
    logs: VecDeque<LogProjection>,
    updated_at: Option<chrono::DateTime<Utc>>,
}

struct OwnedDaemon {
    child: Child,
    executable: PathBuf,
}

impl OwnedDaemon {
    fn is_running(&mut self) -> Result<bool, String> {
        self.child
            .try_wait()
            .map(|status| status.is_none())
            .map_err(|error| format!("could not inspect daemon process: {error}"))
    }

    fn stop(&mut self) {
        if self.child.try_wait().is_ok_and(|status| status.is_none()) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

impl Drop for OwnedDaemon {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Native-only desktop/daemon boundary. The `WebView` can submit versioned data
/// commands, but it never receives process, pipe, shell, or filesystem handles.
pub struct DaemonBridge {
    app: AppHandle,
    cache: RwLock<BridgeCache>,
    owned_daemon: Mutex<Option<OwnedDaemon>>,
    pending: AsyncMutex<HashMap<Uuid, oneshot::Sender<Result<(), String>>>>,
    profile_bootstrap: AsyncMutex<()>,
    #[cfg(windows)]
    sender: AsyncMutex<Option<PipeCommandSender>>,
    shutdown: CancellationToken,
    shutting_down: AtomicBool,
}

impl DaemonBridge {
    pub fn new(app: AppHandle) -> Arc<Self> {
        Arc::new(Self {
            app,
            cache: RwLock::new(BridgeCache::default()),
            owned_daemon: Mutex::new(None),
            pending: AsyncMutex::new(HashMap::new()),
            profile_bootstrap: AsyncMutex::new(()),
            #[cfg(windows)]
            sender: AsyncMutex::new(None),
            shutdown: CancellationToken::new(),
            shutting_down: AtomicBool::new(false),
        })
    }

    pub fn start(self: &Arc<Self>) {
        let bridge = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            bridge.supervise().await;
        });
    }

    #[must_use]
    pub fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::SeqCst)
    }

    pub async fn shutdown(&self) {
        if self.shutting_down.swap(true, Ordering::SeqCst) {
            return;
        }

        let profile_id = self
            .cached_run()
            .map_or_else(Uuid::nil, |snapshot| snapshot.profile_id);
        let emergency = CommandEnvelope::new(profile_id, Command::EmergencyStop);
        let _ = tokio::time::timeout(SHUTDOWN_TIMEOUT, self.dispatch(emergency)).await;

        #[cfg(windows)]
        {
            let graceful_exit = CommandEnvelope::new(profile_id, Command::ShutdownDaemon);
            let _ =
                tokio::time::timeout(SHUTDOWN_TIMEOUT, self.send_untracked(graceful_exit)).await;
        }
        self.shutdown.cancel();

        #[cfg(windows)]
        {
            let mut sender = self.sender.lock().await;
            if let Some(mut active) = sender.take() {
                let _ = active.close().await;
            }
        }

        self.fail_pending("desktop shell is shutting down").await;
        #[cfg(windows)]
        self.stop_owned_daemon_gracefully().await;
        #[cfg(not(windows))]
        self.stop_owned_daemon();
    }

    pub fn force_stop_owned_daemon(&self) {
        self.shutdown.cancel();
        self.stop_owned_daemon();
    }

    pub async fn dispatch(&self, envelope: CommandEnvelope) -> Result<DispatchReceipt, String> {
        envelope
            .validate_version()
            .map_err(|error| error.to_string())?;
        if self.is_shutting_down() {
            return Err("desktop shell is shutting down".to_owned());
        }

        #[cfg(not(windows))]
        {
            let _ = envelope;
            return Err("the native automation daemon is supported only on Windows".to_owned());
        }

        #[cfg(windows)]
        {
            let request_id = envelope.request_id;
            let (result_sender, result_receiver) = oneshot::channel();
            self.pending.lock().await.insert(request_id, result_sender);

            let send_result = {
                let mut sender = self.sender.lock().await;
                match sender.as_mut() {
                    Some(sender) => sender
                        .send(&envelope)
                        .await
                        .map_err(|error| error.to_string()),
                    None => Err("automation daemon is not connected".to_owned()),
                }
            };
            if let Err(error) = send_result {
                self.pending.lock().await.remove(&request_id);
                self.record_transport_error(error.clone());
                return Err(error);
            }

            let accepted = tokio::time::timeout(COMMAND_TIMEOUT, result_receiver)
                .await
                .map_err(|_| format!("daemon did not acknowledge request {request_id} in time"))?
                .map_err(|_| format!("daemon acknowledgement channel closed for {request_id}"))?;
            self.pending.lock().await.remove(&request_id);
            accepted?;
            Ok(DispatchReceipt {
                request_id,
                accepted_at: Utc::now(),
            })
        }
    }

    pub async fn refresh_snapshot(&self) -> Result<(), String> {
        let profile_id = self
            .cached_run()
            .map_or_else(Uuid::nil, |snapshot| snapshot.profile_id);
        self.dispatch(CommandEnvelope::new(profile_id, Command::GetSnapshot))
            .await?;
        Ok(())
    }

    #[must_use]
    pub fn cached_run(&self) -> Option<RunSnapshot> {
        self.cache.read().ok()?.run.clone()
    }

    pub async fn dashboard_snapshot(&self) -> Value {
        if self.cache.read().is_ok_and(|cache| cache.connected) {
            let _ = self.ensure_selected_profile().await;
        }
        if self.cached_run().is_none() {
            let _ = self.refresh_snapshot().await;
        }
        self.project_dashboard()
    }

    async fn ensure_selected_profile(&self) -> Result<Uuid, String> {
        let _bootstrap_guard = self.profile_bootstrap.lock().await;
        if let Ok(cache) = self.cache.read()
            && let Some(profile_id) = cache.selected_profile_id
            && cache.profiles.contains_key(&profile_id)
        {
            return Ok(profile_id);
        }

        let profile_id = default_profile_id();
        match self
            .dispatch(CommandEnvelope::new(profile_id, Command::ExportProfile))
            .await
        {
            Ok(_) => {}
            Err(error) if error.contains("was not found") => {
                let mut profile = Profile::new("Primary profile");
                profile.id = profile_id;
                profile.onboarding_complete = false;
                self.dispatch(CommandEnvelope::new(
                    profile_id,
                    Command::SaveProfile {
                        profile: Box::new(profile.clone()),
                    },
                ))
                .await?;
                self.cache
                    .write()
                    .map_err(|_| "daemon cache lock poisoned".to_owned())?
                    .profiles
                    .insert(profile_id, profile);
            }
            Err(error) => return Err(error),
        }

        let mut cache = self
            .cache
            .write()
            .map_err(|_| "daemon cache lock poisoned".to_owned())?;
        if !cache.profiles.contains_key(&profile_id) {
            return Err("daemon acknowledged the profile but did not return it".to_owned());
        }
        cache.selected_profile_id = Some(profile_id);
        Ok(profile_id)
    }

    pub async fn save_automation_settings(
        &self,
        profile_id: Uuid,
        settings: UiAutomationSettings,
    ) -> Result<(), String> {
        settings.validate()?;
        let mut profile = self.profile(profile_id)?;
        settings.apply_to(&mut profile);
        profile.updated_at = Utc::now();
        self.dispatch(CommandEnvelope::new(
            profile_id,
            Command::SaveProfile {
                profile: Box::new(profile),
            },
        ))
        .await?;
        Ok(())
    }

    pub async fn complete_onboarding(&self, profile_id: Uuid) -> Result<(), String> {
        let mut profile = self.profile(profile_id)?;
        profile.onboarding_complete = true;
        profile.updated_at = Utc::now();
        self.dispatch(CommandEnvelope::new(
            profile_id,
            Command::SaveProfile {
                profile: Box::new(profile),
            },
        ))
        .await?;
        Ok(())
    }

    pub async fn trust_extension(
        &self,
        profile_id: Uuid,
        extension_id: String,
        digest: String,
    ) -> Result<(), String> {
        let normalized = digest.strip_prefix("sha256:").unwrap_or(&digest);
        if normalized.len() != 64 || !normalized.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("extension trust requires a complete SHA-256 digest".to_owned());
        }
        if extension_id.trim().is_empty() || extension_id.len() > 128 {
            return Err("extension identifier is invalid".to_owned());
        }
        let mut profile = self.profile(profile_id)?;
        profile
            .trusted_extensions
            .insert(extension_id, normalized.to_ascii_lowercase());
        profile.updated_at = Utc::now();
        self.dispatch(CommandEnvelope::new(
            profile_id,
            Command::SaveProfile {
                profile: Box::new(profile),
            },
        ))
        .await?;
        Ok(())
    }

    pub async fn select_profile(&self, profile_id: Uuid) -> Result<(), String> {
        if self.profile(profile_id).is_err() {
            self.dispatch(CommandEnvelope::new(profile_id, Command::ExportProfile))
                .await?;
            self.profile(profile_id)?;
        }
        self.cache
            .write()
            .map_err(|_| "daemon cache lock poisoned".to_owned())?
            .selected_profile_id = Some(profile_id);
        Ok(())
    }

    fn profile(&self, profile_id: Uuid) -> Result<Profile, String> {
        self.cache
            .read()
            .map_err(|_| "daemon cache lock poisoned".to_owned())?
            .profiles
            .get(&profile_id)
            .cloned()
            .ok_or_else(|| format!("daemon profile {profile_id} is not available"))
    }

    #[cfg(windows)]
    async fn supervise(&self) {
        let mut reconnect_index = 0_usize;
        while !self.shutdown.is_cancelled() {
            match self.connect().await {
                Ok((sender, mut events)) => {
                    reconnect_index = 0;
                    *self.sender.lock().await = Some(sender);
                    self.set_connected(true, None);
                    let _ = self
                        .send_untracked(CommandEnvelope::new(Uuid::nil(), Command::GetSnapshot))
                        .await;

                    let mut refresh = tokio::time::interval_at(
                        tokio::time::Instant::now() + SNAPSHOT_INTERVAL,
                        SNAPSHOT_INTERVAL,
                    );
                    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    loop {
                        tokio::select! {
                            () = self.shutdown.cancelled() => break,
                            _ = refresh.tick() => {
                                let profile_id = self.cached_run()
                                    .map_or_else(Uuid::nil, |snapshot| snapshot.profile_id);
                                if self.send_untracked(CommandEnvelope::new(
                                    profile_id,
                                    Command::GetSnapshot,
                                )).await.is_err() {
                                    break;
                                }
                            }
                            event = events.next() => match event {
                                Ok(Some(event)) => {
                                    match self.handle_event(event).await {
                                        Ok(Some(command)) => {
                                            let _ = self.send_untracked(command).await;
                                        }
                                        Ok(None) => {}
                                        Err(error) => {
                                            self.record_transport_error(error);
                                            break;
                                        }
                                    }
                                }
                                Ok(None) => {
                                    self.record_transport_error("daemon pipe closed".to_owned());
                                    break;
                                }
                                Err(error) => {
                                    self.record_transport_error(error.to_string());
                                    break;
                                }
                            }
                        }
                    }
                    *self.sender.lock().await = None;
                    self.set_connected(false, Some("daemon connection was interrupted".to_owned()));
                    self.fail_pending("daemon connection was interrupted").await;
                }
                Err(error) => {
                    self.set_connected(false, Some(error));
                }
            }

            if self.shutdown.is_cancelled() {
                break;
            }
            let delay = RECONNECT_DELAYS[reconnect_index.min(RECONNECT_DELAYS.len() - 1)];
            reconnect_index = reconnect_index.saturating_add(1);
            tokio::select! {
                () = self.shutdown.cancelled() => break,
                () = tokio::time::sleep(delay) => {}
            }
        }
    }

    #[cfg(not(windows))]
    async fn supervise(&self) {
        self.set_connected(
            false,
            Some("the native automation daemon is supported only on Windows".to_owned()),
        );
    }

    #[cfg(windows)]
    async fn connect(
        &self,
    ) -> Result<
        (
            PipeCommandSender,
            nectarpilot_core::transport::EventReceiver<tokio::io::ReadHalf<NamedPipeClient>>,
        ),
        String,
    > {
        let spec = NamedPipeSpec::for_current_environment();
        match connect_named_pipe(&spec).await {
            Ok(stream) => return Ok(daemon_client(stream)),
            Err(first_error) => {
                self.ensure_owned_daemon().map_err(|spawn_error| {
                    format!("could not connect ({first_error}) or start daemon ({spawn_error})")
                })?;
            }
        }
        connect_named_pipe(&spec)
            .await
            .map(daemon_client)
            .map_err(|error| format!("daemon did not open its secure user pipe: {error}"))
    }

    #[cfg(windows)]
    async fn send_untracked(&self, envelope: CommandEnvelope) -> Result<(), String> {
        let mut sender = self.sender.lock().await;
        sender
            .as_mut()
            .ok_or_else(|| "automation daemon is not connected".to_owned())?
            .send(&envelope)
            .await
            .map_err(|error| error.to_string())
    }

    async fn handle_event(
        &self,
        envelope: EventEnvelope,
    ) -> Result<Option<CommandEnvelope>, String> {
        if envelope.protocol_version != PROTOCOL_VERSION {
            return Err(format!(
                "daemon protocol mismatch: expected {PROTOCOL_VERSION}, received {}",
                envelope.protocol_version
            ));
        }

        let follow_up = match &envelope.event {
            DaemonEvent::CommandAccepted { request_id } => {
                if let Some(waiter) = self.pending.lock().await.remove(request_id) {
                    let _ = waiter.send(Ok(()));
                }
                None
            }
            DaemonEvent::CommandRejected { request_id, reason } => {
                if let Some(waiter) = self.pending.lock().await.remove(request_id) {
                    let _ = waiter.send(Err(reason.clone()));
                }
                None
            }
            DaemonEvent::Snapshot(snapshot) => {
                let profile_id = snapshot.profile_id;
                if let Ok(mut cache) = self.cache.write() {
                    cache.run = Some(snapshot.clone());
                }
                (!profile_id.is_nil())
                    .then(|| CommandEnvelope::new(profile_id, Command::ExportProfile))
            }
            DaemonEvent::ProfileSaved { profile_id } => {
                Some(CommandEnvelope::new(*profile_id, Command::ExportProfile))
            }
            DaemonEvent::ProfileExported { profile_id, json } => {
                let profile: Profile = serde_json::from_str(json)
                    .map_err(|error| format!("daemon exported an invalid profile: {error}"))?;
                if profile.id != *profile_id {
                    return Err("daemon profile export identifier mismatch".to_owned());
                }
                let mut cache = self
                    .cache
                    .write()
                    .map_err(|_| "daemon cache lock poisoned".to_owned())?;
                cache.profiles.insert(*profile_id, profile);
                if cache.selected_profile_id.is_none() {
                    cache.selected_profile_id = Some(*profile_id);
                }
                None
            }
            DaemonEvent::ProfileDeleted { profile_id } => {
                if let Ok(mut cache) = self.cache.write() {
                    cache.profiles.remove(profile_id);
                }
                None
            }
            _ => None,
        };

        self.project_event(&envelope);
        self.app
            .emit(EVENT_NAME, &envelope)
            .map_err(|error| format!("could not forward daemon event: {error}"))?;
        Ok(follow_up)
    }

    fn project_event(&self, envelope: &EventEnvelope) {
        let Ok(mut cache) = self.cache.write() else {
            return;
        };
        cache.updated_at = Some(envelope.timestamp);
        if let Some(snapshot) = cache.run.as_mut() {
            snapshot.last_sequence = envelope.sequence;
            snapshot.run_id = envelope.run_id;
        }

        let timeline = match &envelope.event {
            DaemonEvent::StateChanged {
                current, reason, ..
            } => {
                if let Some(snapshot) = cache.run.as_mut() {
                    snapshot.state = *current;
                }
                cache.run_state_reason = Some(reason.clone());
                Some(TimelineProjection {
                    id: format!("event-{}", envelope.sequence),
                    timestamp: envelope.timestamp,
                    title: format!("Automation {}", run_state_label(*current).to_lowercase()),
                    detail: reason.clone(),
                    tone: match current {
                        RunState::Faulted | RunState::NeedsAttention => "danger",
                        RunState::Recovering => "warning",
                        RunState::Running => "success",
                        _ => "info",
                    },
                })
            }
            DaemonEvent::CommandRejected { reason, .. } => Some(TimelineProjection {
                id: format!("event-{}", envelope.sequence),
                timestamp: envelope.timestamp,
                title: "Command rejected".to_owned(),
                detail: reason.clone(),
                tone: "warning",
            }),
            DaemonEvent::ActionCompleted(result) => Some(TimelineProjection {
                id: format!("event-{}", envelope.sequence),
                timestamp: envelope.timestamp,
                title: result.action.clone(),
                detail: result.message.clone(),
                tone: match result.outcome {
                    nectarpilot_contracts::ActionOutcome::Succeeded => "success",
                    nectarpilot_contracts::ActionOutcome::Failed
                    | nectarpilot_contracts::ActionOutcome::NeedsAttention => "danger",
                    _ => "info",
                },
            }),
            DaemonEvent::SafeModeEntered { crash_count, .. } => Some(TimelineProjection {
                id: format!("event-{}", envelope.sequence),
                timestamp: envelope.timestamp,
                title: "Safe mode entered".to_owned(),
                detail: format!("{crash_count} daemon crashes were recorded in ten minutes"),
                tone: "danger",
            }),
            _ => None,
        };
        if let Some(timeline) = timeline {
            cache.timeline.push_front(timeline);
            cache.timeline.truncate(MAX_PROJECTED_EVENTS);
        }
        if let DaemonEvent::Log {
            level,
            target,
            message,
            ..
        } = &envelope.event
        {
            cache.logs.push_front(LogProjection {
                id: format!("log-{}", envelope.sequence),
                timestamp: envelope.timestamp,
                level: match level {
                    nectarpilot_contracts::EventLevel::Trace
                    | nectarpilot_contracts::EventLevel::Debug => "debug",
                    nectarpilot_contracts::EventLevel::Info => "info",
                    nectarpilot_contracts::EventLevel::Warn => "warning",
                    nectarpilot_contracts::EventLevel::Error => "error",
                },
                component: target.clone(),
                message: message.clone(),
            });
            cache.logs.truncate(MAX_PROJECTED_EVENTS);
        }
    }

    fn project_dashboard(&self) -> Value {
        let Ok(cache) = self.cache.read() else {
            return unavailable_dashboard("daemon cache lock poisoned");
        };
        let actual_run = cache.run.as_ref();
        let run_profile_id = actual_run
            .map(|snapshot| snapshot.profile_id)
            .filter(|profile_id| !profile_id.is_nil());
        let profile_id = cache
            .selected_profile_id
            .or(run_profile_id)
            .or_else(|| cache.profiles.keys().next().copied())
            .unwrap_or_else(default_profile_id);
        let selected_profile = cache.profiles.get(&profile_id);
        let profile_projection =
            selected_profile.map_or_else(|| unavailable_profile(profile_id), project_profile);
        let state = actual_run.map_or(RunState::Faulted, |snapshot| snapshot.state);
        let reason = cache.run_state_reason.clone().unwrap_or_else(|| {
            if cache.connected {
                "Connected to the local automation daemon".to_owned()
            } else {
                cache
                    .last_transport_error
                    .clone()
                    .unwrap_or_else(|| "Waiting for the local automation daemon".to_owned())
            }
        });
        let profile_available = selected_profile.is_some();
        let active_task = actual_run.and_then(|snapshot| snapshot.active_task.as_deref());
        let updated_at = cache.updated_at.unwrap_or_else(Utc::now);
        let run_id = actual_run
            .filter(|snapshot| snapshot.state != RunState::Idle)
            .map(|snapshot| snapshot.run_id.to_string());

        json!({
            "runId": run_id,
            "runState": run_state_label(state),
            "runStateReason": reason,
            "activeProfileId": profile_id.to_string(),
            "profiles": [profile_projection],
            "onboardingComplete": selected_profile.is_some_and(|profile| profile.onboarding_complete),
            "safeMode": actual_run.is_some_and(|snapshot| snapshot.safe_mode),
            "session": {
                "connected": false,
                "processName": Value::Null,
                "pid": Value::Null,
                "windowTitle": Value::Null,
                "resolution": Value::Null,
                "dpi": Value::Null,
                "foreground": false,
                "calibration": Value::Null,
            },
            "readiness": [
                {
                    "id": "daemon",
                    "label": "Automation service",
                    "detail": if cache.connected { "Secure current-user pipe connected" } else { "Local daemon is unavailable" },
                    "status": if cache.connected { "ready" } else { "blocked" },
                },
                {
                    "id": "profile",
                    "label": "Configuration profile",
                    "detail": if profile_available { "Daemon-owned profile loaded" } else { "Waiting for a daemon-owned profile" },
                    "status": if profile_available { "ready" } else { "blocked" },
                },
                {
                    "id": "roblox",
                    "label": "Roblox client",
                    "detail": "No verified Roblox session has been reported",
                    "status": "checking",
                },
                {
                    "id": "budget",
                    "label": "Item safeguards",
                    "detail": budget_summary(selected_profile),
                    "status": "ready",
                }
            ],
            "metrics": [],
            "timeline": cache.timeline.iter().collect::<Vec<_>>(),
            "queue": active_task.map_or_else(Vec::<Value>::new, |task| vec![json!({
                "id": "active-daemon-task",
                "label": task,
                "detail": "Reported by the automation daemon",
                "status": "active",
            })]),
            "features": selected_profile.map_or_else(Vec::<Value>::new, project_features),
            "extensions": [],
            "logs": cache.logs.iter().collect::<Vec<_>>(),
            "updatedAt": updated_at,
        })
    }

    fn set_connected(&self, connected: bool, error: Option<String>) {
        if let Ok(mut cache) = self.cache.write() {
            cache.connected = connected;
            cache.last_transport_error = error;
            cache.updated_at = Some(Utc::now());
        }
    }

    fn record_transport_error(&self, error: String) {
        if let Ok(mut cache) = self.cache.write() {
            cache.connected = false;
            cache.last_transport_error = Some(error.clone());
            cache.updated_at = Some(Utc::now());
            cache.logs.push_front(LogProjection {
                id: format!("bridge-{}", Uuid::now_v7()),
                timestamp: Utc::now(),
                level: "error",
                component: "desktop_bridge".to_owned(),
                message: error,
            });
            cache.logs.truncate(MAX_PROJECTED_EVENTS);
        }
    }

    async fn fail_pending(&self, reason: &str) {
        let pending = std::mem::take(&mut *self.pending.lock().await);
        for (_, waiter) in pending {
            let _ = waiter.send(Err(reason.to_owned()));
        }
    }

    fn ensure_owned_daemon(&self) -> Result<(), String> {
        let mut owned = self
            .owned_daemon
            .lock()
            .map_err(|_| "daemon process lock poisoned".to_owned())?;
        if let Some(daemon) = owned.as_mut() {
            if daemon.is_running()? {
                return Ok(());
            }
            tracing::warn!(path = %daemon.executable.display(), "owned daemon exited; restarting");
            owned.take();
        }

        let executable = self.resolve_daemon_executable()?;
        let database = self
            .app
            .path()
            .app_local_data_dir()
            .map_err(|error| format!("could not resolve application data directory: {error}"))?
            .join("nectarpilot.sqlite3");
        if let Some(parent) = database.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("could not prepare app data directory: {error}"))?;
        }

        let mut command = ProcessCommand::new(&executable);
        command
            .arg("serve")
            .arg("--pipe")
            .arg("--database")
            .arg(&database)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let child = command.spawn().map_err(|error| {
            format!(
                "could not launch packaged daemon {}: {error}",
                executable.display()
            )
        })?;
        tracing::info!(pid = child.id(), path = %executable.display(), "started owned daemon sidecar");
        *owned = Some(OwnedDaemon { child, executable });
        Ok(())
    }

    fn resolve_daemon_executable(&self) -> Result<PathBuf, String> {
        let executable_name = if cfg!(windows) {
            "nectarpilot-daemon.exe"
        } else {
            "nectarpilot-daemon"
        };
        let current_executable = std::env::current_exe()
            .map_err(|error| format!("could not resolve desktop executable: {error}"))?;
        let mut candidates = Vec::new();

        if cfg!(debug_assertions)
            && let Some(override_path) = std::env::var_os("NECTARPILOT_DAEMON_PATH")
        {
            candidates.push(PathBuf::from(override_path));
        }
        if let Some(directory) = current_executable.parent() {
            candidates.push(directory.join(executable_name));
        }
        if let Ok(resource_dir) = self.app.path().resource_dir() {
            candidates.push(resource_dir.join(executable_name));
            candidates.push(resource_dir.join("binaries").join(executable_name));
        }
        if cfg!(debug_assertions) {
            candidates.extend(development_daemon_candidates(&current_executable));
            if let Ok(current_dir) = std::env::current_dir() {
                candidates.extend(development_daemon_candidates(&current_dir));
            }
        }
        candidates
            .into_iter()
            .find(|candidate| candidate.is_file())
            .ok_or_else(|| {
                "packaged nectarpilot-daemon sidecar was not found; run the sidecar preparation script"
                    .to_owned()
            })
    }

    fn stop_owned_daemon(&self) {
        let Ok(mut owned) = self.owned_daemon.lock() else {
            return;
        };
        if let Some(mut daemon) = owned.take() {
            tracing::info!(pid = daemon.child.id(), path = %daemon.executable.display(), "stopping exact owned daemon");
            daemon.stop();
        }
    }

    #[cfg(windows)]
    async fn stop_owned_daemon_gracefully(&self) {
        let Some(mut daemon) = self
            .owned_daemon
            .lock()
            .ok()
            .and_then(|mut owned| owned.take())
        else {
            return;
        };
        let deadline = tokio::time::Instant::now() + SHUTDOWN_TIMEOUT;
        loop {
            match daemon.child.try_wait() {
                Ok(Some(status)) => {
                    tracing::info!(
                        pid = daemon.child.id(),
                        path = %daemon.executable.display(),
                        %status,
                        "owned daemon exited cleanly"
                    );
                    return;
                }
                Ok(None) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Ok(None) => {
                    tracing::warn!(
                        pid = daemon.child.id(),
                        path = %daemon.executable.display(),
                        "owned daemon exceeded graceful shutdown deadline"
                    );
                    daemon.stop();
                    return;
                }
                Err(error) => {
                    tracing::warn!(
                        pid = daemon.child.id(),
                        path = %daemon.executable.display(),
                        %error,
                        "could not inspect owned daemon during graceful shutdown"
                    );
                    daemon.stop();
                    return;
                }
            }
        }
    }
}

fn development_daemon_candidates(start: &Path) -> Vec<PathBuf> {
    let executable_name = if cfg!(windows) {
        "nectarpilot-daemon.exe"
    } else {
        "nectarpilot-daemon"
    };
    start
        .ancestors()
        .flat_map(|ancestor| {
            [
                ancestor.join("target").join("debug").join(executable_name),
                ancestor
                    .join("target")
                    .join("release")
                    .join(executable_name),
            ]
        })
        .collect()
}

fn default_profile_id() -> Uuid {
    Uuid::parse_str("01900000-0000-7000-8000-000000000001")
        .expect("built-in primary profile UUID is valid")
}

fn run_state_label(state: RunState) -> &'static str {
    match state {
        RunState::Idle => "Idle",
        RunState::Preflight => "Preflight",
        RunState::Running => "Running",
        RunState::Paused => "Paused",
        RunState::Recovering => "Recovering",
        RunState::NeedsAttention => "NeedsAttention",
        RunState::Stopping => "Stopping",
        RunState::Faulted => "Faulted",
    }
}

fn unavailable_dashboard(reason: &str) -> Value {
    json!({
        "runId": Value::Null,
        "runState": "Faulted",
        "runStateReason": reason,
        "activeProfileId": Uuid::nil().to_string(),
        "profiles": [unavailable_profile(Uuid::nil())],
        "onboardingComplete": false,
        "safeMode": false,
        "session": { "connected": false, "processName": Value::Null, "pid": Value::Null, "windowTitle": Value::Null, "resolution": Value::Null, "dpi": Value::Null, "foreground": false, "calibration": Value::Null },
        "readiness": [], "metrics": [], "timeline": [], "queue": [], "features": [], "extensions": [], "logs": [],
        "updatedAt": Utc::now(),
    })
}

fn unavailable_profile(profile_id: Uuid) -> Value {
    json!({
        "id": profile_id.to_string(),
        "name": "Profile unavailable",
        "description": "Waiting for the daemon-owned configuration",
        "accent": "#f4b942",
        "lastUsedAt": Utc::now(),
        "settings": safe_ui_settings(),
    })
}

fn project_profile(profile: &Profile) -> Value {
    let first_rotation = profile.automation.rotations.first();
    let fields = profile
        .automation
        .rotations
        .iter()
        .map(|rotation| rotation.field.clone())
        .collect::<Vec<_>>();
    json!({
        "id": profile.id.to_string(),
        "name": profile.name,
        "description": "Daemon-owned NectarPilot profile",
        "accent": "#f4b942",
        "lastUsedAt": profile.updated_at,
        "settings": {
            "features": feature_map(&profile.automation.features),
            "gathering": {
                "enabled": profile.automation.gathering_enabled,
                "fields": fields,
                "pattern": first_rotation.map_or("stationary", |rotation| rotation.pattern.as_str()),
                "minutesPerField": first_rotation.map_or(1, |rotation| (rotation.gather_seconds / 60).max(1)),
                "returnAtCapacity": 100,
                "driftCorrection": false,
            },
            "safety": {
                "pauseOnFocusLoss": true,
                "requireForeground": true,
                "confirmHighRiskActions": true,
                "budgets": {
                    "fieldDice": profile.safety.item_budgets.dice,
                    "glitter": profile.safety.item_budgets.glitter,
                    "eggs": profile.safety.item_budgets.eggs,
                    "stickers": profile.safety.item_budgets.stickers,
                    "vouchers": profile.safety.item_budgets.vouchers,
                    "shrineDonations": profile.safety.item_budgets.shrine_donations,
                },
            },
            "recovery": {
                "reconnectEnabled": profile.automation.reconnect_enabled,
                "maxAttempts": 5,
                "deadlineMinutes": 15,
                "restartOnConfirmedFreeze": false,
            },
            "monitoring": {
                "discordEnabled": profile.discord.enabled,
                "evidenceRetentionDays": profile.safety.evidence_retention_days,
                "evidenceLimitMb": profile.safety.evidence_retention_megabytes,
                "permissions": {
                    "status": profile.discord.permissions.status,
                    "macroControl": profile.discord.permissions.macro_control,
                    "settings": profile.discord.permissions.settings,
                    "screenshots": profile.discord.permissions.screenshots,
                    "remoteInput": profile.discord.permissions.remote_input,
                    "extensionImport": profile.discord.permissions.extension_import,
                    "systemPower": profile.discord.permissions.system_power,
                },
            },
            "hotkeys": {
                "start": profile.automation.hotkeys.start,
                "pause": profile.automation.hotkeys.pause_resume,
                "stop": profile.automation.hotkeys.stop,
                "emergencyStop": profile.automation.hotkeys.emergency_stop,
            },
        },
    })
}

fn safe_ui_settings() -> Value {
    json!({
        "features": {},
        "gathering": { "enabled": false, "fields": [], "pattern": "stationary", "minutesPerField": 1, "returnAtCapacity": 100, "driftCorrection": false },
        "safety": { "pauseOnFocusLoss": true, "requireForeground": true, "confirmHighRiskActions": true, "budgets": { "fieldDice": 0, "glitter": 0, "eggs": 0, "stickers": 0, "vouchers": 0, "shrineDonations": 0 } },
        "recovery": { "reconnectEnabled": true, "maxAttempts": 5, "deadlineMinutes": 15, "restartOnConfirmedFreeze": false },
        "monitoring": { "discordEnabled": false, "evidenceRetentionDays": 14, "evidenceLimitMb": 250, "permissions": { "status": false, "macroControl": false, "settings": false, "screenshots": false, "remoteInput": false, "extensionImport": false, "systemPower": false } },
        "hotkeys": { "start": "F1", "pause": "F2", "stop": "F3", "emergencyStop": "Ctrl+Shift+F12" },
    })
}

fn feature_map(flags: &FeatureFlags) -> Value {
    json!({
        "collect": flags.collections,
        "bosses": flags.bosses,
        "vicious": flags.vicious_bee,
        "memory": flags.memory_matches,
        "quests": flags.quests,
        "science-bear": flags.quests,
        "planter-cycle": flags.planters,
        "field-boost": flags.boosts,
        "shrine": flags.shrine,
        "stickers": flags.stickers,
        "hotbar": flags.hotbar_scheduling,
        "mutations-auto-jelly": flags.mutations_and_auto_jelly,
        "seasonal": flags.seasonal,
        "custom-extensions": flags.custom_extensions,
    })
}

fn project_features(profile: &Profile) -> Vec<Value> {
    let flags = &profile.automation.features;
    [
        (
            "collect",
            "Daily collections",
            flags.collections,
            "activity",
        ),
        ("bosses", "Boss runs", flags.bosses, "activity"),
        ("vicious", "Vicious Bee hunt", flags.vicious_bee, "activity"),
        ("memory", "Memory matches", flags.memory_matches, "activity"),
        ("field-boost", "Field boosters", flags.boosts, "boost"),
        (
            "hotbar",
            "Hotbar schedule",
            flags.hotbar_scheduling,
            "boost",
        ),
        ("quests", "Quest routing", flags.quests, "quest"),
        (
            "science-bear",
            "Science Bear planner",
            flags.quests,
            "quest",
        ),
        ("planter-cycle", "Planter cycle", flags.planters, "planter"),
    ]
    .into_iter()
    .map(|(id, title, enabled, category)| {
        json!({
            "id": id,
            "title": title,
            "description": "Configured in the active daemon profile.",
            "enabled": enabled,
            "status": if enabled { "Enabled" } else { "Off" },
            "category": category,
        })
    })
    .collect()
}

fn budget_summary(profile: Option<&Profile>) -> &'static str {
    let Some(profile) = profile else {
        return "No profile is loaded; valuable-item use is blocked";
    };
    let budget = &profile.safety.item_budgets;
    if budget.dice == 0
        && budget.glitter == 0
        && budget.eggs == 0
        && budget.stickers == 0
        && budget.vouchers == 0
        && budget.shrine_donations == 0
        && budget.other.values().all(|value| *value == 0)
    {
        "All valuable-item budgets are zero"
    } else {
        "Explicit valuable-item budgets are configured"
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiAutomationSettings {
    features: BTreeMap<String, bool>,
    gathering: UiGatheringSettings,
    safety: UiSafetySettings,
    recovery: UiRecoverySettings,
    monitoring: UiMonitoringSettings,
    hotkeys: UiHotkeys,
}

impl UiAutomationSettings {
    fn validate(&self) -> Result<(), String> {
        if self.gathering.fields.len() > 64 {
            return Err("a profile can contain at most 64 gathering fields".to_owned());
        }
        if self.gathering.fields.iter().any(|field| {
            field.trim().is_empty() || field.len() > 100 || field.chars().any(char::is_control)
        }) {
            return Err("gathering field names must be non-empty printable text".to_owned());
        }
        if self.gathering.pattern.trim().is_empty() || self.gathering.pattern.len() > 128 {
            return Err("gathering pattern identifier is invalid".to_owned());
        }
        if !(1..=24 * 60).contains(&self.gathering.minutes_per_field) {
            return Err("minutes per field must be between 1 and 1440".to_owned());
        }
        if !(1..=100).contains(&self.gathering.return_at_capacity) {
            return Err("return-at-capacity must be between 1 and 100 percent".to_owned());
        }
        if self.recovery.max_attempts != 5 || self.recovery.deadline_minutes != 15 {
            return Err(
                "v1 reconnect policy is fixed at five attempts within 15 minutes".to_owned(),
            );
        }
        if self.hotkeys.emergency_stop != "Ctrl+Shift+F12" {
            return Err("the hard emergency stop must remain Ctrl+Shift+F12".to_owned());
        }
        Ok(())
    }

    fn apply_to(&self, profile: &mut Profile) {
        profile.automation.gathering_enabled = self.gathering.enabled;
        profile.automation.reconnect_enabled = self.recovery.reconnect_enabled;
        let gather_seconds = self.gathering.minutes_per_field.saturating_mul(60);
        profile.automation.rotations = self
            .gathering
            .fields
            .iter()
            .map(|field| FieldRotation {
                field: field.clone(),
                pattern: self.gathering.pattern.clone(),
                gather_seconds,
                repetitions: 1,
            })
            .collect();
        apply_feature_map(&mut profile.automation.features, &self.features);
        profile
            .automation
            .hotkeys
            .start
            .clone_from(&self.hotkeys.start);
        profile
            .automation
            .hotkeys
            .pause_resume
            .clone_from(&self.hotkeys.pause);
        profile
            .automation
            .hotkeys
            .stop
            .clone_from(&self.hotkeys.stop);
        profile
            .automation
            .hotkeys
            .emergency_stop
            .clone_from(&self.hotkeys.emergency_stop);
        profile.safety.item_budgets = ValuableItemBudgets {
            dice: self.safety.budgets.field_dice,
            glitter: self.safety.budgets.glitter,
            eggs: self.safety.budgets.eggs,
            stickers: self.safety.budgets.stickers,
            vouchers: self.safety.budgets.vouchers,
            shrine_donations: self.safety.budgets.shrine_donations,
            other: profile.safety.item_budgets.other.clone(),
        };
        profile.safety.evidence_retention_days = self.monitoring.evidence_retention_days;
        profile.safety.evidence_retention_megabytes = self.monitoring.evidence_limit_mb;
        profile.discord.enabled = self.monitoring.discord_enabled;
        profile.discord.permissions = DiscordPermissions {
            status: self.monitoring.permissions.status,
            macro_control: self.monitoring.permissions.macro_control,
            settings: self.monitoring.permissions.settings,
            screenshots: self.monitoring.permissions.screenshots,
            remote_input: self.monitoring.permissions.remote_input,
            extension_import: self.monitoring.permissions.extension_import,
            system_power: self.monitoring.permissions.system_power,
        };

        // These are UI invariants today. Reading them prevents silently
        // accepting a weakened projection while the input broker remains the
        // authority for focus enforcement and freeze confirmation.
        let _ = (
            self.gathering.drift_correction,
            self.safety.pause_on_focus_loss,
            self.safety.require_foreground,
            self.safety.confirm_high_risk_actions,
            self.recovery.restart_on_confirmed_freeze,
        );
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UiGatheringSettings {
    enabled: bool,
    fields: Vec<String>,
    pattern: String,
    minutes_per_field: u32,
    return_at_capacity: u8,
    drift_correction: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UiSafetySettings {
    pause_on_focus_loss: bool,
    require_foreground: bool,
    confirm_high_risk_actions: bool,
    budgets: UiBudgets,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UiBudgets {
    field_dice: u32,
    glitter: u32,
    eggs: u32,
    stickers: u32,
    vouchers: u32,
    shrine_donations: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UiRecoverySettings {
    reconnect_enabled: bool,
    max_attempts: u8,
    deadline_minutes: u16,
    restart_on_confirmed_freeze: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UiMonitoringSettings {
    discord_enabled: bool,
    evidence_retention_days: u16,
    evidence_limit_mb: u32,
    permissions: UiRemotePermissions,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_excessive_bools)] // Mirrors independently revocable remote permissions.
struct UiRemotePermissions {
    status: bool,
    macro_control: bool,
    settings: bool,
    screenshots: bool,
    remote_input: bool,
    extension_import: bool,
    system_power: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UiHotkeys {
    start: String,
    pause: String,
    stop: String,
    emergency_stop: String,
}

fn apply_feature_map(flags: &mut FeatureFlags, values: &BTreeMap<String, bool>) {
    let enabled = |key: &str| values.get(key).copied().unwrap_or(false);
    flags.collections = enabled("collect");
    flags.bosses = enabled("bosses");
    flags.vicious_bee = enabled("vicious");
    flags.memory_matches = enabled("memory");
    flags.quests = enabled("quests")
        || enabled("science-bear")
        || enabled("brown-bear")
        || enabled("polar-bear")
        || enabled("bucko-riley");
    flags.planters = enabled("planter-cycle") || enabled("pesticide") || enabled("tacky");
    flags.boosts = enabled("field-boost") || enabled("nectar");
    flags.shrine = enabled("shrine");
    flags.stickers = enabled("stickers");
    flags.hotbar_scheduling = enabled("hotbar");
    flags.mutations_and_auto_jelly = enabled("mutations-auto-jelly");
    flags.seasonal = enabled("seasonal");
    flags.custom_extensions = enabled("custom-extensions");
}

#[cfg(test)]
mod tests {
    use nectarpilot_contracts::Profile;

    use super::{
        budget_summary, default_profile_id, project_profile, run_state_label, safe_ui_settings,
    };

    #[test]
    fn primary_profile_id_matches_the_desktop_seed() {
        assert_eq!(
            default_profile_id().to_string(),
            "01900000-0000-7000-8000-000000000001"
        );
    }

    #[test]
    fn disconnected_projection_keeps_all_item_budgets_at_zero() {
        let settings = safe_ui_settings();
        let budgets = &settings["safety"]["budgets"];
        assert_eq!(budgets["fieldDice"], 0);
        assert_eq!(budgets["glitter"], 0);
        assert_eq!(budgets["eggs"], 0);
        assert_eq!(budgets["shrineDonations"], 0);
    }

    #[test]
    fn profile_projection_does_not_enable_dangerous_defaults() {
        let profile = Profile::new("Safe");
        let projected = project_profile(&profile);
        assert_eq!(projected["settings"]["monitoring"]["discordEnabled"], false);
        assert_eq!(
            projected["settings"]["monitoring"]["permissions"]["remoteInput"],
            false
        );
        assert_eq!(
            budget_summary(Some(&profile)),
            "All valuable-item budgets are zero"
        );
    }

    #[test]
    fn all_contract_run_states_have_ui_labels() {
        assert_eq!(
            run_state_label(nectarpilot_contracts::RunState::NeedsAttention),
            "NeedsAttention"
        );
    }
}
