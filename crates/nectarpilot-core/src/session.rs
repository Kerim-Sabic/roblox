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
    /// An allowlisted cooldown-scheduled clock, dispenser, or booster route.
    Collect,
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
    #[error("collect target {0:?} is not an approved clock, dispenser, or booster route")]
    InvalidCollectTarget(String),
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
        let route_file = field_route(&rotation.field)
            .ok_or_else(|| SessionPlanError::InvalidField(rotation.field.clone()))?;
        let route_id = format!("legacy:route:paths/{route_file}");
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

        let pattern_file = pattern_file_name(&rotation.pattern)
            .ok_or_else(|| SessionPlanError::InvalidPattern(rotation.pattern.clone()))?;
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

/// A cooldown-gated collect step plus its persistence key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollectStep {
    pub step: SessionStep,
    /// Runtime-state key holding the RFC3339 time of the last successful run.
    pub last_run_key: String,
    pub cooldown_minutes: u32,
}

/// Builds cooldown-scheduled steps for the explicitly approved clock,
/// dispenser, and field-booster routes from the imported manifest. Valuable
/// or state-changing `gtc-*` routes are deliberately not selectable here.
/// Due-ness is evaluated by the engine at cycle boundaries so a collect run
/// never interrupts gathering mid-pattern.
pub fn build_collect_steps(profile: &Profile) -> Result<Vec<CollectStep>, SessionPlanError> {
    let mut steps = Vec::new();
    for task in &profile.automation.collect {
        let route = collect_route(&task.target)
            .ok_or_else(|| SessionPlanError::InvalidCollectTarget(task.target.clone()))?;
        if !(5..=10_080).contains(&task.cooldown_minutes) {
            return Err(SessionPlanError::InvalidLimits(
                "collect cooldown must be 5..=10080 minutes",
            ));
        }
        let route_id = format!("legacy:route:paths/{}", route.file_name);
        let sha = trusted_digest(profile, &route_id)
            .ok_or_else(|| SessionPlanError::NotTrusted(route_id.clone()))?;
        steps.push(CollectStep {
            step: SessionStep {
                kind: SessionStepKind::Collect,
                script_id: route_id,
                approved_sha256: sha,
                repetitions: 1,
                gather_seconds: None,
                description: format!(
                    "Collect {} (every {} min)",
                    task.target, task.cooldown_minutes
                ),
            },
            last_run_key: format!(
                "profile:{}:collect_last_run:{}",
                profile.id, route.runtime_key
            ),
            cooldown_minutes: task.cooldown_minutes,
        });
    }
    if steps.len() > MAX_STEPS_PER_CYCLE {
        return Err(SessionPlanError::TooManySteps);
    }
    Ok(steps)
}

#[derive(Clone, Copy)]
struct ApprovedCollectRoute {
    /// Exact, case-sensitive file name from `assets/routes/_legacy-manifest.yaml`.
    file_name: &'static str,
    /// Stable lowercase identifier used only inside this profile's runtime key.
    runtime_key: &'static str,
}

/// Maps case-insensitive profile input onto exact manifest casing. This is an
/// allowlist, not a general `gtc-<slug>` constructor: shrine, sticker, blender,
/// honeystorm, pass, memory-match, and seasonal routes can never enter the
/// scheduled collection path through this function.
fn collect_route(target: &str) -> Option<ApprovedCollectRoute> {
    let route = match asset_slug(target).as_str() {
        "clock" => ("gtc-clock.ahk", "clock"),
        "blueberrydis" | "blueberrydispenser" => ("gtc-blueberrydis.ahk", "blueberry-dispenser"),
        "coconutdis" | "coconutdispenser" => ("gtc-coconutdis.ahk", "coconut-dispenser"),
        "gluedis" | "gluedispenser" => ("gtc-gluedis.ahk", "glue-dispenser"),
        "honeydis" | "honeydispenser" => ("gtc-honeydis.ahk", "honey-dispenser"),
        "royaljellydis" | "royaljellydispenser" => {
            ("gtc-royaljellydis.ahk", "royal-jelly-dispenser")
        }
        "strawberrydis" | "strawberrydispenser" => {
            ("gtc-strawberrydis.ahk", "strawberry-dispenser")
        }
        "treatdis" | "treatdispenser" => ("gtc-treatdis.ahk", "treat-dispenser"),
        "blue" | "bluebooster" | "bluefieldbooster" => ("gtb-blue.ahk", "blue-booster"),
        "red" | "redbooster" | "redfieldbooster" => ("gtb-red.ahk", "red-booster"),
        "mountain" | "mountainbooster" | "mountaintopbooster" => {
            ("gtb-mountain.ahk", "mountain-booster")
        }
        _ => return None,
    };
    Some(ApprovedCollectRoute {
        file_name: route.0,
        runtime_key: route.1,
    })
}

