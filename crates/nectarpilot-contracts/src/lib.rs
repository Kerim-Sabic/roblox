//! Versioned messages and configuration shared by every `NectarPilot` process.
//!
//! This crate intentionally contains data only. It has no filesystem, process,
//! input, or network access, which keeps the UI/daemon trust boundary explicit.

mod detection;
mod profile;
mod protocol;

pub mod bindings;

pub use detection::{Detection, DetectionEvidence, NormalizedRegion};
pub use profile::{
    AutomationConfig, DiscordConfig, DiscordPermissions, FeatureFlags, FieldRotation, HotkeyConfig,
    LegacySnapshot, LegacySource, PROFILE_SCHEMA_VERSION, Profile, SafetyConfig,
    ValuableItemBudgets,
};
pub use protocol::{
    ActionOutcome, ActionResult, Command, CommandEnvelope, DaemonEvent, EventEnvelope, EventLevel,
    PROTOCOL_VERSION, ReconnectProgress, RunSnapshot, RunState, StartMode,
};
