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

/** One page of a run listing, as the API returns it. */
export interface RunPage<T> {
  runs: T[];
  /** Whether older runs exist beyond this page. */
  has_more: boolean;
}

export type SleepRunStatus = "running" | "success" | "partial_failure" | "failed" | "skipped";
export type SleepRunTrigger = "manual" | "scheduled" | "backfill";
export type SleepStepStatus = "pending" | "running" | "success" | "failed" | "skipped";

export interface SleepRun {
  id: string;
  agent_id: string;
  status: SleepRunStatus;
  trigger: SleepRunTrigger;
  started_at: string;
  finished_at: string | null;
  source_chats_json: string;
  source_digest_md: string | null;
  input_tokens: number;
  output_tokens: number;
  total_tokens: number;
  error_message: string | null;
  session_count: number;
  /** Memory files this run published changes to, episodic → semantic → prospective. */
  changed_memory: string[];
}

export interface SleepRunStep {
  step: string;
  status: SleepStepStatus;
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

/** The agent's next scheduled sleep check and the condition it decides on. */
export interface SleepOutlook {
  scheduler_enabled: boolean;
  /** Whether the agent is one of the scheduler's targets. */
  targeted: boolean;
  /** Null when the scheduler is disabled or the agent is not targeted. */
  next_check_at: string | null;
  pending_messages: number;
  /** Pending messages above this make the check run. */
  threshold: number;
}

export type PulseRunStatus = "running" | "success" | "failed" | "skipped";
export type PulseOutputKind = "notify" | "silent";

export interface PulseRun {
  id: string;
  agent_id: string;
  intention_id: string;
  status: PulseRunStatus;
  started_at: string;
  finished_at: string | null;
  output_kind: PulseOutputKind | null;
  /** The delivered message for `notify`, `PULSE_OK` for `silent`. */
  output_text: string | null;
  /** The error for `failed`, the reason for `skipped`. */
  error_message: string | null;
}

export interface PulseRunDetail extends PulseRun {
  /** Attention the run was activated with; null for runs that predate it. */
  attention_snapshot: string | null;
  /** The session a notification was delivered to. */
  delivery: (PulseDelivery & { session_key: string }) | null;
}

export interface PulseDelivery {
  channel: string;
  external_chat_id: string;
}

export type PulseSchedule =
  | { kind: "daily"; at: string }
  | { kind: "weekly"; day: string; at: string }
  | { kind: "interval"; interval_days: number; at: string }
  | { kind: "once"; date: string; at: string };

export interface PulseNextDue {
  state: "due_now" | "scheduled" | "attempted_this_period" | "attempted_once" | "no_upcoming";
  /** Set for `scheduled`. */
  at: string | null;
}

export interface PulseIntention {
  id: string;
  enabled: boolean;
  schedule: PulseSchedule;
  attention: string;
  /** Null when the intention uses the default delivery. */
  delivery: PulseDelivery | null;
  /**
   * The conversation a firing would be delivered to now, resolved as the
   * scheduler does; null when none resolves, so a firing would be skipped.
   */
  target: { channel: string; session_key: string } | null;
  /** Null while the agent's Pulse or the intention is disabled. */
  next: PulseNextDue | null;
  /** Newest first. */
  recent_runs: Pick<PulseRun, "id" | "status" | "output_kind" | "started_at">[];
}

export interface AgentPulse {
  /** Null when PULSE.yaml fails to parse. */
  enabled: boolean | null;
  error: string | null;
  timezone: string;
  default_delivery: PulseDelivery | null;
  intentions: PulseIntention[];
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
