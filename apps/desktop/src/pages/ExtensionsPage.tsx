import {
  AlertTriangle,
  Box,
  Check,
  ChevronRight,
  FileKey2,
  Filter,
  LockKeyhole,
  Play,
  Puzzle,
  Search,
  ShieldAlert,
  ShieldCheck,
  X,
} from "lucide-react";
import { useMemo, useState } from "react";
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

type CatalogFilter =
  "all" | "routes" | "patterns" | "review_required" | "trusted" | "blocked";

export function ExtensionsPage({
  snapshot,
  actions,
  pendingAction,
}: ExtensionsPageProps) {
  const [reviewing, setReviewing] = useState<ExtensionManifest | null>(null);
  const [confirmed, setConfirmed] = useState(false);
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<CatalogFilter>("all");
  const [runFeedback, setRunFeedback] = useState<{
    tone: "success" | "warning";
    message: string;
  } | null>(null);
  const extensions = useMemo(() => {
    const normalized = query.trim().toLocaleLowerCase();
    return snapshot.extensions.filter((extension) => {
      const matchesQuery =
        normalized.length === 0 ||
        [
          extension.name,
          extension.author,
          extension.description,
          extension.id,
          extension.digest,
        ].some((value) => value.toLocaleLowerCase().includes(normalized));
      const matchesFilter =
        filter === "all" ||
        (filter === "routes" && extension.id.startsWith("legacy:route:")) ||
        (filter === "patterns" && extension.id.startsWith("legacy:pattern:")) ||
        extension.trust === filter;
      return matchesQuery && matchesFilter;
    });
  }, [filter, query, snapshot.extensions]);
  const trust = async () => {
    if (!reviewing || !confirmed) return;
    if (await actions.trustExtension(reviewing.id, reviewing.digest)) {
      setReviewing(null);
      setConfirmed(false);
    }
  };
  const runContainedScript = async (extension: ExtensionManifest) => {
    const accepted = await actions.runLegacyExtension(
      extension.id,
      extension.digest,
    );
    setRunFeedback(
      accepted
        ? {
            tone: "success",
            message: `${extension.name}: the contained-run request was accepted. Watch the run state and Diagnostics for execution results.`,
          }
        : {
            tone: "warning",
            message: `${extension.name}: the contained-run request was rejected. Review the error above; no script was started.`,
          },
    );
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
        <button
          className="button button-primary"
          disabled
          title="Importing third-party extensions is not available in this build."
        >
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
        <button
          className="button button-secondary"
          disabled
          title="Compatibility limits are fixed by the contained runner in this build."
        >
          Compatibility settings
        </button>
      </div>
      <div className="inline-note" role="note">
        <AlertTriangle size={17} />
        <span>
          Importing third-party extensions and changing compatibility limits are
          not available yet. Only the bundled, hash-pinned catalog can run.
        </span>
      </div>
      {runFeedback && (
        <div
          className={`inline-alert inline-alert-${runFeedback.tone === "success" ? "success" : "warning"}`}
          role="status"
        >
          {runFeedback.tone === "success" ? (
            <Check size={18} />
          ) : (
            <AlertTriangle size={18} />
          )}
          <div>
            <strong>
              {runFeedback.tone === "success"
                ? "Run request accepted"
                : "Run request rejected"}
            </strong>
            <span>{runFeedback.message}</span>
          </div>
        </div>
      )}
      <section
        className="extension-toolbar"
        aria-label="Extension catalog filters"
      >
        <label className="search-input">
          <Search size={15} />
          <input
            aria-label="Search compatibility catalog"
            type="search"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder="Search 103 pinned routes and patterns"
          />
        </label>
        <label className="filter-select">
          <Filter size={15} />
          <select
            aria-label="Filter compatibility catalog"
            value={filter}
            onChange={(event) => setFilter(event.target.value as CatalogFilter)}
          >
            <option value="all">All entries</option>
            <option value="routes">Routes</option>
            <option value="patterns">Patterns</option>
            <option value="review_required">Review required</option>
            <option value="trusted">Trusted</option>
            <option value="blocked">Blocked</option>
          </select>
        </label>
        <span className="extension-results">
          {extensions.length} of {snapshot.extensions.length}
        </span>
      </section>
      <section className="extension-list">
        {extensions.map((extension) => (
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
                  <button
                    className="button button-secondary button-small"
                    disabled
                    title="Changed or unavailable assets cannot be inspected or run from this build."
                  >
                    Inspect change
                  </button>
                ) : extension.executionMode === "native_preview" ? (
                  <span
                    className="safe-default-badge"
                    aria-label="Native conversion preview; execution unavailable"
                  >
                    <Box size={14} /> Native conversion preview
                  </span>
                ) : extension.executionMode === "legacy_bridge" ? (
                  <>
                    <button
                      className="button button-secondary button-small"
                      disabled={pendingAction !== null}
                      title="Shows the exact generated harness this asset would run"
                      onClick={() => void actions.inspectLegacy(extension.id)}
                    >
                      Preview harness
                    </button>
                    <button
                      className="button button-secondary button-small"
                      disabled={pendingAction !== null}
                      onClick={() => void runContainedScript(extension)}
                    >
                      <Play size={14} /> Run contained script
                    </button>
                  </>
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
        {extensions.length === 0 && (
          <div className="panel extension-empty">
            <Search size={20} />
            <strong>No catalog entries match</strong>
            <span>Clear the search or choose a different filter.</span>
          </div>
        )}
      </section>
      {snapshot.legacyInspection && (
        <section className="panel legacy-inspection">
          <header className="panel-header">
            <div>
              <span className="eyebrow">Generated harness preview</span>
              <h2>{snapshot.legacyInspection.scriptId}</h2>
            </div>
          </header>
          <dl className="legacy-inspection-meta">
            <div>
              <dt>Pinned asset size</dt>
              <dd>{snapshot.legacyInspection.bytes} bytes</dd>
            </div>
            <div>
              <dt>Pinned asset SHA-256</dt>
              <dd>
                <code>{snapshot.legacyInspection.sha256}</code>
              </dd>
            </div>
          </dl>
          <p>
            The metadata above identifies the exact pinned source asset. The
            preview below is the complete generated harness that would execute:
            that asset wrapped in Natro&apos;s walk environment.
          </p>
          <pre className="legacy-harness-preview">
            {snapshot.legacyInspection.harnessPreview}
          </pre>
        </section>
      )}
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
