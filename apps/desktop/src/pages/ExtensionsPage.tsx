import {
  AlertTriangle,
  Box,
  Check,
  ChevronRight,
  FileKey2,
  LockKeyhole,
  Puzzle,
  ShieldAlert,
  ShieldCheck,
  X,
} from "lucide-react";
import { useState } from "react";
import type { NectarActions } from "../hooks/useNectarPilot";
import type { DashboardSnapshot, ExtensionManifest } from "../types/contracts";

interface ExtensionsPageProps {
  snapshot: DashboardSnapshot;
  actions: NectarActions;
  pendingAction: string | null;
}

const trustLabels = {
  built_in: "Built in",
  trusted: "Trusted",
  review_required: "Review required",
  blocked: "Blocked",
} as const;

export function ExtensionsPage({
  snapshot,
  actions,
  pendingAction,
}: ExtensionsPageProps) {
  const [reviewing, setReviewing] = useState<ExtensionManifest | null>(null);
  const [confirmed, setConfirmed] = useState(false);
  const trust = async () => {
    if (!reviewing || !confirmed) return;
    await actions.trustExtension(reviewing.id, reviewing.digest);
    setReviewing(null);
    setConfirmed(false);
  };
  return (
    <div className="page">
      <section className="page-heading">
        <div>
          <span className="eyebrow">Paths, patterns & compatibility</span>
          <h2>Extensions</h2>
          <p>
            Every imported extension is pinned to a digest and reviewed before
            it can run.
          </p>
        </div>
        <button className="button button-primary">
          <Puzzle size={16} /> Import extension
        </button>
      </section>
      <div className="inline-alert inline-alert-warning">
        <AlertTriangle size={18} />
        <div>
          <strong>Legacy AHK runs in a contained compatibility worker</strong>
          <span>
            Exact PID tracking, cancellation, time limits, and trust hashes
            apply. Only enable scripts you understand.
          </span>
        </div>
        <button className="button button-secondary">
          Compatibility settings
        </button>
      </div>
      <section className="extension-list">
        {snapshot.extensions.map((extension) => (
          <article key={extension.id} className="panel extension-card">
            <span className={`extension-icon extension-${extension.trust}`}>
              {extension.trust === "blocked" ? (
                <ShieldAlert />
              ) : extension.trust === "review_required" ? (
                <FileKey2 />
              ) : extension.trust === "built_in" ? (
                <Box />
              ) : (
                <ShieldCheck />
              )}
            </span>
            <div className="extension-main">
              <header>
                <div>
                  <h3>{extension.name}</h3>
                  <span>
                    v{extension.version} · {extension.author}
                  </span>
                </div>
                <span className={`trust-badge trust-${extension.trust}`}>
                  {extension.trust === "trusted" ||
                  extension.trust === "built_in" ? (
                    <Check size={13} />
                  ) : (
                    <LockKeyhole size={13} />
                  )}
                  {trustLabels[extension.trust]}
                </span>
              </header>
              <p>{extension.description}</p>
              <div className="permission-chips">
                {extension.permissions.map((permission) => (
                  <span key={permission}>{permission}</span>
                ))}
              </div>
              <footer>
                <code>{extension.digest}</code>
                {extension.trust === "review_required" ? (
                  <button
                    className="button button-secondary button-small"
                    onClick={() => setReviewing(extension)}
                  >
                    Review & trust <ChevronRight size={15} />
                  </button>
                ) : extension.trust === "blocked" ? (
                  <button className="button button-secondary button-small">
                    Inspect change
                  </button>
                ) : (
                  <label
                    className="switch-only"
                    aria-label={`Enable ${extension.name}`}
                  >
                    <input
                      className="switch-input"
                      type="checkbox"
                      checked={extension.enabled}
                      readOnly
                    />
                  </label>
                )}
              </footer>
            </div>
          </article>
        ))}
      </section>
      {reviewing && (
        <div className="dialog-backdrop">
          <div
            className="trust-dialog"
            role="dialog"
            aria-modal="true"
            aria-labelledby="trust-title"
          >
            <button
              className="icon-button dialog-x"
              onClick={() => setReviewing(null)}
              aria-label="Close review"
            >
              <X size={18} />
            </button>
            <span className="dialog-icon warning">
              <FileKey2 size={23} />
            </span>
            <span className="eyebrow">Extension trust review</span>
            <h2 id="trust-title">Trust “{reviewing.name}”?</h2>
            <p>
              Trust is granted only to this exact file digest. Any change blocks
              the extension until you review it again.
            </p>
            <div className="trust-detail">
              <span>
                Publisher<strong>{reviewing.author}</strong>
              </span>
              <span>
                Version<strong>{reviewing.version}</strong>
              </span>
              <span>
                Digest<code>{reviewing.digest}</code>
              </span>
            </div>
            <div className="requested-permissions">
              <strong>Requested capabilities</strong>
              {reviewing.permissions.map((permission) => (
                <span key={permission}>
                  <Check size={14} />
                  {permission}
                </span>
              ))}
            </div>
            <label className="check-row trust-confirm">
              <input
                type="checkbox"
                checked={confirmed}
                onChange={(event) => setConfirmed(event.target.checked)}
              />
              <span>
                <strong>I reviewed these capabilities</strong>
                <small>
                  I understand that legacy automation can control keyboard and
                  mouse input.
                </small>
              </span>
            </label>
            <footer>
              <button
                className="button button-secondary"
                onClick={() => setReviewing(null)}
              >
                Cancel
              </button>
              <button
                className="button button-primary"
                disabled={!confirmed || pendingAction !== null}
                onClick={() => void trust()}
              >
                <ShieldCheck size={16} /> Trust exact digest
              </button>
            </footer>
          </div>
        </div>
      )}
    </div>
  );
}
