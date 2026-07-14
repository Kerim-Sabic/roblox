import {
  Activity,
  BellRing,
  Camera,
  CloudOff,
  Database,
  Eye,
  History,
  LockKeyhole,
  ShieldCheck,
} from "lucide-react";
import { activeProfile, type DashboardSnapshot } from "../types/contracts";

export function MonitoringPage({
  snapshot,
  onOpenSettings,
}: {
  snapshot: DashboardSnapshot;
  onOpenSettings(): void;
}) {
  const profile = activeProfile(snapshot);
  const monitoring = profile.settings.monitoring;
  const metrics = snapshot.metrics ?? [];
  const runHistory = (snapshot.runHistory ?? []).filter(
    (record) => record.profileId === snapshot.activeProfileId,
  );
  return (
    <div className="page">
      <section className="page-heading">
        <div>
          <span className="eyebrow">Local-first visibility</span>
          <h2>Monitoring</h2>
          <p>
            Understand the session without giving optional services control by
            default.
          </p>
        </div>
        <span className="safe-default-badge">
          <LockKeyhole size={16} /> Diagnostics stay local
        </span>
      </section>
      {metrics.length > 0 && (
        <section className="stat-metrics-grid" aria-label="Live HUD metrics">
          {metrics.map((metric) => (
            <article
              className={`panel stat-metric-card stat-metric-${metric.tone ?? "neutral"}`}
              key={metric.id}
            >
              <span>{metric.label}</span>
              <strong>{metric.value}</strong>
              <small>{metric.delta ?? "Confident HUD readings only"}</small>
            </article>
          ))}
        </section>
      )}
      <section className="panel run-history-panel">
        <header className="panel-header">
          <div>
            <span className="eyebrow">Run history</span>
            <h2>
              <History size={18} /> Recent runs
            </h2>
          </div>
        </header>
        {runHistory.length === 0 ? (
          <p className="run-history-empty">
            No completed runs recorded for {profile.name} yet.
          </p>
        ) : (
          <div className="run-history-list">
            {runHistory.slice(0, 10).map((record) => (
              <article className="run-history-entry" key={record.runId}>
                <span
                  className={`run-history-state run-history-state-${record.finalState.toLocaleLowerCase()}`}
                >
                  {record.finalState}
                </span>
                <div className="run-history-copy">
                  <strong>{record.kind.replaceAll("_", " ")}</strong>
                  <p>{record.summary}</p>
                </div>
                <div className="run-history-meta">
                  <span>
                    {record.stepsSucceeded} succeeded · {record.stepsFailed}{" "}
                    failed
                  </span>
                  <time dateTime={record.finishedAt}>
                    {new Date(record.finishedAt).toLocaleString()}
                  </time>
                </div>
              </article>
            ))}
          </div>
        )}
      </section>
      <section className="monitoring-hero-grid">
        <article className="panel monitor-status-card">
          <div className="monitor-orb">
            <Activity size={28} />
          </div>
          <div>
            <span className="eyebrow">Live supervision</span>
            <h2>All core systems nominal</h2>
            <p>
              Session ownership, input release, vision confidence, and scheduler
              health are being checked.
            </p>
          </div>
          <div className="health-bar-list">
            <span>
              <i style={{ width: "98%" }} />
              Input guard<strong>98%</strong>
            </span>
            <span>
              <i style={{ width: "94%" }} />
              Vision consensus<strong>94%</strong>
            </span>
            <span>
              <i style={{ width: "100%" }} />
              Scheduler<strong>100%</strong>
            </span>
          </div>
        </article>
        <article className="panel discord-card">
          <span className="discord-icon">
            <CloudOff size={22} />
          </span>
          <div>
            <span className="eyebrow">Discord</span>
            <h2>
              {monitoring.discordEnabled
                ? "Connected with limits"
                : "Remote control is off"}
            </h2>
            <p>
              {monitoring.discordEnabled
                ? "Only explicitly granted capabilities are available."
                : "No bot is connected and no session data leaves this device."}
            </p>
          </div>
          <button className="button button-secondary" onClick={onOpenSettings}>
            Configure permissions
          </button>
        </article>
      </section>
      <section className="monitor-cards-grid">
        <MonitorCard
          icon={<Camera />}
          title="Failure evidence"
          value="18 MB"
          detail={`Cropped captures · ${monitoring.evidenceRetentionDays}-day retention`}
          status="Local only"
        />
        <MonitorCard
          icon={<Database />}
          title="Event database"
          value="4,218"
          detail="Structured task and recovery events"
          status="Healthy"
        />
        <MonitorCard
          icon={<BellRing />}
          title="Alerts"
          value="2 rules"
          detail="Needs Attention and emergency stop"
          status="Desktop only"
        />
        <MonitorCard
          icon={<Eye />}
          title="Vision checks"
          value="98.2%"
          detail="Average temporal consensus"
          status="High confidence"
        />
      </section>
      <section className="panel privacy-panel">
        <header className="panel-header">
          <div>
            <span className="eyebrow">Privacy guardrails</span>
            <h2>What leaves this device</h2>
          </div>
          <ShieldCheck className="icon-success" />
        </header>
        <div className="privacy-grid">
          <div>
            <strong>Session status</strong>
            <span>Not shared</span>
          </div>
          <div>
            <strong>Screenshots</strong>
            <span>Not shared</span>
          </div>
          <div>
            <strong>Private server link</strong>
            <span>DPAPI encrypted</span>
          </div>
          <div>
            <strong>Diagnostic bundles</strong>
            <span>Explicit export only</span>
          </div>
        </div>
      </section>
    </div>
  );
}

function MonitorCard({
  icon,
  title,
  value,
  detail,
  status,
}: {
  icon: React.ReactNode;
  title: string;
  value: string;
  detail: string;
  status: string;
}) {
  return (
    <article className="panel monitor-card">
      <header>
        <span>{icon}</span>
        <small>{status}</small>
      </header>
      <strong>{value}</strong>
      <h3>{title}</h3>
      <p>{detail}</p>
    </article>
  );
}
