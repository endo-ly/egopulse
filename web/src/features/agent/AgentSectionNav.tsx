import type { ReactNode } from "react";

import type { AgentSection } from "../../app/router";
import { fetchAgentProfile } from "../../shared/api/agents";
import type { AgentProfile } from "../../shared/api/types";
import { useAgentQuery } from "./agentQuery";
import { formatChars, totalMemoryChars } from "./profileFormat";

export interface AgentSectionNavProps {
  agentId: string;
  authToken: string;
  section: AgentSection;
  onSelectSection: (section: AgentSection) => void;
}

function icon(path: string): ReactNode {
  return (
    <svg
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d={path} />
    </svg>
  );
}

interface SectionItem {
  section: AgentSection;
  label: string;
  icon: ReactNode;
  /** One-line summary; absent until the profile has loaded. */
  summary: (profile: AgentProfile) => string;
}

const ITEMS: SectionItem[] = [
  {
    section: "overview",
    label: "Overview",
    icon: icon("M3 3h7v7H3zM14 3h7v7h-7zM3 14h7v7H3zM14 14h7v7h-7z"),
    summary: (profile) => profile.llm[0]?.model ?? "",
  },
  {
    section: "soul",
    label: "Soul",
    icon: icon("M20.8 4.6a5.5 5.5 0 0 0-7.8 0L12 5.7l-1-1.1a5.5 5.5 0 0 0-7.8 7.8L12 21.2l8.8-8.8a5.5 5.5 0 0 0 0-7.8z"),
    summary: (profile) => profile.prompt_sources.soul?.path ?? "Not set",
  },
  {
    section: "rules",
    label: "Rules",
    icon: icon("M8 6h13M8 12h13M8 18h13M3 6h.01M3 12h.01M3 18h.01"),
    summary: (profile) => {
      const count = profile.prompt_sources.rules.length;
      return count === 0 ? "None" : `${count} AGENTS.md`;
    },
  },
  {
    section: "secret",
    label: "Secret",
    icon: icon("M5 11h14v10H5zM8 11V7a4 4 0 0 1 8 0v4"),
    summary: (profile) => (profile.prompt_sources.secret.exists ? "Secret-scope turns only" : "Not created"),
  },
  {
    section: "memory",
    label: "Memory",
    icon: icon("M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8Z"),
    summary: (profile) => {
      const chars = totalMemoryChars(profile);
      return chars === 0 ? "Empty" : formatChars(chars);
    },
  },
  {
    section: "skills",
    label: "Skills",
    icon: icon("M4 19.5A2.5 2.5 0 0 1 6.5 17H20V3H6.5A2.5 2.5 0 0 0 4 5.5v14zM4 19.5A2.5 2.5 0 0 0 6.5 22H20v-5"),
    summary: (profile) => `${profile.prompt_sources.skills} available`,
  },
];

/** Sidebar context for the Agent screen, laid out like the session list. */
export function AgentSectionNav({ agentId, authToken, section, onSelectSection }: AgentSectionNavProps) {
  const profile = useAgentQuery(authToken, ["profile", agentId], () =>
    fetchAgentProfile(agentId, authToken),
  ).data;

  return (
    <nav className="agent-section-nav" aria-label="Agent sections">
      <h2 className="section-title">SECTIONS</h2>
      <ul className="context-list">
        {ITEMS.map((item) => {
          const current = item.section === section;
          return (
            <li key={item.section}>
              <button
                type="button"
                className={`context-row ${current ? "active" : ""}`}
                aria-current={current ? "page" : undefined}
                onClick={() => onSelectSection(item.section)}
              >
                <span className="context-icon">{item.icon}</span>
                <span className="context-text">
                  <span className="context-title">{item.label}</span>
                  {profile && <span className="context-sub">{item.summary(profile)}</span>}
                </span>
              </button>
            </li>
          );
        })}
      </ul>
    </nav>
  );
}
