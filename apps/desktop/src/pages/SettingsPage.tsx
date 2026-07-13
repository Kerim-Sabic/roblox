import {
  AlertTriangle,
  Check,
  CloudCog,
  Eye,
  Keyboard,
  RefreshCw,
  RotateCcw,
  Search,
  ShieldCheck,
  SlidersHorizontal,
} from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import type { NectarActions } from "../hooks/useNectarPilot";
import {
  activeProfile,
  type AutomationSettings,
  type DashboardSnapshot,
  type ThemePreference,
  type ValuableItem,
} from "../types/contracts";

interface SettingsPageProps {
  snapshot: DashboardSnapshot;
  actions: NectarActions;
  pendingAction: string | null;
  theme: ThemePreference;
  onThemeChange(theme: ThemePreference): void;
}

const budgetLabels: Record<ValuableItem, { label: string; unit: string }> = {
  fieldDice: { label: "Field dice", unit: "/ run" },
  glitter: { label: "Glitter", unit: "/ day" },
  eggs: { label: "Eggs", unit: "/ run" },
  stickers: { label: "Stickers", unit: "/ run" },
  vouchers: { label: "Vouchers", unit: "/ run" },
  shrineDonations: { label: "Shrine donations", unit: "items / day" },
};

type SettingsSection = "general" | "safety" | "recovery" | "remote" | "hotkeys";

const sections: Array<{
  id: SettingsSection;
  label: string;
  icon: typeof ShieldCheck;
  keywords: string;
}> = [
  {
    id: "general",
    label: "General",
    icon: SlidersHorizontal,
    keywords: "general appearance theme profile gathering",
  },
  {
    id: "safety",
    label: "Safety & budgets",
    icon: ShieldCheck,
    keywords:
      "safety valuable items dice glitter eggs stickers vouchers shrine focus foreground",
  },
  {
    id: "recovery",
    label: "Recovery",
    icon: RefreshCw,
    keywords: "reconnect retries deadline restart freeze",
  },
  {
    id: "remote",
    label: "Remote & privacy",
    icon: CloudCog,
    keywords: "discord monitoring screenshots permissions evidence retention",
  },
  {
    id: "hotkeys",
    label: "Hotkeys",
    icon: Keyboard,
    keywords: "keyboard start pause stop emergency",
  },
];

