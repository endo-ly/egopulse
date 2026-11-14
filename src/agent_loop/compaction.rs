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
    estimate_text_tokens,
};
use crate::llm::{LlmProvider, Message, MessageContent, MessageContentPart, MessagesResponse};
use crate::llm::{RetryPolicy, complete_response};
use crate::tools::sanitize_output_string;

const TOOL_RESULT_MAX_CHARS: usize = 2_000;

/// Separator between the history summary and a split-turn prefix summary.
const SPLIT_TURN_SEPARATOR: &str = "\n\n---\n\n**Turn Context (split turn):**\n\n";

/// Text used when a split turn has no summarized history before it.
const NO_PRIOR_HISTORY: &str = "No prior history.";

/// One summarization request shares the LLM client's own request ceiling.
///
/// Retries, backoff, and `Retry-After` waits are bounded by this deadline, so a
/// provider that keeps failing cannot stretch one compaction indefinitely.
const SUMMARY_REQUEST_DEADLINE: Duration = Duration::from_secs(600);

/// Calibration bucket for the summary request input estimate.
const SUMMARY_REQUEST_KIND: &str = "compaction";

/// Number of times a summary request is retried after a provider overflow.
///
/// The first attempt uses the calibrated estimate. A provider-side rejection
/// means the estimate was too low, so each retry halves the input budget.
const SUMMARY_OVERFLOW_RETRIES: usize = 2;

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
            .map(|checkpoint| checkpoint.summary.clone()),
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

/// Holds labeled conversation text for summary input.
///
/// Visible text, tool calls, and tool results are kept; provider reasoning is
/// omitted because it is often not part of the model's live context.
struct ConversationPart {
    label: String,
    body: String,
}

impl ConversationPart {
    fn new(label: &str, body: String) -> Self {
        Self {
            label: label.to_string(),
            body,
        }
    }
}

