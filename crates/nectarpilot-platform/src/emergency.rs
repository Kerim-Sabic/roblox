use thiserror::Error;

use crate::input::{BrokerError, InputBroker, InputSink};

pub const EMERGENCY_STOP_CHORD: &str = "Ctrl+Shift+F12";

#[derive(Debug, Error)]
pub enum EmergencyStopError {
    #[error("global emergency hotkey backend failed: {0}")]
    Backend(String),
    #[error("emergency stop could not release all input: {0}")]
    Release(#[from] BrokerError),
}

/// Backend registration and polling must occur on the same thread. Native
/// Windows registration is provided by `WindowsEmergencyHotkey`.
pub trait EmergencyHotkeyBackend {
    fn poll_triggered(&mut self) -> Result<bool, EmergencyStopError>;
}

/// Latches the hard stop and releases all broker-owned keys/buttons before
/// reporting the trigger to the state machine.
pub struct EmergencyStop<B> {
    backend: B,
    latched: bool,
}

impl<B: EmergencyHotkeyBackend> EmergencyStop<B> {
    pub const fn new(backend: B) -> Self {
        Self {
            backend,
            latched: false,
        }
    }

    pub const fn is_latched(&self) -> bool {
        self.latched
    }

    pub fn poll<S: InputSink>(
        &mut self,
        broker: &mut InputBroker<S>,
    ) -> Result<bool, EmergencyStopError> {
        if self.latched {
            return Ok(true);
        }
        match self.backend.poll_triggered() {
            Ok(false) => Ok(false),
            Ok(true) => {
                broker.cancel()?;
                self.latched = true;
                Ok(true)
            }
            Err(error) => {
                // Failure to observe the hard-stop channel is itself unsafe;
                // release input before surfacing the backend fault.
                let _ = broker.cancel();
                Err(error)
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct MockEmergencyHotkey {
    triggered: bool,
    failure: Option<String>,
}

impl MockEmergencyHotkey {
    pub fn trigger(&mut self) {
        self.triggered = true;
    }

    pub fn fail(&mut self, message: impl Into<String>) {
        self.failure = Some(message.into());
    }
}

impl EmergencyHotkeyBackend for MockEmergencyHotkey {
    fn poll_triggered(&mut self) -> Result<bool, EmergencyStopError> {
        if let Some(message) = self.failure.take() {
            return Err(EmergencyStopError::Backend(message));
        }
        Ok(std::mem::take(&mut self.triggered))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{InputAction, Key, MockInputSink};
    use crate::session::{ProcessId, SessionTarget, WindowHandle};

    #[test]
    fn emergency_stop_releases_inputs_and_latches() {
        let target = SessionTarget {
            pid: ProcessId::new(100).unwrap(),
            window: WindowHandle::new(200).unwrap(),
        };
        let mut broker = InputBroker::new(target, MockInputSink::new(Some(target)));
        broker
            .dispatch(InputAction::KeyDown { key: Key::Forward })
            .unwrap();
        let mut backend = MockEmergencyHotkey::default();
        backend.trigger();
        let mut emergency = EmergencyStop::new(backend);

        assert!(emergency.poll(&mut broker).unwrap());
        assert!(emergency.is_latched());
        assert!(broker.is_clean());
        assert!(
            broker
                .sink()
                .actions()
                .contains(&InputAction::KeyUp { key: Key::Forward })
        );
    }
}
