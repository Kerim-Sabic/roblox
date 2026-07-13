import {
  AlertCircle,
  ArrowRight,
  Check,
  ChevronRight,
  Clock3,
  Crosshair,
  ExternalLink,
  Gamepad2,
  Hexagon,
  RefreshCw,
  ShieldCheck,
  Sparkles,
} from "lucide-react";
import type { NectarActions } from "../hooks/useNectarPilot";
import type { DashboardSnapshot, ReadinessCheck } from "../types/contracts";
import { RunControls } from "../components/RunControls";
import { StatusPill } from "../components/StatusPill";

interface OverviewPageProps {
  snapshot: DashboardSnapshot;
  actions: NectarActions;
  pendingAction: string | null;
  onNavigate(page: "gather" | "diagnostics" | "settings"): void;
}

function relativeTime(timestamp: string) {
  const minutes = Math.max(
    0,
    Math.round((Date.now() - new Date(timestamp).getTime()) / 60_000),
  );
  if (minutes < 1) return "just now";
  if (minutes < 60) return `${minutes}m ago`;
  return `${Math.floor(minutes / 60)}h ago`;
}

function ReadinessIcon({ check }: { check: ReadinessCheck }) {
  if (check.status === "ready") return <Check size={15} />;
  return <AlertCircle size={15} />;
}

