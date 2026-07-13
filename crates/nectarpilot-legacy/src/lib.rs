//! Opt-in compatibility for legacy `AutoHotkey` content.
//!
//! Import and execution are intentionally separate. [`inspect_script`] only
//! reads, hashes, and attempts to convert a script. The only execution entry
//! point requires fresh hash-bound [`LegacyConsent`].

#![deny(unsafe_code)]

mod bulk;
mod converter;
mod detectors;
mod harness;
mod runner;
mod support;

pub use bulk::{
    AssetCatalog, AssetCatalogEntry, AssetKind, AssetStatus, BulkConversionError, IssueCounts,
    generate_asset_catalog,
};
pub use converter::{
    ConversionIssue, ConversionReport, IssueKind, PatternStep, SafePattern,
    convert_movement_pattern,
};
pub use detectors::{
    DetectorAssetKind, DetectorCatalog, DetectorCatalogEntry, DetectorCatalogError,
    DetectorValidationReport, TemplateReference, extract_inline_template,
    generate_detector_catalog, scan_template_references, validate_detector_templates,
    validate_inline_templates, write_detector_catalog,
};
pub use harness::{
    FragmentKind, HarnessError, HarnessSettings, MoveMethod, PatternSettings, PatternSize,
    WATCHDOG_EXIT_CODE, generate_reset_script, generate_walk_script,
};
pub use runner::{
    ExecutionOutcome, ExecutionRequest, LegacyConsent, LegacyError, LegacyImportReport,
    LegacyRunner, RunnerPolicy, ScriptTrust, inspect_script,
};
pub use support::{
    SUPPORT_FILES, SupportCatalog, SupportCatalogEntry, SupportCatalogError,
    generate_support_catalog, verify_support_files, write_support_catalog,
};
