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
  Trash2,
} from "lucide-react";
import { useMemo, useState } from "react";
import type { NectarActions } from "../hooks/useNectarPilot";
import { activeProfile, type DashboardSnapshot } from "../types/contracts";

type FieldOption = {
  label: string;
  route: string;
  color: string;
  bonus: string;
};

type PatternOption = {
  value: string;
  file: string;
  label: string;
};

// These are stable display-to-manifest mappings. The Rust session planner owns
// the same allowlist and rechecks it before any path can be run.
const fieldOptions: FieldOption[] = [
  {
    label: "Sunflower Field",
    route: "gtf-sunflower.ahk",
    color: "#f6c94b",
    bonus: "Starter field · balanced",
  },
  {
    label: "Dandelion Field",
    route: "gtf-dandelion.ahk",
    color: "#e7bc54",
    bonus: "Starter field · white pollen",
  },
  {
    label: "Mushroom Field",
    route: "gtf-mushroom.ahk",
    color: "#df765e",
    bonus: "Red pollen · quest",
  },
  {
    label: "Blue Flower Field",
    route: "gtf-blueflower.ahk",
    color: "#5f92d4",
    bonus: "Blue pollen",
  },
  {
    label: "Clover Field",
    route: "gtf-clover.ahk",
    color: "#70ab62",
    bonus: "Balanced · +7% luck",
  },
  {
    label: "Spider Field",
    route: "gtf-spider.ahk",
    color: "#7b8c61",
    bonus: "White pollen",
  },
  {
    label: "Bamboo Field",
    route: "gtf-bamboo.ahk",
    color: "#4e9870",
    bonus: "Blue pollen",
  },
  {
    label: "Strawberry Field",
    route: "gtf-strawberry.ahk",
    color: "#df6572",
    bonus: "Red pollen",
  },
  {
    label: "Pineapple Patch",
    route: "gtf-pineapple.ahk",
    color: "#e2aa4a",
    bonus: "Mixed pollen",
  },
  {
    label: "Pumpkin Patch",
    route: "gtf-pumpkin.ahk",
    color: "#d88742",
    bonus: "White pollen",
  },
  {
    label: "Cactus Field",
    route: "gtf-cactus.ahk",
    color: "#a5a75d",
    bonus: "Blue/red pollen",
  },
  {
    label: "Rose Field",
    route: "gtf-rose.ahk",
    color: "#d66179",
    bonus: "Red pollen · +12%",
  },
  {
    label: "Pine Tree Forest",
    route: "gtf-pinetree.ahk",
    color: "#589d80",
    bonus: "Blue pollen · +18%",
  },
  {
    label: "Mountain Top Field",
    route: "gtf-mountaintop.ahk",
    color: "#909bb0",
    bonus: "Mixed pollen",
  },
  {
    label: "Stump Field",
    route: "gtf-stump.ahk",
    color: "#91826d",
    bonus: "Blue pollen",
  },
  {
    label: "Pepper Patch",
    route: "gtf-pepper.ahk",
    color: "#db5c55",
    bonus: "Red pollen",
  },
  {
    label: "Coconut Field",
    route: "gtf-coconut.ahk",
    color: "#7aab8a",
    bonus: "White pollen",
  },
];

const patternOptions: PatternOption[] = [
  { value: "e_lol", file: "e_lol.ahk", label: "e_lol · balanced" },
  { value: "Snake", file: "Snake.ahk", label: "Snake" },
  { value: "CornerXSnake", file: "CornerXSnake.ahk", label: "Corner X Snake" },
  { value: "XSnake", file: "XSnake.ahk", label: "X Snake" },
  { value: "SuperCat", file: "SuperCat.ahk", label: "SuperCat" },
  { value: "Auryn", file: "Auryn.ahk", label: "Auryn" },
  { value: "Diamonds", file: "Diamonds.ahk", label: "Diamonds" },
  { value: "Fork", file: "Fork.ahk", label: "Fork" },
  { value: "Lines", file: "Lines.ahk", label: "Lines" },
  { value: "Slimline", file: "Slimline.ahk", label: "Slimline" },
  { value: "Squares", file: "Squares.ahk", label: "Squares" },
];

