import { describe, expect, it } from "vitest";
import { detectionCanTarget, toUiRunState, type Detection } from "./contracts";

describe("detectionCanTarget", () => {
  it("allows only confident found detections to become targets", () => {
    expect(
      detectionCanTarget({
        status: "found",
        value: "Pine Tree Forest",
        confidence: 0.98,
      }),
    ).toBe(true);
    expect(
      detectionCanTarget({
        status: "found",
        value: "Pine Tree Forest",
        confidence: 0.4,
      }),
    ).toBe(false);
  });

  it("never turns uncertain Brown Bear OCR into a movement target", () => {
    const detection: Detection<string> = {
      status: "uncertain",
      candidates: ["Brown Bear", "Black Bear"],
      reason: "OCR consensus did not converge",
    };
    expect(detectionCanTarget(detection)).toBe(false);
  });
});

describe("toUiRunState", () => {
  it("adapts the lower-snake Rust wire state for presentation", () => {
    expect(toUiRunState("needs_attention")).toBe("NeedsAttention");
    expect(toUiRunState("preflight")).toBe("Preflight");
  });
});
