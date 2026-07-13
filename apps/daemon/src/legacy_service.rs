//! Bounded, hash-pinned execution of the imported `AutoHotkey` compatibility assets.
//!
//! This module deliberately does not discover scripts from the filesystem.  The only
//! executable identifiers are the ones embedded in the checked-in manifests, and a
//! request is checked against its manifest hash, byte length, profile-bound trust,
//! an all-zero spending policy, and the pinned `AutoHotkey` interpreter hash before a
//! child can be created.

use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use chrono::Utc;
use nectarpilot_contracts::{ActionOutcome, ActionResult, Profile};
use nectarpilot_core::{
    AutomationError, BUILTIN_RESET_SCRIPT_ID, LegacyExecutionPort as CoreLegacyExecutionPort,
    TaskContext,
};
use nectarpilot_legacy::{
    AssetCatalog, AssetKind, AssetStatus, ExecutionOutcome, ExecutionRequest, FragmentKind,
    HarnessError, HarnessSettings, LegacyConsent, LegacyError, LegacyRunner, MoveMethod,
    RunnerPolicy, SupportCatalog, SupportCatalogError, generate_reset_script, generate_walk_script,
    verify_support_files,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

#[cfg(windows)]
use nectarpilot_platform::process::ProcessController;
#[cfg(windows)]
use nectarpilot_platform::session::WindowSnapshot;
#[cfg(windows)]
use nectarpilot_platform::{DiscoveredRobloxClient, discover_roblox_clients};
use nectarpilot_platform::{process::ProcessIdentity, session::SessionTarget};

/// The exact `AutoHotkey` binary imported with Natro Macro v1.1.2.
pub const PINNED_AUTOHOTKEY64_SHA256: &str =
    "37ff15a23a98f0a658298e21f1873ca896a05208810bf796f90ca212ee07c7b1";

/// A legacy script can never outlive this bounded execution window.
pub const LEGACY_EXECUTION_MAXIMUM: Duration = Duration::from_secs(30 * 60);

const ROUTE_MANIFEST: &str = include_str!("../../../assets/routes/_legacy-manifest.yaml");
const PATTERN_MANIFEST: &str = include_str!("../../../assets/patterns/_legacy-manifest.yaml");
const SUPPORT_MANIFEST: &str = include_str!("../../../assets/legacy-support/_legacy-manifest.yaml");
const LEGACY_ROOT_ENV: &str = "NECTARPILOT_LEGACY_ROOT";
const AUTOHOTKEY_PATH_ENV: &str = "NECTARPILOT_AUTOHOTKEY_PATH";

/// A UI-safe view of a manifest-pinned compatibility asset.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacyAssetDescriptor {
    /// Stable ID accepted by this service, for example
    /// `legacy:route:paths/gtb-blue.ahk`.
    pub id: String,
    pub legacy_source: String,
    pub sha256: String,
    pub bytes: u64,
    /// `false` for a converted native DSL entry.  Such entries are intentionally
    /// visible but cannot be executed through this compatibility bridge.
    pub requires_legacy_bridge: bool,
}

#[derive(Clone, Debug)]
struct PinnedAsset {
    descriptor: LegacyAssetDescriptor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AdoptedRobloxClient {
    identity: ProcessIdentity,
    target: SessionTarget,
}

/// Errors are intentionally specific so the UI can explain why a compatibility
/// asset was not started without falling back to permissive behavior.
#[derive(Debug, Error)]
pub enum LegacyCompatibilityError {
    #[error("the embedded legacy manifest is invalid: {0}")]
    Manifest(String),
    #[error("legacy compatibility root does not exist or cannot be resolved: {path}: {source}")]
    RootResolution {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("legacy asset {id:?} is not a manifest-pinned route or pattern")]
    UnknownAsset { id: String },
    #[error("legacy asset {id:?} was converted to the native DSL and must not use AutoHotkey")]
    NativeDslAsset { id: String },
    #[error("legacy asset path escaped the configured compatibility root: {path}")]
    OutsideRoot { path: PathBuf },
    #[error("legacy asset filesystem operation failed for {path}: {source}")]
    AssetIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "legacy asset {id:?} did not match its imported manifest (expected {expected_bytes} bytes / {expected_sha256}, got {actual_bytes} bytes / {actual_sha256})"
    )]
    AssetTrustMismatch {
        id: String,
        expected_sha256: String,
        expected_bytes: u64,
        actual_sha256: String,
        actual_bytes: u64,
    },
    #[error("profile did not explicitly trust legacy asset {id:?} at digest {expected_sha256}")]
    ProfileTrustRequired { id: String, expected_sha256: String },
    #[error(
        "profile trust for legacy asset {id:?} is not an exact match (expected {expected_sha256}, got {actual_sha256})"
    )]
    ProfileTrustMismatch {
        id: String,
        expected_sha256: String,
        actual_sha256: String,
    },
    #[error(
        "legacy execution requires every valuable-item budget to be zero; non-zero budget: {name}"
    )]
    ValuableBudgetEnabled { name: String },
    #[error("legacy execution is blocked while purchases are enabled")]
    PurchasesEnabled,
    #[error("legacy execution is blocked while donations are enabled")]
    DonationsEnabled,
    #[error("legacy execution is blocked while trades are enabled")]
    TradesEnabled,
    #[error("legacy execution is blocked while Discord or any Discord permission is enabled")]
    DiscordEnabled,
    #[error("legacy execution requires exactly one visible RobloxPlayerBeta client; found {count}")]
    RobloxClientAmbiguous { count: usize },
    #[error("legacy execution requires the verified Roblox client to be foreground and restored")]
    RobloxClientNotForeground,
    #[error(
        "the adopted Roblox client is no longer the exact process/window selected at preflight"
    )]
    RobloxClientChanged,
    #[error("the adopted Roblox client process is no longer available")]
    RobloxClientUnavailable,
    #[error("could not inspect Roblox clients before legacy execution: {0}")]
    RobloxDiscovery(String),
    #[error("invalid imported movement setting {name}={value:?}: {reason}")]
    InvalidMovementSetting {
        name: &'static str,
        value: String,
        reason: &'static str,
    },
    #[error("legacy support library failed verification: {0}")]
    SupportLibrary(#[from] SupportCatalogError),
    #[error("legacy harness generation failed: {0}")]
    Harness(#[from] HarnessError),
    #[error("legacy harness staging failed at {path}: {source}")]
    HarnessIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("pinned AutoHotkey64.exe is missing or unreadable at {path}: {source}")]
    InterpreterIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "AutoHotkey64.exe did not match the pinned import digest (expected {expected_sha256}, got {actual_sha256})"
    )]
    InterpreterTrustMismatch {
        expected_sha256: &'static str,
        actual_sha256: String,
    },
    #[error("legacy runner rejected the execution: {0}")]
    Runner(#[from] LegacyError),
}

