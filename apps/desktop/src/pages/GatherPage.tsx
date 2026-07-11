import {
  Check,
  ChevronDown,
  CircleDot,
  MapPin,
  Navigation,
  RotateCcw,
  Route,
  ShieldCheck,
  Timer,
} from "lucide-react";
import { useMemo, useState } from "react";
import type { NectarActions } from "../hooks/useNectarPilot";
import { activeProfile, type DashboardSnapshot } from "../types/contracts";

const fieldMeta: Record<string, { color: string; bonus: string }> = {
  "Pine Tree Forest": { color: "#589d80", bonus: "Blue pollen · +18%" },
  "Mushroom Field": { color: "#df765e", bonus: "Red pollen · quest" },
  "Clover Field": { color: "#70ab62", bonus: "Balanced · +7% luck" },
  "Cactus Field": { color: "#a5a75d", bonus: "Blue/red pollen" },
  "Rose Field": { color: "#d66179", bonus: "Red pollen · +12%" },
};

interface GatherPageProps {
  snapshot: DashboardSnapshot;
  actions: NectarActions;
  pendingAction: string | null;
}

export function GatherPage({
  snapshot,
  actions,
  pendingAction,
}: GatherPageProps) {
  const profile = activeProfile(snapshot);
  const [draft, setDraft] = useState(() => structuredClone(profile.settings));
  const [savedSignature, setSavedSignature] = useState(() =>
    JSON.stringify(profile.settings.gathering),
  );
  const dirty = useMemo(
    () => JSON.stringify(draft.gathering) !== savedSignature,
    [draft.gathering, savedSignature],
  );

  const updateGathering = (patch: Partial<typeof draft.gathering>) => {
    setDraft((current) => ({
      ...current,
      gathering: { ...current.gathering, ...patch },
    }));
  };

  const moveField = (index: number, direction: -1 | 1) => {
    const fields = [...draft.gathering.fields];
    const target = index + direction;
    if (target < 0 || target >= fields.length) return;
    [fields[index], fields[target]] = [
      fields[target] as string,
      fields[index] as string,
    ];
    updateGathering({ fields });
  };

  const apply = async () => {
    await actions.saveSettings(draft);
    setSavedSignature(JSON.stringify(draft.gathering));
  };

  return (
    <div className="page">
      <section className="page-heading">
        <div>
          <span className="eyebrow">Field automation</span>
          <h2>Gather plan</h2>
          <p>
            Build a predictable rotation with validated paths and clear return
            conditions.
          </p>
        </div>
        <div className="draft-actions">
          {dirty && (
            <span className="unsaved-badge">
              <CircleDot size={13} /> Unsaved changes
            </span>
          )}
          <button
            className="button button-secondary"
            disabled={!dirty}
            onClick={() => setDraft(structuredClone(profile.settings))}
          >
            <RotateCcw size={16} /> Reset
          </button>
          <button
            className="button button-primary"
            disabled={!dirty || pendingAction !== null}
            onClick={() => void apply()}
          >
            <Check size={16} /> Apply plan
          </button>
        </div>
      </section>

      <section className="gather-layout">
        <article className="panel rotation-panel">
          <header className="panel-header">
            <div>
              <span className="eyebrow">Rotation</span>
              <h2>{draft.gathering.fields.length} fields queued</h2>
            </div>
            <button className="button button-secondary button-small">
              <MapPin size={15} /> Add field
            </button>
          </header>
          <div className="field-rotation-list">
            {draft.gathering.fields.map((field, index) => {
              const meta = fieldMeta[field] ?? {
                color: "#8b8f98",
                bonus: "Validated field route",
              };
              return (
                <div key={`${field}-${index}`} className="field-card">
                  <span className="field-order">{index + 1}</span>
                  <span
                    className="field-swatch"
                    style={{ background: meta.color }}
                  >
                    <MapPin size={16} />
                  </span>
                  <div>
                    <strong>{field}</strong>
                    <span>{meta.bonus}</span>
                  </div>
                  <span className="field-duration">
                    <Timer size={14} /> {draft.gathering.minutesPerField} min
                  </span>
                  <div className="reorder-controls">
                    <button
                      onClick={() => moveField(index, -1)}
                      disabled={index === 0}
                      aria-label={`Move ${field} earlier`}
                    >
                      <ChevronDown size={16} transform="rotate(180)" />
                    </button>
                    <button
                      onClick={() => moveField(index, 1)}
                      disabled={index === draft.gathering.fields.length - 1}
                      aria-label={`Move ${field} later`}
                    >
                      <ChevronDown size={16} />
                    </button>
                  </div>
                </div>
              );
            })}
          </div>
          <div className="rotation-summary">
            <Route size={18} />
            <div>
              <strong>
                Estimated cycle ·{" "}
                {draft.gathering.fields.length *
                  draft.gathering.minutesPerField +
                  8}{" "}
                minutes
              </strong>
              <span>Includes hive conversion and travel estimates.</span>
            </div>
          </div>
        </article>

        <div className="gather-side">
          <article className="panel">
            <header className="panel-header compact-panel-header">
              <div>
                <span className="eyebrow">Movement</span>
                <h2>Pattern & timing</h2>
              </div>
              <Navigation size={20} />
            </header>
            <div className="form-stack">
              <label className="field-label">
                Gathering pattern
                <select
                  value={draft.gathering.pattern}
                  onChange={(event) =>
                    updateGathering({ pattern: event.target.value })
                  }
                >
                  <option value="e_lol">e_lol · balanced</option>
                  <option value="stationary">Stationary</option>
                  <option value="cornerxsnake">Corner X Snake</option>
                  <option value="supercat">SuperCat</option>
                </select>
              </label>
              <label className="field-label">
                Minutes per field
                <div className="number-input">
                  <input
                    type="number"
                    min="1"
                    max="60"
                    value={draft.gathering.minutesPerField}
                    onChange={(event) =>
                      updateGathering({
                        minutesPerField: Number(event.target.value),
                      })
                    }
                  />
                  <span>min</span>
                </div>
              </label>
              <label className="field-label">
                Return at capacity
                <div className="range-value">
                  <input
                    type="range"
                    min="50"
                    max="100"
                    value={draft.gathering.returnAtCapacity}
                    onChange={(event) =>
                      updateGathering({
                        returnAtCapacity: Number(event.target.value),
                      })
                    }
                  />
                  <output>{draft.gathering.returnAtCapacity}%</output>
                </div>
              </label>
              <label className="switch-row">
                <span>
                  <strong>Drift correction</strong>
                  <small>Re-anchor after movement variance.</small>
                </span>
                <input
                  className="switch-input"
                  type="checkbox"
                  checked={draft.gathering.driftCorrection}
                  onChange={(event) =>
                    updateGathering({ driftCorrection: event.target.checked })
                  }
                />
              </label>
            </div>
          </article>

          <article className="panel confidence-panel">
            <div className="confidence-score">
              <ShieldCheck size={22} />
              <strong>98%</strong>
            </div>
            <div>
              <span className="eyebrow">Route confidence</span>
              <h3>All movement targets validated</h3>
              <p>
                Uncertain detections pause the plan and never become input
                targets.
              </p>
            </div>
          </article>
        </div>
      </section>
    </div>
  );
}
