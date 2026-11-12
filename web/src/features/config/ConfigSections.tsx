import { useState, type ComponentType } from "react";

import type {
  AgentConfigEntry,
  BotEntry,
  ChatChannelEntry,
  CompactionModelOverride,
  LlmSource,
  McpDefinitions,
  ProviderConfigEntry,
  RuntimeConfig,
} from "../../shared/api/config";
import { Button } from "../../shared/ui/Button";
import { QueryState } from "../../shared/ui/QueryState";
import { StatusDot } from "../../shared/ui/StatusDot";
import { formatCount } from "../metrics/metricsFormat";
import {
  Card,
  Chips,
  EditableCard,
  None,
  OnOff,
  Row,
  Rows,
  Secret,
  capitalize,
  type ConfigSectionContext,
} from "./ConfigCard";
import { agentTarget, generalTarget, providerTarget, pulseTarget, sleepTarget } from "./configForms";
import { useConfigMcp, useConfigYaml } from "./configQuery";
import type { ConfigSection } from "./configSections";
import type { McpStatusSummary } from "./mcpStatus";

/** Restart-required groups say so once instead of marking every field. */
const RESTART_NOTE = "Changing these needs a restart: edit the YAML file, then restart EgoPulse.";

interface SectionProps {
  ctx: ConfigSectionContext;
}

// ---------------------------------------------------------------------------
// General
// ---------------------------------------------------------------------------

function GeneralSection({ ctx }: SectionProps) {
  const { config } = ctx;
  return (
    <div className="config-stack">
      <EditableCard cardKey="general" edit={generalTarget(config)} title="Defaults" ctx={ctx}>
        <GeneralView config={config} />
      </EditableCard>
      <Card title="Process" note={RESTART_NOTE}>
        <Rows>
          <Row label="Log level" value={config.general.log_level} env={config.env_overrides.log_level} />
          <Row label="State directory" value={<code>{config.general.state_root}</code>} />
        </Rows>
      </Card>
    </div>
  );
}

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
      <h4 className="config-subhead">History and compaction</h4>
      <Rows>
        <Row label="History messages" value={g.max_history_messages} isDefault={g.max_history_messages === d.max_history_messages} />
        <Row
          label="Context window"
          value={`${formatCount(g.default_context_window_tokens)} tokens`}
          sub="For models without their own setting"
          isDefault={g.default_context_window_tokens === d.default_context_window_tokens}
        />
        <Row
          label="Compaction"
          value={g.compaction_enabled ? "Automatic" : "Off"}
          isDefault={g.compaction_enabled === d.compaction_enabled}
        />
        <Row
          label="Reserve tokens"
          value={`${formatCount(g.compaction_reserve_tokens)} tokens`}
          isDefault={g.compaction_reserve_tokens === d.compaction_reserve_tokens}
        />
        <Row
          label="Keep recent tokens"
          value={`${formatCount(g.compaction_keep_recent_tokens)} tokens`}
          isDefault={g.compaction_keep_recent_tokens === d.compaction_keep_recent_tokens}
        />
      </Rows>
      {g.compaction_model_overrides.length > 0 && (
        <>
          <h4 className="config-subhead">Compaction overrides</h4>
          <Rows>
            {g.compaction_model_overrides.map((entry) => (
              <Row
                key={entry.model}
                label={entry.model}
                value={compactionOverrideValue(entry)}
              />
            ))}
          </Rows>
        </>
      )}
    </>
  );
}

function compactionOverrideValue(entry: CompactionModelOverride): string {
  const parts = [
    entry.reserve_tokens === null ? null : `reserve ${formatCount(entry.reserve_tokens)}`,
    entry.keep_recent_tokens === null ? null : `keep ${formatCount(entry.keep_recent_tokens)}`,
  ].filter((part): part is string => part !== null);
  return parts.length > 0 ? `${parts.join(" · ")} tokens` : "Inherits the global values";
}

// ---------------------------------------------------------------------------
// Agents
// ---------------------------------------------------------------------------

