//! Context-window compaction.
//!
//! Long conversations are summarized into a checkpoint so the next model
//! request fits the provider's context window. The algorithm follows the
//! reference implementation:
//!
//! 1. Estimate the context tokens of the request about to be sent. The last
//!    measured provider usage is the base; only the difference in the request
//!    overhead and the messages appended after the measurement are estimated.
//!    An unchanged request therefore projects to exactly the measured value.
//! 2. Trigger when the estimate exceeds `context_window - reserve_tokens`.
//!    The token settings are resolved for the model that is about to run.
//! 3. Cut the history backwards from the newest message until at least
//!    `keep_recent_tokens` are retained. The cut never lands on a tool result.
//! 4. Summarize the compacted range, updating the previous checkpoint when one
//!    exists. A turn split mid-way gets a second, turn-prefix summary.
//! 5. Store the summary as a checkpoint and keep the retained messages
//!    verbatim.
//!
//! Summaries are reference material for the next request, never instructions.

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tracing::{info, warn};

use crate::agent_loop::TurnDependencies;
use crate::agent_loop::message_format::{
    is_tool_error_message, is_tool_result_success, message_to_archive_text, strip_thinking,
    tool_result_body,
};
use crate::agent_loop::session_snapshot::{
    CompactionCheckpoint, ContextUsageAnchor, SessionContext, checkpoint_message,
};
use crate::conversation::SurfaceContext;
use crate::error::{EgoPulseError, LlmError};
use crate::llm::calibration::CalibrationKey;
use crate::llm::tokens::{
    CHARS_PER_TOKEN_ESTIMATE, estimate_message_tokens, estimate_messages_tokens,
    estimate_text_tokens, text_chars,
};
use crate::llm::{LlmProvider, Message, MessageContent, MessageContentPart, MessagesResponse};
use crate::llm::{RetryPolicy, complete_response};
use crate::tools::sanitize_output_string;

/// Maximum characters of a single tool result inside a summary request.
const TOOL_RESULT_MAX_CHARS: usize = 2_000;

/// Appended when the history does not fit the summarizer's input budget.
const SUMMARY_TRUNCATION_MARKER: &str =
    "\n\n[... conversation truncated to fit the summarization request ...]";

/// Separator between the history summary and a split-turn prefix summary.
const SPLIT_TURN_SEPARATOR: &str = "\n\n---\n\n**Turn Context (split turn):**\n\n";

/// Text used when a split turn has no summarized history before it.
const NO_PRIOR_HISTORY: &str = "No prior history.";

/// One summarization request shares the LLM client's own request ceiling.
///
/// Retries, backoff, and `Retry-After` waits are bounded by this deadline, so a
/// provider that keeps failing cannot stretch one compaction indefinitely.
const SUMMARY_REQUEST_DEADLINE: Duration = Duration::from_secs(600);

/// System prompt for every checkpoint summarization request.
const SUMMARIZATION_SYSTEM_PROMPT: &str = "\
You are a context summarization assistant. Read the conversation between a user and an AI assistant, \
then write the structured summary requested by the instructions. Do NOT continue the conversation, \
do NOT answer questions in it, and do NOT invent goals or next actions it does not contain. \
Only output the summary.";

/// Instructions for the first checkpoint of a conversation.
const INITIAL_SUMMARY_PROMPT: &str = "\
The conversation above is history that must be summarized for the next agent. \
It is not a request to answer.

Write a structured checkpoint in the same language the conversation used, using exactly this format:

## 会話の経緯
Topic changes, important events, and how the understanding evolved.

## 共有された情報
The user's facts, opinions, preferences, and conditions; agreements; and statements that were later corrected.

## 重要な参照情報
Numbers, dates, proper nouns, file paths, and identifiers needed to continue. Omit when unnecessary.

Rules:
- Separate facts stated in the conversation from your own inference.
- Keep what the user said and what the assistant said distinguishable.
- Record decisions as decisions and proposals as proposals.
- Do not invent goals, tasks, or next actions that the conversation does not contain.
- Preserve exact identifiers, numbers, paths, and error messages.
- Omit a section instead of writing empty bullets.";

/// Instructions used when a checkpoint already exists.
const UPDATE_SUMMARY_PROMPT: &str = "\
The conversation above contains new messages to incorporate into the checkpoint provided in \
<previous-summary>. Update the summary with the new information.

Rules:
- Preserve information that is still valid.
- Move corrected facts, withdrawn wishes, and resolved topics to their latest state.
- Merge duplicates instead of repeating the previous wording verbatim.
- Do not invent goals, tasks, or next actions that the conversation does not contain.
- Preserve exact identifiers, numbers, paths, and error messages.

Use the same format as the previous summary:

## 会話の経緯
## 共有された情報
## 重要な参照情報";

/// Instructions used to summarize the first part of a split turn.
const TURN_PREFIX_SUMMARY_PROMPT: &str = "\
The messages above are the earlier part of one ongoing turn. The later messages are stored \
separately and follow this checkpoint verbatim.

Create a concise checkpoint of what the user asked and what happened so far, so the later messages \
can be understood in context.

## この発話の始まり
- What the user was talking about when this turn started.

## ここまでの経過
- What happened in these messages.

## 続きの理解に必要な情報
- Information from these messages needed to understand the later messages.

Only summarize information explicitly present above. Do not infer or reconstruct the later messages.";

/// Identity of the request a provider usage was measured against.
#[derive(Clone, Debug)]
pub(crate) struct RequestMeasurement {
    /// `provider/model` that produced the usage.
    pub(crate) model_key: String,
    /// Hash of the request shape (system prompt, tool definitions, model).
    pub(crate) request_shape_hash: String,
    /// Estimated tokens of the request that were not conversation messages
    /// (system prompt, tools, checkpoint, request-only messages).
    pub(crate) request_overhead_tokens: usize,
}

impl RequestMeasurement {
    /// Builds the anchor for `usage` measured at `index`.
    ///
    /// Returns `None` when the usage has no token counts, so a zero report
    /// cannot replace a valid earlier measurement.
    pub(crate) fn anchor(
        &self,
        index: usize,
        usage: &crate::llm::LlmUsage,
    ) -> Option<ContextUsageAnchor> {
        let anchor = ContextUsageAnchor::for_message(
            index,
            usage,
            self.request_overhead_tokens,
            &self.model_key,
            &self.request_shape_hash,
        );
        anchor.has_measured_tokens().then_some(anchor)
    }
}

/// Provider usage plus the request identity it was measured against.
#[derive(Clone, Debug)]
pub(crate) struct MeasuredUsage {
    pub(crate) usage: crate::llm::LlmUsage,
    pub(crate) request: RequestMeasurement,
}

impl MeasuredUsage {
    /// Builds the anchor for the assistant message at `index`.
    pub(crate) fn anchor(&self, index: usize) -> Option<ContextUsageAnchor> {
        self.request.anchor(index, &self.usage)
    }
}

/// Request shape and dependencies shared by every compaction entry point.
pub(crate) struct CompactionRequest<'a> {
    pub(crate) state: &'a TurnDependencies,
    pub(crate) context: &'a SurfaceContext,
    pub(crate) chat_id: i64,
    pub(crate) llm: &'a Arc<dyn LlmProvider>,
    pub(crate) prompt: PromptContext<'a>,
    pub(crate) config: &'a crate::config::Config,
    /// Messages added only to the current request (channel context, runtime
    /// guards). They count toward the projection but are never summarized or
    /// persisted into the checkpoint.
    pub(crate) request_only_messages: Vec<&'a Message>,
}

/// The final request context that the compaction decision is based on.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PromptContext<'a> {
    pub(crate) system_prompt: &'a str,
    pub(crate) tools_json: Option<&'a str>,
    pub(crate) has_tools: bool,
}

/// Why compaction is being attempted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompactionReason {
    /// The projected request no longer fits `context_window - reserve_tokens`.
    Threshold,
    /// A user ran `/compact`.
    Manual,
    /// The provider rejected a request for exceeding the context window.
    Overflow,
}

impl CompactionReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Threshold => "threshold",
            Self::Manual => "manual",
            Self::Overflow => "overflow",
        }
    }
}

/// Result of one compaction attempt.
#[derive(Debug, Clone)]
pub(crate) enum CompactionOutcome {
    /// Nothing was summarized; the caller keeps its current context.
    Unchanged,
    /// The context was replaced by a checkpoint plus the retained messages.
    Compacted(SessionContext),
}

/// A valid cut position in the retained history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CutPoint {
    /// Index of the first message kept verbatim.
    pub(crate) first_kept_index: usize,
    /// Index of the user message that started the turn being split.
    pub(crate) turn_start_index: Option<usize>,
    /// Whether the cut lands inside a turn rather than at its start.
    pub(crate) is_split_turn: bool,
}

/// Everything needed to summarize a range of the conversation.
#[derive(Debug)]
struct CompactionPreparation {
    first_kept_index: usize,
    messages_to_summarize: Vec<Message>,
    turn_prefix_messages: Vec<Message>,
    is_split_turn: bool,
    previous_summary: Option<String>,
    file_operations: FileOperations,
}

