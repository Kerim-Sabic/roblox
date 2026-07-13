import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { createMockSnapshot } from "../services/seed";
import { MonitoringPage } from "./MonitoringPage";

describe("MonitoringPage", () => {
  it("shows only run history owned by the active profile", () => {
    const snapshot = createMockSnapshot();
    const finishedAt = new Date().toISOString();
    snapshot.runHistory = [
      {
        runId: "active-run",
        profileId: snapshot.activeProfileId,
        kind: "legacy_session",
        startedAt: finishedAt,
        finishedAt,
        finalState: "Idle",
        summary: "Active profile completed safely",
        stepsSucceeded: 4,
        stepsFailed: 0,
      },
      {
        runId: "other-run",
        profileId: "01900000-0000-7000-8000-000000000002",
        kind: "legacy",
        startedAt: finishedAt,
        finishedAt,
        finalState: "Faulted",
        summary: "Another profile must stay private",
        stepsSucceeded: 0,
        stepsFailed: 1,
      },
    ];

    render(<MonitoringPage snapshot={snapshot} />);

    expect(screen.getByText("Active profile completed safely")).toBeVisible();
    expect(
      screen.queryByText("Another profile must stay private"),
    ).not.toBeInTheDocument();
    expect(
      screen.getByRole("region", { name: "Live HUD metrics" }),
    ).toBeVisible();
  });
});
