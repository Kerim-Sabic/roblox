//! Orchestrated legacy gather sessions.
//!
//! A session turns the profile's field rotation into a bounded plan of
//! individually trusted legacy steps: travel to the field, run the gather
//! pattern, then reset to the hive and convert — the same loop the legacy
//! macro ran, but supervised step-by-step by the engine with cancellation,
//! pause, and per-step outcomes.

use nectarpilot_contracts::Profile;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Identifier the compatibility port recognizes as its own generated
/// reset-and-convert script rather than a manifest asset. It contains no user
/// content: only the pinned Natro support code the harness always embeds.
pub const BUILTIN_RESET_SCRIPT_ID: &str = "builtin:reset-convert";
/// Consent marker for builtin steps; manifest assets always use real digests.
pub const BUILTIN_APPROVAL: &str = "builtin";

pub const MAX_SESSION_CYCLES: u32 = 100;
pub const MAX_SESSION_MINUTES: u32 = 720;
pub const MAX_STEPS_PER_CYCLE: usize = 64;
pub const MAX_GATHER_SECONDS: u32 = 24 * 60 * 60;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStepKind {
    Travel,
    Gather,
    ResetConvert,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionStep {
    pub kind: SessionStepKind,
    pub script_id: String,
    pub approved_sha256: String,
    /// How many times the step's script runs back-to-back.
    pub repetitions: u16,
    /// Gather steps repeat until this bounded field duration expires. Other
    /// step kinds use the exact repetition count above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gather_seconds: Option<u32>,
    pub description: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SessionPlanError {
    #[error("the profile has no field rotations configured")]
    EmptyRotation,
    #[error("rotation field name {0:?} cannot be mapped to a route asset")]
    InvalidField(String),
    #[error("rotation pattern name {0:?} cannot be mapped to a pattern asset")]
    InvalidPattern(String),
    #[error(
        "rotation field {field:?} has invalid gather duration {seconds}; expected 1..={MAX_GATHER_SECONDS} seconds"
    )]
    InvalidGatherDuration { field: String, seconds: u32 },
    #[error("asset {0} is not hash-trusted in this profile; review it on the Extensions page")]
    NotTrusted(String),
    #[error("session limits are out of bounds: {0}")]
    InvalidLimits(&'static str),
    #[error("a session cycle may contain at most {MAX_STEPS_PER_CYCLE} steps")]
    TooManySteps,
}

/// Validates the requested bounds against the hard session limits.
pub fn validate_session_limits(max_cycles: u32, max_minutes: u32) -> Result<(), SessionPlanError> {
    if max_cycles == 0 || max_cycles > MAX_SESSION_CYCLES {
        return Err(SessionPlanError::InvalidLimits("cycles must be 1..=100"));
    }
    if !(5..=MAX_SESSION_MINUTES).contains(&max_minutes) {
        return Err(SessionPlanError::InvalidLimits("minutes must be 5..=720"));
    }
    Ok(())
}

/// Builds one cycle's step list from the profile rotation. Every referenced
/// manifest asset must already be hash-trusted in the profile; the returned
/// digests are the profile's own consent records, which the port re-checks.
pub fn build_session_plan(profile: &Profile) -> Result<Vec<SessionStep>, SessionPlanError> {
    let rotations = &profile.automation.rotations;
    if rotations.is_empty() {
        return Err(SessionPlanError::EmptyRotation);
    }
    let mut steps = Vec::new();
    for rotation in rotations {
        if !(1..=MAX_GATHER_SECONDS).contains(&rotation.gather_seconds) {
            return Err(SessionPlanError::InvalidGatherDuration {
                field: rotation.field.clone(),
                seconds: rotation.gather_seconds,
            });
        }
        let field_slug = asset_slug(&rotation.field);
        if field_slug.is_empty() || field_slug.len() > 32 {
            return Err(SessionPlanError::InvalidField(rotation.field.clone()));
        }
        let route_id = format!("legacy:route:paths/gtf-{field_slug}.ahk");
        let route_sha = trusted_digest(profile, &route_id)
            .ok_or_else(|| SessionPlanError::NotTrusted(route_id.clone()))?;
        steps.push(SessionStep {
            kind: SessionStepKind::Travel,
            script_id: route_id,
            approved_sha256: route_sha,
            repetitions: 1,
            gather_seconds: None,
            description: format!("Travel to {}", rotation.field),
        });

        let pattern_file = pattern_file_name(&rotation.pattern);
        if pattern_file.is_empty() || pattern_file.len() > 48 {
            return Err(SessionPlanError::InvalidPattern(rotation.pattern.clone()));
        }
        let pattern_id = format!("legacy:pattern:patterns/{pattern_file}.ahk");
        let (pattern_id, pattern_sha) = if let Some(sha) = trusted_digest(profile, &pattern_id) {
            (pattern_id, sha)
        } else {
            // Trusted keys carry the manifest's exact casing; retry with a
            // case-insensitive match before failing.
            profile
                .trusted_extensions
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(&pattern_id))
                .map(|(key, sha)| (key.clone(), sha.clone()))
                .ok_or(SessionPlanError::NotTrusted(pattern_id))?
        };
        let repetitions = rotation.repetitions.clamp(1, 50);
        steps.push(SessionStep {
            kind: SessionStepKind::Gather,
            script_id: pattern_id,
            approved_sha256: pattern_sha,
            repetitions,
            gather_seconds: Some(rotation.gather_seconds),
            description: format!(
                "Gather {} in {} for {} seconds",
                rotation.pattern, rotation.field, rotation.gather_seconds
            ),
        });

        steps.push(SessionStep {
            kind: SessionStepKind::ResetConvert,
            script_id: BUILTIN_RESET_SCRIPT_ID.to_owned(),
            approved_sha256: BUILTIN_APPROVAL.to_owned(),
            repetitions: 1,
            gather_seconds: None,
            description: "Reset to hive and convert".to_owned(),
        });
    }
    if steps.len() > MAX_STEPS_PER_CYCLE {
        return Err(SessionPlanError::TooManySteps);
    }
    Ok(steps)
}

