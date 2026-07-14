import { waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { DaemonEventEnvelope } from "../types/contracts";
import { createMockSnapshot } from "./seed";

const invoke = vi.fn();
let eventHandler:
  | ((event: { payload: DaemonEventEnvelope }) => void)
  | undefined;
const listen = vi.fn(
  async (
    _eventName: string,
    handler: (event: { payload: DaemonEventEnvelope }) => void,
  ) => {
    eventHandler = handler;
    return vi.fn();
  },
);

vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen }));

const { TauriNectarService } = await import("./nectar-service");

function daemonEvent(type: string): DaemonEventEnvelope {
  return {
    protocol_version: 3,
    sequence: 1,
    run_id: "run",
    timestamp: new Date().toISOString(),
    event: { type },
  };
}

describe("TauriNectarService events", () => {
  beforeEach(() => {
    invoke.mockReset();
    listen.mockClear();
    eventHandler = undefined;
  });

  it("keeps heartbeat snapshots live but skips their redundant acknowledgement", async () => {
    const service = new TauriNectarService();
    const listener = vi.fn();
    service.subscribe(listener);
    await waitFor(() => expect(eventHandler).toBeDefined());

    eventHandler?.({ payload: daemonEvent("command_accepted") });
    await Promise.resolve();
    expect(invoke).not.toHaveBeenCalled();

    const snapshot = createMockSnapshot();
    invoke.mockResolvedValueOnce(snapshot);
    eventHandler?.({ payload: daemonEvent("snapshot") });
    await waitFor(() => expect(listener).toHaveBeenCalledWith(snapshot));
    expect(invoke).toHaveBeenCalledWith("get_dashboard_snapshot");
  });

  it("reports a background refresh failure instead of leaving stale status silently visible", async () => {
    const service = new TauriNectarService();
    const onError = vi.fn();
    service.subscribe(vi.fn(), onError);
    await waitFor(() => expect(eventHandler).toBeDefined());

    const failure = new Error("daemon connection was interrupted");
    invoke.mockRejectedValueOnce(failure);
    eventHandler?.({ payload: daemonEvent("snapshot") });

    await waitFor(() => expect(onError).toHaveBeenCalledWith(failure));
  });
});
