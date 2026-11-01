import { useEffect, useRef, useState, type ReactNode, type RefObject } from "react";

import { patchConfig, type RuntimeConfig } from "../../shared/api/config";
import { setQueryData } from "../../shared/hooks/useServerState";
import { QueryState } from "../../shared/ui/QueryState";
import { StatusDot } from "../../shared/ui/StatusDot";
import { Toast } from "../../shared/ui/Toast";
import { useRuntimeStatus } from "../metrics/metricsQuery";
import type { ConfigSectionContext } from "./ConfigCard";
import { configQueryKey, useRuntimeConfig } from "./configQuery";
import {
  CONFIG_SECTIONS,
  SECTION_INFO,
  configSectionElementId,
  pickActiveSection,
  scrollToConfigSection,
  sectionSummary,
  type ConfigSection,
} from "./configSections";
import { ConfigSectionBody } from "./ConfigSections";
import type { EditTarget } from "./configForms";
import type { FormValues } from "./EditForm";
import { summarizeMcp, type McpStatusSummary } from "./mcpStatus";

export interface ConfigPanelProps {
  authToken: string;
  /** The section the URL points at; it is scrolled to when it changes. */
  section: ConfigSection;
  /** The section currently under the sticky bar, reported by the page. */
  activeSection: ConfigSection | null;
  onSelectSection: (section: ConfigSection) => void;
  onActiveSectionChange: (section: ConfigSection | null) => void;
  onOpenSchedule: (agentId: string) => void;
}

/** Where the active-section line sits below the top of the scrolling body. */
const ACTIVE_SECTION_OFFSET = 64;

/**
 * Every runtime setting on one page. Sections are stacked in a fixed order and
 * the sticky bar and sidebar both act as a table of contents: they jump to a
 * section and follow the reader as the page scrolls.
 */
export function ConfigPanel(props: ConfigPanelProps) {
  const config = useRuntimeConfig(props.authToken);
  const status = useRuntimeStatus(props.authToken);
  const [editing, setEditing] = useState<string | null>(null);
  const [toast, setToast] = useState<string | null>(null);
  const bodyRef = useRef<HTMLDivElement>(null);
  const scrolledTo = useRef<ConfigSection | null>(null);
  const active = props.activeSection ?? props.section;

  const reload = () => config.refresh();

  const save = async (edit: EditTarget, changes: FormValues) => {
    const current = config.data;
    if (!current) return;
    const updated = await patchConfig(props.authToken, edit.target, current.fingerprint, changes);
    setQueryData(configQueryKey(props.authToken), updated);
    setEditing(null);
    setToast("Saved. New turns use the updated config.");
  };

  // Scroll to the route's section once the content exists. Polls after that
  // keep the reader where they are.
  useEffect(() => {
    if (!config.data) {
      scrolledTo.current = null;
      return;
    }
    if (scrolledTo.current === props.section) return;
    scrolledTo.current = props.section;
    // The first section is already at the top of a fresh page; scrolling to
    // it would only tuck it under the sticky bar.
    if (props.section === CONFIG_SECTIONS[0] && (bodyRef.current?.scrollTop ?? 0) === 0) return;
    scrollToConfigSection(props.section);
  }, [config.data, props.section]);

  // A card can disappear while its form is open (e.g. the YAML file was
  // edited elsewhere); do not leave every other Edit button disabled.
  useEffect(() => {
    if (editing === null || !config.data) return;
    const exists =
      editing === "general" ||
      editing === "sleep" ||
      editing === "pulse" ||
      config.data.agents.some((agent) => `agent:${agent.id}` === editing) ||
      config.data.providers.some((provider) => `provider:${provider.id}` === editing);
    if (!exists) setEditing(null);
  }, [config.data, editing]);

  useConfigScrollSpy(bodyRef, config.data?.revision, props.onActiveSectionChange);

  const mcp = summarizeMcp(status.data);

  return (
    <div className="screen-panel config-panel">
      <header className="screen-header config-header">
        <div>
          <h1 className="panel-title">Config</h1>
          <p className="panel-description">Every runtime setting in one place. Values are shown as they take effect.</p>
        </div>
        {config.data && (
          <div className="config-source" title={`Revision ${config.data.revision}`}>
            <code>{config.data.config_path}</code>
            <span>Revision {config.data.revision}</span>
          </div>
        )}
      </header>
      <div className="agent-panel-body config-body" ref={bodyRef}>
        <QueryState state={config}>
          {(data) => {
            const ctx: ConfigSectionContext = {
              config: data,
              authToken: props.authToken,
              mcp,
              editing,
              onEdit: setEditing,
              onSave: save,
              onReload: reload,
              onOpenSchedule: props.onOpenSchedule,
            };
            return (
              <div className="config-page">
                <ConfigToc config={data} mcp={mcp} active={active} onSelect={props.onSelectSection} />
                {CONFIG_SECTIONS.map((section) => (
                  <ConfigSectionFrame key={section} section={section} config={data} mcp={mcp}>
                    <ConfigSectionBody section={section} ctx={ctx} />
                  </ConfigSectionFrame>
                ))}
              </div>
            );
          }}
        </QueryState>
      </div>
      {toast && (
        <div className="toast-container">
          <Toast tone="success" message={toast} onClose={() => setToast(null)} />
        </div>
      )}
    </div>
  );
}

