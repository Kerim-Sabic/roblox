import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { NectarActions } from "../hooks/useNectarPilot";
import { createMockSnapshot } from "../services/seed";
import { FeaturePage } from "./FeaturePage";

describe("FeaturePage", () => {
  it("does not present unavailable category scheduling as executable", () => {
    render(
      <FeaturePage
        category="activity"
        eyebrow="Activities"
        title="Activities"
        description="Activity preferences"
        snapshot={createMockSnapshot()}
        actions={{} as NectarActions}
        pendingAction={null}
      />,
    );

    expect(
      screen.getByText(
        /native activity, boost, quest, and planter scheduling is not connected/i,
      ),
    ).toBeVisible();
    expect(
      screen.getByRole("button", {
        name: "Configure Daily collections unavailable",
      }),
    ).toBeDisabled();
  });
});
