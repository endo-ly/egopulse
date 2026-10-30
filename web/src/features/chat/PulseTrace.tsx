import { useId, useState } from "react";
import type { ChatMessage } from "../../shared/api/types";
import { MarkdownRenderer } from "./MarkdownRenderer";
import { ToolCard } from "./ToolCard";
import { parseToolEvent } from "./toolEvent";
import type { PulseTraceData } from "./timelineEntries";

export interface PulseTraceProps {
  trace: PulseTraceData;
}

/**
 * One quiet line naming the Pulse intention behind a delivered output. It
 * expands in place to the activation trace for auditing.
 */
export function PulseTrace({ trace }: PulseTraceProps) {
  const [expanded, setExpanded] = useState(false);
  const bodyId = useId();
  const name = trace.intentionId ?? "Pulse";
  const expandable = trace.entries.length > 0;

  const label = (
    <>
      <PulseIcon />
      <span className="pulse-trace-name">{name}</span>
    </>
  );

  return (
    <div className="pulse-trace">
      {expandable ? (
        <button
          type="button"
          className="pulse-trace-summary"
          aria-expanded={expanded}
          aria-controls={expanded ? bodyId : undefined}
          title={expanded ? "Hide Pulse activity" : "Show Pulse activity"}
          onClick={() => setExpanded((open) => !open)}
        >
          {label}
          <ChevronIcon />
        </button>
      ) : (
        <span className="pulse-trace-summary">{label}</span>
      )}
      {expandable && expanded && (
        <div id={bodyId} className="pulse-trace-body">
          {trace.entries.map((entry) => (
            <PulseTraceEntry key={entry.id} message={entry} />
          ))}
        </div>
      )}
    </div>
  );
}

function PulseTraceEntry({ message }: { message: ChatMessage }) {
  const toolEvent = parseToolEvent(message);
  if (toolEvent) return <ToolCard event={toolEvent} />;
  if (message.pulse?.role === "input") {
    return <p className="pulse-trace-input">{message.content}</p>;
  }
  return (
    <div className="pulse-trace-narration">
      <MarkdownRenderer content={message.content} />
    </div>
  );
}

function PulseIcon() {
  return (
    <svg
      className="pulse-trace-icon"
      width="12"
      height="12"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2.25"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M22 12h-4l-3 9L9 3l-3 9H2" />
    </svg>
  );
}

function ChevronIcon() {
  return (
    <svg
      className="pulse-trace-chevron"
      width="12"
      height="12"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2.25"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="m9 18 6-6-6-6" />
    </svg>
  );
}
