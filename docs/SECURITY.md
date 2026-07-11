# Security policy

## Boundaries

NectarPilot automates the official, unmodified Roblox Windows client through visible-screen capture and normal Windows input. It does not inject code, read Roblox memory, alter the client, or bypass anti-cheat systems.

Legacy AutoHotkey extensions are executable code. The safe `.nectar.yaml` format should be preferred. Full legacy mode is opt-in per script, displays the script hash and source path, and may access anything available to the current Windows user.

## Reporting

Do not include Discord tokens, webhooks, private-server links, full screenshots, or personal identifiers in a public report. Use the in-app diagnostics exporter, review its contents, and attach the resulting redacted bundle privately when possible.

## Release integrity

Release workflows create SHA-256 checksums, signed Tauri updater artifacts, and GitHub build attestations. The updater is disabled in developer checkouts and never installs while automation is running.
