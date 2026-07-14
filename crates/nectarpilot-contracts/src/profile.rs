use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use specta::Type;
use uuid::Uuid;

pub const PROFILE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct Profile {
    pub id: Uuid,
    pub schema_version: u32,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub automation: AutomationConfig,
    #[serde(default)]
    pub safety: SafetyConfig,
    #[serde(default)]
    pub discord: DiscordConfig,
    #[serde(default)]
    pub onboarding_complete: bool,
    /// Extension identifier to the exact SHA-256 hash the user approved.
    #[serde(default)]
    pub trusted_extensions: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy: Option<LegacySnapshot>,
}

impl Profile {
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::now_v7(),
            schema_version: PROFILE_SCHEMA_VERSION,
            name: name.into(),
            created_at: now,
            updated_at: now,
            automation: AutomationConfig::default(),
            safety: SafetyConfig::default(),
            discord: DiscordConfig::default(),
            onboarding_complete: false,
            trusted_extensions: BTreeMap::new(),
            legacy: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(default)]
pub struct AutomationConfig {
    pub gathering_enabled: bool,
    pub reconnect_enabled: bool,
    pub rotations: Vec<FieldRotation>,
    pub features: FeatureFlags,
    pub hotkeys: HotkeyConfig,
    pub session: SessionConfig,
    /// Character movement calibration used by every generated legacy harness:
    /// walk speed, hive slot, travel method, and key delay. These are the same
    /// values the Natro Macro GUI exposed front and center.
    pub movement: MovementConfig,
    /// Whether `movement` was explicitly supplied by the user-facing
    /// calibration UI or imported from a known legacy configuration. This must
    /// not be inferred by comparing values with defaults: a user is allowed to
    /// deliberately choose every stock Natro value and have that decision win
    /// over an older INI snapshot.
    pub movement_configured: bool,
    /// Manual planter reminders; the desktop shows countdowns and due badges.
    /// Nothing is placed or collected automatically from these entries.
    pub planters: Vec<ManualPlanterTimer>,
    /// Allowlisted clock, free-dispenser, and field-booster routes run at cycle
    /// boundaries during orchestrated sessions. Valuable and state-changing
    /// routes are never accepted by this scheduler.
    pub collect: Vec<CollectTask>,
}

impl Default for AutomationConfig {
    fn default() -> Self {
        Self {
            gathering_enabled: false,
            reconnect_enabled: true,
            rotations: Vec::new(),
            features: FeatureFlags::default(),
            hotkeys: HotkeyConfig::default(),
            session: SessionConfig::default(),
            movement: MovementConfig::default(),
            movement_configured: false,
            planters: Vec::new(),
            collect: Vec::new(),
        }
    }
}

/// How the player character travels, mirroring the Natro Macro movement
/// settings. All values are bounds-checked again by the harness generator
/// before any script is produced, so an out-of-range value fails closed.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Type)]
#[serde(default)]
pub struct MovementConfig {
    /// The exact in-game walk speed shown by Roblox (10.0..=200.0). This is the
    /// single most important calibration for accurate movement timing.
    pub walk_speed: f64,
    /// Hive slot 1..=6 as counted from the left of the hive.
    pub hive_slot: u8,
    /// Bees in the hive 0..=50; used by the reset/return path.
    pub hive_bees: u8,
    /// Extra per-key send delay in milliseconds, 0..=1000. Raise it only if the
    /// game drops inputs on a slow machine.
    pub key_delay: u16,
    /// `true` travels between hive and field by cannon; `false` walks.
    pub cannon_travel: bool,
    /// `true` uses buff-corrected movement timing (Natro `NewWalk`); leave on.
    pub buff_corrected_walk: bool,
}

impl Default for MovementConfig {
    fn default() -> Self {
        // Natro Macro stock defaults.
        Self {
            walk_speed: 28.0,
            hive_slot: 6,
            hive_bees: 50,
            key_delay: 20,
            cannon_travel: true,
            buff_corrected_walk: true,
        }
    }
}

impl MovementConfig {
    /// Clamps every field into its supported range. The UI validates too, but
    /// this guarantees a stored profile can never produce an invalid harness.
    #[must_use]
    pub fn sanitized(self) -> Self {
        Self {
            walk_speed: if self.walk_speed.is_finite() {
                self.walk_speed.clamp(10.0, 200.0)
            } else {
                28.0
            },
            hive_slot: self.hive_slot.clamp(1, 6),
            hive_bees: self.hive_bees.min(50),
            key_delay: self.key_delay.min(1000),
            cannon_travel: self.cannon_travel,
            buff_corrected_walk: self.buff_corrected_walk,
        }
    }
}