export function SettingsPage({
  snapshot,
  actions,
  pendingAction,
  theme,
  onThemeChange,
}: SettingsPageProps) {
  const profile = activeProfile(snapshot);
  const [draft, setDraft] = useState<AutomationSettings>(() =>
    structuredClone(profile.settings),
  );
  const [baseline, setBaseline] = useState(() =>
    JSON.stringify(profile.settings),
  );
  const [activeSection, setActiveSection] =
    useState<SettingsSection>("general");
  const [query, setQuery] = useState("");

  useEffect(() => {
    setDraft(structuredClone(profile.settings));
    setBaseline(JSON.stringify(profile.settings));
  }, [profile.id, profile.settings]);

  const dirty = useMemo(
    () => JSON.stringify(draft) !== baseline,
    [draft, baseline],
  );
  const visibleSections = query.trim()
    ? sections.filter(
        (section) =>
          section.keywords.includes(query.trim().toLowerCase()) ||
          section.label.toLowerCase().includes(query.trim().toLowerCase()),
      )
    : sections;

  useEffect(() => {
    if (query && visibleSections[0]) setActiveSection(visibleSections[0].id);
  }, [query]); // eslint-disable-line react-hooks/exhaustive-deps

  const update = <K extends keyof AutomationSettings>(
    key: K,
    value: AutomationSettings[K],
  ) => {
    setDraft((current) => ({ ...current, [key]: value }));
  };

  const apply = async () => {
    await actions.saveSettings(draft);
    setBaseline(JSON.stringify(draft));
  };

  const discard = () => setDraft(structuredClone(profile.settings));

  return (
    <div className="page settings-page">
      <section className="page-heading">
        <div>
          <span className="eyebrow">{profile.name}</span>
          <h2>Settings</h2>
          <p>Changes stay in a draft until you apply them to this profile.</p>
        </div>
        <div className="draft-actions">
          {dirty && <span className="unsaved-badge">Unsaved changes</span>}
          <button
            className="button button-secondary"
            disabled={!dirty}
            onClick={discard}
          >
            <RotateCcw size={16} /> Cancel
          </button>
          <button
            className="button button-primary"
            disabled={!dirty || pendingAction !== null}
            onClick={() => void apply()}
          >
            <Check size={16} />{" "}
            {pendingAction === "save-settings" ? "Applying…" : "Apply"}
          </button>
        </div>
      </section>

      <section className="settings-layout">
        <aside className="settings-index">
          <label className="search-input">
            <Search size={16} />
            <input
              type="search"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder="Search settings"
              aria-label="Search settings"
            />
          </label>
          <nav aria-label="Settings sections">
            {visibleSections.map((section) => {
              const Icon = section.icon;
              return (
                <button
                  key={section.id}
                  className={activeSection === section.id ? "active" : ""}
                  onClick={() => setActiveSection(section.id)}
                >
                  <Icon size={17} />
                  {section.label}
                </button>
              );
            })}
            {visibleSections.length === 0 && (
              <p className="no-results">No settings match “{query}”.</p>
            )}
          </nav>
          <div className="settings-profile-card">
            <span
              className="profile-avatar"
              style={{ background: profile.accent }}
            >
              {profile.name.slice(0, 1)}
            </span>
            <span>
              <strong>{profile.name}</strong>
              <small>{profile.description}</small>
            </span>
          </div>
        </aside>

        <div className="settings-detail">
          {activeSection === "general" && (
            <SettingsSectionCard
              icon={<SlidersHorizontal size={20} />}
              title="General"
              description="App appearance and core gathering behavior."
            >
              <SettingRow
                title="Color theme"
                description="Follow Windows or choose a fixed appearance."
              >
                <div
                  className="segmented-control"
                  role="group"
                  aria-label="Color theme"
                >
                  {(["system", "light", "dark"] as const).map((option) => (
                    <button
                      key={option}
                      className={theme === option ? "active" : ""}
                      onClick={() => onThemeChange(option)}
                    >
                      {option}
                    </button>
                  ))}
                </div>
              </SettingRow>
              <SettingRow
                title="Gathering automation"
                description="Include field gathering in this profile’s scheduler."
              >
                <Switch
                  checked={draft.gathering.enabled}
                  onChange={(checked) =>
                    update("gathering", {
                      ...draft.gathering,
                      enabled: checked,
                    })
                  }
                  label="Gathering automation"
                />
              </SettingRow>
              <SettingRow
                title="Return at capacity"
                description="Begin a safe hive return at this estimated bag level."
              >
                <div className="number-input compact-number">
                  <input
                    type="number"
                    min="50"
                    max="100"
                    value={draft.gathering.returnAtCapacity}
                    onChange={(event) =>
                      update("gathering", {
                        ...draft.gathering,
                        returnAtCapacity: Number(event.target.value),
                      })
                    }
                  />
                  <span>%</span>
                </div>
              </SettingRow>
            </SettingsSectionCard>
          )}

          {activeSection === "safety" && (
            <>
              <div className="inline-alert inline-alert-warning">
                <AlertTriangle size={18} />
                <div>
                  <strong>Valuable items require an explicit budget</strong>
                  <span>
                    Zero means NectarPilot will never spend that item. Limits
                    reset on the interval shown.
                  </span>
                </div>
              </div>
              <SettingsSectionCard
                icon={<ShieldCheck size={20} />}
                title="Input safety"
                description="Hard rules enforced by the input broker."
              >
                <SettingRow
                  title="Pause on focus loss"
                  description="Release input as soon as Roblox is no longer foreground."
                >
                  <Switch
                    checked={draft.safety.pauseOnFocusLoss}
                    onChange={(checked) =>
                      update("safety", {
                        ...draft.safety,
                        pauseOnFocusLoss: checked,
                      })
                    }
                    label="Pause on focus loss"
                  />
                </SettingRow>
                <SettingRow
                  title="Require the adopted window"
                  description="Refuse every input when the exact Roblox PID/HWND is not verified."
                >
                  <Switch
                    checked={draft.safety.requireForeground}
                    onChange={(checked) =>
                      update("safety", {
                        ...draft.safety,
                        requireForeground: checked,
                      })
                    }
                    label="Require adopted window"
                  />
                </SettingRow>
                <SettingRow
                  title="Confirm high-risk actions"
                  description="Pause before purchases, donations, trades, or irreversible actions."
                >
                  <Switch
                    checked={draft.safety.confirmHighRiskActions}
                    onChange={(checked) =>
                      update("safety", {
                        ...draft.safety,
                        confirmHighRiskActions: checked,
                      })
                    }
                    label="Confirm high-risk actions"
                  />
                </SettingRow>
              </SettingsSectionCard>
              <SettingsSectionCard
                icon={<Eye size={20} />}
                title="Valuable-item budgets"
                description="Maximum permitted use for this profile. All defaults are zero."
              >
                <div className="budget-grid">
                  {(Object.keys(budgetLabels) as ValuableItem[]).map((item) => (
                    <label key={item} className="budget-field">
                      <span>
                        <strong>{budgetLabels[item].label}</strong>
                        <small>{budgetLabels[item].unit}</small>
                      </span>
                      <input
                        aria-label={`${budgetLabels[item].label} budget`}
                        type="number"
                        min="0"
                        max="999"
                        value={draft.safety.budgets[item]}
                        onChange={(event) =>
                          update("safety", {
                            ...draft.safety,
                            budgets: {
                              ...draft.safety.budgets,
                              [item]: Math.max(0, Number(event.target.value)),
                            },
                          })
                        }
                      />
                    </label>
                  ))}
                </div>
              </SettingsSectionCard>
            </>
          )}

          {activeSection === "recovery" && (
            <SettingsSectionCard
              icon={<RefreshCw size={20} />}
              title="Bounded recovery"
              description="Recovery stops and asks for attention when its budget is exhausted."
            >
              <SettingRow
                title="Reconnect automatically"
                description="Try to rejoin after a confirmed disconnect."
              >
                <Switch
                  checked={draft.recovery.reconnectEnabled}
                  onChange={(checked) =>
                    update("recovery", {
                      ...draft.recovery,
                      reconnectEnabled: checked,
                    })
                  }
                  label="Automatic reconnect"
                />
              </SettingRow>
              <SettingRow
                title="Maximum attempts"
                description="Five attempts is the recommended safe default."
              >
                <input
                  className="standalone-number"
                  aria-label="Maximum reconnect attempts"
                  type="number"
                  min="1"
                  max="10"
                  value={draft.recovery.maxAttempts}
                  onChange={(event) =>
                    update("recovery", {
                      ...draft.recovery,
                      maxAttempts: Number(event.target.value),
                    })
                  }
                />
              </SettingRow>
              <SettingRow
                title="Overall deadline"
                description="Recovery enters Needs Attention after this deadline."
              >
                <div className="number-input compact-number">
                  <input
                    aria-label="Reconnect deadline"
                    type="number"
                    min="5"
                    max="60"
                    value={draft.recovery.deadlineMinutes}
                    onChange={(event) =>
                      update("recovery", {
                        ...draft.recovery,
                        deadlineMinutes: Number(event.target.value),
                      })
                    }
                  />
                  <span>min</span>
                </div>
              </SettingRow>
              <SettingRow
                title="Restart after a confirmed freeze"
                description="Requires multiple independent signals; uncertain OCR is never sufficient."
              >
                <Switch
                  checked={draft.recovery.restartOnConfirmedFreeze}
                  onChange={(checked) =>
                    update("recovery", {
                      ...draft.recovery,
                      restartOnConfirmedFreeze: checked,
                    })
                  }
                  label="Restart after confirmed freeze"
                />
              </SettingRow>
            </SettingsSectionCard>
          )}

          {activeSection === "remote" && (
            <SettingsSectionCard
              icon={<CloudCog size={20} />}
              title="Remote control & privacy"
              description="Discord access is off until enabled and granted per capability."
            >
              <SettingRow
                title="Discord integration"
                description="Allow the optional Discord component to connect."
              >
                <Switch
                  checked={draft.monitoring.discordEnabled}
                  onChange={(checked) =>
                    update("monitoring", {
                      ...draft.monitoring,
                      discordEnabled: checked,
                    })
                  }
                  label="Discord integration"
                />
              </SettingRow>
              <SettingRow
                title="Failure evidence retention"
                description="Cropped evidence is local and removed after this many days."
              >
                <div className="number-input compact-number">
                  <input
                    aria-label="Evidence retention days"
                    type="number"
                    min="1"
                    max="90"
                    value={draft.monitoring.evidenceRetentionDays}
                    onChange={(event) =>
                      update("monitoring", {
                        ...draft.monitoring,
                        evidenceRetentionDays: Number(event.target.value),
                      })
                    }
                  />
                  <span>days</span>
                </div>
              </SettingRow>
              <SettingRow
                title="Evidence storage limit"
                description="Oldest evidence is removed first when the cap is reached."
              >
                <div className="number-input compact-number">
                  <input
                    aria-label="Evidence storage limit"
                    type="number"
                    min="50"
                    max="2000"
                    step="50"
                    value={draft.monitoring.evidenceLimitMb}
                    onChange={(event) =>
                      update("monitoring", {
                        ...draft.monitoring,
                        evidenceLimitMb: Number(event.target.value),
                      })
                    }
                  />
                  <span>MB</span>
                </div>
              </SettingRow>
              <div className="permission-list">
                {(
                  Object.entries(draft.monitoring.permissions) as Array<
                    [keyof typeof draft.monitoring.permissions, boolean]
                  >
                ).map(([permission, allowed]) => (
                  <label
                    key={permission}
                    className={
                      !draft.monitoring.discordEnabled ? "disabled" : ""
                    }
                  >
                    <span>{permission.replace(/([A-Z])/g, " $1")}</span>
                    <input
                      type="checkbox"
                      checked={allowed}
                      disabled={!draft.monitoring.discordEnabled}
                      onChange={(event) =>
                        update("monitoring", {
                          ...draft.monitoring,
                          permissions: {
                            ...draft.monitoring.permissions,
                            [permission]: event.target.checked,
                          },
                        })
                      }
                    />
                  </label>
                ))}
              </div>
            </SettingsSectionCard>
          )}

          {activeSection === "hotkeys" && (
            <SettingsSectionCard
              icon={<Keyboard size={20} />}
              title="Global hotkeys"
              description="Emergency stop is always registered while NectarPilot is open."
            >
              {(
                Object.entries(draft.hotkeys) as Array<
                  [keyof typeof draft.hotkeys, string]
                >
              ).map(([command, value]) => (
                <SettingRow
                  key={command}
                  title={command.replace(/([A-Z])/g, " $1")}
                  description={
                    command === "emergencyStop"
                      ? "Hard stop; releases every held key and mouse button."
                      : `Global ${command} control.`
                  }
                >
                  <button
                    className="hotkey-recorder"
                    aria-label={`Change ${command} hotkey`}
                  >
                    <kbd>{value}</kbd>
                    <span>Change</span>
                  </button>
                </SettingRow>
              ))}
            </SettingsSectionCard>
          )}
        </div>
      </section>
    </div>
  );
}

function SettingsSectionCard({
  icon,
  title,
  description,
  children,
}: {
  icon: React.ReactNode;
  title: string;
  description: string;
  children: React.ReactNode;
}) {
  return (
    <section className="panel settings-card">
      <header>
        <span className="settings-card-icon">{icon}</span>
        <div>
          <h3>{title}</h3>
          <p>{description}</p>
        </div>
      </header>
      <div className="settings-card-body">{children}</div>
    </section>
  );
}

function SettingRow({
  title,
  description,
  children,
}: {
  title: string;
  description: string;
  children: React.ReactNode;
}) {
  return (
    <div className="setting-row">
      <div>
        <strong>{title}</strong>
        <span>{description}</span>
      </div>
      <div>{children}</div>
    </div>
  );
}

function Switch({
  checked,
  onChange,
  label,
}: {
  checked: boolean;
  onChange(checked: boolean): void;
  label: string;
}) {
  return (
    <label className="switch-only" aria-label={label}>
      <input
        className="switch-input"
        type="checkbox"
        checked={checked}
        onChange={(event) => onChange(event.target.checked)}
      />
    </label>
  );
}