/// File paths touched by `read` / `write` / `edit` tool calls.
#[derive(Clone, Debug, Default)]
struct FileOperations {
    read: BTreeSet<String>,
    modified: BTreeSet<String>,
}

impl FileOperations {
    fn from_checkpoint(checkpoint: Option<&CompactionCheckpoint>) -> Self {
        let mut operations = Self::default();
        if let Some(checkpoint) = checkpoint {
            operations
                .read
                .extend(checkpoint.read_files.iter().cloned());
            operations
                .modified
                .extend(checkpoint.modified_files.iter().cloned());
        }
        operations
    }

    fn extract_from(&mut self, messages: &[Message]) {
        let mut index = 0;
        while index < messages.len() {
            let message = &messages[index];
            if message.role != "assistant" || message.tool_calls.is_empty() {
                index += 1;
                continue;
            }

            // A tool call is identified only within its own result block.
            // The same id may be reused by a later turn, so a conversation-wide
            // lookup would apply one turn's outcome to another turn's call.
            let mut successful_results = HashSet::new();
            let mut result_index = index + 1;
            while let Some(result) = messages.get(result_index) {
                if result.role != "tool" {
                    break;
                }
                if let Some(tool_call_id) = result.tool_call_id.as_deref() {
                    if is_tool_result_success(result) {
                        successful_results.insert(tool_call_id.to_string());
                    }
                }
                result_index += 1;
            }

            for tool_call in &message.tool_calls {
                // Invalid arguments are rejected before the Tool runs, so the
                // call cannot have touched the file. A normal write or edit is
                // different: it may have written part of the file before an
                // error, so an error result does not prove the file is intact.
                if tool_call.invalid_arguments.is_some() {
                    continue;
                }
                let Some(path) = tool_call
                    .arguments
                    .get("path")
                    .and_then(serde_json::Value::as_str)
                else {
                    continue;
                };
                let path = path.trim();
                if path.is_empty() {
                    continue;
                }
                match tool_call.name.as_str() {
                    "read" if successful_results.contains(&tool_call.id) => {
                        self.read.insert(path.to_string());
                    }
                    "write" | "edit" => {
                        self.modified.insert(path.to_string());
                    }
                    _ => {}
                }
            }
            index = result_index;
        }
    }

    /// Returns `(read_files, modified_files)`. A modified file is never also
    /// reported as read-only.
    fn compute_lists(&self) -> (Vec<String>, Vec<String>) {
        let read_files = self
            .read
            .iter()
            .filter(|path| !self.modified.contains(*path))
            .cloned()
            .collect();
        let modified_files = self.modified.iter().cloned().collect();
        (read_files, modified_files)
    }
}

/// Returns the `provider/model` identity used by the usage anchor.
pub(crate) fn model_key(llm: &dyn LlmProvider) -> String {
    format!("{}/{}", llm.provider_name(), llm.model_name())
}

/// Returns a stable hash of the request shape that produced a usage sample.
///
/// The hash covers the parts of the request that change the prompt but are not
/// stored in the conversation itself: system prompt, tool definitions, and the
/// model. Raw prompt text is never persisted.
pub(crate) fn request_shape_hash(
    system_prompt: &str,
    tools_json: Option<&str>,
    model_key: &str,
) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(system_prompt, &mut hasher);
    std::hash::Hash::hash(&tools_json.unwrap_or_default(), &mut hasher);
    std::hash::Hash::hash(model_key, &mut hasher);
    format!("{:016x}", std::hash::Hasher::finish(&hasher))
}

/// Applies the usage calibration factor to a token delta.
///
/// The delta may be negative: the request overhead of the current request can
/// be smaller than the one that was measured. Converting it to an unsigned type
/// before scaling would wrap, so the sign is kept until the caller clamps the
/// final projection.
fn calibrated_estimate(tokens: i64, factor: f64) -> i64 {
    ((tokens as f64) * factor).ceil() as i64
}

/// Estimates the tokens of a request that are not conversation messages.
///
/// These are the parts that change between iterations of the same turn, so the
/// projection has to re-measure them whenever it reuses a usage measurement.
pub(crate) fn estimate_request_overhead(
    prompt: &PromptContext<'_>,
    session: &SessionContext,
    request_only_messages: &[&Message],
) -> usize {
    let mut tokens = estimate_text_tokens(prompt.system_prompt);
    if let Some(tools) = prompt.tools_json {
        tokens += estimate_text_tokens(tools);
    }
    if let Some(checkpoint) = &session.checkpoint {
        tokens += estimate_message_tokens(&checkpoint_message(checkpoint));
    }
    tokens
        + request_only_messages
            .iter()
            .map(|message| estimate_message_tokens(message))
            .sum::<usize>()
}

/// Estimates the tokens of a whole request including the checkpoint projection.
fn estimate_request_tokens(
    prompt: &PromptContext<'_>,
    session: &SessionContext,
    request_only_messages: &[&Message],
) -> usize {
    estimate_request_overhead(prompt, session, request_only_messages)
        + estimate_messages_tokens(&session.messages)
}

/// Projects the context tokens of the request that would be sent next.
///
/// A usage anchor measured against the same request shape describes the tokens
/// up to the measured assistant message. The anchor's own overhead is removed
/// and the current overhead is added again, so channel context, retry
/// corrections, and runtime guards that changed since the measurement are
/// always counted. The messages appended after the anchor are estimated. When
/// the anchor is missing or stale, the whole request is estimated instead.
pub(crate) fn project_context_tokens(
    prompt: &PromptContext<'_>,
    session: &SessionContext,
    request_only_messages: &[&Message],
    model_key: &str,
    shape_hash: &str,
    factor: f64,
) -> usize {
    let anchor_matches = session.last_usage.as_ref().filter(|anchor| {
        anchor.has_measured_tokens()
            && anchor.model_key == model_key
            && anchor.request_shape_hash == shape_hash
            && anchor.after_message_index < session.messages.len()
    });

    let Some(anchor) = anchor_matches else {
        return clamp_tokens(calibrated_estimate(
            estimate_request_tokens(prompt, session, request_only_messages) as i64,
            factor,
        ));
    };

    // The provider measured the whole request. Remove the overhead it was
    // measured with and add the current one again, so the conversation part
    // stays measured and only the difference is estimated.
    let trailing = &session.messages[anchor.after_message_index + 1..];
    let delta = estimate_messages_tokens(trailing) as i64
        + estimate_request_overhead(prompt, session, request_only_messages) as i64
        - anchor.request_overhead_tokens as i64;
    clamp_tokens(anchor.measured_context_tokens() as i64 + calibrated_estimate(delta, factor))
}

fn clamp_tokens(tokens: i64) -> usize {
    usize::try_from(tokens.max(0)).unwrap_or(usize::MAX)
}

/// Returns `true` when the projected context no longer fits the usable window.
pub(crate) fn should_compact(
    context_tokens: usize,
    context_window: usize,
    reserve_tokens: usize,
) -> bool {
    context_tokens > context_window.saturating_sub(reserve_tokens)
}

fn is_cut_point(message: &Message) -> bool {
    matches!(message.role.as_str(), "user" | "assistant")
}

fn is_turn_start(message: &Message) -> bool {
    message.role == "user"
}

/// Finds the boundary that keeps approximately `keep_recent_tokens` verbatim.
///
/// Walks backwards accumulating estimated message sizes until the budget is
/// reached, then cuts at the closest user or assistant message at or after that
/// position. Tool results are never cut points, so a tool call and its results
/// always stay together. When the whole history fits the budget the walk never
/// exceeds it, so the cut stays at the first cut point and compaction becomes a
/// no-op.
pub(crate) fn find_cut_point(messages: &[Message], keep_recent_tokens: usize) -> CutPoint {
    let cut_points: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, message)| is_cut_point(message))
        .map(|(index, _)| index)
        .collect();
    let Some(&first_cut_point) = cut_points.first() else {
        return CutPoint {
            first_kept_index: 0,
            turn_start_index: None,
            is_split_turn: false,
        };
    };

    let mut accumulated = 0usize;
    let mut cut_index = first_cut_point;
    for index in (0..messages.len()).rev() {
        accumulated += estimate_message_tokens(&messages[index]);
        if accumulated >= keep_recent_tokens {
            // If the trailing messages exceed the budget on their own, keep the
            // last valid cut point instead of falling back to the beginning:
            // summarizing everything would leave nothing to continue from.
            cut_index = cut_points
                .iter()
                .copied()
                .find(|candidate| *candidate >= index)
                .unwrap_or(*cut_points.last().expect("cut points are non-empty"));
            break;
        }
    }

    let starts_turn = is_turn_start(&messages[cut_index]);
    let turn_start_index = if starts_turn {
        None
    } else {
        (0..=cut_index)
            .rev()
            .find(|index| is_turn_start(&messages[*index]))
    };

    CutPoint {
        first_kept_index: cut_index,
        turn_start_index,
        is_split_turn: !starts_turn && turn_start_index.is_some(),
    }
}

