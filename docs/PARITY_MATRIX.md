# Feature parity matrix

Public beta is blocked until every row is implemented in the Rust engine or explicitly routed through the consented legacy bridge and has a passing regression scenario.

| Area | Capability | Native Rust | Legacy bridge | Fixture | Live safe test |
| --- | --- | :---: | :---: | :---: | :---: |
| Gather | Three-field rotation and priorities | ☐ | ☐ | ☐ | ☐ |
| Gather | Pattern size, repetitions, shift and inversion | ☐ | ☐ | ☐ | ☐ |
| Gather | Time/pack stop conditions and return modes | ☐ | ☐ | ☐ | ☐ |
| Gather | Sprinklers and drift compensation | ☐ | ☐ | ☐ | ☐ |
| Travel | Hive detection and claiming | ☐ | ☐ | ☐ | ☐ |
| Travel | Walk/cannon travel and interaction prompts | ☐ | ☐ | ☐ | ☐ |
| Recovery | Reset, death, disconnect and bounded reconnect | ☐ | ☐ | ☐ | ☐ |
| Activities | Clock and dispensers | ☐ | ☐ | ☐ | ☐ |
| Activities | Blender and memory matches | ☐ | ☐ | ☐ | ☐ |
| Combat | Bug runs and bosses | ☐ | ☐ | ☐ | ☐ |
| Combat | Night and Vicious Bee | ☐ | ☐ | ☐ | ☐ |
| Boosts | Field boosters and hotbar schedule | ☐ | ☐ | ☐ | ☐ |
| Boosts | Auto field boost and consumable budgets | ☐ | ☐ | ☐ | ☐ |
| Boosts | Wind Shrine, Sticker Stack and Sticker Printer | ☐ | ☐ | ☐ | ☐ |
| Quests | Polar, Honey, Black and Brown Bear | ☐ | ☐ | ☐ | ☐ |
| Quests | Science Bear catalog and overlap planner | ☑ | N/A | ☑ | ☐ |
| Quests | Bucko and Riley Bee | ☐ | ☐ | ☐ | ☐ |
| Planters | Manual mode and timers | ☐ | ☐ | ☐ | ☐ |
| Planters | Nectar priority automation | ☐ | ☐ | ☐ | ☐ |
| Monitoring | Runtime/honey/session statistics | ☐ | ☐ | ☐ | ☐ |
| Integrations | Webhooks, reports and screenshots | ☐ | ☐ | ☐ | ☐ |
| Integrations | Permission-scoped Discord commands | ☐ | ☐ | ☐ | ☐ |
| Utilities | Autoclicker, hotkeys, autostart and FPS | ☐ | ☐ | ☐ | ☐ |
| Utilities | Mutations and Auto-Jelly | ☐ | ☐ | ☐ | ☐ |
| Seasonal | Feature-flagged Beesmas tasks | ☐ | ☐ | ☐ | ☐ |
| Extensions | Built-in paths and patterns | ☐ | ☑ | ☑ | ☐ |
| Extensions | `.nectar.yaml` import/export | ☐ | N/A | ☐ | ☐ |
| Extensions | Trusted legacy AHK compatibility | N/A | ☑ | ☑ | ☐ |

`Unknown` and other uncertain perception results must have negative tests showing that they cannot initiate travel, item use, or process recovery.
