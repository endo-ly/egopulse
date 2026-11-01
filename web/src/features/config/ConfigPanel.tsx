import { useEffect, useRef, useState, type ReactNode } from "react";

import {
  patchConfig,
  type AgentConfigEntry,
  type BotEntry,
  type ChatChannelEntry,
  type LlmSource,
  type McpDefinitions,
  type ProviderConfigEntry,
  type RuntimeConfig,
  type SecretStatus,
} from "../../shared/api/config";
import { setQueryData } from "../../shared/hooks/useServerState";
import { Button } from "../../shared/ui/Button";
import { QueryState } from "../../shared/ui/QueryState";
import { StatusDot } from "../../shared/ui/StatusDot";
import { Toast } from "../../shared/ui/Toast";
import { useRuntimeStatus } from "../metrics/metricsQuery";
import { formatCount } from "../metrics/metricsFormat";
import { agentTarget, generalTarget, providerTarget, pulseTarget, sleepTarget, type EditTarget } from "./configForms";
import { configQueryKey, useConfigMcp, useConfigYaml, useRuntimeConfig } from "./configQuery";
import { SECTION_INFO, type ConfigSection } from "./configSections";
import { EditForm, type FormValues } from "./EditForm";
import { summarizeMcp, type McpStatusSummary } from "./mcpStatus";

export interface ConfigPanelProps {
  authToken: string;
  section: ConfigSection;
  onOpenSchedule: (agentId: string) => void;
}

/** Restart-required groups say so once instead of marking every field. */
const RESTART_NOTE = "Changing these needs a restart: edit the YAML file, then restart EgoPulse.";

/**
 * The runtime config, section by section. Values are shown as they take
 * effect (inherited models resolved, secrets as their state only), and the
 * settings that apply without a restart can be edited in place.
 */
