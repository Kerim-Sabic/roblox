# Legacy compatibility

NectarPilot preserves every built-in Natro v1.1.2 route and pattern while the native Rust ports are completed.

- `assets/routes/_legacy-manifest.yaml` catalogs all 91 route files.
- `assets/patterns/_legacy-manifest.yaml` catalogs all 12 pattern files.
- `assets/legacy-support/_legacy-manifest.yaml` pins the 9 imported library files the execution harness includes.
- `assets/detectors/_legacy-manifest.yaml` catalogs every `nm_image_assets` detector template with digest, dimensions, and inline-template counts.
- Each entry records its SHA-256 digest, conversion result, and a bounded sample of unsupported syntax.
- `stationary.nectar.yaml` is the first fully declarative conversion. The remaining dynamic scripts are marked `legacy_bridge_required`; they are not partially converted or silently treated as safe.

## Execution harness

Natro never executed `paths/*.ahk` or `patterns/*.ahk` on their own: its `nm_createWalk` wrapped each fragment in a generated walk script defining the movement keys, `nm_Walk`, movespeed-corrected `Walk`, `nm_gotoRamp`/`nm_gotoCannon`/`nm_Reset`, and the Gdip/Roblox libraries. A bare fragment fails at load with "call to nonexistent function". `nectarpilot-legacy::generate_walk_script` reproduces that wrapper exactly:

- the route context (`nm_gotoRamp`, `nm_gotoCannon`, `nm_Reset`) is Natro's own `nm_PathVars` body, extracted verbatim into `crates/nectarpilot-legacy/support/route_context.ahk`;
- pattern runs receive Natro's gather environment (`size`, `reps`, `facingcorner`, TC/AFC keys, `nm_CameraRotation`);
- movement settings (`MoveMethod`, `MoveSpeedNum`, `NewWalk`, `HiveSlot`, `HiveBees`, `KeyDelay`) come from the profile's imported INI snapshot, with Natro's stock defaults otherwise, and are bounds-checked before generation;
- the generated script carries an internal watchdog (exit code 86) below the runner's 30-minute hard kill, and releases all held keys on exit.

Every `#Include`d library file is re-verified against the pinned support manifest immediately before a run. The daemon writes the generated harness to its private run directory, hashes it, and the runner re-hashes that exact file immediately before process creation.

Validate that every fragment loads under the pinned interpreter (the same `/Validate` mechanism Natro used for pattern imports; no game code executes):

```powershell
cargo run -p nectarpilot-legacy --bin validate_legacy_assets
```

This checks 390 generated variants (91 routes x walk/cannon x new/legacy walk, 11 bridge patterns x 2, and the builtin reset/convert step x 4) and must report zero failures.

## Orchestrated sessions

`StartLegacySession` turns the profile's saved field rotation into a supervised loop of individually trusted steps: travel route → gather pattern (repeated) → builtin reset/convert (Natro's own `nm_Reset` + ramp walk + prompt-checked Make Honey press). The engine preflights every unique asset before the first step, emits per-step progress and outcomes, honors pause (the harness F16 handler releases and restores held keys), enforces cycle and wall-clock limits, and records a run-history row plus a redacted JSON report. On a failed step with reconnect enabled, the daemon confirms the legacy disconnect dialog (or a fully absent client), rejoins through the DPAPI-encrypted `private_server_link` secret, and re-anchors with the reset step; anything ambiguous ends the session as needs-attention instead of guessing.

## Consent and containment

The compatibility runner never executes a script during import. A profile must explicitly trust the exact script digest, and any subsequent file change invalidates that trust. The Extensions page offers a `Run contained script` control only after that review. The daemon then independently rechecks the embedded manifest, script size/hash, the pinned AutoHotkey64 interpreter hash, the pinned support-library hashes, the profile trust record, zero valuable-item budgets, disabled purchases/donations/trades/Discord, and one restored foreground RobloxPlayerBeta window at launch. Execution uses an exact child process in a Windows job object, with a 30-minute timeout, cancellation, and kill-on-close containment. The UI must still show that legacy AutoHotkey is arbitrary code with the current user's authority.

Regenerate the catalogs deterministically from the repository root:

```powershell
cargo run -p nectarpilot-legacy --bin convert_legacy_assets
```

The command also validates every detector template (file images, inline base64 templates, and script references) and fails on any missing or corrupt entry.

Native conversion remains a release-gated task. A manifest entry proves preservation and classification; it does not count as a native Rust port or a live safety test.
