import type { AgentPulse, PulseIntention, SessionEntry } from "../../shared/api/types";
import { channelLabel } from "../../shared/lib/format";
import { EmptyState } from "../../shared/ui/EmptyState";
import { PULSE_ICON } from "../../shared/ui/icons";
import { QueryState } from "../../shared/ui/QueryState";
import { DocumentCard } from "../agent/DocumentCard";
import { MetaLine, OutcomeLabel } from "./parts";
import { describeDelivery, describeNextDue, describeSchedule } from "./scheduleFormat";

export interface PulseSectionProps {
  pulse: { data: AgentPulse | undefined; loading: boolean; error: Error | null };
  sessions: readonly SessionEntry[];
  /** The selected intention; null selects the first. */
  intentionId: string | null;
  onSelectIntention: (intentionId: string) => void;
  onOpenSession: (sessionKey: string) => void;
}

/** Pulse: the agent-level settings and each intention's definition. */
export function PulseSection({ pulse, sessions, intentionId, onSelectIntention, onOpenSession }: PulseSectionProps) {
  return (
    <QueryState state={pulse}>
      {(data) => (
        <div className="agent-page">
          {data.error !== null ? (
            <div className="schedule-alert" role="alert">
              <div className="schedule-alert-item">
                <p>PULSE.yaml could not be read, so no intention fires until it is fixed.</p>
                <pre className="row-error">{data.error}</pre>
              </div>
            </div>
          ) : (
            <dl className="agent-kv">
              <div className="agent-kv-row">
                <dt>Status</dt>
                <dd>
                  <OutcomeLabel tone={data.enabled ? "success" : "idle"}>{data.enabled ? "On" : "Off"}</OutcomeLabel>
                </dd>
              </div>
              <div className="agent-kv-row">
                <dt>Timezone</dt>
                <dd>{data.timezone}</dd>
              </div>
              <div className="agent-kv-row">
                <dt>Default destination</dt>
                <dd>
                  {data.default_delivery
                    ? `${channelLabel(data.default_delivery.channel)} · ${data.default_delivery.external_chat_id}`
                    : "The latest Discord or Telegram conversation"}
                </dd>
              </div>
            </dl>
          )}
          <Intentions
            pulse={data}
            sessions={sessions}
            intentionId={intentionId}
            onSelectIntention={onSelectIntention}
            onOpenSession={onOpenSession}
          />
        </div>
      )}
    </QueryState>
  );
}

function Intentions({
  pulse,
  sessions,
  intentionId,
  onSelectIntention,
  onOpenSession,
}: Omit<PulseSectionProps, "pulse"> & { pulse: AgentPulse }) {
  if (pulse.error !== null) return null;
  if (pulse.intentions.length === 0) {
    return (
      <EmptyState
        icon={PULSE_ICON}
        title="No intentions"
        description="Ask the agent in chat to check in on something at a set time."
      />
    );
  }
  // A selection that vanished from PULSE.yaml falls back to the first intention.
  const active = pulse.intentions.find((intention) => intention.id === intentionId) ?? pulse.intentions[0];
  return (
    <div className="skills-layout">
      <ul className="skills-list" aria-label="Intentions">
        {pulse.intentions.map((intention) => {
          const on = pulse.enabled === true && intention.enabled;
          const current = intention.id === active.id;
          return (
            <li key={intention.id}>
              <button
                type="button"
                className={`skill-row ${current ? "active" : ""}`}
                data-inactive={!on || undefined}
                aria-current={current ? "true" : undefined}
                onClick={() => onSelectIntention(intention.id)}
              >
                <span className="skill-row-name">
                  {intention.id}
                  {!on && <span className="skill-row-source">Off</span>}
                </span>
                <span className="skill-row-description">
                  {describeSchedule(intention.schedule)}
                  {on && ` · next ${describeNextDue(intention.next)}`}
                </span>
              </button>
            </li>
          );
        })}
      </ul>
      <IntentionDetail
        key={active.id}
        intention={active}
        pulse={pulse}
        sessions={sessions}
        onOpenSession={onOpenSession}
      />
    </div>
  );
}

const ORIGIN_NOTES = {
  intention: null,
  default: "the agent's default destination",
  latest: "the agent's latest Discord or Telegram conversation",
} as const;

function IntentionDetail({
  intention,
  pulse,
  sessions,
  onOpenSession,
}: {
  intention: PulseIntention;
  pulse: AgentPulse;
  sessions: readonly SessionEntry[];
  onOpenSession: (sessionKey: string) => void;
}) {
  const on = pulse.enabled === true && intention.enabled;
  const delivery = describeDelivery(intention, pulse.default_delivery, sessions);
  const { sessionKey } = delivery;
  const origin = ORIGIN_NOTES[delivery.origin];

  return (
    <section className="skill-detail" aria-label={intention.id}>
      <header className="run-detail-head">
        <h2 className="run-detail-title">{intention.id}</h2>
        <MetaLine
          items={[
            <OutcomeLabel key="state" tone={on ? "success" : "idle"}>
              {on ? "Active" : pulse.enabled === false ? "Off (Pulse is off)" : "Off"}
            </OutcomeLabel>,
          ]}
        />
      </header>
      <dl className="agent-kv">
        <div className="agent-kv-row">
          <dt>When</dt>
          <dd>
            {describeSchedule(intention.schedule)} ({pulse.timezone})
          </dd>
        </div>
        <div className="agent-kv-row">
          <dt>Next</dt>
          <dd>{on ? describeNextDue(intention.next) : "Does not fire while off"}</dd>
        </div>
        <div className="agent-kv-row">
          <dt>Sends to</dt>
          <dd>
            {delivery.label}
            {origin && <span className="field-note"> — {origin}</span>}
            {sessionKey !== null ? (
              <>
                {" "}
                <button type="button" className="text-link" onClick={() => onOpenSession(sessionKey)}>
                  Open in Chat
                </button>
              </>
            ) : (
              <p className="field-warning">
                {delivery.origin === "latest"
                  ? "There is no Discord or Telegram conversation to send to yet, so firings are skipped."
                  : "No conversation matches this destination, so firings are skipped."}
              </p>
            )}
          </dd>
        </div>
      </dl>
      <DocumentCard eyebrow="Attention" path={`PULSE.yaml · ${intention.id}`} content={intention.attention} />
    </section>
  );
}
