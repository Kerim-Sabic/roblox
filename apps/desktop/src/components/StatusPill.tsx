import {
  AlertTriangle,
  CircleDot,
  LoaderCircle,
  Pause,
  ShieldAlert,
  Square,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import type { RunState } from "../types/contracts";

const statusPresentation: Record<
  RunState,
  { label: string; tone: string; icon: LucideIcon }
> = {
  Idle: { label: "Ready", tone: "neutral", icon: Square },
  Preflight: { label: "Preflight", tone: "info", icon: LoaderCircle },
  Running: { label: "Running", tone: "success", icon: CircleDot },
  Paused: { label: "Paused", tone: "warning", icon: Pause },
  Recovering: { label: "Recovering", tone: "info", icon: LoaderCircle },
  NeedsAttention: {
    label: "Needs attention",
    tone: "warning",
    icon: AlertTriangle,
  },
  Stopping: { label: "Stopping", tone: "neutral", icon: LoaderCircle },
  Faulted: { label: "Faulted", tone: "danger", icon: ShieldAlert },
};

export function StatusPill({
  state,
  detail = false,
}: {
  state: RunState;
  detail?: boolean;
}) {
  const presentation = statusPresentation[state];
  const Icon = presentation.icon;
  return (
    <span
      className={`status-pill status-${presentation.tone}`}
      aria-label={`Macro status: ${presentation.label}`}
    >
      <Icon
        size={detail ? 16 : 14}
        className={
          state === "Preflight" ||
          state === "Recovering" ||
          state === "Stopping"
            ? "spin"
            : undefined
        }
      />
      {presentation.label}
    </span>
  );
}
