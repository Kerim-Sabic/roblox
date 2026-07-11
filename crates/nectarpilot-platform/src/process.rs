use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::session::ProcessId;

/// Includes the OS creation timestamp to protect against PID reuse.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub pid: ProcessId,
    pub created_at_ticks: u64,
    pub executable_path: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessOwnership {
    LaunchedByNectarPilot,
    ExplicitlyAdopted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrackedProcess {
    pub identity: ProcessIdentity,
    pub ownership: ProcessOwnership,
}

/// Deliberate proof required by the API before an existing process is adopted.
#[derive(Debug)]
pub struct ExplicitAdoption {
    confirmed: bool,
}

impl ExplicitAdoption {
    #[must_use]
    pub const fn confirmed_by_user() -> Self {
        Self { confirmed: true }
    }
}

#[derive(Debug, Error, PartialEq)]
pub enum ProcessError {
    #[error("process {0} is not tracked and cannot be terminated")]
    Untracked(u32),
    #[error("process {0} no longer exists")]
    NotFound(u32),
    #[error("process {0} was reused or changed identity; operation refused")]
    IdentityChanged(u32),
    #[error("explicit adoption was not confirmed")]
    AdoptionNotConfirmed,
    #[error("platform process operation failed: {0}")]
    Platform(String),
}

pub trait ProcessController: Send {
    fn identity(&self, pid: ProcessId) -> Result<Option<ProcessIdentity>, ProcessError>;

    fn terminate_exact(&mut self, identity: &ProcessIdentity) -> Result<(), ProcessError>;
}

/// Registry that permits termination only for the exact identities it recorded.
pub struct ProcessRegistry<C: ProcessController> {
    controller: C,
    tracked: HashMap<ProcessId, TrackedProcess>,
}

impl<C: ProcessController> ProcessRegistry<C> {
    pub fn new(controller: C) -> Self {
        Self {
            controller,
            tracked: HashMap::new(),
        }
    }

    pub fn record_launched(&mut self, pid: ProcessId) -> Result<TrackedProcess, ProcessError> {
        self.record(pid, ProcessOwnership::LaunchedByNectarPilot)
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "the proof is deliberately consumed so adoption requires a fresh action"
    )]
    pub fn adopt(
        &mut self,
        pid: ProcessId,
        proof: ExplicitAdoption,
    ) -> Result<TrackedProcess, ProcessError> {
        if !proof.confirmed {
            return Err(ProcessError::AdoptionNotConfirmed);
        }
        self.record(pid, ProcessOwnership::ExplicitlyAdopted)
    }

    pub fn tracked(&self, pid: ProcessId) -> Option<&TrackedProcess> {
        self.tracked.get(&pid)
    }

    pub fn forget(&mut self, pid: ProcessId) -> Option<TrackedProcess> {
        self.tracked.remove(&pid)
    }

    pub fn terminate(&mut self, pid: ProcessId) -> Result<(), ProcessError> {
        let expected = self
            .tracked
            .get(&pid)
            .ok_or(ProcessError::Untracked(pid.get()))?
            .identity
            .clone();
        let current = self
            .controller
            .identity(pid)?
            .ok_or(ProcessError::NotFound(pid.get()))?;
        if current != expected {
            return Err(ProcessError::IdentityChanged(pid.get()));
        }
        self.controller.terminate_exact(&expected)?;
        self.tracked.remove(&pid);
        Ok(())
    }

    pub fn controller(&self) -> &C {
        &self.controller
    }

    fn record(
        &mut self,
        pid: ProcessId,
        ownership: ProcessOwnership,
    ) -> Result<TrackedProcess, ProcessError> {
        let identity = self
            .controller
            .identity(pid)?
            .ok_or(ProcessError::NotFound(pid.get()))?;
        let tracked = TrackedProcess {
            identity,
            ownership,
        };
        self.tracked.insert(pid, tracked.clone());
        Ok(tracked)
    }
}

#[derive(Clone, Default)]
pub struct MockProcessController {
    state: std::sync::Arc<std::sync::Mutex<MockProcessState>>,
}

#[derive(Default)]
struct MockProcessState {
    identities: HashMap<ProcessId, ProcessIdentity>,
    terminated: Vec<ProcessIdentity>,
}

impl MockProcessController {
    pub fn insert(&self, identity: ProcessIdentity) {
        self.state
            .lock()
            .expect("mock process lock poisoned")
            .identities
            .insert(identity.pid, identity);
    }

    #[must_use]
    pub fn terminated(&self) -> Vec<ProcessIdentity> {
        self.state
            .lock()
            .expect("mock process lock poisoned")
            .terminated
            .clone()
    }
}

impl ProcessController for MockProcessController {
    fn identity(&self, pid: ProcessId) -> Result<Option<ProcessIdentity>, ProcessError> {
        Ok(self
            .state
            .lock()
            .expect("mock process lock poisoned")
            .identities
            .get(&pid)
            .cloned())
    }

    fn terminate_exact(&mut self, identity: &ProcessIdentity) -> Result<(), ProcessError> {
        let mut state = self.state.lock().expect("mock process lock poisoned");
        if state.identities.get(&identity.pid) != Some(identity) {
            return Err(ProcessError::IdentityChanged(identity.pid.get()));
        }
        state.terminated.push(identity.clone());
        state.identities.remove(&identity.pid);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(pid: u32, created_at_ticks: u64) -> ProcessIdentity {
        ProcessIdentity {
            pid: ProcessId::new(pid).unwrap(),
            created_at_ticks,
            executable_path: Some(PathBuf::from(format!("process-{pid}.exe"))),
        }
    }

    #[test]
    fn never_terminates_an_unrelated_process() {
        let controller = MockProcessController::default();
        controller.insert(identity(10, 100));
        controller.insert(identity(20, 200));
        let observer = controller.clone();
        let mut registry = ProcessRegistry::new(controller);
        registry
            .record_launched(ProcessId::new(10).unwrap())
            .unwrap();

        let error = registry.terminate(ProcessId::new(20).unwrap()).unwrap_err();

        assert_eq!(error, ProcessError::Untracked(20));
        assert!(observer.terminated().is_empty());
    }

    #[test]
    fn refuses_a_reused_pid() {
        let controller = MockProcessController::default();
        controller.insert(identity(10, 100));
        let observer = controller.clone();
        let mut registry = ProcessRegistry::new(controller);
        registry
            .record_launched(ProcessId::new(10).unwrap())
            .unwrap();
        observer.insert(identity(10, 999));

        let error = registry.terminate(ProcessId::new(10).unwrap()).unwrap_err();

        assert_eq!(error, ProcessError::IdentityChanged(10));
        assert!(observer.terminated().is_empty());
    }
}
