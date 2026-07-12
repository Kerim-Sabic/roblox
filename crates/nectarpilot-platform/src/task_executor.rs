//! Safe adapter from native task plans to the exact Roblox session/input broker.

use std::{sync::RwLock, time::Duration};

use async_trait::async_trait;
use nectarpilot_contracts::{ActionResult, Detection, DetectionEvidence, Profile, StartMode};
use nectarpilot_core::{
    AutomationBackend, AutomationError, DetectedTarget, DetectionRequirement, TaskAction,
    TaskContext, TaskControl, TaskPlan, TaskRuntime, TaskRuntimeError, execute_task_plan,
};
use tokio_util::sync::CancellationToken;

use crate::{
    input::{BrokerError, InputAction, InputBroker, InputSink, Key, MouseButton},
    session::{
        ProcessId, RobloxSession, SessionError, SessionProbe, SessionTarget, WindowGeometry,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetectionContext {
    pub target: SessionTarget,
    pub geometry: WindowGeometry,
    pub calibration_revision: u64,
}

#[async_trait]
pub trait GuardDetector: Send {
    async fn detect(
        &mut self,
        requirement: &DetectionRequirement,
        context: DetectionContext,
    ) -> Detection<DetectedTarget>;
}

/// Explicit fail-closed detector for builds where reviewed visual assets are
/// unavailable. It is useful for wiring diagnostics without ever authorizing
/// live task input.
#[derive(Debug, Clone)]
pub struct UnavailableDetector {
    reason: String,
}

impl UnavailableDetector {
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

impl Default for UnavailableDetector {
    fn default() -> Self {
        Self::new("reviewed task detector assets are unavailable")
    }
}

#[async_trait]
impl GuardDetector for UnavailableDetector {
    async fn detect(
        &mut self,
        requirement: &DetectionRequirement,
        _context: DetectionContext,
    ) -> Detection<DetectedTarget> {
        Detection::Uncertain {
            reason: self.reason.clone(),
            evidence: DetectionEvidence {
                detector: requirement.detector_name().into(),
                observed_at: chrono::Utc::now(),
                region: None,
                artifact_id: None,
                notes: vec!["fail-closed detector placeholder".into()],
            },
        }
    }
}

/// Owns the only session and input broker used by one automation run.
pub struct NativeTaskRuntime<P, S, D>
where
    P: SessionProbe,
    S: InputSink,
    D: GuardDetector,
{
    probe: P,
    session: RobloxSession,
    broker: InputBroker<S>,
    detector: D,
    dry_run_intents: Vec<(String, TaskAction)>,
}

impl<P, S, D> NativeTaskRuntime<P, S, D>
where
    P: SessionProbe,
    S: InputSink,
    D: GuardDetector,
{
    pub fn attach(pid: ProcessId, probe: P, sink: S, detector: D) -> Result<Self, SessionError> {
        let session = RobloxSession::attach(&probe, pid)?;
        Ok(Self::from_session(session, probe, sink, detector))
    }

    #[must_use]
    pub fn from_session(session: RobloxSession, probe: P, sink: S, detector: D) -> Self {
        let broker = InputBroker::new(session.target(), sink);
        Self {
            probe,
            session,
            broker,
            detector,
            dry_run_intents: Vec::new(),
        }
    }

    #[must_use]
    pub fn session(&self) -> &RobloxSession {
        &self.session
    }

    #[must_use]
    pub fn broker(&self) -> &InputBroker<S> {
        &self.broker
    }

    pub fn broker_mut(&mut self) -> &mut InputBroker<S> {
        &mut self.broker
    }

    #[must_use]
    pub fn dry_run_intents(&self) -> &[(String, TaskAction)] {
        &self.dry_run_intents
    }

    pub fn detector_mut(&mut self) -> &mut D {
        &mut self.detector
    }

    fn ensure_ready(&mut self) -> Result<(), TaskRuntimeError> {
        let changed = self
            .session
            .refresh(&self.probe)
            .map_err(|error| map_session(&error))?;
        if changed {
            let _ = self.broker.cancel();
            return Err(TaskRuntimeError::CalibrationChanged);
        }
        let geometry = self.session.geometry();
        if geometry.minimized || geometry.client.width == 0 || geometry.client.height == 0 {
            let _ = self.broker.cancel();
            return Err(TaskRuntimeError::FocusLost);
        }
        if !self.session.is_foreground() {
            let _ = self.broker.cancel();
            return Err(TaskRuntimeError::FocusLost);
        }
        self.broker.resume_after_focus_check().map_err(map_broker)
    }

    async fn hold_key(
        &mut self,
        key: Key,
        duration: Duration,
        cancellation: &CancellationToken,
    ) -> Result<(), TaskRuntimeError> {
        self.broker
            .dispatch(InputAction::KeyDown { key })
            .map_err(map_broker)?;
        let cancelled = cancellable_delay(duration, cancellation).await;
        let release = self
            .broker
            .dispatch(InputAction::KeyUp { key })
            .map_err(map_broker);
        if cancelled {
            let _ = self.broker.cancel();
            return Err(TaskRuntimeError::Cancelled);
        }
        release
    }

    async fn hold_button(
        &mut self,
        button: MouseButton,
        duration: Duration,
        cancellation: &CancellationToken,
    ) -> Result<(), TaskRuntimeError> {
        self.broker
            .dispatch(InputAction::MouseDown { button })
            .map_err(map_broker)?;
        let cancelled = cancellable_delay(duration, cancellation).await;
        let release = self
            .broker
            .dispatch(InputAction::MouseUp { button })
            .map_err(map_broker);
        if cancelled {
            let _ = self.broker.cancel();
            return Err(TaskRuntimeError::Cancelled);
        }
        release
    }
}

#[async_trait]
impl<P, S, D> TaskRuntime for NativeTaskRuntime<P, S, D>
where
    P: SessionProbe + Send + Sync,
    S: InputSink,
    D: GuardDetector,
{
    async fn preflight(&mut self, _mode: StartMode) -> Result<(), TaskRuntimeError> {
        self.ensure_ready()
    }

    async fn detect(&mut self, requirement: &DetectionRequirement) -> Detection<DetectedTarget> {
        if let Err(error) = self.ensure_ready() {
            return precondition_detection(requirement, error);
        }
        let context = DetectionContext {
            target: self.session.target(),
            geometry: self.session.geometry(),
            calibration_revision: self.session.geometry_revision(),
        };
        let detection = self.detector.detect(requirement, context).await;
        match detection {
            Detection::Found {
                value,
                confidence: _,
                evidence,
            } if value.calibration_revision != context.calibration_revision => {
                Detection::Uncertain {
                    reason: "detector result used a stale geometry calibration".into(),
                    evidence,
                }
            }
            Detection::Found {
                value: _,
                confidence: _,
                evidence,
            } if evidence.region.is_some_and(|region| !region.is_valid()) => Detection::Uncertain {
                reason: "detector evidence has an invalid viewport region".into(),
                evidence,
            },
            other => other,
        }
    }

    async fn perform(
        &mut self,
        action: &TaskAction,
        cancellation: &CancellationToken,
    ) -> Result<(), TaskRuntimeError> {
        self.ensure_ready()?;
        let duration = Duration::from_millis(u64::from(action.duration_ms()));
        match action {
            TaskAction::Move { direction, .. } => {
                let key = match direction {
                    nectarpilot_core::dsl::MoveDirection::Forward => Key::Forward,
                    nectarpilot_core::dsl::MoveDirection::Backward => Key::Backward,
                    nectarpilot_core::dsl::MoveDirection::Left => Key::Left,
                    nectarpilot_core::dsl::MoveDirection::Right => Key::Right,
                };
                self.hold_key(key, duration, cancellation).await
            }
            TaskAction::Tap { control, .. } => {
                let key = match control {
                    TaskControl::Jump => Key::Jump,
                    TaskControl::Interact => Key::Interact,
                    TaskControl::Shift => Key::Shift,
                };
                self.hold_key(key, duration, cancellation).await
            }
            TaskAction::Attack { .. } => {
                self.hold_button(MouseButton::Left, duration, cancellation)
                    .await
            }
            TaskAction::Wait { .. } => {
                if cancellable_delay(duration, cancellation).await {
                    Err(TaskRuntimeError::Cancelled)
                } else {
                    Ok(())
                }
            }
        }
    }

    fn record_dry_run(&mut self, task_id: &str, action: &TaskAction) {
        self.dry_run_intents
            .push((task_id.to_owned(), action.clone()));
    }

    async fn release_all(&mut self) -> Result<(), TaskRuntimeError> {
        self.broker.release_all().map_err(map_broker)
    }
}

/// `AutomationBackend` implementation that can be placed directly behind the
/// daemon engine once the desktop has explicitly adopted a Roblox PID.
pub struct NativeAutomationBackend<R: TaskRuntime> {
    runtime: tokio::sync::Mutex<R>,
    plan: RwLock<TaskPlan>,
    mode: RwLock<StartMode>,
}

#[cfg(windows)]
pub type WindowsNativeTaskRuntime<D> = NativeTaskRuntime<
    crate::windows_backend::WindowsSessionProbe,
    crate::windows_backend::WindowsInputSink,
    D,
>;

/// Adopts an exact Roblox PID and returns a live backend wired to the native
/// Windows session probe and input sink. The caller must supply a reviewed
/// detector and validated task plan; no permissive fallback detector exists.
#[cfg(windows)]
pub fn attach_windows_backend<D>(
    pid: ProcessId,
    detector: D,
    plan: TaskPlan,
) -> Result<NativeAutomationBackend<WindowsNativeTaskRuntime<D>>, AutomationError>
where
    D: GuardDetector + 'static,
{
    let runtime = NativeTaskRuntime::attach(
        pid,
        crate::windows_backend::WindowsSessionProbe,
        crate::windows_backend::WindowsInputSink,
        detector,
    )
    .map_err(|error| AutomationError::Backend(error.to_string()))?;
    NativeAutomationBackend::new(runtime, plan)
}

impl<R: TaskRuntime> NativeAutomationBackend<R> {
    pub fn new(runtime: R, plan: TaskPlan) -> Result<Self, AutomationError> {
        plan.validate()
            .map_err(|error| AutomationError::Backend(error.to_string()))?;
        Ok(Self {
            runtime: tokio::sync::Mutex::new(runtime),
            plan: RwLock::new(plan),
            mode: RwLock::new(StartMode::Diagnostics),
        })
    }

    pub fn replace_plan(&self, plan: TaskPlan) -> Result<(), AutomationError> {
        plan.validate()
            .map_err(|error| AutomationError::Backend(error.to_string()))?;
        *self
            .plan
            .write()
            .map_err(|_| AutomationError::Backend("native plan lock poisoned".into()))? = plan;
        Ok(())
    }
}

#[async_trait]
impl<R> AutomationBackend for NativeAutomationBackend<R>
where
    R: TaskRuntime + 'static,
{
    async fn preflight(&self, _profile: &Profile, mode: StartMode) -> Result<(), AutomationError> {
        {
            let plan = self
                .plan
                .read()
                .map_err(|_| AutomationError::Backend("native plan lock poisoned".into()))?;
            plan.validate()
                .map_err(|error| AutomationError::Backend(error.to_string()))?;
        }
        *self
            .mode
            .write()
            .map_err(|_| AutomationError::Backend("native mode lock poisoned".into()))? = mode;
        self.runtime
            .lock()
            .await
            .preflight(mode)
            .await
            .map_err(|error| AutomationError::Backend(error.to_string()))
    }

    async fn execute(&self, _profile: &Profile, context: TaskContext) -> ActionResult {
        let mode = self
            .mode
            .read()
            .map_or(StartMode::Diagnostics, |mode| *mode);
        let plan = self
            .plan
            .read()
            .map_or_else(|_| poisoned_plan(), |plan| plan.clone());
        execute_task_plan(&mut *self.runtime.lock().await, &plan, mode, context).await
    }

    async fn reconnect_attempt(
        &self,
        _profile: &Profile,
        _attempt: u8,
    ) -> Result<(), AutomationError> {
        Err(AutomationError::Backend(
            "native reconnect requires explicit Roblox session re-adoption".into(),
        ))
    }

    async fn release_all_inputs(&self) -> Result<(), AutomationError> {
        self.runtime
            .lock()
            .await
            .release_all()
            .await
            .map_err(|error| AutomationError::Backend(error.to_string()))
    }
}

fn poisoned_plan() -> TaskPlan {
    TaskPlan {
        name: "invalid-poisoned-plan".into(),
        tasks: Vec::new(),
    }
}

async fn cancellable_delay(duration: Duration, cancellation: &CancellationToken) -> bool {
    tokio::select! {
        () = cancellation.cancelled() => true,
        () = tokio::time::sleep(duration) => false,
    }
}

fn map_session(error: &SessionError) -> TaskRuntimeError {
    TaskRuntimeError::SessionChanged(error.to_string())
}

fn map_broker(error: BrokerError) -> TaskRuntimeError {
    match error {
        BrokerError::WrongForeground => TaskRuntimeError::FocusLost,
        other => TaskRuntimeError::Input(other.to_string()),
    }
}

fn precondition_detection(
    requirement: &DetectionRequirement,
    error: TaskRuntimeError,
) -> Detection<DetectedTarget> {
    let evidence = DetectionEvidence {
        detector: requirement.detector_name().into(),
        observed_at: chrono::Utc::now(),
        region: None,
        artifact_id: None,
        notes: vec![error.to_string()],
    };
    match error {
        TaskRuntimeError::FocusLost | TaskRuntimeError::CalibrationChanged => {
            Detection::Uncertain {
                reason: error.to_string(),
                evidence,
            }
        }
        other => Detection::Error {
            code: "session_precondition".into(),
            message: other.to_string(),
            evidence: Some(evidence),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use nectarpilot_contracts::{ActionOutcome, DetectionEvidence};
    use nectarpilot_core::{
        GuardedTask, TaskKind,
        dsl::MoveDirection,
        tasks::{evidence, execute_task_plan},
    };

    use super::*;
    use crate::{
        input::MockInputSink,
        session::{MockSessionProbe, WindowHandle, WindowSnapshot},
    };

    struct QueueDetector {
        detections: VecDeque<Detection<DetectedTarget>>,
        lose_focus: Option<(MockSessionProbe, WindowSnapshot)>,
    }

    #[async_trait]
    impl GuardDetector for QueueDetector {
        async fn detect(
            &mut self,
            _requirement: &DetectionRequirement,
            _context: DetectionContext,
        ) -> Detection<DetectedTarget> {
            if let Some((probe, mut snapshot)) = self.lose_focus.take() {
                snapshot.is_foreground = false;
                probe.set_snapshot(snapshot);
            }
            self.detections.pop_front().expect("queued detection")
        }
    }

    fn target() -> SessionTarget {
        SessionTarget {
            pid: ProcessId::new(42).expect("pid"),
            window: WindowHandle::new(100).expect("window"),
        }
    }

    fn snapshot() -> WindowSnapshot {
        let rectangle = crate::session::Rect {
            left: 0,
            top: 0,
            width: 1280,
            height: 720,
        };
        WindowSnapshot {
            target: target(),
            geometry: WindowGeometry {
                outer: rectangle,
                client: rectangle,
                monitor: rectangle,
                dpi: 96,
                minimized: false,
                fullscreen: false,
            },
            is_foreground: true,
        }
    }

    fn found(label: &str, revision: u64) -> Detection<DetectedTarget> {
        Detection::Found {
            value: DetectedTarget {
                label: label.into(),
                calibration_revision: revision,
            },
            confidence: 0.99,
            evidence: evidence("mock", "fixture"),
        }
    }

    fn plan() -> TaskPlan {
        TaskPlan {
            name: "native test".into(),
            tasks: vec![GuardedTask::new(
                "travel",
                TaskKind::Travel {
                    destination: "Sunflower".into(),
                    route_anchor: "Hive".into(),
                },
                vec![TaskAction::Move {
                    direction: MoveDirection::Forward,
                    duration_ms: 1,
                }],
            )],
        }
    }

    fn runtime(
        detection: Detection<DetectedTarget>,
    ) -> NativeTaskRuntime<MockSessionProbe, MockInputSink, QueueDetector> {
        let snapshot = snapshot();
        NativeTaskRuntime::attach(
            target().pid,
            MockSessionProbe::new(snapshot),
            MockInputSink::new(Some(target())),
            QueueDetector {
                detections: vec![detection].into(),
                lose_focus: None,
            },
        )
        .expect("runtime")
    }

    #[tokio::test]
    async fn uncertain_detection_sends_no_native_input() {
        let detection = Detection::Uncertain {
            reason: "ambiguous route anchor".into(),
            evidence: DetectionEvidence {
                detector: "route_anchor".into(),
                observed_at: chrono::Utc::now(),
                region: None,
                artifact_id: None,
                notes: Vec::new(),
            },
        };
        let mut runtime = runtime(detection);
        let result = execute_task_plan(
            &mut runtime,
            &plan(),
            StartMode::Normal,
            TaskContext::unpaused(CancellationToken::new()),
        )
        .await;
        assert_eq!(result.outcome, ActionOutcome::NeedsAttention);
        assert!(runtime.broker().sink().actions().is_empty());
    }

    #[tokio::test]
    async fn unavailable_detector_is_never_actionable() {
        let mut detector = UnavailableDetector::default();
        let detection = detector
            .detect(
                &DetectionRequirement::RouteAnchor {
                    anchor: "Hive".into(),
                    destination: "Sunflower".into(),
                },
                DetectionContext {
                    target: target(),
                    geometry: snapshot().geometry,
                    calibration_revision: 0,
                },
            )
            .await;
        assert!(detection.actionable(0.0).is_none());
    }

    #[tokio::test]
    async fn dry_run_records_intent_without_native_input() {
        let mut runtime = runtime(found("Hive", 0));
        let result = execute_task_plan(
            &mut runtime,
            &plan(),
            StartMode::DryRun,
            TaskContext::unpaused(CancellationToken::new()),
        )
        .await;
        assert_eq!(result.outcome, ActionOutcome::Succeeded);
        assert_eq!(runtime.dry_run_intents().len(), 1);
        assert!(runtime.broker().sink().actions().is_empty());
    }

    #[tokio::test]
    async fn focus_loss_between_detection_and_action_sends_no_input() {
        let snapshot = snapshot();
        let probe = MockSessionProbe::new(snapshot);
        let detector = QueueDetector {
            detections: vec![found("Hive", 0)].into(),
            lose_focus: Some((probe.clone(), snapshot)),
        };
        let mut runtime = NativeTaskRuntime::attach(
            target().pid,
            probe,
            MockInputSink::new(Some(target())),
            detector,
        )
        .expect("runtime");

        let result = execute_task_plan(
            &mut runtime,
            &plan(),
            StartMode::Normal,
            TaskContext::unpaused(CancellationToken::new()),
        )
        .await;
        assert_eq!(result.outcome, ActionOutcome::NeedsAttention);
        assert!(runtime.broker().sink().actions().is_empty());
    }

    #[tokio::test]
    async fn live_action_is_balanced_key_down_and_key_up() {
        let mut runtime = runtime(found("Hive", 0));
        let result = execute_task_plan(
            &mut runtime,
            &plan(),
            StartMode::Normal,
            TaskContext::unpaused(CancellationToken::new()),
        )
        .await;
        assert_eq!(result.outcome, ActionOutcome::Succeeded);
        assert_eq!(
            runtime.broker().sink().actions(),
            &[
                InputAction::KeyDown { key: Key::Forward },
                InputAction::KeyUp { key: Key::Forward },
            ]
        );
        assert!(runtime.broker().is_clean());
    }

    #[tokio::test]
    async fn stale_calibration_detection_sends_no_input() {
        let mut runtime = runtime(found("Hive", 99));
        let result = execute_task_plan(
            &mut runtime,
            &plan(),
            StartMode::Normal,
            TaskContext::unpaused(CancellationToken::new()),
        )
        .await;
        assert_eq!(result.outcome, ActionOutcome::NeedsAttention);
        assert!(runtime.broker().sink().actions().is_empty());
    }
}
