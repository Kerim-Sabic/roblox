import {
  AlertTriangle,
  CheckCircle2,
  Copy,
  Database,
  Download,
  Filter,
  MonitorCog,
  Search,
  ShieldCheck,
  Trash2,
} from "lucide-react";
import { useMemo, useState } from "react";
import type { NectarActions } from "../hooks/useNectarPilot";
import type { DashboardSnapshot, DiagnosticLog } from "../types/contracts";

const levels: Array<DiagnosticLog["level"] | "all"> = [
  "all",
  "info",
  "warning",
  "error",
  "debug",
];

export function DiagnosticsPage({
  snapshot,
  actions,
  pendingAction,
}: {
  snapshot: DashboardSnapshot;
  actions: NectarActions;
  pendingAction: string | null;
}) {
  const [level, setLevel] = useState<(typeof levels)[number]>("all");
  const [query, setQuery] = useState("");
  const [bundleReady, setBundleReady] = useState(false);
  const logs = useMemo(
    () =>
      snapshot.logs.filter((log) => {
        const matchesLevel = level === "all" || log.level === level;
        const haystack = `${log.component} ${log.message}`.toLowerCase();
        return matchesLevel && haystack.includes(query.toLowerCase());
      }),
    [level, query, snapshot.logs],
  );

  return (
    <div className="page">
      <section className="page-heading">
        <div>
          <span className="eyebrow">Evidence-backed health</span>
          <h2>Diagnostics</h2>
          <p>
            Inspect ownership, calibration, recovery, and redacted logs without
            exposing secrets.
          </p>
        </div>
        <div className="draft-actions">
          <button
            className="button button-secondary"
            disabled={pendingAction !== null}
            onClick={() => void actions.refreshSession()}
          >
            Refresh session
          </button>
          <button
            className="button button-primary"
            onClick={() => setBundleReady(true)}
          >
            <Download size={16} /> Export support bundle
          </button>
        </div>
      </section>
      {(snapshot.safeMode ||
        snapshot.runState === "Faulted" ||
        snapshot.runState === "NeedsAttention") && (
        <div className="inline-alert inline-alert-danger" role="alert">
          <AlertTriangle size={18} />
          <div>
            <strong>Automation needs acknowledgement</strong>
            <span>
              {snapshot.runStateReason ??
                "Review the visible log reason before allowing another run."}
            </span>
          </div>
          <button
            className="button button-secondary"
            disabled={pendingAction !== null}
            onClick={() => void actions.acknowledgeAttention()}
          >
            Acknowledge & reset
          </button>
        </div>
      )}
      {bundleReady && (
        <div className="inline-alert inline-alert-success" role="status">
          <CheckCircle2 size={18} />
          <div>
            <strong>Redacted bundle prepared</strong>
            <span>
              Full-screen captures, private links, tokens, and account
              identifiers were excluded.
            </span>
          </div>
          <button
            className="icon-button"
            aria-label="Dismiss bundle notification"
            onClick={() => setBundleReady(false)}
          >
            ×
          </button>
        </div>
      )}
      <section className="diagnostic-status-grid">
        <DiagnosticStatus
          icon={<MonitorCog />}
          label="Roblox session"
          value={
            snapshot.session.connected
              ? snapshot.session.foreground
                ? "Verified & foreground"
                : "Found — input paused"
              : "Not detected"
          }
          detail={`PID ${snapshot.session.pid ?? "—"} · ${snapshot.session.resolution ?? "Unknown"}`}
          tone={
            snapshot.session.connected && snapshot.session.foreground
              ? "success"
              : "warning"
          }
        />
        <DiagnosticStatus
          icon={<ShieldCheck />}
          label="Input broker"
          value={
            snapshot.session.foreground
              ? "Guarded"
              : "Waiting for verified focus"
          }
          detail={
            snapshot.session.foreground
              ? "Input stays scoped to the adopted Roblox window"
              : "No input can be sent until Roblox is foreground"
          }
          tone={snapshot.session.foreground ? "success" : "warning"}
        />
        <DiagnosticStatus
          icon={<Database />}
          label="Runtime database"
          value={
            snapshot.runState === "Faulted" ? "Needs recovery" : "Connected"
          }
          detail={
            snapshot.safeMode
              ? "Safe mode is active after repeated daemon crashes"
              : "The daemon is the only configuration and runtime-state writer"
          }
          tone={
            snapshot.safeMode || snapshot.runState === "Faulted"
              ? "warning"
              : "success"
          }
        />
        <DiagnosticStatus
          icon={<AlertTriangle />}
          label="Vision evidence"
          value={
            snapshot.readiness.find((check) => check.id === "native-detectors")
              ?.status === "ready"
              ? "Ready"
              : "Compatibility mode"
          }
          detail={
            snapshot.readiness.find((check) => check.id === "native-detectors")
              ?.detail ?? "No detector status was reported yet"
          }
          tone="warning"
        />
      </section>
      <section className="panel session-inspector">
        <header className="panel-header">
          <div>
            <span className="eyebrow">Adopted process</span>
            <h2>Session inspector</h2>
          </div>
          <span className="safe-default-badge">
            <ShieldCheck size={15} /> Exact process only
          </span>
        </header>
        <dl>
          <div>
            <dt>Executable</dt>
            <dd>{snapshot.session.processName ?? "Not connected"}</dd>
          </div>
          <div>
            <dt>Window title</dt>
            <dd>{snapshot.session.windowTitle ?? "—"}</dd>
          </div>
          <div>
            <dt>Client area</dt>
            <dd>{snapshot.session.resolution ?? "—"}</dd>
          </div>
          <div>
            <dt>Windows scale</dt>
            <dd>{snapshot.session.dpi ?? "—"}%</dd>
          </div>
          <div>
            <dt>Foreground</dt>
            <dd>
              {snapshot.session.foreground ? "Verified" : "No — input paused"}
            </dd>
          </div>
          <div>
            <dt>Calibration</dt>
            <dd>{snapshot.session.calibration ?? "Not calibrated"}</dd>
          </div>
        </dl>
      </section>
      <section className="panel log-panel">
        <header className="panel-header">
          <div>
            <span className="eyebrow">Structured events</span>
            <h2>Live logs</h2>
          </div>
          <div className="log-actions">
            <button
              className="icon-button"
              title="Copy visible logs"
              aria-label="Copy visible logs"
            >
              <Copy size={16} />
            </button>
            <button
              className="icon-button"
              title="Clear view"
              aria-label="Clear log view"
            >
              <Trash2 size={16} />
            </button>
          </div>
        </header>
        <div className="log-toolbar">
          <label className="search-input">
            <Search size={15} />
            <input
              aria-label="Search logs"
              type="search"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder="Search component or message"
            />
          </label>
          <label className="filter-select">
            <Filter size={15} />
            <select
              aria-label="Filter log level"
              value={level}
              onChange={(event) => setLevel(event.target.value as typeof level)}
            >
              {levels.map((item) => (
                <option key={item} value={item}>
                  {item === "all" ? "All levels" : item}
                </option>
              ))}
            </select>
          </label>
          <span>{logs.length} events</span>
        </div>
        <div className="log-table" role="table" aria-label="Diagnostic logs">
          <div className="log-table-head" role="row">
            <span>Time</span>
            <span>Level</span>
            <span>Component</span>
            <span>Message</span>
          </div>
          {logs.map((log) => (
            <div key={log.id} className="log-table-row" role="row">
              <time>
                {new Date(log.timestamp).toLocaleTimeString([], {
                  hour: "2-digit",
                  minute: "2-digit",
                  second: "2-digit",
                })}
              </time>
              <span className={`log-level log-${log.level}`}>{log.level}</span>
              <code>{log.component}</code>
              <span>{log.message}</span>
            </div>
          ))}
          {logs.length === 0 && (
            <div className="empty-log">No events match this filter.</div>
          )}
        </div>
      </section>
    </div>
  );
}

function DiagnosticStatus({
  icon,
  label,
  value,
  detail,
  tone,
}: {
  icon: React.ReactNode;
  label: string;
  value: string;
  detail: string;
  tone: "success" | "warning";
}) {
  return (
    <article className="panel diagnostic-status">
      <span className={`diagnostic-icon diagnostic-${tone}`}>{icon}</span>
      <div>
        <small>{label}</small>
        <strong>{value}</strong>
        <span>{detail}</span>
      </div>
    </article>
  );
}
