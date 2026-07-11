# Controlled live-test protocol

Live automation starts only after the offline suite passes for the scenario being tested.

## Before a run

1. Use the `Safe test` profile.
2. Confirm all valuable-item budgets are zero.
3. Disable purchases, trades, donations, Auto-Jelly, shrine, stickers, dice, glitter, eggs and remote input.
4. Confirm the official Roblox web client is open on Bee Swarm Simulator and no other Roblox/Studio process is adopted.
5. Confirm `F3` stops normally and `Ctrl+Shift+F12` triggers the hard emergency stop.
6. Announce the exact scenario and maximum duration before starting.

## During a run

- Keep the first scenarios attended and bounded to five minutes.
- Stop on focus loss, an uncertain detector, an unexpected item prompt, or movement outside the expected route.
- Never approve an item-spend prompt simply to continue a test.
- Capture cropped evidence and the typed state transition; do not use full-screen recordings by default.

## After a run

- Verify every held key and mouse button was released.
- Verify only the adopted Roblox PID was touched.
- Review the diagnostics bundle before sharing it.
- Record the scenario, build commit, profile digest, duration, result and any recovery action.

The 24-hour and 72-hour gates begin only after all attended feature scenarios pass. A soak failure resets the corresponding gate after the defect is fixed.
