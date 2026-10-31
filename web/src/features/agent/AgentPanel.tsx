import type { AgentSection } from "../../app/router";
import type { SessionEntry } from "../../shared/api/types";
import { AgentOverview } from "./AgentOverview";
import { MemorySection } from "./MemorySection";
import { PromptFileView } from "./PromptFileView";
import { SkillsSection } from "./SkillsSection";

export interface AgentPanelProps {
  agentId: string;
  authToken: string;
  section: AgentSection;
  avatarUrl: string | undefined;
  sessions: SessionEntry[];
  onSelectSection: (section: AgentSection) => void;
  /** Opens a sleep run in the Schedule screen. */
  onOpenSleepRun: (runId: string) => void;
}

const HEADINGS: Record<AgentSection, { title: string; description?: string }> = {
  overview: { title: "Overview" },
  soul: {
    title: "Soul",
    description: "Identity and persona. The agent's SOUL.md wins; otherwise the global SOUL.md is used.",
  },
  rules: {
    title: "Rules",
    description: "AGENTS.md rules. The global file and the agent's file are both injected, in this order.",
  },
  secret: {
    title: "Secret",
    description: "Instructions injected only in secret-scope turns.",
  },
  memory: {
    title: "Memory",
    description: "Long-term memory distilled by sleep batches and injected into every turn.",
  },
  skills: {
    title: "Skills",
    description: "Skills available to every agent, listed in the prompt as a catalog.",
  },
};

/** Agent screen: what the selected agent is, remembers, and can do. */
export function AgentPanel(props: AgentPanelProps) {
  const heading = HEADINGS[props.section];
  return (
    <div className="screen-panel">
      <header className="screen-header">
        <h1 className="panel-title">{heading.title}</h1>
        {heading.description && <p className="panel-description">{heading.description}</p>}
      </header>
      <div className="agent-panel-body">
        <AgentSectionBody {...props} />
      </div>
    </div>
  );
}

function AgentSectionBody({
  agentId,
  authToken,
  section,
  avatarUrl,
  sessions,
  onSelectSection,
  onOpenSleepRun,
}: AgentPanelProps) {
  switch (section) {
    case "overview":
      return (
        <AgentOverview
          agentId={agentId}
          authToken={authToken}
          avatarUrl={avatarUrl}
          sessions={sessions}
          onSelectSection={onSelectSection}
          onOpenSleepRun={onOpenSleepRun}
        />
      );
    case "soul":
    case "rules":
    case "secret":
      return (
        <PromptFileView key={`${agentId}:${section}`} agentId={agentId} authToken={authToken} kind={section} />
      );
    case "memory":
      return <MemorySection agentId={agentId} authToken={authToken} onOpenSleepRun={onOpenSleepRun} />;
    case "skills":
      return <SkillsSection authToken={authToken} />;
  }
}