/// Splits the conversation into the summarized ranges and the retained range.
fn prepare_compaction(
    session: &SessionContext,
    keep_recent_tokens: usize,
) -> Option<CompactionPreparation> {
    if session.messages.is_empty() {
        return None;
    }

    let cut = find_cut_point(&session.messages, keep_recent_tokens);
    let history_end = match cut.turn_start_index {
        Some(turn_start) if cut.is_split_turn => turn_start,
        _ => cut.first_kept_index,
    };
    let turn_prefix_start = cut.turn_start_index.unwrap_or(cut.first_kept_index);

    let messages_to_summarize: Vec<Message> = session.messages[..history_end].to_vec();
    let turn_prefix_messages: Vec<Message> = if cut.is_split_turn {
        session.messages[turn_prefix_start..cut.first_kept_index].to_vec()
    } else {
        Vec::new()
    };

    if messages_to_summarize.is_empty() && turn_prefix_messages.is_empty() {
        return None;
    }

    let mut file_operations = FileOperations::from_checkpoint(session.checkpoint.as_ref());
    file_operations.extract_from(&messages_to_summarize);
    file_operations.extract_from(&turn_prefix_messages);

    Some(CompactionPreparation {
        first_kept_index: cut.first_kept_index,
        messages_to_summarize,
        turn_prefix_messages,
        is_split_turn: cut.is_split_turn,
        previous_summary: session
            .checkpoint
            .as_ref()
            .map(|checkpoint| strip_file_operation_suffix(&checkpoint.summary).to_string()),
        file_operations,
    })
}

