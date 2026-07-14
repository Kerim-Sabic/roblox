export const PROTOCOL_VERSION = 3 as const;

export type RunState =
  | "Idle"
  | "Preflight"
  | "Running"
  | "Paused"
  | "Recovering"
  | "NeedsAttention"
  | "Stopping"
  | "Faulted";

export type Confidence = "high" | "medium" | "low";

export type Detection<T> =
  | { status: "found"; value: T; confidence: number; evidence?: string }
  | { status: "not_found"; reason?: string }
  | { status: "uncertain"; candidates: T[]; reason: string }
  | { status: "error"; code: string; message: string };

export type DaemonRunState =
  | "idle"
  | "preflight"
  | "running"
  | "paused"
  | "recovering"
  | "needs_attention"
  | "stopping"
  | "faulted";

export type StartMode = "normal" | "dry_run" | "diagnostics";

export type DaemonCommand =
  | { type: "start"; payload: { mode: StartMode } }
  | { type: "pause" }
  | { type: "resume" }
  | { type: "stop" }
  | { type: "emergency_stop" }
  | { type: "get_snapshot" }
  | { type: "acknowledge_attention" };

export interface CommandEnvelope<
  TCommand extends DaemonCommand = DaemonCommand,
> {
  protocol_version: typeof PROTOCOL_VERSION;
  request_id: string;
  profile_id: string;
  command: TCommand;
}

export interface EventEnvelope<TEvent extends NectarEvent = NectarEvent> {
  protocol_version: typeof PROTOCOL_VERSION;
  sequence: number;
  run_id: string | null;
  timestamp: string;
  event: TEvent;
}

/** Core daemon wire envelope. Payload variants are generated from the Rust contracts. */
export interface DaemonEventEnvelope {
  protocol_version: number;
  sequence: number;
  run_id: string;
  timestamp: string;
  event: { type: string; payload?: unknown };
}

export type NectarEvent =
  | { type: "snapshot"; snapshot: DashboardSnapshot }
  | { type: "run_state_changed"; state: RunState; reason?: string }
  | { type: "timeline_added"; entry: TimelineEntry }
  | { type: "settings_saved"; profile_id: string };

export interface ValuableItemBudgets {
  fieldDice: number;
  glitter: number;
  eggs: number;
  stickers: number;
  vouchers: number;
  shrineDonations: number;
}

export type ValuableItem = keyof ValuableItemBudgets;

export interface RemotePermissions {
  status: boolean;
  macroControl: boolean;
  settings: boolean;
  screenshots: boolean;
  remoteInput: boolean;
  extensionImport: boolean;
  systemPower: boolean;
}

export interface MovementSettings {
  walkSpeed: number;
  hiveSlot: number;
  hiveBees: number;
  keyDelay: number;
  cannonTravel: boolean;
  buffCorrectedWalk: boolean;
}

export interface AutomationSettings {
  features: Record<string, boolean>;
  movement: MovementSettings;
  gathering: {
    enabled: boolean;
    fields: string[];
    pattern: string;
    minutesPerField: number;
    returnAtCapacity: number;
    driftCorrection: boolean;
  };
  safety: {
    pauseOnFocusLoss: boolean;
    requireForeground: boolean;
    confirmHighRiskActions: boolean;
    budgets: ValuableItemBudgets;
  };
  recovery: {
    reconnectEnabled: boolean;
    maxAttempts: number;
    deadlineMinutes: number;
    restartOnConfirmedFreeze: boolean;
  };
  monitoring: {
    discordEnabled: boolean;
    evidenceRetentionDays: number;
    evidenceLimitMb: number;
    permissions: RemotePermissions;
  };
  hotkeys: {
    start: string;
    pause: string;
    stop: string;
    emergencyStop: string;
  };
}

export interface Profile {
  id: string;
  name: string;
  description: string;
  accent: string;
  lastUsedAt: string;
  settings: AutomationSettings;
}

