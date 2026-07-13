import { ArrowUpRight, Hexagon } from "lucide-react";
import type { NectarActions } from "../hooks/useNectarPilot";
import type { DashboardSnapshot } from "../types/contracts";
import { RunControls } from "./RunControls";
import { StatusPill } from "./StatusPill";

interface CompactMonitorProps {
  snapshot: DashboardSnapshot;
  actions: NectarActions;
  pendingAction: string | null;
  onExpand(): void;
}

export function CompactMonitor({
  snapshot,
  actions,
  pendingAction,
  onExpand,
}: CompactMonitorProps) {
  const currentTask =
    snapshot.queue.find((task) => task.status === "active") ??
    snapshot.queue.find((task) => task.status === "next");
  return (
    <main className="compact-monitor" aria-label="NectarPilot compact monitor">
      <header className="compact-header">
        <div className="compact-brand">
          <Hexagon size={20} fill="currentColor" />
          <strong>NectarPilot</strong>
        </div>
        <div className="compact-header-actions">
          <StatusPill state={snapshot.runState} />
          <button
            className="icon-button icon-button-small"
            onClick={onExpand}
            aria-label="Return to dashboard"
            title="Return to dashboard"
          >
            <ArrowUpRight size={17} />
          </button>
        </div>
      </header>
      <section className="compact-task">
        <div>
          <span className="eyebrow">
            {snapshot.runState === "Running" ? "Now gathering" : "Up next"}
          </span>
          <h1>{currentTask?.label ?? "No task queued"}</h1>
          <p>{currentTask?.detail ?? snapshot.runStateReason}</p>
        </div>
        <div
          className="compact-progress-ring"
          style={{ "--progress": "68%" } as React.CSSProperties}
        >
          <strong>68%</strong>
          <span>bag</span>
        </div>
      </section>
      <section className="compact-stats" aria-label="Session statistics">
        <span>
          <strong>{snapshot.metrics[0]?.value ?? "—"}</strong> honey
        </span>
        <span>
          <strong>{snapshot.metrics[1]?.value ?? "—"}</strong> rate
        </span>
        <span>
          <strong>{snapshot.session.calibration ?? "—"}</strong> confidence
        </span>
      </section>
      <RunControls
        state={snapshot.runState}
        actions={actions}
        pendingAction={pendingAction}
        startBlocked={
          snapshot.safeMode ||
          snapshot.readiness.some(
            (check) =>
              check.status === "blocked" || check.status === "checking",
          )
        }
        startBlockedReason="Open the dashboard and resolve readiness checks before starting"
        compact
      />
    </main>
  );
}