/// Daemon-owned implementation of the core legacy execution port.
#[derive(Clone, Debug)]
pub struct LegacyCompatibilityService {
    root: PathBuf,
    assets: BTreeMap<String, PinnedAsset>,
    support: SupportCatalog,
    runner: LegacyRunner,
    adopted_client: Arc<Mutex<Option<AdoptedRobloxClient>>>,
}

/// The resolved legacy compatibility root: `NECTARPILOT_LEGACY_ROOT` when
/// explicitly set, otherwise the compile-time repository root — never the
/// process working directory.
pub fn compatibility_root() -> PathBuf {
    env::var_os(LEGACY_ROOT_ENV).map_or_else(safe_development_root, PathBuf::from)
}

impl LegacyCompatibilityService {
    /// Builds the service from [`compatibility_root`].
    pub fn from_environment() -> Result<Self, LegacyCompatibilityError> {
        Self::from_root(compatibility_root())
    }

    /// Builds the service from a packaged resource root or a controlled test root.
    /// The root is canonicalized once; individual files are canonicalized and
    /// checked again before they are read or executed.
    pub fn from_root(root: impl AsRef<Path>) -> Result<Self, LegacyCompatibilityError> {
        let requested_root = root.as_ref().to_path_buf();
        let root = fs::canonicalize(&requested_root).map_err(|source| {
            LegacyCompatibilityError::RootResolution {
                path: requested_root,
                source,
            }
        })?;
        let assets = embedded_assets()?;
        let support: SupportCatalog = serde_yaml::from_str(SUPPORT_MANIFEST)
            .map_err(|error| LegacyCompatibilityError::Manifest(error.to_string()))?;
        let runner = LegacyRunner::new(RunnerPolicy {
            maximum_timeout: LEGACY_EXECUTION_MAXIMUM,
            ..RunnerPolicy::default()
        });
        Ok(Self {
            root,
            assets,
            support,
            runner,
            adopted_client: Arc::new(Mutex::new(None)),
        })
    }

    fn pinned_asset(&self, id: &str) -> Result<&PinnedAsset, LegacyCompatibilityError> {
        self.assets
            .get(id)
            .ok_or_else(|| LegacyCompatibilityError::UnknownAsset { id: id.to_owned() })
    }

    fn checked_path(&self, logical_source: &str) -> Result<PathBuf, LegacyCompatibilityError> {
        let requested = self.root.join(logical_source);
        let canonical =
            fs::canonicalize(&requested).map_err(|source| LegacyCompatibilityError::AssetIo {
                path: requested,
                source,
            })?;
        if !canonical.starts_with(&self.root) {
            return Err(LegacyCompatibilityError::OutsideRoot { path: canonical });
        }
        Ok(canonical)
    }

    fn verified_interpreter(&self) -> Result<PathBuf, LegacyCompatibilityError> {
        let mut candidates = Vec::new();
        if let Some(path) = env::var_os(AUTOHOTKEY_PATH_ENV).map(PathBuf::from) {
            candidates.push(path);
        }
        candidates.extend([
            self.root.join("AutoHotkey64.exe"),
            self.root.join("submacros").join("AutoHotkey64.exe"),
        ]);
        let requested = candidates
            .into_iter()
            .find(|candidate| candidate.is_file())
            .unwrap_or_else(|| self.root.join("AutoHotkey64.exe"));
        let interpreter = fs::canonicalize(&requested).map_err(|source| {
            LegacyCompatibilityError::InterpreterIo {
                path: requested,
                source,
            }
        })?;
        if !interpreter.starts_with(&self.root) {
            return Err(LegacyCompatibilityError::OutsideRoot { path: interpreter });
        }
        let bytes =
            fs::read(&interpreter).map_err(|source| LegacyCompatibilityError::InterpreterIo {
                path: interpreter.clone(),
                source,
            })?;
        let actual_sha256 = hex::encode(Sha256::digest(&bytes));
        if actual_sha256 != PINNED_AUTOHOTKEY64_SHA256 {
            return Err(LegacyCompatibilityError::InterpreterTrustMismatch {
                expected_sha256: PINNED_AUTOHOTKEY64_SHA256,
                actual_sha256,
            });
        }
        Ok(interpreter)
    }

    fn require_safe_profile(
        profile: &Profile,
        asset: Option<&PinnedAsset>,
    ) -> Result<(), LegacyCompatibilityError> {
        let budgets = &profile.safety.item_budgets;
        for (name, budget) in [
            ("dice", budgets.dice),
            ("glitter", budgets.glitter),
            ("eggs", budgets.eggs),
            ("stickers", budgets.stickers),
            ("vouchers", budgets.vouchers),
            ("shrine_donations", budgets.shrine_donations),
        ] {
            if budget != 0 {
                return Err(LegacyCompatibilityError::ValuableBudgetEnabled {
                    name: name.to_owned(),
                });
            }
        }
        if let Some((name, _)) = budgets.other.iter().find(|(_, budget)| **budget != 0) {
            return Err(LegacyCompatibilityError::ValuableBudgetEnabled { name: name.clone() });
        }
        if profile.safety.purchases_enabled {
            return Err(LegacyCompatibilityError::PurchasesEnabled);
        }
        if profile.safety.donations_enabled {
            return Err(LegacyCompatibilityError::DonationsEnabled);
        }
        if profile.safety.trades_enabled {
            return Err(LegacyCompatibilityError::TradesEnabled);
        }
        let permissions = &profile.discord.permissions;
        if profile.discord.enabled
            || permissions.status
            || permissions.macro_control
            || permissions.settings
            || permissions.screenshots
            || permissions.remote_input
            || permissions.extension_import
            || permissions.system_power
        {
            return Err(LegacyCompatibilityError::DiscordEnabled);
        }

        let Some(asset) = asset else {
            return Ok(());
        };
        match profile.trusted_extensions.get(&asset.descriptor.id) {
            Some(actual_sha256) if actual_sha256 == &asset.descriptor.sha256 => Ok(()),
            Some(actual_sha256) => Err(LegacyCompatibilityError::ProfileTrustMismatch {
                id: asset.descriptor.id.clone(),
                expected_sha256: asset.descriptor.sha256.clone(),
                actual_sha256: actual_sha256.clone(),
            }),
            None => Err(LegacyCompatibilityError::ProfileTrustRequired {
                id: asset.descriptor.id.clone(),
                expected_sha256: asset.descriptor.sha256.clone(),
            }),
        }
    }

