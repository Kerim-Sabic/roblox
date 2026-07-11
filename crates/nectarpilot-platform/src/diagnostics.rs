use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use image::{DynamicImage, GenericImageView, ImageFormat};
use regex::Regex;
use serde::{Deserialize, Serialize};
use thiserror::Error;

static CAPTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureScope {
    CroppedRegion,
    FullScreen,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CropRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct RetentionPolicy {
    pub max_age: Duration,
    pub max_bytes: u64,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            max_age: Duration::from_secs(14 * 24 * 60 * 60),
            max_bytes: 250 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvidenceRecord {
    pub path: PathBuf,
    pub scope: CaptureScope,
    pub created_at: DateTime<Utc>,
    pub bytes: u64,
}

#[derive(Debug, Error)]
pub enum DiagnosticError {
    #[error("crop is empty or extends outside the source image")]
    InvalidCrop,
    #[error("diagnostic filesystem operation failed: {0}")]
    Io(#[from] io::Error),
    #[error("diagnostic image operation failed: {0}")]
    Image(#[from] image::ImageError),
}

pub struct EvidenceStore {
    root: PathBuf,
    retention: RetentionPolicy,
}

impl EvidenceStore {
    pub fn new(root: impl Into<PathBuf>, retention: RetentionPolicy) -> Self {
        Self {
            root: root.into(),
            retention,
        }
    }

    pub fn store_crop(
        &self,
        image: &DynamicImage,
        region: CropRegion,
        label: &str,
    ) -> Result<EvidenceRecord, DiagnosticError> {
        let (source_width, source_height) = image.dimensions();
        let right = region.x.checked_add(region.width);
        let bottom = region.y.checked_add(region.height);
        if region.width == 0
            || region.height == 0
            || right.is_none_or(|value| value > source_width)
            || bottom.is_none_or(|value| value > source_height)
        {
            return Err(DiagnosticError::InvalidCrop);
        }
        fs::create_dir_all(&self.root)?;
        let now = Utc::now();
        let sequence = CAPTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let safe_label = sanitize_label(label);
        let path = self.root.join(format!(
            "{}-{sequence}-{safe_label}-crop.png",
            now.format("%Y%m%dT%H%M%S%.3fZ")
        ));
        image
            .crop_imm(region.x, region.y, region.width, region.height)
            .save_with_format(&path, ImageFormat::Png)?;
        let bytes = fs::metadata(&path)?.len();
        self.prune()?;
        Ok(EvidenceRecord {
            path,
            scope: CaptureScope::CroppedRegion,
            created_at: now,
            bytes,
        })
    }

    /// Removes expired captures first, then oldest captures until the byte cap
    /// is met. Only PNG files directly inside the evidence directory are touched.
    pub fn prune(&self) -> Result<(), DiagnosticError> {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return Ok(());
        };
        let now = SystemTime::now();
        let mut retained = Vec::new();
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("png") {
                continue;
            }
            let metadata = entry.metadata()?;
            if !metadata.is_file() {
                continue;
            }
            let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if now.duration_since(modified).unwrap_or_default() > self.retention.max_age {
                fs::remove_file(path)?;
            } else {
                retained.push((path, modified, metadata.len()));
            }
        }
        retained.sort_by_key(|(_, modified, _)| *modified);
        let mut total = retained.iter().map(|(_, _, bytes)| bytes).sum::<u64>();
        for (path, _, bytes) in retained {
            if total <= self.retention.max_bytes {
                break;
            }
            fs::remove_file(path)?;
            total = total.saturating_sub(bytes);
        }
        Ok(())
    }
}

pub fn exportable_evidence(
    records: impl IntoIterator<Item = EvidenceRecord>,
    approve_full_screen: bool,
) -> Vec<EvidenceRecord> {
    records
        .into_iter()
        .filter(|record| approve_full_screen || record.scope == CaptureScope::CroppedRegion)
        .collect()
}

/// Redacts links, common secret assignments, webhook URLs, and long external IDs
/// before logs are placed into a user-created support bundle.
#[must_use]
pub fn redact_sensitive_text(input: &str) -> String {
    let url = Regex::new(r#"(?i)https?://[^\s\"']+"#).expect("static URL regex is valid");
    let secret =
        Regex::new(r"(?i)\b(token|secret|webhook|private[_ -]?server)(\s*[:=]\s*)[^\s,;]+")
            .expect("static secret regex is valid");
    let external_id = Regex::new(r"\b\d{17,}\b").expect("static ID regex is valid");
    let without_urls = url.replace_all(input, "[redacted-url]");
    let without_secrets = secret.replace_all(&without_urls, "$1$2[redacted]");
    external_id
        .replace_all(&without_secrets, "[redacted-id]")
        .into_owned()
}

fn sanitize_label(label: &str) -> String {
    let cleaned = label
        .chars()
        .filter_map(|character| {
            if character.is_ascii_alphanumeric() {
                Some(character.to_ascii_lowercase())
            } else if character == '-' || character == '_' {
                Some(character)
            } else {
                None
            }
        })
        .take(48)
        .collect::<String>();
    if cleaned.is_empty() {
        "evidence".to_owned()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_never_stores_the_full_source() {
        let directory = tempfile::tempdir().unwrap();
        let store = EvidenceStore::new(directory.path(), RetentionPolicy::default());
        let source = DynamicImage::new_rgb8(200, 100);

        let record = store
            .store_crop(
                &source,
                CropRegion {
                    x: 10,
                    y: 10,
                    width: 50,
                    height: 20,
                },
                "detector/failure",
            )
            .unwrap();

        assert_eq!(record.scope, CaptureScope::CroppedRegion);
        assert_eq!(image::open(record.path).unwrap().dimensions(), (50, 20));
    }

    #[test]
    fn support_text_is_redacted() {
        let value = "webhook=https://discord.com/api/webhooks/secret user 123456789012345678";
        let redacted = redact_sensitive_text(value);
        assert!(!redacted.contains("discord.com"));
        assert!(!redacted.contains("123456789012345678"));
    }

    #[test]
    fn full_screen_evidence_needs_separate_approval() {
        let records = [
            EvidenceRecord {
                path: PathBuf::from("crop.png"),
                scope: CaptureScope::CroppedRegion,
                created_at: Utc::now(),
                bytes: 1,
            },
            EvidenceRecord {
                path: PathBuf::from("screen.png"),
                scope: CaptureScope::FullScreen,
                created_at: Utc::now(),
                bytes: 1,
            },
        ];
        assert_eq!(exportable_evidence(records, false).len(), 1);
    }
}
