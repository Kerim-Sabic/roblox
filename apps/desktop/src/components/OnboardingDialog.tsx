import {
  ArrowLeft,
  ArrowRight,
  Check,
  Gamepad2,
  ShieldCheck,
  Sparkles,
  X,
} from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { NectarActions } from "../hooks/useNectarPilot";
import type { DashboardSnapshot } from "../types/contracts";
import { NectarMark } from "./brand";

interface OnboardingDialogProps {
  snapshot: DashboardSnapshot;
  actions: NectarActions;
  open: boolean;
  required: boolean;
  pending: boolean;
  onClose(): void;
}

const steps = ["Welcome", "Readiness", "Safety"];

export function OnboardingDialog({
  snapshot,
  actions,
  open,
  required,
  pending,
  onClose,
}: OnboardingDialogProps) {
  const [step, setStep] = useState(0);
  const dialogRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const previousFocus = document.activeElement as HTMLElement | null;
    dialogRef.current?.focus();
    return () => previousFocus?.focus();
  }, [open]);

  useEffect(() => {
    if (!open) setStep(0);
  }, [open]);

  if (!open) return null;

  const finish = async () => {
    if (!snapshot.onboardingComplete) await actions.completeOnboarding();
    onClose();
  };

  return (
    <div className="dialog-backdrop" role="presentation">
      <div
        ref={dialogRef}
        className="onboarding-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="onboarding-title"
        tabIndex={-1}
        onKeyDown={(event) => {
          if (event.key === "Escape" && !required) onClose();
        }}
      >
        <aside className="onboarding-rail">
          <NectarMark className="onboarding-mark" />
          <div>
            <strong>NectarPilot</strong>
            <span>Setup guide</span>
          </div>
          <ol>
            {steps.map((label, index) => (
              <li
                key={label}
                className={
                  index === step ? "active" : index < step ? "complete" : ""
                }
              >
                <span>{index < step ? <Check size={14} /> : index + 1}</span>
                {label}
              </li>
            ))}
          </ol>
          <p>
            Screen and input automation only. NectarPilot never reads Roblox
            memory or modifies the client.
          </p>
        </aside>
        <section className="onboarding-content">
          {!required && (
            <button
              className="icon-button onboarding-close"
              onClick={onClose}
              aria-label="Close setup guide"
            >
              <X size={19} />
            </button>
          )}

          {step === 0 && (
            <div className="onboarding-step">
              <div className="dialog-icon">
                <Sparkles size={24} />
              </div>
              <span className="eyebrow">Welcome aboard</span>
              <h2 id="onboarding-title">A calmer way to run your hive</h2>
              <p className="dialog-lead">
                NectarPilot keeps your macro status, safeguards, and recovery
                decisions visible. You stay in control at every step.
              </p>
              <div className="risk-callout">
                <ShieldCheck size={20} />
                <div>
                  <strong>Know the account risk</strong>
                  <p>
                    Roblox may moderate accounts for automation. Test with
                    conservative settings and use NectarPilot at your own
                    discretion.
                  </p>
                </div>
              </div>
              <label className="check-row">
                <input type="checkbox" defaultChecked />
                <span>
                  <strong>Keep the input guard enabled</strong>
                  <small>Pause immediately when Roblox loses focus.</small>
                </span>
              </label>
            </div>
          )}

          {step === 1 && (
            <div className="onboarding-step">
              <div className="dialog-icon">
                <Gamepad2 size={24} />
              </div>
              <span className="eyebrow">System check</span>
              <h2 id="onboarding-title">Your session looks ready</h2>
              <p className="dialog-lead">
                These checks run again before every start. A blocked check
                always prevents input.
              </p>
              <div className="setup-checks">
                {snapshot.readiness.map((check) => (
                  <div key={check.id} className="setup-check-row">
                    <span
                      className={`readiness-icon readiness-${check.status}`}
                    >
                      <Check size={14} />
                    </span>
                    <span>
                      <strong>{check.label}</strong>
                      <small>{check.detail}</small>
                    </span>
                    <span className={`mini-state mini-${check.status}`}>
                      {check.status}
                    </span>
                  </div>
                ))}
              </div>
            </div>
          )}

          {step === 2 && (
            <div className="onboarding-step">
              <div className="dialog-icon">
                <ShieldCheck size={24} />
              </div>
              <span className="eyebrow">Safe by default</span>
              <h2 id="onboarding-title">Nothing valuable is spent yet</h2>
              <p className="dialog-lead">
                Dice, glitter, eggs, stickers, vouchers, and shrine donations
                all start with a zero budget.
              </p>
              <div className="safety-summary-grid">
                <div>
                  <strong>0</strong>
                  <span>valuable items allowed</span>
                </div>
                <div>
                  <strong>5 / 15m</strong>
                  <span>reconnect budget</span>
                </div>
                <div>
                  <strong>Off</strong>
                  <span>Discord control</span>
                </div>
                <div>
                  <strong>14 days</strong>
                  <span>evidence retention</span>
                </div>
              </div>
              <div className="hotkey-callout">
                <kbd>Ctrl</kbd>
                <span>+</span>
                <kbd>Shift</kbd>
                <span>+</span>
                <kbd>F12</kbd>
                <p>
                  <strong>Emergency stop</strong>
                  <small>Releases every held input immediately.</small>
                </p>
              </div>
            </div>
          )}

          <footer className="dialog-footer">
            <button
              className="button button-secondary"
              onClick={() => setStep((value) => Math.max(0, value - 1))}
              disabled={step === 0}
            >
              <ArrowLeft size={16} /> Back
            </button>
            <span>
              Step {step + 1} of {steps.length}
            </span>
            {step < steps.length - 1 ? (
              <button
                className="button button-primary"
                onClick={() => setStep((value) => value + 1)}
              >
                Continue <ArrowRight size={16} />
              </button>
            ) : (
              <button
                className="button button-primary"
                onClick={() => void finish()}
                disabled={pending}
              >
                <Check size={16} /> {pending ? "Saving…" : "Finish setup"}
              </button>
            )}
          </footer>
        </section>
      </div>
    </div>
  );
}
