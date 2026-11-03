import { useEffect, useRef, useState } from "react";
import { apiFetchBlob } from "../../shared/api/client";
import type { AttachmentInfo } from "../../shared/api/types";
import { Spinner } from "../../shared/ui/Spinner";

export interface AttachmentCardProps {
  attachment: AttachmentInfo;
  /** Optional text the agent passed to `send_attachment`. */
  caption?: string;
  authToken: string;
}

type LoadState =
  | { status: "idle" }
  | { status: "loading" }
  | { status: "ready"; url: string }
  | { status: "error"; message: string };

/** Formats a byte count for the attachment header. */
function formatAttachmentBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "";
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value < 10 ? value.toFixed(1) : Math.round(value)} ${units[unit]}`;
}

/**
 * Renders one Web-delivered file. The attachment endpoint sits behind bearer
 * auth, which media elements cannot send, so the blob is fetched on demand and
 * exposed through an object URL. Loading on demand keeps a long history with
 * many audio messages from downloading everything at once.
 */
export function AttachmentCard({
  attachment,
  caption,
  authToken,
}: AttachmentCardProps) {
  const [state, setState] = useState<LoadState>({ status: "idle" });
  const [playRequested, setPlayRequested] = useState(false);
  const objectUrlRef = useRef<string | null>(null);
  const loadingRef = useRef(false);
  const mountedRef = useRef(true);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      if (objectUrlRef.current) {
        URL.revokeObjectURL(objectUrlRef.current);
        objectUrlRef.current = null;
      }
    };
  }, []);

  const load = async (): Promise<string | null> => {
    if (state.status === "ready") return state.url;
    if (loadingRef.current) return null;
    loadingRef.current = true;
    setState({ status: "loading" });
    try {
      const blob = await apiFetchBlob(
        `/api/attachments/${encodeURIComponent(attachment.id)}`,
        authToken,
      );
      if (!mountedRef.current) return null;
      const url = URL.createObjectURL(blob);
      if (objectUrlRef.current) URL.revokeObjectURL(objectUrlRef.current);
      objectUrlRef.current = url;
      setState({ status: "ready", url });
      return url;
    } catch (error) {
      if (mountedRef.current) {
        setState({
          status: "error",
          message: error instanceof Error ? error.message : String(error),
        });
      }
      return null;
    } finally {
      loadingRef.current = false;
    }
  };

  const play = () => {
    setPlayRequested(true);
    void load();
  };

  const download = async () => {
    setPlayRequested(false);
    const url = await load();
    if (!url) return;
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = attachment.filename;
    anchor.rel = "noopener";
    document.body.appendChild(anchor);
    anchor.click();
    anchor.remove();
  };

  const isAudio = attachment.content_type.startsWith("audio/");

  return (
    <div className="attachment-card">
      <div className="attachment-card-header">
        <span className="attachment-card-icon" aria-hidden="true">
          {isAudio ? <SpeakerIcon /> : <FileIcon />}
        </span>
        <span className="attachment-card-title" title={attachment.filename}>
          {attachment.filename}
        </span>
        <span className="attachment-card-size">
          {formatAttachmentBytes(attachment.size_bytes)}
        </span>
      </div>
      {caption && <p className="attachment-card-caption">{caption}</p>}
      {isAudio && state.status === "ready" ? (
        <audio
          className="attachment-audio"
          controls
          autoPlay={playRequested}
          aria-label={attachment.filename}
          src={state.url}
        />
      ) : null}
      <div className="attachment-card-actions">
        {isAudio && state.status !== "ready" ? (
          <button
            type="button"
            className="attachment-card-action"
            onClick={play}
            disabled={state.status === "loading"}
          >
            {state.status === "loading" ? (
              <>
                <Spinner size="sm" /> Loading…
              </>
            ) : (
              "Play audio"
            )}
          </button>
        ) : null}
        <button
          type="button"
          className="attachment-card-action"
          onClick={() => void download()}
          disabled={state.status === "loading"}
        >
          {state.status === "loading" ? "Loading…" : "Download"}
        </button>
      </div>
      {state.status === "error" && (
        <p className="attachment-card-error" role="alert">
          Failed to load attachment: {state.message}{" "}
          <button
            type="button"
            className="attachment-card-retry"
            onClick={() => void load()}
          >
            Retry
          </button>
        </p>
      )}
    </div>
  );
}

function SpeakerIcon() {
  return (
    <svg
      width="18"
      height="18"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <path d="M11 5 6 9H2v6h4l5 4V5Z" />
      <path d="M15.5 8.5a5 5 0 0 1 0 7" />
      <path d="M19 5a10 10 0 0 1 0 14" />
    </svg>
  );
}

function FileIcon() {
  return (
    <svg
      width="18"
      height="18"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8Z" />
      <polyline points="14 2 14 8 20 8" />
    </svg>
  );
}
