use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::converter::{ConversionIssue, IssueKind, convert_movement_pattern};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    Route,
    Pattern,
}

impl AssetKind {
    const fn source_directory(self) -> &'static str {
        match self {
            Self::Route => "paths",
            Self::Pattern => "patterns",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetStatus {
    SafeDsl,
    LegacyBridgeRequired,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct IssueCounts {
    pub unsupported_syntax: usize,
    pub unsafe_capabilities: usize,
    pub invalid_values: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AssetCatalogEntry {
    pub legacy_source: String,
    pub sha256: String,
    pub bytes: u64,
    pub status: AssetStatus,
    pub generated_asset: Option<String>,
    pub requires_explicit_consent: bool,
    pub issue_counts: IssueCounts,
    /// A bounded deterministic preview; counts above remain authoritative.
    pub issue_samples: Vec<ConversionIssue>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AssetCatalog {
    pub format_version: u16,
    pub kind: AssetKind,
    pub source_directory: String,
    pub total_files: usize,
    pub safe_dsl_files: usize,
    pub legacy_bridge_files: usize,
    pub entries: Vec<AssetCatalogEntry>,
}

#[derive(Debug, Error)]
pub enum BulkConversionError {
    #[error("legacy asset filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("legacy asset YAML generation failed: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("legacy asset {path} is not valid UTF-8")]
    InvalidText { path: PathBuf },
    #[error("two converted assets resolve to the same output name: {0}")]
    OutputCollision(String),
}

/// Hashes and classifies every `.ahk` file in one legacy asset directory.
/// Safe YAML and the catalog are written, but no imported script is executed.
pub fn generate_asset_catalog(
    legacy_root: &Path,
    output_directory: &Path,
    kind: AssetKind,
) -> Result<AssetCatalog, BulkConversionError> {
    let source_name = kind.source_directory();
    let source_directory = legacy_root.join(source_name);
    let mut paths = Vec::new();
    for entry in fs::read_dir(&source_directory)? {
        let path = entry?.path();
        if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("ahk"))
        {
            paths.push(path);
        }
    }
    paths.sort_by(|left, right| {
        left.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase()
            .cmp(
                &right
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_ascii_lowercase(),
            )
    });

    fs::create_dir_all(output_directory)?;
    let mut output_names = HashSet::new();
    let mut entries = Vec::with_capacity(paths.len());
    for path in paths {
        let bytes = fs::read(&path)?;
        let source = String::from_utf8(bytes.clone())
            .map_err(|_| BulkConversionError::InvalidText { path: path.clone() })?;
        let file_name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let logical_source = format!("{source_name}/{file_name}");
        let stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("imported-asset");
        let report = convert_movement_pattern(stem, &source);
        let counts = count_issues(&report.issues);
        let generated_asset = if let Some(pattern) = report.converted {
            let output_name = format!("{}.nectar.yaml", pattern.name);
            if !output_names.insert(output_name.to_ascii_lowercase()) {
                return Err(BulkConversionError::OutputCollision(output_name));
            }
            fs::write(output_directory.join(&output_name), pattern.to_yaml()?)?;
            Some(output_name)
        } else {
            None
        };
        let status = if generated_asset.is_some() {
            AssetStatus::SafeDsl
        } else {
            AssetStatus::LegacyBridgeRequired
        };
        entries.push(AssetCatalogEntry {
            legacy_source: logical_source,
            sha256: hex::encode(Sha256::digest(&bytes)),
            bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            status,
            generated_asset,
            requires_explicit_consent: status == AssetStatus::LegacyBridgeRequired,
            issue_counts: counts,
            issue_samples: report.issues.into_iter().take(8).collect(),
        });
    }
    let safe_dsl_files = entries
        .iter()
        .filter(|entry| entry.status == AssetStatus::SafeDsl)
        .count();
    let catalog = AssetCatalog {
        format_version: 1,
        kind,
        source_directory: source_name.to_owned(),
        total_files: entries.len(),
        safe_dsl_files,
        legacy_bridge_files: entries.len().saturating_sub(safe_dsl_files),
        entries,
    };
    fs::write(
        output_directory.join("_legacy-manifest.yaml"),
        serde_yaml::to_string(&catalog)?,
    )?;
    Ok(catalog)
}

fn count_issues(issues: &[ConversionIssue]) -> IssueCounts {
    let mut counts = IssueCounts::default();
    for issue in issues {
        match issue.kind {
            IssueKind::UnsupportedSyntax => counts.unsupported_syntax += 1,
            IssueKind::UnsafeCapability => counts.unsafe_capabilities += 1,
            IssueKind::InvalidValue => counts.invalid_values += 1,
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogs_every_file_without_executing_unsupported_content() {
        let root = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("patterns")).unwrap();
        fs::write(root.path().join("patterns/safe.ahk"), "Sleep 25\n").unwrap();
        fs::write(
            root.path().join("patterns/dynamic.ahk"),
            "Run(\"untrusted.exe\")\n",
        )
        .unwrap();

        let catalog =
            generate_asset_catalog(root.path(), output.path(), AssetKind::Pattern).unwrap();

        assert_eq!(catalog.total_files, 2);
        assert_eq!(catalog.safe_dsl_files, 1);
        assert_eq!(catalog.legacy_bridge_files, 1);
        assert!(output.path().join("safe.nectar.yaml").is_file());
        assert!(output.path().join("_legacy-manifest.yaml").is_file());
        let dynamic = catalog
            .entries
            .iter()
            .find(|entry| entry.legacy_source.ends_with("dynamic.ahk"))
            .unwrap();
        assert!(dynamic.requires_explicit_consent);
        assert_eq!(dynamic.issue_counts.unsafe_capabilities, 1);
    }
}
