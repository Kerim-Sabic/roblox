//! Validation of every imported in-game detector template.
//!
//! Natro's detectors come in two forms: image files under `nm_image_assets/`
//! that `nm_imgSearch` loads by name at runtime, and inline base64 PNG
//! templates created with `Gdip_BitmapFromBase64` (buff icons, reset markers,
//! haste digits, and menu anchors). A missing or corrupt template silently
//! degrades the legacy macro at runtime, so this module catalogs all of them
//! with digests, decodes every image, and cross-references every template the
//! scripts mention against the files that actually exist.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Directories whose `.ahk` sources are scanned for template references.
const REFERENCE_SOURCE_DIRECTORIES: [&str; 4] = ["submacros", "lib", "paths", "patterns"];

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DetectorAssetKind {
    /// A decodable raster template with verified dimensions.
    Image { width: u32, height: u32 },
    /// An include script defining inline base64 PNG templates.
    BitmapScript { inline_templates: usize },
    /// A non-template auxiliary file (icons, msstyles themes).
    Auxiliary,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DetectorCatalogEntry {
    /// Forward-slash path relative to `nm_image_assets/`.
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
    #[serde(flatten)]
    pub kind: DetectorAssetKind,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DetectorCatalog {
    pub format_version: u16,
    pub total_files: usize,
    pub image_files: usize,
    pub inline_templates: usize,
    pub entries: Vec<DetectorCatalogEntry>,
}

/// One template mention found in legacy script source.
#[derive(Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub enum TemplateReference {
    /// `nm_imgSearch("mantis.png", ...)`-style complete file name.
    Literal(String),
    /// `nm_imgSearch("brown_bear" i ".png", ...)`-style computed name.
    Prefix(String),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DetectorValidationReport {
    pub cataloged_files: usize,
    pub decoded_images: usize,
    pub inline_templates: usize,
    pub references: usize,
    /// Script references that resolve to no cataloged image.
    pub missing: Vec<String>,
    /// Cataloged images or inline payloads that failed to decode.
    pub corrupt: Vec<String>,
}

impl DetectorValidationReport {
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.missing.is_empty() && self.corrupt.is_empty()
    }
}

#[derive(Debug, Error)]
pub enum DetectorCatalogError {
    #[error("detector asset filesystem operation failed for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("detector catalog YAML generation failed: {0}")]
    Yaml(#[from] serde_yaml::Error),
}

/// Walks `nm_image_assets/` and catalogs every file with digest and, for
/// images and bitmap scripts, decode-time metadata. Corrupt entries are still
/// cataloged (as `Auxiliary`) so `validate_detector_templates` can name them.
pub fn generate_detector_catalog(
    legacy_root: &Path,
) -> Result<(DetectorCatalog, Vec<String>), DetectorCatalogError> {
    let assets_root = legacy_root.join("nm_image_assets");
    let mut files = Vec::new();
    collect_files(&assets_root, &mut files)?;
    files.sort_by_key(|path| relative_forward_slash(&assets_root, path).to_ascii_lowercase());

    let mut entries = Vec::with_capacity(files.len());
    let mut corrupt = Vec::new();
    let mut image_files = 0_usize;
    let mut inline_templates = 0_usize;
    for path in &files {
        let relative = relative_forward_slash(&assets_root, path);
        let bytes = fs::read(path).map_err(|source| DetectorCatalogError::Io {
            path: path.clone(),
            source,
        })?;
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        let kind = match extension.as_str() {
            "png" | "jpg" | "jpeg" => match image::load_from_memory(&bytes) {
                Ok(decoded) => {
                    image_files += 1;
                    DetectorAssetKind::Image {
                        width: decoded.width(),
                        height: decoded.height(),
                    }
                }
                Err(error) => {
                    corrupt.push(format!("{relative}: {error}"));
                    DetectorAssetKind::Auxiliary
                }
            },
            "ahk" => {
                let source = String::from_utf8_lossy(&bytes);
                let (count, mut errors) = validate_inline_templates(&relative, &source);
                inline_templates += count;
                corrupt.append(&mut errors);
                DetectorAssetKind::BitmapScript {
                    inline_templates: count,
                }
            }
            _ => DetectorAssetKind::Auxiliary,
        };
        entries.push(DetectorCatalogEntry {
            path: relative,
            sha256: hex::encode(Sha256::digest(&bytes)),
            bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            kind,
        });
    }

    Ok((
        DetectorCatalog {
            format_version: 1,
            total_files: entries.len(),
            image_files,
            inline_templates,
            entries,
        },
        corrupt,
    ))
}

/// Writes the manifest YAML for `generate_detector_catalog`.
pub fn write_detector_catalog(
    legacy_root: &Path,
    output: &Path,
) -> Result<DetectorCatalog, DetectorCatalogError> {
    let (catalog, _) = generate_detector_catalog(legacy_root)?;
    fs::write(output, serde_yaml::to_string(&catalog)?).map_err(|source| {
        DetectorCatalogError::Io {
            path: output.to_path_buf(),
            source,
        }
    })?;
    Ok(catalog)
}

/// Extracts every template mention from the legacy scripts:
/// `nm_imgSearch` file names (literal and computed prefixes) plus direct
/// `nm_image_assets\...` path strings.
pub fn scan_template_references(
    legacy_root: &Path,
) -> Result<BTreeSet<TemplateReference>, DetectorCatalogError> {
    let img_search =
        Regex::new(r#"(?i)nm_imgSearch\(\s*"([^"]+)""#).expect("static imgSearch regex is valid");
    let direct = Regex::new(r"(?i)nm_image_assets\\([A-Za-z0-9_ .\\-]+\.(?:png|jpg|jpeg))")
        .expect("static direct-path regex is valid");

    let mut sources = Vec::new();
    for directory in REFERENCE_SOURCE_DIRECTORIES {
        collect_files(&legacy_root.join(directory), &mut sources)?;
    }
    sources.retain(|path| {
        path.extension()
            .and_then(|value| value.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("ahk"))
    });

    let mut references = BTreeSet::new();
    for path in sources {
        let source = fs::read_to_string(&path).map_err(|error| DetectorCatalogError::Io {
            path: path.clone(),
            source: error,
        })?;
        for captures in img_search.captures_iter(&source) {
            let name = captures[1].trim().to_owned();
            if name.is_empty() {
                continue;
            }
            let lower = name.to_ascii_lowercase();
            #[allow(
                clippy::case_sensitive_file_extension_comparisons,
                reason = "the value is lowercased on the previous line"
            )]
            if lower.ends_with(".png") || lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
                references.insert(TemplateReference::Literal(name.replace('\\', "/")));
            } else {
                references.insert(TemplateReference::Prefix(name.replace('\\', "/")));
            }
        }
        for captures in direct.captures_iter(&source) {
            references.insert(TemplateReference::Literal(captures[1].replace('\\', "/")));
        }
    }
    Ok(references)
}