function fieldFor(label: string): FieldOption | undefined {
  return fieldOptions.find((field) => field.label === label);
}

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
  const [fieldToAdd, setFieldToAdd] = useState(fieldOptions[0]!.label);
  const [trustDialogOpen, setTrustDialogOpen] = useState(false);
  const [trustConfirmed, setTrustConfirmed] = useState(false);
  const dirty = useMemo(
    () => JSON.stringify(draft.gathering) !== savedSignature,
    [draft.gathering, savedSignature],
  );
  const selectedPattern =
    patternOptions.find(
      (pattern) => pattern.value === draft.gathering.pattern,
    ) ?? patternOptions[0]!;
  const fieldAssets = draft.gathering.fields.map((field) => ({
    field,
    meta: fieldFor(field),
  }));
  const requiredAssets = useMemo(() => {
    const assets = new Map<string, string>();
    for (const { field, meta } of fieldAssets) {
      if (meta) assets.set(`legacy:route:paths/${meta.route}`, field);
    }
    assets.set(
      `legacy:pattern:patterns/${selectedPattern.file}`,
      `${selectedPattern.label} pattern`,
    );
    return assets;
  }, [fieldAssets, selectedPattern]);
  const extensionById = useMemo(
    () =>
      new Map(
        snapshot.extensions.map((extension) => [extension.id, extension]),
      ),
    [snapshot.extensions],
  );
  const missingTrust = [...requiredAssets.entries()].filter(([id]) => {
    const extension = extensionById.get(id);
    return extension?.trust === "review_required";
  });
  const unavailableAssets = [
    ...fieldAssets
      .filter(({ meta }) => !meta)
      .map(({ field }) => `Unknown field: ${field}`),
    ...[...requiredAssets.entries()]
      .filter(([id]) => {
        const extension = extensionById.get(id);
        return !extension || extension.trust === "blocked";
      })
      .map(([, label]) => label),
  ];
  const hasFields = draft.gathering.fields.length > 0;
  const canStart =
    hasFields &&
    draft.gathering.enabled &&
    !dirty &&
    missingTrust.length === 0 &&
    unavailableAssets.length === 0;

  const updateGathering = (patch: Partial<typeof draft.gathering>) => {
    setDraft((current) => ({
      ...current,
      gathering: { ...current.gathering, ...patch },
    }));
  };

  const addField = () => {
    if (draft.gathering.fields.includes(fieldToAdd)) return;
    updateGathering({
      enabled: true,
      fields: [...draft.gathering.fields, fieldToAdd],
    });
  };

  const removeField = (index: number) => {
    const fields = draft.gathering.fields.filter(
      (_, itemIndex) => itemIndex !== index,
    );
    updateGathering({ fields, enabled: fields.length > 0 });
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
    await actions.saveSettings({
      ...draft,
      gathering: { ...draft.gathering, pattern: selectedPattern.value },
    });
    setSavedSignature(
      JSON.stringify({ ...draft.gathering, pattern: selectedPattern.value }),
    );
  };

  const trustSelectedAssets = async () => {
    for (const [id] of missingTrust) {
      const extension = extensionById.get(id);
      if (extension)
        await actions.trustExtension(extension.id, extension.digest);
    }
    await actions.refreshSession();
    setTrustDialogOpen(false);
    setTrustConfirmed(false);
  };

  return (
    <div className="page">
      <section className="page-heading">
        <div>
          <span className="eyebrow">Field automation</span>
          <h2>Gather plan</h2>
          <p>
            Build a saved rotation from exact Natro routes and patterns. Every
            selected compatibility asset is hash-pinned and reviewed before run.
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
          <button
            className="button button-primary"
            disabled={!canStart || pendingAction !== null}
            title={
              canStart
                ? "Starts the saved, bounded compatibility session and transfers focus to the exact Roblox window."
                : "Apply a non-empty plan and resolve its trust/readiness notice before starting."
            }
            onClick={() => void actions.start()}
          >
            Start saved plan
          </button>
        </div>
      </section>

      {!hasFields && (
        <div className="inline-alert inline-alert-warning" role="status">
          <MapPin size={18} />
          <div>
            <strong>Add your first field</strong>
            <span>
              A new profile begins empty so it never moves Roblox until you
              choose a field and apply a plan.
            </span>
          </div>
        </div>
      )}
      {dirty && selectedPattern.value !== draft.gathering.pattern && (
        <div className="inline-alert inline-alert-warning" role="status">
          <ShieldCheck size={18} />
          <div>
            <strong>Choose a legacy bridge pattern</strong>
            <span>
              The old Stationary preview cannot be run by the compatibility
              worker; e_lol is selected until you apply a supported pattern.
            </span>
          </div>
        </div>
      )}
      {!dirty && missingTrust.length > 0 && unavailableAssets.length === 0 && (
        <div className="inline-alert inline-alert-warning" role="status">
          <ShieldCheck size={18} />
          <div>
            <strong>
              {missingTrust.length} selected asset
              {missingTrust.length === 1 ? "" : "s"} needs review
            </strong>
            <span>
              Trust is per profile and only covers the exact pinned
              route/pattern digests in this saved plan.
            </span>
          </div>
          <button
            className="button button-secondary"
            disabled={pendingAction !== null}
            onClick={() => {
              setTrustConfirmed(false);
              setTrustDialogOpen(true);
            }}
          >
            Review & trust selected assets
          </button>
        </div>
      )}
      {unavailableAssets.length > 0 && (
        <div className="inline-alert inline-alert-danger" role="alert">
          <ShieldCheck size={18} />
          <div>
            <strong>Selected assets are unavailable</strong>
            <span>{unavailableAssets.join(", ")}</span>
          </div>
        </div>
      )}

      <section className="panel privacy-panel">
        <header className="panel-header">
          <div>
            <span className="eyebrow">Quest advisor</span>
            <h2>
              {snapshot.questScan?.questName ??
                snapshot.questScan?.giver?.replaceAll("_", " ") ??
                "No quest scanned yet"}
            </h2>
          </div>
          <button
            className="button button-secondary button-small"
            disabled={pendingAction !== null || snapshot.runState !== "Idle"}
            title="Focuses the exact Roblox window, opens the quest log through a validated legacy route, and reports only confident text evidence."
            onClick={() => void actions.scanQuests()}
          >
            Scan quests
          </button>
        </header>
        {snapshot.questScan ? (
          <div>
            {snapshot.questScan.barsComplete.length > 0 && (
              <p>
                Objectives:{" "}
                {snapshot.questScan.barsComplete
                  .map((complete) => (complete ? "✓" : "…"))
                  .join(" ")}
              </p>
            )}
            {snapshot.questScan.recommendedFields.length > 0 && (
              <p>
                Recommended fields:{" "}
                <strong>
                  {snapshot.questScan.recommendedFields
                    .map((field) => field.replaceAll("_", " "))
                    .join(", ")}
                </strong>
              </p>
            )}
            {snapshot.questScan.notes.map((note) => (
              <p key={note}>· {note}</p>
            ))}
            <p>
              Scanned {new Date(snapshot.questScan.scannedAt).toLocaleString()}.
              Advisory only — uncertain readings are reported, never guessed.
            </p>
          </div>
        ) : (
          <p>
            Scan reads the quest log with the validated legacy templates and
            recommends fields that advance incomplete objectives.
          </p>
        )}
      </section>

      <section className="gather-layout">
        <article className="panel rotation-panel">
          <header className="panel-header">
            <div>
              <span className="eyebrow">Rotation</span>
              <h2>{draft.gathering.fields.length} fields queued</h2>
            </div>
            <div className="field-add-control">
              <select
                aria-label="Field to add"
                value={fieldToAdd}
                onChange={(event) => setFieldToAdd(event.target.value)}
              >
                {fieldOptions.map((field) => (
                  <option key={field.route} value={field.label}>
                    {field.label}
                  </option>
                ))}
              </select>
              <button
                className="button button-secondary button-small"
                disabled={draft.gathering.fields.includes(fieldToAdd)}
                onClick={addField}
              >
                <MapPin size={15} /> Add field
              </button>
            </div>
          </header>
          <div className="field-rotation-list">
            {draft.gathering.fields.map((field, index) => {
              const meta = fieldFor(field) ?? {
                color: "#8b8f98",
                bonus:
                  "Unrecognized imported field — replace it before starting",
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
                    <button
                      onClick={() => removeField(index)}
                      aria-label={`Remove ${field}`}
                      title={`Remove ${field}`}
                    >
                      <Trash2 size={15} />
                    </button>
                  </div>
                </div>
              );
            })}
            {!hasFields && (
              <div className="empty-log">
                Select a field above to add the first route to this rotation.
              </div>
            )}
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
              <span>
                Includes the pinned reset-and-convert harness between fields.
              </span>
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
                  value={selectedPattern.value}
                  onChange={(event) =>
                    updateGathering({ pattern: event.target.value })
                  }
                >
                  {patternOptions.map((pattern) => (
                    <option key={pattern.file} value={pattern.value}>
                      {pattern.label}
                    </option>
                  ))}
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
                  <small>
                    Stored for the native path; the legacy bridge keeps its own
                    verified path.
                  </small>
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
              <strong>{unavailableAssets.length === 0 ? "100%" : "0%"}</strong>
            </div>
            <div>
              <span className="eyebrow">Route confidence</span>
              <h3>
                {unavailableAssets.length === 0
                  ? "Exact manifest targets only"
                  : "Action needed before input"}
              </h3>
              <p>
                The session planner maps display names to allowlisted asset IDs;
                uncertain text can never become a movement target.
              </p>
            </div>
          </article>
        </div>
      </section>
      {trustDialogOpen && (
        <div className="dialog-backdrop">
          <div
            className="trust-dialog"
            role="dialog"
            aria-modal="true"
            aria-labelledby="gather-trust-title"
          >
            <span className="dialog-icon warning">
              <ShieldCheck size={23} />
            </span>
            <span className="eyebrow">Saved plan trust review</span>
            <h2 id="gather-trust-title">Trust selected route assets?</h2>
            <p>
              NectarPilot will permit only these exact hashes for this profile.
              A changed file is blocked until you review it again.
            </p>
            <div className="requested-permissions">
              <strong>Assets to trust</strong>
              {missingTrust.map(([id, label]) => (
                <span key={id}>
                  <Check size={14} />
                  {label}
                </span>
              ))}
            </div>
            <label className="check-row trust-confirm">
              <input
                type="checkbox"
                checked={trustConfirmed}
                onChange={(event) => setTrustConfirmed(event.target.checked)}
              />
              <span>
                <strong>
                  I reviewed the selected legacy route and pattern assets
                </strong>
                <small>
                  I understand that a trusted compatibility asset can control
                  keyboard and mouse input only while the exact Roblox window is
                  verified.
                </small>
              </span>
            </label>
            <footer>
              <button
                className="button button-secondary"
                onClick={() => setTrustDialogOpen(false)}
              >
                Cancel
              </button>
              <button
                className="button button-primary"
                disabled={!trustConfirmed || pendingAction !== null}
                onClick={() => void trustSelectedAssets()}
              >
                <ShieldCheck size={16} /> Trust exact assets
              </button>
            </footer>
          </div>
        </div>
      )}
    </div>
  );
}
