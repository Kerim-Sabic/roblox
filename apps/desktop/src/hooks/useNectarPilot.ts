import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  createNectarService,
  type NectarService,
} from "../services/nectar-service";
import type { AutomationSettings, DashboardSnapshot } from "../types/contracts";

const defaultService = createNectarService();

export interface NectarActions {
  refreshSession(): Promise<void>;
  start(): Promise<void>;
  pause(): Promise<void>;
  stop(): Promise<void>;
  emergencyStop(): Promise<void>;
  selectProfile(profileId: string): Promise<void>;
  saveSettings(settings: AutomationSettings): Promise<void>;
  completeOnboarding(): Promise<void>;
  trustExtension(extensionId: string, digest: string): Promise<void>;
  runLegacyExtension(extensionId: string, digest: string): Promise<void>;
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
  return error instanceof Error
    ? error.message
    : "NectarPilot could not complete that action.";
}

export function useNectarPilot(
  service: NectarService = defaultService,
): NectarController {
  const [snapshot, setSnapshot] = useState<DashboardSnapshot | null>(null);
  const [loading, setLoading] = useState(true);
  const [pendingAction, setPendingAction] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const activeProfileId = useRef<string | null>(null);

  useEffect(() => {
    let mounted = true;
    const unsubscribe = service.subscribe((nextSnapshot) => {
      activeProfileId.current = nextSnapshot.activeProfileId;
      setSnapshot(nextSnapshot);
    });

    void service
      .getSnapshot()
      .then((nextSnapshot) => {
        if (!mounted) return;
        activeProfileId.current = nextSnapshot.activeProfileId;
        setSnapshot(nextSnapshot);
      })
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
  }, [service]);

  const run = useCallback(
    async (name: string, action: (profileId: string) => Promise<void>) => {
      const profileId = activeProfileId.current;
      if (!profileId) return;
      setPendingAction(name);
      setError(null);
      try {
        await action(profileId);
      } catch (cause) {
        setError(errorMessage(cause));
      } finally {
        setPendingAction(null);
      }
    },
    [],
  );

  const actions = useMemo<NectarActions>(
    () => ({
      refreshSession: async () => {
        setPendingAction("refresh-session");
        setError(null);
        try {
          const nextSnapshot = await service.getSnapshot();
          activeProfileId.current = nextSnapshot.activeProfileId;
          setSnapshot(nextSnapshot);
        } catch (cause) {
          setError(errorMessage(cause));
        } finally {
          setPendingAction(null);
        }
      },
      start: () => run("start", (profileId) => service.start(profileId)),
      pause: () => run("pause", (profileId) => service.pause(profileId)),
      stop: () => run("stop", (profileId) => service.stop(profileId)),
      emergencyStop: () =>
        run("emergency-stop", (profileId) => service.emergencyStop(profileId)),
      selectProfile: async (profileId) => {
        setPendingAction("profile");
        setError(null);
        try {
          await service.selectProfile(profileId);
          activeProfileId.current = profileId;
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
      setCompactMode: async (compact) => {
        setPendingAction("compact-mode");
        setError(null);
        try {
          await service.setCompactMode(compact);
        } catch (cause) {
          setError(errorMessage(cause));
        } finally {
          setPendingAction(null);
        }
      },
    }),
    [run, service],
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
