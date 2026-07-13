import { CirclePause, CirclePlay, OctagonX, Square } from "lucide-react";
import type { NectarActions } from "../hooks/useNectarPilot";
import type { RunState } from "../types/contracts";

interface RunControlsProps {
  state: RunState;
  actions: NectarActions;
  pendingAction: string | null;
  compact?: boolean;
  startBlocked?: boolean;
  startBlockedReason?: string;
}

export function RunControls({
  state,
  actions,
  pendingAction,
  compact = false,
  startBlocked = false,
  startBlockedReason,
}: RunControlsProps) {
  const active = state !== "Idle" && state !== "Faulted";
  const paused = state === "Paused";
  const blocked = state === "Preflight" || state === "Stopping";

  return (
    <div
      className={`run-controls ${compact ? "run-controls-compact" : ""}`}
      aria-label="Macro controls"
    >
      {!active ? (
        <button
          className="button button-primary"
          onClick={() => void actions.start()}
          disabled={pendingAction !== null || startBlocked}
          title={startBlocked ? startBlockedReason : undefined}
          aria-label={
            startBlocked
              ? `Start unavailable: ${startBlockedReason ?? "readiness checks are incomplete"}`
              : "Start macro"
          }
        >
          <CirclePlay size={17} />
          Start
          {!compact && <kbd>F1</kbd>}
        </button>
      ) : (
        <button
          className="button button-secondary"
          onClick={() => void actions.pause()}
          disabled={blocked || pendingAction !== null}
        >
          {paused ? <CirclePlay size={17} /> : <CirclePause size={17} />}
          {paused ? "Resume" : "Pause"}
          {!compact && <kbd>F2</kbd>}
        </button>
      )}
      <button
        className="button button-secondary"
        onClick={() => void actions.stop()}
        disabled={!active || blocked || pendingAction !== null}
      >
        <Square size={15} />
        Stop
        {!compact && <kbd>F3</kbd>}
      </button>
      <button
        className="button button-danger-ghost icon-button-with-label"
        onClick={() => void actions.emergencyStop()}
        title="Emergency stop (Ctrl+Shift+F12)"
      >
        <OctagonX size={17} />
        <span className={compact ? "sr-only" : ""}>Emergency stop</span>
      </button>
    </div>
  );
}
