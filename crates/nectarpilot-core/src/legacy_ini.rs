use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use chrono::Utc;
use nectarpilot_contracts::{
    FieldRotation, LegacySnapshot, LegacySource, Profile, ValuableItemBudgets,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq)]
pub struct ImportedProfile {
    pub profile: Profile,
    pub report: LegacyImportReport,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LegacyImportReport {
    pub source_files: Vec<PathBuf>,
    pub mapped: Vec<LegacySettingRef>,
    pub unmapped: Vec<LegacySettingValue>,
    pub sensitive: Vec<LegacySettingRef>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LegacySettingRef {
    pub file_name: String,
    pub section: String,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacySettingValue {
    pub setting: LegacySettingRef,
    pub value: String,
}

/// Imports legacy configuration without modifying, renaming, or deleting any
/// source file. Every entry is either mapped, reported as unmapped, or marked
/// sensitive. Sensitive values remain only in the original source and must be
/// handed to the platform secret store by an explicit follow-up flow.
pub fn import_legacy_ini_files(
    paths: impl IntoIterator<Item = impl AsRef<Path>>,
    profile_name: impl Into<String>,
) -> Result<ImportedProfile, LegacyImportError> {
    let mut profile = Profile::new(profile_name);
    let mut report = LegacyImportReport::default();
    let mut sources = Vec::new();
    let mut parsed_sources = Vec::new();

    for path in paths {
        let path = path.as_ref();
        let bytes = fs::read(path)?;
        let text = String::from_utf8(bytes.clone()).map_err(|source| {
            LegacyImportError::InvalidEncoding {
                path: path.to_path_buf(),
                source,
            }
        })?;
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("legacy.ini")
            .to_owned();
        let (sections, warnings) = parse_ini(&text, &file_name);
        report.warnings.extend(warnings);
        report.source_files.push(path.to_path_buf());

        let mut safe_sections = sections.clone();
        for (section_name, values) in &mut safe_sections {
            for (key, value) in values {
                if is_sensitive_key(section_name, key) {
                    *value = "<redacted:requires-secret-import>".into();
                }
            }
        }
        sources.push(LegacySource {
            file_name: file_name.clone(),
            sha256: hex::encode(Sha256::digest(&bytes)),
            sections: safe_sections,
        });
        parsed_sources.push((file_name, sections));
    }

    let mut mapped = HashSet::<LegacySettingRef>::new();
    for (file_name, sections) in &parsed_sources {
        if file_name.eq_ignore_ascii_case("nm_config.ini") {
            map_main_config(file_name, sections, &mut profile, &mut mapped, &mut report);
        }
    }

    for (file_name, sections) in &parsed_sources {
        for (section, values) in sections {
            for (key, value) in values {
                let setting = LegacySettingRef {
                    file_name: file_name.clone(),
                    section: section.clone(),
                    key: key.clone(),
                };
                if is_sensitive_key(section, key) {
                    report.sensitive.push(setting);
                } else if mapped.contains(&setting) {
                    report.mapped.push(setting);
                } else {
                    report.unmapped.push(LegacySettingValue {
                        setting,
                        value: value.clone(),
                    });
                }
            }
        }
    }

    profile.updated_at = Utc::now();
    profile.legacy = Some(LegacySnapshot { sources });
    Ok(ImportedProfile { profile, report })
}

#[allow(clippy::too_many_lines)] // Keeps related legacy-key mappings auditable in one table-like function.
fn map_main_config(
    file_name: &str,
    sections: &BTreeMap<String, BTreeMap<String, String>>,
    profile: &mut Profile,
    mapped: &mut HashSet<LegacySettingRef>,
    report: &mut LegacyImportReport,
) {
    if let Some(settings) = find_section(sections, "Settings") {
        map_string(
            file_name,
            "Settings",
            settings,
            "StartHotkey",
            &mut profile.automation.hotkeys.start,
            mapped,
        );
        map_string(
            file_name,
            "Settings",
            settings,
            "PauseHotkey",
            &mut profile.automation.hotkeys.pause_resume,
            mapped,
        );
        map_string(
            file_name,
            "Settings",
            settings,
            "StopHotkey",
            &mut profile.automation.hotkeys.stop,
            mapped,
        );
    }

    if let Some(gather) = find_section(sections, "Gather") {
        let mut rotations = Vec::new();
        for slot in 1..=3 {
            let field_key = format!("FieldName{slot}");
            let Some((actual_field_key, field)) = find_value(gather, &field_key) else {
                continue;
            };
            if field.trim().is_empty() || field.eq_ignore_ascii_case("none") {
                continue;
            }
            mark(mapped, file_name, "Gather", actual_field_key);
            let pattern = take_or_default(
                gather,
                &format!("FieldPattern{slot}"),
                "Stationary",
                file_name,
                "Gather",
                mapped,
            );
            let minutes = parse_number(
                gather,
                &format!("FieldUntilMins{slot}"),
                10_u32,
                file_name,
                "Gather",
                mapped,
                report,
            );
            let repetitions = parse_number(
                gather,
                &format!("FieldPatternReps{slot}"),
                1_u16,
                file_name,
                "Gather",
                mapped,
                report,
            )
            .max(1);
            rotations.push(FieldRotation {
                field: field.clone(),
                pattern,
                gather_seconds: minutes.saturating_mul(60),
                repetitions,
            });
        }
        profile.automation.gathering_enabled = !rotations.is_empty();
        profile.automation.rotations = rotations;
    }

    profile.automation.features.collections = section_has_enabled_value(sections, "Collect");
    profile.automation.features.quests = section_has_enabled_value(sections, "Quests");
    profile.automation.features.planters = section_has_enabled_value(sections, "Planters");
    profile.automation.features.boosts = section_has_enabled_value(sections, "Boost");
    profile.automation.features.shrine = section_has_enabled_value(sections, "Shrine");

    if let Some(boost) = find_section(sections, "Boost") {
        map_budget(
            boost,
            "AFBDiceLimitEnable",
            "AFBDiceLimit",
            &mut profile.safety.item_budgets,
            true,
            file_name,
            mapped,
            report,
        );
        map_budget(
            boost,
            "AFBGlitterLimitEnable",
            "AFBGlitterLimit",
            &mut profile.safety.item_budgets,
            false,
            file_name,
            mapped,
            report,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn map_budget(
    section: &BTreeMap<String, String>,
    enabled_key: &str,
    limit_key: &str,
    budgets: &mut ValuableItemBudgets,
    dice: bool,
    file_name: &str,
    mapped: &mut HashSet<LegacySettingRef>,
    report: &mut LegacyImportReport,
) {
    let enabled = find_value(section, enabled_key)
        .is_some_and(|(_, value)| parse_bool(value).unwrap_or(false));
    if let Some((actual, _)) = find_value(section, enabled_key) {
        mark(mapped, file_name, "Boost", actual);
    }
    if let Some((actual, value)) = find_value(section, limit_key) {
        mark(mapped, file_name, "Boost", actual);
        match value.parse::<u32>() {
            Ok(limit) if enabled => {
                if dice {
                    budgets.dice = limit;
                } else {
                    budgets.glitter = limit;
                }
            }
            Ok(_) => {}
            Err(_) => report.warnings.push(format!(
                "{file_name} [Boost] {actual} has invalid integer value {value:?}; budget kept at zero"
            )),
        }
    }
}

fn section_has_enabled_value(
    sections: &BTreeMap<String, BTreeMap<String, String>>,
    section: &str,
) -> bool {
    find_section(sections, section).is_some_and(|values| {
        values.iter().any(|(key, value)| {
            (key.ends_with("Check") || key.ends_with("Enable"))
                && parse_bool(value).unwrap_or(false)
        })
    })
}

fn map_string(
    file_name: &str,
    section_name: &str,
    section: &BTreeMap<String, String>,
    key: &str,
    destination: &mut String,
    mapped: &mut HashSet<LegacySettingRef>,
) {
    if let Some((actual, value)) = find_value(section, key) {
        if !value.trim().is_empty() {
            destination.clone_from(value);
        }
        mark(mapped, file_name, section_name, actual);
    }
}

fn take_or_default(
    section: &BTreeMap<String, String>,
    key: &str,
    default: &str,
    file_name: &str,
    section_name: &str,
    mapped: &mut HashSet<LegacySettingRef>,
) -> String {
    find_value(section, key).map_or_else(
        || default.into(),
        |(actual, value)| {
            mark(mapped, file_name, section_name, actual);
            value.clone()
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn parse_number<T>(
    section: &BTreeMap<String, String>,
    key: &str,
    default: T,
    file_name: &str,
    section_name: &str,
    mapped: &mut HashSet<LegacySettingRef>,
    report: &mut LegacyImportReport,
) -> T
where
    T: std::str::FromStr + Copy,
{
    let Some((actual, value)) = find_value(section, key) else {
        return default;
    };
    mark(mapped, file_name, section_name, actual);
    value.parse().unwrap_or_else(|_| {
        report.warnings.push(format!(
            "{file_name} [{section_name}] {actual} has invalid numeric value {value:?}; default used"
        ));
        default
    })
}

fn mark(mapped: &mut HashSet<LegacySettingRef>, file_name: &str, section: &str, key: &str) {
    mapped.insert(LegacySettingRef {
        file_name: file_name.into(),
        section: section.into(),
        key: key.into(),
    });
}

fn find_section<'a>(
    sections: &'a BTreeMap<String, BTreeMap<String, String>>,
    wanted: &str,
) -> Option<&'a BTreeMap<String, String>> {
    sections
        .iter()
        .find_map(|(name, values)| name.eq_ignore_ascii_case(wanted).then_some(values))
}

fn find_value<'a>(
    values: &'a BTreeMap<String, String>,
    wanted: &str,
) -> Option<(&'a str, &'a String)> {
    values.iter().find_map(|(key, value)| {
        key.eq_ignore_ascii_case(wanted)
            .then_some((key.as_str(), value))
    })
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn is_sensitive_key(section: &str, key: &str) -> bool {
    let combined = format!("{section}.{key}").to_ascii_lowercase();
    [
        "token",
        "webhook",
        "privserver",
        "private_server",
        "password",
        "secret",
        "cookie",
    ]
    .iter()
    .any(|needle| combined.contains(needle))
}

fn parse_ini(
    source: &str,
    file_name: &str,
) -> (BTreeMap<String, BTreeMap<String, String>>, Vec<String>) {
    let mut sections = BTreeMap::<String, BTreeMap<String, String>>::new();
    let mut warnings = Vec::new();
    let mut section = "General".to_owned();
    for (index, raw_line) in source.lines().enumerate() {
        let line = raw_line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            let name = line[1..line.len() - 1].trim();
            if name.is_empty() {
                warnings.push(format!("{file_name}:{} has an empty section", index + 1));
            } else {
                name.clone_into(&mut section);
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            warnings.push(format!(
                "{file_name}:{} is not a key=value entry and was ignored",
                index + 1
            ));
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            warnings.push(format!("{file_name}:{} has an empty key", index + 1));
            continue;
        }
        let previous = sections
            .entry(section.clone())
            .or_default()
            .insert(key.to_owned(), value.trim().to_owned());
        if previous.is_some() {
            warnings.push(format!(
                "{file_name}:{} repeats [{section}] {key}; the final value was imported",
                index + 1
            ));
        }
    }
    (sections, warnings)
}

#[derive(Debug, Error)]
pub enum LegacyImportError {
    #[error("cannot read legacy settings: {0}")]
    Io(#[from] std::io::Error),
    #[error("legacy file {path} is not UTF-8: {source}")]
    InvalidEncoding {
        path: PathBuf,
        #[source]
        source: std::string::FromUtf8Error,
    },
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::import_legacy_ini_files;

    #[test]
    fn imports_known_values_reports_unknown_and_preserves_source() {
        let directory = tempdir().expect("temp directory");
        let path = directory.path().join("nm_config.ini");
        let original = b"[Settings]\nStartHotkey=F8\nPrivServer=https://secret\nMystery=abc\n[Gather]\nFieldName1=Sunflower\nFieldPattern1=Snake\nFieldUntilMins1=12\nFieldPatternReps1=2\n[Boost]\nAFBDiceLimitEnable=1\nAFBDiceLimit=3\n";
        fs::write(&path, original).expect("fixture");

        let imported = import_legacy_ini_files([&path], "Imported").expect("import");
        assert_eq!(fs::read(&path).expect("unchanged source"), original);
        assert_eq!(imported.profile.automation.hotkeys.start, "F8");
        assert_eq!(imported.profile.automation.rotations[0].gather_seconds, 720);
        assert_eq!(imported.profile.safety.item_budgets.dice, 3);
        assert!(
            imported
                .report
                .unmapped
                .iter()
                .any(|value| value.setting.key == "Mystery" && value.value == "abc")
        );
        assert!(
            imported
                .report
                .sensitive
                .iter()
                .any(|setting| setting.key == "PrivServer")
        );
        let snapshot = imported.profile.legacy.expect("snapshot");
        assert_eq!(
            snapshot.sources[0].sections["Settings"]["PrivServer"],
            "<redacted:requires-secret-import>"
        );
    }
}
