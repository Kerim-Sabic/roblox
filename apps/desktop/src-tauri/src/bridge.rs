use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command as ProcessCommand, Stdio},
    sync::{
        Arc, Mutex, OnceLock, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use chrono::Utc;
use nectarpilot_contracts::{
    Command, CommandEnvelope, DaemonEvent, DiscordPermissions, EventEnvelope, FeatureFlags,
    FieldRotation, LegacyInspection, PROTOCOL_VERSION, Profile, QuestScanResult, RunRecord,
    RunSnapshot, RunState, StatsSample, ValuableItemBudgets,
};
use nectarpilot_core::transport::NamedPipeSpec;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{Mutex as AsyncMutex, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[cfg(windows)]
use nectarpilot_core::transport::{
    CommandSender, connect_named_pipe, daemon_client, try_connect_named_pipe,
};
#[cfg(windows)]
use nectarpilot_platform::discover_roblox_clients;
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
const ROUTE_MANIFEST: &str = include_str!("../../../../assets/routes/_legacy-manifest.yaml");
const PATTERN_MANIFEST: &str = include_str!("../../../../assets/patterns/_legacy-manifest.yaml");
const LEGACY_VERSION: &str = "1.1.2";
const AUTOHOTKEY64_SHA256: &str =
    "37ff15a23a98f0a658298e21f1873ca896a05208810bf796f90ca212ee07c7b1";

static LEGACY_CATALOG: OnceLock<Result<Vec<LegacyCatalogEntry>, String>> = OnceLock::new();

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

struct RobloxDiscoveryProjection {
    session: Value,
    detail: String,
    status: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LegacyEntryStatus {
    SafeDsl,
    LegacyBridgeRequired,
}

#[derive(Debug, Deserialize)]
struct LegacyManifest {
    format_version: u16,
    kind: String,
    source_directory: String,
    total_files: usize,
    safe_dsl_files: usize,
    legacy_bridge_files: usize,
    entries: Vec<LegacyManifestEntry>,
}

#[derive(Debug, Deserialize)]
struct LegacyManifestEntry {
    legacy_source: String,
    sha256: String,
    bytes: u64,
    status: LegacyEntryStatus,
    generated_asset: Option<String>,
    requires_explicit_consent: bool,
    issue_counts: LegacyIssueCounts,
}

#[derive(Debug, Deserialize)]
struct LegacyIssueCounts {
    unsupported_syntax: usize,
    unsafe_capabilities: usize,
    invalid_values: usize,
}

#[derive(Debug, Clone)]
struct LegacyCatalogEntry {
    id: String,
    kind: String,
    source: String,
    display_name: String,
    sha256: String,
    bytes: u64,
    status: LegacyEntryStatus,
    generated_asset: Option<String>,
    unsupported_syntax: usize,
    unsafe_capabilities: usize,
    invalid_values: usize,
}

#[derive(Debug, Clone)]
struct TrustedAssetLayout {
    legacy_root: PathBuf,
    assets_root: Option<PathBuf>,
    autohotkey: Option<PathBuf>,
}

#[derive(Debug, Clone)]
enum AssetVerification {
    Verified,
    Unavailable(String),
}

#[derive(Debug, Clone)]
struct LegacyAssetInventory {
    layout: TrustedAssetLayout,
    entries: HashMap<String, AssetVerification>,
}

impl LegacyAssetInventory {
    fn verification(&self, extension_id: &str) -> AssetVerification {
        self.entries.get(extension_id).cloned().unwrap_or_else(|| {
            AssetVerification::Unavailable("asset was not inventoried".to_owned())
        })
    }

    fn verified_count(&self) -> usize {
        self.entries
            .values()
            .filter(|status| matches!(status, AssetVerification::Verified))
            .count()
    }
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
    stats: Option<StatsSample>,
    run_history: Vec<RunRecord>,
    legacy_inspection: Option<LegacyInspection>,
    quest_scan: Option<QuestScanResult>,
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
    legacy_assets: Result<LegacyAssetInventory, String>,
    cache: RwLock<BridgeCache>,
    owned_daemon: Mutex<Option<OwnedDaemon>>,
    pending: AsyncMutex<HashMap<Uuid, oneshot::Sender<Result<(), String>>>>,
    shutdown_ready: AsyncMutex<HashMap<Uuid, oneshot::Sender<()>>>,
    #[cfg(windows)]
    sender: AsyncMutex<Option<PipeCommandSender>>,
    shutdown: CancellationToken,
    shutting_down: AtomicBool,
}

impl DaemonBridge {
    pub fn new(app: AppHandle) -> Arc<Self> {
        let legacy_assets =
            legacy_catalog().and_then(|catalog| verify_legacy_assets(&app, catalog));
        Arc::new(Self {
            app,
            legacy_assets,
            cache: RwLock::new(BridgeCache::default()),
            owned_daemon: Mutex::new(None),
            pending: AsyncMutex::new(HashMap::new()),
            shutdown_ready: AsyncMutex::new(HashMap::new()),
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
        #[cfg(windows)]
        {
            if self.has_owned_daemon() {
                let graceful_exit = CommandEnvelope::new(profile_id, Command::ShutdownDaemon);
                let request_id = graceful_exit.request_id;
                let (ready_sender, ready_receiver) = oneshot::channel();
                self.shutdown_ready
                    .lock()
                    .await
                    .insert(request_id, ready_sender);
                if self.send_untracked(graceful_exit).await.is_ok() {
                    let _ = tokio::time::timeout(SHUTDOWN_TIMEOUT, ready_receiver).await;
                }
                self.shutdown_ready.lock().await.remove(&request_id);
            } else {
                let emergency = CommandEnvelope::new(profile_id, Command::EmergencyStop);
                let _ = self.send_untracked(emergency).await;
            }
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
        if self.cached_run().is_none() {
            let _ = self.refresh_snapshot().await;
        }
        if self
            .cache
            .read()
            .is_ok_and(|cache| cache.connected && cache.profiles.is_empty())
        {
            let _ = self.refresh_profiles().await;
        }
        self.project_dashboard()
    }

    async fn refresh_profiles(&self) -> Result<(), String> {
        let profile_id = self
            .cached_run()
            .map_or_else(Uuid::nil, |snapshot| snapshot.profile_id);
        self.dispatch(CommandEnvelope::new(profile_id, Command::GetProfiles))
            .await?;
        Ok(())
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
        let catalog_entry = legacy_catalog()?
            .iter()
            .find(|entry| entry.id == extension_id)
            .ok_or_else(|| {
                "extension is not present in the bundled compatibility catalog".to_owned()
            })?;
        if catalog_entry.status != LegacyEntryStatus::LegacyBridgeRequired {
            return Err("built-in DSL assets do not require explicit trust".to_owned());
        }
        if !catalog_entry.sha256.eq_ignore_ascii_case(normalized) {
            return Err(
                "extension digest does not match the bundled compatibility catalog".to_owned(),
            );
        }
        let inventory = self
            .legacy_assets
            .as_ref()
            .map_err(|error| format!("compatibility assets are unavailable: {error}"))?;
        if let AssetVerification::Unavailable(reason) = inventory.verification(&extension_id) {
            return Err(format!("compatibility asset cannot be trusted: {reason}"));
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

    /// Starts only a catalog-pinned, already trusted legacy asset. The `WebView`
    /// supplies no path or executable; the daemon re-verifies the manifest,
    /// profile consent, safe test policy, interpreter pin, timeout, and job
    /// containment before accepting the command.
    pub async fn start_legacy_extension(
        &self,
        profile_id: Uuid,
        extension_id: String,
        digest: String,
    ) -> Result<(), String> {
        let normalized = digest.strip_prefix("sha256:").unwrap_or(&digest);
        let catalog_entry = legacy_catalog()?
            .iter()
            .find(|entry| entry.id == extension_id)
            .ok_or_else(|| {
                "extension is not present in the bundled compatibility catalog".to_owned()
            })?;
        if catalog_entry.status != LegacyEntryStatus::LegacyBridgeRequired {
            return Err("only legacy bridge assets can use the compatibility runner".to_owned());
        }
        if !catalog_entry.sha256.eq_ignore_ascii_case(normalized) {
            return Err(
                "extension digest does not match the pinned compatibility catalog".to_owned(),
            );
        }
        let inventory = self
            .legacy_assets
            .as_ref()
            .map_err(|error| format!("compatibility assets are unavailable: {error}"))?;
        if let AssetVerification::Unavailable(reason) = inventory.verification(&extension_id) {
            return Err(format!("compatibility asset cannot run: {reason}"));
        }
        let profile = self.profile(profile_id)?;
        let trusted = profile
            .trusted_extensions
            .get(&extension_id)
            .is_some_and(|trusted| trusted.eq_ignore_ascii_case(normalized));
        if !trusted {
            return Err("trust this exact legacy script digest before running it".to_owned());
        }
        self.dispatch(CommandEnvelope::new(
            profile_id,
            Command::StartLegacy {
                script_id: extension_id,
                approved_sha256: normalized.to_ascii_lowercase(),
            },
        ))
        .await?;
        Ok(())
    }

    /// Starts the orchestrated legacy gather session built from the profile's
    /// rotations. Trust and safety re-checks happen daemon-side per step.
    pub async fn start_legacy_session(
        &self,
        profile_id: Uuid,
        max_cycles: u32,
        max_minutes: u32,
    ) -> Result<(), String> {
        self.dispatch(CommandEnvelope::new(
            profile_id,
            Command::StartLegacySession {
                max_cycles,
                max_minutes,
            },
        ))
        .await?;
        Ok(())
    }

    /// Requests the generated-harness review payload for one asset; the
    /// daemon answers with a `legacy_inspection` event.
    pub async fn inspect_legacy(&self, profile_id: Uuid, script_id: String) -> Result<(), String> {
        self.dispatch(CommandEnvelope::new(
            profile_id,
            Command::InspectLegacy { script_id },
        ))
        .await?;
        Ok(())
    }

    /// Stores one named secret in the daemon's encrypted store. The value is
    /// forwarded once and never cached or logged by the desktop process.
    pub async fn import_secret(
        &self,
        profile_id: Uuid,
        name: String,
        value: String,
    ) -> Result<(), String> {
        self.dispatch(CommandEnvelope::new(
            profile_id,
            Command::ImportSecret { name, value },
        ))
        .await?;
        Ok(())
    }

    /// One bounded advisory quest-log scan; the daemon answers with a
    /// `quest_scan` event that lands in the dashboard snapshot.
    pub async fn scan_quests(&self, profile_id: Uuid) -> Result<(), String> {
        self.dispatch(CommandEnvelope::new(profile_id, Command::ScanQuests))
            .await?;
        Ok(())
    }

    pub async fn get_run_history(&self, profile_id: Uuid) -> Result<(), String> {
        self.dispatch(CommandEnvelope::new(profile_id, Command::GetRunHistory))
            .await?;
        Ok(())
    }

    pub async fn select_profile(&self, profile_id: Uuid) -> Result<(), String> {
        self.dispatch(CommandEnvelope::new(profile_id, Command::SelectProfile))
            .await?;
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
                    let _ = self
                        .send_untracked(CommandEnvelope::new(Uuid::nil(), Command::GetProfiles))
                        .await;
                    let _ = self
                        .send_untracked(CommandEnvelope::new(Uuid::nil(), Command::GetRunHistory))
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
        match try_connect_named_pipe(&spec) {
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

    #[allow(clippy::too_many_lines)] // Exhaustive protocol projection is intentionally kept in one audit point.
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
                    if !profile_id.is_nil() {
                        let profile_changed = cache
                            .selected_profile_id
                            .is_some_and(|current| current != profile_id);
                        cache.selected_profile_id = Some(profile_id);
                        if profile_changed {
                            clear_profile_scoped_projections(&mut cache);
                        }
                    }
                }
                let profile_loaded = self
                    .cache
                    .read()
                    .ok()
                    .is_some_and(|cache| cache.profiles.contains_key(&profile_id));
                if profile_loaded {
                    None
                } else {
                    Some(CommandEnvelope::new(profile_id, Command::GetProfiles))
                }
            }
            DaemonEvent::ProfileSaved { profile_id } => {
                Some(CommandEnvelope::new(*profile_id, Command::GetProfiles))
            }
            DaemonEvent::Profiles {
                profiles,
                selected_profile_id,
            } => {
                if !profiles
                    .iter()
                    .any(|profile| profile.id == *selected_profile_id)
                {
                    return Err(
                        "daemon selected profile is absent from its profile list".to_owned()
                    );
                }
                let mapped_profiles = profiles
                    .iter()
                    .cloned()
                    .map(|profile| (profile.id, profile))
                    .collect::<BTreeMap<_, _>>();
                if mapped_profiles.len() != profiles.len() {
                    return Err("daemon profile list contains duplicate identifiers".to_owned());
                }
                let mut cache = self
                    .cache
                    .write()
                    .map_err(|_| "daemon cache lock poisoned".to_owned())?;
                let profile_changed = cache
                    .selected_profile_id
                    .is_some_and(|current| current != *selected_profile_id);
                cache.profiles = mapped_profiles;
                cache.selected_profile_id = Some(*selected_profile_id);
                if profile_changed {
                    clear_profile_scoped_projections(&mut cache);
                }
                if let Some(snapshot) = cache.run.as_mut() {
                    snapshot.profile_id = *selected_profile_id;
                }
                None
            }
            DaemonEvent::ProfileSelected { profile_id } => {
                let mut cache = self
                    .cache
                    .write()
                    .map_err(|_| "daemon cache lock poisoned".to_owned())?;
                if !cache.profiles.contains_key(profile_id) {
                    return Err("daemon selected an unknown profile".to_owned());
                }
                let profile_changed = cache.selected_profile_id != Some(*profile_id);
                cache.selected_profile_id = Some(*profile_id);
                if profile_changed {
                    clear_profile_scoped_projections(&mut cache);
                }
                if let Some(snapshot) = cache.run.as_mut() {
                    snapshot.profile_id = *profile_id;
                }
                Some(CommandEnvelope::new(*profile_id, Command::GetRunHistory))
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
                Some(CommandEnvelope::new(Uuid::nil(), Command::GetProfiles))
            }
            DaemonEvent::ShutdownReady { request_id } => {
                if let Some(waiter) = self.shutdown_ready.lock().await.remove(request_id) {
                    let _ = waiter.send(());
                }
                None
            }
            DaemonEvent::StatsSample(sample) => {
                if let Ok(mut cache) = self.cache.write() {
                    cache.stats = Some(sample.clone());
                }
                None
            }
            DaemonEvent::RunHistory {
                profile_id,
                entries,
            } => {
                if entries
                    .iter()
                    .any(|record| record.profile_id != *profile_id)
                {
                    return Err("daemon returned cross-profile run history".to_owned());
                }
                if let Ok(mut cache) = self.cache.write()
                    && cache.selected_profile_id == Some(*profile_id)
                {
                    cache.run_history.clone_from(entries);
                }
                None
            }
            DaemonEvent::LegacyInspection(inspection) => {
                if let Ok(mut cache) = self.cache.write() {
                    cache.legacy_inspection = Some(inspection.clone());
                }
                None
            }
            DaemonEvent::QuestScan(result) => {
                if let Ok(mut cache) = self.cache.write() {
                    cache.quest_scan = Some(result.clone());
                }
                None
            }
            // Refresh the active profile's history whenever a run settles.
            DaemonEvent::StateChanged { current, .. } if should_refresh_run_history(*current) => {
                let profile_id = self
                    .cache
                    .read()
                    .ok()
                    .and_then(|cache| cache.selected_profile_id)
                    .unwrap_or_else(Uuid::nil);
                Some(CommandEnvelope::new(profile_id, Command::GetRunHistory))
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
            DaemonEvent::SessionProgress(progress) => Some(TimelineProjection {
                id: format!("event-{}", envelope.sequence),
                timestamp: envelope.timestamp,
                title: format!(
                    "Session cycle {}/{} step {}/{}",
                    progress.cycle, progress.max_cycles, progress.step_index, progress.step_count
                ),
                detail: progress.description.clone(),
                tone: "info",
            }),
            DaemonEvent::SecretStored { name } => Some(TimelineProjection {
                id: format!("event-{}", envelope.sequence),
                timestamp: envelope.timestamp,
                title: "Secret stored".to_owned(),
                detail: format!("{name} was sealed into the encrypted daemon store"),
                tone: "success",
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

    #[allow(clippy::too_many_lines)] // One explicit compatibility projection keeps the JS boundary auditable.
    fn project_dashboard(&self) -> Value {
        let roblox = project_roblox_session();
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
            .unwrap_or_else(Uuid::nil);
        let selected_profile = cache.profiles.get(&profile_id);
        let profile_projections = if cache.profiles.is_empty() {
            vec![unavailable_profile(profile_id)]
        } else {
            cache.profiles.values().map(project_profile).collect()
        };
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
        let (catalog_status, catalog_detail, extensions) = match legacy_catalog() {
            Ok(catalog) => match &self.legacy_assets {
                Ok(inventory) => {
                    let verified = inventory.verified_count();
                    (
                        if verified == catalog.len() {
                            "ready"
                        } else {
                            "warning"
                        },
                        format!(
                            "{verified}/{} pinned routes and patterns verified",
                            catalog.len()
                        ),
                        project_extensions(catalog, selected_profile, Some(inventory)),
                    )
                }
                Err(error) => (
                    "blocked",
                    format!("Compatibility assets are unavailable: {error}"),
                    project_extensions(catalog, selected_profile, None),
                ),
            },
            Err(error) => (
                "blocked",
                format!("Compatibility catalog failed validation: {error}"),
                Vec::new(),
            ),
        };

        json!({
            "runId": run_id,
            "runState": run_state_label(state),
            "runStateReason": reason,
            "activeProfileId": profile_id.to_string(),
            "profiles": profile_projections,
            "onboardingComplete": selected_profile.is_some_and(|profile| profile.onboarding_complete),
            "safeMode": actual_run.is_some_and(|snapshot| snapshot.safe_mode),
            "session": roblox.session,
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
                    "detail": roblox.detail,
                    "status": roblox.status,
                },
                {
                    "id": "native-detectors",
                    "label": "Native route detectors",
                    "detail": "The input engine is installed, but normal runs stay blocked until reviewed field, hive, prompt, and combat detector assets are calibrated. Trusted legacy routes remain available through Extensions.",
                    "status": "warning",
                },
                {
                    "id": "budget",
                    "label": "Item safeguards",
                    "detail": budget_summary(selected_profile),
                    "status": "ready",
                },
                {
                    "id": "legacy-catalog",
                    "label": "Compatibility catalog",
                    "detail": catalog_detail,
                    "status": catalog_status,
                }
            ],
            "metrics": project_stats_metrics(cache.stats.as_ref()),
            "runHistory": cache.run_history.iter()
                .filter(|record| record.profile_id == profile_id)
                .map(|record| json!({
                "runId": record.run_id.to_string(),
                "profileId": record.profile_id.to_string(),
                "kind": record.kind,
                "startedAt": record.started_at.to_rfc3339(),
                "finishedAt": record.finished_at.to_rfc3339(),
                "finalState": record.final_state,
                "summary": record.summary,
                "stepsSucceeded": record.steps_succeeded,
                "stepsFailed": record.steps_failed,
            })).collect::<Vec<_>>(),
            "legacyInspection": cache.legacy_inspection.as_ref().map(|inspection| json!({
                "scriptId": inspection.script_id,
                "sha256": inspection.sha256,
                "bytes": inspection.bytes,
                "harnessPreview": inspection.harness_preview,
            })),
            "questScan": cache.quest_scan.as_ref().map(|scan| json!({
                "scannedAt": scan.scanned_at.to_rfc3339(),
                "giver": scan.giver,
                "questId": scan.quest_id,
                "questName": scan.quest_name,
                "barsComplete": scan.bars_complete,
                "recommendedFields": scan.recommended_fields,
                "notes": scan.notes,
            })),
            "timeline": cache.timeline.iter().collect::<Vec<_>>(),
            "queue": active_task.map_or_else(Vec::<Value>::new, |task| vec![json!({
                "id": "active-daemon-task",
                "label": task,
                "detail": "Reported by the automation daemon",
                "status": "active",
            })]),
            "features": selected_profile.map_or_else(Vec::<Value>::new, project_features),
            "extensions": extensions,
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
        if let Ok(inventory) = &self.legacy_assets {
            command.env("NECTARPILOT_LEGACY_ROOT", &inventory.layout.legacy_root);
            if let Some(assets_root) = &inventory.layout.assets_root {
                command.env("NECTARPILOT_ASSETS_ROOT", assets_root);
            }
            if let Some(autohotkey) = &inventory.layout.autohotkey {
                command.env("NECTARPILOT_AUTOHOTKEY_PATH", autohotkey);
            }
        }
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
    fn has_owned_daemon(&self) -> bool {
        self.owned_daemon
            .lock()
            .ok()
            .and_then(|mut owned| owned.as_mut().and_then(|daemon| daemon.is_running().ok()))
            .unwrap_or(false)
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

fn legacy_catalog() -> Result<&'static [LegacyCatalogEntry], String> {
    LEGACY_CATALOG
        .get_or_init(build_legacy_catalog)
        .as_ref()
        .map(Vec::as_slice)
        .map_err(Clone::clone)
}

fn build_legacy_catalog() -> Result<Vec<LegacyCatalogEntry>, String> {
    let manifests = [("route", ROUTE_MANIFEST), ("pattern", PATTERN_MANIFEST)];
    let mut catalog = Vec::new();
    let mut identifiers = std::collections::HashSet::new();
    for (expected_kind, source) in manifests {
        let manifest: LegacyManifest = serde_yaml::from_str(source)
            .map_err(|error| format!("invalid {expected_kind} manifest: {error}"))?;
        validate_legacy_manifest(&manifest, expected_kind)?;
        for entry in manifest.entries {
            let id = format!("legacy:{expected_kind}:{}", entry.legacy_source);
            if id.len() > 128 {
                return Err(format!("catalog identifier is too long: {id}"));
            }
            if !identifiers.insert(id.clone()) {
                return Err(format!("duplicate compatibility identifier: {id}"));
            }
            let display_name = legacy_display_name(&entry.legacy_source);
            catalog.push(LegacyCatalogEntry {
                id,
                kind: expected_kind.to_owned(),
                source: entry.legacy_source,
                display_name,
                sha256: entry.sha256,
                bytes: entry.bytes,
                status: entry.status,
                generated_asset: entry.generated_asset,
                unsupported_syntax: entry.issue_counts.unsupported_syntax,
                unsafe_capabilities: entry.issue_counts.unsafe_capabilities,
                invalid_values: entry.issue_counts.invalid_values,
            });
        }
    }
    Ok(catalog)
}

fn validate_legacy_manifest(manifest: &LegacyManifest, expected_kind: &str) -> Result<(), String> {
    if manifest.format_version != 1 {
        return Err(format!(
            "{} manifest uses unsupported format version {}",
            expected_kind, manifest.format_version
        ));
    }
    if manifest.kind != expected_kind {
        return Err(format!(
            "expected {expected_kind} manifest, received {}",
            manifest.kind
        ));
    }
    if manifest.entries.len() != manifest.total_files
        || manifest.safe_dsl_files + manifest.legacy_bridge_files != manifest.total_files
    {
        return Err(format!(
            "{expected_kind} manifest file counts do not reconcile"
        ));
    }
    let safe_count = manifest
        .entries
        .iter()
        .filter(|entry| entry.status == LegacyEntryStatus::SafeDsl)
        .count();
    let bridge_count = manifest.entries.len() - safe_count;
    if safe_count != manifest.safe_dsl_files || bridge_count != manifest.legacy_bridge_files {
        return Err(format!(
            "{expected_kind} manifest status counts do not reconcile"
        ));
    }
    for entry in &manifest.entries {
        let source = Path::new(&entry.legacy_source);
        let mut components = source.components();
        let starts_in_declared_directory = components.next().is_some_and(|component| {
            matches!(component, std::path::Component::Normal(value) if value == std::ffi::OsStr::new(&manifest.source_directory))
        });
        if !starts_in_declared_directory
            || !components.all(|component| matches!(component, std::path::Component::Normal(_)))
        {
            return Err(format!(
                "{expected_kind} manifest contains an unsafe source path: {}",
                entry.legacy_source
            ));
        }
        if entry.bytes == 0
            || entry.sha256.len() != 64
            || !entry
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(format!(
                "{expected_kind} manifest contains invalid integrity data for {}",
                entry.legacy_source
            ));
        }
        match entry.status {
            LegacyEntryStatus::SafeDsl => {
                if entry.requires_explicit_consent || entry.generated_asset.is_none() {
                    return Err(format!(
                        "safe DSL entry {} has inconsistent consent metadata",
                        entry.legacy_source
                    ));
                }
            }
            LegacyEntryStatus::LegacyBridgeRequired => {
                if !entry.requires_explicit_consent || entry.generated_asset.is_some() {
                    return Err(format!(
                        "legacy bridge entry {} has inconsistent consent metadata",
                        entry.legacy_source
                    ));
                }
            }
        }
    }
    Ok(())
}

fn legacy_display_name(source: &str) -> String {
    let stem = Path::new(source)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("Legacy asset");
    stem.split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut characters = part.chars();
            characters.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + characters.as_str()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn verify_legacy_assets(
    app: &AppHandle,
    catalog: &[LegacyCatalogEntry],
) -> Result<LegacyAssetInventory, String> {
    let mut layout = discover_trusted_asset_layout(app)?;
    let runtime_error = verify_autohotkey_runtime(&layout).err();
    if runtime_error.is_some() {
        layout.autohotkey = None;
    }
    let mut entries = HashMap::with_capacity(catalog.len());
    for entry in catalog {
        let status = verify_catalog_entry(&layout, entry, runtime_error.as_deref())
            .map_or_else(AssetVerification::Unavailable, |()| {
                AssetVerification::Verified
            });
        entries.insert(entry.id.clone(), status);
    }
    Ok(LegacyAssetInventory { layout, entries })
}

fn discover_trusted_asset_layout(app: &AppHandle) -> Result<TrustedAssetLayout, String> {
    let executable = std::env::current_exe()
        .map_err(|error| format!("could not resolve executable for asset discovery: {error}"))?;
    let executable_directory = executable
        .parent()
        .ok_or_else(|| "desktop executable has no parent directory".to_owned())?;
    let resource_directory = app.path().resource_dir().ok();
    let mut candidates = Vec::new();
    if let Some(resources) = &resource_directory {
        candidates.push(resources.clone());
        candidates.push(resources.join("legacy"));
    }
    candidates.push(executable_directory.join("legacy"));
    candidates.push(executable_directory.to_path_buf());

    if cfg!(debug_assertions) {
        for ancestor in executable.ancestors() {
            if ancestor.join(".git").is_dir()
                && ancestor.join("Cargo.toml").is_file()
                && ancestor.join("assets").is_dir()
            {
                candidates.push(ancestor.to_path_buf());
            }
        }
    }

    for candidate in candidates {
        if !candidate.join("paths").is_dir() || !candidate.join("patterns").is_dir() {
            continue;
        }
        let Ok(legacy_root) = candidate.canonicalize() else {
            continue;
        };
        let assets_root = [
            candidate.join("assets"),
            candidate
                .parent()
                .map_or_else(PathBuf::new, |parent| parent.join("assets")),
            resource_directory
                .as_ref()
                .map_or_else(PathBuf::new, |resources| resources.join("assets")),
        ]
        .into_iter()
        .find(|root| {
            root.join("routes").join("_legacy-manifest.yaml").is_file()
                && root
                    .join("patterns")
                    .join("_legacy-manifest.yaml")
                    .is_file()
        })
        .and_then(|root| root.canonicalize().ok());
        let autohotkey = [
            candidate.join("AutoHotkey64.exe"),
            candidate.join("submacros").join("AutoHotkey64.exe"),
            candidate
                .parent()
                .map_or_else(PathBuf::new, |parent| parent.join("AutoHotkey64.exe")),
            candidate.parent().map_or_else(PathBuf::new, |parent| {
                parent.join("submacros").join("AutoHotkey64.exe")
            }),
        ]
        .into_iter()
        .find(|path| path.is_file())
        .and_then(|path| path.canonicalize().ok());
        return Ok(TrustedAssetLayout {
            legacy_root,
            assets_root,
            autohotkey,
        });
    }
    Err("no packaged, portable, or development compatibility asset root was found".to_owned())
}

fn verify_catalog_entry(
    layout: &TrustedAssetLayout,
    entry: &LegacyCatalogEntry,
    runtime_error: Option<&str>,
) -> Result<(), String> {
    if entry.status == LegacyEntryStatus::LegacyBridgeRequired
        && let Some(error) = runtime_error
    {
        return Err(error.to_owned());
    }
    let source = confined_asset_path(&layout.legacy_root, Path::new(&entry.source))
        .map_err(|reason| format!("{}: {reason}", entry.source))?;
    let metadata = source
        .metadata()
        .map_err(|_| format!("{} is unavailable", entry.source))?;
    if metadata.len() != entry.bytes {
        return Err(format!("{} size does not match its manifest", entry.source));
    }
    let actual_digest = sha256_file(&source)
        .map_err(|_| format!("{} could not be integrity checked", entry.source))?;
    if actual_digest != entry.sha256 {
        return Err(format!(
            "{} digest does not match its manifest",
            entry.source
        ));
    }

    if let Some(generated_asset) = &entry.generated_asset {
        let assets_root = layout
            .assets_root
            .as_ref()
            .ok_or_else(|| "the validated DSL asset directory is unavailable".to_owned())?;
        let relative = Path::new("patterns").join(generated_asset);
        confined_asset_path(assets_root, &relative)
            .map_err(|reason| format!("generated asset {generated_asset}: {reason}"))?;
    }
    Ok(())
}

fn verify_autohotkey_runtime(layout: &TrustedAssetLayout) -> Result<(), String> {
    let runtime = layout
        .autohotkey
        .as_ref()
        .ok_or_else(|| "the contained AutoHotkey runtime is not packaged".to_owned())?;
    let actual = sha256_file(runtime).map_err(|_| {
        "the contained AutoHotkey runtime could not be integrity checked".to_owned()
    })?;
    if actual != AUTOHOTKEY64_SHA256 {
        return Err("the contained AutoHotkey runtime digest does not match its pin".to_owned());
    }
    Ok(())
}

fn confined_asset_path(root: &Path, relative: &Path) -> Result<PathBuf, String> {
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err("path is not a confined relative asset path".to_owned());
    }
    let canonical_root = root
        .canonicalize()
        .map_err(|_| "trusted asset root is unavailable".to_owned())?;
    let canonical = canonical_root
        .join(relative)
        .canonicalize()
        .map_err(|_| "asset file is unavailable".to_owned())?;
    if !canonical.starts_with(&canonical_root) || !canonical.is_file() {
        return Err("asset escapes its trusted root or is not a file".to_owned());
    }
    Ok(canonical)
}

fn sha256_file(path: &Path) -> Result<String, std::io::Error> {
    let mut file = std::fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    let digest = digest.finalize();
    Ok(format!("{digest:x}"))
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

#[cfg(windows)]
fn project_roblox_session() -> RobloxDiscoveryProjection {
    match discover_roblox_clients() {
        Ok(candidates) => {
            let visible = candidates
                .iter()
                .filter_map(|candidate| candidate.window.map(|window| (candidate.pid, window)))
                .collect::<Vec<_>>();
            match visible.as_slice() {
                [] if candidates.is_empty() => disconnected_roblox_projection(
                    "No official RobloxPlayerBeta process is open",
                    "checking",
                ),
                [] => disconnected_roblox_projection(
                    "RobloxPlayerBeta is starting or has no usable client window yet",
                    "checking",
                ),
                [(pid, window)] => {
                    let geometry = window.geometry;
                    let minimized = geometry.minimized;
                    RobloxDiscoveryProjection {
                        session: json!({
                            "connected": true,
                            "processName": "RobloxPlayerBeta",
                            "pid": pid.get(),
                            "windowTitle": "Verified Roblox client window",
                            "resolution": format!("{}×{}", geometry.client.width, geometry.client.height),
                            "dpi": geometry.dpi,
                            "foreground": window.is_foreground,
                            "calibration": if minimized { "low" } else if window.is_foreground { "high" } else { "medium" },
                        }),
                        detail: if minimized {
                            format!(
                                "RobloxPlayerBeta PID {} is open but minimized; restore it before a safe run",
                                pid.get()
                            )
                        } else if window.is_foreground {
                            format!(
                                "Verified RobloxPlayerBeta PID {} is foreground at {}×{}",
                                pid.get(),
                                geometry.client.width,
                                geometry.client.height
                            )
                        } else {
                            format!(
                                "Verified RobloxPlayerBeta PID {} is open; focus it before a safe run",
                                pid.get()
                            )
                        },
                        status: if minimized { "warning" } else { "ready" },
                    }
                }
                _ => disconnected_roblox_projection(
                    "Multiple RobloxPlayerBeta client windows are open; explicitly adopt one before automation",
                    "blocked",
                ),
            }
        }
        Err(error) => disconnected_roblox_projection(
            &format!("Could not inspect Roblox processes: {error}"),
            "warning",
        ),
    }
}

#[cfg(not(windows))]
fn project_roblox_session() -> RobloxDiscoveryProjection {
    disconnected_roblox_projection(
        "Roblox session discovery is available only on Windows",
        "blocked",
    )
}

fn disconnected_roblox_projection(detail: &str, status: &'static str) -> RobloxDiscoveryProjection {
    RobloxDiscoveryProjection {
        session: json!({
            "connected": false,
            "processName": Value::Null,
            "pid": Value::Null,
            "windowTitle": Value::Null,
            "resolution": Value::Null,
            "dpi": Value::Null,
            "foreground": false,
            "calibration": Value::Null,
        }),
        detail: detail.to_owned(),
        status,
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
        "readiness": [], "metrics": [], "runHistory": [], "legacyInspection": Value::Null, "questScan": Value::Null,
        "timeline": [], "queue": [], "features": [], "extensions": [], "logs": [],
        "updatedAt": Utc::now(),
    })
}

fn clear_profile_scoped_projections(cache: &mut BridgeCache) {
    cache.run_history.clear();
    cache.legacy_inspection = None;
    cache.quest_scan = None;
}

const fn should_refresh_run_history(state: RunState) -> bool {
    matches!(
        state,
        RunState::Idle | RunState::Faulted | RunState::NeedsAttention
    )
}

/// Projects the latest passive stats sample into dashboard stat tiles.
/// Unconfident readings surface as em-dashes rather than stale or guessed
/// numbers.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "rates and minutes are clamped non-negative and bounded far below u64"
)]
fn project_stats_metrics(stats: Option<&StatsSample>) -> Vec<Value> {
    let Some(sample) = stats else {
        return Vec::new();
    };
    let honey = sample
        .honey
        .map_or_else(|| "—".to_owned(), format_grouped_number);
    let rate = sample.honey_per_hour.map_or_else(
        || "—".to_owned(),
        |value| format!("{}/h", format_grouped_number(value.round().max(0.0) as u64)),
    );
    let minutes = sample.session_minutes.round().max(0.0) as u64;
    vec![
        json!({
            "id": "honey",
            "label": "Honey (HUD)",
            "value": honey,
            "tone": "gold",
        }),
        json!({
            "id": "honey-rate",
            "label": "Honey per hour",
            "value": rate,
            "tone": "green",
        }),
        json!({
            "id": "session-minutes",
            "label": "Stats session",
            "value": format!("{}h {:02}m", minutes / 60, minutes % 60),
            "tone": "blue",
        }),
    ]
}

fn format_grouped_number(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(character);
    }
    grouped
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

fn project_extensions(
    catalog: &[LegacyCatalogEntry],
    profile: Option<&Profile>,
    inventory: Option<&LegacyAssetInventory>,
) -> Vec<Value> {
    catalog
        .iter()
        .map(|entry| {
            let verification = inventory.map_or_else(
                || AssetVerification::Unavailable("compatibility assets are unavailable".to_owned()),
                |inventory| inventory.verification(&entry.id),
            );
            let stored_digest = profile
                .and_then(|profile| profile.trusted_extensions.get(&entry.id));
            let digest_changed = stored_digest.is_some_and(|digest| digest != &entry.sha256);
            let trust = match (&entry.status, &verification, stored_digest) {
                (LegacyEntryStatus::SafeDsl, AssetVerification::Verified, _) => "built_in",
                (_, AssetVerification::Unavailable(_), _) => "blocked",
                (LegacyEntryStatus::LegacyBridgeRequired, AssetVerification::Verified, Some(digest))
                    if digest == &entry.sha256 => "trusted",
                (LegacyEntryStatus::LegacyBridgeRequired, AssetVerification::Verified, _) => {
                    "review_required"
                }
            };
            let unavailable_reason = match &verification {
                AssetVerification::Verified => None,
                AssetVerification::Unavailable(reason) => Some(reason.as_str()),
            };
            let description = if let Some(reason) = unavailable_reason {
                format!(
                    "{} {} is blocked: {reason}.",
                    capitalize(&entry.kind),
                    entry.source
                )
            } else if entry.status == LegacyEntryStatus::SafeDsl {
                format!(
                    "Converted from {} into the validated {} DSL asset. Preview only; native DSL execution is not connected yet.",
                    entry.source,
                    entry.generated_asset.as_deref().unwrap_or("NectarPilot")
                )
            } else if digest_changed {
                format!(
                    "The pinned digest changed since the last review. Re-review this contained legacy {} before use.",
                    entry.kind
                )
            } else {
                format!(
                    "Pinned legacy {} ({} bytes; {} unsupported, {} unsafe-capability, {} invalid-value findings) requiring the opt-in compatibility worker.",
                    entry.kind,
                    entry.bytes,
                    entry.unsupported_syntax,
                    entry.unsafe_capabilities,
                    entry.invalid_values
                )
            };
            let enabled = entry.status == LegacyEntryStatus::LegacyBridgeRequired
                && trust == "trusted"
                && profile.is_some_and(|profile| profile.automation.features.custom_extensions);
            let permissions = if entry.status == LegacyEntryStatus::SafeDsl {
                vec!["Validated movement plan preview"]
            } else {
                vec![
                    "Contained legacy AHK runner",
                    "Verified keyboard input",
                    "Verified mouse input",
                ]
            };
            json!({
                "id": entry.id,
                "name": format!("{} · {}", capitalize(&entry.kind), entry.display_name),
                "author": "Natro Team contributors",
                "version": LEGACY_VERSION,
                "description": description,
                "digest": format!("sha256:{}", entry.sha256),
                "trust": trust,
                "permissions": permissions,
                "enabled": enabled,
                "executionMode": if entry.status == LegacyEntryStatus::SafeDsl { "native_preview" } else { "legacy_bridge" },
            })
        })
        .collect()
}

fn capitalize(value: &str) -> String {
    let mut characters = value.chars();
    characters.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + characters.as_str()
    })
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
    use std::{collections::HashMap, path::PathBuf};

    use nectarpilot_contracts::Profile;

    use super::{
        AssetVerification, LegacyAssetInventory, LegacyEntryStatus, TrustedAssetLayout,
        budget_summary, legacy_catalog, project_extensions, project_profile, run_state_label,
        safe_ui_settings, should_refresh_run_history, verify_autohotkey_runtime,
        verify_catalog_entry,
    };

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

    #[test]
    fn every_settled_state_refreshes_run_history() {
        assert!(should_refresh_run_history(
            nectarpilot_contracts::RunState::Idle
        ));
        assert!(should_refresh_run_history(
            nectarpilot_contracts::RunState::Faulted
        ));
        assert!(should_refresh_run_history(
            nectarpilot_contracts::RunState::NeedsAttention
        ));
        assert!(!should_refresh_run_history(
            nectarpilot_contracts::RunState::Running
        ));
        assert!(!should_refresh_run_history(
            nectarpilot_contracts::RunState::Paused
        ));
    }

    #[test]
    fn bundled_legacy_manifests_form_a_complete_pinned_catalog() {
        let catalog = legacy_catalog().expect("catalog");
        assert_eq!(catalog.len(), 103);
        assert_eq!(
            catalog
                .iter()
                .filter(|entry| entry.status == LegacyEntryStatus::LegacyBridgeRequired)
                .count(),
            102
        );
        assert_eq!(
            catalog
                .iter()
                .filter(|entry| entry.status == LegacyEntryStatus::SafeDsl)
                .count(),
            1
        );
        assert!(catalog.iter().all(|entry| entry.sha256.len() == 64));
    }

    #[test]
    fn safe_dsl_projection_is_preview_only() {
        let catalog = legacy_catalog().expect("catalog");
        let entries = catalog
            .iter()
            .map(|entry| (entry.id.clone(), AssetVerification::Verified))
            .collect::<HashMap<_, _>>();
        let inventory = LegacyAssetInventory {
            layout: TrustedAssetLayout {
                legacy_root: PathBuf::new(),
                assets_root: None,
                autohotkey: None,
            },
            entries,
        };
        let safe_dsl = catalog
            .iter()
            .find(|entry| entry.status == LegacyEntryStatus::SafeDsl)
            .expect("safe DSL catalog entry");
        let projected = project_extensions(catalog, None, Some(&inventory));
        let preview = projected
            .iter()
            .find(|entry| entry["id"] == safe_dsl.id)
            .expect("projected safe DSL entry");

        assert_eq!(preview["trust"], "built_in");
        assert_eq!(preview["enabled"], false);
        assert_eq!(preview["executionMode"], "native_preview");
        assert!(
            preview["description"]
                .as_str()
                .is_some_and(|description| description.contains("Preview only"))
        );
    }

    #[test]
    fn extension_projection_uses_profile_digest_trust() {
        let catalog = legacy_catalog().expect("catalog");
        let entries = catalog
            .iter()
            .map(|entry| (entry.id.clone(), AssetVerification::Verified))
            .collect::<HashMap<_, _>>();
        let inventory = LegacyAssetInventory {
            layout: TrustedAssetLayout {
                legacy_root: PathBuf::new(),
                assets_root: None,
                autohotkey: None,
            },
            entries,
        };
        let legacy = catalog
            .iter()
            .find(|entry| entry.status == LegacyEntryStatus::LegacyBridgeRequired)
            .expect("legacy entry");
        let mut profile = Profile::new("Safe");
        let before = project_extensions(catalog, Some(&profile), Some(&inventory));
        let projected = before
            .iter()
            .find(|entry| entry["id"] == legacy.id)
            .expect("projected entry");
        assert_eq!(projected["trust"], "review_required");

        profile
            .trusted_extensions
            .insert(legacy.id.clone(), legacy.sha256.clone());
        let after = project_extensions(catalog, Some(&profile), Some(&inventory));
        let projected = after
            .iter()
            .find(|entry| entry["id"] == legacy.id)
            .expect("projected entry");
        assert_eq!(projected["trust"], "trusted");
    }

    #[test]
    fn development_compatibility_assets_match_every_integrity_pin() {
        let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let layout = TrustedAssetLayout {
            legacy_root: workspace.clone(),
            assets_root: Some(workspace.join("assets")),
            autohotkey: Some(workspace.join("submacros/AutoHotkey64.exe")),
        };
        verify_autohotkey_runtime(&layout).expect("pinned AutoHotkey runtime");
        for entry in legacy_catalog().expect("catalog") {
            verify_catalog_entry(&layout, entry, None)
                .unwrap_or_else(|error| panic!("{} failed verification: {error}", entry.source));
        }
    }
}
