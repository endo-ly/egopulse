import type { ReactNode } from "react";

import type { AgentSection } from "../../app/router";
import { fetchAgentProfile, fetchMemoryHistory } from "../../shared/api/agents";
import { fetchSleepRuns } from "../../shared/api/sleep";
import type { AgentProfile, MemoryChange, SessionEntry, SleepRun } from "../../shared/api/types";
import { countLineChanges } from "../../shared/lib/diff";
import { channelLabel, formatRelativeTime } from "../../shared/lib/format";
import { StatusDot } from "../../shared/ui/StatusDot";
import { statusLabel, statusTone, triggerLabel } from "../schedule/sleepStatus";
import { useAgentQuery } from "./agentQuery";
import { formatChars, llmSourceKey, totalMemoryChars } from "./profileFormat";
import { QueryState } from "./QueryState";

export interface AgentOverviewProps {
  agentId: string;
  authToken: string;
  avatarUrl: string | undefined;
  /** All sessions; the overview keeps the ones of this agent. */
  sessions: SessionEntry[];
  onSelectSection: (section: AgentSection) => void;
  onOpenRun: (runId: string) => void;
}

const RECENT_ACTIVITY = 5;

/** The agent at a glance: identity, usage, model, prompt makeup, recent activity. */
export function AgentOverview(props: AgentOverviewProps) {
  const { agentId, authToken } = props;
  const profileState = useAgentQuery(authToken, ["profile", agentId], () =>
    fetchAgentProfile(agentId, authToken),
  );
  const historyState = useAgentQuery(authToken, ["memory-history", agentId], () =>
    fetchMemoryHistory(agentId, authToken),
  );
  const runsState = useAgentQuery(authToken, ["sleep-runs", agentId], () =>
    fetchSleepRuns(agentId, RECENT_ACTIVITY, authToken),
  );

  return (
    <QueryState state={profileState}>
      {(profile) => (
        <div className="agent-overview">
          <Hero profile={profile} avatarUrl={props.avatarUrl} />
          <Stats
            profile={profile}
            sessions={props.sessions.filter((session) => session.agent_id === agentId)}
            lastMemoryChange={historyState.data?.[0]}
          />
          <div className="agent-overview-grid">
            <ModelCard profile={profile} />
            <PromptCard profile={profile} onSelectSection={props.onSelectSection} />
          </div>
          <ActivityCard
            runs={runsState.data ?? []}
            changes={historyState.data ?? []}
            onOpenRun={props.onOpenRun}
            onSelectSection={props.onSelectSection}
          />
        </div>
      )}
    </QueryState>
  );
}

function Card({ title, children }: { title: string; children: ReactNode }) {
  const id = `agent-card-${title.toLowerCase().replace(/\s+/g, "-")}`;
  return (
    <section className="agent-card" aria-labelledby={id}>
      <h2 id={id} className="run-section-title">
        {title}
      </h2>
      {children}
    </section>
  );
}

function Hero({ profile, avatarUrl }: { profile: AgentProfile; avatarUrl: string | undefined }) {
  const primary = profile.llm[0];
  const bindings = [
    ["Discord", profile.bindings.discord_bot],
    ["Telegram", profile.bindings.telegram_bot],
  ].filter((binding): binding is [string, string] => binding[1] !== null);

  return (
    <section className="agent-hero" aria-label={profile.label}>
      <span className="agent-hero-avatar" aria-hidden="true">
        {avatarUrl ? <img src={avatarUrl} alt="" /> : profile.label.slice(0, 1).toUpperCase()}
      </span>
      <div className="agent-hero-text">
        <div className="agent-hero-name">
          <h2>{profile.label}</h2>
          {profile.is_default && <span className="agent-default-tag">default</span>}
        </div>
        <p className="agent-hero-meta">
          <code>{profile.id}</code>
          {primary && (
            <span>
              {primary.provider} / {primary.model}
            </span>
          )}
          {profile.reasoning_effort && <span>effort {profile.reasoning_effort}</span>}
        </p>
        <ul className="agent-hero-bindings">
          {bindings.length === 0 ? (
            <li className="agent-hero-binding muted">No bot bindings</li>
          ) : (
            bindings.map(([channel, bot]) => (
              <li key={channel} className="agent-hero-binding">
                {channel} <code>{bot}</code>
              </li>
            ))
          )}
        </ul>
      </div>
    </section>
  );
}