/// Full detector validation: catalog `nm_image_assets`, decode every image and
/// inline template, and confirm every script reference resolves to a real
/// cataloged image (Windows file names are case-insensitive, so matching is
/// case-insensitive too).
pub fn validate_detector_templates(
    legacy_root: &Path,
) -> Result<DetectorValidationReport, DetectorCatalogError> {
    let (catalog, corrupt) = generate_detector_catalog(legacy_root)?;
    let references = scan_template_references(legacy_root)?;

    let image_paths: Vec<String> = catalog
        .entries
        .iter()
        .filter(|entry| matches!(entry.kind, DetectorAssetKind::Image { .. }))
        .map(|entry| entry.path.to_ascii_lowercase())
        .collect();

    let mut missing = Vec::new();
    for reference in &references {
        let found = match reference {
            TemplateReference::Literal(name) => image_paths.contains(&name.to_ascii_lowercase()),
            TemplateReference::Prefix(prefix) => {
                let needle = prefix.to_ascii_lowercase();
                image_paths.iter().any(|path| path.starts_with(&needle))
            }
        };
        if !found {
            missing.push(match reference {
                TemplateReference::Literal(name) => format!("literal {name}"),
                TemplateReference::Prefix(prefix) => format!("prefix {prefix}"),
            });
        }
    }

    Ok(DetectorValidationReport {
        cataloged_files: catalog.total_files,
        decoded_images: catalog.image_files,
        inline_templates: catalog.inline_templates,
        references: references.len(),
        missing,
        corrupt,
    })
}

