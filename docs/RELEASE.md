# Release process

Development builds can produce an unsigned, current-user NSIS installer with:

```powershell
pnpm bundle:unsigned
```

This does not satisfy a public beta or stable release gate. Public releases require all of the following:

1. Every row in `PARITY_MATRIX.md` is complete, including live safe tests.
2. A reviewed updater public key and HTTPS stable/beta endpoint are present in the matching Tauri configuration.
3. The matching updater private key is supplied only through the `TAURI_SIGNING_PRIVATE_KEY` repository release secret.
4. The signed-off JSON soak result satisfies `docs/soak-result.schema.json` and the 24-hour beta or 72-hour stable duration.
5. The workflow passes tests, builds the per-user installer, produces updater signatures/checksums and an SBOM, and creates build provenance attestations.

Self-update remains disabled in a Git checkout. The checked-in public key only verifies release artifacts; it cannot sign them. No release workflow is allowed to bypass the parity or elapsed-time soak checks.

## Recording a real soak

Run this only against the dedicated Safe test profile, after completing the attended scenarios in [LIVE_TEST_PROTOCOL.md](LIVE_TEST_PROTOCOL.md). The profile export is a starting assertion; live mode also reads the daemon-owned database at every heartbeat so it stops if a risky setting is enabled.

```powershell
./scripts/start-soak.ps1 -Channel beta -Mode live `
  -ProfileJson .\safe-test-profile.json `
  -Database "$env:LOCALAPPDATA\NectarPilot\nectarpilot.sqlite3" `
  -DaemonPath .\target\release\nectarpilot-daemon.exe `
  -RobloxPid <adopted-RobloxPlayerBeta-PID> -AllowLiveInput
```

After at least 24 hours for beta or 72 hours plus 50 forced recovery scenarios for stable, review the NDJSON evidence and complete it manually:

```powershell
./scripts/complete-soak.ps1 -Manifest <evidence-directory>\manifest.json `
  -ApprovedBy "reviewer" -Passed -ForcedRecoveryScenarios 50
```

The completion script refuses fixture-only runs, shortened wall-clock durations, unsafe profile observations, and non-zero safety metrics. The resulting JSON is the only kind accepted by the release workflow.
