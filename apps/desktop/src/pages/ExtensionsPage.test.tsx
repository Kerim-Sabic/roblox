import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { NectarActions } from "../hooks/useNectarPilot";
import { createMockSnapshot } from "../services/seed";
import { ExtensionsPage } from "./ExtensionsPage";

describe("ExtensionsPage", () => {
  it("labels pinned asset metadata separately from the generated harness", () => {
    const snapshot = createMockSnapshot();
    snapshot.legacyInspection = {
      scriptId: "legacy:pattern:patterns/e_lol.ahk",
      sha256: "a".repeat(64),
      bytes: 42,
      harnessPreview: "; generated harness source",
    };

    render(
      <ExtensionsPage
        snapshot={snapshot}
        actions={{} as NectarActions}
        pendingAction={null}
      />,
    );

    expect(screen.getByText("Pinned asset size")).toBeVisible();
    expect(screen.getByText("42 bytes")).toBeVisible();
    expect(screen.getByText("Pinned asset SHA-256")).toBeVisible();
    expect(screen.getByText("a".repeat(64))).toBeVisible();
    expect(screen.getByText("; generated harness source")).toBeVisible();
  });

  it("keeps the trust review open when the daemon rejects the digest", async () => {
    const user = userEvent.setup();
    const trustExtension = vi.fn().mockResolvedValue(false);

    render(
      <ExtensionsPage
        snapshot={createMockSnapshot()}
        actions={{ trustExtension } as unknown as NectarActions}
        pendingAction={null}
      />,
    );

    await user.click(screen.getByRole("button", { name: /Review & trust/ }));
    await user.click(
      screen.getByRole("checkbox", { name: /I reviewed these capabilities/ }),
    );
    await user.click(
      screen.getByRole("button", { name: "Trust exact digest" }),
    );

    await waitFor(() => expect(trustExtension).toHaveBeenCalledTimes(1));
    expect(screen.getByRole("dialog")).toBeVisible();
  });

  it("reports a rejected contained-run request instead of implying it started", async () => {
    const user = userEvent.setup();
    const snapshot = createMockSnapshot();
    snapshot.extensions = snapshot.extensions.map((extension) =>
      extension.id === "legacy:pattern:patterns/e_lol.ahk"
        ? { ...extension, trust: "trusted" as const }
        : extension,
    );
    const runLegacyExtension = vi.fn().mockResolvedValue(false);

    render(
      <ExtensionsPage
        snapshot={snapshot}
        actions={{ runLegacyExtension } as unknown as NectarActions}
        pendingAction={null}
      />,
    );

    await user.click(
      screen.getByRole("button", { name: "Run contained script" }),
    );

    await waitFor(() => expect(runLegacyExtension).toHaveBeenCalledTimes(1));
    expect(screen.getByRole("status")).toHaveTextContent(
      "Run request rejected",
    );
    expect(screen.getByText(/no script was started/i)).toBeVisible();
  });

  it("marks unavailable extension operations as disabled", () => {
    render(
      <ExtensionsPage
        snapshot={createMockSnapshot()}
        actions={{} as NectarActions}
        pendingAction={null}
      />,
    );

    expect(
      screen.getByRole("button", { name: "Import extension" }),
    ).toBeDisabled();
    expect(
      screen.getByRole("button", { name: "Compatibility settings" }),
    ).toBeDisabled();
    expect(
      screen.getByRole("button", { name: "Inspect change" }),
    ).toBeDisabled();
  });
});
