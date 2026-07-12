//! Platform-neutral `NectarPilot` orchestration, persistence, and path language.

pub mod crash_guard;
pub mod dsl;
pub mod engine;
pub mod legacy_ini;
pub mod perception;
pub mod persistence;
pub mod quests;
pub mod reconnect;
pub mod scheduler;
pub mod tasks;
pub mod transport;

pub use engine::{
    AutomationBackend, AutomationEngine, AutomationError, LegacyExecutionPort, MockBackend,
    TaskContext,
};
pub use perception::{
    FieldCandidate, HiveCandidate, HiveState, LivePerception, MovementTarget, PromptCandidate,
    PromptKind, QuestCandidate,
};
pub use persistence::SqliteStore;
pub use tasks::{
    DetectedTarget, DetectionRequirement, GuardedTask, TaskAction, TaskControl, TaskKind, TaskPlan,
    TaskRuntime, TaskRuntimeError, execute_task_plan,
};