export function OverviewPage({
  snapshot,
  actions,
  pendingAction,
  onNavigate,
}: OverviewPageProps) {
  const activeTask =
    snapshot.queue.find((task) => task.status === "active") ??
    snapshot.queue.find((task) => task.status === "next");
  const allReady = snapshot.readiness.every(
    (check) => check.status === "ready" || check.status === "warning",
  );

  return (
    <div className="page overview-page">
      <section className="hero-card">
        <div className="hero-status">
          <div className="hero-status-icon">
            <Hexagon size={26} fill="currentColor" />
          </div>
          <div>
            <div className="hero-title-row">
              <h2>
                {snapshot.runState === "Idle"
                  ? "Ready when you are"
                  : snapshot.runStateReason}
              </h2>
              <StatusPill state={snapshot.runState} detail />
            </div>
            <p>
              {snapshot.runState === "Idle"
                ? "Preflight will verify the Roblox window, calibration, and every safety limit before sending input."
                : (activeTask?.detail ??
                  "NectarPilot is supervising the active automation session.")}
            </p>
          </div>
        </div>
        <RunControls
          state={snapshot.runState}
          actions={actions}
          pendingAction={pendingAction}
          startBlocked={!allReady || snapshot.safeMode}
          startBlockedReason={
            snapshot.safeMode
              ? "Acknowledge safe mode in Diagnostics before starting"
              : "Resolve blocked or still-checking readiness items before starting"
          }
        />
      </section>

      {snapshot.safeMode && (
        <div className="inline-alert inline-alert-danger" role="alert">
          <AlertCircle size={18} />
          <div>
            <strong>Safe mode is active</strong>
            <span>
              Automation is disabled after repeated daemon crashes. Review
              diagnostics before restarting.
            </span>
          </div>
          <button
            className="button button-secondary"
            onClick={() => onNavigate("diagnostics")}
          >
            View diagnostics
          </button>
        </div>
      )}

      <section className="metric-grid" aria-label="Session metrics">
        {snapshot.metrics.map((metric, index) => (
          <article
            key={metric.id}
            className={`metric-card metric-${metric.tone ?? "neutral"}`}
          >
            <div className="metric-label-row">
              <span>{metric.label}</span>
              {index === 0 ? (
                <Sparkles size={16} />
              ) : index === 1 ? (
                <Crosshair size={16} />
              ) : index === 2 ? (
                <Clock3 size={16} />
              ) : (
                <Hexagon size={16} />
              )}
            </div>
            <strong>{metric.value}</strong>
            {metric.delta && <small>{metric.delta}</small>}
          </article>
        ))}
      </section>

      <section className="overview-grid">
        <article className="panel panel-plan">
          <header className="panel-header">
            <div>
              <span className="eyebrow">Automation plan</span>
              <h2>Today’s route</h2>
            </div>
            <button
              className="text-button"
              onClick={() => onNavigate("gather")}
            >
              Edit plan <ArrowRight size={15} />
            </button>
          </header>
          <div className="plan-list">
            {snapshot.queue.map((task, index) => (
              <div key={task.id} className={`plan-row plan-${task.status}`}>
                <div className="plan-step">
                  {task.status === "active" ? (
                    <span className="pulse-ring" />
                  ) : (
                    index + 1
                  )}
                </div>
                <div className="plan-copy">
                  <strong>{task.label}</strong>
                  <span>{task.detail}</span>
                </div>
                {task.confidence !== undefined && (
                  <span
                    className={`confidence confidence-${task.confidence >= 0.9 ? "high" : task.confidence >= 0.7 ? "medium" : "low"}`}
                  >
                    {Math.round(task.confidence * 100)}% match
                  </span>
                )}
                <ChevronRight size={16} className="plan-chevron" />
              </div>
            ))}
          </div>
        </article>

        <div className="overview-side-column">
          <article className="panel readiness-panel">
            <header className="panel-header compact-panel-header">
              <div>
                <span className="eyebrow">Preflight</span>
                <h2>{allReady ? "Ready to run" : "Action needed"}</h2>
              </div>
              <ShieldCheck
                size={21}
                className={allReady ? "icon-success" : "icon-warning"}
              />
            </header>
            <div className="readiness-list">
              {snapshot.readiness.map((check) => (
                <div key={check.id} className="readiness-row">
                  <span className={`readiness-icon readiness-${check.status}`}>
                    <ReadinessIcon check={check} />
                  </span>
                  <div>
                    <strong>{check.label}</strong>
                    <span>{check.detail}</span>
                  </div>
                  {check.actionLabel && (
                    <button
                      className="tiny-button"
                      onClick={() =>
                        onNavigate(
                          check.id === "gather-plan" ? "gather" : "settings",
                        )
                      }
                    >
                      {check.actionLabel}
                    </button>
                  )}
                </div>
              ))}
            </div>
          </article>

          <article className="panel session-card">
            <header className="panel-header compact-panel-header">
              <div>
                <span className="eyebrow">Roblox session</span>
                <h2>
                  {snapshot.session.connected
                    ? "Client linked"
                    : "Not connected"}
                </h2>
              </div>
              <Gamepad2
                size={21}
                className={
                  snapshot.session.connected ? "icon-success" : "icon-muted"
                }
              />
            </header>
            <dl className="session-details">
              <div>
                <dt>Window</dt>
                <dd>{snapshot.session.resolution ?? "—"}</dd>
              </div>
              <div>
                <dt>Calibration</dt>
                <dd>
                  <span
                    className={`confidence-dot confidence-${snapshot.session.calibration ?? "low"}`}
                  />
                  {snapshot.session.calibration ?? "—"}
                </dd>
              </div>
              <div>
                <dt>Input target</dt>
                <dd>PID {snapshot.session.pid ?? "—"}</dd>
              </div>
            </dl>
            <button
              className="wide-text-button"
              disabled={pendingAction !== null}
              onClick={() => void actions.refreshSession()}
            >
              <RefreshCw size={14} />
              {pendingAction === "refresh-session"
                ? "Checking Roblox…"
                : "Recheck Roblox"}
            </button>
            <button
              className="wide-text-button"
              onClick={() => onNavigate("diagnostics")}
            >
              Open session details <ExternalLink size={14} />
            </button>
          </article>
        </div>
      </section>

      <section className="panel timeline-panel">
        <header className="panel-header">
          <div>
            <span className="eyebrow">Recent activity</span>
            <h2>Safety & task timeline</h2>
          </div>
          <button
            className="text-button"
            onClick={() => onNavigate("diagnostics")}
          >
            View all <ArrowRight size={15} />
          </button>
        </header>
        <div className="timeline-list">
          {snapshot.timeline.slice(0, 4).map((entry) => (
            <div key={entry.id} className="timeline-entry">
              <span className={`timeline-dot timeline-${entry.tone}`} />
              <div>
                <strong>{entry.title}</strong>
                <span>{entry.detail}</span>
              </div>
              <time dateTime={entry.timestamp}>
                {relativeTime(entry.timestamp)}
              </time>
            </div>
          ))}
        </div>
      </section>
    </div>
  );
}