/// Extracts one named inline template (`bitmaps["<name>"] :=
/// Gdip_BitmapFromBase64("...")`) from a pinned legacy script and returns its
/// decoded image bytes. This lets native detectors reuse the exact templates
/// the legacy macro shipped, without duplicating binary assets.
#[must_use]
pub fn extract_inline_template(source: &str, name: &str) -> Option<Vec<u8>> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let pattern =
        format!(r#"bitmaps\["{name}"\]\s*:=\s*Gdip_BitmapFromBase64\(\s*"([A-Za-z0-9+/=]+)"\s*\)"#);
    let regex = Regex::new(&pattern).ok()?;
    let payload = regex.captures(source)?.get(1)?.as_str();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .ok()?;
    image::load_from_memory(&bytes).ok()?;
    Some(bytes)
}

/// Decodes every `Gdip_BitmapFromBase64("...")` payload in one script and
/// reports the count plus any payload that is not a valid PNG/JPEG image.
#[must_use]
pub fn validate_inline_templates(label: &str, source: &str) -> (usize, Vec<String>) {
    let payloads = Regex::new(r#"Gdip_BitmapFromBase64\(\s*"([A-Za-z0-9+/=]+)"\s*\)"#)
        .expect("static base64 payload regex is valid");
    let mut count = 0_usize;
    let mut errors = Vec::new();
    for captures in payloads.captures_iter(source) {
        count += 1;
        match base64::engine::general_purpose::STANDARD.decode(&captures[1]) {
            Ok(bytes) => {
                if let Err(error) = image::load_from_memory(&bytes) {
                    errors.push(format!(
                        "{label}: inline template {count} is corrupt: {error}"
                    ));
                }
            }
            Err(error) => {
                errors.push(format!(
                    "{label}: inline template {count} has invalid base64: {error}"
                ));
            }
        }
    }
    (count, errors)
}

fn collect_files(directory: &Path, into: &mut Vec<PathBuf>) -> Result<(), DetectorCatalogError> {
    let reader = fs::read_dir(directory).map_err(|source| DetectorCatalogError::Io {
        path: directory.to_path_buf(),
        source,
    })?;
    for entry in reader {
        let entry = entry.map_err(|source| DetectorCatalogError::Io {
            path: directory.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, into)?;
        } else {
            into.push(path);
        }
    }
    Ok(())
}

fn relative_forward_slash(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    #[test]
    fn every_referenced_in_game_template_exists_and_decodes() {
        let report = validate_detector_templates(&repo_root()).expect("scan repository");

        assert!(report.cataloged_files >= 170, "expected the full import");
        assert!(report.decoded_images >= 100, "PNG templates must decode");
        assert!(
            report.inline_templates >= 800,
            "inline base64 templates must be discovered"
        );
        assert!(report.references >= 40, "script references must be found");
        assert!(
            report.is_clean(),
            "missing: {:?}\ncorrupt: {:?}",
            report.missing,
            report.corrupt
        );
    }

    #[test]
    fn inline_template_validation_flags_corrupt_payloads() {
        let good = "bitmaps[\"x\"] := Gdip_BitmapFromBase64(\"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==\")";
        let (count, errors) = validate_inline_templates("good.ahk", good);
        assert_eq!(count, 1);
        assert!(errors.is_empty(), "{errors:?}");

        let bad = "bitmaps[\"x\"] := Gdip_BitmapFromBase64(\"aGVsbG8gd29ybGQ=\")";
        let (count, errors) = validate_inline_templates("bad.ahk", bad);
        assert_eq!(count, 1);
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn missing_referenced_template_is_reported() {
        let staged = tempfile::tempdir().unwrap();
        fs::create_dir_all(staged.path().join("nm_image_assets")).unwrap();
        fs::create_dir_all(staged.path().join("submacros")).unwrap();
        for directory in ["lib", "paths", "patterns"] {
            fs::create_dir_all(staged.path().join(directory)).unwrap();
        }
        fs::write(
            staged.path().join("submacros/macro.ahk"),
            "ret := nm_imgSearch(\"ghost.png\", 30)\n",
        )
        .unwrap();

        let report = validate_detector_templates(staged.path()).unwrap();
        assert_eq!(report.missing, vec!["literal ghost.png".to_owned()]);
        assert!(!report.is_clean());
    }
}
