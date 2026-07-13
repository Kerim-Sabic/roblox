import { useEffect, useMemo, useState } from "react";
import type { ThemePreference } from "../types/contracts";

const STORAGE_KEY = "nectarpilot.theme";

function storedTheme(): ThemePreference {
  const value = window.localStorage.getItem(STORAGE_KEY);
  return value === "light" || value === "dark" || value === "system"
    ? value
    : "system";
}

export function useTheme() {
  const [preference, setPreference] = useState<ThemePreference>(storedTheme);
  const [systemDark, setSystemDark] = useState(
    () => window.matchMedia("(prefers-color-scheme: dark)").matches,
  );

  useEffect(() => {
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const update = (event: MediaQueryListEvent) => setSystemDark(event.matches);
    media.addEventListener("change", update);
    return () => media.removeEventListener("change", update);
  }, []);

  const resolved =
    preference === "system" ? (systemDark ? "dark" : "light") : preference;

  useEffect(() => {
    document.documentElement.dataset.theme = resolved;
    document.documentElement.style.colorScheme = resolved;
    window.localStorage.setItem(STORAGE_KEY, preference);
  }, [preference, resolved]);

  return useMemo(
    () => ({ preference, resolved, setPreference }),
    [preference, resolved],
  );
}