export function ConfigPanel({ authToken, section, onOpenSchedule }: ConfigPanelProps) {
  const config = useRuntimeConfig(authToken);
  const status = useRuntimeStatus(authToken);
  const [editing, setEditing] = useState<string | null>(null);
  const [toast, setToast] = useState<string | null>(null);
  const info = SECTION_INFO[section];

  useEffect(() => setEditing(null), [section]);

  // Keeps the current config on screen so an open form survives the reload.
  const reload = () => config.refresh();

  const save = async (edit: EditTarget, changes: FormValues) => {
    const current = config.data;
    if (!current) return;
    const updated = await patchConfig(authToken, edit.target, current.fingerprint, changes);
    setQueryData(configQueryKey(authToken), updated);
    setEditing(null);
    setToast("Saved. New turns use the updated config.");
  };

  return (
    <div className="screen-panel config-panel">
      <header className="screen-header config-header">
        <div>
          <h1 className="panel-title">{info.label}</h1>
          <p className="panel-description">{info.description}</p>
        </div>
        {config.data && (
          <div className="config-source" title={`Revision ${config.data.revision}`}>
            <code>{config.data.config_path}</code>
            <span>Revision {config.data.revision}</span>
          </div>
        )}
      </header>
      <div className="agent-panel-body config-body">
        <QueryState state={config}>
          {(data) => (
            <SectionView
              section={section}
              config={data}
              authToken={authToken}
              mcp={summarizeMcp(status.data)}
              editing={editing}
              onEdit={setEditing}
              onSave={save}
              onReload={reload}
              onOpenSchedule={onOpenSchedule}
            />
          )}
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

interface SectionViewProps {
  section: ConfigSection;
  config: RuntimeConfig;
  authToken: string;
  mcp: McpStatusSummary | null;
  editing: string | null;
  onEdit: (key: string | null) => void;
  onSave: (edit: EditTarget, changes: FormValues) => Promise<void>;
  onReload: () => void;
  onOpenSchedule: (agentId: string) => void;
}

function SectionView(props: SectionViewProps) {
  const { section, config } = props;
  const editable = (key: string, edit: EditTarget | null, title: ReactNode, view: ReactNode, extra?: ReactNode) => (
    <EditableCard key={key} cardKey={key} edit={edit} title={title} props={props} extra={extra}>
      {view}
    </EditableCard>
  );

  switch (section) {
    case "general":
      return (
        <div className="config-stack">
          {editable("general", generalTarget(config), "Defaults", <GeneralView config={config} />)}
          <Card title="Process" note={RESTART_NOTE}>
            <Rows>
              <Row label="Log level" value={config.general.log_level} env={config.env_overrides.log_level} />
              <Row label="State directory" value={<code>{config.general.state_root}</code>} />
            </Rows>
          </Card>
        </div>
      );
    case "agents":
      return (
        <div className="config-stack">
          {config.agents.map((agent) =>
            editable(
              `agent:${agent.id}`,
              agentTarget(config, agent.id),
              <span className="card-title-with-id">
                {agent.label}
                <code>{agent.id}</code>
                {agent.is_default && <span className="agent-default-tag">default</span>}
              </span>,
              <AgentView agent={agent} config={config} />,
            ),
          )}
        </div>
      );
    case "providers":
      return (
        <div className="config-stack">
          {config.providers.map((provider) =>
            editable(
              `provider:${provider.id}`,
              providerTarget(config, provider.id),
              <span className="card-title-with-id">
                {provider.label}
                <code>{provider.id}</code>
                {provider.id === config.general.default_provider && <span className="agent-default-tag">default</span>}
              </span>,
              <ProviderView provider={provider} config={config} />,
            ),
          )}
        </div>
      );
    case "channels":
      return <ChannelsView config={config} />;
    case "sleep":
      return <div className="config-stack">{editable("sleep", sleepTarget(config), "Sleep batch", <SleepView config={config} />)}</div>;
    case "pulse":
      return (
        <div className="config-stack">
          {editable(
            "pulse",
            pulseTarget(config),
            "Scheduler",
            <Rows>
              <Row label="Check every" value={config.pulse.tick_interval} isDefault={config.pulse.tick_interval === config.pulse.default_tick_interval} />
            </Rows>,
            <p className="config-card-foot">
              Whether Pulse runs and what each intention does is set per agent in its PULSE.yaml.{" "}
              <button type="button" className="text-link" onClick={() => props.onOpenSchedule(config.general.default_agent)}>
                Open Schedule
              </button>
            </p>,
          )}
        </div>
      );
    case "webhooks":
      return <WebhooksView config={config} />;
    case "backup":
      return (
        <Card title="Database backup" note={RESTART_NOTE}>
          <Rows>
            <Row label="Scheduled backups" value={<OnOff on={config.backup.enabled} />} />
            <Row label="Every" value={`${config.backup.interval_days} ${config.backup.interval_days === 1 ? "day" : "days"} at ${config.backup.time}`} />
            <Row label="Keep" value={`${config.backup.max_generations} backups`} />
          </Rows>
        </Card>
      );
    case "web-fetch":
      return <WebFetchView config={config} />;
    case "mcp":
      return <McpView authToken={props.authToken} mcp={props.mcp} />;
    case "yaml":
      return <YamlView authToken={props.authToken} revision={config.revision} />;
  }
}

// ---------------------------------------------------------------------------
// Building blocks
// ---------------------------------------------------------------------------

function Card({ title, note, action, children }: { title: ReactNode; note?: string; action?: ReactNode; children: ReactNode }) {
  return (
    <section className="config-card">
      <header className="config-card-head">
        <h2>{title}</h2>
        {action}
      </header>
      {note && <p className="config-card-note">{note}</p>}
      {children}
    </section>
  );
}

function EditableCard({
  cardKey,
  edit,
  title,
  props,
  extra,
  children,
}: {
  cardKey: string;
  edit: EditTarget | null;
  title: ReactNode;
  props: SectionViewProps;
  extra?: ReactNode;
  children: ReactNode;
}) {
  const isEditing = props.editing === cardKey;
  const ref = useRef<HTMLElement>(null);
  useEffect(() => {
    if (isEditing) ref.current?.querySelector<HTMLElement>("input, select, button")?.focus();
  }, [isEditing]);

  return (
    <section ref={ref} className={`config-card ${isEditing ? "editing" : ""}`}>
      <header className="config-card-head">
        <h2>{title}</h2>
        {edit && !isEditing && (
          <Button
            variant="secondary"
            type="button"
            onClick={() => props.onEdit(cardKey)}
            disabled={props.editing !== null}
            title={props.editing !== null ? "Finish the other edit first" : undefined}
          >
            Edit
          </Button>
        )}
      </header>
      {isEditing && edit ? (
        <EditForm
          formId={cardKey}
          specs={edit.specs}
          values={edit.values}
          onSave={(changes) => props.onSave(edit, changes)}
          onCancel={() => props.onEdit(null)}
          onReload={props.onReload}
        />
      ) : (
        <>
          {children}
          {extra}
        </>
      )}
    </section>
  );
}

function Rows({ children }: { children: ReactNode }) {
  return <dl className="config-rows">{children}</dl>;
}

function Row({
  label,
  value,
  sub,
  env,
  isDefault,
}: {
  label: string;
  value: ReactNode;
  /** A second line: where an inherited value comes from, etc. */
  sub?: ReactNode;
  env?: string;
  isDefault?: boolean;
}) {
  return (
    <div className="config-row">
      <dt>{label}</dt>
      <dd>
        <span className="config-value">
          {value}
          {isDefault && <span className="value-tag">default</span>}
          {env && (
            <span className="value-tag env" title={`Set by the ${env} environment variable, which overrides the YAML file`}>
              env {env}
            </span>
          )}
        </span>
        {sub && <span className="config-sub">{sub}</span>}
      </dd>
    </div>
  );
}

function OnOff({ on }: { on: boolean }) {
  return <span className={`on-off ${on ? "on" : "off"}`}>{on ? "On" : "Off"}</span>;
}

function Secret({
  secret,
  optional = false,
  envOverride,
}: {
  secret: SecretStatus;
  optional?: boolean;
  /** Environment variable that replaces the YAML value at startup. */
  envOverride?: string;
}) {
  if (envOverride) {
    return (
      <span className="secret">
        <StatusDot tone="success" />
        Set <span className="secret-source">from the <code>{envOverride}</code> environment variable</span>
      </span>
    );
  }
  if (!secret.configured) {
    return (
      <span className={`secret ${optional ? "" : "missing"}`}>
        <StatusDot tone={optional ? "idle" : "warning"} />
        {optional ? "Not needed" : "Not set"}
      </span>
    );
  }
  const source =
    secret.kind === "env" ? (
      <>
        from <code>{secret.ref}</code>
      </>
    ) : secret.kind === "exec" ? (
      "from a command"
    ) : (
      "written in the YAML file"
    );
  return (
    <span className="secret">
      <StatusDot tone="success" />
      Set <span className="secret-source">{source}</span>
    </span>
  );
}

function None({ children = "None" }: { children?: ReactNode }) {
  return <span className="muted">{children}</span>;
}

function Chips({ items, empty = "None" }: { items: string[]; empty?: string }) {
  if (items.length === 0) return <None>{empty}</None>;
  return (
    <span className="chips">
      {items.map((item) => (
        <span key={item} className="chip">
          {item}
        </span>
      ))}
    </span>
  );
}

// ---------------------------------------------------------------------------
// Sections
// ---------------------------------------------------------------------------

function GeneralView({ config }: { config: RuntimeConfig }) {
  const g = config.general;
  const d = g.defaults;
  const defaultAgent = config.agents.find((a) => a.id === g.default_agent);
  const provider = config.providers.find((p) => p.id === g.default_provider);
  return (
    <>
      <Rows>
        <Row label="Default agent" value={defaultAgent?.label ?? g.default_agent} />
        <Row label="Default provider" value={provider?.label ?? g.default_provider} />
        <Row
          label="Default model"
          value={g.default_model ?? provider?.default_model ?? "—"}
          sub={g.default_model ? undefined : "The default provider's own default"}
        />
        <Row label="Timezone" value={g.timezone} isDefault={g.timezone === d.timezone} />
      </Rows>
      <h3 className="config-subhead">History and compaction</h3>
      <Rows>
        <Row label="History messages" value={g.max_history_messages} isDefault={g.max_history_messages === d.max_history_messages} />
        <Row label="Context window" value={`${formatCount(g.default_context_window_tokens)} tokens`} sub="For models without their own setting" isDefault={g.default_context_window_tokens === d.default_context_window_tokens} />
        <Row
          label="Compaction"
          value={`At ${Math.round(g.compaction_threshold_ratio * 100)}% of the window, down to ${Math.round(g.compaction_target_ratio * 100)}%`}
          isDefault={g.compaction_threshold_ratio === d.compaction_threshold_ratio && g.compaction_target_ratio === d.compaction_target_ratio}
        />
        <Row label="Keep recent" value={`${g.compact_keep_recent} messages`} isDefault={g.compact_keep_recent === d.compact_keep_recent} />
        <Row label="Compaction timeout" value={`${g.compaction_timeout_secs}s`} isDefault={g.compaction_timeout_secs === d.compaction_timeout_secs} />
      </Rows>
    </>
  );
}

const SOURCE_TEXT: Record<LlmSource, string> = {
  profile: "set by the channel profile",
  agent: "set for this agent",
  global_default: "inherited from the global default",
  provider_default: "the provider's default model",
};

function AgentView({ agent, config }: { agent: AgentConfigEntry; config: RuntimeConfig }) {
  const effective = agent.effective;
  const inherited: string[] = [];
  if (effective && effective.provider_source !== "agent") inherited.push(`provider ${SOURCE_TEXT[effective.provider_source]}`);
  if (effective && effective.model_source !== "agent") inherited.push(`model ${SOURCE_TEXT[effective.model_source]}`);
  const providerLabel = (id: string) => config.providers.find((p) => p.id === id)?.label ?? id;
  return (
    <Rows>
      <Row
        label="Model"
        value={effective ? <><span className="muted-strong">{providerLabel(effective.provider)}</span> <code>{effective.model}</code></> : <span className="danger">Provider not found</span>}
        sub={inherited.length > 0 ? capitalize(inherited.join(", ")) : undefined}
      />
      <Row label="Reasoning effort" value={agent.reasoning_effort ?? <None>Provider default</None>} />
      <Row label="Discord bot" value={agent.discord_bot ? <code>{agent.discord_bot}</code> : <None />} />
      <Row label="Telegram bot" value={agent.telegram_bot ? <code>{agent.telegram_bot}</code> : <None />} />
      {agent.profiles.length > 0 && (
        <Row
          label="Channel profiles"
          value={
            <ul className="profile-list">
              {agent.profiles.map((profile) => (
                <li key={profile.channel}>
                  <span className="chip">{profile.channel}</span>
                  {profile.effective ? (
                    <>
                      {providerLabel(profile.effective.provider)} <code>{profile.effective.model}</code>
                    </>
                  ) : (
                    <span className="danger">Provider not found</span>
                  )}
                </li>
              ))}
            </ul>
          }
          sub="Edit in the YAML file"
        />
      )}
    </Rows>
  );
}

function capitalize(text: string): string {
  return text.charAt(0).toUpperCase() + text.slice(1);
}

function ProviderView({ provider, config }: { provider: ProviderConfigEntry; config: RuntimeConfig }) {
  const agentLabel = (id: string) => config.agents.find((a) => a.id === id)?.label ?? id;
  return (
    <Rows>
      <Row label="Base URL" value={<code className="wrap">{provider.base_url}</code>} />
      <Row label="API key" value={<Secret secret={provider.api_key} optional={provider.api_key_optional} />} />
      <Row label="Default model" value={<code>{provider.default_model}</code>} />
      <Row
        label="Models"
        value={
          <ul className="model-list">
            {provider.models.map((model) => (
              <li key={model.name}>
                <code>{model.name}</code>
                {model.context_window_tokens !== null && <span className="model-meta">{formatCount(model.context_window_tokens)} context</span>}
                {model.has_instructions && <span className="model-meta">custom instructions</span>}
              </li>
            ))}
          </ul>
        }
      />
      {provider.header_names.length > 0 && <Row label="Extra headers" value={<Chips items={provider.header_names} />} sub="Values are hidden" />}
      <Row label="Used by" value={<Chips items={provider.used_by.map(agentLabel)} empty="No agent" />} />
    </Rows>
  );
}

function BotsTable({ bots, agentLabel }: { bots: BotEntry[]; agentLabel: (id: string) => string }) {
  if (bots.length === 0) return <None>No bots</None>;
  return (
    <table className="metrics-table">
      <thead>
        <tr>
          <th scope="col">Bot</th>
          <th scope="col">Token</th>
          <th scope="col">Agents</th>
        </tr>
      </thead>
      <tbody>
        {bots.map((bot) => (
          <tr key={bot.id}>
            <td>
              <code>{bot.id}</code>
            </td>
            <td>
              <Secret secret={bot.token} />
            </td>
            <td>
              <Chips items={bot.agents.map(agentLabel)} empty="No agent" />
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function ChatChannelsTable({ channels, agentLabel, idLabel }: { channels: ChatChannelEntry[]; agentLabel: (id: string) => string; idLabel: string }) {
  if (channels.length === 0) return <None>No channels allowed yet</None>;
  return (
    <table className="metrics-table">
      <thead>
        <tr>
          <th scope="col">{idLabel}</th>
          <th scope="col">Agents</th>
          <th scope="col">Behavior</th>
        </tr>
      </thead>
      <tbody>
        {channels.map((channel) => {
          const traits = [
            channel.require_mention ? "Mention required" : null,
            channel.multi_agent ? "Multi-agent" : null,
            channel.secret ? "Secret" : null,
            channel.tool_progress ? "Tool progress" : null,
          ].filter((t): t is string => t !== null);
          return (
            <tr key={channel.id}>
              <td>
                <code>{channel.id}</code>
              </td>
              <td>
                <Chips items={channel.agents.map(agentLabel)} />
              </td>
              <td>
                <Chips items={traits} empty="Default" />
              </td>
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}

function ChannelsView({ config }: { config: RuntimeConfig }) {
  const { web, discord, telegram, voice } = config.channels;
  const env = config.env_overrides;
  const agentLabel = (id: string) => config.agents.find((a) => a.id === id)?.label ?? id;
  return (
    <div className="config-stack">
      <p className="config-section-note">{RESTART_NOTE} Allowed Discord and Telegram channels apply without a restart once the file is saved.</p>
      <Card title={<span className="card-title-with-id">Web <OnOff on={web.enabled} /></span>}>
        <Rows>
          <Row label="Listens on" value={<code>{`${web.host}:${web.port}`}</code>} env={env["channels.web.host"] ?? env["channels.web.port"]} />
          <Row label="Auth token" value={<Secret secret={web.auth_token} envOverride={env["channels.web.auth_token"]} />} />
          <Row label="Allowed origins" value={<Chips items={web.allowed_origins} empty="Same origin only" />} env={env["channels.web.allowed_origins"]} />
        </Rows>
      </Card>
      <Card title={<span className="card-title-with-id">Discord <OnOff on={discord.enabled} /></span>}>
        <h3 className="config-subhead">Bots</h3>
        <BotsTable bots={discord.bots} agentLabel={agentLabel} />
        <h3 className="config-subhead">Allowed channels</h3>
        <ChatChannelsTable channels={discord.channels} agentLabel={agentLabel} idLabel="Channel ID" />
      </Card>
      <Card title={<span className="card-title-with-id">Telegram <OnOff on={telegram.enabled} /></span>}>
        <h3 className="config-subhead">Bots</h3>
        <BotsTable bots={telegram.bots} agentLabel={agentLabel} />
        <h3 className="config-subhead">Allowed chats</h3>
        <ChatChannelsTable channels={telegram.channels} agentLabel={agentLabel} idLabel="Chat ID" />
      </Card>
      <Card title={<span className="card-title-with-id">Voice <OnOff on={voice.enabled} /></span>}>
        <Rows>
          <Row label="Auth token" value={<Secret secret={voice.auth_token} optional={!voice.enabled} />} />
          <Row label="Default surface" value={<code>{voice.default_surface}</code>} />
          <Row label="Default session" value={<code>{voice.default_session}</code>} />
          <Row label="Allowed surfaces" value={<Chips items={voice.allowed_surfaces} empty="Any surface" />} />
        </Rows>
      </Card>
    </div>
  );
}

function SleepView({ config }: { config: RuntimeConfig }) {
  const s = config.sleep;
  const agentLabel = (id: string) => config.agents.find((a) => a.id === id)?.label ?? id;
  const inherited = [s.provider === null ? "provider" : null, s.model === null ? "model" : null].filter(Boolean);
  return (
    <Rows>
      <Row
        label="Schedule"
        value={s.enabled ? (s.schedule ? `Daily at ${s.schedule}` : <span className="warning">On, but no time is set</span>) : "Manual only"}
        sub={s.enabled && s.schedule ? config.general.timezone : undefined}
      />
      <Row label="Agents" value={s.agents === null ? "All agents" : <Chips items={s.agents.map(agentLabel)} empty="No agents" />} />
      <Row
        label="Model"
        value={s.effective ? <><span className="muted-strong">{config.providers.find((p) => p.id === s.effective?.provider)?.label ?? s.effective.provider}</span> <code>{s.effective.model}</code></> : <span className="danger">Provider not found</span>}
        sub={inherited.length > 0 ? `${capitalize(inherited.join(" and "))} inherited from the global default` : undefined}
      />
      <Row label="Reasoning effort" value={s.reasoning_effort ?? <None>Provider default</None>} />
      <Row label="Retry" value={s.retry_max_attempts === 0 ? "No retries" : `Up to ${s.retry_max_attempts} times, ${s.retry_interval_minutes} min apart`} />
    </Rows>
  );
}

function WebhooksView({ config }: { config: RuntimeConfig }) {
  const agentLabel = (id: string) => config.agents.find((a) => a.id === id)?.label ?? id;
  return (
    <Card title="Receivers" note="Receivers apply without a restart once the YAML file is saved.">
      {config.webhooks.length === 0 ? (
        <None>No receivers. Add one under webhooks.receivers in the YAML file.</None>
      ) : (
        <table className="metrics-table">
          <thead>
            <tr>
              <th scope="col">Receiver</th>
              <th scope="col">Delivers to</th>
              <th scope="col">Agent</th>
              <th scope="col">Token</th>
            </tr>
          </thead>
          <tbody>
            {config.webhooks.map((hook) => (
              <tr key={hook.id}>
                <td>
                  <code>{hook.id}</code>
                  <span className="cell-sub">
                    <code>POST /api/webhooks/{hook.id}</code>
                  </span>
                </td>
                <td>
                  {hook.channel}
                  {hook.thread && <span className="cell-sub"><code>{hook.thread}</code></span>}
                </td>
                <td>{hook.agent ? agentLabel(hook.agent) : <None>Default agent</None>}</td>
                <td>
                  <Secret secret={hook.token} />
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </Card>
  );
}

function bytes(n: number): string {
  if (n >= 1024 * 1024) return `${(n / 1024 / 1024).toFixed(n % (1024 * 1024) === 0 ? 0 : 1)} MiB`;
  if (n >= 1024) return `${Math.round(n / 1024)} KiB`;
  return `${n} B`;
}

function WebFetchView({ config }: { config: RuntimeConfig }) {
  const f = config.web_fetch;
  return (
    <Card title="web_fetch tool" note={RESTART_NOTE}>
      <Rows>
        <Row label="Schemes" value={<Chips items={f.allowed_schemes} />} />
        <Row label="Private addresses" value={f.allow_private_ips ? <span className="warning">Allowed</span> : "Blocked"} sub="Loopback and private network IPs" />
        <Row label="Allowed hosts" value={<Chips items={f.allowlist} empty="Any host" />} />
        <Row label="Blocked hosts" value={<Chips items={f.denylist} empty="None" />} />
        <Row label="Timeout" value={`${f.timeout_secs}s`} />
        <Row label="Read limit" value={bytes(f.max_fetch_bytes)} sub={`Returns at most ${bytes(f.max_output_bytes)} of text`} />
        <Row
          label="Prompt-injection scan"
          value={f.content_validation.enabled ? (f.content_validation.strict_mode ? "On, strict" : "On") : <span className="warning">Off</span>}
          sub={f.content_validation.enabled ? `First ${bytes(f.content_validation.max_scan_bytes)} of each page` : undefined}
        />
      </Rows>
    </Card>
  );
}

function McpView({ authToken, mcp }: { authToken: string; mcp: McpStatusSummary | null }) {
  const definitions = useConfigMcp(authToken);
  return <QueryState state={definitions}>{(data) => <McpDefinitionsView definitions={data} mcp={mcp} />}</QueryState>;
}

function McpDefinitionsView({ definitions, mcp }: { definitions: McpDefinitions; mcp: McpStatusSummary | null }) {
  const { servers, files, error } = definitions;
  return (
    <div className="config-stack">
      {error && <div className="run-error">{error}</div>}
      {servers.length === 0 ? (
        <Card title="Servers">
          <None>No MCP servers. Define them in one of the files below.</None>
        </Card>
      ) : (
        servers.map((server) => {
          const state = mcp?.byName[server.name];
          return (
            <Card
              key={server.name}
              title={
                <span className="card-title-with-id">
                  {server.name}
                  <span className="chip">{server.transport === "stdio" ? "stdio" : "HTTP"}</span>
                </span>
              }
              action={
                state && (
                  <span className="inline-status">
                    <StatusDot tone={state.connected ? "live" : "error"} />
                    {state.connected ? `Connected, ${state.tools} ${state.tools === 1 ? "tool" : "tools"}` : "Not connected"}
                  </span>
                )
              }
            >
              <Rows>
                {state && !state.connected && <Row label="Error" value={<span className="danger">{state.error}</span>} />}
                {server.command && <Row label="Command" value={<code>{server.command}</code>} sub="Arguments are hidden" />}
                {server.endpoint && <Row label="Endpoint" value={<code className="wrap">{server.endpoint}</code>} />}
                {server.env_keys.length > 0 && <Row label="Environment" value={<Chips items={server.env_keys} />} sub="Values are hidden" />}
                {server.header_keys.length > 0 && <Row label="Headers" value={<Chips items={server.header_keys} />} sub="Values are hidden" />}
                <Row label="Request timeout" value={`${server.request_timeout_secs}s`} />
              </Rows>
            </Card>
          );
        })
      )}
      <Card title="Definition files" note="Later files override servers of the same name. Changes apply on the next start.">
        <ol className="file-list">
          {files.map((file) => (
            <li key={file}>
              <code>{file}</code>
            </li>
          ))}
        </ol>
      </Card>
    </div>
  );
}

function YamlView({ authToken, revision }: { authToken: string; revision: number }) {
  const yaml = useConfigYaml(authToken, revision);
  const [copied, setCopied] = useState(false);
  return (
    <QueryState state={yaml}>
      {(data) => {
        const lines = data.content.replace(/\n$/, "").split("\n");
        return (
          <section className="config-card yaml-card">
            <header className="config-card-head">
              <h2>
                <code>{data.path}</code>
              </h2>
              <Button
                variant="secondary"
                type="button"
                onClick={() => {
                  void navigator.clipboard?.writeText(data.content).then(() => {
                    setCopied(true);
                    setTimeout(() => setCopied(false), 1500);
                  });
                }}
              >
                {copied ? "Copied" : "Copy"}
              </Button>
            </header>
            <p className="config-card-note">Secret values and commands are replaced with ********. Edit the file itself to change anything not editable here.</p>
            <pre className="yaml-view">
              {lines.map((line, index) => (
                <span key={index} className="yaml-line">
                  <span className="yaml-ln" aria-hidden="true">
                    {index + 1}
                  </span>
                  <span className="yaml-text">{line || " "}</span>
                </span>
              ))}
            </pre>
          </section>
        );
      }}
    </QueryState>
  );
}