function Stats({
  profile,
  sessions,
  lastMemoryChange,
}: {
  profile: AgentProfile;
  sessions: SessionEntry[];
  lastMemoryChange: MemoryChange | undefined;
}) {
  const active = sessions.filter((session) => !session.archived);
  const counts = new Map<string, number>();
  for (const session of active) counts.set(session.channel, (counts.get(session.channel) ?? 0) + 1);
  const byChannel = [...counts].sort((a, b) => b[1] - a[1]);
  const lastActive = sessions.reduce((latest, session) => Math.max(latest, session.last_message_time), 0);
  const memoryChars = totalMemoryChars(profile);

  return (
    <dl className="agent-stats">
      <Stat
        label="Sessions"
        value={String(active.length)}
        hint={byChannel.map(([channel, count]) => `${channelLabel(channel)} ${count}`).join(" · ") || "None yet"}
      />
      <Stat
        label="Last active"
        value={lastActive > 0 ? formatRelativeTime(new Date(lastActive).toISOString()) : "—"}
        hint={lastActive > 0 ? new Date(lastActive).toLocaleString() : "No conversations yet"}
      />
      <Stat
        label="Memory"
        value={memoryChars > 0 ? formatChars(memoryChars) : "Empty"}
        hint={
          lastMemoryChange
            ? `${lastMemoryChange.file} updated ${formatRelativeTime(lastMemoryChange.created_at)}`
            : "No sleep changes yet"
        }
      />
      <Stat label="Skills" value={String(profile.prompt_sources.skills)} hint="Shared by all agents" />
    </dl>
  );
}

function Stat({ label, value, hint }: { label: string; value: string; hint: string }) {
  return (
    <div className="agent-stat">
      <dt>{label}</dt>
      <dd className="agent-stat-value">{value}</dd>
      <dd className="agent-stat-hint" title={hint}>
        {hint}
      </dd>
    </div>
  );
}