    fn require_approved_digest(
        asset: &PinnedAsset,
        approved_sha256: &str,
    ) -> Result<(), LegacyCompatibilityError> {
        let approved = approved_sha256
            .strip_prefix("sha256:")
            .unwrap_or(approved_sha256);
        if approved.eq_ignore_ascii_case(&asset.descriptor.sha256) {
            Ok(())
        } else {
            Err(LegacyCompatibilityError::ProfileTrustMismatch {
                id: asset.descriptor.id.clone(),
                expected_sha256: asset.descriptor.sha256.clone(),
                actual_sha256: approved.to_owned(),
            })
        }
    }

    fn preflight_legacy(
        &self,
        profile: &Profile,
        id: &str,
        approved_sha256: &str,
    ) -> Result<(), LegacyCompatibilityError> {
        if id == BUILTIN_RESET_SCRIPT_ID {
            // The reset step contains no user script content; it still runs
            // every policy, environment, and support-library gate.
            Self::require_safe_profile(profile, None)?;
            self.require_foreground_roblox()?;
            verify_support_files(&self.root, &self.support)?;
            self.verified_interpreter()?;
            self.builtin_reset_script(profile)?;
            return Ok(());
        }
        let asset = self.pinned_asset(id)?;
        if !asset.descriptor.requires_legacy_bridge {
            return Err(LegacyCompatibilityError::NativeDslAsset { id: id.to_owned() });
        }
        Self::require_approved_digest(asset, approved_sha256)?;
        Self::require_safe_profile(profile, Some(asset))?;
        self.require_foreground_roblox()?;
        verify_support_files(&self.root, &self.support)?;
        self.verified_interpreter()?;
        // Prove the complete walk script can be generated before reporting a
        // clean preflight, so Start never discovers a harness problem mid-run.
        let (fragment, kind) = self.verified_fragment(id)?;
        generate_walk_script(&self.root, kind, &fragment, &harness_settings(profile)?)?;
        Ok(())
    }

    fn builtin_reset_script(&self, profile: &Profile) -> Result<String, LegacyCompatibilityError> {
        Ok(generate_reset_script(
            &self.root,
            &harness_settings(profile)?,
            profile.automation.session.convert_wait_seconds,
        )?)
    }

    /// Reads a manifest-pinned fragment and re-checks its digest on the exact
    /// bytes that will be embedded, closing the read-then-use gap.
    fn verified_fragment(
        &self,
        id: &str,
    ) -> Result<(String, FragmentKind), LegacyCompatibilityError> {
        let asset = self.pinned_asset(id)?;
        let path = self.checked_path(&asset.descriptor.legacy_source)?;
        let bytes = fs::read(&path).map_err(|source| LegacyCompatibilityError::AssetIo {
            path: path.clone(),
            source,
        })?;
        let actual_sha256 = hex::encode(Sha256::digest(&bytes));
        let actual_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if actual_sha256 != asset.descriptor.sha256 || actual_bytes != asset.descriptor.bytes {
            return Err(LegacyCompatibilityError::AssetTrustMismatch {
                id: id.to_owned(),
                expected_sha256: asset.descriptor.sha256.clone(),
                expected_bytes: asset.descriptor.bytes,
                actual_sha256,
                actual_bytes,
            });
        }
        let fragment = String::from_utf8(bytes)
            .map_err(|_| LegacyCompatibilityError::Manifest(format!("{id} is not valid UTF-8")))?;
        let kind = if id.starts_with("legacy:pattern:") {
            FragmentKind::Pattern
        } else {
            FragmentKind::Route
        };
        Ok((fragment, kind))
    }

    /// Writes the generated walk script into the daemon's private run
    /// directory. The runner re-hashes this exact file immediately before
    /// process creation, so a swapped file can never launch.
    fn stage_harness(script: &str) -> Result<PathBuf, LegacyCompatibilityError> {
        let directory = env::var_os("LOCALAPPDATA")
            .map_or_else(env::temp_dir, PathBuf::from)
            .join("NectarPilot")
            .join("legacy-runs");
        fs::create_dir_all(&directory).map_err(|source| LegacyCompatibilityError::HarnessIo {
            path: directory.clone(),
            source,
        })?;
        let path = directory.join(format!("run-{}.ahk", uuid::Uuid::new_v4()));
        fs::write(&path, script).map_err(|source| LegacyCompatibilityError::HarnessIo {
            path: path.clone(),
            source,
        })?;
        Ok(path)
    }

    async fn execute_verified(
        &self,
        profile: &Profile,
        id: &str,
        approved_sha256: &str,
        cancellation: CancellationToken,
    ) -> Result<ExecutionOutcome, LegacyCompatibilityError> {
        self.preflight_legacy(profile, id, approved_sha256)?;
        let interpreter = self.verified_interpreter()?;
        // Legacy fragments are not standalone scripts: Natro always wrapped
        // them in a generated walk script defining nm_Walk, the movement keys,
        // ramp/cannon travel, and the Gdip/Roblox libraries. Reproduce that
        // exact environment, then execute the wrapper under containment.
        let script_text = if id == BUILTIN_RESET_SCRIPT_ID {
            self.builtin_reset_script(profile)?
        } else {
            let (fragment, kind) = self.verified_fragment(id)?;
            generate_walk_script(&self.root, kind, &fragment, &harness_settings(profile)?)?
        };
        let staged = Self::stage_harness(&script_text)?;
        let report = match self.runner.inspect(&staged) {
            Ok(report) => report,
            Err(error) => {
                let _ = fs::remove_file(&staged);
                return Err(error.into());
            }
        };
        let request = ExecutionRequest {
            interpreter,
            interpreter_arguments: vec!["/ErrorStdOut".into()],
            script: staged.clone(),
            consent: Some(LegacyConsent::acknowledge_for(&report.trust)),
            timeout: LEGACY_EXECUTION_MAXIMUM,
        };
        let outcome = self.runner.execute(request, cancellation).await;
        let _ = fs::remove_file(&staged);
        outcome.map_err(Into::into)
    }
}