fn trusted_digest(profile: &Profile, asset_id: &str) -> Option<String> {
    profile.trusted_extensions.get(asset_id).cloned()
}

/// Maps the display names accepted by the desktop and legacy INI importer to
/// the exact route file names pinned in the bundled manifest. This is an
/// allowlist rather than a general `gtf-<slug>` constructor so a profile can
/// never turn arbitrary text into an executable path.
fn field_route(field: &str) -> Option<&'static str> {
    match asset_slug(field).as_str() {
        "bamboo" | "bamboofield" => Some("gtf-bamboo.ahk"),
        "blueflower" | "blueflowerfield" => Some("gtf-blueflower.ahk"),
        "cactus" | "cactusfield" => Some("gtf-cactus.ahk"),
        "clover" | "cloverfield" => Some("gtf-clover.ahk"),
        "coconut" | "coconutfield" => Some("gtf-coconut.ahk"),
        "dandelion" | "dandelionfield" => Some("gtf-dandelion.ahk"),
        "mountaintop" | "mountaintopfield" => Some("gtf-mountaintop.ahk"),
        "mushroom" | "mushroomfield" => Some("gtf-mushroom.ahk"),
        "pepper" | "pepperpatch" | "pepperfield" => Some("gtf-pepper.ahk"),
        "pineapple" | "pineapplepatch" | "pineapplefield" => Some("gtf-pineapple.ahk"),
        "pinetree" | "pinetreeforest" | "pineforest" => Some("gtf-pinetree.ahk"),
        "pumpkin" | "pumpkinpatch" | "pumpkinfield" => Some("gtf-pumpkin.ahk"),
        "rose" | "rosefield" => Some("gtf-rose.ahk"),
        "spider" | "spiderfield" => Some("gtf-spider.ahk"),
        "strawberry" | "strawberryfield" => Some("gtf-strawberry.ahk"),
        "stump" | "stumpfield" => Some("gtf-stump.ahk"),
        "sunflower" | "sunflowerfield" => Some("gtf-sunflower.ahk"),
        _ => None,
    }
}

/// Returns whether a display field name resolves to one of the exact bundled
/// legacy gather routes. Desktop forms use this to reject a typo before it can
/// overwrite a profile; execution performs the same check again.
#[must_use]
pub fn is_supported_legacy_field(field: &str) -> bool {
    field_route(field).is_some()
}

/// `"Blue Flower"` -> `blueflower`, matching Natro's `StrReplace(name, " ")`.
fn asset_slug(name: &str) -> String {
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

/// Maps friendly pattern names to the exact legacy bridge file names. The
/// converted `Stationary` asset intentionally does not appear here: it is a
/// native-preview DSL asset and cannot be sent to the compatibility worker.
fn pattern_file_name(name: &str) -> Option<&'static str> {
    match asset_slug(name).as_str() {
        "auryn" => Some("Auryn"),
        "cornerxsnake" => Some("CornerXSnake"),
        "diamonds" => Some("Diamonds"),
        "elol" => Some("e_lol"),
        "fork" => Some("Fork"),
        "lines" => Some("Lines"),
        "slimline" => Some("Slimline"),
        "snake" => Some("Snake"),
        "squares" => Some("Squares"),
        "supercat" => Some("SuperCat"),
        "xsnake" => Some("XSnake"),
        _ => None,
    }
}

