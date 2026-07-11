// Win32 has no safe Rust ABI. All unsafe blocks in this crate are narrow FFI
// adapters with local SAFETY arguments; callers receive safe abstractions.
#![allow(unsafe_code)]

//! Windows automation primitives with safety invariants enforced at the API boundary.
//!
//! Native calls are isolated in `windows_backend`. The portable types and mock
//! backends allow the daemon to exercise the same focus, ownership, and recovery
//! rules in tests on every operating system.

pub mod diagnostics;
pub mod emergency;
pub mod freeze;
pub mod input;
pub mod job;
pub mod pipe;
pub mod process;
pub mod secrets;
pub mod session;

#[cfg(windows)]
pub mod windows_backend;

pub use session::{ProcessId, SessionTarget, WindowHandle};
