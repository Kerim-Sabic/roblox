import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { NectarActions } from "../hooks/useNectarPilot";
import { createMockSnapshot } from "../services/seed";
import { OnboardingDialog } from "./OnboardingDialog";

describe("OnboardingDialog", () => {
  it("stays open when setup persistence is rejected", async () => {
    const user = userEvent.setup();
    const completeOnboarding = vi.fn().mockResolvedValue(false);
    const onClose = vi.fn();
    const snapshot = createMockSnapshot();
    snapshot.onboardingComplete = false;

    render(
      <OnboardingDialog
        snapshot={snapshot}
        actions={{ completeOnboarding } as unknown as NectarActions}
        open
        required
        pending={false}
        onClose={onClose}
      />,
    );

    await user.click(screen.getByRole("button", { name: /Continue/ }));
    await user.click(screen.getByRole("button", { name: /Continue/ }));
    await user.click(screen.getByRole("button", { name: "Finish setup" }));

    await waitFor(() => expect(completeOnboarding).toHaveBeenCalledTimes(1));
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByRole("dialog")).toBeVisible();
  });

  it("closes only after setup persistence succeeds", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    const snapshot = createMockSnapshot();
    snapshot.onboardingComplete = false;

    render(
      <OnboardingDialog
        snapshot={snapshot}
        actions={
          {
            completeOnboarding: vi.fn().mockResolvedValue(true),
          } as unknown as NectarActions
        }
        open
        required
        pending={false}
        onClose={onClose}
      />,
    );

    await user.click(screen.getByRole("button", { name: /Continue/ }));
    await user.click(screen.getByRole("button", { name: /Continue/ }));
    await user.click(screen.getByRole("button", { name: "Finish setup" }));

    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  });
});
