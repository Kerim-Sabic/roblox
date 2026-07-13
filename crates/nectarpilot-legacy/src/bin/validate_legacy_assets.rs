//! Load-validates the generated walk harness for every manifest-pinned legacy
//! asset using the pinned `AutoHotkey64.exe` in `/Validate` mode.
//!
//! `/Validate` makes the interpreter parse the script, process `#Include`s,
//! and resolve every function reference, then exit without executing any code.
//! This is the same mechanism Natro Macro v1.1.2 used to vet imported
//! patterns, so a clean pass here means each fragment loads exactly as it did
//! inside the legacy macro.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use nectarpilot_legacy::{
    AssetCatalog, AssetStatus, FragmentKind, HarnessSettings, MoveMethod, generate_reset_script,
    generate_walk_script,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let legacy_root = arguments.next().map_or_else(
        || std::env::current_dir().expect("current directory is available"),
        PathBuf::from,
    );
    if arguments.next().is_some() {
        return Err("usage: validate_legacy_assets [legacy-root]".into());
    }
    let legacy_root = fs::canonicalize(&legacy_root)?;
    let interpreter = legacy_root.join("submacros").join("AutoHotkey64.exe");
    if !interpreter.is_file() {
        return Err(format!("pinned interpreter missing at {}", interpreter.display()).into());
    }

    let staging = std::env::temp_dir().join(format!(
        "nectarpilot-harness-validate-{}",
        std::process::id()
    ));
    fs::create_dir_all(&staging)?;
    let mut checked = 0_u32;
    let mut failures = Vec::new();

    for (manifest, kind) in [
        ("assets/routes/_legacy-manifest.yaml", FragmentKind::Route),
        (
            "assets/patterns/_legacy-manifest.yaml",
            FragmentKind::Pattern,
        ),
    ] {
        let catalog: AssetCatalog =
            serde_yaml::from_str(&fs::read_to_string(legacy_root.join(manifest))?)?;
        for entry in &catalog.entries {
            if entry.status != AssetStatus::LegacyBridgeRequired {
                continue; // native DSL conversions never touch AutoHotkey
            }
            let fragment = fs::read_to_string(legacy_root.join(&entry.legacy_source))?;
            let variants: &[MoveMethod] = match kind {
                FragmentKind::Route => &[MoveMethod::Walk, MoveMethod::Cannon],
                FragmentKind::Pattern => &[MoveMethod::Cannon],
            };
            for &move_method in variants {
                for new_walk in [true, false] {
                    let settings = HarnessSettings {
                        move_method,
                        new_walk,
                        ..HarnessSettings::default()
                    };
                    let script = generate_walk_script(&legacy_root, kind, &fragment, &settings)?;
                    let label = format!(
                        "{} ({:?}, new_walk={new_walk})",
                        entry.legacy_source, move_method
                    );
                    checked += 1;
                    if let Some(problem) =
                        validate_script(&interpreter, &staging, &script, checked)?
                    {
                        failures.push(format!("{label}: {problem}"));
                    }
                }
            }
        }
    }

    // The orchestrator's generated reset-and-convert step must load cleanly in
    // every walk-mode/move-method combination too.
    for move_method in [MoveMethod::Walk, MoveMethod::Cannon] {
        for new_walk in [true, false] {
            let settings = HarnessSettings {
                move_method,
                new_walk,
                ..HarnessSettings::default()
            };
            let script = generate_reset_script(&legacy_root, &settings, 30)?;
            checked += 1;
            if let Some(problem) = validate_script(&interpreter, &staging, &script, checked)? {
                failures.push(format!(
                    "builtin:reset-convert ({move_method:?}, new_walk={new_walk}): {problem}"
                ));
            }
        }
    }

    let _ = fs::remove_dir_all(&staging);
    if failures.is_empty() {
        println!(
            "all {checked} generated harness variants load cleanly under the pinned AutoHotkey64"
        );
        Ok(())
    } else {
        for failure in &failures {
            eprintln!("FAIL {failure}");
        }
        Err(format!(
            "{} of {checked} harness variants failed to load",
            failures.len()
        )
        .into())
    }
}

fn validate_script(
    interpreter: &Path,
    staging: &Path,
    script: &str,
    index: u32,
) -> Result<Option<String>, std::io::Error> {
    let path = staging.join(format!("harness-{index}.ahk"));
    fs::write(&path, script)?;
    let output = Command::new(interpreter)
        .arg("/ErrorStdOut")
        .arg("/Validate")
        .arg(&path)
        .output()?;
    if output.status.success() {
        Ok(None)
    } else {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        Ok(Some(format!(
            "exit={:?} {}{}",
            output.status.code(),
            stdout.trim(),
            stderr.trim()
        )))
    }
}