/// Movement environment for the harness, taken from the profile's imported
/// Natro INI snapshot when present and Natro's stock defaults only when a
/// setting was genuinely absent. Present-but-invalid values fail preflight so
/// the user can repair the preserved import instead of running with a silent
/// behavioral change.
fn harness_settings(profile: &Profile) -> Result<HarnessSettings, LegacyCompatibilityError> {
    let mut settings = HarnessSettings::default();
    let Some(snapshot) = profile.legacy.as_ref() else {
        return Ok(settings);
    };
    for source in &snapshot.sources {
        let Some(section) = source.sections.get("Settings") else {
            continue;
        };
        if let Some(value) = section.get("MoveMethod") {
            settings.move_method = match value.trim() {
                candidate if candidate.eq_ignore_ascii_case("walk") => MoveMethod::Walk,
                candidate if candidate.eq_ignore_ascii_case("cannon") => MoveMethod::Cannon,
                _ => {
                    return Err(invalid_movement_setting(
                        "MoveMethod",
                        value,
                        "expected Walk or Cannon",
                    ));
                }
            };
        }
        if let Some(value) = section.get("HiveSlot") {
            settings.hive_slot = value
                .trim()
                .parse::<u8>()
                .ok()
                .filter(|parsed| (1..=6).contains(parsed))
                .ok_or_else(|| {
                    invalid_movement_setting("HiveSlot", value, "expected an integer from 1 to 6")
                })?;
        }
        if let Some(value) = section.get("HiveBees") {
            settings.hive_bees = value
                .trim()
                .parse::<u8>()
                .ok()
                .filter(|parsed| *parsed <= 50)
                .ok_or_else(|| {
                    invalid_movement_setting("HiveBees", value, "expected an integer from 0 to 50")
                })?;
        }
        if let Some(value) = section.get("KeyDelay") {
            settings.key_delay = value
                .trim()
                .parse::<u16>()
                .ok()
                .filter(|parsed| *parsed <= 1000)
                .ok_or_else(|| {
                    invalid_movement_setting(
                        "KeyDelay",
                        value,
                        "expected an integer from 0 to 1000",
                    )
                })?;
        }
        if let Some(value) = section.get("MoveSpeedNum") {
            settings.move_speed = value
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|parsed| parsed.is_finite() && (10.0..=200.0).contains(parsed))
                .ok_or_else(|| {
                    invalid_movement_setting(
                        "MoveSpeedNum",
                        value,
                        "expected a finite number from 10.0 to 200.0",
                    )
                })?;
        }
        if let Some(value) = section.get("NewWalk") {
            settings.new_walk = match value.trim() {
                "0" => false,
                "1" => true,
                _ => {
                    return Err(invalid_movement_setting(
                        "NewWalk",
                        value,
                        "expected 0 or 1",
                    ));
                }
            };
        }
    }
    Ok(settings)
}

fn invalid_movement_setting(
    name: &'static str,
    value: &str,
    reason: &'static str,
) -> LegacyCompatibilityError {
    LegacyCompatibilityError::InvalidMovementSetting {
        name,
        value: value.to_owned(),
        reason,
    }
}

#[cfg(windows)]
fn terminate_exact_adopted<C: ProcessController>(
    controller: &mut C,
    adopted: &AdoptedRobloxClient,
) -> Result<(), String> {
    let Some(current) = controller
        .identity(adopted.identity.pid)
        .map_err(|error| error.to_string())?
    else {
        return Ok(());
    };
    if current != adopted.identity {
        return Err(format!(
            "adopted Roblox PID {} changed identity; termination refused",
            adopted.identity.pid.get()
        ));
    }
    controller
        .terminate_exact(&adopted.identity)
        .map_err(|error| error.to_string())
}

#[cfg(windows)]
fn exact_single_snapshot(
    clients: &[DiscoveredRobloxClient],
) -> Result<WindowSnapshot, LegacyCompatibilityError> {
    if clients.len() != 1 {
        return Err(LegacyCompatibilityError::RobloxClientAmbiguous {
            count: clients.len(),
        });
    }
    clients[0]
        .window
        .ok_or(LegacyCompatibilityError::RobloxClientUnavailable)
}

#[cfg(windows)]
impl LegacyCompatibilityService {
    fn adopted_client(&self) -> Result<AdoptedRobloxClient, LegacyCompatibilityError> {
        self.adopted_client
            .lock()
            .map_err(|_| {
                LegacyCompatibilityError::RobloxDiscovery("adopted-client lock was poisoned".into())
            })?
            .clone()
            .ok_or(LegacyCompatibilityError::RobloxClientUnavailable)
    }

    fn clear_adopted_client(&self) -> Result<(), LegacyCompatibilityError> {
        *self.adopted_client.lock().map_err(|_| {
            LegacyCompatibilityError::RobloxDiscovery("adopted-client lock was poisoned".into())
        })? = None;
        Ok(())
    }

    fn store_or_verify_adoption(
        &self,
        snapshot: WindowSnapshot,
    ) -> Result<(), LegacyCompatibilityError> {
        use nectarpilot_platform::windows_backend::WindowsProcessController;

        let controller = WindowsProcessController;
        let identity = controller
            .identity(snapshot.target.pid)
            .map_err(|error| LegacyCompatibilityError::RobloxDiscovery(error.to_string()))?
            .ok_or(LegacyCompatibilityError::RobloxClientUnavailable)?;
        let mut adopted = self.adopted_client.lock().map_err(|_| {
            LegacyCompatibilityError::RobloxDiscovery("adopted-client lock was poisoned".into())
        })?;
        match adopted.as_ref() {
            Some(expected)
                if expected.identity == identity && expected.target == snapshot.target =>
            {
                Ok(())
            }
            Some(_) => Err(LegacyCompatibilityError::RobloxClientChanged),
            None => {
                *adopted = Some(AdoptedRobloxClient {
                    identity,
                    target: snapshot.target,
                });
                Ok(())
            }
        }
    }

    fn require_foreground_roblox(&self) -> Result<(), LegacyCompatibilityError> {
        let clients = discover_roblox_clients()
            .map_err(|error| LegacyCompatibilityError::RobloxDiscovery(error.to_string()))?;
        let snapshot = exact_single_snapshot(&clients)?;
        if snapshot.geometry.minimized || !snapshot.is_foreground {
            return Err(LegacyCompatibilityError::RobloxClientNotForeground);
        }
        self.store_or_verify_adoption(snapshot)
    }
}

#[cfg(not(windows))]
impl LegacyCompatibilityService {
    fn require_foreground_roblox(&self) -> Result<(), LegacyCompatibilityError> {
        Err(LegacyCompatibilityError::RobloxDiscovery(
            "legacy compatibility is available only on Windows".into(),
        ))
    }
}

#[cfg(test)]
impl LegacyCompatibilityService {
    fn assets(&self) -> Vec<LegacyAssetDescriptor> {
        self.assets
            .values()
            .map(|asset| asset.descriptor.clone())
            .collect()
    }

