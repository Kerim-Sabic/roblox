import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { NectarActions } from "../hooks/useNectarPilot";
import { createMockSnapshot } from "../services/seed";
import type { AutomationSettings } from "../types/contracts";
import { GatherPage } from "./GatherPage";

function createActions(saveResult = true) {
  let savedSettings: AutomationSettings | undefined;
  const pageActions: NectarActions = {
    refreshSession: vi.fn().mockResolvedValue(true),
    start: vi.fn().mockResolvedValue(undefined),
    acknowledgeAttention: vi.fn().mockResolvedValue(undefined),
    pause: vi.fn().mockResolvedValue(undefined),
    stop: vi.fn().mockResolvedValue(undefined),
    emergencyStop: vi.fn().mockResolvedValue(undefined),
    selectProfile: vi.fn().mockResolvedValue(undefined),
    saveSettings: async (settings) => {
      savedSettings = settings;
      return saveResult;
    },
    completeOnboarding: vi.fn().mockResolvedValue(true),
    trustExtension: vi.fn().mockResolvedValue(true),
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

  it("keeps a rejected plan dirty instead of pretending it was saved", async () => {
    const user = userEvent.setup();
    const snapshot = createMockSnapshot();
    const actionState = createActions(false);

    render(
      <GatherPage
        snapshot={snapshot}
        actions={actionState.pageActions}
        pendingAction={null}
      />,
    );

    await user.click(
      screen.getByRole("button", { name: "Remove Pine Tree Forest" }),
    );
    expect(screen.getByText("Unsaved changes")).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Apply plan" }));

    expect(actionState.savedSettings()).toBeDefined();
    expect(screen.getByText("Unsaved changes")).toBeVisible();
  });

  it("requires an explicit replacement for an unsupported imported pattern", async () => {
    const user = userEvent.setup();
    const snapshot = createMockSnapshot();
    snapshot.profiles[0]!.settings.gathering = {
      ...snapshot.profiles[0]!.settings.gathering,
      enabled: true,
      fields: ["Pine Tree Forest"],
      pattern: "Stationary",
    };
    const actionState = createActions();

    render(
      <GatherPage
        snapshot={snapshot}
        actions={actionState.pageActions}
        pendingAction={null}
      />,
    );

    expect(
      screen.getByText("Replace the imported pattern before starting"),
    ).toBeVisible();
    expect(
      screen.getByRole("button", { name: "Start saved plan" }),
    ).toBeDisabled();

    await user.click(screen.getByRole("button", { name: "Use e_lol instead" }));
    expect(screen.getByText("Unsaved changes")).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Apply plan" }));

    expect(actionState.savedSettings()?.gathering.pattern).toBe("e_lol");
  });
});