fn trusted_digest(profile: &Profile, asset_id: &str) -> Option<String> {
    profile.trusted_extensions.get(asset_id).cloned()
}

/// `"Blue Flower"` -> `blueflower`, matching Natro's `StrReplace(name, " ")`.
fn asset_slug(name: &str) -> String {
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

/// Pattern files keep their imported casing (`Snake.ahk`, `e_lol.ahk`); keep
/// the name as-is minus anything path-like.
fn pattern_file_name(name: &str) -> String {
    name.chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '_')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nectarpilot_contracts::FieldRotation;

    fn profile_with_rotation() -> Profile {
        let mut profile = Profile::new("session");
        profile.automation.rotations = vec![FieldRotation {
            field: "Blue Flower".into(),
            pattern: "Snake".into(),
            gather_seconds: 600,
            repetitions: 3,
        }];
        profile.trusted_extensions.insert(
            "legacy:route:paths/gtf-blueflower.ahk".into(),
            "a".repeat(64),
        );
        profile
            .trusted_extensions
            .insert("legacy:pattern:patterns/Snake.ahk".into(), "b".repeat(64));
        profile
    }

    #[test]
    fn plan_expands_rotation_into_travel_gather_reset() {
        let plan = build_session_plan(&profile_with_rotation()).expect("plan");
        assert_eq!(plan.len(), 3);
        assert_eq!(plan[0].kind, SessionStepKind::Travel);
        assert_eq!(plan[0].script_id, "legacy:route:paths/gtf-blueflower.ahk");
        assert_eq!(plan[0].gather_seconds, None);
        assert_eq!(plan[1].kind, SessionStepKind::Gather);
        assert_eq!(plan[1].repetitions, 3);
        assert_eq!(plan[1].gather_seconds, Some(600));
        assert_eq!(plan[2].script_id, BUILTIN_RESET_SCRIPT_ID);
        assert_eq!(plan[2].gather_seconds, None);
    }

    #[test]
    fn untrusted_assets_block_the_whole_plan() {
        let mut profile = profile_with_rotation();
        profile
            .trusted_extensions
            .remove("legacy:pattern:patterns/Snake.ahk");
        assert!(matches!(
            build_session_plan(&profile),
            Err(SessionPlanError::NotTrusted(id)) if id.contains("Snake")
        ));
    }

    #[test]
    fn pattern_trust_matches_case_insensitively() {
        let mut profile = profile_with_rotation();
        profile.automation.rotations[0].pattern = "snake".into();
        let plan = build_session_plan(&profile).expect("plan");
        assert_eq!(plan[1].script_id, "legacy:pattern:patterns/Snake.ahk");
    }

    #[test]
    fn limits_are_bounded() {
        assert!(validate_session_limits(10, 120).is_ok());
        assert!(validate_session_limits(0, 120).is_err());
        assert!(validate_session_limits(101, 120).is_err());
        assert!(validate_session_limits(10, 4).is_err());
        assert!(validate_session_limits(10, 721).is_err());
    }

    #[test]
    fn zero_gather_duration_is_rejected_without_clamping() {
        let mut profile = profile_with_rotation();
        profile.automation.rotations[0].gather_seconds = 0;

        assert!(matches!(
            build_session_plan(&profile),
            Err(SessionPlanError::InvalidGatherDuration { seconds: 0, .. })
        ));
    }
}