fn conversation_parts(messages: &[Message]) -> Vec<ConversationPart> {
    let mut parts = Vec::new();
    for message in messages {
        match message.role.as_str() {
            "user" => {
                let content = content_text(&message.content);
                if !content.trim().is_empty() {
                    parts.push(ConversationPart::new("User", content));
                }
            }
            "assistant" => {
                let content = content_text(&message.content);
                if !content.trim().is_empty() {
                    parts.push(ConversationPart::new("Assistant", content));
                }
                if !message.tool_calls.is_empty() {
                    let calls = message
                        .tool_calls
                        .iter()
                        .map(|tool_call| format!("{}({})", tool_call.name, tool_call.arguments))
                        .collect::<Vec<_>>()
                        .join("; ");
                    parts.push(ConversationPart::new("Assistant tool calls", calls));
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
                let total = body.chars().count();
                let body = if total > TOOL_RESULT_MAX_CHARS {
                    format!(
                        "{}\n\n[... {} more characters truncated]",
                        body.chars().take(TOOL_RESULT_MAX_CHARS).collect::<String>(),
                        total - TOOL_RESULT_MAX_CHARS
                    )
                } else {
                    body
                };
                parts.push(ConversationPart::new(label, body));
            }
            _ => {}
        }
    }
    parts
}

/// Returns the raw token budget for one summarization request.
///
/// The summarizer needs room for its own answer. Reserve the configured
/// output limit, bounded by half the context window so a model whose output
/// limit exceeds its window still leaves room for the history.
///
/// The result is in raw estimate units, not provider tokens. The compaction
/// calibration factor converts between the two. Without a measurement, the
/// conservative default factor leaves headroom for text that tokenizes denser
/// than the UTF-16 estimate; a provider overflow is still retried with a
/// smaller budget by the caller.
fn summary_calibration_key(request: &CompactionRequest<'_>) -> CalibrationKey {
    CalibrationKey::new(
        request.llm.provider_name(),
        request.llm.model_name(),
        SUMMARY_REQUEST_KIND,
        false,
    )
}

async fn summary_input_budget(request: &CompactionRequest<'_>) -> usize {
    let metadata = request.config.resolve_model_metadata(
        &crate::config::ProviderId::new(request.llm.provider_name()),
        request.llm.model_name(),
    );
    let output_reserve = metadata.max_output_tokens.min(metadata.context_window / 2);
    let token_budget = metadata
        .context_window
        .saturating_sub(output_reserve)
        .max(1);
    let factor = request
        .state
        .usage_calibrator
        .factor(&summary_calibration_key(request))
        .await;
    (((token_budget as f64) / factor).floor() as usize)
        .max(1)
        .min(token_budget)
}

#[derive(Clone, Copy, Default)]
struct SummaryCursor {
    index: usize,
    offset: usize,
}

struct SummaryChunk {
    prompt: String,
    next: SummaryCursor,
}

/// Fits complete messages first, splitting only a message that cannot fit by itself.
fn summary_chunk<F>(
    budget: usize,
    units: &[ConversationPart],
    cursor: SummaryCursor,
    previous: Option<&str>,
    instructions: Option<&str>,
    build: F,
) -> Result<SummaryChunk, EgoPulseError>
where
    F: Fn(&str, Option<&str>, Option<&str>) -> String,
{
    let fixed = build("", previous, instructions);
    let fixed_chars = crate::llm::tokens::text_chars(&fixed)
        + crate::llm::tokens::text_chars(SUMMARIZATION_SYSTEM_PROMPT);
    let capacity = budget
        .saturating_mul(CHARS_PER_TOKEN_ESTIMATE)
        .saturating_sub(fixed_chars);
    let mut chunk = String::new();
    let mut used = 0;
    let mut pos = cursor.index;
    let mut start = cursor.offset;
    while let Some(ConversationPart { label, body }) = units.get(pos) {
        let prefix = if start == 0 {
            format!("[{label}]: ")
        } else {
            format!("[{label} continued]: ")
        };
        let separator = if chunk.is_empty() { "" } else { "\n\n" };
        let remaining = &body[start..];
        let overhead =
            crate::llm::tokens::text_chars(separator) + crate::llm::tokens::text_chars(&prefix);
        let available = capacity.saturating_sub(used + overhead);
        let mut chars = 0;
        let mut end = 0;
        for (i, ch) in remaining.char_indices() {
            if chars + ch.len_utf16() > available {
                break;
            }
            chars += ch.len_utf16();
            end = i + ch.len_utf8();
        }
        if end == remaining.len() && used + overhead <= capacity {
            chunk.push_str(separator);
            chunk.push_str(&prefix);
            chunk.push_str(remaining);
            used += overhead + chars;
            pos += 1;
            start = 0;
            continue;
        }
        if !chunk.is_empty() {
            break;
        }
        if end == 0 {
            return Err(EgoPulseError::Llm(LlmError::SummaryInputCapacityExceeded {
                budget_tokens: budget,
                required_tokens: summary_request_estimate(&fixed) + 1,
            }));
        }
        chunk.push_str(&prefix);
        chunk.push_str(&remaining[..end]);
        start += end;
        break;
    }
    if units.is_empty() && summary_request_estimate(&fixed) > budget {
        return Err(EgoPulseError::Llm(LlmError::SummaryInputCapacityExceeded {
            budget_tokens: budget,
            required_tokens: summary_request_estimate(&fixed),
        }));
    }
    Ok(SummaryChunk {
        prompt: build(&chunk, previous, instructions),
        next: SummaryCursor {
            index: pos,
            offset: start,
        },
    })
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
            request_kind: SUMMARY_REQUEST_KIND,
            estimated_tokens,
            has_tools: false,
            cached_input_tokens,
        })
    })
    .await
    .inspect_err(|error| warn!(error = %error, "compaction usage logging failed"));
}

