# Legacy compatibility

NectarPilot preserves every built-in Natro v1.1.2 route and pattern while the native Rust ports are completed.

- `assets/routes/_legacy-manifest.yaml` catalogs all 91 route files.
- `assets/patterns/_legacy-manifest.yaml` catalogs all 12 pattern files.
- Each entry records its SHA-256 digest, conversion result, and a bounded sample of unsupported syntax.
- `stationary.nectar.yaml` is the first fully declarative conversion. The remaining dynamic scripts are marked `legacy_bridge_required`; they are not partially converted or silently treated as safe.

The compatibility runner never executes a script during import. A profile must explicitly trust the exact script digest, and any subsequent file change invalidates that trust. Execution uses an exact child process in a Windows job object, with a timeout, cancellation, and kill-on-close containment. The UI must still show that legacy AutoHotkey is arbitrary code with the current user's authority.

Regenerate the catalogs deterministically from the repository root:

```powershell
cargo run -p nectarpilot-legacy --bin convert_legacy_assets
```

Native conversion remains a release-gated task. A manifest entry proves preservation and classification; it does not count as a native Rust port or a live safety test.
