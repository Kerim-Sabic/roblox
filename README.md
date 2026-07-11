# NectarPilot

NectarPilot is a safety-first Windows automation dashboard for Bee Swarm Simulator, rebuilt from Natro Macro v1.1.2 with Tauri, React, TypeScript, and Rust.

> [!IMPORTANT]
> NectarPilot is an independent GPLv3 fork and is not affiliated with Natro Team, Roblox Corporation, or the Bee Swarm Simulator developers. The rewrite is under active development and is not yet a public beta.

## What changes

- A responsive Fluent Honey dashboard and compact running monitor
- A native Rust state machine with bounded retries and an emergency stop
- Exact Roblox window/process ownership and focus-safe input
- Typed, transactional profiles with legacy INI migration
- Declarative `.nectar.yaml` paths and patterns
- Explicitly trusted legacy AutoHotkey compatibility
- Local, redacted diagnostics and signed update artifacts

The original Natro v1.1.2 import is preserved in Git under the `natro-v1.1.2-import` tag. Runtime settings, logs, private-server links, webhooks, and tokens are ignored and are never included in the repository.

## Development

Requirements: Windows 10/11 x64, Rust 1.96, Node.js 22, pnpm 10, and the Microsoft C++ build tools required by Tauri.

In a development checkout, double-click `START.bat`; it now launches NectarPilot rather than the old AutoHotkey UI. From a terminal, the equivalent command is:

```powershell
pnpm install
pnpm dev
```

Run all checks:

```powershell
pnpm check
pnpm test
```

See [architecture](docs/ARCHITECTURE.md), [quest intelligence](docs/QUEST_INTELLIGENCE.md), [security](docs/SECURITY.md), [release gates](docs/RELEASE.md), [legacy compatibility](docs/LEGACY_COMPATIBILITY.md), and [contributing](CONTRIBUTING.md).

## Account risk

[Roblox states that cheating or exploiting violates its rules](https://en.help.roblox.com/hc/en-us/articles/203312450-Cheating-and-Exploiting) and may lead to account moderation or deletion. Users are responsible for checking the current Roblox rules and the rules of the experience they automate. NectarPilot does not attempt to hide automation or bypass anti-cheat systems.

## License and attribution

NectarPilot is licensed under [GNU GPL v3.0](LICENSE.md). It is based on Natro Macro, Copyright © Natro Team and its contributors. All modifications are identified through this repository's Git history.
