import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { NectarActions } from "../hooks/useNectarPilot";
import { RunControls } from "./RunControls";

const actions: NectarActions = {
  refreshSession: vi.fn(),
  start: vi.fn(),
  pause: vi.fn(),
  stop: vi.fn(),
  emergencyStop: vi.fn(),
  selectProfile: vi.fn(),
  saveSettings: vi.fn(),
  completeOnboarding: vi.fn(),
  trustExtension: vi.fn(),
  runLegacyExtension: vi.fn(),
  startLegacySession: vi.fn(),
  inspectLegacy: vi.fn(),
  setCompactMode: vi.fn(),
};

describe("RunControls", () => {
  it("cannot start while readiness is unresolved", () => {
    render(
      <RunControls
        state="Idle"
        actions={actions}
        pendingAction={null}
        startBlocked
        startBlockedReason="Roblox window is not verified"
      />,
    );

    expect(
      screen.getByRole("button", {
        name: "Start unavailable: Roblox window is not verified",
      }),
    ).toBeDisabled();
  });
});
