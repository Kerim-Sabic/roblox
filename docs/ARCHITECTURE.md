# NectarPilot architecture

NectarPilot replaces the legacy shared-global AutoHotkey design with explicit process and trust boundaries.

## Processes

1. **Desktop shell** — Tauri hosts the bundled React UI. The WebView receives only narrowly scoped custom commands and never receives shell, filesystem, input, or process APIs.
2. **Daemon** — the single configuration and runtime-state owner. It schedules automation, supervises workers, owns the Roblox session, and writes transactional state.
3. **Legacy bridge** — an optional child process for explicitly trusted AutoHotkey v2 paths and patterns. Imported scripts never run automatically.

The shell and daemon exchange versioned messages over a current-user-only named pipe. Every request has an ID and every state-changing command is acknowledged. Unknown message versions are rejected.

## Safety invariants

- A detection marked `Uncertain`, `NotFound`, or `Error` cannot produce a movement target.
- Input is sent only while the adopted Roblox HWND is the foreground window.
- Cancellation, focus loss, errors, and process exit release every held key and mouse button.
- Only exact owned or explicitly adopted PIDs can be terminated.
- Reconnect and recovery have fixed attempt and time budgets.
- Optional integrations cannot be part of the core liveness decision.
- Valuable item budgets default to zero.

## Data

The daemon is the sole SQLite writer. Schema migrations and profile imports run in a transaction after a backup. Secrets are encrypted using Windows DPAPI and are never emitted into logs, IPC traces, command lines, or diagnostics.

Portable profile exports are versioned JSON without decrypted secrets. `.nectar.yaml` extensions are declarative, schema validated, and contain no file, network, process, or arbitrary-code primitives.

## Legacy migration

The untouched Natro v1.1.2 import is preserved by the `natro-v1.1.2-import` tag. NectarPilot imports legacy INI values without modifying the source files and produces an explicit mapped/unmapped/invalid report.
