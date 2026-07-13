import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  PROTOCOL_VERSION,
  type AutomationSettings,
  type CommandEnvelope,
  type DaemonCommand,
  type DaemonEventEnvelope,
  type DashboardSnapshot,
  type Profile,
} from "../types/contracts";
import { createMockSnapshot } from "./seed";

export type SnapshotListener = (snapshot: DashboardSnapshot) => void;

export interface NectarService {
  getSnapshot(): Promise<DashboardSnapshot>;
  subscribe(listener: SnapshotListener): () => void;
  start(profileId: string): Promise<void>;
  acknowledgeAttention(profileId: string): Promise<void>;
  pause(profileId: string): Promise<void>;
  stop(profileId: string): Promise<void>;
  emergencyStop(profileId: string): Promise<void>;
  selectProfile(profileId: string): Promise<void>;
  saveSettings(profileId: string, settings: AutomationSettings): Promise<void>;
  completeOnboarding(profileId: string): Promise<void>;
  trustExtension(
    profileId: string,
    extensionId: string,
    digest: string,
  ): Promise<void>;
  runLegacyExtension(
    profileId: string,
    extensionId: string,
    digest: string,
  ): Promise<void>;
  startLegacySession(
    profileId: string,
    maxCycles: number,
    maxMinutes: number,
  ): Promise<void>;
  inspectLegacy(profileId: string, scriptId: string): Promise<void>;
  scanQuests(profileId: string): Promise<void>;
  setCompactMode(compact: boolean): Promise<void>;
}

function commandEnvelope(
  profileId: string,
  command: DaemonCommand,
): CommandEnvelope {
  return {
    protocol_version: PROTOCOL_VERSION,
    request_id: crypto.randomUUID(),
    profile_id: profileId,
    command,
  };
}

export class TauriNectarService implements NectarService {
  private lastRunState: DashboardSnapshot["runState"] = "Idle";

  async getSnapshot(): Promise<DashboardSnapshot> {
    const snapshot = await invoke<DashboardSnapshot>("get_dashboard_snapshot");
    this.lastRunState = snapshot.runState;
    return snapshot;
  }

