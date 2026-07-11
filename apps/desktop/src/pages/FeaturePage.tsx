import {
  ArrowRight,
  CheckCircle2,
  CircleOff,
  Clock3,
  Info,
  ShieldCheck,
} from "lucide-react";
import type { ReactNode } from "react";
import { useEffect, useState } from "react";
import type { NectarActions } from "../hooks/useNectarPilot";
import {
  activeProfile,
  type DashboardSnapshot,
  type FeatureCard,
} from "../types/contracts";

interface FeaturePageProps {
  category: FeatureCard["category"];
  title: string;
  eyebrow: string;
  description: string;
  snapshot: DashboardSnapshot;
  actions: NectarActions;
  pendingAction: string | null;
  aside?: ReactNode;
}

export function FeaturePage({
  category,
  title,
  eyebrow,
  description,
  snapshot,
  actions,
  pendingAction,
  aside,
}: FeaturePageProps) {
  const profile = activeProfile(snapshot);
  const features = snapshot.features.filter(
    (feature) => feature.category === category,
  );
  const [enabled, setEnabled] = useState<Record<string, boolean>>(() => ({
    ...profile.settings.features,
  }));

  useEffect(
    () => setEnabled({ ...profile.settings.features }),
    [profile.id, profile.settings.features],
  );

  const toggle = async (feature: FeatureCard) => {
    const next = {
      ...enabled,
      [feature.id]: !(enabled[feature.id] ?? feature.enabled),
    };
    setEnabled(next);
    await actions.saveSettings({ ...profile.settings, features: next });
  };

  return (
    <div className="page">
      <section className="page-heading">
        <div>
          <span className="eyebrow">{eyebrow}</span>
          <h2>{title}</h2>
          <p>{description}</p>
        </div>
        <span className="safe-default-badge">
          <ShieldCheck size={16} /> Budget safeguards apply
        </span>
      </section>

      {aside}

      <section className="feature-grid">
        {features.map((feature) => {
          const isEnabled = enabled[feature.id] ?? feature.enabled;
          return (
            <article
              key={feature.id}
              className={`feature-card ${isEnabled ? "feature-enabled" : ""}`}
            >
              <header>
                <span
                  className={`feature-state-icon ${isEnabled ? "enabled" : ""}`}
                >
                  {isEnabled ? (
                    <CheckCircle2 size={18} />
                  ) : (
                    <CircleOff size={18} />
                  )}
                </span>
                <label
                  className="switch-only"
                  aria-label={`${isEnabled ? "Disable" : "Enable"} ${feature.title}`}
                >
                  <input
                    className="switch-input"
                    type="checkbox"
                    checked={isEnabled}
                    disabled={pendingAction !== null}
                    onChange={() => void toggle(feature)}
                  />
                </label>
              </header>
              <h3>{feature.title}</h3>
              <p>{feature.description}</p>
              <footer>
                <span className={isEnabled ? "feature-status-on" : ""}>
                  <Clock3 size={14} /> {isEnabled ? feature.status : "Disabled"}
                </span>
                <button aria-label={`Configure ${feature.title}`}>
                  <ArrowRight size={16} />
                </button>
              </footer>
            </article>
          );
        })}
      </section>

      <div className="inline-note">
        <Info size={17} />
        <span>
          Changes apply to <strong>{profile.name}</strong>. Active tasks finish
          their current safe step before updated scheduling takes effect.
        </span>
      </div>
    </div>
  );
}
