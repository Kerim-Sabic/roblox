import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { NectarActions } from "../hooks/useNectarPilot";
import { createMockSnapshot } from "../services/seed";
import { DiagnosticsPage } from "./DiagnosticsPage";

describe("DiagnosticsPage", () => {
  it("does not claim an export was created when support-bundle export is unavailable", () => {
    render(
      <DiagnosticsPage
        snapshot={createMockSnapshot()}
        actions={{} as NectarActions}
        pendingAction={null}
      />,
    );

    expect(
      screen.getByRole("button", { name: "Export unavailable" }),
    ).toBeDisabled();
    expect(
      screen.getByRole("button", { name: "Copy visible logs" }),
    ).toBeDisabled();
    expect(
      screen.getByRole("button", { name: "Clear log view" }),
    ).toBeDisabled();
    expect(screen.getByText("Support-bundle export unavailable")).toBeVisible();
  });
});
