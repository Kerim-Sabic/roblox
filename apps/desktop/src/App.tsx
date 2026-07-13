import {
  AlertTriangle,
  BrainCircuit,
  Flower2,
  Menu,
  PackageOpen,
  ShieldCheck,
  Sparkles,
  X,
} from "lucide-react";
import { useEffect, useState } from "react";
import { CompactMonitor } from "./components/CompactMonitor";
import { OnboardingDialog } from "./components/OnboardingDialog";
import { Sidebar, navItems, type NavId } from "./components/Sidebar";
import { TopBar } from "./components/TopBar";
import { useNectarPilot } from "./hooks/useNectarPilot";
import { useTheme } from "./hooks/useTheme";
import type { NectarService } from "./services/nectar-service";
import { AboutPage } from "./pages/AboutPage";
import { DiagnosticsPage } from "./pages/DiagnosticsPage";
import { ExtensionsPage } from "./pages/ExtensionsPage";
import { FeaturePage } from "./pages/FeaturePage";
import { GatherPage } from "./pages/GatherPage";
import { MonitoringPage } from "./pages/MonitoringPage";
import { OverviewPage } from "./pages/OverviewPage";
import { SettingsPage } from "./pages/SettingsPage";

export default function App({ service }: { service?: NectarService }) {
  const controller = useNectarPilot(service);
  const theme = useTheme();
  const [activePage, setActivePage] = useState<NavId>("overview");
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const [mobileNavOpen, setMobileNavOpen] = useState(false);
  const [compact, setCompact] = useState(false);
  const [setupOpen, setSetupOpen] = useState(false);

  useEffect(() => {
    if (controller.snapshot && !controller.snapshot.onboardingComplete)
      setSetupOpen(true);
  }, [controller.snapshot]);

  useEffect(() => {
    const hotkeys = (event: KeyboardEvent) => {
      if (!controller.snapshot) return;
      if (event.ctrlKey && event.shiftKey && event.key === "F12") {
        event.preventDefault();
        void controller.actions.emergencyStop();
        return;
      }
      const target = event.target as HTMLElement | null;
      if (target?.matches('input, select, textarea, [contenteditable="true"]'))
        return;
      if (event.key === "F1") {
        event.preventDefault();
        void controller.actions.start();
      }
      if (event.key === "F2") {
        event.preventDefault();
        void controller.actions.pause();
      }
      if (event.key === "F3") {
        event.preventDefault();
        void controller.actions.stop();
      }
    };
    window.addEventListener("keydown", hotkeys);
    return () => window.removeEventListener("keydown", hotkeys);
  }, [controller.actions, controller.snapshot]);

  if (controller.loading || !controller.snapshot) {
    return (
      <main className="loading-screen">
        <div className="loading-mark">
          <span />
          <span />
          <span />
        </div>
        <strong>Waking NectarPilot…</strong>
        <span>Connecting to the local automation service</span>
      </main>
    );
  }

  const snapshot = controller.snapshot;
  if (compact) {
    return (
      <CompactMonitor
        snapshot={snapshot}
        actions={controller.actions}
        pendingAction={controller.pendingAction}
        onExpand={() => {
          setCompact(false);
          void controller.actions.setCompactMode(false);
        }}
      />
    );
  }

  const pageTitle =
    navItems.find((item) => item.id === activePage)?.label ?? "Overview";
  const page = (() => {
    switch (activePage) {
      case "overview":
        return (
          <OverviewPage
            snapshot={snapshot}
            actions={controller.actions}
            pendingAction={controller.pendingAction}
            onNavigate={setActivePage}
          />
        );
      case "gather":
        return (
          <GatherPage
            key={snapshot.activeProfileId}
            snapshot={snapshot}
            actions={controller.actions}
            pendingAction={controller.pendingAction}
          />
        );
      case "activities":
        return (
          <FeaturePage
            category="activity"
            eyebrow="Collect, fight & explore"
            title="Activities"
            description="Schedule recurring Bee Swarm tasks without letting one failure block the rest."
            snapshot={snapshot}
            actions={controller.actions}
            pendingAction={controller.pendingAction}
            aside={
              <div className="feature-highlight">
                <span>
                  <Flower2 size={20} />
                </span>
                <div>
                  <strong>Next available activity</strong>
                  <p>
                    King Beetle is ready. The scheduler will visit after the
                    current gathering cycle.
                  </p>
                </div>
                <button className="button button-secondary button-small">
                  View schedule
                </button>
              </div>
            }
          />
        );
      case "boosts":
        return (
          <FeaturePage
            category="boost"
            eyebrow="Measured item use"
            title="Boosts"
            description="Coordinate field boosts and consumables while enforcing hard spending limits."
            snapshot={snapshot}
            actions={controller.actions}
            pendingAction={controller.pendingAction}
            aside={
              <div className="feature-highlight">
                <span>
                  <Sparkles size={20} />
                </span>
                <div>
                  <strong>Valuable items are protected</strong>
                  <p>
                    All item budgets are zero. Automatic boost selection can use
                    only free dispensers.
                  </p>
                </div>
                <button
                  className="button button-secondary button-small"
                  onClick={() => setActivePage("settings")}
                >
                  Edit budgets
                </button>
              </div>
            }
          />
        );
      case "quests":
        return (
          <FeaturePage
            category="quest"
            eyebrow="Evidence before travel"
            title="Quests"
            description="Track supported quest goals and travel only when a target is confidently identified."
            snapshot={snapshot}
            actions={controller.actions}
            pendingAction={controller.pendingAction}
            aside={
              <div className="quest-intelligence-stack">
                <div className="feature-highlight feature-highlight-safe">
                  <span>
                    <ShieldCheck size={20} />
                  </span>
                  <div>
                    <strong>Unknown is never a destination</strong>
                    <p>
                      Uncertain quest-giver or field detections pause for
                      another observation instead of moving.
                    </p>
                  </div>
                </div>
                <article className="quest-intelligence-card">
                  <header>
                    <span className="quest-brain-icon">
                      <BrainCircuit size={21} />
                    </span>
                    <div>
                      <span className="eyebrow">Smart quest planner</span>
                      <h3>Science Bear knowledge pack</h3>
                    </div>
                    <span className="safe-default-badge">Versioned data</span>
                  </header>
                  <div className="quest-intelligence-metrics">
                    <div>
                      <strong>31</strong>
                      <span>quests indexed</span>
                    </div>
                    <div>
                      <strong>3</strong>
                      <span>Translator milestones</span>
                    </div>
                    <div>
                      <strong>0</strong>
                      <span>uncertain targets allowed</span>
                    </div>
                  </div>
                  <footer>
                    <p>
                      Overlap scoring combines field, color, goo, token, and mob
                      goals, then discounts travel and holds valuable-item tasks
                      behind explicit budgets.
                    </p>
                    <span>Awaiting a confident in-game quest scan</span>
                  </footer>
                </article>
              </div>
            }
          />
        );
      case "planters":
        return (
          <FeaturePage
            category="planter"
            eyebrow="Nectar-aware cycles"
            title="Planters"
            description="Plan harvest timing and field rotation around your nectar priorities."
            snapshot={snapshot}
            actions={controller.actions}
            pendingAction={controller.pendingAction}
            aside={
              <div className="planter-strip">
                <span>
                  <PackageOpen size={21} />
                </span>
                <div>
                  <strong>Pesticide planter · 72%</strong>
                  <p>Pine Tree Forest · about 18 minutes remaining</p>
                </div>
                <div className="planter-progress">
                  <i style={{ width: "72%" }} />
                </div>
              </div>
            }
          />
        );
      case "monitoring":
        return <MonitoringPage snapshot={snapshot} />;
      case "extensions":
        return (
          <ExtensionsPage
            snapshot={snapshot}
            actions={controller.actions}
            pendingAction={controller.pendingAction}
          />
        );
      case "settings":
        return (
          <SettingsPage
            snapshot={snapshot}
            actions={controller.actions}
            pendingAction={controller.pendingAction}
            theme={theme.preference}
            onThemeChange={theme.setPreference}
          />
        );
      case "diagnostics":
        return (
          <DiagnosticsPage
            snapshot={snapshot}
            actions={controller.actions}
            pendingAction={controller.pendingAction}
          />
        );
      case "about":
        return <AboutPage />;
    }
  })();

  return (
    <div
      className={`app-shell ${sidebarCollapsed ? "app-shell-collapsed" : ""}`}
    >
      <Sidebar
        active={activePage}
        collapsed={sidebarCollapsed}
        mobileOpen={mobileNavOpen}
        onNavigate={setActivePage}
        onToggleCollapsed={() => setSidebarCollapsed((value) => !value)}
        onCloseMobile={() => setMobileNavOpen(false)}
      />
      <div className="app-workspace">
        <TopBar
          title={pageTitle}
          connected={snapshot.session.connected}
          theme={theme.preference}
          resolvedTheme={theme.resolved}
          profiles={snapshot.profiles}
          activeProfileId={snapshot.activeProfileId}
          profilePending={controller.pendingAction === "profile"}
          onThemeChange={theme.setPreference}
          onProfileChange={(profileId) =>
            void controller.actions.selectProfile(profileId)
          }
          onMenu={() => setMobileNavOpen(true)}
          onCompact={() => {
            setCompact(true);
            void controller.actions.setCompactMode(true);
          }}
          onSetup={() => setSetupOpen(true)}
        />
        {controller.error && (
          <div className="global-error" role="alert">
            <AlertTriangle size={17} />
            <span>{controller.error}</span>
            <button onClick={controller.clearError} aria-label="Dismiss error">
              <X size={16} />
            </button>
          </div>
        )}
        <div className="page-scroll" id="main-content">
          {page}
        </div>
      </div>
      <button
        className="floating-menu"
        onClick={() => setMobileNavOpen(true)}
        aria-label="Open navigation"
      >
        <Menu size={20} />
      </button>
      <OnboardingDialog
        snapshot={snapshot}
        actions={controller.actions}
        open={setupOpen}
        required={!snapshot.onboardingComplete}
        pending={controller.pendingAction === "onboarding"}
        onClose={() => setSetupOpen(false)}
      />
    </div>
  );
}
