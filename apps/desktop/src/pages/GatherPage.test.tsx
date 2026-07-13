import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { NectarActions } from "../hooks/useNectarPilot";
import { createMockSnapshot } from "../services/seed";
import type { AutomationSettings } from "../types/contracts";
import { GatherPage } from "./GatherPage";

function createActions() {
  let savedSettings: AutomationSettings | undefined;
  const pageActions: NectarActions = {
    refreshSession: vi.fn().mockResolvedValue(undefined),
    start: vi.fn().mockResolvedValue(undefined),
    acknowledgeAttention: vi.fn().mockResolvedValue(undefined),
    pause: vi.fn().mockResolvedValue(undefined),
    stop: vi.fn().mockResolvedValue(undefined),
    emergencyStop: vi.fn().mockResolvedValue(undefined),
    selectProfile: vi.fn().mockResolvedValue(undefined),
    saveSettings: async (settings) => {
      savedSettings = settings;
    },
    completeOnboarding: vi.fn().mockResolvedValue(undefined),
    trustExtension: vi.fn().mockResolvedValue(undefined),
    runLegacyExtension: vi.fn().mockResolvedValue(undefined),
    startLegacySession: vi.fn().mockResolvedValue(undefined),
    inspectLegacy: vi.fn().mockResolvedValue(undefined),
    scanQuests: vi.fn().mockResolvedValue(undefined),
    setCompactMode: vi.fn().mockResolvedValue(undefined),
  };
  return { pageActions, savedSettings: () => savedSettings };
}

describe("GatherPage", () => {
  it("turns an empty profile into an explicit saved field rotation", async () => {
    const user = userEvent.setup();
    const snapshot = createMockSnapshot();
    snapshot.profiles[0]!.settings.gathering = {
      ...snapshot.profiles[0]!.settings.gathering,
      enabled: false,
      fields: [],
      pattern: "e_lol",
    };
    const actionState = createActions();

    render(
      <GatherPage
        snapshot={snapshot}
        actions={actionState.pageActions}
        pendingAction={null}
      />,
    );

    expect(screen.getByText("Add your first field")).toBeVisible();
    await user.selectOptions(
      screen.getByLabelText("Field to add"),
      "Pine Tree Forest",
    );
    await user.click(screen.getByRole("button", { name: /Add field/ }));

    expect(
      screen.getByRole("button", { name: "Remove Pine Tree Forest" }),
    ).toBeVisible();
    expect(screen.getByText("Unsaved changes")).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Apply plan" }));
    const saved = actionState.savedSettings();
    expect(saved).toBeDefined();
    expect(saved?.gathering.enabled).toBe(true);
    expect(saved?.gathering.fields).toEqual(["Pine Tree Forest"]);
    expect(saved?.gathering.pattern).toBe("e_lol");
  });
});
