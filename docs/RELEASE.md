# Release process

Development builds can produce an unsigned, current-user NSIS installer with:

```powershell
pnpm bundle:unsigned
```

This does not satisfy a public beta or stable release gate. Public releases require all of the following:

1. Every row in `PARITY_MATRIX.md` is complete, including live safe tests.
2. A reviewed updater public key and HTTPS beta/stable endpoint are present in `tauri.conf.json`.
3. The matching encrypted Tauri private key is supplied only through repository release secrets.
4. The signed-off JSON soak result satisfies `docs/soak-result.schema.json` and the 24-hour beta or 72-hour stable duration.
5. The workflow passes tests, builds the per-user installer, produces updater signatures/checksums and an SBOM, and creates build provenance attestations.

Self-update remains disabled in a Git checkout. The checked-in updater configuration is intentionally empty during pre-beta development, so an unsigned local build cannot accidentally trust a placeholder key or endpoint.
