import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { NectarActions } from "../hooks/useNectarPilot";
import { createMockSnapshot } from "../services/seed";
import { SettingsPage } from "./SettingsPage";

const actions: NectarActions = {
  refreshSession: vi.fn().mockResolvedValue(true),
  start: vi.fn().mockResolvedValue(undefined),
  acknowledgeAttention: vi.fn().mockResolvedValue(undefined),
  pause: vi.fn().mockResolvedValue(undefined),
  stop: vi.fn().mockResolvedValue(undefined),
  emergencyStop: vi.fn().mockResolvedValue(undefined),
  selectProfile: vi.fn().mockResolvedValue(undefined),
  saveSettings: vi.fn().mockResolvedValue(true),
  completeOnboarding: vi.fn().mockResolvedValue(undefined),
  trustExtension: vi.fn().mockResolvedValue(true),
  runLegacyExtension: vi.fn().mockResolvedValue(undefined),
  startLegacySession: vi.fn().mockResolvedValue(undefined),
  inspectLegacy: vi.fn().mockResolvedValue(undefined),
  scanQuests: vi.fn().mockResolvedValue(undefined),
  setCompactMode: vi.fn().mockResolvedValue(undefined),
};

describe("SettingsPage", () => {
  it("does not erase an unsaved movement edit when a fresh equivalent daemon snapshot arrives", async () => {
    const user = userEvent.setup();
    const snapshot = createMockSnapshot();
    const view = render(
      <SettingsPage
        snapshot={snapshot}
        actions={actions}
        pendingAction={null}
        theme="dark"
        onThemeChange={vi.fn()}
      />,
    );

    await user.click(screen.getByRole("button", { name: "Movement" }));
    const walkSpeed = screen.getByRole("spinbutton", { name: "Walk speed" });
    await user.clear(walkSpeed);
    await user.type(walkSpeed, "34.5");
    expect(screen.getByText("Unsaved changes")).toBeVisible();

    view.rerender(
      <SettingsPage
        snapshot={structuredClone(snapshot)}
        actions={actions}
        pendingAction={null}
        theme="dark"
        onThemeChange={vi.fn()}
      />,
    );

    expect(screen.getByRole("spinbutton", { name: "Walk speed" })).toHaveValue(
      34.5,
    );
    expect(screen.getByText("Unsaved changes")).toBeVisible();
  });
});
