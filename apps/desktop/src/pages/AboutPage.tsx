import {
  ExternalLink,
  Github,
  Heart,
  Scale,
  ShieldCheck,
  Sparkles,
} from "lucide-react";
import { NectarMark } from "../components/brand";

export function AboutPage() {
  return (
    <div className="page about-page">
      <section className="about-hero panel">
        <NectarMark className="about-mark" />
        <span className="eyebrow">NectarPilot for Windows</span>
        <h2>Automation you can actually understand.</h2>
        <p>
          A safety-focused Bee Swarm Simulator companion built on the work of
          Natro Macro and its community.
        </p>
        <div className="about-version">
          <span>Version 0.1.0-beta</span>
          <span>Protocol v1</span>
          <span>Update channel · Beta</span>
        </div>
        <div className="about-actions">
          <button className="button button-primary">
            <Sparkles size={16} /> Check for updates
          </button>
          <button className="button button-secondary">
            <Github size={16} /> View source <ExternalLink size={13} />
          </button>
        </div>
      </section>
      <section className="about-card-grid">
        <article className="panel about-info-card">
          <span>
            <Heart size={21} />
          </span>
          <h3>Built on Natro Macro</h3>
          <p>
            NectarPilot is a modified, non-affiliated GPLv3 fork. We preserve
            attribution to Natro Team and contributors.
          </p>
        </article>
        <article className="panel about-info-card">
          <span>
            <Scale size={21} />
          </span>
          <h3>Free & open source</h3>
          <p>
            Licensed under GNU GPLv3. Source, modification notes, and
            third-party notices ship with every build.
          </p>
        </article>
        <article className="panel about-info-card">
          <span>
            <ShieldCheck size={21} />
          </span>
          <h3>No client modification</h3>
          <p>
            Screen and input automation only—no injection, memory reading,
            Roblox modification, or anti-cheat bypass.
          </p>
        </article>
      </section>
      <section className="panel legal-notice">
        <h3>Account-risk notice</h3>
        <p>
          Roblox prohibits cheating and exploiting and may moderate accounts.
          Automation can carry account risk. Use conservative settings,
          supervise early runs, and proceed at your own discretion.
        </p>
        <button className="text-button">
          Read Roblox policy <ExternalLink size={14} />
        </button>
      </section>
      <footer className="about-footer">
        <span>
          © NectarPilot contributors · Natro Team attribution retained
        </span>
        <nav>
          <button>License</button>
          <button>Third-party notices</button>
          <button>Privacy</button>
          <button>Release notes</button>
        </nav>
      </footer>
    </div>
  );
}
