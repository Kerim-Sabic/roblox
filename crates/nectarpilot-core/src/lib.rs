//! Platform-neutral `NectarPilot` orchestration, persistence, and path language.

pub mod crash_guard;
pub mod dsl;
pub mod engine;
pub mod legacy_ini;
pub mod persistence;
pub mod quests;
pub mod reconnect;
pub mod scheduler;
pub mod transport;

pub use engine::{AutomationBackend, AutomationEngine, AutomationError, MockBackend, TaskContext};
pub use persistence::SqliteStore;