/// Sends summarization requests until one succeeds, shrinking the input after
/// a provider overflow, then records usage and validates the result.
///
/// Every final failure is returned: the caller decides whether the original
/// context is kept (automatic compaction) or reported to the user (`/compact`).
/// A provider overflow is not final until the retry budget is exhausted or the
/// input cannot shrink further.
async fn summarize<F>(
    request: &CompactionRequest<'_>,
    label: &str,
    build_prompt: F,
) -> Result<(String, SummaryCursor), EgoPulseError>
where
    F: Fn(usize) -> Result<SummaryChunk, EgoPulseError>,
{
    let mut budget = summary_input_budget(request).await;
    let mut overflow_retries = 0usize;
    loop {
        let chunk = build_prompt(budget)?;
        let estimate = summary_request_estimate(&chunk.prompt);
        let response = match request_summary(request.llm, chunk.prompt).await {
            Ok(response) => response,
            Err(error) => {
                // If the prompt is already far below the budget, halving the
                // budget alone could rebuild an identical request. Cap the
                // retry at half the current estimate as well.
                let next_budget = (budget / 2).min(estimate / 2).max(1);
                if error.is_context_overflow()
                    && overflow_retries < SUMMARY_OVERFLOW_RETRIES
                    && next_budget < budget
                {
                    overflow_retries += 1;
                    warn!(
                        channel = %request.context.channel,
                        chat_id = request.chat_id,
                        label,
                        attempt = overflow_retries,
                        previous_budget_tokens = budget,
                        next_budget_tokens = next_budget,
                        "context compaction summary exceeded the provider window; retrying with a smaller input"
                    );
                    budget = next_budget;
                    continue;
                }
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
                    summary_calibration_key(request),
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

        return validate_summary(&response)
            .map(|summary| (summary, chunk.next))
            .inspect_err(|error| {
                warn!(
                    channel = %request.context.channel,
                    chat_id = request.chat_id,
                    label,
                    error = %error,
                    "context compaction summary rejected"
                );
            });
    }
}

/// Makes room for the next input by treating an oversized checkpoint as history.
fn prepare_previous_summary(
    units: &mut Vec<ConversationPart>,
    cursor: &mut SummaryCursor,
    previous: &mut Option<String>,
    instructions: Option<&str>,
    budget: usize,
    recovery_attempts: &mut usize,
) -> Result<(), EgoPulseError> {
    let Some(prior) = previous.as_ref() else {
        return Ok(());
    };
    let fixed = summary_prompt("", Some(prior), instructions);
    if summary_request_estimate(&summary_prompt(
        "[Previous checkpoint continued]: x",
        Some(prior),
        instructions,
    )) <= budget
    {
        return Ok(());
    }
    if *recovery_attempts >= 3 {
        return Err(EgoPulseError::Llm(LlmError::SummaryInputCapacityExceeded {
            budget_tokens: budget,
            required_tokens: summary_request_estimate(&fixed),
        }));
    }
    if cursor.offset > 0 {
        let part = &mut units[cursor.index];
        part.label.push_str(" continued");
        part.body = part.body[cursor.offset..].to_string();
    }
    units.insert(
        cursor.index,
        ConversationPart::new("Previous checkpoint", previous.take().unwrap()),
    );
    cursor.offset = 0;
    *recovery_attempts += 1;
    Ok(())
}

async fn summarize_conversation(
    request: &CompactionRequest<'_>,
    messages: &[Message],
    previous_summary: Option<&str>,
    instructions: Option<&str>,
    label: &str,
    turn_prefix: bool,
) -> Result<String, EgoPulseError> {
    let mut units = conversation_parts(messages);
    for part in &mut units {
        part.body = redact_summary_text(&part.body, request.config);
    }
    let instructions = instructions.map(|text| redact_summary_text(text, request.config));
    let mut previous = previous_summary.map(|text| redact_summary_text(text, request.config));
    let mut cursor = SummaryCursor::default();
    let mut recovery_attempts = 0;
    loop {
        prepare_previous_summary(
            &mut units,
            &mut cursor,
            &mut previous,
            instructions.as_deref(),
            summary_input_budget(request).await,
            &mut recovery_attempts,
        )?;
        let (summary, next) = summarize(request, label, |budget| {
            summary_chunk(
                budget,
                &units,
                cursor,
                previous.as_deref(),
                instructions.as_deref(),
                |chunk, prior, focus| {
                    if turn_prefix {
                        let input = match prior {
                            Some(prior) => format!(
                                "<previous-summary>\n{prior}\n</previous-summary>\n\n{chunk}"
                            ),
                            None => chunk.to_string(),
                        };
                        turn_prefix_prompt(&input, focus)
                    } else {
                        summary_prompt(chunk, prior, focus)
                    }
                },
            )
        })
        .await?;
        cursor = next;
        if cursor.index == units.len() {
            return Ok(summary);
        }
        previous = Some(summary);
    }
}

async fn generate_summary(
    request: &CompactionRequest<'_>,
    messages: &[Message],
    previous_summary: Option<&str>,
    instructions: Option<&str>,
    label: &str,
) -> Result<String, EgoPulseError> {
    summarize_conversation(
        request,
        messages,
        previous_summary,
        instructions,
        label,
        false,
    )
    .await
}

async fn generate_turn_prefix_summary(
    request: &CompactionRequest<'_>,
    messages: &[Message],
    instructions: Option<&str>,
) -> Result<String, EgoPulseError> {
    summarize_conversation(request, messages, None, instructions, "turn prefix", true).await
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
    let summary = tokio::time::timeout(
        SUMMARY_REQUEST_DEADLINE,
        build_summary(request, &preparation, instructions),
    )
    .await
    .map_err(|_| EgoPulseError::Llm(LlmError::DeadlineExceeded))??;
    let (read_files, modified_files) = preparation.file_operations.compute_lists();
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
    let mut projected_messages =
        Vec::with_capacity(context.messages.len() + 1 + request.request_only_messages.len());
    let checkpoint = context.checkpoint.as_ref().map(checkpoint_message);
    if let Some(checkpoint) = &checkpoint {
        projected_messages.push(checkpoint);
    }
    projected_messages.extend(context.messages.iter());
    projected_messages.extend(request.request_only_messages.iter().copied());
    // Validate the projected request with the provider's own sizing so a
    // checkpoint is never saved for a request the next model call would reject.
    // This uses the raw prompt estimate and the resolved output limit instead
    // of the calibrated projection used for trigger/logging.
    request.llm.request_output_limit(
        request.prompt.system_prompt,
        request.prompt.tools_json,
        &projected_messages,
    )?;
    archive_before_compaction(request, session).await;
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
    use crate::agent_loop::test_support::{RecordingProvider, TestOutputLimits};
    use crate::llm::ToolCall;
    use crate::llm::tokens::text_chars;

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
    fn serializes_tool_results_with_a_character_limit() {
        let long = "x".repeat(2050);
        let messages = vec![
            text("user", "hello"),
            tool_call("call_1", "read", "src/main.rs"),
            tool_result(
                "call_1",
                &format!(r#"{{"tool":"read","status":"success","result":"{long}"}}"#),
            ),
        ];

        let serialized = conversation_parts(&messages)
            .iter()
            .map(|part| format!("[{}]: {}", part.label, part.body))
            .collect::<Vec<_>>()
            .join("\n\n");

        assert!(serialized.starts_with("[User]: hello"));
        assert!(serialized.contains("[Assistant tool calls]: read("));
        assert!(serialized.contains(&"x".repeat(2_000)));
        assert!(serialized.contains("50 more characters truncated"));
    }

    #[test]
    fn serialization_omits_reasoning() {
        let mut message = text("assistant", "answer");
        message.reasoning_content = Some("hidden chain of thought".to_string());

        let serialized = conversation_parts(&[message])
            .iter()
            .map(|part| format!("[{}]: {}", part.label, part.body))
            .collect::<Vec<_>>()
            .join("\n\n");

        assert_eq!(serialized, "[Assistant]: answer");
    }

    #[test]
    fn summary_chunks_consume_each_message_once() {
        // Arrange
        let units = vec![
            ConversationPart {
                label: "User".into(),
                body: "first".into(),
            },
            ConversationPart {
                label: "Assistant".into(),
                body: "abc😀".repeat(100),
            },
            ConversationPart {
                label: "User".into(),
                body: "last".into(),
            },
        ];
        let mut cursor = SummaryCursor::default();
        let mut reconstructed = vec![String::new(); units.len()];

        // Act
        while cursor.index < units.len() {
            let chunk = summary_chunk(100, &units, cursor, None, None, |text, _, _| {
                text.to_string()
            })
            .unwrap();
            for (index, part) in (cursor.index..).zip(chunk.prompt.split("\n\n")) {
                let (label, body) = part.split_once("]: ").unwrap();
                let expected = if index == cursor.index && cursor.offset > 0 {
                    format!("[{} continued", units[index].label)
                } else {
                    format!("[{}", units[index].label)
                };
                assert_eq!(label, expected);
                reconstructed[index].push_str(body);
            }
            assert!(chunk.next.index > cursor.index || chunk.next.offset > cursor.offset);
            cursor = chunk.next;
        }

        // Assert
        assert_eq!(
            reconstructed,
            units
                .iter()
                .map(|unit| unit.body.clone())
                .collect::<Vec<_>>()
        );
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

    struct CompactionFixture {
        state: crate::runtime::AppState,
        context: SurfaceContext,
        runtime: TurnDependencies,
        llm: Arc<dyn LlmProvider>,
        chat_id: i64,
    }

    impl CompactionFixture {
        async fn new(
            config: crate::config::Config,
            provider: &RecordingProvider,
            session_id: &str,
        ) -> Self {
            let state =
                crate::agent_loop::test_support::build_state(config, Box::new(provider.clone()));
            let context = crate::agent_loop::test_support::cli_context(session_id);
            let chat_id = chat_id_for(&state, &context).await;
            let runtime = state.turn_dependencies();
            let llm: Arc<dyn LlmProvider> = Arc::new(provider.clone());
            Self {
                state,
                context,
                runtime,
                llm,
                chat_id,
            }
        }

        fn request(&self) -> CompactionRequest<'_> {
            CompactionRequest {
                state: &self.runtime,
                context: &self.context,
                chat_id: self.chat_id,
                llm: &self.llm,
                prompt: PromptContext {
                    system_prompt: "system",
                    tools_json: None,
                    has_tools: false,
                },
                config: &self.state.config,
                request_only_messages: Vec::new(),
            }
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn builds_a_checkpoint_and_keeps_the_recent_tail() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            (0..10)
                .map(|_| Ok(summary_response("history summary")))
                .collect(),
            vec![0; 10],
        )
        .with_output_limits(TestOutputLimits {
            context_window: 9_000,
            max_output_tokens: 4_096,
            minimum_output_tokens: 1,
        });
        let mut config = crate::agent_loop::test_support::test_config_with_compaction(
            dir.path().to_string_lossy().to_string(),
            1,
        );
        let secret = "cfg-secret-4f9d2a7c";
        config
            .providers
            .get_mut(&crate::config::ProviderId::new("openai"))
            .expect("openai provider")
            .api_key = Some(crate::config::secret_ref::ResolvedValue::Literal(
            secret.to_string(),
        ));
        // This history is far larger than the test model's 9,000-token window,
        // so the summary request itself must be bounded before it is sent.
        let huge_answer = "old answer ".repeat(3_000);
        let correction = format!(
            "The old plan is withdrawn. New decision: keep the newest text. API key: {secret}"
        );
        let session = session(vec![
            text("user", "old question"),
            text("assistant", &huge_answer),
            text("user", &correction),
            text("user", "fresh question"),
        ]);
        let fixture = CompactionFixture::new(config, &provider, "manual-compact").await;
        let request = fixture.request();
        let input_budget = summary_input_budget(&request).await;
        assert!(
            text_chars(&huge_answer) > input_budget * CHARS_PER_TOKEN_ESTIMATE,
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
        assert!(seen_systems.len() > 1);
        assert!(
            seen_systems
                .iter()
                .all(|system| system == SUMMARIZATION_SYSTEM_PROMPT)
        );
        let prompt = provider.seen_messages()[0][0].content.as_text_lossy();
        assert!(prompt.contains("<conversation>"));
        assert!(prompt.contains("old question"));
        let all_prompts = provider.seen_messages();
        let mut assistant_chunks = 0;
        for (index, messages) in all_prompts.iter().enumerate() {
            let prompt = messages[0].content.as_text_lossy();
            if prompt.contains("[Assistant]: ") || prompt.contains("[Assistant continued]: ") {
                assistant_chunks += 1;
            }
            if index > 0 {
                assert!(
                    prompt.contains("<previous-summary>\nhistory summary\n</previous-summary>")
                );
            }
            assert!(summary_request_estimate(&prompt) <= input_budget);
            assert!(!prompt.contains(secret));
        }
        assert!(
            assistant_chunks >= 2,
            "split messages retain their role and continuation"
        );
        assert!(all_prompts.iter().any(|messages| {
            messages[0]
                .content
                .as_text_lossy()
                .contains("[Assistant continued]: ")
        }));
        let last = all_prompts.last().unwrap()[0].content.as_text_lossy();
        assert!(last.contains("The old plan is withdrawn."));
        assert!(last.contains("[REDACTED:provider.openai.api_key]"));
        assert!(prompt.contains(INITIAL_SUMMARY_PROMPT));

        // An oversized retained message cannot produce a usable checkpoint.
        let oversized = SessionContext {
            checkpoint: None,
            messages: Arc::new(vec![
                text("user", "old question"),
                text("user", &"x".repeat(100_000)),
            ]),
            last_usage: None,
        };
        let error = compact(&request, &oversized, CompactionReason::Manual, None)
            .await
            .expect_err("retained context exceeds the window");
        assert!(matches!(
            error,
            EgoPulseError::Llm(LlmError::OutputCapacityExceeded { .. })
        ));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn a_configured_output_limit_below_the_api_minimum_keeps_the_context() {
        // Arrange: the configured output limit is below the API minimum used
        // by the test provider.
        let dir = tempfile::tempdir().expect("tempdir");
        let provider =
            RecordingProvider::new(vec![Ok(summary_response("history summary"))], vec![0])
                .with_output_limits(TestOutputLimits {
                    context_window: 9_000,
                    max_output_tokens: 8,
                    minimum_output_tokens: 16,
                });
        let config = crate::agent_loop::test_support::test_config_with_compaction(
            dir.path().to_string_lossy().to_string(),
            1,
        );
        let fixture = CompactionFixture::new(config, &provider, "below-minimum").await;
        let request = fixture.request();
        let session = session(vec![
            text("user", "old question"),
            text("assistant", "old answer"),
            text("user", "fresh question"),
        ]);

        // Act
        let error = compact(&request, &session, CompactionReason::Manual, None)
            .await
            .expect_err("the configured output limit is below the API minimum");

        // Assert
        assert!(matches!(
            error,
            EgoPulseError::Llm(LlmError::OutputLimitBelowApiMinimum {
                configured: 8,
                minimum: 16,
            })
        ));
        assert_eq!(
            provider.seen_systems().len(),
            1,
            "the summary succeeds before the checkpoint is validated"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn summarizes_a_previous_checkpoint_that_exceeds_the_input_budget() {
        // Arrange: the checkpoint alone is larger than the model's input budget.
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            (0..8)
                .map(|_| Ok(summary_response("updated summary")))
                .collect(),
            vec![0; 8],
        );
        let config = crate::agent_loop::test_support::test_config_with_compaction(
            dir.path().to_string_lossy().to_string(),
            1,
        );
        let fixture = CompactionFixture::new(config, &provider, "large-checkpoint").await;
        let request = fixture.request();
        let mut session = session(vec![
            text("user", "old question"),
            text(
                "assistant",
                "The earlier plan is withdrawn; the current decision is the correction.",
            ),
            text("user", "fresh question"),
        ]);
        let latest_decision = "The old plan is withdrawn. Latest decision: use plan B.";
        session.checkpoint = Some(CompactionCheckpoint {
            summary: format!("{}\n\n{latest_decision}", "old checkpoint ".repeat(2_000)),
            tokens_before: 1,
            read_files: Vec::new(),
            modified_files: Vec::new(),
        });

        // Act
        let outcome = compact(&request, &session, CompactionReason::Manual, None)
            .await
            .expect("oversized checkpoint can be summarized");
        let CompactionOutcome::Compacted(compacted) = outcome else {
            panic!("expected checkpoint")
        };
        assert_eq!(compacted.checkpoint.unwrap().summary, "updated summary");
        let prompts = provider.seen_messages();
        assert!(prompts.len() > 1);
        assert!(
            prompts[0][0]
                .content
                .as_text_lossy()
                .contains("[Previous checkpoint]:")
        );
        assert!(prompts.iter().any(|messages| {
            messages[0]
                .content
                .as_text_lossy()
                .contains(latest_decision)
        }));
        assert!(
            prompts.last().unwrap()[0]
                .content
                .as_text_lossy()
                .contains("old question")
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn oversized_summaries_stop_after_the_recovery_limit() {
        // Arrange: every generated summary is too large to use as a checkpoint.
        let dir = tempfile::tempdir().expect("tempdir");
        let oversized = "long summary ".repeat(5_000);
        let provider = RecordingProvider::new(
            (0..3).map(|_| Ok(summary_response(&oversized))).collect(),
            vec![0; 3],
        );
        let config = crate::agent_loop::test_support::test_config_with_compaction(
            dir.path().to_string_lossy().to_string(),
            1,
        );
        let fixture = CompactionFixture::new(config, &provider, "oversized-summary").await;
        let request = fixture.request();

        // Act
        let result = generate_summary(
            &request,
            &[text("user", "new conversation")],
            Some(&oversized),
            None,
            "history",
        )
        .await;

        // Assert
        assert!(matches!(
            result,
            Err(EgoPulseError::Llm(
                LlmError::SummaryInputCapacityExceeded { .. }
            ))
        ));
        assert_eq!(provider.seen_messages().len(), 3);
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
        let fixture = CompactionFixture::new(config, &provider, "update-compact").await;
        let request = fixture.request();

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
        assert!(
            checkpoint_message(compacted.checkpoint.as_ref().unwrap())
                .content
                .as_text_lossy()
                .contains("<read-files>\nsrc/main.rs\n</read-files>")
        );
        let prompt = provider.seen_messages()[0][0].content.as_text_lossy();
        assert!(prompt.contains("<previous-summary>\nprevious summary\n</previous-summary>"));
        assert!(prompt.contains(UPDATE_SUMMARY_PROMPT));
        assert!(prompt.contains("Additional focus: focus on decisions"));
        assert!(!prompt.contains(INITIAL_SUMMARY_PROMPT));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn a_summary_context_overflow_retries_with_a_smaller_input() {
        // Arrange: the calibrated estimate fits, but the provider tokenizer
        // needs more tokens. The first attempt overflows; the second succeeds.
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![
                Err(LlmError::ContextOverflow {
                    status: reqwest::StatusCode::BAD_REQUEST,
                    body_preview: "context length exceeded".to_string(),
                }),
                Ok(summary_response("history summary")),
                Ok(summary_response("history summary")),
                Ok(summary_response("history summary")),
                Ok(summary_response("history summary")),
                Ok(summary_response("history summary")),
            ],
            vec![0; 6],
        );
        let config = crate::agent_loop::test_support::test_config_with_compaction(
            dir.path().to_string_lossy().to_string(),
            1,
        );
        let session = session(vec![
            text("user", &"old question ".repeat(2_000)),
            text("assistant", "old answer"),
            text("user", "fresh question"),
        ]);
        let fixture = CompactionFixture::new(config, &provider, "overflow-compact").await;
        let request = fixture.request();
        let initial_budget = summary_input_budget(&request).await;

        // Act
        let outcome = compact(&request, &session, CompactionReason::Manual, None)
            .await
            .expect("compact");

        // Assert: the retry shrinks the input and the checkpoint is saved.
        let CompactionOutcome::Compacted(compacted) = outcome else {
            panic!("expected a compacted context");
        };
        assert_eq!(
            compacted.checkpoint.as_ref().expect("checkpoint").summary,
            "history summary"
        );
        let prompts = provider.seen_messages();
        assert!(prompts.len() >= 2);
        assert_eq!(provider.seen_systems().len(), prompts.len());
        let first = prompts[0][0].content.as_text_lossy();
        let second = prompts[1][0].content.as_text_lossy();
        assert!(text_chars(&second) < text_chars(&first));
        assert!(
            summary_request_estimate(&first) <= initial_budget,
            "the first summary request must use the initial budget"
        );
        assert!(
            summary_request_estimate(&second) <= initial_budget / 2,
            "the retry must use a smaller input budget"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn summary_input_budget_applies_the_compaction_calibration() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(Vec::new(), Vec::new());
        let config = crate::agent_loop::test_support::test_config_with_compaction(
            dir.path().to_string_lossy().to_string(),
            1,
        );
        let fixture = CompactionFixture::new(config, &provider, "calibrated-compact").await;
        let request = fixture.request();
        let budget_before = summary_input_budget(&request).await;

        // Act: the provider measured three tokens for every estimated token.
        request
            .state
            .usage_calibrator
            .record(summary_calibration_key(&request), 100, 300)
            .await;

        // Assert
        let budget_after = summary_input_budget(&request).await;
        assert!(budget_after < budget_before);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn a_fixed_summary_template_that_exceeds_the_budget_is_rejected() {
        // Arrange: even the template without history cannot fit this window.
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(Vec::new(), Vec::new());
        let config = crate::agent_loop::test_support::test_config_with_compaction_window(
            dir.path().to_string_lossy().to_string(),
            1,
            200,
        );
        let session = session(vec![
            text("user", "old question"),
            text("assistant", "old answer"),
            text("user", "fresh question"),
        ]);
        let fixture = CompactionFixture::new(config, &provider, "tiny-compact").await;
        let request = fixture.request();

        // Act
        let result = compact(&request, &session, CompactionReason::Manual, None).await;

        // Assert
        assert!(matches!(
            result,
            Err(EgoPulseError::Llm(
                LlmError::SummaryInputCapacityExceeded { .. }
            ))
        ));
        assert!(
            provider.seen_messages().is_empty(),
            "a truncated template must never be sent"
        );
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
        let fixture = CompactionFixture::new(config, &provider, "split-compact").await;
        let request = fixture.request();

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
        assert!(
            checkpoint_message(compacted.checkpoint.as_ref().unwrap())
                .content
                .as_text_lossy()
                .contains("<read-files>\nsrc/lib.rs\n</read-files>")
        );
        assert_eq!(provider.seen_systems().len(), 2);
        let turn_prefix_prompt = provider.seen_messages()[1][0].content.as_text_lossy();
        assert!(turn_prefix_prompt.contains("# Conversation"));
        assert!(turn_prefix_prompt.contains(TURN_PREFIX_SUMMARY_PROMPT));
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
        let session = session(vec![text("user", "hello"), text("assistant", "hi")]);
        let fixture = CompactionFixture::new(config, &provider, "small-compact").await;
        let request = fixture.request();

        // Act
        let outcome = compact_if_needed(&request, &session, None)
            .await
            .expect("compact");

        // Assert
        assert!(matches!(outcome, CompactionOutcome::Unchanged));
        assert!(provider.seen_systems().is_empty());
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
