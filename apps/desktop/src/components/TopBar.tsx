import {
  Bell,
  HelpCircle,
  Menu,
  MonitorUp,
  Moon,
  PanelTop,
  Sun,
} from "lucide-react";
import type { Profile, ThemePreference } from "../types/contracts";

interface TopBarProps {
  title: string;
  connected: boolean;
  theme: ThemePreference;
  resolvedTheme: "light" | "dark";
  profiles: Profile[];
  activeProfileId: string;
  profilePending: boolean;
  onThemeChange(theme: ThemePreference): void;
  onProfileChange(profileId: string): void;
  onMenu(): void;
  onCompact(): void;
  onSetup(): void;
}

export function TopBar({
  title,
  connected,
  theme,
  resolvedTheme,
  profiles,
  activeProfileId,
  profilePending,
  onThemeChange,
  onProfileChange,
  onMenu,
  onCompact,
  onSetup,
}: TopBarProps) {
  const nextTheme: ThemePreference =
    theme === "system" ? "light" : theme === "light" ? "dark" : "system";
  const themeLabel = `Theme: ${theme}. Switch to ${nextTheme}`;

  return (
    <header className="topbar">
      <div className="topbar-title">
        <button
          className="icon-button mobile-menu-button"
          onClick={onMenu}
          aria-label="Open navigation"
        >
          <Menu size={20} />
        </button>
        <div>
          <span className="eyebrow">NectarPilot</span>
          <h1>{title}</h1>
        </div>
      </div>
      <div className="topbar-actions">
        <label className="profile-select">
          <span className="sr-only">Active profile</span>
          <span
            className="profile-select-avatar"
            style={{
              background: profiles.find(
                (profile) => profile.id === activeProfileId,
              )?.accent,
            }}
          >
            {profiles
              .find((profile) => profile.id === activeProfileId)
              ?.name.slice(0, 1) ?? "N"}
          </span>
          <select
            value={activeProfileId}
            disabled={profilePending}
            onChange={(event) => onProfileChange(event.target.value)}
          >
            {profiles.map((profile) => (
              <option key={profile.id} value={profile.id}>
                {profile.name}
              </option>
            ))}
          </select>
        </label>
        <div
          className={`connection-badge ${connected ? "connected" : "disconnected"}`}
        >
          <span />
          <span className="connection-copy">
            {connected ? "Roblox connected" : "Roblox not found"}
          </span>
        </div>
        <button
          className="icon-button"
          onClick={onSetup}
          aria-label="Open setup guide"
          title="Setup guide"
        >
          <HelpCircle size={19} />
        </button>
        <button
          className="icon-button"
          aria-label="Notifications"
          title="Notifications"
        >
          <Bell size={19} />
          <span className="notification-dot" />
        </button>
        <button
          className="icon-button"
          onClick={() => onThemeChange(nextTheme)}
          aria-label={themeLabel}
          title={themeLabel}
        >
          {theme === "system" ? (
            <MonitorUp size={19} />
          ) : resolvedTheme === "dark" ? (
            <Moon size={19} />
          ) : (
            <Sun size={19} />
          )}
        </button>
        <button
          className="icon-button compact-trigger"
          onClick={onCompact}
          aria-label="Open compact monitor"
          title="Compact monitor"
        >
          <PanelTop size={19} />
        </button>
      </div>
    </header>
  );
}