/// Returns the text form of one content value used inside a summary request.
fn content_text(content: &MessageContent) -> String {
    match content {
        MessageContent::Text(text) => text.clone(),
        MessageContent::Parts(parts) => parts
            .iter()
            .map(|part| match part {
                MessageContentPart::InputText { text } => text.clone(),
                MessageContentPart::InputImage { .. } => "[image]".to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn truncate_for_summary(text: &str, max_chars: usize) -> String {
    let total = text.chars().count();
    if total <= max_chars {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_chars).collect();
    format!(
        "{truncated}\n\n[... {} more characters truncated]",
        total - max_chars
    )
}

/// Serializes messages into plain text so the model does not continue them.
///
/// Visible text, tool calls, and tool results are kept; provider reasoning is
/// omitted because it is often not part of the model's live context. Each tool
/// result is truncated so one large result cannot dominate the request.
pub(crate) fn serialize_conversation(messages: &[Message]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for message in messages {
        match message.role.as_str() {
            "user" => {
                let content = content_text(&message.content);
                if !content.trim().is_empty() {
                    parts.push(format!("[User]: {content}"));
                }
            }
            "assistant" => {
                let content = content_text(&message.content);
                if !content.trim().is_empty() {
                    parts.push(format!("[Assistant]: {content}"));
                }
                if !message.tool_calls.is_empty() {
                    let calls = message
                        .tool_calls
                        .iter()
                        .map(|tool_call| format!("{}({})", tool_call.name, tool_call.arguments))
                        .collect::<Vec<_>>()
                        .join("; ");
                    parts.push(format!("[Assistant tool calls]: {calls}"));
                }
            }
            "tool" => {
                let content = content_text(&message.content);
                if content.trim().is_empty() {
                    continue;
                }
                let body = if serde_json::from_str::<serde_json::Value>(&content).is_ok() {
                    tool_result_body(&content)
                } else {
                    content
                };
                let label = if is_tool_error_message(message) {
                    "Tool error"
                } else {
                    "Tool result"
                };
                parts.push(format!(
                    "[{label}]: {}",
                    truncate_for_summary(&body, TOOL_RESULT_MAX_CHARS)
                ));
            }
            _ => {}
        }
    }
    parts.join("\n\n")
}

/// Returns the token budget for one summarization request.
///
/// The summarizer needs room for its own answer. Reserve the configured
/// output limit, bounded by half the context window so a model whose output
/// limit exceeds its window still leaves room for the history. A low budget
/// cannot increase a request; it only limits the conversation text so the
/// provider does not reject the summary for exceeding the context window.
fn summary_input_budget(request: &CompactionRequest<'_>) -> usize {
    let metadata = request.config.resolve_model_metadata(
        &crate::config::ProviderId::new(request.llm.provider_name()),
        request.llm.model_name(),
    );
    let output_reserve = metadata.max_output_tokens.min(metadata.context_window / 2);
    metadata
        .context_window
        .saturating_sub(output_reserve)
        .max(1)
}

/// Serializes the history within the summarizer's remaining input budget.
///
/// The oldest text is kept and the tail is replaced by a marker when the
/// serialized history does not fit. The original conversation remains in the
/// compaction archive and the append-only `messages` table.
fn summary_conversation(
    request: &CompactionRequest<'_>,
    messages: &[Message],
    fixed_prompt: &str,
) -> String {
    let budget = summary_input_budget(request);
    let overhead = summary_request_estimate(fixed_prompt);
    let conversation_budget = budget.saturating_sub(overhead);
    truncate_summary_conversation(serialize_conversation(messages), conversation_budget)
}

fn truncate_summary_conversation(conversation: String, budget_tokens: usize) -> String {
    let max_chars = budget_tokens.saturating_mul(CHARS_PER_TOKEN_ESTIMATE);
    if text_chars(&conversation) <= max_chars {
        return conversation;
    }
    let marker_chars = text_chars(SUMMARY_TRUNCATION_MARKER);
    let keep_chars = max_chars.saturating_sub(marker_chars);
    if keep_chars == 0 {
        return String::new();
    }
    let end = utf16_prefix_end(&conversation, keep_chars);
    if end == 0 {
        return String::new();
    }
    let mut truncated = conversation;
    truncated.truncate(end);
    truncated.push_str(SUMMARY_TRUNCATION_MARKER);
    truncated
}

/// Returns the byte index of the longest UTF-16 prefix within `max_chars`.
fn utf16_prefix_end(text: &str, max_chars: usize) -> usize {
    let mut used = 0;
    for (index, character) in text.char_indices() {
        let next = used + character.len_utf16();
        if next > max_chars {
            return index;
        }
        used = next;
    }
    text.len()
}

/// Removes a previous checkpoint's trailing file-operation blocks.
///
/// The lists are carried in the checkpoint fields and re-appended after the
/// update, so passing the old blocks back to the model would duplicate them.
fn strip_file_operation_suffix(summary: &str) -> &str {
    let mut end = summary.len();
    for marker in ["\n\n<read-files>", "\n\n<modified-files>"] {
        if let Some(index) = summary.find(marker) {
            end = end.min(index);
        }
    }
    summary[..end].trim_end()
}

fn file_operations_suffix(read_files: &[String], modified_files: &[String]) -> String {
    let mut sections = Vec::new();
    if !read_files.is_empty() {
        sections.push(format!(
            "<read-files>\n{}\n</read-files>",
            read_files.join("\n")
        ));
    }
    if !modified_files.is_empty() {
        sections.push(format!(
            "<modified-files>\n{}\n</modified-files>",
            modified_files.join("\n")
        ));
    }
    if sections.is_empty() {
        return String::new();
    }
    format!("\n\n{}", sections.join("\n\n"))
}

fn summary_prompt(
    conversation: &str,
    previous_summary: Option<&str>,
    instructions: Option<&str>,
) -> String {
    let mut prompt = format!("<conversation>\n{conversation}\n</conversation>\n\n");
    match previous_summary {
        Some(previous) => {
            prompt.push_str(&format!(
                "<previous-summary>\n{previous}\n</previous-summary>\n\n"
            ));
            prompt.push_str(UPDATE_SUMMARY_PROMPT);
        }
        None => prompt.push_str(INITIAL_SUMMARY_PROMPT),
    }
    if let Some(instructions) = instructions {
        prompt.push_str("\n\nAdditional focus: ");
        prompt.push_str(instructions);
    }
    prompt
}

fn turn_prefix_prompt(conversation: &str, instructions: Option<&str>) -> String {
    let mut prompt =
        format!("# Conversation\n{conversation}\n\n# Instructions\n{TURN_PREFIX_SUMMARY_PROMPT}");
    if let Some(instructions) = instructions {
        prompt.push_str("\n\nAdditional focus: ");
        prompt.push_str(instructions);
    }
    prompt
}

fn summary_request_estimate(prompt: &str) -> usize {
    (crate::llm::tokens::text_chars(prompt)
        + crate::llm::tokens::text_chars(SUMMARIZATION_SYSTEM_PROMPT))
    .div_ceil(CHARS_PER_TOKEN_ESTIMATE)
    .max(1)
}

async fn request_summary(
    llm: &Arc<dyn LlmProvider>,
    prompt: String,
) -> Result<MessagesResponse, LlmError> {
    let messages = Arc::new(vec![Message::text("user", prompt)]);
    let policy = RetryPolicy::new(tokio::time::Instant::now() + SUMMARY_REQUEST_DEADLINE);
    complete_response(
        policy,
        || llm.send_message(SUMMARIZATION_SYSTEM_PROMPT, Arc::clone(&messages), None),
        |attempt, delay, error| {
            warn!(
                provider = llm.provider_name(),
                model = llm.model_name(),
                attempt,
                delay_ms = delay.as_millis() as u64,
                error = %error,
                "context compaction summarization failed; retrying"
            );
        },
    )
    .await
    .map_err(|failure| failure.into_llm_error())
}

/// Rejects summaries that must never become a checkpoint.
fn validate_summary(response: &MessagesResponse) -> Result<String, EgoPulseError> {
    if !response.tool_calls.is_empty() {
        return Err(EgoPulseError::Llm(LlmError::InvalidResponse(
            "compaction summary contained a tool call".to_string(),
        )));
    }
    let text = strip_thinking(&response.content);
    if text.trim().is_empty() {
        return Err(EgoPulseError::Llm(LlmError::InvalidResponse(
            "compaction summary was empty".to_string(),
        )));
    }
    Ok(text)
}

async fn log_summarizer_usage(
    state: &TurnDependencies,
    context: &SurfaceContext,
    chat_id: i64,
    llm: &Arc<dyn LlmProvider>,
    response: &MessagesResponse,
    estimated_tokens: usize,
) {
    let Some(usage) = &response.usage else {
        return;
    };

    let db = state.storage_for(context.scope).db;
    let channel = context.channel.clone();
    let provider = llm.provider_name().to_string();
    let model = llm.model_name().to_string();
    let input_tokens = usage.input_tokens;
    let output_tokens = usage.output_tokens;
    let cached_input_tokens = usage.cached_input_tokens;
    let estimated_tokens: i64 = estimated_tokens.try_into().unwrap_or(0);
    crate::runtime::metrics::inc_llm_tokens_total("input", &provider, input_tokens);
    crate::runtime::metrics::inc_llm_tokens_total("output", &provider, output_tokens);
    let _ = crate::storage::call_blocking(db, move |db| {
        db.log_llm_usage(&crate::storage::LlmUsageLogEntry {
            chat_id,
            caller_channel: &channel,
            provider: &provider,
            model: &model,
            input_tokens,
            output_tokens,
            request_kind: "compaction",
            estimated_tokens,
            has_tools: false,
            cached_input_tokens,
        })
    })
    .await
    .inspect_err(|error| warn!(error = %error, "compaction usage logging failed"));
}

/// Sends one summarization request, records its usage, and validates the
/// result.
///
/// Every failure is returned: the caller decides whether the original context
/// is kept (automatic compaction) or reported to the user (`/compact`).
async fn summarize(
    request: &CompactionRequest<'_>,
    prompt: String,
    label: &str,
) -> Result<String, EgoPulseError> {
    let estimate = summary_request_estimate(&prompt);
    let response = match request_summary(request.llm, prompt).await {
        Ok(response) => response,
        Err(error) => {
            warn!(
                channel = %request.context.channel,
                chat_id = request.chat_id,
                label,
                error = %error,
                "context compaction summarization failed"
            );
            return Err(EgoPulseError::Llm(error));
        }
    };

    if let Some(usage) = &response.usage {
        request
            .state
            .usage_calibrator
            .record(
                CalibrationKey::new(
                    request.llm.provider_name(),
                    request.llm.model_name(),
                    "compaction",
                    false,
                ),
                estimate,
                usage.input_tokens,
            )
            .await;
    }
    log_summarizer_usage(
        request.state,
        request.context,
        request.chat_id,
        request.llm,
        &response,
        estimate,
    )
    .await;

    validate_summary(&response).inspect_err(|error| {
        warn!(
            channel = %request.context.channel,
            chat_id = request.chat_id,
            label,
            error = %error,
            "context compaction summary rejected"
        );
    })
}

async fn generate_summary(
    request: &CompactionRequest<'_>,
    messages: &[Message],
    previous_summary: Option<&str>,
    instructions: Option<&str>,
    label: &str,
) -> Result<String, EgoPulseError> {
    let fixed_prompt = summary_prompt("", previous_summary, instructions);
    let conversation = summary_conversation(request, messages, &fixed_prompt);
    let prompt = summary_prompt(&conversation, previous_summary, instructions);
    summarize(request, prompt, label).await
}

async fn generate_turn_prefix_summary(
    request: &CompactionRequest<'_>,
    messages: &[Message],
    instructions: Option<&str>,
) -> Result<String, EgoPulseError> {
    let fixed_prompt = turn_prefix_prompt("", instructions);
    let conversation = summary_conversation(request, messages, &fixed_prompt);
    let prompt = turn_prefix_prompt(&conversation, instructions);
    summarize(request, prompt, "turn prefix").await
}

/// Compacts the context when it no longer fits the provider window.
///
/// Returns [`CompactionOutcome::Unchanged`] when compaction is disabled, the
/// projected context still fits, or nothing is old enough to summarize.
///
/// # Errors
///
/// Returns the summarization error when the summary could not be produced.
/// Callers decide whether to keep the original context or surface the failure.
pub(crate) async fn compact_if_needed(
    request: &CompactionRequest<'_>,
    session: &SessionContext,
    instructions: Option<&str>,
) -> Result<CompactionOutcome, EgoPulseError> {
    if !request.config.compaction.enabled {
        return Ok(CompactionOutcome::Unchanged);
    }
    compact(request, session, CompactionReason::Threshold, instructions).await
}

/// Compacts the context regardless of the threshold.
///
/// Manual and overflow compaction still run when `compaction.enabled` is
/// false: the setting controls automatic compaction only.
///
/// The token settings are resolved for the model of this request exactly once,
/// so the trigger decision and the retained boundary cannot disagree when a
/// model override applies.
///
/// # Errors
///
/// Returns the summarization error when the summary could not be produced or
/// was unusable. The context is left untouched in that case.
pub(crate) async fn compact(
    request: &CompactionRequest<'_>,
    session: &SessionContext,
    reason: CompactionReason,
    instructions: Option<&str>,
) -> Result<CompactionOutcome, EgoPulseError> {
    let settings = request
        .config
        .compaction
        .for_model(request.llm.provider_name(), request.llm.model_name());
    let context_window = request
        .config
        .resolve_model_metadata(
            &crate::config::ProviderId::new(request.llm.provider_name()),
            request.llm.model_name(),
        )
        .context_window;

    let model_key = model_key(request.llm.as_ref());
    let shape_hash = request_shape_hash(
        request.prompt.system_prompt,
        request.prompt.tools_json,
        &model_key,
    );
    let factor = request
        .state
        .usage_calibrator
        .factor(&CalibrationKey::new(
            request.llm.provider_name(),
            request.llm.model_name(),
            "agent_loop",
            request.prompt.has_tools,
        ))
        .await;
    let tokens_before = project_context_tokens(
        &request.prompt,
        session,
        &request.request_only_messages,
        &model_key,
        &shape_hash,
        factor,
    );

    if reason == CompactionReason::Threshold
        && !should_compact(tokens_before, context_window, settings.reserve_tokens)
    {
        return Ok(CompactionOutcome::Unchanged);
    }

    let Some(preparation) = prepare_compaction(session, settings.keep_recent_tokens) else {
        info!(
            channel = %request.context.channel,
            chat_id = request.chat_id,
            reason = reason.as_str(),
            tokens_before,
            "context compaction found nothing to summarize"
        );
        return Ok(CompactionOutcome::Unchanged);
    };

    let started = Instant::now();
    let mut summary = build_summary(request, &preparation, instructions).await?;
    // Archive only once the summary exists: a failed attempt keeps the context
    // untruncated, so there is nothing to preserve yet.
    archive_before_compaction(request, session).await;

    let (read_files, modified_files) = preparation.file_operations.compute_lists();
    summary.push_str(&file_operations_suffix(&read_files, &modified_files));
    let summary = redact_summary_text(&summary, request.config);
    let kept_messages: Vec<Message> = session.messages[preparation.first_kept_index..].to_vec();
    let context = SessionContext {
        checkpoint: Some(CompactionCheckpoint {
            summary,
            tokens_before: tokens_before.try_into().unwrap_or(u64::MAX),
            read_files: redact_paths(read_files, request.config),
            modified_files: redact_paths(modified_files, request.config),
        }),
        messages: Arc::new(kept_messages),
        last_usage: None,
    };

    let tokens_after = clamp_tokens(calibrated_estimate(
        estimate_request_tokens(&request.prompt, &context, &request.request_only_messages) as i64,
        factor,
    ));
    info!(
        channel = %request.context.channel,
        chat_id = request.chat_id,
        reason = reason.as_str(),
        provider = request.llm.provider_name(),
        model = request.llm.model_name(),
        tokens_before,
        tokens_after,
        kept_messages = context.messages.len(),
        summarized_messages = preparation.messages_to_summarize.len()
            + preparation.turn_prefix_messages.len(),
        is_split_turn = preparation.is_split_turn,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "context compaction completed"
    );

    Ok(CompactionOutcome::Compacted(context))
}

/// Generates the checkpoint text.
async fn build_summary(
    request: &CompactionRequest<'_>,
    preparation: &CompactionPreparation,
    instructions: Option<&str>,
) -> Result<String, EgoPulseError> {
    if !preparation.is_split_turn {
        return generate_summary(
            request,
            &preparation.messages_to_summarize,
            preparation.previous_summary.as_deref(),
            instructions,
            "history",
        )
        .await;
    }

    let history = if preparation.messages_to_summarize.is_empty() {
        preparation
            .previous_summary
            .clone()
            .unwrap_or_else(|| NO_PRIOR_HISTORY.to_string())
    } else {
        generate_summary(
            request,
            &preparation.messages_to_summarize,
            preparation.previous_summary.as_deref(),
            instructions,
            "history",
        )
        .await?
    };

    let turn_prefix =
        generate_turn_prefix_summary(request, &preparation.turn_prefix_messages, instructions)
            .await?;

    Ok(format!("{history}{SPLIT_TURN_SEPARATOR}{turn_prefix}"))
}

fn redact_summary_text(text: &str, config: &crate::config::Config) -> String {
    let secrets = crate::tools::collect_config_secrets(config);
    sanitize_output_string(text, &secrets)
}

fn redact_paths(paths: Vec<String>, config: &crate::config::Config) -> Vec<String> {
    let secrets = crate::tools::collect_config_secrets(config);
    paths
        .into_iter()
        .map(|path| sanitize_output_string(&path, &secrets))
        .collect()
}

/// Writes the pre-compaction conversation to the archive directory.
///
/// Archive failures are reported but never abort compaction: the checkpoint is
/// the recovery path for the live context, and the append-only `messages` table
/// still holds the full history.
async fn archive_before_compaction(request: &CompactionRequest<'_>, session: &SessionContext) {
    let mut messages: Vec<Message> = Vec::with_capacity(session.messages.len() + 1);
    if let Some(checkpoint) = &session.checkpoint {
        messages.push(checkpoint_message(checkpoint));
    }
    messages.extend(session.messages.iter().cloned());

    let storage = request.state.storage_for(request.context.scope);
    let secrets = crate::tools::collect_config_secrets(request.config);
    let now = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let unique_suffix = uuid::Uuid::new_v4().simple();
    if let Err(error) = archive_conversation(
        &storage.archive_root,
        &request.context.channel,
        request.chat_id,
        &messages,
        &secrets,
        &format!("{now}-{unique_suffix}"),
    )
    .await
    {
        warn!(
            channel = %request.context.channel,
            chat_id = request.chat_id,
            error = %error,
            "context compaction archive failed"
        );
    }
}

/// Archives a conversation to disk as a Markdown file.
///
/// # Errors
///
/// Returns [`std::io::Error`] when the archive task cannot be joined or the
/// blocking archive fails.
pub(crate) async fn archive_conversation(
    groups_dir: &std::path::Path,
    channel: &str,
    chat_id: i64,
    messages: &[Message],
    secrets: &[(String, String)],
    idempotency_key: &str,
) -> std::io::Result<()> {
    let groups_dir = groups_dir.to_path_buf();
    let channel = channel.to_string();
    let messages: std::sync::Arc<[Message]> =
        std::sync::Arc::from(messages.to_vec().into_boxed_slice());
    let secrets: Vec<(String, String)> = secrets.to_vec();
    let idempotency_key = idempotency_key.to_string();
    let join_channel = channel.clone();
    let join_chat_id = chat_id;
    tokio::task::spawn_blocking(move || {
        archive_conversation_blocking(
            &groups_dir,
            &channel,
            chat_id,
            &messages,
            &secrets,
            &idempotency_key,
        )
    })
    .await
    .map_err(|e| {
        std::io::Error::other(format!(
            "archive task join failed for {join_channel}:{join_chat_id}: {e}"
        ))
    })
    .and_then(|inner| inner)
}

/// Archives a conversation to disk as a Markdown file, returning an error if
/// any I/O step fails so the caller can prevent data loss.
///
/// `idempotency_key` becomes the file name stem (`{key}.md`). A deterministic
/// key (e.g. `{run_id}-{chat_id}`) makes retry safe across crashes:
///
/// - If the archive file already exists it is **never overwritten**. After the
///   first successful archive the session is truncated, so a retry would read
///   the truncated snapshot and shrink the archive. Skipping protects the
///   original full content.
/// - The write goes to a temporary file first, then is atomically renamed to
///   the final path. A crash mid-write leaves no partial archive; the temp file
///   is simply overwritten on the next attempt.
///
/// # Errors
///
/// Returns [`std::io::Error`] if directory creation, the file write, or the
/// rename fails.
pub(crate) fn archive_conversation_blocking(
    groups_dir: &std::path::Path,
    channel: &str,
    chat_id: i64,
    messages: &[Message],
    secrets: &[(String, String)],
    idempotency_key: &str,
) -> std::io::Result<()> {
    let channel_dir = if channel.trim().is_empty() {
        "unknown"
    } else {
        channel.trim()
    };
    let dir = groups_dir
        .join(channel_dir)
        .join(chat_id.to_string())
        .join("conversations");

    std::fs::create_dir_all(&dir)?;

    let path = dir.join(format!("{idempotency_key}.md"));

    // Idempotent guard: never overwrite an existing archive. After the first
    // successful archive the session is truncated; a retry would read the
    // truncated snapshot and destroy the original full conversation.
    if path.exists() {
        info!(
            path = %path.display(),
            "archive already exists; skipping write to preserve original content"
        );
        return Ok(());
    }

    let mut content = String::new();
    for message in messages {
        let role = &message.role;
        let text = message_to_archive_text(message);
        let redacted = sanitize_output_string(&text, secrets);
        content.push_str(&format!("## {role}\n\n{redacted}\n\n---\n\n"));
    }

    // Atomic write: temp file → rename. A crash before the rename leaves no
    // partial archive at the final path, so the next attempt re-archives
    // cleanly.
    let tmp = dir.join(format!("{idempotency_key}.tmp"));
    std::fs::write(&tmp, &content)?;

    // Set file permissions to owner-only (0600) for security before rename.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(error) = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)) {
            warn!(
                "failed to set archive file permissions {}: {error}",
                tmp.display()
            );
        }
    }

    std::fs::rename(&tmp, path.clone())?;
    info!(
        "archived conversation ({} messages) to {}",
        messages.len(),
        path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_loop::test_support::RecordingProvider;
    use crate::llm::ToolCall;

    fn text(role: &str, content: &str) -> Message {
        Message::text(role, content)
    }

    fn tool_call(id: &str, name: &str, path: &str) -> Message {
        Message {
            role: "assistant".to_string(),
            content: MessageContent::text(""),
            reasoning_content: None,
            tool_calls: vec![ToolCall {
                id: id.to_string(),
                name: name.to_string(),
                arguments: serde_json::json!({ "path": path }),
                invalid_arguments: None,
            }],
            tool_call_id: None,
            finish_reason: None,
        }
    }

    fn tool_result(id: &str, body: &str) -> Message {
        Message {
            role: "tool".to_string(),
            content: MessageContent::text(body),
            reasoning_content: None,
            tool_calls: Vec::new(),
            tool_call_id: Some(id.to_string()),
            finish_reason: None,
        }
    }

    fn tool_result_status(id: &str, status: &str) -> Message {
        tool_result(
            id,
            &serde_json::json!({
                "tool": "write",
                "status": status,
                "result": "ok",
            })
            .to_string(),
        )
    }

    fn session(messages: Vec<Message>) -> SessionContext {
        SessionContext {
            checkpoint: None,
            messages: Arc::new(messages),
            last_usage: None,
        }
    }

    #[test]
    fn triggers_only_above_the_reserved_window() {
        assert!(!should_compact(100_000 - 16_384, 100_000, 16_384));
        assert!(should_compact(100_000 - 16_384 + 1, 100_000, 16_384));
    }

    #[test]
    fn zero_and_oversized_reserves_do_not_break_the_trigger() {
        // A zero reserve only leaves the context window itself.
        assert!(!should_compact(1_000, 1_000, 0));
        assert!(should_compact(1_001, 1_000, 0));

        // A reserve at or above the window leaves no headroom instead of
        // becoming a configuration error: any context triggers compaction.
        assert!(should_compact(1, 1_000, 1_000));
        assert!(should_compact(1, 1_000, 2_000));
        assert!(!should_compact(0, 1_000, 2_000));
    }

    #[test]
    fn zero_keep_recent_keeps_only_the_last_turn_boundary() {
        let messages = vec![
            text("user", "first question"),
            text("assistant", "first answer"),
            text("user", "second question"),
        ];

        let cut = find_cut_point(&messages, 0);

        assert_eq!(cut.first_kept_index, 2);
        assert!(prepare_compaction(&session(messages), 0).is_some());
    }

    #[test]
    fn keeps_the_whole_history_when_it_fits_the_budget() {
        let messages = vec![
            text("user", "hello"),
            text("assistant", "hi"),
            text("user", "how are you"),
        ];

        let cut = find_cut_point(&messages, 20_000);

        assert_eq!(cut.first_kept_index, 0);
        assert!(!cut.is_split_turn);
        assert!(prepare_compaction(&session(messages), 20_000).is_none());
    }

    #[test]
    fn cuts_at_a_user_message_and_keeps_the_budget() {
        let mut messages = Vec::new();
        for index in 0..10 {
            messages.push(text(
                "user",
                &format!("question {index} {}", "x".repeat(5_000)),
            ));
            messages.push(text(
                "assistant",
                &format!("answer {index} {}", "y".repeat(5_000)),
            ));
        }

        let cut = find_cut_point(&messages, 20_000);

        let kept: usize = messages[cut.first_kept_index..]
            .iter()
            .map(estimate_message_tokens)
            .sum();
        assert!(kept >= 20_000);
        assert_eq!(messages[cut.first_kept_index].role, "user");
        assert!(!cut.is_split_turn);
    }

    #[test]
    fn never_cuts_at_a_tool_result() {
        let mut messages = vec![text("user", &"a".repeat(60_000))];
        messages.push(tool_call("call_1", "read", "src/main.rs"));
        messages.push(tool_result("call_1", &"r".repeat(60_000)));
        messages.push(text("assistant", "done"));
        messages.push(text("user", "next"));

        let cut = find_cut_point(&messages, 10);

        assert_ne!(messages[cut.first_kept_index].role, "tool");
        assert!(
            cut.first_kept_index >= 1,
            "the retained range must not start before the tool call that owns it"
        );
    }

    #[test]
    fn splits_a_turn_when_a_single_turn_exceeds_the_budget() {
        let mut messages = vec![
            text("user", "older turn"),
            text("assistant", "older answer"),
        ];
        messages.push(text("user", &format!("big request {}", "x".repeat(60_000))));
        messages.push(tool_call("call_1", "read", "src/lib.rs"));
        messages.push(tool_result("call_1", &"r".repeat(60_000)));
        messages.push(text("assistant", "partial answer"));
        messages.push(text("assistant", &"z".repeat(60_000)));

        let cut = find_cut_point(&messages, 20_000);

        assert!(cut.is_split_turn);
        assert_eq!(cut.turn_start_index, Some(2));
        assert!(cut.first_kept_index > cut.turn_start_index.expect("turn start"));

        let preparation =
            prepare_compaction(&session(messages.clone()), 20_000).expect("preparation");
        assert_eq!(preparation.messages_to_summarize.len(), 2);
        assert!(!preparation.turn_prefix_messages.is_empty());
        assert_eq!(
            preparation.first_kept_index, cut.first_kept_index,
            "kept range must start at the cut"
        );
    }

    #[test]
    fn carries_previous_checkpoint_and_file_operations_forward() {
        let mut previous = session(vec![
            text("user", "old question"),
            text("assistant", &"old answer ".repeat(1_000)),
            text("user", "fresh question"),
        ]);
        previous.checkpoint = Some(CompactionCheckpoint {
            summary: "previous".to_string(),
            tokens_before: 10,
            read_files: vec!["README.md".to_string()],
            modified_files: vec!["src/main.rs".to_string()],
        });
        previous.messages = Arc::new(vec![
            tool_call("call_1", "read", "docs/config.md"),
            tool_result(
                "call_1",
                r#"{"tool":"read","status":"success","result":"ok"}"#,
            ),
            text("assistant", "answer"),
            text("user", &"b".repeat(60_000)),
            text("assistant", "reply"),
        ]);

        let preparation = prepare_compaction(&previous, 100).expect("preparation");

        assert_eq!(preparation.previous_summary.as_deref(), Some("previous"));
        let (read_files, modified_files) = preparation.file_operations.compute_lists();
        assert_eq!(
            read_files,
            vec!["README.md".to_string(), "docs/config.md".to_string()]
        );
        assert_eq!(modified_files, vec!["src/main.rs".to_string()]);
    }

    #[test]
    fn file_operations_record_possible_changes_and_skip_rejected_calls() {
        // A write or edit may change the file before an error is returned, so
        // an error result is still a modification. Calls rejected before
        // execution (invalid arguments) cannot have touched the file. Reads
        // count only when they succeeded.
        let mut rejected = tool_call("6", "edit", "/tmp/e");
        rejected.tool_calls[0].invalid_arguments = Some("truncated tool call".to_string());

        let mut operations = FileOperations::default();
        operations.extract_from(&[
            tool_call("1", "read", "/tmp/a"),
            tool_result_status("1", "success"),
            tool_call("2", "write", "/tmp/a"),
            tool_result_status("2", "success"),
            tool_call("3", "edit", "/tmp/b"),
            tool_result_status("3", "error"),
            tool_call("4", "read", "/tmp/c"),
            tool_result_status("4", "success"),
            tool_call("5", "write", "/tmp/d"),
            tool_result_status("5", "error"),
            rejected,
            tool_call("7", "read", "/tmp/f"),
            tool_result_status("7", "error"),
            // A later turn may reuse an id. The failure must not erase the
            // earlier success, and the later success must not apply to the
            // earlier failed call.
            text("user", "next turn"),
            tool_call("2", "write", "/tmp/old"),
            tool_result_status("2", "error"),
            tool_call("4", "read", "/tmp/later"),
            tool_result_status("4", "error"),
            tool_call("7", "read", "/tmp/new"),
            tool_result_status("7", "success"),
        ]);

        let (read_files, modified_files) = operations.compute_lists();

        assert_eq!(
            read_files,
            vec!["/tmp/c".to_string(), "/tmp/new".to_string()]
        );
        assert_eq!(
            modified_files,
            vec![
                "/tmp/a".to_string(),
                "/tmp/b".to_string(),
                "/tmp/d".to_string(),
                "/tmp/old".to_string()
            ]
        );
    }

    #[test]
    fn serializes_tool_results_with_a_bounded_length() {
        let long = "x".repeat(TOOL_RESULT_MAX_CHARS + 50);
        let messages = vec![
            text("user", "hello"),
            tool_call("call_1", "read", "src/main.rs"),
            tool_result(
                "call_1",
                &format!(r#"{{"tool":"read","status":"success","result":"{long}"}}"#),
            ),
        ];

        let serialized = serialize_conversation(&messages);

        assert!(serialized.starts_with("[User]: hello"));
        assert!(serialized.contains("[Assistant tool calls]: read("));
        assert!(serialized.contains("more characters truncated"));
        assert!(!serialized.contains(&"x".repeat(TOOL_RESULT_MAX_CHARS + 1)));
    }

    #[test]
    fn serialization_omits_reasoning() {
        let mut message = text("assistant", "answer");
        message.reasoning_content = Some("hidden chain of thought".to_string());

        let serialized = serialize_conversation(&[message]);

        assert_eq!(serialized, "[Assistant]: answer");
    }

    #[test]
    fn previous_file_blocks_are_stripped_before_the_update_prompt() {
        // Arrange: a previous checkpoint whose summary already ends with tags.
        let mut previous = session(vec![
            text("user", "old question"),
            text("assistant", &"old answer ".repeat(1_000)),
            text("user", "fresh question"),
        ]);
        previous.checkpoint = Some(CompactionCheckpoint {
            summary: "previous\n\n<read-files>\nsrc/a.rs\n</read-files>".to_string(),
            tokens_before: 10,
            read_files: vec!["src/a.rs".to_string()],
            modified_files: Vec::new(),
        });

        // Act: the summary passed to the model is stripped of the old blocks.
        let preparation = prepare_compaction(&previous, 100).expect("preparation");

        // Assert
        assert_eq!(preparation.previous_summary.as_deref(), Some("previous"));
    }

    #[test]
    fn update_prompt_places_the_previous_summary_separately() {
        let prompt = summary_prompt("[User]: hi", Some("previous summary"), None);

        assert!(prompt.contains("<conversation>\n[User]: hi\n</conversation>"));
        assert!(prompt.contains("<previous-summary>\nprevious summary\n</previous-summary>"));
        assert!(prompt.contains(UPDATE_SUMMARY_PROMPT));
        assert!(!prompt.contains(INITIAL_SUMMARY_PROMPT));
    }

    #[test]
    fn manual_instructions_are_appended_as_focus() {
        let prompt = summary_prompt("conv", None, Some("focus on decisions"));

        assert!(prompt.ends_with("Additional focus: focus on decisions"));
    }

    #[test]
    fn a_usage_anchor_is_used_until_the_request_shape_changes() {
        // Arrange
        let mut context = session(vec![
            text("user", &"one ".repeat(1_000)),
            text("assistant", &"two ".repeat(1_000)),
            text("user", &"three ".repeat(1_000)),
        ]);
        context.last_usage = Some(crate::agent_loop::session_snapshot::ContextUsageAnchor {
            after_message_index: 1,
            input_tokens: 1_000,
            output_tokens: 100,
            request_overhead_tokens: 2,
            model_key: "p/m".to_string(),
            request_shape_hash: "shape-a".to_string(),
        });
        let prompt = PromptContext {
            system_prompt: "system",
            tools_json: None,
            has_tools: false,
        };

        // Act
        let matched = project_context_tokens(&prompt, &context, &[], "p/m", "shape-a", 1.0);
        let reestimated = project_context_tokens(&prompt, &context, &[], "p/m", "shape-b", 1.0);

        // Assert
        let trailing = estimate_message_tokens(&context.messages[2]);
        assert_eq!(matched, 1_100 + trailing);
        assert!(reestimated > matched);

        // A provider can report an all-zero usage. It carries no measurement,
        // so projection falls back to estimating the whole request instead of
        // anchoring the context at zero.
        context.last_usage = Some(crate::agent_loop::session_snapshot::ContextUsageAnchor {
            after_message_index: 1,
            input_tokens: 0,
            output_tokens: 0,
            request_overhead_tokens: 2,
            model_key: "p/m".to_string(),
            request_shape_hash: "shape-a".to_string(),
        });
        let zero_anchor = project_context_tokens(&prompt, &context, &[], "p/m", "shape-a", 1.0);
        assert_eq!(zero_anchor, estimate_request_tokens(&prompt, &context, &[]));
    }

    #[test]
    fn request_only_messages_are_added_to_a_measured_anchor() {
        // Arrange: the anchor was measured without channel context, and the
        // next request prepends a large one.
        let mut context = session(vec![text("user", "hello"), text("assistant", "hi")]);
        context.last_usage = Some(crate::agent_loop::session_snapshot::ContextUsageAnchor {
            after_message_index: 1,
            input_tokens: 1_000,
            output_tokens: 100,
            request_overhead_tokens: 0,
            model_key: "p/m".to_string(),
            request_shape_hash: "shape".to_string(),
        });
        let prompt = PromptContext {
            system_prompt: "",
            tools_json: None,
            has_tools: false,
        };
        let channel_context = text("user", &"channel ".repeat(1_000));

        // Act
        let without = project_context_tokens(&prompt, &context, &[], "p/m", "shape", 1.0);
        let with =
            project_context_tokens(&prompt, &context, &[&channel_context], "p/m", "shape", 1.0);

        // Assert
        assert_eq!(with - without, estimate_message_tokens(&channel_context));
        assert!(should_compact(with, 2_000, 0));
    }

    fn summary_response(text: &str) -> MessagesResponse {
        MessagesResponse {
            content: text.to_string(),
            reasoning_content: None,
            tool_calls: Vec::new(),
            usage: None,
        }
    }

    async fn chat_id_for(state: &crate::runtime::AppState, context: &SurfaceContext) -> i64 {
        crate::agent_loop::session::resolve_chat_id(&state.turn_dependencies(), context)
            .await
            .expect("chat id")
    }

    fn seed_session(state: &crate::runtime::AppState, chat_id: i64, context: &SessionContext) {
        let snapshot =
            crate::agent_loop::session_snapshot::context_to_snapshot(&state.assets, context)
                .expect("snapshot");
        let json = serde_json::to_string(&snapshot).expect("json");
        state.db.save_session(chat_id, &json).expect("save session");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn manual_compaction_stores_a_checkpoint_and_keeps_the_recent_tail() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let provider =
            RecordingProvider::new(vec![Ok(summary_response("history summary"))], vec![0]);
        let config = crate::agent_loop::test_support::test_config_with_compaction(
            dir.path().to_string_lossy().to_string(),
            1,
        );
        let state =
            crate::agent_loop::test_support::build_state(config, Box::new(provider.clone()));
        let context = crate::agent_loop::test_support::cli_context("manual-compact");
        let chat_id = chat_id_for(&state, &context).await;
        // This history is far larger than the test model's 9,000-token window,
        // so the summary request itself must be bounded before it is sent.
        let huge_answer = "old answer ".repeat(40_000);
        let session = session(vec![
            text("user", "old question"),
            text("assistant", &huge_answer),
            text("user", "fresh question"),
        ]);
        seed_session(&state, chat_id, &session);
        let runtime = state.turn_dependencies();
        let llm: Arc<dyn LlmProvider> = Arc::new(provider.clone());
        let request = CompactionRequest {
            state: &runtime,
            context: &context,
            chat_id,
            llm: &llm,
            prompt: PromptContext {
                system_prompt: "system",
                tools_json: None,
                has_tools: false,
            },
            config: &state.config,
            request_only_messages: Vec::new(),
        };
        assert!(
            text_chars(&huge_answer) > summary_input_budget(&request) * CHARS_PER_TOKEN_ESTIMATE,
            "the test history must exceed the summarizer input budget"
        );

        // Act
        let outcome = compact(&request, &session, CompactionReason::Manual, None)
            .await
            .expect("compact");

        // Assert
        let CompactionOutcome::Compacted(compacted) = outcome else {
            panic!("expected a compacted context");
        };
        let checkpoint = compacted.checkpoint.as_ref().expect("checkpoint");
        assert_eq!(checkpoint.summary, "history summary");
        assert!(checkpoint.tokens_before > 0);
        assert_eq!(compacted.messages.len(), 1);
        assert_eq!(
            compacted.messages[0].content.as_text_lossy(),
            "fresh question"
        );
        assert!(compacted.last_usage.is_none());

        let seen_systems = provider.seen_systems();
        assert_eq!(seen_systems, vec![SUMMARIZATION_SYSTEM_PROMPT.to_string()]);
        let prompt = provider.seen_messages()[0][0].content.as_text_lossy();
        assert!(prompt.contains("<conversation>"));
        assert!(prompt.contains("[User]: old question"));
        assert!(prompt.contains("[Assistant]: old answer"));
        assert!(prompt.contains(SUMMARY_TRUNCATION_MARKER.trim()));
        assert!(prompt.contains(INITIAL_SUMMARY_PROMPT));
        assert!(
            summary_request_estimate(&prompt) <= summary_input_budget(&request),
            "the summary request must stay within the model input budget"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn repeated_compaction_updates_the_previous_checkpoint() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let provider =
            RecordingProvider::new(vec![Ok(summary_response("updated summary"))], vec![0]);
        let config = crate::agent_loop::test_support::test_config_with_compaction(
            dir.path().to_string_lossy().to_string(),
            1,
        );
        let state =
            crate::agent_loop::test_support::build_state(config, Box::new(provider.clone()));
        let context = crate::agent_loop::test_support::cli_context("update-compact");
        let chat_id = chat_id_for(&state, &context).await;
        let mut session = session(vec![
            text("user", "older question"),
            text("assistant", "older answer"),
            text("user", "newest question"),
        ]);
        session.checkpoint = Some(CompactionCheckpoint {
            summary: "previous summary".to_string(),
            tokens_before: 42,
            read_files: vec!["src/main.rs".to_string()],
            modified_files: Vec::new(),
        });
        let runtime = state.turn_dependencies();
        let llm: Arc<dyn LlmProvider> = Arc::new(provider.clone());
        let request = CompactionRequest {
            state: &runtime,
            context: &context,
            chat_id,
            llm: &llm,
            prompt: PromptContext {
                system_prompt: "system",
                tools_json: None,
                has_tools: false,
            },
            config: &state.config,
            request_only_messages: Vec::new(),
        };

        // Act
        let outcome = compact(
            &request,
            &session,
            CompactionReason::Manual,
            Some("focus on decisions"),
        )
        .await
        .expect("compact");

        // Assert
        let CompactionOutcome::Compacted(compacted) = outcome else {
            panic!("expected a compacted context");
        };
        let summary = &compacted.checkpoint.as_ref().expect("checkpoint").summary;
        assert!(summary.starts_with("updated summary"));
        assert!(summary.contains("<read-files>\nsrc/main.rs\n</read-files>"));
        let prompt = provider.seen_messages()[0][0].content.as_text_lossy();
        assert!(prompt.contains("<previous-summary>\nprevious summary\n</previous-summary>"));
        assert!(prompt.contains(UPDATE_SUMMARY_PROMPT));
        assert!(prompt.contains("Additional focus: focus on decisions"));
        assert!(!prompt.contains(INITIAL_SUMMARY_PROMPT));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn summarization_failure_is_returned_to_the_caller() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![Err(LlmError::ApiError {
                status: reqwest::StatusCode::BAD_REQUEST,
                body_preview: "invalid request".to_string(),
                retry_after_secs: None,
            })],
            vec![0],
        );
        let config = crate::agent_loop::test_support::test_config_with_compaction(
            dir.path().to_string_lossy().to_string(),
            1,
        );
        let state =
            crate::agent_loop::test_support::build_state(config, Box::new(provider.clone()));
        let context = crate::agent_loop::test_support::cli_context("failed-compact");
        let chat_id = chat_id_for(&state, &context).await;
        let session = session(vec![
            text("user", "old question"),
            text("assistant", "old answer"),
            text("user", "fresh question"),
        ]);
        let runtime = state.turn_dependencies();
        let llm: Arc<dyn LlmProvider> = Arc::new(provider.clone());
        let request = CompactionRequest {
            state: &runtime,
            context: &context,
            chat_id,
            llm: &llm,
            prompt: PromptContext {
                system_prompt: "system",
                tools_json: None,
                has_tools: false,
            },
            config: &state.config,
            request_only_messages: Vec::new(),
        };

        // Act
        let result = compact(&request, &session, CompactionReason::Manual, None).await;

        // Assert
        assert!(matches!(result, Err(EgoPulseError::Llm(_))));
        assert_eq!(provider.seen_systems().len(), 1, "no retry on a 400");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn a_split_turn_adds_a_turn_prefix_summary() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![
                Ok(summary_response("history summary")),
                Ok(summary_response("turn prefix summary")),
            ],
            vec![0, 0],
        );
        let config = crate::agent_loop::test_support::test_config_with_compaction(
            dir.path().to_string_lossy().to_string(),
            1,
        );
        let state =
            crate::agent_loop::test_support::build_state(config, Box::new(provider.clone()));
        let context = crate::agent_loop::test_support::cli_context("split-compact");
        let chat_id = chat_id_for(&state, &context).await;
        let mut messages = vec![
            text("user", "old question"),
            text("assistant", "old answer"),
            text("user", "big request"),
        ];
        messages.push(tool_call("call_1", "read", "src/lib.rs"));
        messages.push(tool_result(
            "call_1",
            r#"{"tool":"read","status":"success","result":"contents"}"#,
        ));
        messages.push(text("assistant", &"partial ".repeat(50)));
        let session = session(messages);
        let runtime = state.turn_dependencies();
        let llm: Arc<dyn LlmProvider> = Arc::new(provider.clone());
        let request = CompactionRequest {
            state: &runtime,
            context: &context,
            chat_id,
            llm: &llm,
            prompt: PromptContext {
                system_prompt: "system",
                tools_json: None,
                has_tools: false,
            },
            config: &state.config,
            request_only_messages: Vec::new(),
        };

        // Act
        let outcome = compact(&request, &session, CompactionReason::Manual, None)
            .await
            .expect("compact");

        // Assert
        let CompactionOutcome::Compacted(compacted) = outcome else {
            panic!("expected a compacted context");
        };
        let summary = &compacted.checkpoint.as_ref().expect("checkpoint").summary;
        assert!(summary.contains("history summary"));
        assert!(summary.contains(SPLIT_TURN_SEPARATOR));
        assert!(summary.contains("turn prefix summary"));
        assert!(summary.contains("<read-files>\nsrc/lib.rs\n</read-files>"));
        assert_eq!(provider.seen_systems().len(), 2);
        let turn_prefix_prompt = provider.seen_messages()[1][0].content.as_text_lossy();
        assert!(turn_prefix_prompt.contains("# Conversation"));
        assert!(turn_prefix_prompt.contains(TURN_PREFIX_SUMMARY_PROMPT));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn automatic_compaction_respects_the_disabled_setting() {
        // Arrange: one response for the manual compaction below; any automatic
        // summarization call would consume it and fail the assertion.
        let dir = tempfile::tempdir().expect("tempdir");
        let provider =
            RecordingProvider::new(vec![Ok(summary_response("manual summary"))], vec![0]);
        let mut config = crate::agent_loop::test_support::test_config_with_compaction(
            dir.path().to_string_lossy().to_string(),
            1,
        );
        config.compaction.enabled = false;
        let state =
            crate::agent_loop::test_support::build_state(config, Box::new(provider.clone()));
        let context = crate::agent_loop::test_support::cli_context("disabled-compact");
        let chat_id = chat_id_for(&state, &context).await;
        let session = session(vec![
            text("user", &"x".repeat(100_000)),
            text("user", "fresh question"),
        ]);
        let runtime = state.turn_dependencies();
        let llm: Arc<dyn LlmProvider> = Arc::new(provider.clone());
        let request = CompactionRequest {
            state: &runtime,
            context: &context,
            chat_id,
            llm: &llm,
            prompt: PromptContext {
                system_prompt: "system",
                tools_json: None,
                has_tools: false,
            },
            config: &state.config,
            request_only_messages: Vec::new(),
        };

        // Act: the automatic path stays off.
        let outcome = compact_if_needed(&request, &session, None)
            .await
            .expect("compact");

        // Assert
        assert!(matches!(outcome, CompactionOutcome::Unchanged));
        assert!(provider.seen_systems().is_empty());

        // Act: the manual path still runs while automatic compaction is off.
        let outcome = compact(&request, &session, CompactionReason::Manual, None)
            .await
            .expect("compact");

        // Assert
        assert!(matches!(outcome, CompactionOutcome::Compacted(_)));
        assert_eq!(provider.seen_systems().len(), 1);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn automatic_compaction_skips_a_context_that_still_fits() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(Vec::new(), Vec::new());
        let config = crate::agent_loop::test_support::test_config_with_compaction(
            dir.path().to_string_lossy().to_string(),
            1,
        );
        let state =
            crate::agent_loop::test_support::build_state(config, Box::new(provider.clone()));
        let context = crate::agent_loop::test_support::cli_context("small-compact");
        let chat_id = chat_id_for(&state, &context).await;
        let session = session(vec![text("user", "hello"), text("assistant", "hi")]);
        let runtime = state.turn_dependencies();
        let llm: Arc<dyn LlmProvider> = Arc::new(provider.clone());
        let request = CompactionRequest {
            state: &runtime,
            context: &context,
            chat_id,
            llm: &llm,
            prompt: PromptContext {
                system_prompt: "system",
                tools_json: None,
                has_tools: false,
            },
            config: &state.config,
            request_only_messages: Vec::new(),
        };

        // Act
        let outcome = compact_if_needed(&request, &session, None)
            .await
            .expect("compact");

        // Assert
        assert!(matches!(outcome, CompactionOutcome::Unchanged));
        assert!(provider.seen_systems().is_empty());
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn automatic_compaction_triggers_above_the_window() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(vec![Ok(summary_response("summary"))], vec![0]);
        let config = crate::agent_loop::test_support::test_config_with_compaction_window(
            dir.path().to_string_lossy().to_string(),
            100,
            2_000,
        );
        let state =
            crate::agent_loop::test_support::build_state(config, Box::new(provider.clone()));
        let context = crate::agent_loop::test_support::cli_context("trigger-compact");
        let chat_id = chat_id_for(&state, &context).await;
        let session = session(vec![
            text("user", "earlier question"),
            text("user", &"x".repeat(20_000)),
            text("assistant", "answer"),
            text("user", "fresh question"),
        ]);
        let runtime = state.turn_dependencies();
        let llm: Arc<dyn LlmProvider> = Arc::new(provider.clone());
        let request = CompactionRequest {
            state: &runtime,
            context: &context,
            chat_id,
            llm: &llm,
            prompt: PromptContext {
                system_prompt: "system",
                tools_json: None,
                has_tools: false,
            },
            config: &state.config,
            request_only_messages: Vec::new(),
        };

        // Act
        let outcome = compact_if_needed(&request, &session, None)
            .await
            .expect("compact");

        // Assert
        let CompactionOutcome::Compacted(compacted) = outcome else {
            panic!("expected a compacted context");
        };
        assert!(compacted.checkpoint.is_some());
        assert_eq!(provider.seen_systems().len(), 1);
    }

    #[test]
    fn a_measured_context_is_not_rescaled_when_the_request_is_unchanged() {
        // Arrange: the anchor describes exactly the current request overhead.
        let mut context = session(vec![text("user", "hello"), text("assistant", "hi")]);
        let prompt = PromptContext {
            system_prompt: "system prompt",
            tools_json: Some("[{\"name\":\"read\"}]"),
            has_tools: true,
        };
        context.last_usage = Some(crate::agent_loop::session_snapshot::ContextUsageAnchor {
            after_message_index: 1,
            input_tokens: 1_000,
            output_tokens: 100,
            request_overhead_tokens: estimate_request_overhead(&prompt, &context, &[]),
            model_key: "p/m".to_string(),
            request_shape_hash: "shape".to_string(),
        });

        // Act & Assert: the calibration factor only scales the delta.
        assert_eq!(
            project_context_tokens(&prompt, &context, &[], "p/m", "shape", 0.75),
            1_100
        );
        assert_eq!(
            project_context_tokens(&prompt, &context, &[], "p/m", "shape", 2.0),
            1_100
        );
    }

    #[test]
    fn a_smaller_request_overhead_reduces_the_projection() {
        // Arrange: the measured request carried a much larger channel context.
        let mut context = session(vec![text("user", "hello"), text("assistant", "hi")]);
        let prompt = PromptContext {
            system_prompt: "",
            tools_json: None,
            has_tools: false,
        };
        let larger_overhead = estimate_request_overhead(&prompt, &context, &[]) + 500;
        context.last_usage = Some(crate::agent_loop::session_snapshot::ContextUsageAnchor {
            after_message_index: 1,
            input_tokens: 1_000,
            output_tokens: 100,
            request_overhead_tokens: larger_overhead,
            model_key: "p/m".to_string(),
            request_shape_hash: "shape".to_string(),
        });

        // Act
        let projected = project_context_tokens(&prompt, &context, &[], "p/m", "shape", 1.0);

        // Assert
        assert_eq!(projected, 1_100 - 500);
    }
}
