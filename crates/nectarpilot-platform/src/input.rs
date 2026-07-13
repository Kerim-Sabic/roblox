use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::session::SessionTarget;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Key {
    Forward,
    Backward,
    Left,
    Right,
    Jump,
    Escape,
    Interact,
    Shift,
    Control,
    F1,
    F2,
    F3,
    F12,
    Digit(u8),
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputAction {
    KeyDown {
        key: Key,
    },
    KeyUp {
        key: Key,
    },
    MouseDown {
        button: MouseButton,
    },
    MouseUp {
        button: MouseButton,
    },
    /// Coordinates are normalized to the current client area and checked again
    /// by the native backend at dispatch time.
    MouseMoveClient {
        x: f32,
        y: f32,
    },
    MouseWheel {
        delta: i32,
    },
}

impl InputAction {
    fn validate(self) -> Result<Self, BrokerError> {
        match self {
            Self::MouseMoveClient { x, y }
                if !x.is_finite()
                    || !y.is_finite()
                    || !(0.0..=1.0).contains(&x)
                    || !(0.0..=1.0).contains(&y) =>
            {
                Err(BrokerError::InvalidCoordinates)
            }
            Self::KeyDown {
                key: Key::Digit(value),
            }
            | Self::KeyUp {
                key: Key::Digit(value),
            } if value > 9 => Err(BrokerError::InvalidKey),
            _ => Ok(self),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrokerStatus {
    Ready,
    PausedForFocus,
}

#[derive(Debug, Error, PartialEq)]
pub enum BrokerError {
    #[error("input refused because the exact Roblox PID/HWND is not foreground")]
    WrongForeground,
    #[error("client-relative coordinates must be finite values between zero and one")]
    InvalidCoordinates,
    #[error("the key value is outside the supported range")]
    InvalidKey,
    #[error("native input backend failed: {0}")]
    Backend(String),
    #[error("one or more injected inputs could not be released: {0}")]
    ReleaseFailed(String),
}

pub trait InputSink: Send {
    fn foreground_target(&self) -> Result<Option<SessionTarget>, BrokerError>;

    fn send(&mut self, target: SessionTarget, action: InputAction) -> Result<(), BrokerError>;
}

/// The only path through which automation input may be emitted.
///
/// Every state-changing input checks both the foreground PID and HWND. On a
/// mismatch, any keys/buttons previously injected by this broker are released
/// before the broker enters a paused state.
pub struct InputBroker<S: InputSink> {
    target: SessionTarget,
    sink: S,
    pressed_keys: BTreeSet<Key>,
    pressed_buttons: BTreeSet<MouseButton>,
    status: BrokerStatus,
}

impl<S: InputSink> InputBroker<S> {
    pub fn new(target: SessionTarget, sink: S) -> Self {
        Self {
            target,
            sink,
            pressed_keys: BTreeSet::new(),
            pressed_buttons: BTreeSet::new(),
            status: BrokerStatus::Ready,
        }
    }

    pub const fn status(&self) -> BrokerStatus {
        self.status
    }

    pub fn is_clean(&self) -> bool {
        self.pressed_keys.is_empty() && self.pressed_buttons.is_empty()
    }

    pub fn sink(&self) -> &S {
        &self.sink
    }

    pub fn sink_mut(&mut self) -> &mut S {
        &mut self.sink
    }

    pub fn resume_after_focus_check(&mut self) -> Result<(), BrokerError> {
        self.ensure_foreground()?;
        self.status = BrokerStatus::Ready;
        Ok(())
    }

    pub fn dispatch(&mut self, action: InputAction) -> Result<(), BrokerError> {
        let action = action.validate()?;
        if let Err(error) = self.ensure_foreground() {
            let _ = self.release_all();
            self.status = BrokerStatus::PausedForFocus;
            return Err(error);
        }
        if self.status == BrokerStatus::PausedForFocus {
            return Err(BrokerError::WrongForeground);
        }

        if let Err(error) = self.sink.send(self.target, action) {
            let _ = self.release_all();
            self.status = BrokerStatus::PausedForFocus;
            return Err(error);
        }
        match action {
            InputAction::KeyDown { key } => {
                self.pressed_keys.insert(key);
            }
            InputAction::KeyUp { key } => {
                self.pressed_keys.remove(&key);
            }
            InputAction::MouseDown { button } => {
                self.pressed_buttons.insert(button);
            }
            InputAction::MouseUp { button } => {
                self.pressed_buttons.remove(&button);
            }
            InputAction::MouseMoveClient { .. } | InputAction::MouseWheel { .. } => {}
        }
        Ok(())
    }

    pub fn release_all(&mut self) -> Result<(), BrokerError> {
        let keys = std::mem::take(&mut self.pressed_keys);
        let buttons = std::mem::take(&mut self.pressed_buttons);
        let mut failures = Vec::new();

        for key in keys {
            if let Err(error) = self.sink.send(self.target, InputAction::KeyUp { key }) {
                failures.push(error.to_string());
            }
        }
        for button in buttons {
            if let Err(error) = self.sink.send(self.target, InputAction::MouseUp { button }) {
                failures.push(error.to_string());
            }
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(BrokerError::ReleaseFailed(failures.join("; ")))
        }
    }

    pub fn cancel(&mut self) -> Result<(), BrokerError> {
        self.status = BrokerStatus::PausedForFocus;
        self.release_all()
    }

    fn ensure_foreground(&self) -> Result<(), BrokerError> {
        if self.sink.foreground_target()? == Some(self.target) {
            Ok(())
        } else {
            Err(BrokerError::WrongForeground)
        }
    }
}

impl<S: InputSink> Drop for InputBroker<S> {
    fn drop(&mut self) {
        let _ = self.release_all();
    }
}

#[derive(Clone, Debug)]
pub struct MockInputSink {
    foreground: Option<SessionTarget>,
    actions: Vec<InputAction>,
    fail_sends: bool,
}

impl MockInputSink {
    #[must_use]
    pub fn new(foreground: Option<SessionTarget>) -> Self {
        Self {
            foreground,
            actions: Vec::new(),
            fail_sends: false,
        }
    }

    pub fn set_foreground(&mut self, foreground: Option<SessionTarget>) {
        self.foreground = foreground;
    }

    pub fn set_fail_sends(&mut self, fail_sends: bool) {
        self.fail_sends = fail_sends;
    }

    #[must_use]
    pub fn actions(&self) -> &[InputAction] {
        &self.actions
    }
}

impl InputSink for MockInputSink {
    fn foreground_target(&self) -> Result<Option<SessionTarget>, BrokerError> {
        Ok(self.foreground)
    }

    fn send(&mut self, _target: SessionTarget, action: InputAction) -> Result<(), BrokerError> {
        if self.fail_sends {
            Err(BrokerError::Backend("injected mock failure".to_owned()))
        } else {
            self.actions.push(action);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{ProcessId, WindowHandle};

    fn target(pid: u32, hwnd: u64) -> SessionTarget {
        SessionTarget {
            pid: ProcessId::new(pid).unwrap(),
            window: WindowHandle::new(hwnd).unwrap(),
        }
    }

    #[test]
    fn refuses_wrong_foreground_and_releases_held_inputs() {
        let roblox = target(41, 100);
        let mut broker = InputBroker::new(roblox, MockInputSink::new(Some(roblox)));
        broker
            .dispatch(InputAction::KeyDown { key: Key::Forward })
            .unwrap();
        broker.sink_mut().set_foreground(Some(target(99, 500)));

        let error = broker
            .dispatch(InputAction::KeyDown { key: Key::Left })
            .unwrap_err();

        assert_eq!(error, BrokerError::WrongForeground);
        assert_eq!(broker.status(), BrokerStatus::PausedForFocus);
        assert!(broker.is_clean());
        assert_eq!(
            broker.sink().actions(),
            &[
                InputAction::KeyDown { key: Key::Forward },
                InputAction::KeyUp { key: Key::Forward }
            ]
        );
    }

    #[test]
    fn cancel_releases_every_key_and_button() {
        let roblox = target(41, 100);
        let mut broker = InputBroker::new(roblox, MockInputSink::new(Some(roblox)));
        broker
            .dispatch(InputAction::KeyDown { key: Key::Forward })
            .unwrap();
        broker
            .dispatch(InputAction::KeyDown { key: Key::Jump })
            .unwrap();
        broker
            .dispatch(InputAction::MouseDown {
                button: MouseButton::Left,
            })
            .unwrap();

        broker.cancel().unwrap();

        assert!(broker.is_clean());
        assert!(
            broker
                .sink()
                .actions()
                .contains(&InputAction::KeyUp { key: Key::Forward })
        );
        assert!(
            broker
                .sink()
                .actions()
                .contains(&InputAction::KeyUp { key: Key::Jump })
        );
        assert!(broker.sink().actions().contains(&InputAction::MouseUp {
            button: MouseButton::Left
        }));
    }
}