function ModelCard({ profile }: { profile: AgentProfile }) {
  // The channel-less row covers every channel without a profile.
  const defaultLabel = profile.llm.length > 1 ? "Other channels" : "All channels";
  return (
    <Card title="Model">
      <table className="agent-model-table">
        <thead>
          <tr>
            <th scope="col">Channel</th>
            <th scope="col">Model</th>
            <th scope="col">Set by</th>
          </tr>
        </thead>
        <tbody>
          {profile.llm.map((row) => {
            const providerKey = llmSourceKey(row.provider_source, profile.id, row, "provider");
            const modelKey = llmSourceKey(row.model_source, profile.id, row, "model");
            return (
              <tr key={row.channel ?? ""}>
                <th scope="row">{row.channel ?? defaultLabel}</th>
                <td>
                  <span className="agent-model-name">{row.model}</span>
                  <span className="agent-model-provider">{row.provider}</span>
                </td>
                <td>
                  <code className="agent-config-key">{modelKey}</code>
                  {providerKey !== modelKey && (
                    <code className="agent-config-key" title="Provider">
                      {providerKey}
                    </code>
                  )}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
      <p className="agent-card-note">
        Reasoning effort <strong>{profile.reasoning_effort ?? "default"}</strong>
      </p>
    </Card>
  );
}

interface PromptPart {
  section: AgentSection;
  label: string;
  chars: number;
  detail: string;
}

function PromptCard({
  profile,
  onSelectSection,
}: {
  profile: AgentProfile;
  onSelectSection: (section: AgentSection) => void;
}) {
  const { soul, rules, secret, skills } = profile.prompt_sources;
  const parts: PromptPart[] = [
    { section: "soul", label: "Soul", chars: soul?.chars ?? 0, detail: soul?.path ?? "Not set" },
    {
      section: "rules",
      label: "Rules",
      chars: rules.reduce((sum, source) => sum + source.chars, 0),
      detail: rules.length > 0 ? rules.map((source) => source.path).join(" + ") : "None",
    },
    { section: "memory", label: "Memory", chars: totalMemoryChars(profile), detail: "episodic + semantic + prospective" },
  ];
  const total = parts.reduce((sum, part) => sum + part.chars, 0);

  return (
    <Card title="System prompt">
      <div className="prompt-bar" role="img" aria-label={`${formatChars(total)} of agent-specific context`}>
        {parts
          .filter((part) => part.chars > 0)
          .map((part) => (
            <span key={part.section} data-part={part.section} style={{ flexGrow: part.chars }} />
          ))}
      </div>
      <ul className="prompt-parts">
        {parts.map((part) => (
          <li key={part.section}>
            <button type="button" className="prompt-part" onClick={() => onSelectSection(part.section)}>
              <span className="prompt-part-swatch" data-part={part.section} aria-hidden="true" />
              <span className="prompt-part-label">{part.label}</span>
              <span className="prompt-part-detail">{part.detail}</span>
              <span className="prompt-part-size">{part.chars > 0 ? formatChars(part.chars) : "—"}</span>
            </button>
          </li>
        ))}
      </ul>
      <p className="agent-card-note">
        {secret.exists ? (
          <>
            <button type="button" className="text-link" onClick={() => onSelectSection("secret")}>
              SECRET.md
            </button>{" "}
            is added in secret-scope turns.{" "}
          </>
        ) : null}
        The catalog of {skills} skills is added to every turn.
      </p>
    </Card>
  );
}

type Activity =
  | { kind: "sleep"; at: string; run: SleepRun }
  | { kind: "memory"; at: string; change: MemoryChange };

function ActivityCard({
  runs,
  changes,
  onOpenRun,
  onSelectSection,
}: {
  runs: SleepRun[];
  changes: MemoryChange[];
  onOpenRun: (runId: string) => void;
  onSelectSection: (section: AgentSection) => void;
}) {
  const items: Activity[] = [
    ...runs.map((run): Activity => ({ kind: "sleep", at: run.started_at, run })),
    ...changes.map((change): Activity => ({ kind: "memory", at: change.created_at, change })),
  ]
    .sort((a, b) => b.at.localeCompare(a.at))
    .slice(0, RECENT_ACTIVITY);

  return (
    <Card title="Recent activity">
      {items.length === 0 ? (
        <p className="agent-card-note">No sleep runs or memory changes yet.</p>
      ) : (
        <ul className="agent-activity">
          {items.map((item) =>
            item.kind === "sleep" ? (
              <li key={`sleep:${item.run.id}`}>
                <button type="button" className="agent-activity-row" onClick={() => onOpenRun(item.run.id)}>
                  <StatusDot tone={statusTone(item.run.status)} />
                  <span className="agent-activity-title">Sleep · {triggerLabel(item.run.trigger)}</span>
                  <span className="agent-activity-detail">
                    {statusLabel(item.run.status)} · {item.run.session_count}{" "}
                    {item.run.session_count === 1 ? "session" : "sessions"}
                  </span>
                  <time dateTime={item.at}>{formatRelativeTime(item.at)}</time>
                </button>
              </li>
            ) : (
              <li key={`memory:${item.change.run_id}:${item.change.file}`}>
                <button
                  type="button"
                  className="agent-activity-row"
                  onClick={() => onSelectSection("memory")}
                >
                  <StatusDot tone="idle" />
                  <span className="agent-activity-title">Memory · {item.change.file}</span>
                  <MemoryChangeCount change={item.change} />
                  <time dateTime={item.at}>{formatRelativeTime(item.at)}</time>
                </button>
              </li>
            ),
          )}
        </ul>
      )}
    </Card>
  );
}

function MemoryChangeCount({ change }: { change: MemoryChange }) {
  const counts = countLineChanges(change.content_before, change.content_after);
  return (
    <span className="agent-activity-detail tab-switch-count">
      <span className="count-add">+{counts.add}</span> <span className="count-remove">−{counts.remove}</span>
    </span>
  );
}
