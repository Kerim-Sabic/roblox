//! Hash pinning for the legacy library files the generated harness includes.
//!
//! The walk harness `#Include`s a fixed set of imported Natro support files.
//! Those files execute with the same authority as the fragment, so they are
//! cataloged with digests exactly like `paths/` and `patterns/` assets and are
//! re-verified immediately before every run.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Every file the generated harness can include, relative to the legacy root.
/// Order is stable so the generated manifest is deterministic.
pub const SUPPORT_FILES: [&str; 9] = [
    "lib/Gdip_All.ahk",
    "lib/Gdip_ImageSearch.ahk",
    "lib/HyperSleep.ahk",
    "lib/Roblox.ahk",
    "lib/Walk.ahk",
    "nm_image_assets/convert/bitmaps.ahk",
    "nm_image_assets/general/bitmaps.ahk",
    "nm_image_assets/offset/bitmaps.ahk",
    "nm_image_assets/reset/bitmaps.ahk",
];

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SupportCatalogEntry {
    /// Forward-slash path relative to the legacy root.
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SupportCatalog {
    pub format_version: u16,
    pub entries: Vec<SupportCatalogEntry>,
}

#[derive(Debug, Error)]
pub enum SupportCatalogError {
    #[error("support file {path} could not be read: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("support catalog YAML generation failed: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error(
        "support file {path} does not match its pinned import (expected {expected}, got {actual})"
    )]
    DigestMismatch {
        path: String,
        expected: String,
        actual: String,
    },
    #[error("support catalog is missing pinned entry for {path}")]
    MissingEntry { path: String },
}

/// Hashes every harness support file under `legacy_root`.
pub fn generate_support_catalog(legacy_root: &Path) -> Result<SupportCatalog, SupportCatalogError> {
    let mut entries = Vec::with_capacity(SUPPORT_FILES.len());
    for relative in SUPPORT_FILES {
        let bytes = read_support(legacy_root, relative)?;
        entries.push(SupportCatalogEntry {
            path: relative.to_owned(),
            sha256: hex::encode(Sha256::digest(&bytes)),
            bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        });
    }
    Ok(SupportCatalog {
        format_version: 1,
        entries,
    })
}

/// Writes the manifest YAML for `generate_support_catalog`.
pub fn write_support_catalog(
    legacy_root: &Path,
    output: &Path,
) -> Result<SupportCatalog, SupportCatalogError> {
    let catalog = generate_support_catalog(legacy_root)?;
    fs::write(output, serde_yaml::to_string(&catalog)?).map_err(|source| {
        SupportCatalogError::Io {
            path: output.display().to_string(),
            source,
        }
    })?;
    Ok(catalog)
}

/// Confirms every pinned support file on disk still matches `catalog`.
/// Returns the first mismatch; a passing result means the harness includes
/// exactly the imported Natro v1.1.2 library code.
pub fn verify_support_files(
    legacy_root: &Path,
    catalog: &SupportCatalog,
) -> Result<(), SupportCatalogError> {
    for relative in SUPPORT_FILES {
        let entry = catalog
            .entries
            .iter()
            .find(|entry| entry.path == relative)
            .ok_or_else(|| SupportCatalogError::MissingEntry {
                path: relative.to_owned(),
            })?;
        let bytes = read_support(legacy_root, relative)?;
        let actual = hex::encode(Sha256::digest(&bytes));
        if actual != entry.sha256 || u64::try_from(bytes.len()).unwrap_or(u64::MAX) != entry.bytes {
            return Err(SupportCatalogError::DigestMismatch {
                path: relative.to_owned(),
                expected: entry.sha256.clone(),
                actual,
            });
        }
    }
    Ok(())
}

fn read_support(legacy_root: &Path, relative: &str) -> Result<Vec<u8>, SupportCatalogError> {
    let path = legacy_root.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
    fs::read(&path).map_err(|source| SupportCatalogError::Io {
        path: path.display().to_string(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    #[test]
    fn repository_support_files_hash_deterministically() {
        let catalog = generate_support_catalog(&repo_root()).expect("support files present");
        assert_eq!(catalog.entries.len(), SUPPORT_FILES.len());
        verify_support_files(&repo_root(), &catalog).expect("fresh catalog must verify");
    }

    #[test]
    fn tampered_support_file_is_detected() {
        let staged = tempfile::tempdir().unwrap();
        for relative in SUPPORT_FILES {
            let destination = staged.path().join(relative);
            fs::create_dir_all(destination.parent().unwrap()).unwrap();
            fs::copy(repo_root().join(relative), destination).unwrap();
        }
        let catalog = generate_support_catalog(staged.path()).unwrap();
        fs::write(staged.path().join("lib/Walk.ahk"), "ExitApp\n").unwrap();

        assert!(matches!(
            verify_support_files(staged.path(), &catalog),
            Err(SupportCatalogError::DigestMismatch { path, .. }) if path == "lib/Walk.ahk"
        ));
    }
}
