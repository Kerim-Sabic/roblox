use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use specta::Type;

/// A viewport-relative rectangle. All coordinates must be finite and in `0..=1`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Type)]
pub struct NormalizedRegion {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl NormalizedRegion {
    #[must_use]
    pub fn is_valid(self) -> bool {
        [self.x, self.y, self.width, self.height]
            .into_iter()
            .all(f32::is_finite)
            && self.x >= 0.0
            && self.y >= 0.0
            && self.width > 0.0
            && self.height > 0.0
            && self.x + self.width <= 1.0
            && self.y + self.height <= 1.0
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct DetectionEvidence {
    pub detector: String,
    pub observed_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<NormalizedRegion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// A detector result that makes uncertainty impossible to confuse with a value.
///
/// Callers must use [`Detection::actionable`] to obtain an automation target.
/// `NotFound`, `Uncertain`, malformed confidence values, and `Error` always
/// return `None`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Detection<T> {
    Found {
        value: T,
        confidence: f32,
        evidence: DetectionEvidence,
    },
    NotFound {
        evidence: DetectionEvidence,
    },
    Uncertain {
        reason: String,
        evidence: DetectionEvidence,
    },
    Error {
        code: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        evidence: Option<DetectionEvidence>,
    },
}

impl<T> Detection<T> {
    /// Returns the detected value only for a well-formed `Found` result meeting
    /// the requested confidence. This is the sole actionability conversion.
    #[must_use]
    pub fn actionable(&self, minimum_confidence: f32) -> Option<&T> {
        let threshold = minimum_confidence.clamp(0.0, 1.0);
        match self {
            Self::Found {
                value, confidence, ..
            } if confidence.is_finite()
                && (0.0..=1.0).contains(confidence)
                && *confidence >= threshold =>
            {
                Some(value)
            }
            Self::Found { .. }
            | Self::NotFound { .. }
            | Self::Uncertain { .. }
            | Self::Error { .. } => None,
        }
    }

    #[must_use]
    pub const fn is_uncertain(&self) -> bool {
        matches!(self, Self::Uncertain { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::{Detection, DetectionEvidence};
    use chrono::Utc;

    fn evidence() -> DetectionEvidence {
        DetectionEvidence {
            detector: "brown_bear".into(),
            observed_at: Utc::now(),
            region: None,
            artifact_id: None,
            notes: Vec::new(),
        }
    }

    #[test]
    fn uncertain_brown_bear_is_never_actionable() {
        let result = Detection::<String>::Uncertain {
            reason: "OCR returned Unknown".into(),
            evidence: evidence(),
        };
        assert_eq!(result.actionable(0.0), None);
    }

    #[test]
    fn malformed_confidence_is_never_actionable() {
        let result = Detection::Found {
            value: "brown_bear".to_owned(),
            confidence: f32::NAN,
            evidence: evidence(),
        };
        assert_eq!(result.actionable(0.0), None);
    }
}
