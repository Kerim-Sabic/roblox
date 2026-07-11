//! Typed quest knowledge and overlap-aware field planning.
//!
//! Quest data is versioned separately from automation code so changed game
//! requirements can be reviewed without weakening detector safety. A planner
//! only consumes objectives from a confident quest detection and only emits a
//! field when the field opportunity is also confidently calibrated.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use chrono::Utc;
use nectarpilot_contracts::{Detection, DetectionEvidence};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const MINIMUM_ACTION_CONFIDENCE: f32 = 0.75;
const CURRENT_FIELD_HYSTERESIS: f64 = 0.88;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestGiver {
    ScienceBear,
    BlackBear,
    BrownBear,
    PolarBear,
    PandaBear,
    MotherBear,
    SpiritBear,
    DapperBear,
    GiftedBuckoBee,
    GiftedRileyBee,
    HoneyBee,
    Onett,
    StickerSeeker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldId {
    Sunflower,
    Dandelion,
    Mushroom,
    BlueFlower,
    Clover,
    Spider,
    Bamboo,
    Strawberry,
    Pineapple,
    Pumpkin,
    Cactus,
    Rose,
    PineTree,
    MountainTop,
    Stump,
    Ant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PollenColor {
    Red,
    Blue,
    White,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum QuestObjective {
    Pollen {
        amount: u64,
        #[serde(default)]
        field: Option<FieldId>,
        #[serde(default)]
        color: Option<PollenColor>,
    },
    Goo {
        amount: u64,
        #[serde(default)]
        field: Option<FieldId>,
        #[serde(default)]
        color: Option<PollenColor>,
    },
    Token {
        token: String,
        amount: u64,
    },
    Defeat {
        mob: String,
        count: u64,
    },
    DiscoverBeeTypes {
        count: u64,
    },
    EarnBadge {
        badge: String,
        count: u64,
    },
    Craft {
        station: String,
        count: u64,
    },
    UseItem {
        item: String,
        count: u64,
    },
    FeedItem {
        item: String,
        count: u64,
    },
    UseMachine {
        machine: String,
        count: u64,
    },
    ObtainBee {
        bee: String,
    },
    CompleteQuest {
        giver: QuestGiver,
        count: u64,
    },
}

impl QuestObjective {
    #[must_use]
    pub const fn target(&self) -> u64 {
        match self {
            Self::Pollen { amount, .. } | Self::Goo { amount, .. } | Self::Token { amount, .. } => {
                *amount
            }
            Self::Defeat { count, .. }
            | Self::DiscoverBeeTypes { count }
            | Self::EarnBadge { count, .. }
            | Self::Craft { count, .. }
            | Self::UseItem { count, .. }
            | Self::FeedItem { count, .. }
            | Self::UseMachine { count, .. }
            | Self::CompleteQuest { count, .. } => *count,
            Self::ObtainBee { .. } => 1,
        }
    }

    fn validate(&self) -> Result<(), QuestCatalogError> {
        if self.target() == 0 {
            return Err(QuestCatalogError::ZeroObjectiveTarget);
        }
        let label = match self {
            Self::Token { token, .. } => Some(token),
            Self::Defeat { mob, .. } => Some(mob),
            Self::EarnBadge { badge, .. } => Some(badge),
            Self::Craft { station, .. } => Some(station),
            Self::UseItem { item, .. } | Self::FeedItem { item, .. } => Some(item),
            Self::UseMachine { machine, .. } => Some(machine),
            Self::ObtainBee { bee } => Some(bee),
            _ => None,
        };
        if label.is_some_and(|value| value.trim().is_empty() || value.len() > 120) {
            return Err(QuestCatalogError::InvalidObjectiveLabel);
        }
        Ok(())
    }

    fn requires_explicit_budget(&self) -> Option<&str> {
        match self {
            Self::UseItem { item, .. } | Self::FeedItem { item, .. } => Some(item),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestDefinition {
    pub sequence: u16,
    pub id: String,
    pub giver: QuestGiver,
    pub name: String,
    #[serde(default)]
    pub translator_reward: bool,
    pub objectives: Vec<QuestObjective>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestCatalog {
    pub schema_version: u16,
    pub knowledge_version: String,
    pub source_url: String,
    pub quests: Vec<QuestDefinition>,
}

impl QuestCatalog {
    pub fn from_json(source: &str) -> Result<Self, QuestCatalogError> {
        let catalog: Self = serde_json::from_str(source)?;
        catalog.validate()?;
        Ok(catalog)
    }

    pub fn validate(&self) -> Result<(), QuestCatalogError> {
        if self.schema_version != 1 {
            return Err(QuestCatalogError::UnsupportedSchema(self.schema_version));
        }
        if !self.source_url.starts_with("https://") || self.knowledge_version.trim().is_empty() {
            return Err(QuestCatalogError::InvalidProvenance);
        }
        if self.quests.is_empty() {
            return Err(QuestCatalogError::EmptyCatalog);
        }
        let mut ids = BTreeSet::new();
        for (index, quest) in self.quests.iter().enumerate() {
            let expected =
                u16::try_from(index + 1).map_err(|_| QuestCatalogError::TooManyQuests)?;
            if quest.sequence != expected {
                return Err(QuestCatalogError::NonContiguousSequence {
                    expected,
                    actual: quest.sequence,
                });
            }
            if quest.id.trim().is_empty() || quest.name.trim().is_empty() || !ids.insert(&quest.id)
            {
                return Err(QuestCatalogError::InvalidQuestIdentity);
            }
            if quest.objectives.is_empty() {
                return Err(QuestCatalogError::EmptyQuest(quest.id.clone()));
            }
            for objective in &quest.objectives {
                objective.validate()?;
            }
        }
        Ok(())
    }
}

#[must_use]
pub fn science_bear_catalog() -> QuestCatalog {
    QuestCatalog::from_json(include_str!("../../../assets/quests/science-bear.v1.json"))
        .expect("checked-in Science Bear catalog must validate")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestMatch {
    pub id: String,
    pub sequence: u16,
    pub name: String,
}

/// Constrained Science Bear title detector. It never turns arbitrary OCR text
/// into a route; only a unique catalog match with a sufficient margin can be
/// considered by temporal consensus.
#[must_use]
pub fn detect_science_bear_title(observed: &str, ocr_confidence: f32) -> Detection<QuestMatch> {
    let evidence = DetectionEvidence {
        detector: "science_bear_quest_title".to_owned(),
        observed_at: Utc::now(),
        region: None,
        artifact_id: None,
        notes: Vec::new(),
    };
    if !ocr_confidence.is_finite() || !(0.0..=1.0).contains(&ocr_confidence) {
        return Detection::Error {
            code: "invalid_ocr_confidence".to_owned(),
            message: "OCR confidence was outside 0..=1".to_owned(),
            evidence: Some(evidence),
        };
    }
    let normalized = normalize_title(observed);
    if normalized.is_empty() {
        return Detection::NotFound { evidence };
    }
    if normalized.chars().count() > 128 {
        return Detection::Uncertain {
            reason: "quest title exceeded the bounded OCR vocabulary length".to_owned(),
            evidence,
        };
    }
    if ocr_confidence < MINIMUM_ACTION_CONFIDENCE {
        return Detection::Uncertain {
            reason: "quest title OCR confidence is below the safe threshold".to_owned(),
            evidence,
        };
    }

    let catalog = science_bear_catalog();
    let mut scored = catalog
        .quests
        .iter()
        .map(|quest| {
            let candidate = normalize_title(&quest.name);
            let distance = edit_distance(&normalized, &candidate);
            let width = normalized
                .chars()
                .count()
                .max(candidate.chars().count())
                .max(1);
            let bounded_distance =
                u16::try_from(distance).expect("bounded quest titles have a u16 edit distance");
            let bounded_width =
                u16::try_from(width).expect("bounded quest titles have a u16 width");
            let similarity = 1.0 - f32::from(bounded_distance) / f32::from(bounded_width);
            (quest, similarity)
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| right.1.total_cmp(&left.1));
    let Some((best, best_score)) = scored.first().copied() else {
        return Detection::NotFound { evidence };
    };
    let runner_up = scored.get(1).map_or(0.0, |(_, score)| *score);
    if best_score < 0.82 || best_score - runner_up < 0.08 {
        return Detection::Uncertain {
            reason: "quest title did not uniquely match the constrained Science Bear vocabulary"
                .to_owned(),
            evidence,
        };
    }
    Detection::Found {
        value: QuestMatch {
            id: best.id.clone(),
            sequence: best.sequence,
            name: best.name.clone(),
        },
        confidence: ocr_confidence * best_score,
        evidence,
    }
}

#[derive(Debug, Default)]
pub struct QuestTitleConsensus {
    observations: VecDeque<(QuestMatch, f32)>,
}

impl QuestTitleConsensus {
    /// Requires two agreeing catalog matches within the last three confident
    /// frames. Disagreement is `Uncertain`, never whichever frame came last.
    #[must_use]
    pub fn observe(&mut self, observed: &str, ocr_confidence: f32) -> Detection<QuestMatch> {
        let raw = detect_science_bear_title(observed, ocr_confidence);
        let Detection::Found {
            value, confidence, ..
        } = raw
        else {
            return raw;
        };
        self.observations.push_back((value.clone(), confidence));
        while self.observations.len() > 3 {
            self.observations.pop_front();
        }

        let agreeing = self
            .observations
            .iter()
            .filter(|(candidate, _)| candidate.id == value.id)
            .collect::<Vec<_>>();
        let evidence = DetectionEvidence {
            detector: "science_bear_quest_title_consensus".to_owned(),
            observed_at: Utc::now(),
            region: None,
            artifact_id: None,
            notes: vec![format!(
                "{} of {} recent frames agree",
                agreeing.len(),
                self.observations.len()
            )],
        };
        if agreeing.len() < 2 {
            return Detection::Uncertain {
                reason: "waiting for a second agreeing quest-title frame".to_owned(),
                evidence,
            };
        }
        let agreeing_count =
            u16::try_from(agreeing.len()).expect("consensus window contains at most three frames");
        let average = agreeing
            .iter()
            .map(|(_, confidence)| *confidence)
            .sum::<f32>()
            / f32::from(agreeing_count);
        Detection::Found {
            value,
            confidence: average,
            evidence,
        }
    }
}

fn normalize_title(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .filter(|word| !matches!(*word, "science" | "bear" | "quest"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn edit_distance(left: &str, right: &str) -> usize {
    let right_chars = right.chars().collect::<Vec<_>>();
    let mut previous = (0..=right_chars.len()).collect::<Vec<_>>();
    let mut current = vec![0; right_chars.len() + 1];
    for (left_index, left_character) in left.chars().enumerate() {
        current[0] = left_index + 1;
        for (right_index, right_character) in right_chars.iter().enumerate() {
            let substitution = usize::from(left_character != *right_character);
            current[right_index + 1] = (current[right_index] + 1)
                .min(previous[right_index + 1] + 1)
                .min(previous[right_index] + substitution);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right_chars.len()]
}

#[derive(Debug, Clone)]
pub struct ActiveQuest {
    pub quest: QuestDefinition,
    /// Progress values correspond by index to `quest.objectives`.
    pub progress: Vec<u64>,
    pub detection_confidence: f32,
}

impl ActiveQuest {
    fn remaining(&self, objective_index: usize) -> Option<u64> {
        let objective = self.quest.objectives.get(objective_index)?;
        let progress = self.progress.get(objective_index).copied().unwrap_or(0);
        Some(objective.target().saturating_sub(progress))
    }
}

#[derive(Debug, Clone)]
pub struct FieldOpportunity {
    pub field: FieldId,
    pub colors: BTreeSet<PollenColor>,
    pub pollen_per_minute: f64,
    pub goo_per_minute: f64,
    pub token_rates_per_minute: BTreeMap<String, f64>,
    pub mob_rates_per_minute: BTreeMap<String, f64>,
    pub travel_seconds: u32,
    pub calibration_confidence: f32,
}

#[derive(Debug, Clone)]
pub struct QuestPlannerInput {
    pub active_quests: Vec<ActiveQuest>,
    pub fields: Vec<FieldOpportunity>,
    pub current_field: Option<FieldId>,
    pub current_field_dwell_seconds: u32,
    /// Normalized lowercase item identifiers and their remaining allowed uses.
    pub item_budgets: BTreeMap<String, u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObjectiveAdvance {
    pub quest_id: String,
    pub objective_index: usize,
    pub expected_fraction_per_minute: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldCandidate {
    pub field: FieldId,
    pub score: f64,
    pub advances: Vec<ObjectiveAdvance>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldObjective {
    pub quest_id: String,
    pub objective_index: usize,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QuestPlan {
    pub recommended_field: Option<FieldId>,
    pub candidates: Vec<FieldCandidate>,
    pub held_objectives: Vec<HeldObjective>,
}

#[derive(Debug, Default)]
pub struct QuestPlanner;

impl QuestPlanner {
    #[must_use]
    pub fn plan(input: &QuestPlannerInput) -> QuestPlan {
        let confident_quests = input
            .active_quests
            .iter()
            .filter(|quest| {
                quest.detection_confidence.is_finite()
                    && quest.detection_confidence >= MINIMUM_ACTION_CONFIDENCE
            })
            .collect::<Vec<_>>();

        let mut held_objectives = Vec::new();
        for active in &confident_quests {
            for (index, objective) in active.quest.objectives.iter().enumerate() {
                if active
                    .remaining(index)
                    .is_none_or(|remaining| remaining == 0)
                {
                    continue;
                }
                if let Some(item) = objective.requires_explicit_budget() {
                    let normalized = item.trim().to_ascii_lowercase();
                    if input.item_budgets.get(&normalized).copied().unwrap_or(0) == 0 {
                        held_objectives.push(HeldObjective {
                            quest_id: active.quest.id.clone(),
                            objective_index: index,
                            reason: format!(
                                "{item} use is held because its explicit budget is zero"
                            ),
                        });
                    }
                } else if !matches!(
                    objective,
                    QuestObjective::Pollen { .. }
                        | QuestObjective::Goo { .. }
                        | QuestObjective::Token { .. }
                        | QuestObjective::Defeat { .. }
                ) {
                    held_objectives.push(HeldObjective {
                        quest_id: active.quest.id.clone(),
                        objective_index: index,
                        reason: "objective requires a dedicated verified task, not field movement"
                            .to_owned(),
                    });
                }
            }
        }

        let mut candidates = input
            .fields
            .iter()
            .filter(|field| {
                field.calibration_confidence.is_finite()
                    && field.calibration_confidence >= MINIMUM_ACTION_CONFIDENCE
            })
            .filter_map(|field| score_field(field, &confident_quests))
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.field.cmp(&right.field))
        });

        let mut recommended_field = candidates.first().map(|candidate| candidate.field);
        if input.current_field_dwell_seconds < 180
            && let (Some(current), Some(best)) = (input.current_field, candidates.first())
            && let Some(current_candidate) = candidates.iter().find(|item| item.field == current)
            && current_candidate.score >= best.score * CURRENT_FIELD_HYSTERESIS
        {
            recommended_field = Some(current);
        }

        QuestPlan {
            recommended_field,
            candidates,
            held_objectives,
        }
    }
}

fn score_field(field: &FieldOpportunity, quests: &[&ActiveQuest]) -> Option<FieldCandidate> {
    let mut advances = Vec::new();
    let mut raw_score = 0.0;
    for active in quests {
        let progression_weight = match active.quest.giver {
            QuestGiver::ScienceBear if active.quest.translator_reward => 2.4,
            QuestGiver::ScienceBear => 1.55,
            _ => 1.0,
        };
        for (index, objective) in active.quest.objectives.iter().enumerate() {
            let Some(remaining) = active.remaining(index).filter(|remaining| *remaining > 0) else {
                continue;
            };
            let rate = objective_rate(field, objective);
            if !rate.is_finite() || rate <= 0.0 {
                continue;
            }
            let fraction = (rate / u64_as_f64(remaining)).clamp(0.0, 1.0);
            raw_score += fraction * progression_weight;
            advances.push(ObjectiveAdvance {
                quest_id: active.quest.id.clone(),
                objective_index: index,
                expected_fraction_per_minute: fraction,
            });
        }
    }
    if advances.is_empty() {
        return None;
    }
    let travel_multiplier = 1.0 / (1.0 + f64::from(field.travel_seconds) / 300.0);
    Some(FieldCandidate {
        field: field.field,
        score: raw_score * travel_multiplier,
        advances,
    })
}

fn u64_as_f64(value: u64) -> f64 {
    let bytes = value.to_be_bytes();
    let high = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let low = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    f64::from(high).mul_add(4_294_967_296.0, f64::from(low))
}

fn objective_rate(field: &FieldOpportunity, objective: &QuestObjective) -> f64 {
    match objective {
        QuestObjective::Pollen {
            field: required_field,
            color,
            ..
        } => {
            if required_field.is_some_and(|required| required != field.field)
                || color.is_some_and(|required| !field.colors.contains(&required))
            {
                0.0
            } else {
                field.pollen_per_minute
            }
        }
        QuestObjective::Goo {
            field: required_field,
            color,
            ..
        } => {
            if required_field.is_some_and(|required| required != field.field)
                || color.is_some_and(|required| !field.colors.contains(&required))
            {
                0.0
            } else {
                field.goo_per_minute
            }
        }
        QuestObjective::Token { token, .. } => field
            .token_rates_per_minute
            .get(&token.to_ascii_lowercase())
            .copied()
            .unwrap_or(0.0),
        QuestObjective::Defeat { mob, .. } => field
            .mob_rates_per_minute
            .get(&mob.to_ascii_lowercase())
            .copied()
            .unwrap_or(0.0),
        _ => 0.0,
    }
}

#[derive(Debug, Error)]
pub enum QuestCatalogError {
    #[error("quest catalog JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported quest catalog schema {0}")]
    UnsupportedSchema(u16),
    #[error("quest catalog provenance is invalid")]
    InvalidProvenance,
    #[error("quest catalog is empty")]
    EmptyCatalog,
    #[error("quest catalog contains too many quests")]
    TooManyQuests,
    #[error("quest sequence is not contiguous: expected {expected}, received {actual}")]
    NonContiguousSequence { expected: u16, actual: u16 },
    #[error("quest identity is empty or duplicated")]
    InvalidQuestIdentity,
    #[error("quest {0} contains no objectives")]
    EmptyQuest(String),
    #[error("quest objective target must be positive")]
    ZeroObjectiveTarget,
    #[error("quest objective label is invalid")]
    InvalidObjectiveLabel,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(
        field: FieldId,
        colors: &[PollenColor],
        pollen_per_minute: f64,
        travel_seconds: u32,
    ) -> FieldOpportunity {
        FieldOpportunity {
            field,
            colors: colors.iter().copied().collect(),
            pollen_per_minute,
            goo_per_minute: 0.0,
            token_rates_per_minute: BTreeMap::new(),
            mob_rates_per_minute: BTreeMap::new(),
            travel_seconds,
            calibration_confidence: 0.95,
        }
    }

    fn quest(id: &str, objectives: Vec<QuestObjective>, confidence: f32) -> ActiveQuest {
        ActiveQuest {
            quest: QuestDefinition {
                sequence: 1,
                id: id.to_owned(),
                giver: QuestGiver::ScienceBear,
                name: id.to_owned(),
                translator_reward: false,
                objectives,
            },
            progress: Vec::new(),
            detection_confidence: confidence,
        }
    }

    #[test]
    fn overlapping_objectives_outweigh_a_slightly_faster_single_field() {
        let active = quest(
            "overlap",
            vec![
                QuestObjective::Pollen {
                    amount: 100_000,
                    field: Some(FieldId::Bamboo),
                    color: None,
                },
                QuestObjective::Pollen {
                    amount: 100_000,
                    field: None,
                    color: Some(PollenColor::Blue),
                },
            ],
            0.95,
        );
        let plan = QuestPlanner::plan(&QuestPlannerInput {
            active_quests: vec![active],
            fields: vec![
                field(FieldId::Bamboo, &[PollenColor::Blue], 10_000.0, 30),
                field(FieldId::PineTree, &[PollenColor::Blue], 13_000.0, 20),
            ],
            current_field: None,
            current_field_dwell_seconds: 300,
            item_budgets: BTreeMap::new(),
        });
        assert_eq!(plan.recommended_field, Some(FieldId::Bamboo));
        assert_eq!(plan.candidates[0].advances.len(), 2);
    }

    #[test]
    fn uncertain_quest_or_field_never_becomes_a_target() {
        let active = quest(
            "uncertain",
            vec![QuestObjective::Pollen {
                amount: 1,
                field: Some(FieldId::Rose),
                color: None,
            }],
            0.5,
        );
        let plan = QuestPlanner::plan(&QuestPlannerInput {
            active_quests: vec![active],
            fields: vec![field(FieldId::Rose, &[PollenColor::Red], 10_000.0, 1)],
            current_field: None,
            current_field_dwell_seconds: 300,
            item_budgets: BTreeMap::new(),
        });
        assert_eq!(plan.recommended_field, None);
    }

    #[test]
    fn valuable_item_objective_is_held_when_budget_is_zero() {
        let active = quest(
            "items",
            vec![QuestObjective::UseItem {
                item: "glue".to_owned(),
                count: 10,
            }],
            0.99,
        );
        let plan = QuestPlanner::plan(&QuestPlannerInput {
            active_quests: vec![active],
            fields: Vec::new(),
            current_field: None,
            current_field_dwell_seconds: 300,
            item_budgets: BTreeMap::new(),
        });
        assert_eq!(plan.held_objectives.len(), 1);
        assert!(plan.held_objectives[0].reason.contains("budget is zero"));
    }

    #[test]
    fn science_catalog_has_all_31_quests_and_three_translators() {
        let catalog = science_bear_catalog();
        assert_eq!(catalog.quests.len(), 31);
        let translator_sequences = catalog
            .quests
            .iter()
            .filter(|quest| quest.translator_reward)
            .map(|quest| quest.sequence)
            .collect::<Vec<_>>();
        assert_eq!(translator_sequences, vec![21, 26, 31]);
    }

    #[test]
    fn constrained_ocr_tolerates_a_small_typo_but_requires_consensus() {
        let mut consensus = QuestTitleConsensus::default();
        let first = consensus.observe("Science Bear: Mark Mechanlcs", 0.96);
        assert!(first.is_uncertain());
        let second = consensus.observe("Mark Mechanics", 0.98);
        let matched = second.actionable(0.75).expect("two frames agree");
        assert_eq!(matched.id, "science-mark-mechanics");
    }

    #[test]
    fn unknown_quest_text_never_becomes_actionable() {
        let mut consensus = QuestTitleConsensus::default();
        for _ in 0..3 {
            let detection = consensus.observe("Completely Unknown Bear Task", 0.99);
            assert!(detection.actionable(0.0).is_none());
        }
    }
}
