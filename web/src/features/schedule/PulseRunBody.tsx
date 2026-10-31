import { fetchPulseRunDetail } from "../../shared/api/pulse";
import type { AgentPulse, PulseRunDetail, SessionEntry } from "../../shared/api/types";
import { channelLabel, formatDuration } from "../../shared/lib/format";
import { sessionTitle } from "../../shared/lib/session";
import { MarkdownRenderer } from "../../shared/ui/MarkdownRenderer";
import { QueryState } from "../../shared/ui/QueryState";
import { MetaLine, OutcomeLabel } from "./parts";
import { pulseTone } from "./runStatus";
import { formatRunTime, intentionTitle, pulseOutcomeLabel } from "./scheduleFormat";
import { useScheduleQuery } from "./scheduleQuery";

export interface PulseRunBodyProps {
  runId: string;
  authToken: string;
  pulse: AgentPulse | undefined;
  sessions: readonly SessionEntry[];
  onOpenSession: (sessionKey: string) => void;
}

/** One Pulse firing: what came of it, where it went, and what it attended to. */
export function PulseRunBody({ runId, authToken, pulse, sessions, onOpenSession }: PulseRunBodyProps) {
  const state = useScheduleQuery(authToken, ["pulse-run", runId], () => fetchPulseRunDetail(runId, authToken));
  return (
    <QueryState state={state}>
      {(run) => (
        <PulseRunContent run={run} pulse={pulse} sessions={sessions} onOpenSession={onOpenSession} />
      )}
    </QueryState>
  );
}

function PulseRunContent({
  run,
  pulse,
  sessions,
  onOpenSession,
}: {
  run: PulseRunDetail;
  pulse: AgentPulse | undefined;
  sessions: readonly SessionEntry[];
  onOpenSession: (sessionKey: string) => void;
}) {
  const intention = pulse?.intentions.find((candidate) => candidate.id === run.intention_id);
  const { delivery } = run;
  const session = delivery && sessions.find((candidate) => candidate.session_key === delivery.session_key);
  return (
    <>
      <header className="run-detail-head">
        <h2 className="run-detail-title">{intention ? intentionTitle(intention) : run.intention_id}</h2>
        <MetaLine
          items={[
            <OutcomeLabel key="outcome" tone={pulseTone(run.status)}>
              {pulseOutcomeLabel(run)}
            </OutcomeLabel>,
            <span key="at">{formatRunTime(run.started_at)}</span>,
            run.finished_at && (
              <span key="took">took {formatDuration(Date.parse(run.finished_at) - Date.parse(run.started_at))}</span>
            ),
          ]}
        />
      </header>

      <PulseOutcome run={run} />

      {delivery && (
        <p className="run-detail-line">
          Sent to {channelLabel(delivery.channel)}
          {session && ` · ${sessionTitle(session)}`}{" "}
          <button type="button" className="text-link" onClick={() => onOpenSession(delivery.session_key)}>
            Open in Chat
          </button>
        </p>
      )}

      <section className="run-detail-section" aria-label="Attention">
        <h3 className="run-detail-section-title">Attention</h3>
        {run.attention_snapshot === null ? (
          <p className="field-note">Not recorded for this run.</p>
        ) : (
          <p className="field-text">{run.attention_snapshot}</p>
        )}
      </section>
    </>
  );
}

function PulseOutcome({ run }: { run: PulseRunDetail }) {
  switch (run.status) {
    case "running":
      return <p className="field-note">Still running.</p>;
    case "failed":
      return <pre className="row-error">{run.error_message ?? "Unknown error"}</pre>;
    case "skipped":
      return (
        <p className="field-warning">
          The notification could not be delivered: {run.error_message ?? "no reason recorded"}.
        </p>
      );
    case "success":
      return run.output_kind === "notify" && run.output_text ? (
        <div className="message-card">
          <MarkdownRenderer content={run.output_text} />
        </div>
      ) : (
        <p className="field-note">Nothing worth reporting, so no message was sent.</p>
      );
  }
}