function AgentsSection({ ctx }: SectionProps) {
  const { config } = ctx;
  return (
    <div className="config-stack">
      {config.agents.map((agent) => (
        <EditableCard
          key={agent.id}
          cardKey={`agent:${agent.id}`}
          edit={agentTarget(config, agent.id)}
          title={
            <span className="card-title-with-id">
              {agent.label}
              <code>{agent.id}</code>
              {agent.is_default && <span className="agent-default-tag">default</span>}
            </span>
          }
          ctx={ctx}
        >
          <AgentView agent={agent} config={config} />
        </EditableCard>
      ))}
    </div>
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
        value={
          effective ? (
            <>
              <span className="muted-strong">{providerLabel(effective.provider)}</span> <code>{effective.model}</code>
            </>
          ) : (
            <span className="danger">Provider not found</span>
          )
        }
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

// ---------------------------------------------------------------------------
// Providers
// ---------------------------------------------------------------------------

function ProvidersSection({ ctx }: SectionProps) {
  const { config } = ctx;
  return (
    <div className="config-stack">
      {config.providers.map((provider) => (
        <EditableCard
          key={provider.id}
          cardKey={`provider:${provider.id}`}
          edit={providerTarget(config, provider.id)}
          title={
            <span className="card-title-with-id">
              {provider.label}
              <code>{provider.id}</code>
              {provider.id === config.general.default_provider && <span className="agent-default-tag">default</span>}
            </span>
          }
          ctx={ctx}
        >
          <ProviderView provider={provider} config={config} />
        </EditableCard>
      ))}
    </div>
  );
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

// ---------------------------------------------------------------------------
// Channels
// ---------------------------------------------------------------------------

function BotsTable({ bots, agentLabel }: { bots: BotEntry[]; agentLabel: (id: string) => string }) {
  if (bots.length === 0) return <None>No bots</None>;
  return (
    <div className="config-table-scroll">
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
    </div>
  );
}

function ChatChannelsTable({
  channels,
  agentLabel,
  idLabel,
}: {
  channels: ChatChannelEntry[];
  agentLabel: (id: string) => string;
  idLabel: string;
}) {
  if (channels.length === 0) return <None>No channels allowed yet</None>;
  return (
    <div className="config-table-scroll">
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
    </div>
  );
}

function ChannelsSection({ ctx }: SectionProps) {
  const { config } = ctx;
  const { web, discord, telegram, voice } = config.channels;
  const env = config.env_overrides;
  const agentLabel = (id: string) => config.agents.find((a) => a.id === id)?.label ?? id;
  return (
    <div className="config-stack">
      <p className="config-section-note">
        {RESTART_NOTE} Allowed Discord and Telegram channels apply without a restart once the file is saved.
      </p>
      <Card title={<span className="card-title-with-id">Web <OnOff on={web.enabled} /></span>}>
        <Rows>
          <Row label="Listens on" value={<code>{`${web.host}:${web.port}`}</code>} env={env["channels.web.host"] ?? env["channels.web.port"]} />
          <Row label="Auth token" value={<Secret secret={web.auth_token} envOverride={env["channels.web.auth_token"]} />} />
          <Row label="Allowed origins" value={<Chips items={web.allowed_origins} empty="Same origin only" />} env={env["channels.web.allowed_origins"]} />
        </Rows>
      </Card>
      <Card title={<span className="card-title-with-id">Discord <OnOff on={discord.enabled} /></span>}>
        <h4 className="config-subhead">Bots</h4>
        <BotsTable bots={discord.bots} agentLabel={agentLabel} />
        <h4 className="config-subhead">Allowed channels</h4>
        <ChatChannelsTable channels={discord.channels} agentLabel={agentLabel} idLabel="Channel ID" />
      </Card>
      <Card title={<span className="card-title-with-id">Telegram <OnOff on={telegram.enabled} /></span>}>
        <h4 className="config-subhead">Bots</h4>
        <BotsTable bots={telegram.bots} agentLabel={agentLabel} />
        <h4 className="config-subhead">Allowed chats</h4>
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

// ---------------------------------------------------------------------------
// Sleep and Pulse
// ---------------------------------------------------------------------------

function SleepSection({ ctx }: SectionProps) {
  return (
    <div className="config-stack">
      <EditableCard cardKey="sleep" edit={sleepTarget(ctx.config)} title="Sleep batch" ctx={ctx}>
        <SleepView config={ctx.config} />
      </EditableCard>
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
        value={
          s.effective ? (
            <>
              <span className="muted-strong">{config.providers.find((p) => p.id === s.effective?.provider)?.label ?? s.effective.provider}</span>{" "}
              <code>{s.effective.model}</code>
            </>
          ) : (
            <span className="danger">Provider not found</span>
          )
        }
        sub={inherited.length > 0 ? `${capitalize(inherited.join(" and "))} inherited from the global default` : undefined}
      />
      <Row label="Reasoning effort" value={s.reasoning_effort ?? <None>Provider default</None>} />
      <Row
        label="Retry"
        value={
          s.retry_max_attempts <= 1
            ? "No retries"
            : `Up to ${s.retry_max_attempts} attempts, ${s.retry_interval_minutes} min apart`
        }
      />
    </Rows>
  );
}

function PulseSection({ ctx }: SectionProps) {
  const { config } = ctx;
  return (
    <div className="config-stack">
      <EditableCard
        cardKey="pulse"
        edit={pulseTarget(config)}
        title="Scheduler"
        ctx={ctx}
        extra={
          <p className="config-card-foot">
            Whether Pulse runs and what each intention does is set per agent in its PULSE.yaml.{" "}
            <button type="button" className="text-link" onClick={() => ctx.onOpenSchedule(config.general.default_agent)}>
              Open Schedule
            </button>
          </p>
        }
      >
        <Rows>
          <Row label="Check every" value={config.pulse.tick_interval} isDefault={config.pulse.tick_interval === config.pulse.default_tick_interval} />
        </Rows>
      </EditableCard>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Webhooks, backup, web fetch
// ---------------------------------------------------------------------------

function WebhooksSection({ ctx }: SectionProps) {
  const { config } = ctx;
  const agentLabel = (id: string) => config.agents.find((a) => a.id === id)?.label ?? id;
  return (
    <Card title="Receivers" note="Receivers apply without a restart once the YAML file is saved.">
      {config.webhooks.length === 0 ? (
        <None>No receivers. Add one under webhooks.receivers in the YAML file.</None>
      ) : (
        <div className="config-table-scroll">
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
                    {hook.thread && (
                      <span className="cell-sub">
                        <code>{hook.thread}</code>
                      </span>
                    )}
                  </td>
                  <td>{hook.agent ? agentLabel(hook.agent) : <None>Default agent</None>}</td>
                  <td>
                    <Secret secret={hook.token} />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </Card>
  );
}

function BackupSection({ ctx }: SectionProps) {
  const { backup } = ctx.config;
  return (
    <Card title="Database backup" note={RESTART_NOTE}>
      <Rows>
        <Row label="Scheduled backups" value={<OnOff on={backup.enabled} />} />
        <Row label="Every" value={`${backup.interval_days} ${backup.interval_days === 1 ? "day" : "days"} at ${backup.time}`} />
        <Row label="Keep" value={`${backup.max_generations} backups`} />
      </Rows>
    </Card>
  );
}

function bytes(n: number): string {
  if (n >= 1024 * 1024) return `${(n / 1024 / 1024).toFixed(n % (1024 * 1024) === 0 ? 0 : 1)} MiB`;
  if (n >= 1024) return `${Math.round(n / 1024)} KiB`;
  return `${n} B`;
}

function WebFetchSection({ ctx }: SectionProps) {
  const f = ctx.config.web_fetch;
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

// ---------------------------------------------------------------------------
// MCP
// ---------------------------------------------------------------------------

function McpSection({ ctx }: SectionProps) {
  const definitions = useConfigMcp(ctx.authToken);
  return <QueryState state={definitions}>{(data) => <McpDefinitionsView definitions={data} mcp={ctx.mcp} />}</QueryState>;
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

// ---------------------------------------------------------------------------
// YAML file
// ---------------------------------------------------------------------------

function YamlSection({ ctx }: SectionProps) {
  const [open, setOpen] = useState(false);
  return (
    <Card
      title={<code className="wrap">{ctx.config.config_path}</code>}
      note="Secret values and commands are replaced with ********. Edit the file itself to change anything not editable here."
      action={
        <Button variant="secondary" type="button" onClick={() => setOpen((value) => !value)}>
          {open ? "Hide file" : "Show file"}
        </Button>
      }
    >
      {open ? (
        <YamlView authToken={ctx.authToken} revision={ctx.config.revision} />
      ) : (
        <p className="config-card-foot">Open the file to check the raw YAML on disk.</p>
      )}
    </Card>
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
          <>
            <div className="yaml-toolbar">
              <Button
                variant="secondary"
                type="button"
                onClick={() => {
                  void navigator.clipboard
                    ?.writeText(data.content)
                    .then(() => {
                      setCopied(true);
                      setTimeout(() => setCopied(false), 1500);
                    })
                    .catch(() => {
                      // Clipboard access can be denied; the text stays selectable.
                    });
                }}
              >
                {copied ? "Copied" : "Copy"}
              </Button>
            </div>
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
          </>
        );
      }}
    </QueryState>
  );
}

// ---------------------------------------------------------------------------
// Section bodies
// ---------------------------------------------------------------------------

const SECTION_VIEWS: Record<ConfigSection, ComponentType<SectionProps>> = {
  general: GeneralSection,
  agents: AgentsSection,
  providers: ProvidersSection,
  channels: ChannelsSection,
  sleep: SleepSection,
  pulse: PulseSection,
  webhooks: WebhooksSection,
  backup: BackupSection,
  "web-fetch": WebFetchSection,
  mcp: McpSection,
  yaml: YamlSection,
};

/** The content below one section heading on the one-page Config screen. */
export function ConfigSectionBody({ section, ctx }: { section: ConfigSection; ctx: ConfigSectionContext }) {
  const Section = SECTION_VIEWS[section];
  return <Section ctx={ctx} />;
}
