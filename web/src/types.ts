export interface ChatMessage {
  id: string;
  sender_id: string;
  sender_kind: "user" | "assistant" | "system" | "tool";
  content: string;
  timestamp: string;
  message_kind: string;
}

export interface ToolEventData {
  name: string;
  state: "pending" | "success" | "error";
  input?: unknown;
  output?: string;
  is_error?: boolean;
  duration_ms?: number;
}

export interface SleepRun {
  id: string;
  agent_id: string;
  status: string;
  trigger_type: string;
  started_at: string;
  finished_at: string | null;
  source_chats_json: string;
  source_digest_md: string;
  input_tokens: number;
  output_tokens: number;
  total_tokens: number;
  error_message: string | null;
}

export interface MemorySnapshot {
  id: string;
  run_id: string;
  agent_id: string;
  file: string;
  content_before: string;
  content_after: string;
  created_at: string;
}