    fn inspect(&self, id: &str) -> Result<LegacyAssetDescriptor, LegacyCompatibilityError> {
        let asset = self.pinned_asset(id)?;
        if !asset.descriptor.requires_legacy_bridge {
            return Err(LegacyCompatibilityError::NativeDslAsset { id: id.to_owned() });
        }
        self.verified_fragment(id)?;
        Ok(asset.descriptor.clone())
    }

    pub async fn execute(
        &self,
        profile: &Profile,
        id: &str,
        cancellation: CancellationToken,
    ) -> Result<ExecutionOutcome, LegacyCompatibilityError> {
        let asset = self.pinned_asset(id)?;
        self.execute_verified(profile, id, &asset.descriptor.sha256, cancellation)
            .await
    }
}

#[async_trait]
impl CoreLegacyExecutionPort for LegacyCompatibilityService {
    async fn preflight(
        &self,
        profile: &Profile,
        script_id: &str,
        approved_sha256: &str,
    ) -> Result<(), AutomationError> {
        self.preflight_legacy(profile, script_id, approved_sha256)
            .map_err(|error| AutomationError::Preflight(error.to_string()))
    }

    async fn execute(
        &self,
        profile: &Profile,
        script_id: &str,
        approved_sha256: &str,
        context: TaskContext,
    ) -> ActionResult {
        let started_at = Utc::now();
        match self
            .execute_verified(
                profile,
                script_id,
                approved_sha256,
                context.cancellation_token(),
            )
            .await
        {
            Ok(ExecutionOutcome::Completed { pid, exit_code }) if exit_code.unwrap_or(0) == 0 => {
                ActionResult {
                    action: format!("legacy:{script_id}"),
                    outcome: ActionOutcome::Succeeded,
                    started_at,
                    finished_at: Utc::now(),
                    message: "contained legacy compatibility script completed".into(),
                    details: json!({ "asset_id": script_id, "pid": pid, "exit_code": exit_code }),
                }
            }
            Ok(ExecutionOutcome::Completed { pid, exit_code }) => ActionResult {
                action: format!("legacy:{script_id}"),
                outcome: ActionOutcome::Failed,
                started_at,
                finished_at: Utc::now(),
                message: format!("contained legacy compatibility script exited with {exit_code:?}"),
                details: json!({ "asset_id": script_id, "pid": pid, "exit_code": exit_code }),
            },
            Err(LegacyCompatibilityError::Runner(LegacyError::Cancelled { .. })) => ActionResult {
                action: format!("legacy:{script_id}"),
                outcome: ActionOutcome::Cancelled,
                started_at,
                finished_at: Utc::now(),
                message: "contained legacy compatibility script cancelled".into(),
                details: json!({ "asset_id": script_id }),
            },
            Err(LegacyCompatibilityError::Runner(LegacyError::TimedOut { .. })) => ActionResult {
                action: format!("legacy:{script_id}"),
                outcome: ActionOutcome::NeedsAttention,
                started_at,
                finished_at: Utc::now(),
                message: "contained legacy compatibility script reached its 30-minute limit".into(),
                details: json!({ "asset_id": script_id }),
            },
            Err(error) => ActionResult {
                action: format!("legacy:{script_id}"),
                outcome: ActionOutcome::Failed,
                started_at,
                finished_at: Utc::now(),
                message: format!("legacy compatibility execution failed: {error}"),
                details: json!({ "asset_id": script_id }),
            },
        }
    }

    async fn cancel(&self) -> Result<(), AutomationError> {
        // The engine cancels the exact per-run token before invoking this hook.
        // LegacyRunner observes that token, terminates its job object, and does
        // not retain any global or unrelated process handle.
        #[cfg(windows)]
        self.clear_adopted_client()
            .map_err(|error| AutomationError::Backend(error.to_string()))?;
        Ok(())
    }

    /// Toggles the generated harness's F16 pause handler, which releases and
    /// later restores held movement keys exactly as the legacy macro did.
    async fn pause(&self) -> Result<(), AutomationError> {
        toggle_walk_pause()
    }

    async fn resume(&self) -> Result<(), AutomationError> {
        toggle_walk_pause()
    }

    async fn describe(
        &self,
        profile: &Profile,
        script_id: &str,
    ) -> Result<nectarpilot_contracts::LegacyInspection, AutomationError> {
        if script_id == BUILTIN_RESET_SCRIPT_ID {
            let preview = self
                .builtin_reset_script(profile)
                .map_err(|error| AutomationError::Preflight(error.to_string()))?;
            return Ok(nectarpilot_contracts::LegacyInspection {
                script_id: script_id.to_owned(),
                sha256: hex::encode(Sha256::digest(preview.as_bytes())),
                bytes: preview.len() as u64,
                requires_legacy_bridge: true,
                harness_preview: preview,
            });
        }
        let (fragment, kind) = self
            .verified_fragment(script_id)
            .map_err(|error| AutomationError::Preflight(error.to_string()))?;
        let asset = self
            .pinned_asset(script_id)
            .map_err(|error| AutomationError::Preflight(error.to_string()))?;
        let settings = harness_settings(profile)
            .map_err(|error| AutomationError::Preflight(error.to_string()))?;
        let preview = generate_walk_script(&self.root, kind, &fragment, &settings)
            .map_err(|error| AutomationError::Preflight(error.to_string()))?;
        Ok(nectarpilot_contracts::LegacyInspection {
            script_id: script_id.to_owned(),
            sha256: asset.descriptor.sha256.clone(),
            bytes: asset.descriptor.bytes,
            requires_legacy_bridge: asset.descriptor.requires_legacy_bridge,
            harness_preview: preview,
        })
    }

