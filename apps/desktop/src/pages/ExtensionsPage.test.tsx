import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
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
});
