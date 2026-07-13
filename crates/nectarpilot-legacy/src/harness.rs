//! Generates the complete `AutoHotkey` v2 environment a legacy movement
//! fragment needs, exactly the way Natro Macro v1.1.2 did.
//!
//! Natro never executed `paths/*.ahk` or `patterns/*.ahk` on their own: its
//! `nm_createWalk` wrapped each fragment in a generated walk script that
//! defines the movement keys, `nm_Walk`, `Walk` movespeed correction,
//! `nm_gotoRamp`/`nm_gotoCannon`/`nm_Reset`, and Gdip/Roblox library includes,
//! then piped that script to `AutoHotkey64.exe`. Running a bare fragment fails
//! at load with "call to nonexistent function". This module reproduces that
//! wrapper deterministically so every manifest-pinned route and pattern runs
//! with legacy-faithful behavior under the same containment and consent gates.

use std::fmt::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Natro's `nm_PathVars` body (goto-ramp, goto-cannon, and reset support),
/// extracted verbatim from `natro_macro.ahk` v1.1.2 with only the four
/// configuration interpolations and the script-directory reference replaced by
/// placeholders.
const ROUTE_CONTEXT_TEMPLATE: &str = include_str!("../support/route_context.ahk");

/// Watchdog ceiling stays under the runner's 30-minute hard kill so a hung
/// fragment exits through the script's own cleanup path first.
const MAX_WATCHDOG_SECONDS: u32 = 25 * 60;
const DEFAULT_WATCHDOG_SECONDS: u32 = 15 * 60;

/// Exit code the generated watchdog uses so a timeout is distinguishable from
/// fragment success (0) and interpreter load errors (2).
pub const WATCHDOG_EXIT_CODE: i32 = 86;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MoveMethod {
    Walk,
    #[default]
    Cannon,
}

impl MoveMethod {
    const fn legacy_name(self) -> &'static str {
        match self {
            Self::Walk => "walk",
            Self::Cannon => "cannon",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatternSize {
    ExtraSmall,
    Small,
    #[default]
    Medium,
    Large,
    ExtraLarge,
}

impl PatternSize {
    const fn multiplier(self) -> &'static str {
        match self {
            Self::ExtraSmall => "0.25",
            Self::Small => "0.5",
            Self::Medium => "1",
            Self::Large => "1.5",
            Self::ExtraLarge => "2",
        }
    }
}

/// Gather-pattern options mirroring Natro's `nm_gather` variables.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PatternSettings {
    pub size: PatternSize,
    pub repetitions: u8,
    pub facing_corner: bool,
    pub invert_forward_back: bool,
    pub invert_left_right: bool,
}

impl Default for PatternSettings {
    fn default() -> Self {
        Self {
            size: PatternSize::Medium,
            repetitions: 1,
            facing_corner: false,
            invert_forward_back: false,
            invert_left_right: false,
        }
    }
}

/// Movement environment mirroring Natro's stock `nm_config.ini` defaults.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HarnessSettings {
    pub move_method: MoveMethod,
    /// Hive slot 1..=6, exactly as the legacy `HiveSlot` setting.
    pub hive_slot: u8,
    /// Bees in the hive 0..=50; the legacy reset path uses this for waits.
    pub hive_bees: u8,
    /// Extra per-key send delay in milliseconds, 0..=1000.
    pub key_delay: u16,
    /// The exact in-game movespeed shown by Roblox, 10.0..=200.0.
    pub move_speed: f64,
    /// `true` uses the buff-corrected `Walk.ahk` timing (Natro `NewWalk`).
    pub new_walk: bool,
    /// Script-internal timeout in seconds, 30..=1500.
    pub watchdog_seconds: u32,
    pub pattern: PatternSettings,
}

impl Default for HarnessSettings {
    fn default() -> Self {
        Self {
            move_method: MoveMethod::Cannon,
            hive_slot: 6,
            hive_bees: 50,
            key_delay: 20,
            move_speed: 28.0,
            new_walk: true,
            watchdog_seconds: DEFAULT_WATCHDOG_SECONDS,
            pattern: PatternSettings::default(),
        }
    }
}

