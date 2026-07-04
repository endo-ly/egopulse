import { StatusDot } from "../../shared/ui/StatusDot";
import type { AgentEntry } from "../../shared/api/types";

export interface AgentsSectionProps {
  agents: AgentEntry[];
  selectedAgent: string;
  onSelectAgent: (id: string) => void;
}

export function AgentsSection({
  agents,
  selectedAgent,
  onSelectAgent,
}: AgentsSectionProps) {
  return (
    <div className="agents-section">
      <h2 className="section-title">AGENTS</h2>
      <ul className="agents-list">
        {agents.map((agent) => (
          <li key={agent.id}>
            <button
              type="button"
              className={`agent-row ${selectedAgent === agent.id ? "active" : ""}`}
              aria-current={selectedAgent === agent.id ? "true" : undefined}
              onClick={() => onSelectAgent(agent.id)}
            >
              <StatusDot tone={agent.active ? "live" : "idle"} />
              <span className="agent-label">{agent.label}</span>
              {agent.is_default && (
                <span className="agent-default-tag">default</span>
              )}
            </button>
          </li>
        ))}
      </ul>
    </div>
  );
}
