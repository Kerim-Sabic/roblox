import {
  Activity,
  Blocks,
  Bug,
  ChevronLeft,
  ChevronRight,
  CircleGauge,
  Flower2,
  Gauge,
  Info,
  LayoutDashboard,
  PackageOpen,
  Settings,
  Sparkles,
} from "lucide-react";
import { NectarMark } from "./brand";

export const navItems = [
  { id: "overview", label: "Overview", icon: LayoutDashboard },
  { id: "gather", label: "Gather", icon: Flower2 },
  { id: "activities", label: "Activities", icon: Activity },
  { id: "boosts", label: "Boosts", icon: Sparkles },
  { id: "quests", label: "Quests", icon: Blocks },
  { id: "planters", label: "Planters", icon: PackageOpen },
  { id: "monitoring", label: "Monitoring", icon: Gauge },
  { id: "extensions", label: "Extensions", icon: Bug },
  { id: "settings", label: "Settings", icon: Settings },
  { id: "diagnostics", label: "Diagnostics", icon: CircleGauge },
  { id: "about", label: "About", icon: Info },
] as const;

export type NavId = (typeof navItems)[number]["id"];

interface SidebarProps {
  active: NavId;
  collapsed: boolean;
  mobileOpen: boolean;
  onNavigate(id: NavId): void;
  onToggleCollapsed(): void;
  onCloseMobile(): void;
}

export function Sidebar({
  active,
  collapsed,
  mobileOpen,
  onNavigate,
  onToggleCollapsed,
  onCloseMobile,
}: SidebarProps) {
  return (
    <>
      {mobileOpen && (
        <button
          className="nav-scrim"
          aria-label="Close navigation"
          onClick={onCloseMobile}
        />
      )}
      <aside
        className={`sidebar ${collapsed ? "sidebar-collapsed" : ""} ${mobileOpen ? "sidebar-mobile-open" : ""}`}
      >
        <div className="brand-block">
          <NectarMark className="brand-mark" />
          {!collapsed && (
            <div className="brand-copy">
              <strong>NectarPilot</strong>
              <span>Automation, in control</span>
            </div>
          )}
        </div>

        <nav className="primary-nav" aria-label="Main navigation">
          {navItems.map((item) => {
            const Icon = item.icon;
            return (
              <button
                key={item.id}
                className={`nav-item ${active === item.id ? "nav-item-active" : ""}`}
                aria-current={active === item.id ? "page" : undefined}
                title={collapsed ? item.label : undefined}
                onClick={() => {
                  onNavigate(item.id);
                  onCloseMobile();
                }}
              >
                <Icon size={19} strokeWidth={1.9} />
                {!collapsed && <span>{item.label}</span>}
              </button>
            );
          })}
        </nav>

        <div className="sidebar-footer">
          {!collapsed && (
            <div className="safety-note">
              <span className="safety-dot" />
              <span>
                <strong>Input guard active</strong>
                Wrong-window input is blocked
              </span>
            </div>
          )}
          <button
            className="collapse-button"
            onClick={onToggleCollapsed}
            aria-label={collapsed ? "Expand sidebar" : "Collapse sidebar"}
          >
            {collapsed ? <ChevronRight size={18} /> : <ChevronLeft size={18} />}
          </button>
        </div>
      </aside>
    </>
  );
}