impl HarnessSettings {
    fn validate(&self) -> Result<(), HarnessError> {
        if !(1..=6).contains(&self.hive_slot) {
            return Err(HarnessError::InvalidSetting("hive_slot must be 1..=6"));
        }
        if self.hive_bees > 50 {
            return Err(HarnessError::InvalidSetting("hive_bees must be 0..=50"));
        }
        if self.key_delay > 1000 {
            return Err(HarnessError::InvalidSetting("key_delay must be 0..=1000"));
        }
        if !self.move_speed.is_finite() || !(10.0..=200.0).contains(&self.move_speed) {
            return Err(HarnessError::InvalidSetting(
                "move_speed must be 10.0..=200.0",
            ));
        }
        if !(30..=MAX_WATCHDOG_SECONDS).contains(&self.watchdog_seconds) {
            return Err(HarnessError::InvalidSetting(
                "watchdog_seconds must be 30..=1500",
            ));
        }
        if !(1..=10).contains(&self.pattern.repetitions) {
            return Err(HarnessError::InvalidSetting(
                "pattern repetitions must be 1..=10",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum HarnessError {
    #[error("harness setting out of bounds: {0}")]
    InvalidSetting(&'static str),
    #[error("legacy root path cannot be embedded in a script: {0}")]
    UnsafeRoot(String),
    #[error("legacy fragment cannot be embedded in a script: {0}")]
    UnsafeFragment(String),
}

/// Which legacy environment the fragment expects.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FragmentKind {
    /// `paths/*.ahk`: travel routes that may call `nm_gotoramp`/`nm_gotocannon`.
    Route,
    /// `patterns/*.ahk`: gather loops driven by `size`/`reps`/TC-AFC keys.
    Pattern,
}

/// Builds the full standalone walk script for one pinned fragment.
///
/// `legacy_root` must be the canonical compatibility root that contains `lib`,
/// `nm_image_assets`, `paths`, and `patterns`. The fragment source is embedded
/// exactly as Natro embedded it (inside the generated `start()` body), so
/// fragment-local functions such as `gtc-nightmm.ahk`'s `Jump` keep working.
pub fn generate_walk_script(
    legacy_root: &Path,
    kind: FragmentKind,
    fragment_source: &str,
    settings: &HarnessSettings,
) -> Result<String, HarnessError> {
    generate_script(legacy_root, kind, fragment_source, settings, &[])
}

/// Builds the orchestrator's reset-and-convert step: respawn via Natro's own
/// `nm_Reset`, walk the ramp to the hive slot, press E only when the
/// Make Honey prompt template is actually visible (mirroring `nm_convert`),
/// then wait a bounded conversion window. The movement is generated here —
/// it contains no user script content — and runs under the same pinned
/// support files, containment, and safety gates as every other harness.
pub fn generate_reset_script(
    legacy_root: &Path,
    settings: &HarnessSettings,
    convert_wait_seconds: u16,
) -> Result<String, HarnessError> {
    let wait_ms = u32::from(convert_wait_seconds.clamp(1, 600)) * 1000;
    let movement = format!(
        "nm_Reset()\n\
         nm_gotoRamp()\n\
         GetRobloxClientPos()\n\
         pBMConvert := Gdip_BitmapFromScreen(windowX+windowWidth//2-200 \"|\" windowY+offsetY+36 \"|400|120\")\n\
         if (Gdip_ImageSearch(pBMConvert, bitmaps[\"makehoney\"], , , , , , 2, , 2) = 1)\n\
         {{\n\
         \tSend \"{{\" SC_E \" down}}\"\n\
         \tSleep 100\n\
         \tSend \"{{\" SC_E \" up}}\"\n\
         }}\n\
         Gdip_DisposeImage(pBMConvert)\n\
         HyperSleep({wait_ms})\n"
    );
    generate_script(
        legacy_root,
        FragmentKind::Route,
        &movement,
        settings,
        &[
            "nm_image_assets\\general\\bitmaps.ahk",
            "nm_image_assets\\convert\\bitmaps.ahk",
        ],
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "the emitter mirrors the generated script's top-to-bottom section order, which keeps it reviewable against nm_createWalk"
)]
fn generate_script(
    legacy_root: &Path,
    kind: FragmentKind,
    fragment_source: &str,
    settings: &HarnessSettings,
    extra_bitmap_includes: &[&str],
) -> Result<String, HarnessError> {
    settings.validate()?;
    let root = embeddable_root(legacy_root)?;
    let fragment = embeddable_fragment(fragment_source)?;

    let mut script = String::with_capacity(fragment.len() + 8 * 1024);
    script.push_str(
        "; NectarPilot legacy compatibility harness (generated per run; do not edit)\n\
         ; Reproduces Natro Macro v1.1.2 nm_createWalk for one manifest-pinned fragment.\n\
         #Requires AutoHotkey v2.0\n\
         #SingleInstance Off\n\
         #NoTrayIcon\n\
         ProcessSetPriority(\"AboveNormal\")\n\
         KeyHistory 0\n\
         ListLines 0\n\
         OnExit(ExitFunc)\n\n",
    );
    let _ = writeln!(script, "#Include \"{root}\\lib\\Gdip_All.ahk\"");
    let _ = writeln!(script, "#Include \"{root}\\lib\\Gdip_ImageSearch.ahk\"");
    let _ = writeln!(script, "#Include \"{root}\\lib\\HyperSleep.ahk\"");
    let _ = writeln!(script, "#Include \"{root}\\lib\\Roblox.ahk\"");
    script.push('\n');

    if settings.new_walk {
        let _ = writeln!(script, "#Include \"{root}\\lib\\Walk.ahk\"");
        let _ = writeln!(
            script,
            "\nmovespeed := {speed}\n\
             both            := (Mod(movespeed*1000, 1265) = 0) || (Mod(Round((movespeed+0.005)*1000), 1265) = 0)\n\
             hasty_guard     := (both || Mod(movespeed*1000, 1100) < 0.00001)\n\
             gifted_hasty    := (both || Mod(movespeed*1000, 1150) < 0.00001)\n\
             base_movespeed  := round(movespeed / (both ? 1.265 : (hasty_guard ? 1.1 : (gifted_hasty ? 1.15 : 1))), 0)",
            speed = legacy_number(settings.move_speed)
        );
    } else {
        let _ = writeln!(
            script,
            "(bitmaps := Map()).CaseSense := 0\n\
             pToken := Gdip_Startup()\n\
             Walk(param, *) => HyperSleep(4000/{speed}*param)",
            speed = legacy_number(settings.move_speed)
        );
    }

    // The offset bitmaps let GetYOffset locate the Roblox top bar exactly as
    // the legacy macro did; on failure it degrades to Natro's 0 default.
    let _ = writeln!(
        script,
        "\n#Include \"{root}\\nm_image_assets\\offset\\bitmaps.ahk\""
    );
    if !extra_bitmap_includes.is_empty() {
        // general/bitmaps.ahk also populates the Shrine icon map the main
        // macro declared; declare it here so the include loads standalone.
        let _ = writeln!(script, "(Shrine := Map()).CaseSense := 0");
    }
    for include in extra_bitmap_includes {
        let _ = writeln!(script, "#Include \"{root}\\{include}\"");
    }
    let _ = writeln!(script, "offsetY := GetYOffset()\n");

    script.push_str(&key_variables(settings));
    script.push('\n');

    match kind {
        FragmentKind::Route => script.push_str(&route_context(&root, settings)),
        FragmentKind::Pattern => script.push_str(&pattern_context(settings)),
    }

    let _ = writeln!(
        script,
        "\nSetTimer(np_Watchdog, -{})\n\nstart()\nExitApp 0\n",
        u64::from(settings.watchdog_seconds) * 1000
    );

    script.push_str(
        "nm_Walk(tiles, MoveKey1, MoveKey2:=0)\n\
         {\n\
         \tSend \"{\" MoveKey1 \" down}\" (MoveKey2 ? \"{\" MoveKey2 \" down}\" : \"\")\n\
         \tWalk(tiles)\n\
         \tSend \"{\" MoveKey1 \" up}\" (MoveKey2 ? \"{\" MoveKey2 \" up}\" : \"\")\n\
         }\n\n\
         start()\n\
         {\n\
         \tSend \"{F14 down}\"\n",
    );
    script.push_str(&fragment);
    if !fragment.ends_with('\n') {
        script.push('\n');
    }
    script.push_str(
        "\tSend \"{F14 up}\"\n\
         }\n\n",
    );

    let _ = writeln!(
        script,
        "np_Watchdog()\n{{\n\tExitApp {WATCHDOG_EXIT_CODE}\n}}"
    );

    // F16 pause/resume parity with Natro's generated walk scripts.
    script.push_str(
        "\nF16::\n\
         {\n\
         \tstatic key_states := Map(LeftKey,0, RightKey,0, FwdKey,0, BackKey,0, \"LButton\",0, \"RButton\",0, SC_E,0)\n\
         \tif A_IsPaused\n\
         \t{\n\
         \t\tfor k,v in key_states\n\
         \t\t\tif (v = 1)\n\
         \t\t\t\tSend \"{\" k \" down}\"\n\
         \t}\n\
         \telse\n\
         \t{\n\
         \t\tfor k,v in key_states\n\
         \t\t{\n\
         \t\t\tkey_states[k] := GetKeyState(k)\n\
         \t\t\tSend \"{\" k \" up}\"\n\
         \t\t}\n\
         \t}\n\
         \tPause -1\n\
         }\n\n\
         ExitFunc(*)\n\
         {\n\
         \tSend \"{\" LeftKey \" up}{\" RightKey \" up}{\" FwdKey \" up}{\" BackKey \" up}{\" SC_Space \" up}{F14 up}{\" SC_E \" up}\"\n\
         \ttry Gdip_Shutdown(pToken)\n\
         }\n",
    );

    debug_assert!(!script.contains("{{"), "all placeholders must be resolved");
    Ok(script)
}

/// Natro's stock scan-code key bindings, with the toward-center/away pairs
/// applied from the pattern inversion settings the way `nm_gather` swaps them.
fn key_variables(settings: &HarnessSettings) -> String {
    let (tcfb, afcfb) = if settings.pattern.invert_forward_back {
        ("BackKey", "FwdKey")
    } else {
        ("FwdKey", "BackKey")
    };
    let (tclr, afclr) = if settings.pattern.invert_left_right {
        ("RightKey", "LeftKey")
    } else {
        ("LeftKey", "RightKey")
    };
    format!(
        "FwdKey:=\"sc011\" ; w\n\
         LeftKey:=\"sc01e\" ; a\n\
         BackKey:=\"sc01f\" ; s\n\
         RightKey:=\"sc020\" ; d\n\
         TCFBKey:={tcfb}\n\
         TCLRKey:={tclr}\n\
         AFCFBKey:={afcfb}\n\
         AFCLRKey:={afclr}\n\
         RotLeft:=\"sc033\" ; ,\n\
         RotRight:=\"sc034\" ; .\n\
         RotUp:=\"sc149\" ; PgUp\n\
         RotDown:=\"sc151\" ; PgDn\n\
         ZoomIn:=\"sc017\" ; i\n\
         ZoomOut:=\"sc018\" ; o\n\
         SC_E:=\"sc012\" ; e\n\
         SC_R:=\"sc013\" ; r\n\
         SC_L:=\"sc026\" ; l\n\
         SC_Esc:=\"sc001\" ; Esc\n\
         SC_Enter:=\"sc01c\" ; Enter\n\
         SC_LShift:=\"sc02a\" ; LShift\n\
         SC_Space:=\"sc039\" ; Space\n\
         SC_1:=\"sc002\" ; 1\n"
    )
}

fn route_context(root: &str, settings: &HarnessSettings) -> String {
    ROUTE_CONTEXT_TEMPLATE
        .replace("{{HIVE_SLOT}}", &settings.hive_slot.to_string())
        .replace("{{MOVE_METHOD}}", settings.move_method.legacy_name())
        .replace("{{HIVE_BEES}}", &settings.hive_bees.to_string())
        .replace("{{KEY_DELAY}}", &settings.key_delay.to_string())
        .replace("{{LEGACY_ROOT}}", root)
}

/// The gather environment Natro passes into pattern walk scripts, including
/// its camera-rotation helper (verbatim from `nm_gather`).
fn pattern_context(settings: &HarnessSettings) -> String {
    format!(
        "size:={size}\n\
         reps:={reps}\n\
         facingcorner:={facing}\n\
         \n\
         FieldName:=\"\"\n\
         FieldPattern:=\"\"\n\
         FieldPatternSize:=\"\"\n\
         FieldReturnType:=\"\"\n\
         FieldSprinklerLoc:=\"\"\n\
         FieldRotateDirection:=\"\"\n\
         FieldUntilPack:=0\n\
         FieldPatternReps:={reps}\n\
         FieldPatternShift:=0\n\
         FieldSprinklerDist:=0\n\
         FieldRotateTimes:=0\n\
         FieldDriftCheck:=0\n\
         FieldPatternInvertFB:={invert_fb}\n\
         FieldPatternInvertLR:={invert_lr}\n\
         FieldUntilMins:=0\n\
         KeyDelay:={key_delay}\n\
         \n\
         CoordMode \"Mouse\", \"Screen\"\n\
         CoordMode \"Pixel\", \"Screen\"\n\
         \n\
         nm_CameraRotation(Dir, count) {{\n\
         \tStatic LR := 0, UD := 0, init := OnExit((*) => send(\"{{\" Rot%(LR > 0 ? \"Left\" : \"Right\")% \" \" Mod(Abs(LR), 8) \"}}{{\" Rot%(UD > 0 ? \"Up\" : \"Down\")% \" \" Abs(UD) \"}}\"), -1)\n\
         \tsend \"{{\" Rot%Dir% \" \" count \"}}\"\n\
         \tSwitch Dir,0 {{\n\
         \t\tCase \"Left\": LR -= count\n\
         \t\tCase \"Right\": LR += count\n\
         \t\tCase \"Up\": UD -= count\n\
         \t\tCase \"Down\": UD += count\n\
         \t}}\n\
         }}\n",
        size = settings.pattern.size.multiplier(),
        reps = settings.pattern.repetitions,
        facing = u8::from(settings.pattern.facing_corner),
        invert_fb = u8::from(settings.pattern.invert_forward_back),
        invert_lr = u8::from(settings.pattern.invert_left_right),
        key_delay = settings.key_delay,
    )
}

/// Formats a float without scientific notation or a trailing dot, matching how
/// the legacy INI stored movespeed values.
fn legacy_number(value: f64) -> String {
    let mut text = format!("{value:.4}");
    while text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    text
}

/// The root is spliced into `#Include "..."` directives; reject anything the
/// `AutoHotkey` pre-parser could reinterpret rather than trying to escape it.
fn embeddable_root(legacy_root: &Path) -> Result<String, HarnessError> {
    let Some(text) = legacy_root.to_str() else {
        return Err(HarnessError::UnsafeRoot(
            "root path is not valid Unicode".to_owned(),
        ));
    };
    // fs::canonicalize on Windows yields \\?\C:\...; AutoHotkey and users read
    // plain drive paths, so strip the extended-length prefix when present.
    let text = text.strip_prefix(r"\\?\").unwrap_or(text);
    if text.is_empty() || text.len() > 240 {
        return Err(HarnessError::UnsafeRoot(
            "root path is empty or too long".to_owned(),
        ));
    }
    let forbidden = ['"', '%', ';', '`', '*', '?', '\n', '\r', '\t'];
    if text
        .chars()
        .any(|character| forbidden.contains(&character) || character.is_control())
    {
        return Err(HarnessError::UnsafeRoot(format!(
            "root path {text:?} contains characters that cannot be embedded safely"
        )));
    }
    Ok(text.trim_end_matches(['\\', '/']).to_owned())
}

/// The fragment is already pinned by manifest hash and explicit consent; this
/// only normalizes encoding artifacts so splicing matches Natro's `FileRead`.
fn embeddable_fragment(source: &str) -> Result<String, HarnessError> {
    let text = source.trim_start_matches('\u{feff}');
    if text.trim().is_empty() {
        return Err(HarnessError::UnsafeFragment(
            "fragment is empty after decoding".to_owned(),
        ));
    }
    if text.contains('\u{0}') {
        return Err(HarnessError::UnsafeFragment(
            "fragment contains NUL bytes".to_owned(),
        ));
    }
    Ok(text.replace("\r\n", "\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn root() -> PathBuf {
        PathBuf::from(r"C:\NectarPilot\legacy")
    }

    #[test]
    fn route_harness_defines_every_symbol_fragments_use() {
        let fragment = "\u{feff}if (MoveMethod = \"walk\")\n{\n\tnm_gotoramp()\n\tnm_Walk(5, FwdKey)\n}\nHyperSleep(50)\n";
        let script = generate_walk_script(
            &root(),
            FragmentKind::Route,
            fragment,
            &HarnessSettings::default(),
        )
        .unwrap();

        for required in [
            "nm_Walk(tiles, MoveKey1, MoveKey2:=0)",
            "nm_gotoRamp() {",
            "nm_gotoCannon() {",
            "nm_Reset()",
            "FwdKey:=\"sc011\"",
            "MoveMethod:=\"cannon\"",
            "HiveSlot:=6",
            "#Include \"C:\\NectarPilot\\legacy\\lib\\Walk.ahk\"",
            "#Include \"C:\\NectarPilot\\legacy\\nm_image_assets\\offset\\bitmaps.ahk\"",
            "\\nm_image_assets\\reset\\bitmaps.ahk",
            "OnExit(ExitFunc)",
            "ExitApp 0",
        ] {
            assert!(script.contains(required), "missing {required:?}");
        }
        assert!(!script.contains("{{"), "unresolved placeholder in script");
        assert!(!script.contains('\u{feff}'), "BOM must not be embedded");
    }

    #[test]
    fn pattern_harness_defines_gather_environment() {
        let settings = HarnessSettings {
            new_walk: false,
            pattern: PatternSettings {
                size: PatternSize::Large,
                repetitions: 3,
                facing_corner: true,
                invert_forward_back: true,
                invert_left_right: false,
            },
            ..HarnessSettings::default()
        };
        let script = generate_walk_script(
            &root(),
            FragmentKind::Pattern,
            "loop reps {\n\tsend \"{\" TCLRKey \" down}\"\n\tWalk(11 * size)\n\tsend \"{\" TCLRKey \" up}\"\n}\n",
            &settings,
        )
        .unwrap();

        for required in [
            "size:=1.5",
            "reps:=3",
            "facingcorner:=1",
            "TCFBKey:=BackKey",
            "AFCFBKey:=FwdKey",
            "TCLRKey:=LeftKey",
            "nm_CameraRotation(Dir, count)",
            "Walk(param, *) => HyperSleep(4000/28*param)",
        ] {
            assert!(script.contains(required), "missing {required:?}");
        }
        assert!(!script.contains("#Include \"C:\\NectarPilot\\legacy\\lib\\Walk.ahk\""));
    }

    #[test]
    fn unsafe_roots_and_settings_are_rejected() {
        let settings = HarnessSettings::default();
        assert!(matches!(
            generate_walk_script(
                Path::new("C:\\bad\"root"),
                FragmentKind::Route,
                "Sleep 5",
                &settings
            ),
            Err(HarnessError::UnsafeRoot(_))
        ));

        let invalid = HarnessSettings {
            hive_slot: 9,
            ..HarnessSettings::default()
        };
        assert!(matches!(
            generate_walk_script(&root(), FragmentKind::Route, "Sleep 5", &invalid),
            Err(HarnessError::InvalidSetting(_))
        ));

        assert!(matches!(
            generate_walk_script(&root(), FragmentKind::Route, "\u{feff}  \n", &settings),
            Err(HarnessError::UnsafeFragment(_))
        ));
    }

    #[test]
    fn extended_length_prefix_is_stripped_for_includes() {
        let script = generate_walk_script(
            Path::new(r"\\?\C:\Apps\NectarPilot"),
            FragmentKind::Route,
            "Sleep 5",
            &HarnessSettings::default(),
        )
        .unwrap();
        assert!(script.contains("#Include \"C:\\Apps\\NectarPilot\\lib\\Roblox.ahk\""));
        assert!(!script.contains(r"\\?\"));
    }
}