/// One cooldown-scheduled allowlisted target, e.g. `clock` every 240 minutes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct CollectTask {
    /// Approved clock/dispenser/booster name. The engine maps this onto an
    /// exact manifest route and requires that route's trusted digest.
    pub target: String,
    pub cooldown_minutes: u32,
}

/// Bounds for one orchestrated legacy gather session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(default)]
pub struct SessionConfig {
    /// Seconds to remain at the hive converting after each reset step.
    pub convert_wait_seconds: u16,
    pub default_max_cycles: u32,
    pub default_max_minutes: u32,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            convert_wait_seconds: 30,
            default_max_cycles: 10,
            default_max_minutes: 120,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ManualPlanterTimer {
    /// Planter slot 1..=3 as shown in game.
    pub slot: u8,
    /// Free-text description, e.g. "Blue Clay in Bamboo".
    pub label: String,
    pub placed_at: DateTime<Utc>,
    pub harvest_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct FieldRotation {
    pub field: String,
    pub pattern: String,
    pub gather_seconds: u32,
    #[serde(default = "default_one")]
    pub repetitions: u16,
}

const fn default_one() -> u16 {
    1
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, Type)]
#[serde(default)]
#[allow(clippy::struct_excessive_bools)] // Serialized feature matrix; independent toggles are intentional.
pub struct FeatureFlags {
    pub collections: bool,
    pub bosses: bool,
    pub vicious_bee: bool,
    pub memory_matches: bool,
    pub quests: bool,
    pub planters: bool,
    pub boosts: bool,
    pub shrine: bool,
    pub stickers: bool,
    pub hotbar_scheduling: bool,
    pub mutations_and_auto_jelly: bool,
    pub seasonal: bool,
    pub custom_extensions: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(default)]
pub struct HotkeyConfig {
    pub start: String,
    pub pause_resume: String,
    pub stop: String,
    pub emergency_stop: String,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            start: "F1".into(),
            pause_resume: "F2".into(),
            stop: "F3".into(),
            emergency_stop: "Ctrl+Shift+F12".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(default)]
#[allow(clippy::struct_excessive_bools)] // Explicit deny-by-default safety switches.
pub struct SafetyConfig {
    pub item_budgets: ValuableItemBudgets,
    pub purchases_enabled: bool,
    pub donations_enabled: bool,
    pub trades_enabled: bool,
    pub allow_system_power: bool,
    pub evidence_retention_days: u16,
    pub evidence_retention_megabytes: u32,
}

impl Default for SafetyConfig {
    fn default() -> Self {
        Self {
            item_budgets: ValuableItemBudgets::default(),
            purchases_enabled: false,
            donations_enabled: false,
            trades_enabled: false,
            allow_system_power: false,
            evidence_retention_days: 14,
            evidence_retention_megabytes: 250,
        }
    }
}

/// All valuable consumables are denied until the user sets a positive budget.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, Type)]
#[serde(default)]
pub struct ValuableItemBudgets {
    pub dice: u32,
    pub glitter: u32,
    pub eggs: u32,
    pub stickers: u32,
    pub vouchers: u32,
    pub shrine_donations: u32,
    pub other: BTreeMap<String, u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, Type)]
#[serde(default)]
pub struct DiscordConfig {
    pub enabled: bool,
    /// Identifier for a daemon-owned encrypted secret; never the secret itself.
    pub credential_ref: Option<String>,
    pub permissions: DiscordPermissions,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, Type)]
#[serde(default)]
#[allow(clippy::struct_excessive_bools)] // Wire permissions must remain individually auditable.
pub struct DiscordPermissions {
    pub status: bool,
    pub macro_control: bool,
    pub settings: bool,
    pub screenshots: bool,
    pub remote_input: bool,
    pub extension_import: bool,
    pub system_power: bool,
}

/// Portable data retained from one imported INI file. Secret-like values are
/// redacted during import and remain available only in the untouched source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct LegacySource {
    pub file_name: String,
    pub sha256: String,
    pub sections: BTreeMap<String, BTreeMap<String, String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, Type)]
pub struct LegacySnapshot {
    pub sources: Vec<LegacySource>,
}

#[cfg(test)]
mod tests {
    use super::Profile;

    #[test]
    fn dangerous_features_default_off_and_budgets_zero() {
        let profile = Profile::new("Safe profile");
        assert!(!profile.discord.enabled);
        assert!(!profile.discord.permissions.remote_input);
        assert_eq!(profile.safety.item_budgets.dice, 0);
        assert_eq!(profile.safety.item_budgets.glitter, 0);
        assert!(!profile.safety.donations_enabled);
    }
}