    /// Disconnect recovery: confirm the legacy disconnect dialog (or a fully
    /// crashed client), rejoin through the stored private-server link, then
    /// re-anchor at the hive with the builtin reset step.
    async fn recover(
        &self,
        profile: &Profile,
        private_server_link: Option<&str>,
        context: TaskContext,
    ) -> Option<ActionResult> {
        #[cfg(not(windows))]
        {
            let _ = (profile, private_server_link, context);
            None
        }
        #[cfg(windows)]
        {
            let started_at = Utc::now();
            let recovery = |outcome: ActionOutcome, message: String| ActionResult {
                action: "legacy:recover".into(),
                outcome,
                started_at,
                finished_at: Utc::now(),
                message,
                details: json!({}),
            };
            match self.disconnect_status() {
                Ok(false) => None,
                Err(reason) => Some(recovery(
                    ActionOutcome::NeedsAttention,
                    format!("could not determine disconnect state: {reason}"),
                )),
                Ok(true) => {
                    let Some(code) = private_server_link.and_then(extract_link_code) else {
                        return Some(recovery(
                            ActionOutcome::NeedsAttention,
                            "disconnected; store a private_server_link secret with a \
                             32-character link code to enable auto-reconnect"
                                .into(),
                        ));
                    };
                    if let Err(reason) = self
                        .relaunch_and_wait(&code, &context.cancellation_token())
                        .await
                    {
                        return Some(recovery(
                            ActionOutcome::Failed,
                            format!("reconnect launch failed: {reason}"),
                        ));
                    }
                    let reset = self
                        .execute_verified(
                            profile,
                            BUILTIN_RESET_SCRIPT_ID,
                            nectarpilot_core::BUILTIN_APPROVAL,
                            context.cancellation_token(),
                        )
                        .await;
                    Some(match reset {
                        Ok(ExecutionOutcome::Completed { exit_code, .. })
                            if exit_code.unwrap_or(0) == 0 =>
                        {
                            recovery(
                                ActionOutcome::Succeeded,
                                "reconnected to the private server and reset to the hive".into(),
                            )
                        }
                        Ok(ExecutionOutcome::Completed { exit_code, .. }) => recovery(
                            ActionOutcome::Failed,
                            format!("post-reconnect reset exited with {exit_code:?}"),
                        ),
                        Err(error) => recovery(
                            ActionOutcome::Failed,
                            format!("post-reconnect reset failed: {error}"),
                        ),
                    })
                }
            }
        }
    }
}

/// F16 virtual key: the generated harness's pause/resume hotkey.
#[cfg(windows)]
fn toggle_walk_pause() -> Result<(), AutomationError> {
    nectarpilot_platform::tap_global_virtual_key(0x7F)
        .map_err(|error| AutomationError::Backend(error.to_string()))
}

#[cfg(not(windows))]
fn toggle_walk_pause() -> Result<(), AutomationError> {
    Err(AutomationError::InvalidCommand(
        "legacy pause requires the Windows walk harness".into(),
    ))
}

/// Accepts either a full private-server URL or a bare 32-character code.
fn extract_link_code(link: &str) -> Option<String> {
    if link.len() <= 512
        && let Some(index) = link.find("privateServerLinkCode=")
    {
        let code: String = link[index + "privateServerLinkCode=".len()..]
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect();
        return (code.len() == 32).then_some(code);
    }
    (link.len() == 32 && link.chars().all(|c| c.is_ascii_alphanumeric())).then(|| link.to_owned())
}

#[cfg(windows)]
impl LegacyCompatibilityService {
    /// `Ok(true)` only when the adopted client is absent and no other Roblox
    /// process is present, or its exact window confidently shows the legacy
    /// disconnect dialog.
    fn disconnect_status(&self) -> Result<bool, String> {
        use nectarpilot_platform::capture::ClientCapture;
        use nectarpilot_platform::windows_backend::WindowsProcessController;
        use nectarpilot_platform::{
            MultiScaleTemplateMatcher, RobloxSession, TemplateMatcherConfig,
            capture::WindowsClientCapture, template_from_png_bytes,
        };

        let adopted = self.adopted_client().map_err(|error| error.to_string())?;
        let clients = discover_roblox_clients().map_err(|error| error.to_string())?;
        if clients.is_empty() {
            return Ok(true);
        }
        if clients.len() != 1 {
            return Err(format!(
                "expected only adopted Roblox PID {}, found {} client processes",
                adopted.identity.pid.get(),
                clients.len()
            ));
        }
        let candidate = clients[0];
        if candidate.pid != adopted.identity.pid {
            return Err(format!(
                "adopted Roblox PID {} disappeared while unrelated PID {} is present",
                adopted.identity.pid.get(),
                candidate.pid.get()
            ));
        }
        let controller = WindowsProcessController;
        let current = controller
            .identity(candidate.pid)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "the adopted Roblox client disappeared during inspection".to_owned())?;
        if current != adopted.identity {
            return Err("the adopted Roblox process changed identity".into());
        }
        let snapshot = candidate
            .window
            .ok_or_else(|| "the adopted Roblox client has no inspectable window".to_owned())?;
        if snapshot.target != adopted.target {
            return Err("the adopted Roblox window changed identity".into());
        }
        if snapshot.geometry.minimized {
            return Err("the Roblox client is minimized and cannot be inspected".into());
        }
        let session = RobloxSession::from_snapshot(snapshot);
        let frame = WindowsClientCapture
            .capture(&session)
            .map_err(|error| error.to_string())?;
        let source = fs::read_to_string(self.root.join("nm_image_assets/general/bitmaps.ahk"))
            .map_err(|error| error.to_string())?;
        let bytes = nectarpilot_legacy::extract_inline_template(&source, "disconnected")
            .ok_or("the pinned disconnected template is missing")?;
        let template =
            template_from_png_bytes("disconnected", &bytes).map_err(|error| error.to_string())?;
        let crop = frame
            .crop(nectarpilot_contracts::NormalizedRegion {
                x: 0.3,
                y: 0.3,
                width: 0.4,
                height: 0.4,
            })
            .map_err(|error| error.to_string())?;
        let matcher = MultiScaleTemplateMatcher::new(TemplateMatcherConfig::default())
            .map_err(|error| error.to_string())?;
        let best = matcher
            .find_best(&crop.image, &template)
            .map_err(|error| error.to_string())?;
        Ok(best.is_some_and(|found| found.confidence >= 0.92))
    }

    /// Terminates only the exact disconnected client adopted at preflight,
    /// launches the private-server URL, then adopts one unambiguous replacement.
    async fn relaunch_and_wait(
        &self,
        link_code: &str,
        cancellation: &CancellationToken,
    ) -> Result<(), String> {
        use nectarpilot_platform::windows_backend::WindowsProcessController;

        let adopted = self.adopted_client().map_err(|error| error.to_string())?;
        let mut controller = WindowsProcessController;
        terminate_exact_adopted(&mut controller, &adopted)?;
        self.clear_adopted_client()
            .map_err(|error| error.to_string())?;
        tokio::time::sleep(Duration::from_secs(3)).await;
        let url = format!("roblox://placeID=1537690962&linkCode={link_code}");
        std::process::Command::new("cmd")
            .args(["/C", "start", "", &url])
            .spawn()
            .map_err(|error| format!("could not launch the Roblox join URL: {error}"))?;
        for _attempt in 0..60_u32 {
            if cancellation.is_cancelled() {
                return Err("reconnect cancelled".into());
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
            let clients = discover_roblox_clients().map_err(|error| error.to_string())?;
            if clients.len() > 1 {
                return Err(format!(
                    "reconnect is ambiguous: found {} Roblox client processes",
                    clients.len()
                ));
            }
            let Some(candidate) = clients.first() else {
                continue;
            };
            let Some(snapshot) = candidate.window else {
                continue;
            };
            let identity = controller
                .identity(candidate.pid)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| {
                    "replacement Roblox client disappeared before adoption".to_owned()
                })?;
            if identity == adopted.identity {
                continue;
            }
            *self
                .adopted_client
                .lock()
                .map_err(|_| "adopted-client lock was poisoned".to_owned())? =
                Some(AdoptedRobloxClient {
                    identity,
                    target: snapshot.target,
                });
            // Give the game world time to stream in before the reset step
            // starts pressing keys, while remaining immediately cancellable.
            tokio::select! {
                () = cancellation.cancelled() => return Err("reconnect cancelled".into()),
                () = tokio::time::sleep(Duration::from_secs(20)) => return Ok(()),
            }
        }
        Err("no Roblox client appeared within the reconnect window".into())
    }
}