function ConfigToc({
  config,
  mcp,
  active,
  onSelect,
}: {
  config: RuntimeConfig;
  mcp: McpStatusSummary | null;
  active: ConfigSection;
  onSelect: (section: ConfigSection) => void;
}) {
  const barRef = useRef<HTMLElement>(null);

  // Keep the active chip visible when the bar scrolls horizontally.
  useEffect(() => {
    const bar = barRef.current;
    if (!bar || bar.scrollWidth <= bar.clientWidth) return;
    const chip = bar.querySelector<HTMLElement>(`[data-section="${active}"]`);
    if (!chip) return;
    const barRect = bar.getBoundingClientRect();
    const chipRect = chip.getBoundingClientRect();
    if (chipRect.left < barRect.left) {
      bar.scrollBy({ left: chipRect.left - barRect.left - 8, behavior: "smooth" });
    } else if (chipRect.right > barRect.right) {
      bar.scrollBy({ left: chipRect.right - barRect.right + 8, behavior: "smooth" });
    }
  }, [active]);

  return (
    <nav ref={barRef} className="config-toc" aria-label="Config sections on this page">
      {CONFIG_SECTIONS.map((section) => {
        const summary = sectionSummary(section, config, mcp);
        const label = SECTION_INFO[section].label;
        return (
          <button
            key={section}
            type="button"
            data-section={section}
            className={`config-toc-item ${section === active ? "active" : ""}`}
            aria-current={section === active ? "location" : undefined}
            title={summary ? `${label}: ${summary.text}` : label}
            onClick={() => {
              scrollToConfigSection(section);
              onSelect(section);
            }}
          >
            {summary?.tone && <StatusDot tone={summary.tone === "danger" ? "error" : "warning"} />}
            <span>{label}</span>
          </button>
        );
      })}
    </nav>
  );
}

function ConfigSectionFrame({
  section,
  config,
  mcp,
  children,
}: {
  section: ConfigSection;
  config: RuntimeConfig;
  mcp: McpStatusSummary | null;
  children: ReactNode;
}) {
  const info = SECTION_INFO[section];
  const summary = sectionSummary(section, config, mcp);
  const headingId = `${configSectionElementId(section)}-title`;
  return (
    <section id={configSectionElementId(section)} className="config-section" aria-labelledby={headingId}>
      <header className="config-section-head">
        <div className="config-section-heading">
          <h2 id={headingId}>{info.label}</h2>
          <p>{info.description}</p>
        </div>
        {summary && (
          <span className={`config-section-summary ${summary.tone ? `tone-${summary.tone}` : ""}`}>{summary.text}</span>
        )}
      </header>
      {children}
    </section>
  );
}

/** Reports the section whose heading last crossed the sticky bar. */
function useConfigScrollSpy(
  bodyRef: RefObject<HTMLDivElement>,
  revision: number | undefined,
  onActiveChange: (section: ConfigSection | null) => void,
) {
  const last = useRef<ConfigSection | null>(null);
  useEffect(() => {
    const body = bodyRef.current;
    if (!body || revision === undefined) return;
    const update = () => {
      const positions = CONFIG_SECTIONS.flatMap((id) => {
        const element = document.getElementById(configSectionElementId(id));
        return element ? [{ id, top: element.getBoundingClientRect().top }] : [];
      });
      if (positions.length === 0) return;
      const atBottom =
        body.scrollHeight > body.clientHeight &&
        body.scrollTop + body.clientHeight >= body.scrollHeight - 2;
      const next = pickActiveSection(positions, body.getBoundingClientRect().top + ACTIVE_SECTION_OFFSET, atBottom);
      if (last.current === next) return;
      last.current = next;
      onActiveChange(next);
    };
    body.addEventListener("scroll", update, { passive: true });
    update();
    return () => body.removeEventListener("scroll", update);
  }, [bodyRef, revision, onActiveChange]);
}
