# Feature parity matrix

Public beta is blocked until every row is implemented in the Rust engine or explicitly routed through the consented legacy bridge and has a passing regression scenario.

| Area | Capability | Native Rust | Legacy bridge | Fixture | Live safe test |
| --- | --- | :---: | :---: | :---: | :---: |
| Gather | Three-field rotation and priorities (orchestrated legacy session: travel → pattern → reset/convert loop) | ☐ | ☑ | ☑ | ☐ |
| Gather | Pattern size, repetitions, shift and inversion | ☐ | ☐ | ☐ | ☐ |
| Gather | Time/pack stop conditions and return modes | ☐ | ☐ | ☐ | ☐ |
| Gather | Sprinklers and drift compensation | ☐ | ☐ | ☐ | ☐ |
| Travel | Hive detection and claiming | ☐ | ☐ | ☐ | ☐ |
| Travel | Walk/cannon travel and interaction prompts | ☐ | ☐ | ☐ | ☐ |
| Recovery | Reset, death, disconnect and bounded reconnect (disconnect template + private-server rejoin + reset re-anchor) | ☐ | ☑ | ☐ | ☐ |
| Activities | Allowlisted clock, free dispensers, and field boosters (profile-scoped cooldown steps at cycle boundaries; persistence errors fail closed) | ☐ | ☑ | ☑ | ☐ |
| Activities | Blender and memory matches | ☐ | ☐ | ☐ | ☐ |
| Combat | Bug runs and bosses | ☐ | ☐ | ☐ | ☐ |
| Combat | Night and Vicious Bee | ☐ | ☐ | ☐ | ☐ |
| Boosts | Field boosters and hotbar schedule | ☐ | ☐ | ☐ | ☐ |
| Boosts | Auto field boost and consumable budgets | ☐ | ☐ | ☐ | ☐ |
| Boosts | Wind Shrine, Sticker Stack and Sticker Printer | ☐ | ☐ | ☐ | ☐ |
| Quests | Polar, Honey, Black and Brown Bear | ☐ | ☐ | ☐ | ☐ |
| Quests | Science Bear catalog and overlap planner | ☑ | N/A | ☑ | ☐ |
| Quests | Polar/Black/Bucko/Riley catalogs and giver-scoped title detection | ☑ | N/A | ☑ | ☐ |
| Quests | Advisory quest-log scan (verified open/close, icon + title + bars) | ☑ | N/A | ☐ | ☐ |
| Quests | Bucko and Riley Bee | ☐ | ☐ | ☐ | ☐ |
| Planters | Manual mode and timers | ☐ | ☐ | ☐ | ☐ |
| Planters | Nectar priority automation | ☐ | ☐ | ☐ | ☐ |
| Monitoring | Runtime/honey/session statistics (HUD counter OCR voting + windowed honey/hr + run history/reports) | ☑ | N/A | ☑ | ☐ |
| Planters | Manual planter reminder timers in the profile | ☑ | N/A | ☐ | ☐ |
| Utilities | Global start/pause/stop/emergency hotkeys and legacy pause (F16) | ☑ | ☑ | ☐ | ☐ |
| Utilities | In-app movement calibration (walk speed, hive slot/bees, travel method, key delay) | ☑ | ☑ | ☑ | ☐ |
| Integrations | Webhooks, reports and screenshots | ☐ | ☐ | ☐ | ☐ |
| Integrations | Permission-scoped Discord commands | ☐ | ☐ | ☐ | ☐ |
| Utilities | Autoclicker, hotkeys, autostart and FPS | ☐ | ☐ | ☐ | ☐ |
| Utilities | Mutations and Auto-Jelly | ☐ | ☐ | ☐ | ☐ |
| Seasonal | Feature-flagged Beesmas tasks | ☐ | ☐ | ☐ | ☐ |
| Extensions | Built-in paths and patterns (full nm_createWalk harness; 390 variants load-validated under the pinned interpreter) | ☐ | ☑ | ☑ | ☐ |
| Extensions | `.nectar.yaml` import/export | ☐ | N/A | ☐ | ☐ |
| Extensions | Trusted legacy AHK compatibility | N/A | ☑ | ☑ | ☐ |

`Unknown` and other uncertain perception results must have negative tests showing that they cannot initiate travel, item use, or process recovery.
