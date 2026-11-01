import { useRuntimeStatus } from "../metrics/metricsQuery";
import { CONFIG_SECTIONS, SECTION_INFO, sectionSummary, type ConfigSection } from "./configSections";
import { useRuntimeConfig } from "./configQuery";
import { summarizeMcp } from "./mcpStatus";

export interface ConfigSectionNavProps {
  authToken: string;
  section: ConfigSection;
  onSelectSection: (section: ConfigSection) => void;
}

/** Sidebar context for Config: every section with its current state. */
export function ConfigSectionNav({ authToken, section, onSelectSection }: ConfigSectionNavProps) {
  const config = useRuntimeConfig(authToken);
  const status = useRuntimeStatus(authToken);
  const mcp = summarizeMcp(status.data);

  return (
    <nav className="agent-section-nav" aria-label="Config sections">
      <h2 className="section-title">SECTIONS</h2>
      <ul className="context-list">
        {CONFIG_SECTIONS.map((id) => {
          const current = id === section;
          const summary = config.data ? sectionSummary(id, config.data, mcp) : null;
          return (
            <li key={id} className={id === "yaml" ? "context-divider-before" : undefined}>
              <button
                type="button"
                className={`context-row ${current ? "active" : ""}`}
                aria-current={current ? "page" : undefined}
                onClick={() => onSelectSection(id)}
              >
                <span className="context-text">
                  <span className="context-title">{SECTION_INFO[id].label}</span>
                  {summary && (
                    <span className={`context-sub ${summary.tone ? `tone-${summary.tone}` : ""}`}>{summary.text}</span>
                  )}
                </span>
              </button>
            </li>
          );
        })}
      </ul>
    </nav>
  );
}
