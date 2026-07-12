//! Typed observations emitted by the screen-only perception layer.
//!
//! These values deliberately describe what was seen, rather than what the
//! automation should do.  Navigation code must obtain a value through
//! [`Detection::actionable`], so an unknown OCR result or an ambiguous template
//! match can never become an input target.

use nectarpilot_contracts::Detection;
use serde::{Deserialize, Serialize};

use crate::quests::{FieldId, QuestGiver};

/// Minimum confidence required before a live observation can name a movement
/// target. Detectors may use a higher threshold, but never a lower one.
pub const MINIMUM_MOVEMENT_CONFIDENCE: f32 = 0.85;

/// A catalog-constrained quest title observed in the Roblox client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestCandidate {
    pub giver: QuestGiver,
    pub quest_id: String,
    pub sequence: u16,
    pub name: String,
}

/// A calibrated field label. It remains a candidate until a caller explicitly
/// asks [`LivePerception::field_target`] for an actionable target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldCandidate {
    pub field: FieldId,
}

/// The limited set of hive states a visual detector is allowed to report.
/// `Unknown` is intentionally absent: ambiguity must be represented by
/// `Detection::Uncertain`, where it cannot be accidentally acted upon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HiveState {
    Claimable,
    ClaimedByAttachedSession,
    Occupied,
}

/// A detected hive slot and its unambiguous visual state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HiveCandidate {
    pub slot: u8,
    pub state: HiveState,
}

/// A vocabulary-constrained in-game interaction prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptKind {
    Interact,
    ClaimHive,
    AcceptQuest,
    ContinueDialogue,
    Reconnect,
    Disconnect,
}

/// A prompt is an observation only. Separately configured task state and
/// permissions decide whether any keypress is permitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptCandidate {
    pub kind: PromptKind,
}

/// The bounded set of live visual observations collected from one client frame.
#[derive(Debug, Clone, PartialEq)]
pub struct LivePerception {
    pub quest: Detection<QuestCandidate>,
    pub field: Detection<FieldCandidate>,
    pub hive: Detection<HiveCandidate>,
    pub prompt: Detection<PromptCandidate>,
}

/// A routeable target, produced only from a confident `Found` observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovementTarget {
    Field(FieldId),
    Hive(HiveCandidate),
}

impl LivePerception {
    /// Returns a field only when detection is a well-formed, high-confidence
    /// `Found` result. `NotFound`, `Uncertain`, malformed confidence, and
    /// detector errors all result in `None`.
    #[must_use]
    pub fn field_target(&self) -> Option<FieldId> {
        self.field
            .actionable(MINIMUM_MOVEMENT_CONFIDENCE)
            .map(|candidate| candidate.field)
    }

    /// Returns a hive target only for an explicit, high-confidence visual
    /// result. Callers still need task-policy permission before interacting.
    #[must_use]
    pub fn hive_target(&self) -> Option<HiveCandidate> {
        self.hive.actionable(MINIMUM_MOVEMENT_CONFIDENCE).copied()
    }

    /// Prompt observations are never navigation targets. They can only be
    /// consulted by the currently authorised task after the same confidence
    /// gate is applied.
    #[must_use]
    pub fn prompt_target(&self) -> Option<PromptCandidate> {
        self.prompt.actionable(MINIMUM_MOVEMENT_CONFIDENCE).copied()
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use nectarpilot_contracts::DetectionEvidence;

    use super::{
        FieldCandidate, HiveCandidate, HiveState, LivePerception, PromptCandidate, PromptKind,
    };
    use crate::quests::FieldId;
    use nectarpilot_contracts::Detection;

    fn evidence() -> DetectionEvidence {
        DetectionEvidence {
            detector: "fixture".to_owned(),
            observed_at: Utc::now(),
            region: None,
            artifact_id: None,
            notes: Vec::new(),
        }
    }

    #[test]
    fn uncertain_field_and_hive_never_become_movement_targets() {
        let perception = LivePerception {
            quest: Detection::NotFound {
                evidence: evidence(),
            },
            field: Detection::Uncertain {
                reason: "two field templates were too close".to_owned(),
                evidence: evidence(),
            },
            hive: Detection::Uncertain {
                reason: "slot text was partially occluded".to_owned(),
                evidence: evidence(),
            },
            prompt: Detection::Uncertain {
                reason: "prompt OCR was not in the approved vocabulary".to_owned(),
                evidence: evidence(),
            },
        };

        assert_eq!(perception.field_target(), None);
        assert_eq!(perception.hive_target(), None);
        assert_eq!(perception.prompt_target(), None);
    }

    #[test]
    fn only_high_confidence_found_values_are_exposed() {
        let perception = LivePerception {
            quest: Detection::NotFound {
                evidence: evidence(),
            },
            field: Detection::Found {
                value: FieldCandidate {
                    field: FieldId::Bamboo,
                },
                confidence: 0.85,
                evidence: evidence(),
            },
            hive: Detection::Found {
                value: HiveCandidate {
                    slot: 4,
                    state: HiveState::ClaimedByAttachedSession,
                },
                confidence: 0.849,
                evidence: evidence(),
            },
            prompt: Detection::Found {
                value: PromptCandidate {
                    kind: PromptKind::Interact,
                },
                confidence: 1.0,
                evidence: evidence(),
            },
        };

        assert_eq!(perception.field_target(), Some(FieldId::Bamboo));
        assert_eq!(perception.hive_target(), None);
        assert_eq!(
            perception.prompt_target(),
            Some(PromptCandidate {
                kind: PromptKind::Interact
            })
        );
    }
}
