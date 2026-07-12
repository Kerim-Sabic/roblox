# Perception fixtures

Fixtures are cropped, redacted captures of the official Roblox client. Each image must have adjacent JSON metadata containing:

- fixture schema version;
- source client width and height;
- monitor DPI and display scale;
- expected anchors and detector results;
- allowed confidence range;
- whether the frame is safe to act on;
- a short provenance/license note;
- a SHA-256 digest.

Do not commit chat, usernames, private-server codes, notifications, or other personal information. Prefer synthetic fixtures for secret-bearing dialogs.

Required groups before beta:

- startup and BSS-ready anchors;
- loading, disconnect, crash and wrong-game states;
- interaction prompts, hive slots, cannon and reset states;
- every supported quest giver, including unknown/truncated objectives;
- day/night, Vicious Bee and false-positive lookalikes;
- planters, inventory, dispensers, memory match, boosts and bosses;
- performance stats and common overlays;
- 1280×720, 1600×900 and 1920×1080 client sizes at 100% scale;
- UI-only DPI snapshots at 100%, 125%, 150% and 200%.

The fixture suite must assert that an unknown field or quest objective never becomes an actionable route.

`parity-index.json` is the machine-checked inventory for all 28 parity scenarios. It deliberately starts as `contract_only`: it describes the exact redacted captures, supported sizes/DPI values, detector vocabulary, regression assertion, and attended live scenario needed for each capability. CI checks that no scenario or legacy asset class is omitted. A release workflow additionally runs `verify-parity-fixtures.ps1 -RequireCaptured`, which rejects every contract-only item until reviewed captures replace it.
