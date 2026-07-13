// Win32 has no safe Rust ABI. All unsafe blocks in this crate are narrow FFI
// adapters with local SAFETY arguments; callers receive safe abstractions.
#![allow(unsafe_code)]

//! Windows automation primitives with safety invariants enforced at the API boundary.
//!
//! Native calls are isolated in `windows_backend`. The portable types and mock
//! backends allow the daemon to exercise the same focus, ownership, and recovery
//! rules in tests on every operating system.

pub mod capture;
pub mod diagnostics;
pub mod emergency;
pub mod freeze;
pub mod hotkeys;
pub mod input;
pub mod job;
pub mod perception;
pub mod pipe;
pub mod process;
pub mod secrets;
pub mod session;
pub mod task_executor;

#[cfg(windows)]
pub mod windows_backend;

pub use capture::{
    CaptureError, ClientCapture, ClientFrame, NormalizedCrop, PixelRegion, WindowsClientCapture,
    normalized_to_pixels,
};
pub use hotkeys::{HotkeyAction, HotkeyChord, parse_hotkey};
pub use perception::{
    ConsensusPolicy, ConstrainedOcr, HoneyCounterReader, LivePerceptionPipeline,
    MultiScaleTemplateMatcher, OcrError, OcrRead, OcrRequest, PerceptionError, QuestBarState,
    QuestTitleDetector, ScienceBearQuestDetector, Template, TemplateBinding, TemplateDetector,
    TemplateMatch, TemplateMatcherConfig, TemporalConsensus, WindowsOcr, preprocess_for_ocr,
    quest_giver_bindings, read_quest_bars, template_from_png_bytes,
};
pub use session::{ProcessId, RobloxSession, SessionTarget, WindowHandle};

#[cfg(windows)]
pub use windows_backend::{
    DiscoveredRobloxClient, WindowsHotkeySet, discover_roblox_clients, tap_global_virtual_key,
};