fn safe_development_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn embedded_assets() -> Result<BTreeMap<String, PinnedAsset>, LegacyCompatibilityError> {
    let mut assets = BTreeMap::new();
    for (manifest, expected_kind, expected_directory, id_prefix) in [
        (ROUTE_MANIFEST, AssetKind::Route, "paths", "legacy:route:"),
        (
            PATTERN_MANIFEST,
            AssetKind::Pattern,
            "patterns",
            "legacy:pattern:",
        ),
    ] {
        let catalog: AssetCatalog = serde_yaml::from_str(manifest)
            .map_err(|error| LegacyCompatibilityError::Manifest(error.to_string()))?;
        if catalog.format_version != 1
            || catalog.kind != expected_kind
            || catalog.source_directory != expected_directory
        {
            return Err(LegacyCompatibilityError::Manifest(format!(
                "catalog metadata does not match the expected {expected_directory} import"
            )));
        }
        for entry in catalog.entries {
            validate_manifest_source(&entry.legacy_source, expected_directory)?;
            if entry.bytes > 8 * 1024 * 1024 {
                return Err(LegacyCompatibilityError::Manifest(format!(
                    "{} exceeds the legacy runner byte limit",
                    entry.legacy_source
                )));
            }
            if entry.sha256.len() != 64
                || !entry.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err(LegacyCompatibilityError::Manifest(format!(
                    "{} has an invalid SHA-256 digest",
                    entry.legacy_source
                )));
            }
            let id = format!("{id_prefix}{}", entry.legacy_source);
            let descriptor = LegacyAssetDescriptor {
                id: id.clone(),
                legacy_source: entry.legacy_source,
                sha256: entry.sha256,
                bytes: entry.bytes,
                requires_legacy_bridge: entry.status == AssetStatus::LegacyBridgeRequired,
            };
            if assets
                .insert(id.clone(), PinnedAsset { descriptor })
                .is_some()
            {
                return Err(LegacyCompatibilityError::Manifest(format!(
                    "duplicate legacy ID {id}"
                )));
            }
        }
    }
    Ok(assets)
}