  subscribe(listener: SnapshotListener): () => void {
    let cancelled = false;
    let unlisten: UnlistenFn | undefined;
    void listen<DaemonEventEnvelope>("nectarpilot:event", () => {
      if (cancelled) return;
      void this.getSnapshot().then((snapshot) => {
        if (!cancelled) listener(snapshot);
      });
    }).then((dispose) => {
      if (cancelled) dispose();
      else unlisten = dispose;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }

  private async dispatch(
    profileId: string,
    command: DaemonCommand,
  ): Promise<void> {
    await invoke("dispatch_command", {
      envelope: commandEnvelope(profileId, command),
    });
  }

  start(profileId: string) {
    return invoke<void>("start_configured_session", { profileId });
  }

  acknowledgeAttention(profileId: string) {
    return invoke<void>("acknowledge_attention", { profileId });
  }

  pause(profileId: string) {
    return this.dispatch(profileId, {
      type: this.lastRunState === "Paused" ? "resume" : "pause",
    });
  }

  stop(profileId: string) {
    return this.dispatch(profileId, { type: "stop" });
  }

  emergencyStop(profileId: string) {
    return this.dispatch(profileId, { type: "emergency_stop" });
  }

  selectProfile(profileId: string) {
    return invoke<void>("select_profile", { profileId });
  }

  saveSettings(profileId: string, settings: AutomationSettings) {
    return invoke<void>("save_automation_settings", { profileId, settings });
  }

  completeOnboarding(profileId: string) {
    return invoke<void>("complete_onboarding", { profileId });
  }

  trustExtension(profileId: string, extensionId: string, digest: string) {
    return invoke<void>("trust_extension", { profileId, extensionId, digest });
  }

  runLegacyExtension(profileId: string, extensionId: string, digest: string) {
    return invoke<void>("start_legacy_extension", {
      profileId,
      extensionId,
      digest,
    });
  }

  startLegacySession(profileId: string, maxCycles: number, maxMinutes: number) {
    return invoke<void>("start_legacy_session", {
      profileId,
      maxCycles,
      maxMinutes,
    });
  }

  inspectLegacy(profileId: string, scriptId: string) {
    return invoke<void>("inspect_legacy", { profileId, scriptId });
  }

  scanQuests(profileId: string) {
    return invoke<void>("scan_quests", { profileId });
  }

  async setCompactMode(compact: boolean): Promise<void> {
    await invoke("set_compact_mode", { compact });
  }
}

export class MockNectarService implements NectarService {
  private snapshot = createMockSnapshot();
  private readonly listeners = new Set<SnapshotListener>();
  private transitionTimer?: number;

  async getSnapshot(): Promise<DashboardSnapshot> {
    return structuredClone(this.snapshot);
  }

  subscribe(listener: SnapshotListener): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  private publish(): void {
    this.snapshot.updatedAt = new Date().toISOString();
    const copy = structuredClone(this.snapshot);
    this.listeners.forEach((listener) => listener(copy));
  }

  private addTimeline(
    title: string,
    detail: string,
    tone: "success" | "info" | "warning" | "danger",
  ): void {
    this.snapshot.timeline.unshift({
      id: crypto.randomUUID(),
      timestamp: new Date().toISOString(),
      title,
      detail,
      tone,
    });
    this.snapshot.timeline = this.snapshot.timeline.slice(0, 12);
  }

  async start(): Promise<void> {
    this.snapshot.runState = "Preflight";
    this.snapshot.runStateReason =
      "Checking window focus, calibration, and safety limits";
    this.addTimeline(
      "Preflight started",
      "Verifying Roblox and profile safeguards",
      "info",
    );
    this.publish();
    if (this.transitionTimer !== undefined)
      window.clearTimeout(this.transitionTimer);
    this.transitionTimer = window.setTimeout(() => {
      this.snapshot.runState = "Running";
      this.snapshot.runId = crypto.randomUUID();
      this.snapshot.runStateReason = "Gathering in Pine Tree Forest";
      this.snapshot.queue = this.snapshot.queue.map((task, index) => ({
        ...task,
        status: index === 0 ? "active" : index === 1 ? "next" : task.status,
      }));
      this.addTimeline(
        "Macro started",
        "Pine Tree Forest · e_lol pattern",
        "success",
      );
      this.publish();
    }, 650);
  }

  async acknowledgeAttention(): Promise<void> {
    this.snapshot.safeMode = false;
    if (
      this.snapshot.runState === "Faulted" ||
      this.snapshot.runState === "NeedsAttention"
    ) {
      this.snapshot.runState = "Idle";
      this.snapshot.runStateReason = "Attention acknowledged";
    }
    this.addTimeline(
      "Attention acknowledged",
      "Safe mode and recovery notices were cleared in the preview.",
      "info",
    );
    this.publish();
  }

  async pause(): Promise<void> {
    this.snapshot.runState =
      this.snapshot.runState === "Paused" ? "Running" : "Paused";
    this.snapshot.runStateReason =
      this.snapshot.runState === "Paused"
        ? "Paused by user"
        : "Resumed by user";
    this.addTimeline(
      this.snapshot.runState === "Paused" ? "Macro paused" : "Macro resumed",
      "All held inputs were released safely",
      "info",
    );
    this.publish();
  }

  async stop(): Promise<void> {
    if (this.transitionTimer !== undefined)
      window.clearTimeout(this.transitionTimer);
    this.snapshot.runState = "Idle";
    this.snapshot.runId = null;
    this.snapshot.runStateReason = "Stopped safely";
    this.addTimeline("Macro stopped", "Session ended normally", "info");
    this.publish();
  }

  async emergencyStop(): Promise<void> {
    if (this.transitionTimer !== undefined)
      window.clearTimeout(this.transitionTimer);
    this.snapshot.runState = "Idle";
    this.snapshot.runId = null;
    this.snapshot.runStateReason =
      "Emergency stop activated — all input released";
    this.addTimeline(
      "Emergency stop",
      "All keyboard and mouse input was released",
      "danger",
    );
    this.publish();
  }

  async selectProfile(profileId: string): Promise<void> {
    if (!this.snapshot.profiles.some((profile) => profile.id === profileId))
      return;
    this.snapshot.activeProfileId = profileId;
    this.snapshot.runHistory = (this.snapshot.runHistory ?? []).filter(
      (record) => record.profileId === profileId,
    );
    this.snapshot.legacyInspection = null;
    this.snapshot.questScan = null;
    const profile = this.snapshot.profiles.find(
      (item) => item.id === profileId,
    ) as Profile;
    this.addTimeline(
      "Profile changed",
      `${profile.name} is now active`,
      "info",
    );
    this.publish();
  }

  async saveSettings(
    profileId: string,
    settings: AutomationSettings,
  ): Promise<void> {
    this.snapshot.profiles = this.snapshot.profiles.map((profile) =>
      profile.id === profileId
        ? { ...profile, settings: structuredClone(settings) }
        : profile,
    );
    this.snapshot.features = this.snapshot.features.map((feature) => ({
      ...feature,
      enabled: settings.features[feature.id] ?? feature.enabled,
    }));
    this.addTimeline(
      "Settings applied",
      "Profile changes passed validation",
      "success",
    );
    this.publish();
  }

  async completeOnboarding(): Promise<void> {
    this.snapshot.onboardingComplete = true;
    this.addTimeline(
      "Setup completed",
      "NectarPilot is ready for a controlled test run",
      "success",
    );
    this.publish();
  }

  async trustExtension(
    _profileId: string,
    extensionId: string,
    digest: string,
  ): Promise<void> {
    this.snapshot.extensions = this.snapshot.extensions.map((extension) =>
      extension.id === extensionId && extension.digest === digest
        ? { ...extension, trust: "trusted", enabled: true }
        : extension,
    );
    this.addTimeline(
      "Extension trusted",
      "The reviewed extension digest was added to this profile",
      "warning",
    );
    this.publish();
  }

  async runLegacyExtension(
    _profileId: string,
    extensionId: string,
    digest: string,
  ): Promise<void> {
    const extension = this.snapshot.extensions.find(
      (candidate) =>
        candidate.id === extensionId && candidate.digest === digest,
    );
    if (!extension || extension.trust !== "trusted") {
      throw new Error("Trust this exact legacy digest before running it.");
    }
    this.addTimeline(
      "Legacy compatibility requested",
      `${extension.name} will run only in the contained daemon worker`,
      "warning",
    );
    this.publish();
  }

  async startLegacySession(
    _profileId: string,
    maxCycles: number,
    maxMinutes: number,
  ): Promise<void> {
    this.addTimeline(
      "Legacy session requested",
      `Field rotation loop for up to ${maxCycles} cycles / ${maxMinutes} minutes`,
      "warning",
    );
    this.publish();
  }

  async scanQuests(): Promise<void> {
    this.addTimeline(
      "Quest scan requested",
      "The daemon opens the quest log with verified clicks and reads it",
      "info",
    );
    this.publish();
  }

  async inspectLegacy(_profileId: string, scriptId: string): Promise<void> {
    this.addTimeline(
      "Harness preview requested",
      `The daemon renders the exact generated script for ${scriptId}`,
      "info",
    );
    this.publish();
  }

  async setCompactMode(): Promise<void> {
    // The browser preview keeps its viewport. Tauri owns native window sizing.
  }
}

declare global {
  interface Window {
    __TAURI_INTERNALS__?: unknown;
  }
}

export function createNectarService(): NectarService {
  const useMock =
    import.meta.env.VITE_FORCE_MOCK === "true" ||
    window.__TAURI_INTERNALS__ === undefined;
  return useMock ? new MockNectarService() : new TauriNectarService();
}
