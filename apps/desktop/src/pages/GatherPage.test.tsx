import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { NectarActions } from "../hooks/useNectarPilot";
import { createMockSnapshot } from "../services/seed";
import type {
  AutomationSettings,
  ExtensionManifest,
} from "../types/contracts";
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

function trustedLegacyAsset(id: string): ExtensionManifest {
  return {
    id,
    name: id,
    author: "Natro Team contributors",
    version: "1.1.2",
    description: "Pinned compatibility asset",
    digest: "a".repeat(64),
    trust: "trusted",
    permissions: ["Keyboard input", "Mouse input"],
    enabled: true,
    executionMode: "legacy_bridge",
  };
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

  it("hydrates a placeholder Gather draft when the daemon profile arrives", async () => {
    const placeholder = createMockSnapshot();
    placeholder.profiles[0]!.settings.gathering = {
      ...placeholder.profiles[0]!.settings.gathering,
      enabled: false,
      fields: [],
    };
    const hydrated = createMockSnapshot();
    const actionState = createActions();
    const { rerender } = render(
      <GatherPage
        snapshot={placeholder}
        actions={actionState.pageActions}
        pendingAction={null}
      />,
    );

    expect(screen.getByText("Add your first field")).toBeVisible();

    // Startup can project a temporary profile with the final profile ID before
    // its document is delivered on the pipe.  The page must replace that
    // clean placeholder rather than leaving a real saved plan empty.
    rerender(
      <GatherPage
        snapshot={hydrated}
        actions={actionState.pageActions}
        pendingAction={null}
      />,
    );

    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Remove Pine Tree Forest" }),
      ).toBeVisible(),
    );
    expect(screen.queryByText("Add your first field")).not.toBeInTheDocument();
  });

  it("accepts Natro field aliases without falsely blocking a saved plan", async () => {
    const user = userEvent.setup();
    const snapshot = createMockSnapshot();
    snapshot.profiles[0]!.settings.gathering = {
      ...snapshot.profiles[0]!.settings.gathering,
      enabled: true,
      fields: ["Sunflower", "Pine Tree"],
      pattern: "e_lol",
    };
    snapshot.extensions = [
      trustedLegacyAsset("legacy:route:paths/gtf-sunflower.ahk"),
      trustedLegacyAsset("legacy:route:paths/gtf-pinetree.ahk"),
      trustedLegacyAsset("legacy:pattern:patterns/e_lol.ahk"),
    ];
    const actionState = createActions();

    render(
      <GatherPage
        snapshot={snapshot}
        actions={actionState.pageActions}
        pendingAction={null}
      />,
    );

    expect(screen.queryByText("Selected assets are unavailable")).not.toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Start saved plan" }),
    ).toBeEnabled();

    // "Sunflower" and "Sunflower Field" are the same pinned legacy route;
    // do not let a user add it twice merely because it was imported under the
    // older short name.
    await user.selectOptions(
      screen.getByLabelText("Field to add"),
      "Sunflower Field",
    );
    expect(screen.getByRole("button", { name: /Add field/ })).toBeDisabled();
  });
});