/// Returns whether a pattern resolves to an executable legacy bridge asset.
/// The safe-DSL Stationary preview deliberately returns false until a native
/// executor is installed.
#[must_use]
pub fn is_supported_legacy_pattern(pattern: &str) -> bool {
    pattern_file_name(pattern).is_some()
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
    fn field_aliases_resolve_to_exact_pinned_route_names() {
        let manifest = include_str!("../../../assets/routes/_legacy-manifest.yaml");
        for (label, file) in [
            ("Bamboo Field", "gtf-bamboo.ahk"),
            ("Blue Flower Field", "gtf-blueflower.ahk"),
            ("Cactus Field", "gtf-cactus.ahk"),
            ("Clover Field", "gtf-clover.ahk"),
            ("Coconut Field", "gtf-coconut.ahk"),
            ("Dandelion Field", "gtf-dandelion.ahk"),
            ("Mountain Top Field", "gtf-mountaintop.ahk"),
            ("Mushroom Field", "gtf-mushroom.ahk"),
            ("Pepper Patch", "gtf-pepper.ahk"),
            ("Pineapple Patch", "gtf-pineapple.ahk"),
            ("Pine Tree Forest", "gtf-pinetree.ahk"),
            ("Pumpkin Patch", "gtf-pumpkin.ahk"),
            ("Rose Field", "gtf-rose.ahk"),
            ("Spider Field", "gtf-spider.ahk"),
            ("Strawberry Field", "gtf-strawberry.ahk"),
            ("Stump Field", "gtf-stump.ahk"),
            ("Sunflower Field", "gtf-sunflower.ahk"),
        ] {
            assert_eq!(field_route(label), Some(file));
            assert!(manifest.contains(&format!("legacy_source: paths/{file}")));
        }
        assert_eq!(field_route("Pine Tree"), Some("gtf-pinetree.ahk"));
        assert_eq!(field_route("not a field"), None);
    }

    #[test]
    fn pattern_aliases_use_exact_legacy_bridge_names() {
        assert_eq!(pattern_file_name("cornerxsnake"), Some("CornerXSnake"));
        assert_eq!(pattern_file_name("e_lol"), Some("e_lol"));
        assert_eq!(pattern_file_name("SuperCat"), Some("SuperCat"));
        assert_eq!(pattern_file_name("Stationary"), None);
    }

    #[test]
    fn collect_steps_are_profile_scoped_trusted_and_bounded() {
        let mut profile = profile_with_rotation();
        profile.automation.collect = vec![nectarpilot_contracts::CollectTask {
            target: "Clock".into(),
            cooldown_minutes: 240,
        }];
        // Untrusted route blocks the whole plan.
        assert!(matches!(
            build_collect_steps(&profile),
            Err(SessionPlanError::NotTrusted(id)) if id.contains("gtc-clock")
        ));

        profile
            .trusted_extensions
            .insert("legacy:route:paths/gtc-clock.ahk".into(), "c".repeat(64));
        let steps = build_collect_steps(&profile).expect("trusted collect plan");
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].step.kind, SessionStepKind::Collect);
        assert_eq!(steps[0].step.script_id, "legacy:route:paths/gtc-clock.ahk");
        assert_eq!(
            steps[0].last_run_key,
            format!("profile:{}:collect_last_run:clock", profile.id)
        );

        let mut other_profile = profile.clone();
        other_profile.id = uuid::Uuid::now_v7();
        let other_steps = build_collect_steps(&other_profile).expect("other profile plan");
        assert_ne!(steps[0].last_run_key, other_steps[0].last_run_key);

        profile.automation.collect[0].cooldown_minutes = 1;
        assert!(matches!(
            build_collect_steps(&profile),
            Err(SessionPlanError::InvalidLimits(_))
        ));
    }

    #[test]
    fn collect_allowlist_uses_exact_manifest_casing_and_rejects_valuable_routes() {
        let manifest = include_str!("../../../assets/routes/_legacy-manifest.yaml");
        for target in [
            "clock",
            "blueberrydis",
            "coconutdis",
            "gluedis",
            "honeydis",
            "royaljellydis",
            "strawberrydis",
            "treatdis",
            "blue",
            "red",
            "mountain",
        ] {
            let route = collect_route(target).expect("approved fixture target");
            assert!(
                manifest.contains(&format!("legacy_source: paths/{}", route.file_name)),
                "approved route {} must exist with exact casing in the manifest",
                route.file_name
            );
        }

        let mut profile = profile_with_rotation();
        profile.automation.collect = vec![nectarpilot_contracts::CollectTask {
            target: "BLUE Field Booster".into(),
            cooldown_minutes: 60,
        }];
        profile
            .trusted_extensions
            .insert("legacy:route:paths/gtb-blue.ahk".into(), "c".repeat(64));

        let steps = build_collect_steps(&profile).expect("case-safe booster mapping");
        assert_eq!(steps[0].step.script_id, "legacy:route:paths/gtb-blue.ahk");
        assert!(steps[0].step.description.contains("BLUE Field Booster"));

        for blocked in ["WindShrine", "stickerPrinter", "blender", "honeystorm"] {
            profile.automation.collect[0].target = blocked.into();
            profile.trusted_extensions.insert(
                format!("legacy:route:paths/gtc-{blocked}.ahk"),
                "d".repeat(64),
            );
            assert!(matches!(
                build_collect_steps(&profile),
                Err(SessionPlanError::InvalidCollectTarget(target)) if target == blocked
            ));
        }
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