export interface ReadinessCheck {
  id: string;
  label: string;
  detail: string;
  status: "ready" | "warning" | "blocked" | "checking";
  actionLabel?: string;
}

export interface TimelineEntry {
  id: string;
  timestamp: string;
  title: string;
  detail: string;
  tone: "success" | "info" | "warning" | "danger";
}

export interface StatMetric {
  id: string;
  label: string;
  value: string;
  delta?: string;
  tone?: "gold" | "green" | "blue" | "neutral";
}

export interface RunHistoryEntry {
  runId: string;
  profileId: string;
  kind: string;
  startedAt: string;
  finishedAt: string;
  finalState: string;
  summary: string;
  stepsSucceeded: number;
  stepsFailed: number;
}

export interface QuestScanView {
  scannedAt: string;
  giver: string | null;
  questId: string | null;
  questName: string | null;
  barsComplete: boolean[];
  recommendedFields: string[];
  notes: string[];
}

export interface LegacyInspectionView {
  scriptId: string;
  sha256: string;
  bytes: number;
  harnessPreview: string;
}

export interface PlannedTask {
  id: string;
  label: string;
  detail: string;
  status: "active" | "next" | "queued" | "disabled";
  confidence?: number;
}

export interface FeatureCard {
  id: string;
  title: string;
  description: string;
  enabled: boolean;
  status: string;
  category: "activity" | "boost" | "quest" | "planter";
}

export interface ExtensionManifest {
  id: string;
  name: string;
  author: string;
  version: string;
  description: string;
  digest: string;
  trust: "built_in" | "trusted" | "review_required" | "blocked";
  permissions: string[];
  enabled: boolean;
  executionMode?: "legacy_bridge" | "native_preview";
}

export interface DiagnosticLog {
  id: string;
  timestamp: string;
  level: "debug" | "info" | "warning" | "error";
  component: string;
  message: string;
}

export interface SessionInfo {
  connected: boolean;
  processName: string | null;
  pid: number | null;
  windowTitle: string | null;
  resolution: string | null;
  dpi: number | null;
  foreground: boolean;
  calibration: Confidence | null;
}

export interface DashboardSnapshot {
  runId: string | null;
  runState: RunState;
  runStateReason?: string;
  activeProfileId: string;
  profiles: Profile[];
  onboardingComplete: boolean;
  safeMode: boolean;
  session: SessionInfo;
  readiness: ReadinessCheck[];
  metrics: StatMetric[];
  runHistory?: RunHistoryEntry[];
  legacyInspection?: LegacyInspectionView | null;
  questScan?: QuestScanView | null;
  timeline: TimelineEntry[];
  queue: PlannedTask[];
  features: FeatureCard[];
  extensions: ExtensionManifest[];
  logs: DiagnosticLog[];
  updatedAt: string;
}

export type ThemePreference = "system" | "light" | "dark";

export interface UiPreferences {
  theme: ThemePreference;
  compact: boolean;
  sidebarCollapsed: boolean;
}

export function activeProfile(snapshot: DashboardSnapshot): Profile {
  const profile = snapshot.profiles.find(
    (item) => item.id === snapshot.activeProfileId,
  );
  if (!profile) {
    throw new Error(
      `Active profile ${snapshot.activeProfileId} was not supplied by the daemon`,
    );
  }
  return profile;
}

export function detectionCanTarget<T>(
  detection: Detection<T>,
): detection is Extract<Detection<T>, { status: "found" }> {
  return detection.status === "found" && detection.confidence >= 0.7;
}

const runStatePresentation: Record<DaemonRunState, RunState> = {
  idle: "Idle",
  preflight: "Preflight",
  running: "Running",
  paused: "Paused",
  recovering: "Recovering",
  needs_attention: "NeedsAttention",
  stopping: "Stopping",
  faulted: "Faulted",
};

export function toUiRunState(state: DaemonRunState): RunState {
  return runStatePresentation[state];
}
