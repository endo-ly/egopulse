export interface AgentEntry {
  id: string;
  label: string;
  is_default: boolean;
  /** Cache-busted avatar URL; null when the agent has no uploaded image. */
  avatar_url?: string | null;
}

export interface SessionEntry {
  session_key: string;
  chat_id: number;
  /** User-managed display name; null when the session is unnamed. */
  name: string | null;
  channel: string;
  agent_id: string;
  last_message_preview: string;
  last_message_time: number;
  archived: boolean;
  pinned: boolean;
}

export interface ChatMessage {
  id: string;
  sender_id: string;
  sender_kind: "user" | "assistant" | "system" | "tool";
  content: string;
  timestamp: string;
  message_kind: string;
  /** Pulse activation this row belongs to; absent or null outside any activation. */
  pulse?: PulseRef | null;
  /** Owning run for live entries (replay-truncated cleanup); absent on persisted rows. */
  runId?: string;
}

/** A history row's place in one Pulse activation. */
export interface PulseRef {
  run_id: string;
  /** `output` is the delivered message; `input` and `step` are its trace. */
  role: "input" | "step" | "output";
  /** Null when the run record is gone. */
  intention_id: string | null;
}

/** One page of committed chat history returned by `GET /api/history`. */
export interface HistoryPage {
  messages: ChatMessage[];
  /** Exclusive seq cursor for the next older page; null at the beginning. */
  nextBeforeSeq: number | null;
  /** Highest committed seq in this page; null for an empty chat. */
  newestSeq: number | null;
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
  trigger: string;
  started_at: string;
  finished_at: string | null;
  source_chats_json: string;
  source_digest_md: string;
  input_tokens: number;
  output_tokens: number;
  total_tokens: number;
  error_message: string | null;
  session_count: number;
}

export interface SleepRunStep {
  step: string;
  status: string;
  started_at: string | null;
  finished_at: string | null;
  input_tokens: number;
  output_tokens: number;
  error_message: string | null;
}

export interface SleepRunDetail {
  run: SleepRun;
  snapshots: MemorySnapshot[];
  steps: SleepRunStep[];
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

export const MEMORY_LAYERS = ["episodic", "semantic", "prospective"] as const;
export type MemoryLayer = (typeof MEMORY_LAYERS)[number];

/** Published content of each long-term memory layer. */
export type AgentMemory = Record<MemoryLayer, string>;

/** Resolution level that supplied an LLM setting. */
export type LlmSettingSource = "profile" | "agent" | "global_default" | "provider_default";

export interface AgentLlmRow {
  /** Null for channels without a profile. */
  channel: string | null;
  provider: string;
  model: string;
  provider_source: LlmSettingSource;
  model_source: LlmSettingSource;
}

export type PromptFileScope = "global" | "agent";

export interface PromptSource {
  scope: PromptFileScope;
  /** Path relative to the state root. */
  path: string;
  chars: number;
}

export interface AgentProfile {
  id: string;
  label: string;
  is_default: boolean;
  reasoning_effort: string | null;
  llm: AgentLlmRow[];
  bindings: { discord_bot: string | null; telegram_bot: string | null };
  prompt_sources: {
    /** The applied SOUL file; null when no SOUL is injected. */
    soul: PromptSource | null;
    /** Applied AGENTS files in injection order. */
    rules: PromptSource[];
    /** Location and presence of SECRET.md; its content is fetched separately. */
    secret: { path: string; exists: boolean };
    memory: Record<MemoryLayer, number>;
    skills: number;
  };
}

export type PromptFileKind = "soul" | "rules" | "secret";

export interface PromptFileLayer {
  scope: PromptFileScope;
  path: string;
  /** Null when the file is missing or blank. */
  content: string | null;
  /** Whether the content is injected into the system prompt. */
  applied: boolean;
}

export interface MemoryChange {
  run_id: string;
  file: MemoryLayer;
  created_at: string;
  content_before: string;
  content_after: string;
}

export interface SkillSummary {
  name: string;
  description: string;
  source: "builtin" | "workspace";
}

export interface SkillFileEntry {
  /** Path relative to the skill directory. */
  path: string;
  bytes: number;
}

export interface SkillDetail extends SkillSummary {
  /** Skill directory relative to the state root. */
  path: string;
  platforms: string[];
  deps: string[];
  required_env: string[];
  /** SKILL.md body without its frontmatter. */
  instructions: string;
  /** Files bundled with the skill besides SKILL.md. */
  files: SkillFileEntry[];
}
