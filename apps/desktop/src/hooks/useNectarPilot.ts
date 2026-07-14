import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  createNectarService,
  type NectarService,
} from "../services/nectar-service";
import type { AutomationSettings, DashboardSnapshot } from "../types/contracts";

const defaultService = createNectarService();

export interface NectarActions {
  refreshSession(): Promise<boolean>;
  start(): Promise<void>;
  acknowledgeAttention(): Promise<void>;
  pause(): Promise<void>;
  stop(): Promise<void>;
  emergencyStop(): Promise<void>;
  selectProfile(profileId: string): Promise<void>;
  /** Returns false when the daemon rejected the document; callers must keep drafts dirty. */
  saveSettings(settings: AutomationSettings): Promise<boolean>;
  /** Returns false when setup could not be persisted; callers must remain open. */
  completeOnboarding(): Promise<boolean>;
  trustExtension(extensionId: string, digest: string): Promise<boolean>;
  /** Returns true only after the daemon accepted the contained-run request. */
  runLegacyExtension(extensionId: string, digest: string): Promise<boolean>;
  startLegacySession(maxCycles: number, maxMinutes: number): Promise<void>;
  inspectLegacy(scriptId: string): Promise<void>;
  scanQuests(): Promise<void>;
  setCompactMode(compact: boolean): Promise<void>;
}

export interface NectarController {
  snapshot: DashboardSnapshot | null;
  loading: boolean;
  pendingAction: string | null;
  error: string | null;
  clearError(): void;
  actions: NectarActions;
}

function errorMessage(error: unknown): string {
  // Tauri commands reject with the Rust `Err(String)` value directly, so the
  // caught cause is usually a plain string — surface it rather than hiding the
  // real reason behind a generic message.
  if (typeof error === "string" && error.trim().length > 0) {
    return error;
  }
  if (error instanceof Error && error.message.trim().length > 0) {
    return error.message;
  }
  if (error && typeof error === "object") {
    const candidate = error as { message?: unknown; reason?: unknown };
    if (typeof candidate.message === "string" && candidate.message.length > 0) {
      return candidate.message;
    }
    if (typeof candidate.reason === "string" && candidate.reason.length > 0) {
      return candidate.reason;
    }
    try {
      return JSON.stringify(error);
    } catch {
      // fall through to the generic message
    }
  }
  return "NectarPilot could not complete that action.";
}

export function useNectarPilot(
  service: NectarService = defaultService,
): NectarController {
  const [snapshot, setSnapshot] = useState<DashboardSnapshot | null>(null);
  const [loading, setLoading] = useState(true);
  const [pendingAction, setPendingAction] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const activeProfileId = useRef<string | null>(null);

  const applySnapshot = useCallback((nextSnapshot: DashboardSnapshot) => {
    activeProfileId.current = nextSnapshot.activeProfileId;
    setSnapshot(nextSnapshot);
  }, []);

  const refreshSnapshot = useCallback(async () => {
    const nextSnapshot = await service.getSnapshot();
    applySnapshot(nextSnapshot);
  }, [applySnapshot, service]);

  useEffect(() => {
    let mounted = true;
    const unsubscribe = service.subscribe((nextSnapshot) => {
      if (mounted) applySnapshot(nextSnapshot);
    });

    void refreshSnapshot()
      .catch((cause: unknown) => {
        if (mounted) setError(errorMessage(cause));
      })
      .finally(() => {
        if (mounted) setLoading(false);
      });

    return () => {
      mounted = false;
      unsubscribe();
    };
  }, [applySnapshot, refreshSnapshot, service]);

  const run = useCallback(
    async (
      name: string,
      action: (profileId: string) => Promise<void>,
    ): Promise<boolean> => {
      const profileId = activeProfileId.current;
      if (!profileId) {
        setError(
          "NectarPilot has not loaded a profile yet. Refresh the desktop session and try again.",
        );
        return false;
      }
      setPendingAction(name);
      setError(null);
      try {
        await action(profileId);
        // Commands resolve when the daemon accepts them. Pull a fresh
        // snapshot as well as listening for events so a one-shot event can
        // never leave the UI showing stale safe-mode or settings state.
        try {
          await refreshSnapshot();
        } catch (refreshCause) {
          setError(
            `The action was accepted, but the status could not refresh: ${errorMessage(refreshCause)}`,
          );
        }
        return true;
      } catch (cause) {
        setError(errorMessage(cause));
        return false;
      } finally {
        setPendingAction(null);
      }
    },
    [refreshSnapshot],
  );

  const actions = useMemo<NectarActions>(
    () => ({
      refreshSession: async () => {
        setPendingAction("refresh-session");
        setError(null);
        try {
          await refreshSnapshot();
          return true;
        } catch (cause) {
          setError(errorMessage(cause));
          return false;
        } finally {
          setPendingAction(null);
        }
      },
      start: async () => {
        await run("start", (profileId) => service.start(profileId));
      },
      acknowledgeAttention: async () => {
        await run("acknowledge-attention", (profileId) =>
          service.acknowledgeAttention(profileId),
        );
      },
      pause: async () => {
        await run("pause", (profileId) => service.pause(profileId));
      },
      stop: async () => {
        await run("stop", (profileId) => service.stop(profileId));
      },
      emergencyStop: async () => {
        await run("emergency-stop", (profileId) =>
          service.emergencyStop(profileId),
        );
      },
      selectProfile: async (profileId) => {
        setPendingAction("profile");
        setError(null);
        try {
          await service.selectProfile(profileId);
          activeProfileId.current = profileId;
          await refreshSnapshot();
        } catch (cause) {
          setError(errorMessage(cause));
        } finally {
          setPendingAction(null);
        }
      },
      saveSettings: (settings) =>
        run("save-settings", (profileId) =>
          service.saveSettings(profileId, settings),
        ),
      completeOnboarding: () =>
        run("onboarding", (profileId) => service.completeOnboarding(profileId)),
      trustExtension: (extensionId, digest) =>
        run("trust-extension", (profileId) =>
          service.trustExtension(profileId, extensionId, digest),
        ),
      runLegacyExtension: (extensionId, digest) =>
        run("run-legacy-extension", (profileId) =>
          service.runLegacyExtension(profileId, extensionId, digest),
        ),
      startLegacySession: async (maxCycles, maxMinutes) => {
        await run("start-legacy-session", (profileId) =>
          service.startLegacySession(profileId, maxCycles, maxMinutes),
        );
      },
      inspectLegacy: async (scriptId) => {
        await run("inspect-legacy", (profileId) =>
          service.inspectLegacy(profileId, scriptId),
        );
      },
      scanQuests: async () => {
        await run("scan-quests", (profileId) => service.scanQuests(profileId));
      },
      setCompactMode: async (compact) => {
        setPendingAction("compact-mode");
        setError(null);
        try {
          await service.setCompactMode(compact);
          await refreshSnapshot();
        } catch (cause) {
          setError(errorMessage(cause));
        } finally {
          setPendingAction(null);
        }
      },
    }),
    [refreshSnapshot, run, service],
  );

  return {
    snapshot,
    loading,
    pendingAction,
    error,
    clearError: () => setError(null),
    actions,
  };
}
