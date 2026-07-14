import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import App from "./App";
import { MockNectarService } from "./services/nectar-service";

function renderDesktop() {
  return render(<App service={new MockNectarService()} />);
}

describe("NectarPilot desktop", () => {
  it("renders the ready dashboard with all navigation destinations", async () => {
    renderDesktop();

    expect(
      await screen.findByRole("heading", { name: "Ready when you are" }),
    ).toBeInTheDocument();
    const navigation = screen.getByRole("navigation", {
      name: "Main navigation",
    });
    for (const label of [
      "Overview",
      "Gather",
      "Activities",
      "Boosts",
      "Quests",
      "Planters",
      "Monitoring",
      "Extensions",
      "Settings",
      "Diagnostics",
      "About",
    ]) {
      expect(
        within(navigation).getByRole("button", { name: label }),
      ).toBeInTheDocument();
    }
    expect(
      screen.getByText("Wrong-window input is blocked"),
    ).toBeInTheDocument();
  });

  it("opens the compact running monitor and can return to the dashboard", async () => {
    const user = userEvent.setup();
    renderDesktop();
    await screen.findByRole("heading", { name: "Ready when you are" });

    await user.click(
      screen.getByRole("button", { name: "Open compact monitor" }),
    );
    expect(
      screen.getByRole("main", { name: "NectarPilot compact monitor" }),
    ).toBeInTheDocument();
    expect(screen.getByText("Up next")).toBeInTheDocument();

    await user.click(
      screen.getByRole("button", { name: "Return to dashboard" }),
    );
    expect(
      await screen.findByRole("heading", { name: "Ready when you are" }),
    ).toBeInTheDocument();
  });

  it("keeps safety budget edits in a draft until Apply", async () => {
    const user = userEvent.setup();
    renderDesktop();
    await screen.findByRole("heading", { name: "Ready when you are" });

    await user.click(screen.getByRole("button", { name: "Settings" }));
    await user.click(screen.getByRole("button", { name: "Safety & budgets" }));
    const dice = screen.getByRole("spinbutton", { name: "Field dice budget" });
    expect(dice).toHaveValue(0);
    await user.clear(dice);
    await user.type(dice, "2");

    expect(screen.getByText("Unsaved changes")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Apply" }));
    await waitFor(() =>
      expect(screen.queryByText("Unsaved changes")).not.toBeInTheDocument(),
    );
  });

  it("keeps movement calibration editable and saves it through the desktop service", async () => {
    const user = userEvent.setup();
    renderDesktop();
    await screen.findByRole("heading", { name: "Ready when you are" });

    await user.click(screen.getByRole("button", { name: "Settings" }));
    await user.click(screen.getByRole("button", { name: "Movement" }));
    const walkSpeed = screen.getByRole("spinbutton", { name: "Walk speed" });
    await user.clear(walkSpeed);
    await user.type(walkSpeed, "34.5");
    expect(screen.getByText("Unsaved changes")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Apply" }));

    await waitFor(() => expect(walkSpeed).toHaveValue(34.5));
    expect(screen.queryByText("Unsaved changes")).not.toBeInTheDocument();
  });

  it("requires explicit confirmation before trusting a legacy extension", async () => {
    const user = userEvent.setup();
    renderDesktop();
    await screen.findByRole("heading", { name: "Ready when you are" });

    await user.click(screen.getByRole("button", { name: "Extensions" }));
    await user.click(screen.getByRole("button", { name: /Review & trust/ }));
    const trustButton = screen.getByRole("button", {
      name: "Trust exact digest",
    });
    expect(trustButton).toBeDisabled();
    await user.click(
      screen.getByRole("checkbox", { name: /I reviewed these capabilities/ }),
    );
    expect(trustButton).toBeEnabled();
  });

  it("offers the contained compatibility run only after exact-digest trust", async () => {
    const user = userEvent.setup();
    renderDesktop();
    await screen.findByRole("heading", { name: "Ready when you are" });

    await user.click(screen.getByRole("button", { name: "Extensions" }));
    await user.click(screen.getByRole("button", { name: /Review & trust/ }));
    await user.click(
      screen.getByRole("checkbox", { name: /I reviewed these capabilities/ }),
    );
    await user.click(
      screen.getByRole("button", { name: "Trust exact digest" }),
    );

    expect(
      await screen.findByRole("button", { name: "Run contained script" }),
    ).toBeEnabled();
  });

  it("keeps the converted Stationary pattern out of the legacy runner", async () => {
    const user = userEvent.setup();
    renderDesktop();
    await screen.findByRole("heading", { name: "Ready when you are" });

    await user.click(screen.getByRole("button", { name: "Extensions" }));
    const heading = screen.getByRole("heading", {
      name: "Pattern · Stationary",
    });
    const card = heading.closest("article");
    expect(card).not.toBeNull();
    expect(
      within(card as HTMLElement).getByLabelText(
        "Native conversion preview; execution unavailable",
      ),
    ).toBeInTheDocument();
    expect(
      within(card as HTMLElement).queryByRole("button", {
        name: "Run contained script",
      }),
    ).not.toBeInTheDocument();
  });

  it("shows the versioned Science Bear planner instead of pretending quest OCR is ready", async () => {
    const user = userEvent.setup();
    renderDesktop();
    await screen.findByRole("heading", { name: "Ready when you are" });

    await user.click(screen.getByRole("button", { name: "Quests" }));
    expect(
      screen.getByRole("heading", { name: "Science Bear knowledge pack" }),
    ).toBeInTheDocument();
    expect(screen.getByText("31")).toBeInTheDocument();
    expect(
      screen.getByText("Awaiting a confident in-game quest scan"),
    ).toBeInTheDocument();
  });
});
