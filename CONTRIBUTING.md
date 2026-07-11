# Contributing to NectarPilot

NectarPilot is a GPLv3 derivative of Natro Macro. Contributions must preserve attribution and use dependencies compatible with GPLv3 distribution.

## Local checks

```powershell
pnpm install
pnpm check
pnpm test
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Automation changes require offline fixture tests before a live Roblox test. Live tests must use the dedicated safe profile; purchases, donations, trades, and item spending remain disabled unless the tester explicitly authorizes a bounded scenario.

Never add process injection, memory reading, anti-cheat bypasses, arbitrary remote execution, or logging of secrets.
