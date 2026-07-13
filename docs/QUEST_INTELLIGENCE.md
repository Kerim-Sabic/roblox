# Quest intelligence

NectarPilot treats quests as typed, versioned objectives instead of a single “quest field” setting. The planner scores every confidently calibrated field by how much of every confidently detected active quest it can advance per minute, then discounts travel. A short dwell hysteresis prevents field thrashing when the current field remains close to the best score.

## Science Bear v1 catalog

`assets/quests/science-bear.v1.json` contains all 31 permanent Science Bear quests in sequence, including the Translator rewards at quests 21, 26, and 31. The catalog covers:

- field, color, and unrestricted pollen;
- field/color goo;
- ability, boost, mark, focus, link, sparkle, sprout, fruit, and other tokens;
- regular mobs, Ant Challenge mobs, bosses, Vicious Bee, and Stick Bug objectives;
- bee discovery/ownership, badge, Blender, dispenser, and item objectives.

The source snapshot is the community-maintained [Bee Swarm Simulator quest table](https://bee-swarm-simulator.fandom.com/wiki/Quests#Science_Bear), verified on 2026-07-11. It identifies Science Bear's objectives as a mix of pollen, mobs, crafting, and ability tokens, and documents the progression-critical Translator questline. Because the game and community data can change, the knowledge version and source URL are stored with the catalog and must be reviewed when detections stop matching.

## Repeatable quest catalogs (Polar, Black, Bucko, Riley)

Four additional catalogs are transcribed one-to-one from the quest tables embedded in the imported Natro Macro v1.1.2 source (`submacros/natro_macro.ahk`), the same data the legacy macro used in production:

- `assets/quests/polar-bear.v1.json` — the 20-recipe Polar Bear rotation;
- `assets/quests/black-bear.v1.json` — the 18 Black Bear pollen missions;
- `assets/quests/bucko-bee.v1.json` and `assets/quests/riley-bee.v1.json` — the 17-quest Gifted Bucko/Riley pools.

These repeatable quests use completion-bar semantics: every objective's amount is `1`, meaning "this quest bar is complete", exactly how the legacy macro tracked progress (quest-bar color, not numeric counts). The planner therefore ranks fields by objective coverage and overlap for these givers rather than by estimated time-to-target.

Bucko and Riley share several quest names (`Tour`, `Tango`, `Scavenge`, ...). Title OCR alone cannot distinguish them, so `QuestTitleDetector::for_giver` requires the giver up front — in live use that signal comes from the quest-log giver icons (`bucko.png`, `riley.png`, `polar_bear*.png`, `black_bear*.png`), which are cataloged and validated in `assets/detectors/_legacy-manifest.yaml`. `detect_quest_title` never matches text across catalogs.

## Safety and scheduling rules

- An uncertain quest title, objective, field calibration, or OCR value contributes no target.
- Science Bear title OCR is vocabulary-constrained to the 31 catalog names and requires two agreeing matches within three confident frames.
- Valuable item, feeding, crafting, machine, badge, and bee-discovery objectives are reported as held work until a dedicated verified task exists.
- Item and feeding objectives additionally require a positive per-item budget. A quest never overrides a zero budget.
- Translator quests receive higher progression weight, but confidence and safety gates still win.
- Overlap is preferred: for example, Bamboo can advance a Bamboo requirement, blue pollen, goo, token, and mob objectives from several quests in one visit.
- Travel cost and a three-minute dwell window reduce repeated cannon/reset travel for marginal score changes.

## Advisory live scan

`ScanQuests` performs one bounded in-game reading: it uses Natro's client-anchored fixed menu position, verifies the open state against the pinned `questlog` template before reading anything, detects the giver icon (two-frame consensus over the validated icon templates), reads the title with giver-scoped constrained OCR, classifies the objective completion bars, then toggles the log closed and releases all inputs. The scanner runs as an exclusive cancellable engine worker; Stop, the hard emergency stop, focus loss, or its 60-second deadline prevent further clicks and release the local input broker. The result is advisory: it recommends fields only when the detected bar count aligns exactly with the matched quest, and reports every uncertain reading as a note. Dynamic givers (Brown Bear) are reported as held work until the dynamic-objective reader is live-validated.

The planner remains advisory: the scan can surface evidence-backed field recommendations, but it cannot start travel or native quest execution. Broader live quest parity remains fixture- and soak-gated; the catalog alone is not evidence of end-to-end automation.
