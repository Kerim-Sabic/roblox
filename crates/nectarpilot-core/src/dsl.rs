use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const NECTAR_SCHEMA_VERSION: u16 = 1;
const MAX_NESTING: usize = 16;
const MAX_EXPANDED_STEPS: usize = 10_000;
const MAX_STEP_DURATION_MS: u64 = 15 * 60 * 1_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NectarProgram {
    pub schema_version: u16,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub steps: Vec<Step>,
}

impl NectarProgram {
    pub fn from_yaml(source: &str) -> Result<Self, ProgramError> {
        let program: Self = serde_yaml::from_str(source)?;
        program.validate()?;
        Ok(program)
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != NECTAR_SCHEMA_VERSION {
            return Err(ValidationError::UnsupportedVersion {
                received: self.schema_version,
                supported: NECTAR_SCHEMA_VERSION,
            });
        }
        if self.name.trim().is_empty() {
            return Err(ValidationError::EmptyName);
        }
        if self.steps.is_empty() {
            return Err(ValidationError::EmptySteps {
                path: "steps".into(),
            });
        }
        let mut expanded = 0_usize;
        validate_steps(&self.steps, "steps", 0, &mut expanded)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Step {
    Move {
        direction: MoveDirection,
        duration_ms: u32,
        #[serde(default = "default_speed")]
        speed: f32,
    },
    Rotate {
        degrees: f32,
        duration_ms: u32,
    },
    Jump {
        #[serde(default = "default_jump_hold")]
        hold_ms: u32,
    },
    Wait {
        duration_ms: u32,
    },
    Repeat {
        times: u16,
        steps: Vec<Step>,
    },
    /// Runs `on_found` only when the detector produced a confident `Found`.
    /// No branch receives a target for unknown/unavailable detections.
    DetectorCondition {
        detector: String,
        #[serde(default = "default_confidence")]
        minimum_confidence: f32,
        on_found: Vec<Step>,
        #[serde(default)]
        on_unavailable: UnavailableBehavior,
    },
}

const fn default_jump_hold() -> u32 {
    80
}

fn default_speed() -> f32 {
    1.0
}

fn default_confidence() -> f32 {
    0.8
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MoveDirection {
    Forward,
    Backward,
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableBehavior {
    #[default]
    Skip,
    Pause,
    Abort,
}

#[derive(Debug, Error)]
pub enum ProgramError {
    #[error("invalid Nectar YAML: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("invalid Nectar program: {0}")]
    Validation(#[from] ValidationError),
}

#[derive(Debug, Clone, PartialEq, Error)]
pub enum ValidationError {
    #[error("unsupported schema version {received}; this build supports {supported}")]
    UnsupportedVersion { received: u16, supported: u16 },
    #[error("program name cannot be empty")]
    EmptyName,
    #[error("{path} cannot be empty")]
    EmptySteps { path: String },
    #[error("nesting at {path} exceeds {MAX_NESTING} levels")]
    TooDeep { path: String },
    #[error("expanded step count exceeds {MAX_EXPANDED_STEPS}")]
    TooManySteps,
    #[error("invalid duration at {path}: {duration_ms} ms")]
    InvalidDuration { path: String, duration_ms: u64 },
    #[error("invalid number at {path}")]
    InvalidNumber { path: String },
    #[error("repeat count at {path} must be between 1 and 100")]
    InvalidRepeat { path: String },
    #[error("detector name at {path} cannot be empty")]
    EmptyDetector { path: String },
}

fn validate_steps(
    steps: &[Step],
    path: &str,
    depth: usize,
    expanded: &mut usize,
) -> Result<(), ValidationError> {
    if depth > MAX_NESTING {
        return Err(ValidationError::TooDeep { path: path.into() });
    }
    for (index, step) in steps.iter().enumerate() {
        let step_path = format!("{path}[{index}]");
        *expanded = expanded
            .checked_add(1)
            .ok_or(ValidationError::TooManySteps)?;
        if *expanded > MAX_EXPANDED_STEPS {
            return Err(ValidationError::TooManySteps);
        }
        match step {
            Step::Move {
                duration_ms, speed, ..
            } => {
                validate_duration(u64::from(*duration_ms), &step_path)?;
                if !speed.is_finite() || !(0.05..=1.0).contains(speed) {
                    return Err(ValidationError::InvalidNumber {
                        path: format!("{step_path}.speed"),
                    });
                }
            }
            Step::Rotate {
                degrees,
                duration_ms,
            } => {
                validate_duration(u64::from(*duration_ms), &step_path)?;
                if !degrees.is_finite() || *degrees == 0.0 || degrees.abs() > 1_440.0 {
                    return Err(ValidationError::InvalidNumber {
                        path: format!("{step_path}.degrees"),
                    });
                }
            }
            Step::Jump { hold_ms } => validate_duration(u64::from(*hold_ms), &step_path)?,
            Step::Wait { duration_ms } => {
                validate_duration(u64::from(*duration_ms), &step_path)?;
            }
            Step::Repeat { times, steps } => {
                if !(1..=100).contains(times) {
                    return Err(ValidationError::InvalidRepeat { path: step_path });
                }
                if steps.is_empty() {
                    return Err(ValidationError::EmptySteps { path: step_path });
                }
                let before = *expanded;
                validate_steps(steps, &step_path, depth + 1, expanded)?;
                let nested = *expanded - before;
                *expanded = expanded
                    .checked_add(nested.saturating_mul(usize::from(*times) - 1))
                    .ok_or(ValidationError::TooManySteps)?;
                if *expanded > MAX_EXPANDED_STEPS {
                    return Err(ValidationError::TooManySteps);
                }
            }
            Step::DetectorCondition {
                detector,
                minimum_confidence,
                on_found,
                ..
            } => {
                if detector.trim().is_empty() {
                    return Err(ValidationError::EmptyDetector { path: step_path });
                }
                if !minimum_confidence.is_finite() || !(0.0..=1.0).contains(minimum_confidence) {
                    return Err(ValidationError::InvalidNumber {
                        path: format!("{step_path}.minimum_confidence"),
                    });
                }
                if on_found.is_empty() {
                    return Err(ValidationError::EmptySteps {
                        path: format!("{step_path}.on_found"),
                    });
                }
                validate_steps(on_found, &step_path, depth + 1, expanded)?;
            }
        }
    }
    Ok(())
}

fn validate_duration(duration_ms: u64, path: &str) -> Result<(), ValidationError> {
    if duration_ms == 0 || duration_ms > MAX_STEP_DURATION_MS {
        Err(ValidationError::InvalidDuration {
            path: path.into(),
            duration_ms,
        })
    } else {
        Ok(())
    }
}

/// Resolves the only executable branch for a detector result.
#[must_use]
pub fn detector_branch<'a, T>(
    detection: &nectarpilot_contracts::Detection<T>,
    minimum_confidence: f32,
    on_found: &'a [Step],
) -> Option<&'a [Step]> {
    detection.actionable(minimum_confidence).map(|_| on_found)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use nectarpilot_contracts::{Detection, DetectionEvidence};

    use super::{NectarProgram, Step, detector_branch};

    #[test]
    fn parses_and_validates_a_program() {
        let source = r"
schema_version: 1
name: test pattern
steps:
  - type: move
    direction: forward
    duration_ms: 250
  - type: repeat
    times: 2
    steps:
      - type: wait
        duration_ms: 20
";
        let program = NectarProgram::from_yaml(source).expect("valid YAML");
        assert_eq!(program.steps.len(), 2);
    }

    #[test]
    fn unknown_brown_bear_has_no_executable_branch() {
        let evidence = DetectionEvidence {
            detector: "brown_bear".into(),
            observed_at: Utc::now(),
            region: None,
            artifact_id: None,
            notes: vec!["OCR vocabulary result: Unknown".into()],
        };
        let unknown = Detection::<String>::Uncertain {
            reason: "unknown quest state".into(),
            evidence,
        };
        let actions = vec![Step::Move {
            direction: super::MoveDirection::Forward,
            duration_ms: 100,
            speed: 1.0,
        }];
        assert!(detector_branch(&unknown, 0.8, &actions).is_none());
    }
}
