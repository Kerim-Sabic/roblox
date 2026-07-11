use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PatternStep {
    Wait { duration_ms: u32 },
    Rotate { degrees: f32, duration_ms: u32 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SafePattern {
    pub schema_version: u16,
    pub name: String,
    pub steps: Vec<PatternStep>,
}

impl SafePattern {
    pub fn to_yaml(&self) -> Result<String, serde_yaml::Error> {
        serde_yaml::to_string(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueKind {
    UnsupportedSyntax,
    UnsafeCapability,
    InvalidValue,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConversionIssue {
    pub line: usize,
    pub kind: IssueKind,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConversionReport {
    /// Present only when every executable line was recognized and validated.
    pub converted: Option<SafePattern>,
    pub issues: Vec<ConversionIssue>,
    pub requires_legacy_bridge: bool,
}

/// Converts only an intentionally small, deterministic `AutoHotkey` movement
/// subset. Variables, expressions, loops, function calls, and interpolation are
/// reported instead of guessed.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "keeping the accepted grammar in one linear parser makes the safety boundary auditable"
)]
pub fn convert_movement_pattern(name: &str, source: &str) -> ConversionReport {
    let sleep = Regex::new(r"(?i)^\s*(?:Sleep|HyperSleep)\s*\(?\s*(\d+)\s*\)?\s*$")
        .expect("static sleep regex is valid");
    let walk = Regex::new(r"(?i)^\s*Walk\(\s*(\d+(?:\.\d+)?)\s*(?:,\s*(\d{1,3})\s*)?\)\s*$")
        .expect("static walk regex is valid");
    let rotate = Regex::new(r"(?i)^\s*Rotate\(\s*(-?\d+(?:\.\d+)?)\s*,\s*(\d+)\s*\)\s*$")
        .expect("static rotate regex is valid");
    let send = Regex::new(r#"(?i)^\s*Send(?:Input)?\s+\"([^\"]*)\"\s*$"#)
        .expect("static send regex is valid");
    let token = Regex::new(r"(?i)\{\s*(w|a|s|d|space)\s+(down|up)\s*\}")
        .expect("static send-token regex is valid");
    let dangerous = Regex::new(
        r"(?i)\b(DllCall|Run|RunWait|FileDelete|FileMove|FileCopy|RegWrite|Shutdown|ProcessClose|URLDownloadToFile)\b",
    )
    .expect("static dangerous-capability regex is valid");

    let mut steps = Vec::new();
    let mut issues = Vec::new();
    for (index, raw_line) in source.lines().enumerate() {
        let line_number = index + 1;
        let line = strip_comment(raw_line.trim_start_matches('\u{feff}')).trim();
        if line.is_empty() {
            continue;
        }
        if let Some(capability) = dangerous.find(line) {
            issues.push(ConversionIssue {
                line: line_number,
                kind: IssueKind::UnsafeCapability,
                message: format!(
                    "{} is outside the safe pattern DSL and will not be converted",
                    capability.as_str()
                ),
            });
            continue;
        }
        if let Some(captures) = sleep.captures(line) {
            let duration_ms = captures[1].parse::<u32>().unwrap_or(u32::MAX);
            if duration_ms == 0 || duration_ms > 600_000 {
                issues.push(invalid_value(
                    line_number,
                    "wait must be within 1..=600000 ms",
                ));
            } else {
                steps.push(PatternStep::Wait { duration_ms });
            }
            continue;
        }
        if let Some(captures) = walk.captures(line) {
            let units = captures[1].parse::<f64>().unwrap_or(f64::NAN);
            let speed_percent = captures
                .get(2)
                .and_then(|value| value.as_str().parse::<u8>().ok());
            if !units.is_finite()
                || units <= 0.0
                || units > 1_000.0
                || speed_percent.is_some_and(|speed| speed == 0 || speed > 100)
            {
                issues.push(invalid_value(
                    line_number,
                    "walk must use literal units in (0, 1000] and speed in [1, 100]",
                ));
            } else {
                issues.push(unsupported(
                    line_number,
                    "Walk units require profile calibration and cannot be converted to a timed move safely",
                ));
            }
            continue;
        }
        if let Some(captures) = rotate.captures(line) {
            let degrees = captures[1].parse::<f32>().unwrap_or(f32::NAN);
            let duration_ms = captures[2].parse::<u32>().unwrap_or(u32::MAX);
            if !degrees.is_finite()
                || !(-360.0..=360.0).contains(&degrees)
                || duration_ms == 0
                || duration_ms > 60_000
            {
                issues.push(invalid_value(
                    line_number,
                    "rotation requires -360..=360 degrees and a 1..=60000 ms duration",
                ));
            } else {
                steps.push(PatternStep::Rotate {
                    degrees,
                    duration_ms,
                });
            }
            continue;
        }
        if let Some(captures) = send.captures(line) {
            let payload = &captures[1];
            let matches_only_tokens =
                token.replace_all(payload, "").trim().is_empty() && token.is_match(payload);
            if matches_only_tokens {
                issues.push(unsupported(
                    line_number,
                    "key down/up sequences require temporal pairing and cannot be partially converted safely",
                ));
            } else {
                issues.push(unsupported(
                    line_number,
                    "Send accepts only literal w/a/s/d/space down/up tokens",
                ));
            }
            continue;
        }
        issues.push(unsupported(
            line_number,
            "dynamic or unrecognized AutoHotkey syntax requires the opt-in legacy bridge",
        ));
    }

    let requires_legacy_bridge = !issues.is_empty();
    ConversionReport {
        converted: (!requires_legacy_bridge).then(|| SafePattern {
            schema_version: 1,
            name: sanitize_name(name),
            steps,
        }),
        issues,
        requires_legacy_bridge,
    }
}

fn strip_comment(line: &str) -> &str {
    let mut quoted = false;
    for (index, character) in line.char_indices() {
        if character == '"' {
            quoted = !quoted;
        } else if character == ';' && !quoted {
            return &line[..index];
        }
    }
    line
}

fn sanitize_name(name: &str) -> String {
    let value = name
        .chars()
        .filter_map(|character| {
            if character.is_ascii_alphanumeric() {
                Some(character.to_ascii_lowercase())
            } else if character == '-' || character == '_' || character.is_ascii_whitespace() {
                Some('-')
            } else {
                None
            }
        })
        .take(64)
        .collect::<String>();
    if value.is_empty() {
        "imported-pattern".to_owned()
    } else {
        value
    }
}

fn unsupported(line: usize, message: &str) -> ConversionIssue {
    ConversionIssue {
        line,
        kind: IssueKind::UnsupportedSyntax,
        message: message.to_owned(),
    }
}

fn invalid_value(line: usize, message: &str) -> ConversionIssue {
    ConversionIssue {
        line,
        kind: IssueKind::InvalidValue,
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_only_literal_movement_commands() {
        let source = r"
            Sleep 250
            Rotate(45, 500)
        ";
        let report = convert_movement_pattern("Safe Lines", source);
        let pattern = report.converted.unwrap();
        assert!(!report.requires_legacy_bridge);
        assert_eq!(pattern.name, "safe-lines");
        assert_eq!(pattern.steps.len(), 2);
        let yaml = pattern.to_yaml().unwrap();
        assert!(yaml.contains("schema_version: 1"));
        nectarpilot_core::dsl::NectarProgram::from_yaml(&yaml)
            .expect("converted output must remain valid in the native DSL");
    }

    #[test]
    fn reports_dynamic_and_dangerous_lines_without_partial_conversion() {
        let source = "Send \"{w down}\"\nWalk(size * 4)\nDllCall(\"user32.dll\")";
        let report = convert_movement_pattern("unsafe", source);
        assert!(report.converted.is_none());
        assert!(report.requires_legacy_bridge);
        assert_eq!(report.issues.len(), 3);
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.kind == IssueKind::UnsafeCapability)
        );
    }
}
