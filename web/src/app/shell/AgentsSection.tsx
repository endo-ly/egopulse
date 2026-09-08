import { useEffect, useRef, useState } from "react";
import { StatusDot } from "../../shared/ui/StatusDot";
import type { AgentEntry } from "../../shared/api/types";
import { deleteAgentAvatar, putAgentAvatar } from "../../shared/api/agents";
import { AvatarCropModal } from "./AvatarCropModal";

export interface AgentsSectionProps {
  agents: AgentEntry[];
  selectedAgent: string;
  onSelectAgent: (id: string) => void;
  authToken?: string;
  /** Authorized object URLs resolved by the shared avatar hook. */
  avatarUrls?: Readonly<Record<string, string>>;
  /** Called after an avatar upload/removal so the agent list can refresh. */
  onAvatarChanged?: () => void;
  /** Agent ids with sessions newer than the last view. */
  unreadAgentIds?: ReadonlySet<string>;
}

const PENCIL_ICON = (
  <svg
    width="12"
    height="12"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="2"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <path d="M17 3a2.85 2.83 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5Z" />
  </svg>
);

export function AgentsSection({
  agents,
  selectedAgent,
  onSelectAgent,
  authToken,
  avatarUrls,
  onAvatarChanged,
  unreadAgentIds,
}: AgentsSectionProps) {
  const [menuAgentId, setMenuAgentId] = useState<string | null>(null);
  const [cropFile, setCropFile] = useState<{ agentId: string; file: File } | null>(
    null,
  );
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const uploadAgentRef = useRef<string | null>(null);

  // Close the edit menu on any interaction outside of it (the menu itself and
  // its toggle button handle their own clicks) or on Escape.
  useEffect(() => {
    if (menuAgentId === null) return;
    const handlePointerDown = (event: MouseEvent) => {
      const target = event.target as Element | null;
      if (
        target?.closest(".agent-avatar-menu") ||
        target?.closest(".agent-avatar-edit")
      ) {
        return;
      }
      setMenuAgentId(null);
    };
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setMenuAgentId(null);
    };
    document.addEventListener("mousedown", handlePointerDown);
    document.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("mousedown", handlePointerDown);
      document.removeEventListener("keydown", handleKeyDown);
    };
  }, [menuAgentId]);

  const closeMenu = () => setMenuAgentId(null);

  const openFilePicker = (agentId: string) => {
    uploadAgentRef.current = agentId;
    closeMenu();
    fileInputRef.current?.click();
  };

  const handleFileChange = (event: React.ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    const agentId = uploadAgentRef.current;
    event.target.value = "";
    uploadAgentRef.current = null;
    if (!file || !agentId) return;
    setCropFile({ agentId, file });
  };

  const handleCropApply = async (blob: Blob) => {
    if (!cropFile) return;
    setBusy(true);
    setError(null);
    try {
      await putAgentAvatar(cropFile.agentId, blob, authToken ?? "");
      onAvatarChanged?.();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      // Close the editor in the error case too, so the message below the
      // agent list is not hidden behind the modal.
      setCropFile(null);
      setBusy(false);
    }
  };

  const handleRemove = async (agentId: string) => {
    closeMenu();
    setBusy(true);
    setError(null);
    try {
      await deleteAgentAvatar(agentId, authToken ?? "");
      onAvatarChanged?.();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="agents-section">
      <h2 className="section-title">AGENTS</h2>
      <ul className="agents-list">
        {agents.map((agent) => (
          <li key={agent.id} className="agent-item">
            <button
              type="button"
              className={`agent-row ${selectedAgent === agent.id ? "active" : ""}`}
              aria-current={selectedAgent === agent.id ? "true" : undefined}
              onClick={() => onSelectAgent(agent.id)}
            >
              {avatarUrls?.[agent.id] ? (
                <span className="agent-avatar">
                  <img src={avatarUrls[agent.id]} alt="" />
                  <StatusDot
                    tone={unreadAgentIds?.has(agent.id) ? "unread" : "idle"}
                    className="agent-avatar-dot"
                  />
                </span>
              ) : (
                <StatusDot
                  tone={unreadAgentIds?.has(agent.id) ? "unread" : "idle"}
                />
              )}
              <span className="agent-label">{agent.label}</span>
              {agent.is_default && (
                <span className="agent-default-tag">default</span>
              )}
            </button>
            <button
              type="button"
              className="agent-avatar-edit"
              aria-label={`Change ${agent.label} icon`}
              title="Change icon"
              disabled={busy}
              onClick={() =>
                setMenuAgentId(menuAgentId === agent.id ? null : agent.id)
              }
            >
              {PENCIL_ICON}
            </button>
            {menuAgentId === agent.id && (
              <div className="agent-avatar-menu" role="menu">
                <button
                  type="button"
                  role="menuitem"
                  onClick={() => openFilePicker(agent.id)}
                >
                  Upload image…
                </button>
                {agent.avatar_url && (
                  <button
                    type="button"
                    role="menuitem"
                    className="danger"
                    onClick={() => handleRemove(agent.id)}
                  >
                    Remove image
                  </button>
                )}
              </div>
            )}
          </li>
        ))}
      </ul>
      {error && <p className="agent-avatar-error">{error}</p>}
      <input
        ref={fileInputRef}
        type="file"
        accept="image/*"
        hidden
        onChange={handleFileChange}
      />
      {cropFile && (
        <AvatarCropModal
          file={cropFile.file}
          onCancel={() => setCropFile(null)}
          onApply={handleCropApply}
        />
      )}
    </div>
  );
}
