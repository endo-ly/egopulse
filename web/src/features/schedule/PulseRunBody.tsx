import { fetchPulseRunDetail } from "../../shared/api/pulse";
import type { PulseRunDetail, SessionEntry } from "../../shared/api/types";
import { channelLabel, formatDuration } from "../../shared/lib/format";
import { sessionTitle } from "../../shared/lib/session";
import { MarkdownRenderer } from "../../shared/ui/MarkdownRenderer";
import { QueryState } from "../../shared/ui/QueryState";
import { MetaLine, OutcomeLabel } from "./parts";
import { pulseTone } from "./runStatus";
import { formatRunTime, pulseOutcomeLabel } from "./scheduleFormat";
import { useScheduleQuery } from "./scheduleQuery";

export interface PulseRunBodyProps {
  runId: string;
  authToken: string;
  sessions: readonly SessionEntry[];
  onOpenSession: (sessionKey: string) => void;
  /** Opens the intention that fired, where it is set up. */
  onOpenIntention: (intentionId: string) => void;
}

/** One Pulse firing: what came of it, where it went, and what it attended to. */
export function PulseRunBody({
  runId,
  authToken,
  sessions,
  onOpenSession,
  onOpenIntention,
}: PulseRunBodyProps) {
  const state = useScheduleQuery(authToken, ["pulse-run", runId], () => fetchPulseRunDetail(runId, authToken));
  return (
    <QueryState state={state}>
      {(run) => (
        <PulseRunContent
          run={run}
          sessions={sessions}
          onOpenSession={onOpenSession}
          onOpenIntention={onOpenIntention}
        />
      )}
    </QueryState>
  );
}

function PulseRunContent({
  run,
  sessions,
  onOpenSession,
  onOpenIntention,
}: {
  run: PulseRunDetail;
  sessions: readonly SessionEntry[];
  onOpenSession: (sessionKey: string) => void;
  onOpenIntention: (intentionId: string) => void;
}) {
  const { delivery } = run;
  const session = delivery && sessions.find((candidate) => candidate.session_key === delivery.session_key);
  const took = run.finished_at
    ? formatDuration(Date.parse(run.finished_at) - Date.parse(run.started_at))
    : null;
  return (
    <>
      <header className="run-detail-head">
        <h2 className="run-detail-title">
          <button
            type="button"
            className="title-link"
            title="Open this intention in Pulse"
            onClick={() => onOpenIntention(run.intention_id)}
          >
            {run.intention_id}
            <span className="title-link-glyph" aria-hidden="true">
              ↗
            </span>
          </button>
        </h2>
        <MetaLine
          items={[
            <OutcomeLabel key="outcome" tone={pulseTone(run.status)}>
              {pulseOutcomeLabel(run)}
            </OutcomeLabel>,
            <span key="at">
              {formatRunTime(run.started_at)}
              {took !== null && <span className="run-meta-duration"> ({took})</span>}
            </span>,
          ]}
        />
      </header>

      <PulseOutcome run={run} />

      <p className="run-detail-line">
        {delivery && (
          <>
            Sent to {channelLabel(delivery.channel)}
            {session && ` · ${sessionTitle(session)}`}{" "}
            <button type="button" className="text-link" onClick={() => onOpenSession(delivery.session_key)}>
              Open in Chat
            </button>{" "}
          </>
        )}
        <button type="button" className="text-link" onClick={() => onOpenIntention(run.intention_id)}>
          Open intention
        </button>
      </p>

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
