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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(default)]
pub struct AutomationConfig {
    pub gathering_enabled: bool,
    pub reconnect_enabled: bool,
    pub rotations: Vec<FieldRotation>,
    pub features: FeatureFlags,
    pub hotkeys: HotkeyConfig,
}

impl Default for AutomationConfig {
    fn default() -> Self {
        Self {
            gathering_enabled: false,
            reconnect_enabled: true,
            rotations: Vec::new(),
            features: FeatureFlags::default(),
            hotkeys: HotkeyConfig::default(),
        }
    }
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
