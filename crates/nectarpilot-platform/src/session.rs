use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A non-zero operating-system process identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProcessId(NonZeroU32);

impl ProcessId {
    #[must_use]
    pub fn new(pid: u32) -> Option<Self> {
        NonZeroU32::new(pid).map(Self)
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

/// An opaque native window handle. `0` is never accepted.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WindowHandle(u64);

impl WindowHandle {
    #[must_use]
    pub const fn new(raw: u64) -> Option<Self> {
        if raw == 0 { None } else { Some(Self(raw)) }
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionTarget {
    pub pid: ProcessId,
    pub window: WindowHandle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    #[must_use]
    pub fn contains(self, point: Point) -> bool {
        let right = i64::from(self.left) + i64::from(self.width);
        let bottom = i64::from(self.top) + i64::from(self.height);
        i64::from(point.x) >= i64::from(self.left)
            && i64::from(point.x) < right
            && i64::from(point.y) >= i64::from(self.top)
            && i64::from(point.y) < bottom
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WindowGeometry {
    pub outer: Rect,
    pub client: Rect,
    pub monitor: Rect,
    pub dpi: u32,
    pub minimized: bool,
    pub fullscreen: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WindowSnapshot {
    pub target: SessionTarget,
    pub geometry: WindowGeometry,
    pub is_foreground: bool,
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("no usable top-level window belongs to process {0}")]
    WindowNotFound(u32),
    #[error("the window is no longer owned by the attached process")]
    OwnershipChanged,
    #[error("platform session query failed: {0}")]
    Platform(String),
}

pub trait SessionProbe: Send + Sync {
    fn find_main_window(&self, pid: ProcessId) -> Result<WindowSnapshot, SessionError>;

    fn snapshot(&self, target: SessionTarget) -> Result<WindowSnapshot, SessionError>;

    fn foreground_target(&self) -> Result<Option<SessionTarget>, SessionError>;
}

/// The single authoritative association between an adopted Roblox PID and HWND.
#[derive(Clone, Debug)]
pub struct RobloxSession {
    target: SessionTarget,
    geometry: WindowGeometry,
    foreground: bool,
    geometry_revision: u64,
}

impl RobloxSession {
    pub fn attach(probe: &dyn SessionProbe, pid: ProcessId) -> Result<Self, SessionError> {
        let snapshot = probe.find_main_window(pid)?;
        if snapshot.target.pid != pid {
            return Err(SessionError::OwnershipChanged);
        }
        Ok(Self::from_snapshot(snapshot))
    }

    #[must_use]
    pub fn from_snapshot(snapshot: WindowSnapshot) -> Self {
        Self {
            target: snapshot.target,
            geometry: snapshot.geometry,
            foreground: snapshot.is_foreground,
            geometry_revision: 0,
        }
    }

    #[must_use]
    pub const fn target(&self) -> SessionTarget {
        self.target
    }

    #[must_use]
    pub const fn geometry(&self) -> WindowGeometry {
        self.geometry
    }

    #[must_use]
    pub const fn is_foreground(&self) -> bool {
        self.foreground
    }

    #[must_use]
    pub const fn geometry_revision(&self) -> u64 {
        self.geometry_revision
    }

    /// Revalidates exact PID/HWND ownership and detects resize, move, DPI, or
    /// fullscreen changes so higher layers can invalidate calibration.
    pub fn refresh(&mut self, probe: &dyn SessionProbe) -> Result<bool, SessionError> {
        let snapshot = probe.snapshot(self.target)?;
        if snapshot.target != self.target {
            return Err(SessionError::OwnershipChanged);
        }
        let geometry_changed = self.geometry != snapshot.geometry;
        self.geometry = snapshot.geometry;
        self.foreground = snapshot.is_foreground;
        if geometry_changed {
            self.geometry_revision = self.geometry_revision.saturating_add(1);
        }
        Ok(geometry_changed)
    }
}

#[derive(Clone, Debug)]
pub struct MockSessionProbe {
    snapshot: std::sync::Arc<std::sync::Mutex<Result<WindowSnapshot, String>>>,
    foreground: std::sync::Arc<std::sync::Mutex<Option<SessionTarget>>>,
}

impl MockSessionProbe {
    #[must_use]
    pub fn new(snapshot: WindowSnapshot) -> Self {
        Self {
            snapshot: std::sync::Arc::new(std::sync::Mutex::new(Ok(snapshot))),
            foreground: std::sync::Arc::new(std::sync::Mutex::new(
                snapshot.is_foreground.then_some(snapshot.target),
            )),
        }
    }

    pub fn set_snapshot(&self, snapshot: WindowSnapshot) {
        *self.snapshot.lock().expect("mock snapshot lock poisoned") = Ok(snapshot);
    }

    pub fn set_foreground(&self, target: Option<SessionTarget>) {
        *self
            .foreground
            .lock()
            .expect("mock foreground lock poisoned") = target;
    }

    pub fn fail(&self, message: impl Into<String>) {
        *self.snapshot.lock().expect("mock snapshot lock poisoned") = Err(message.into());
    }

    fn current(&self) -> Result<WindowSnapshot, SessionError> {
        self.snapshot
            .lock()
            .expect("mock snapshot lock poisoned")
            .clone()
            .map_err(SessionError::Platform)
    }
}

impl SessionProbe for MockSessionProbe {
    fn find_main_window(&self, pid: ProcessId) -> Result<WindowSnapshot, SessionError> {
        let snapshot = self.current()?;
        if snapshot.target.pid == pid {
            Ok(snapshot)
        } else {
            Err(SessionError::WindowNotFound(pid.get()))
        }
    }

    fn snapshot(&self, target: SessionTarget) -> Result<WindowSnapshot, SessionError> {
        let snapshot = self.current()?;
        if snapshot.target == target {
            Ok(snapshot)
        } else {
            Err(SessionError::OwnershipChanged)
        }
    }

    fn foreground_target(&self) -> Result<Option<SessionTarget>, SessionError> {
        Ok(*self
            .foreground
            .lock()
            .expect("mock foreground lock poisoned"))
    }
}
