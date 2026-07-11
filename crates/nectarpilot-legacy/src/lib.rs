//! Opt-in compatibility for legacy `AutoHotkey` content.
//!
//! Import and execution are intentionally separate. [`inspect_script`] only
//! reads, hashes, and attempts to convert a script. The only execution entry
//! point requires fresh hash-bound [`LegacyConsent`].

#![deny(unsafe_code)]

mod bulk;
mod converter;
mod runner;

pub use bulk::{
    AssetCatalog, AssetCatalogEntry, AssetKind, AssetStatus, BulkConversionError, IssueCounts,
    generate_asset_catalog,
};
pub use converter::{
    ConversionIssue, ConversionReport, IssueKind, PatternStep, SafePattern,
    convert_movement_pattern,
};
pub use runner::{
    ExecutionOutcome, ExecutionRequest, LegacyConsent, LegacyError, LegacyImportReport,
    LegacyRunner, RunnerPolicy, ScriptTrust, inspect_script,
};
