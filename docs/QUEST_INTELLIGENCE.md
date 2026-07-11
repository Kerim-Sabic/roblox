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

## Safety and scheduling rules

- An uncertain quest title, objective, field calibration, or OCR value contributes no target.
- Science Bear title OCR is vocabulary-constrained to the 31 catalog names and requires two agreeing matches within three confident frames.
- Valuable item, feeding, crafting, machine, badge, and bee-discovery objectives are reported as held work until a dedicated verified task exists.
- Item and feeding objectives additionally require a positive per-item budget. A quest never overrides a zero budget.
- Translator quests receive higher progression weight, but confidence and safety gates still win.
- Overlap is preferred: for example, Bamboo can advance a Bamboo requirement, blue pollen, goo, token, and mob objectives from several quests in one visit.
- Travel cost and a three-minute dwell window reduce repeated cannon/reset travel for marginal score changes.

The planner is currently an offline decision component. Connecting its recommendations to live OCR, detectors, and native task execution remains fixture- and soak-gated; the catalog alone is not evidence of live quest parity.
