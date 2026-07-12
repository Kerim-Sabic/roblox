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
    time::Duration,
};

use async_trait::async_trait;
use chrono::Utc;
use nectarpilot_contracts::{ActionOutcome, ActionResult, Profile};
use nectarpilot_core::{
    AutomationError, LegacyExecutionPort as CoreLegacyExecutionPort, TaskContext,
};
use nectarpilot_legacy::{
    AssetCatalog, AssetKind, AssetStatus, ExecutionOutcome, ExecutionRequest, LegacyConsent,
    LegacyError, LegacyRunner, RunnerPolicy,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

#[cfg(windows)]
use nectarpilot_platform::discover_roblox_clients;

/// The exact `AutoHotkey` binary imported with Natro Macro v1.1.2.
pub const PINNED_AUTOHOTKEY64_SHA256: &str =
    "37ff15a23a98f0a658298e21f1873ca896a05208810bf796f90ca212ee07c7b1";

/// A legacy script can never outlive this bounded execution window.
pub const LEGACY_EXECUTION_MAXIMUM: Duration = Duration::from_secs(30 * 60);

const ROUTE_MANIFEST: &str = include_str!("../../../assets/routes/_legacy-manifest.yaml");
const PATTERN_MANIFEST: &str = include_str!("../../../assets/patterns/_legacy-manifest.yaml");
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
    #[error("could not inspect Roblox clients before legacy execution: {0}")]
    RobloxDiscovery(String),
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
    runner: LegacyRunner,
}

impl LegacyCompatibilityService {
    /// Builds the service from `NECTARPILOT_LEGACY_ROOT` when it is explicitly set.
    /// Otherwise it uses the compile-time repository root, never the process CWD.
    pub fn from_environment() -> Result<Self, LegacyCompatibilityError> {
        let root = env::var_os(LEGACY_ROOT_ENV).map_or_else(safe_development_root, PathBuf::from);
        Self::from_root(root)
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
        let runner = LegacyRunner::new(RunnerPolicy {
            maximum_timeout: LEGACY_EXECUTION_MAXIMUM,
            ..RunnerPolicy::default()
        });
        Ok(Self {
            root,
            assets,
            runner,
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

    fn verified_script(&self, asset: &PinnedAsset) -> Result<PathBuf, LegacyCompatibilityError> {
        let script = self.checked_path(&asset.descriptor.legacy_source)?;
        let report = self.runner.inspect(&script)?;
        if report.trust.sha256 != asset.descriptor.sha256
            || report.trust.bytes != asset.descriptor.bytes
        {
            return Err(LegacyCompatibilityError::AssetTrustMismatch {
                id: asset.descriptor.id.clone(),
                expected_sha256: asset.descriptor.sha256.clone(),
                expected_bytes: asset.descriptor.bytes,
                actual_sha256: report.trust.sha256,
                actual_bytes: report.trust.bytes,
            });
        }
        Ok(report.trust.canonical_path)
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
        asset: &PinnedAsset,
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
        let asset = self.pinned_asset(id)?;
        if !asset.descriptor.requires_legacy_bridge {
            return Err(LegacyCompatibilityError::NativeDslAsset { id: id.to_owned() });
        }
        Self::require_approved_digest(asset, approved_sha256)?;
        Self::require_safe_profile(profile, asset)?;
        require_foreground_roblox()?;
        self.verified_script(asset)?;
        self.verified_interpreter()?;
        Ok(())
    }

    async fn execute_verified(
        &self,
        profile: &Profile,
        id: &str,
        approved_sha256: &str,
        cancellation: CancellationToken,
    ) -> Result<ExecutionOutcome, LegacyCompatibilityError> {
        self.preflight_legacy(profile, id, approved_sha256)?;
        let asset = self.pinned_asset(id)?;
        let script = self.verified_script(asset)?;
        let interpreter = self.verified_interpreter()?;
        let report = self.runner.inspect(&script)?;
        // `verified_script` checked every field against the manifest. The
        // runner hashes a third time immediately before spawn and rejects any
        // TOCTOU swap.
        let request = ExecutionRequest {
            interpreter,
            interpreter_arguments: vec!["/ErrorStdOut".into()],
            script,
            consent: Some(LegacyConsent::acknowledge_for(&report.trust)),
            timeout: LEGACY_EXECUTION_MAXIMUM,
        };
        self.runner
            .execute(request, cancellation)
            .await
            .map_err(Into::into)
    }
}

#[cfg(windows)]
fn require_foreground_roblox() -> Result<(), LegacyCompatibilityError> {
    let clients = discover_roblox_clients()
        .map_err(|error| LegacyCompatibilityError::RobloxDiscovery(error.to_string()))?;
    let visible = clients
        .into_iter()
        .filter_map(|client| client.window)
        .collect::<Vec<_>>();
    if visible.len() != 1 {
        return Err(LegacyCompatibilityError::RobloxClientAmbiguous {
            count: visible.len(),
        });
    }
    let session = visible[0];
    if session.geometry.minimized || !session.is_foreground {
        return Err(LegacyCompatibilityError::RobloxClientNotForeground);
    }
    Ok(())
}

#[cfg(not(windows))]
fn require_foreground_roblox() -> Result<(), LegacyCompatibilityError> {
    Err(LegacyCompatibilityError::RobloxDiscovery(
        "legacy compatibility is available only on Windows".into(),
    ))
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
        self.verified_script(asset)?;
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
        Ok(())
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
