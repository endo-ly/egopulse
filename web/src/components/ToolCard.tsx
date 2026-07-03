import { useState, useEffect, useRef } from "react";
import { Spinner } from "./Spinner";
import type { ToolEventData } from "../types";

export interface ToolCardProps {
  event: ToolEventData;
  defaultExpanded?: boolean;
}

function buildSummary(event: ToolEventData): string {
  if (event.state === "pending") return "running…";
  if (event.state === "error") {
    const raw = event.output ?? "";
    return raw.slice(0, 40);
  }
  return summarizeInput(event.name, event.input);
}

function summarizeInput(name: string, input: unknown): string {
  if (!input || typeof input !== "object") return name;
  const obj = input as Record<string, unknown>;
  const pathFields = ["path", "file", "filename"];
  const cmdFields = ["command", "cmd"];
  const queryFields = ["query", "q", "search"];

  for (const f of pathFields) {
    if (typeof obj[f] === "string") return `${name} ${obj[f] as string}`;
  }
  for (const f of cmdFields) {
    if (typeof obj[f] === "string") return `${name} ${obj[f] as string}`;
  }
  for (const f of queryFields) {
    if (typeof obj[f] === "string") return `${name} "${obj[f] as string}"`;
  }
  if (obj.to && typeof obj.to === "string") return `${name} → ${obj.to as string}`;

  for (const v of Object.values(obj)) {
    if (typeof v === "string" || typeof v === "number") {
      return `${name} ${String(v)}`;
    }
  }
  return name;
}

export function ToolCard({ event, defaultExpanded }: ToolCardProps) {
  const autoExpand = event.state === "error";
  const [expanded, setExpanded] = useState(defaultExpanded ?? autoExpand);
  const prevState = useRef(event.state);

  useEffect(() => {
    if (event.state === "error" && prevState.current !== "error") {
      setExpanded(true);
    }
    prevState.current = event.state;
  }, [event.state]);

  const summary = buildSummary(event);

  return (
    <div className="tool-card">
      <button
        type="button"
        className="tool-card-header"
        aria-expanded={expanded}
        onClick={() => setExpanded((e) => !e)}
      >
        <span className="tool-card-summary">{summary}</span>
        <span className="tool-card-badge">
          {event.state === "pending" && <Spinner size="sm" />}
          {event.state === "success" && event.duration_ms != null && (
            <span className="tool-card-duration">{event.duration_ms}ms</span>
          )}
          {event.state === "error" && (
            <span className="tool-card-error-badge">error</span>
          )}
        </span>
      </button>
      {expanded && (
        <div className="tool-card-body">
          {event.input != null && (
            <pre className="tool-card-io">
              <code>{JSON.stringify(event.input, null, 2)}</code>
            </pre>
          )}
          {event.output != null && (
            <pre className="tool-card-io">
              <code>{event.output}</code>
            </pre>
          )}
        </div>
      )}
    </div>
  );
}