fn validate_manifest_source(
    legacy_source: &str,
    expected_directory: &str,
) -> Result<(), LegacyCompatibilityError> {
    let prefix = format!("{expected_directory}/");
    let Some(file_name) = legacy_source.strip_prefix(&prefix) else {
        return Err(LegacyCompatibilityError::Manifest(format!(
            "{legacy_source} is outside {expected_directory}"
        )));
    };
    if file_name.is_empty()
        || file_name.contains(['/', '\\'])
        || !file_name.to_ascii_lowercase().ends_with(".ahk")
    {
        return Err(LegacyCompatibilityError::Manifest(format!(
            "{legacy_source} is not a single AutoHotkey file name"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use tempfile::TempDir;

    const TEST_ID: &str = "legacy:route:paths/gtb-blue.ahk";
    const STATIONARY_ID: &str = "legacy:pattern:patterns/Stationary.ahk";

    fn test_root() -> TempDir {
        let root = tempfile::tempdir().expect("test root");
        fs::create_dir_all(root.path().join("paths")).expect("paths directory");
        let source = safe_development_root().join("paths").join("gtb-blue.ahk");
        fs::copy(source, root.path().join("paths").join("gtb-blue.ahk"))
            .expect("copy manifest-pinned fixture");
        root
    }

    #[test]
    fn manifest_exposes_exactly_the_imported_bridge_assets() {
        let root = test_root();
        let service = LegacyCompatibilityService::from_root(root.path()).expect("service");
        let assets = service.assets();

        assert_eq!(assets.len(), 103);
        assert_eq!(
            assets
                .iter()
                .filter(|asset| asset.requires_legacy_bridge)
                .count(),
            102
        );
        assert_eq!(
            assets
                .iter()
                .filter(|asset| !asset.requires_legacy_bridge)
                .count(),
            1
        );
        assert!(matches!(
            service.inspect(STATIONARY_ID),
            Err(LegacyCompatibilityError::NativeDslAsset { .. })
        ));
        assert!(matches!(
            service.inspect("legacy:route:paths/../not-listed.ahk"),
            Err(LegacyCompatibilityError::UnknownAsset { .. })
        ));
    }

    #[test]
    fn changed_manifest_pinned_script_is_never_inspected_as_trusted() {
        let root = test_root();
        fs::write(root.path().join("paths").join("gtb-blue.ahk"), "Sleep 1\n")
            .expect("alter fixture");
        let service = LegacyCompatibilityService::from_root(root.path()).expect("service");

        assert!(matches!(
            service.inspect(TEST_ID),
            Err(LegacyCompatibilityError::AssetTrustMismatch { .. })
        ));
    }

    #[test]
    fn verified_fragment_wraps_route_in_the_full_legacy_environment() {
        let root = test_root();
        let service = LegacyCompatibilityService::from_root(root.path()).expect("service");
        let (fragment, kind) = service.verified_fragment(TEST_ID).expect("pinned fragment");

        assert_eq!(kind, FragmentKind::Route);
        assert!(fragment.contains("nm_gotoramp()"));

        let script = generate_walk_script(
            &service.root,
            kind,
            &fragment,
            &harness_settings(&Profile::new("defaults")).expect("default settings"),
        )
        .expect("harness generation");
        for required in ["nm_gotoRamp() {", "nm_Walk(tiles", "FwdKey:=\"sc011\""] {
            assert!(script.contains(required), "missing {required:?}");
        }
    }

    #[test]
    fn harness_settings_come_from_the_imported_ini_snapshot() {
        use nectarpilot_contracts::{LegacySnapshot, LegacySource};
        use std::collections::BTreeMap;

        let mut profile = Profile::new("imported");
        assert_eq!(
            harness_settings(&profile).expect("defaults").move_method,
            MoveMethod::Cannon
        );

        let mut settings_section = BTreeMap::new();
        settings_section.insert("MoveMethod".to_owned(), "Walk".to_owned());
        settings_section.insert("MoveSpeedNum".to_owned(), "26".to_owned());
        settings_section.insert("HiveSlot".to_owned(), "3".to_owned());
        settings_section.insert("HiveBees".to_owned(), "32".to_owned());
        settings_section.insert("KeyDelay".to_owned(), "20".to_owned());
        settings_section.insert("NewWalk".to_owned(), "0".to_owned());
        settings_section.insert("HiveSlot ".to_owned(), "not-a-number".to_owned());
        let mut sections = BTreeMap::new();
        sections.insert("Settings".to_owned(), settings_section);
        profile.legacy = Some(LegacySnapshot {
            sources: vec![LegacySource {
                file_name: "nm_config.ini".to_owned(),
                sha256: "0".repeat(64),
                sections,
            }],
        });

        let parsed = harness_settings(&profile).expect("valid imported settings");
        assert_eq!(parsed.move_method, MoveMethod::Walk);
        assert!((parsed.move_speed - 26.0).abs() < f64::EPSILON);
        assert_eq!(parsed.hive_slot, 3);
        assert_eq!(parsed.hive_bees, 32);
        assert_eq!(parsed.key_delay, 20);
        assert!(!parsed.new_walk);

        // A present-but-invalid import is surfaced instead of silently using 6.
        profile.legacy.as_mut().unwrap().sources[0]
            .sections
            .get_mut("Settings")
            .unwrap()
            .insert("HiveSlot".to_owned(), "9".to_owned());
        assert!(matches!(
            harness_settings(&profile),
            Err(LegacyCompatibilityError::InvalidMovementSetting {
                name: "HiveSlot",
                ..
            })
        ));
    }

    #[test]
    fn malformed_boolean_movement_setting_fails_closed() {
        use std::collections::BTreeMap;

        use nectarpilot_contracts::{LegacySnapshot, LegacySource};

        let mut settings = BTreeMap::new();
        settings.insert("NewWalk".to_owned(), "maybe".to_owned());
        let mut sections = BTreeMap::new();
        sections.insert("Settings".to_owned(), settings);
        let mut profile = Profile::new("invalid movement");
        profile.legacy = Some(LegacySnapshot {
            sources: vec![LegacySource {
                file_name: "nm_config.ini".to_owned(),
                sha256: "0".repeat(64),
                sections,
            }],
        });

        assert!(matches!(
            harness_settings(&profile),
            Err(LegacyCompatibilityError::InvalidMovementSetting {
                name: "NewWalk",
                ..
            })
        ));
    }

    #[cfg(windows)]
    #[test]
    fn recovery_termination_targets_only_the_adopted_identity() {
        use nectarpilot_platform::process::MockProcessController;
        use nectarpilot_platform::session::{ProcessId, WindowHandle};

        let controller = MockProcessController::default();
        let adopted_identity = ProcessIdentity {
            pid: ProcessId::new(101).expect("pid"),
            created_at_ticks: 10,
            executable_path: Some(PathBuf::from("RobloxPlayerBeta.exe")),
        };
        let unrelated_identity = ProcessIdentity {
            pid: ProcessId::new(202).expect("pid"),
            created_at_ticks: 20,
            executable_path: Some(PathBuf::from("RobloxPlayerBeta.exe")),
        };
        controller.insert(adopted_identity.clone());
        controller.insert(unrelated_identity.clone());
        let adopted = AdoptedRobloxClient {
            identity: adopted_identity.clone(),
            target: SessionTarget {
                pid: adopted_identity.pid,
                window: WindowHandle::new(303).expect("window"),
            },
        };

        let mut termination_controller = controller.clone();
        terminate_exact_adopted(&mut termination_controller, &adopted).expect("terminate adopted");

        assert_eq!(controller.terminated(), vec![adopted_identity]);
        assert!(!controller.terminated().contains(&unrelated_identity));
    }

    #[cfg(windows)]
    #[test]
    fn recovery_refuses_a_reused_adopted_pid() {
        use nectarpilot_platform::process::MockProcessController;
        use nectarpilot_platform::session::{ProcessId, WindowHandle};

        let pid = ProcessId::new(404).expect("pid");
        let expected = ProcessIdentity {
            pid,
            created_at_ticks: 40,
            executable_path: Some(PathBuf::from("RobloxPlayerBeta.exe")),
        };
        let reused = ProcessIdentity {
            pid,
            created_at_ticks: 41,
            executable_path: Some(PathBuf::from("RobloxPlayerBeta.exe")),
        };
        let controller = MockProcessController::default();
        controller.insert(reused);
        let adopted = AdoptedRobloxClient {
            identity: expected,
            target: SessionTarget {
                pid,
                window: WindowHandle::new(405).expect("window"),
            },
        };

        let mut termination_controller = controller.clone();
        assert!(terminate_exact_adopted(&mut termination_controller, &adopted).is_err());
        assert!(controller.terminated().is_empty());
    }

    #[tokio::test]
    async fn safety_or_exact_profile_trust_blocks_before_any_interpreter_is_read() {
        let root = test_root();
        let service = LegacyCompatibilityService::from_root(root.path()).expect("service");
        let mut profile = Profile::new("guarded");
        profile.safety.item_budgets.dice = 1;

        assert!(matches!(
            service
                .execute(&profile, TEST_ID, CancellationToken::new())
                .await,
            Err(LegacyCompatibilityError::ValuableBudgetEnabled { .. })
        ));

        profile.safety.item_budgets.dice = 0;
        profile
            .trusted_extensions
            .insert(TEST_ID.to_owned(), "not-the-manifest-digest".to_owned());
        assert!(matches!(
            service
                .execute(&profile, TEST_ID, CancellationToken::new())
                .await,
            Err(LegacyCompatibilityError::ProfileTrustMismatch { .. })
        ));
    }

    #[test]
    fn unpinned_interpreter_never_reaches_the_runner() {
        let root = test_root();
        fs::create_dir_all(root.path().join("submacros")).expect("submacros directory");
        fs::write(
            root.path().join("submacros").join("AutoHotkey64.exe"),
            "not an interpreter",
        )
        .expect("fake interpreter");
        let service = LegacyCompatibilityService::from_root(root.path()).expect("service");
        assert!(matches!(
            service.verified_interpreter(),
            Err(LegacyCompatibilityError::InterpreterTrustMismatch { .. })
        ));
    }
}
