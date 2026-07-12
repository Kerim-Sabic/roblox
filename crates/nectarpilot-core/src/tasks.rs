//! Fail-closed, platform-neutral native automation task orchestration.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use nectarpilot_contracts::{ActionOutcome, ActionResult, Detection, DetectionEvidence, StartMode};
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{dsl::MoveDirection, engine::TaskContext};

const MINIMUM_CONFIDENCE: f32 = 0.75;
const MAX_TASKS: usize = 64;
const MAX_ACTIONS_PER_TASK: usize = 2_000;
const MAX_ACTION_DURATION_MS: u32 = 30_000;
const MAX_PLAN_DURATION_MS: u64 = 6 * 60 * 60 * 1_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TaskKind {
    Gather {
        field: String,
    },
    Travel {
        destination: String,
        route_anchor: String,
    },
    Combat {
        target: String,
    },
    Activity {
        name: String,
    },
}

impl TaskKind {
    #[must_use]
    pub fn requirement(&self) -> DetectionRequirement {
        match self {
            Self::Gather { field } => DetectionRequirement::CurrentField {
                field: field.clone(),
            },
            Self::Travel {
                destination,
                route_anchor,
            } => DetectionRequirement::RouteAnchor {
                anchor: route_anchor.clone(),
                destination: destination.clone(),
            },
            Self::Combat { target } => DetectionRequirement::CombatTarget {
                target: target.clone(),
            },
            Self::Activity { name } => DetectionRequirement::InteractionPrompt {
                activity: name.clone(),
            },
        }
    }

    fn label(&self) -> &str {
        match self {
            Self::Gather { field } => field,
            Self::Travel { destination, .. } => destination,
            Self::Combat { target } => target,
            Self::Activity { name } => name,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DetectionRequirement {
    CurrentField { field: String },
    RouteAnchor { anchor: String, destination: String },
    CombatTarget { target: String },
    InteractionPrompt { activity: String },
}

impl DetectionRequirement {
    #[must_use]
    pub fn expected_label(&self) -> &str {
        match self {
            Self::CurrentField { field } => field,
            Self::RouteAnchor { anchor, .. } => anchor,
            Self::CombatTarget { target } => target,
            Self::InteractionPrompt { activity } => activity,
        }
    }

    #[must_use]
    pub const fn detector_name(&self) -> &'static str {
        match self {
            Self::CurrentField { .. } => "current_field",
            Self::RouteAnchor { .. } => "route_anchor",
            Self::CombatTarget { .. } => "combat_target",
            Self::InteractionPrompt { .. } => "interaction_prompt",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedTarget {
    pub label: String,
    /// Geometry revision used to calibrate the detector result.
    pub calibration_revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskControl {
    Jump,
    Interact,
    Shift,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TaskAction {
    Move {
        direction: MoveDirection,
        duration_ms: u32,
    },
    Tap {
        control: TaskControl,
        #[serde(default = "default_tap_ms")]
        hold_ms: u32,
    },
    Attack {
        #[serde(default = "default_attack_ms")]
        hold_ms: u32,
    },
    Wait {
        duration_ms: u32,
    },
}

const fn default_tap_ms() -> u32 {
    60
}

const fn default_attack_ms() -> u32 {
    80
}

impl TaskAction {
    #[must_use]
    pub const fn duration_ms(&self) -> u32 {
        match self {
            Self::Move { duration_ms, .. } | Self::Wait { duration_ms } => *duration_ms,
            Self::Tap { hold_ms, .. } | Self::Attack { hold_ms } => *hold_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuardedTask {
    pub id: String,
    pub kind: TaskKind,
    #[serde(default = "default_confidence")]
    pub minimum_confidence: f32,
    pub actions: Vec<TaskAction>,
}

fn default_confidence() -> f32 {
    0.85
}

impl GuardedTask {
    #[must_use]
    pub fn new(id: impl Into<String>, kind: TaskKind, actions: Vec<TaskAction>) -> Self {
        Self {
            id: id.into(),
            kind,
            minimum_confidence: default_confidence(),
            actions,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskPlan {
    pub name: String,
    pub tasks: Vec<GuardedTask>,
}

impl TaskPlan {
    pub fn validate(&self) -> Result<(), TaskPlanError> {
        if self.name.trim().is_empty() || self.name.len() > 120 {
            return Err(TaskPlanError::InvalidPlanName);
        }
        if self.tasks.is_empty() || self.tasks.len() > MAX_TASKS {
            return Err(TaskPlanError::InvalidTaskCount(self.tasks.len()));
        }
        let mut total_duration = 0_u64;
        for task in &self.tasks {
            if task.id.trim().is_empty()
                || task.id.len() > 120
                || task.kind.label().trim().is_empty()
                || task.kind.label().len() > 120
                || matches!(
                    &task.kind,
                    TaskKind::Travel { route_anchor, .. }
                        if route_anchor.trim().is_empty() || route_anchor.len() > 120
                )
            {
                return Err(TaskPlanError::InvalidTaskIdentity(task.id.clone()));
            }
            if !task.minimum_confidence.is_finite()
                || !(MINIMUM_CONFIDENCE..=1.0).contains(&task.minimum_confidence)
            {
                return Err(TaskPlanError::UnsafeConfidence {
                    task: task.id.clone(),
                    confidence: task.minimum_confidence,
                });
            }
            if task.actions.is_empty() || task.actions.len() > MAX_ACTIONS_PER_TASK {
                return Err(TaskPlanError::InvalidActionCount {
                    task: task.id.clone(),
                    count: task.actions.len(),
                });
            }
            if matches!(task.kind, TaskKind::Travel { .. })
                && !task
                    .actions
                    .iter()
                    .any(|action| matches!(action, TaskAction::Move { .. }))
            {
                return Err(TaskPlanError::TravelWithoutMovement(task.id.clone()));
            }
            if matches!(task.kind, TaskKind::Combat { .. })
                && !task
                    .actions
                    .iter()
                    .any(|action| matches!(action, TaskAction::Attack { .. }))
            {
                return Err(TaskPlanError::CombatWithoutAttack(task.id.clone()));
            }
            for action in &task.actions {
                let duration = action.duration_ms();
                if duration == 0 || duration > MAX_ACTION_DURATION_MS {
                    return Err(TaskPlanError::InvalidActionDuration {
                        task: task.id.clone(),
                        duration_ms: duration,
                    });
                }
                total_duration = total_duration.saturating_add(u64::from(duration));
            }
        }
        if total_duration > MAX_PLAN_DURATION_MS {
            return Err(TaskPlanError::PlanTooLong(total_duration));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Error)]
pub enum TaskPlanError {
    #[error("task plan name is empty or too long")]
    InvalidPlanName,
    #[error("task plan contains an unsafe task count: {0}")]
    InvalidTaskCount(usize),
    #[error("task identity is empty or too long: {0:?}")]
    InvalidTaskIdentity(String),
    #[error("task {task:?} has unsafe confidence {confidence}")]
    UnsafeConfidence { task: String, confidence: f32 },
    #[error("task {task:?} has an unsafe action count: {count}")]
    InvalidActionCount { task: String, count: usize },
    #[error("travel task {0:?} contains no movement")]
    TravelWithoutMovement(String),
    #[error("combat task {0:?} contains no attack")]
    CombatWithoutAttack(String),
    #[error("task {task:?} has invalid action duration {duration_ms} ms")]
    InvalidActionDuration { task: String, duration_ms: u32 },
    #[error("task plan duration {0} ms exceeds the six-hour ceiling")]
    PlanTooLong(u64),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TaskRuntimeError {
    #[error("task was cancelled")]
    Cancelled,
    #[error("the exact Roblox window lost focus")]
    FocusLost,
    #[error("the adopted Roblox session changed: {0}")]
    SessionChanged(String),
    #[error("window geometry changed; recalibration is required")]
    CalibrationChanged,
    #[error("native input failed: {0}")]
    Input(String),
    #[error("detector failed: {0}")]
    Detector(String),
}

#[async_trait]
pub trait TaskRuntime: Send {
    async fn preflight(&mut self, mode: StartMode) -> Result<(), TaskRuntimeError>;

    async fn detect(&mut self, requirement: &DetectionRequirement) -> Detection<DetectedTarget>;

    async fn perform(
        &mut self,
        action: &TaskAction,
        cancellation: &CancellationToken,
    ) -> Result<(), TaskRuntimeError>;

    /// Records an intent without invoking the live input path.
    fn record_dry_run(&mut self, task_id: &str, action: &TaskAction);

    async fn release_all(&mut self) -> Result<(), TaskRuntimeError>;
}

/// Executes a validated task plan. Detection must be a confident, exact-label
/// `Found` before actions are considered. Unknown, missing, stale, or
/// mismatched detections produce no live input.
#[allow(clippy::too_many_lines)] // Linear safety sequence is intentionally auditable in one place.
pub async fn execute_task_plan<R: TaskRuntime + ?Sized>(
    runtime: &mut R,
    plan: &TaskPlan,
    mode: StartMode,
    mut context: TaskContext,
) -> ActionResult {
    let started_at = Utc::now();
    if let Err(error) = plan.validate() {
        return result(
            plan,
            mode,
            ActionOutcome::Failed,
            started_at,
            0,
            0,
            error.to_string(),
        );
    }
    let mut completed_tasks = 0_usize;
    let mut considered_actions = 0_usize;

    for task in &plan.tasks {
        if context.checkpoint().await.is_err() {
            let _ = runtime.release_all().await;
            return result(
                plan,
                mode,
                ActionOutcome::Cancelled,
                started_at,
                completed_tasks,
                considered_actions,
                "task plan cancelled",
            );
        }

        let requirement = task.kind.requirement();
        let detection = runtime.detect(&requirement).await;
        let Some(target) = detection.actionable(task.minimum_confidence) else {
            let outcome = if matches!(detection, Detection::Error { .. }) {
                ActionOutcome::Failed
            } else {
                ActionOutcome::NeedsAttention
            };
            let _ = runtime.release_all().await;
            return result(
                plan,
                mode,
                outcome,
                started_at,
                completed_tasks,
                considered_actions,
                detection_failure_message(&detection, &requirement),
            );
        };
        if !target
            .label
            .eq_ignore_ascii_case(requirement.expected_label())
        {
            let _ = runtime.release_all().await;
            return result(
                plan,
                mode,
                ActionOutcome::NeedsAttention,
                started_at,
                completed_tasks,
                considered_actions,
                format!(
                    "{} detected {:?}, expected {:?}",
                    requirement.detector_name(),
                    target.label,
                    requirement.expected_label()
                ),
            );
        }

        if mode != StartMode::Diagnostics {
            for action in &task.actions {
                if context.checkpoint().await.is_err() {
                    let _ = runtime.release_all().await;
                    return result(
                        plan,
                        mode,
                        ActionOutcome::Cancelled,
                        started_at,
                        completed_tasks,
                        considered_actions,
                        "task plan cancelled",
                    );
                }
                considered_actions += 1;
                if mode == StartMode::DryRun {
                    runtime.record_dry_run(&task.id, action);
                    continue;
                }
                if let Err(error) = runtime.perform(action, &context.cancellation_token()).await {
                    let _ = runtime.release_all().await;
                    let outcome = match error {
                        TaskRuntimeError::Cancelled => ActionOutcome::Cancelled,
                        TaskRuntimeError::FocusLost
                        | TaskRuntimeError::SessionChanged(_)
                        | TaskRuntimeError::CalibrationChanged => ActionOutcome::NeedsAttention,
                        TaskRuntimeError::Input(_) | TaskRuntimeError::Detector(_) => {
                            ActionOutcome::Failed
                        }
                    };
                    return result(
                        plan,
                        mode,
                        outcome,
                        started_at,
                        completed_tasks,
                        considered_actions,
                        error.to_string(),
                    );
                }
            }
        }
        completed_tasks += 1;
    }

    if let Err(error) = runtime.release_all().await {
        return result(
            plan,
            mode,
            ActionOutcome::Failed,
            started_at,
            completed_tasks,
            considered_actions,
            format!("input cleanup failed: {error}"),
        );
    }
    result(
        plan,
        mode,
        ActionOutcome::Succeeded,
        started_at,
        completed_tasks,
        considered_actions,
        match mode {
            StartMode::Normal => "native task plan completed",
            StartMode::DryRun => "dry-run task plan validated without live input",
            StartMode::Diagnostics => "task detections validated without actions",
        },
    )
}

fn detection_failure_message(
    detection: &Detection<DetectedTarget>,
    requirement: &DetectionRequirement,
) -> String {
    match detection {
        Detection::Found { confidence, .. } => format!(
            "{} confidence {confidence:.3} is below the action threshold",
            requirement.detector_name()
        ),
        Detection::NotFound { .. } => format!("{} was not found", requirement.detector_name()),
        Detection::Uncertain { reason, .. } => {
            format!("{} is uncertain: {reason}", requirement.detector_name())
        }
        Detection::Error { code, message, .. } => {
            format!("{} error {code}: {message}", requirement.detector_name())
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn result(
    plan: &TaskPlan,
    mode: StartMode,
    outcome: ActionOutcome,
    started_at: DateTime<Utc>,
    completed_tasks: usize,
    considered_actions: usize,
    message: impl Into<String>,
) -> ActionResult {
    ActionResult {
        action: format!("native_plan:{}", plan.name),
        outcome,
        started_at,
        finished_at: Utc::now(),
        message: message.into(),
        details: json!({
            "mode": mode,
            "completed_tasks": completed_tasks,
            "considered_actions": considered_actions,
        }),
    }
}

#[must_use]
pub fn evidence(detector: &str, note: impl Into<String>) -> DetectionEvidence {
    DetectionEvidence {
        detector: detector.into(),
        observed_at: Utc::now(),
        region: None,
        artifact_id: None,
        notes: vec![note.into()],
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use nectarpilot_contracts::Detection;

    use super::*;

    struct MockRuntime {
        detections: VecDeque<Detection<DetectedTarget>>,
        performed: Vec<TaskAction>,
        dry_run: Vec<TaskAction>,
        released: usize,
    }

    #[async_trait]
    impl TaskRuntime for MockRuntime {
        async fn preflight(&mut self, _mode: StartMode) -> Result<(), TaskRuntimeError> {
            Ok(())
        }

        async fn detect(
            &mut self,
            _requirement: &DetectionRequirement,
        ) -> Detection<DetectedTarget> {
            self.detections.pop_front().expect("queued detection")
        }

        async fn perform(
            &mut self,
            action: &TaskAction,
            _cancellation: &CancellationToken,
        ) -> Result<(), TaskRuntimeError> {
            self.performed.push(action.clone());
            Ok(())
        }

        fn record_dry_run(&mut self, _task_id: &str, action: &TaskAction) {
            self.dry_run.push(action.clone());
        }

        async fn release_all(&mut self) -> Result<(), TaskRuntimeError> {
            self.released += 1;
            Ok(())
        }
    }

    fn found(label: &str) -> Detection<DetectedTarget> {
        Detection::Found {
            value: DetectedTarget {
                label: label.into(),
                calibration_revision: 0,
            },
            confidence: 0.99,
            evidence: evidence("mock", "fixture"),
        }
    }

    fn plan() -> TaskPlan {
        TaskPlan {
            name: "all categories".into(),
            tasks: vec![
                GuardedTask::new(
                    "travel-sunflower",
                    TaskKind::Travel {
                        destination: "Sunflower".into(),
                        route_anchor: "Hive".into(),
                    },
                    vec![TaskAction::Move {
                        direction: MoveDirection::Forward,
                        duration_ms: 1,
                    }],
                ),
                GuardedTask::new(
                    "gather-sunflower",
                    TaskKind::Gather {
                        field: "Sunflower".into(),
                    },
                    vec![TaskAction::Move {
                        direction: MoveDirection::Left,
                        duration_ms: 1,
                    }],
                ),
                GuardedTask::new(
                    "combat-ladybug",
                    TaskKind::Combat {
                        target: "Ladybug".into(),
                    },
                    vec![TaskAction::Attack { hold_ms: 1 }],
                ),
                GuardedTask::new(
                    "activity-dispenser",
                    TaskKind::Activity {
                        name: "Treat Dispenser".into(),
                    },
                    vec![TaskAction::Tap {
                        control: TaskControl::Interact,
                        hold_ms: 1,
                    }],
                ),
            ],
        }
    }

    fn runtime(detections: Vec<Detection<DetectedTarget>>) -> MockRuntime {
        MockRuntime {
            detections: detections.into(),
            performed: Vec::new(),
            dry_run: Vec::new(),
            released: 0,
        }
    }

    #[tokio::test]
    async fn uncertain_detection_never_executes_actions() {
        let mut runtime = runtime(vec![Detection::Uncertain {
            reason: "OCR returned Unknown".into(),
            evidence: evidence("route_anchor", "ambiguous"),
        }]);
        let result = execute_task_plan(
            &mut runtime,
            &plan(),
            StartMode::Normal,
            TaskContext::unpaused(CancellationToken::new()),
        )
        .await;
        assert_eq!(result.outcome, ActionOutcome::NeedsAttention);
        assert!(runtime.performed.is_empty());
        assert_eq!(runtime.released, 1);
    }

    #[tokio::test]
    async fn dry_run_records_every_category_without_performing_input() {
        let mut runtime = runtime(vec![
            found("Hive"),
            found("Sunflower"),
            found("Ladybug"),
            found("Treat Dispenser"),
        ]);
        let result = execute_task_plan(
            &mut runtime,
            &plan(),
            StartMode::DryRun,
            TaskContext::unpaused(CancellationToken::new()),
        )
        .await;
        assert_eq!(result.outcome, ActionOutcome::Succeeded);
        assert_eq!(runtime.dry_run.len(), 4);
        assert!(runtime.performed.is_empty());
    }

    #[tokio::test]
    async fn mismatched_found_label_never_executes_actions() {
        let mut runtime = runtime(vec![found("Pine Tree")]);
        let result = execute_task_plan(
            &mut runtime,
            &plan(),
            StartMode::Normal,
            TaskContext::unpaused(CancellationToken::new()),
        )
        .await;
        assert_eq!(result.outcome, ActionOutcome::NeedsAttention);
        assert!(runtime.performed.is_empty());
    }

    #[tokio::test]
    async fn pre_cancelled_plan_releases_without_detection_or_actions() {
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let mut runtime = runtime(Vec::new());
        let result = execute_task_plan(
            &mut runtime,
            &plan(),
            StartMode::Normal,
            TaskContext::unpaused(cancellation),
        )
        .await;
        assert_eq!(result.outcome, ActionOutcome::Cancelled);
        assert!(runtime.performed.is_empty());
        assert_eq!(runtime.released, 1);
    }

    #[test]
    fn rejects_travel_without_movement_and_combat_without_attack() {
        let mut invalid = plan();
        invalid.tasks[0].actions = vec![TaskAction::Wait { duration_ms: 1 }];
        assert!(matches!(
            invalid.validate(),
            Err(TaskPlanError::TravelWithoutMovement(_))
        ));
        invalid.tasks[0].actions = vec![TaskAction::Move {
            direction: MoveDirection::Forward,
            duration_ms: 1,
        }];
        invalid.tasks[2].actions = vec![TaskAction::Wait { duration_ms: 1 }];
        assert!(matches!(
            invalid.validate(),
            Err(TaskPlanError::CombatWithoutAttack(_))
        ));
    }

    #[tokio::test]
    async fn live_plan_executes_gather_travel_combat_and_activity() {
        let mut runtime = runtime(vec![
            found("Hive"),
            found("Sunflower"),
            found("Ladybug"),
            found("Treat Dispenser"),
        ]);
        let result = execute_task_plan(
            &mut runtime,
            &plan(),
            StartMode::Normal,
            TaskContext::unpaused(CancellationToken::new()),
        )
        .await;
        assert_eq!(result.outcome, ActionOutcome::Succeeded);
        assert_eq!(runtime.performed.len(), 4);
        assert_eq!(runtime.released, 1);
    }
}
