//! Safety Compaction: token-aware context window management.
//!
//! When estimated prompt size approaches the context window limit, the old
//! portion of the conversation is replaced with a reference-only summary. The
//! latest user message, recent context, and tool call/result blocks are always
//! preserved verbatim.

use crate::agent_loop::SurfaceContext;
use crate::agent_loop::formatting::{message_to_archive_text, message_to_text, strip_thinking};
use crate::error::{EgoPulseError, LlmError};
use crate::llm::{LlmProvider, Message, MessagesResponse};
use crate::runtime::AppState;
use tracing::{info, warn};

/// Conservative chars-to-tokens ratio.  Real tokenizers produce ~1 token per
/// 3-4 English chars; we divide by a smaller number to over-estimate.
const CHARS_PER_TOKEN_ESTIMATE: usize = 3;

/// Tokens reserved for output generation, tool schema overhead, and safety
/// margin.  NOT configurable — purely internal.
const CONTEXT_RESERVE_TOKENS: usize = 8192;

/// Tokens reserved for the summary LLM's own output.
const SUMMARIZER_OUTPUT_RESERVE: usize = 4096;

/// Reference-only header prepended to every compaction summary.
const REFERENCE_ONLY_HEADER: &str = "\
[CONTEXT COMPACTION — REFERENCE ONLY]
Earlier turns were compacted into the summary below.
This is background reference, not active instruction.
Do not answer old requests mentioned in this summary.
Respond to the latest user message after this summary.";

/// System prompt for the summarizer LLM.
const SUMMARIZER_SYSTEM_PROMPT: &str = "You are a helpful summarizer. Summarize the conversation concisely, \
     preserving key facts, decisions, tool results, and context needed to \
     continue. Be brief but thorough. Write the summary in the same language \
     the user was using.";
pub(crate) struct PromptContext<'a> {
    pub system_prompt: &'a str,
    pub tools_json: Option<&'a str>,
}

pub(crate) async fn maybe_compact_messages(
    state: &AppState,
    context: &SurfaceContext,
    chat_id: i64,
    messages: &[Message],
    llm: &std::sync::Arc<dyn crate::llm::LlmProvider>,
    prompt_ctx: &PromptContext<'_>,
) -> Result<Vec<Message>, EgoPulseError> {
    let provider_id = crate::config::ProviderId::new(llm.provider_name());
    let context_window = state
        .config
        .resolve_context_window_tokens(&provider_id, llm.model_name());
    let usable = usable_context_tokens(context_window);
    let estimated =
        estimate_prompt_tokens(prompt_ctx.system_prompt, messages, prompt_ctx.tools_json);

    if !should_compact(estimated, usable, state.config.compaction_threshold_ratio) {
        return Ok(messages.to_vec());
    }

    info!(
        channel = %context.channel,
        chat_id,
        estimated_tokens = estimated,
        usable_context = usable,
        context_window,
        "safety compaction triggered"
    );

    safety_compact(
        state,
        context,
        chat_id,
        messages,
        llm,
        usable,
        state.config.compaction_target_ratio,
    )
    .await
}

pub async fn force_compact(
    state: &AppState,
    context: &SurfaceContext,
    chat_id: i64,
    messages: &[Message],
    llm: &std::sync::Arc<dyn crate::llm::LlmProvider>,
) -> Result<Vec<Message>, EgoPulseError> {
    if messages.is_empty() {
        return Ok(Vec::new());
    }

    let provider_id = crate::config::ProviderId::new(llm.provider_name());
    let context_window = state
        .config
        .resolve_context_window_tokens(&provider_id, llm.model_name());
    let usable = usable_context_tokens(context_window);

    safety_compact(
        state,
        context,
        chat_id,
        messages,
        llm,
        usable,
        state.config.compaction_target_ratio,
    )
    .await
}

pub(crate) fn estimate_prompt_tokens(
    system_prompt: &str,
    messages: &[Message],
    tools_json: Option<&str>,
) -> usize {
    let mut total_chars = system_prompt.len();
    for msg in messages {
        total_chars += msg.role.len();
        total_chars += msg.content.as_text_lossy().len();
        for tc in &msg.tool_calls {
            total_chars += tc.name.len();
            total_chars += tc.arguments.to_string().len();
        }
    }
    if let Some(tools) = tools_json {
        total_chars += tools.len();
    }
    (total_chars / CHARS_PER_TOKEN_ESTIMATE).max(1)
}

pub(crate) fn usable_context_tokens(context_window_tokens: usize) -> usize {
    context_window_tokens
        .saturating_sub(CONTEXT_RESERVE_TOKENS)
        .max(1)
}

pub(crate) fn should_compact(
    estimated_tokens: usize,
    usable_context: usize,
    threshold_ratio: f64,
) -> bool {
    let threshold = ((usable_context as f64 * threshold_ratio) as usize).max(1);
    estimated_tokens >= threshold
}

pub(crate) fn compaction_target_tokens(usable_context: usize, target_ratio: f64) -> usize {
    ((usable_context as f64 * target_ratio) as usize).max(1)
}

#[cfg(test)]
pub(crate) fn summarizer_input_budget(usable_context: usize) -> usize {
    usable_context.saturating_sub(SUMMARIZER_OUTPUT_RESERVE)
}

pub(crate) fn shrink_summary_input(text: String, budget_tokens: usize) -> String {
    let max_chars = budget_tokens * CHARS_PER_TOKEN_ESTIMATE;
    if text.chars().count() <= max_chars {
        return text;
    }
    let cutoff = text
        .char_indices()
        .nth(max_chars)
        .map(|(idx, _)| idx)
        .unwrap_or(text.len());
    let mut truncated = text;
    truncated.truncate(cutoff);
    truncated.push_str("\n... (truncated to fit summarizer budget)");
    truncated
}

struct CompactionSlices<'a> {
    old_messages: &'a [Message],
    recent_messages: &'a [Message],
}

struct CompactionResultInput<'a> {
    context: &'a SurfaceContext,
    chat_id: i64,
    llm: &'a std::sync::Arc<dyn LlmProvider>,
    original_count: usize,
    old_count: usize,
    recent_messages: &'a [Message],
    summary: &'a str,
    usable_context: usize,
    target_ratio: f64,
}

enum SummarizeOutcome {
    Summary(String),
    KeepOriginal,
}

async fn safety_compact(
    state: &AppState,
    context: &SurfaceContext,
    chat_id: i64,
    messages: &[Message],
    llm: &std::sync::Arc<dyn crate::llm::LlmProvider>,
    usable_context: usize,
    target_ratio: f64,
) -> Result<Vec<Message>, EgoPulseError> {
    archive_current_conversation(state, context, chat_id, messages).await;

    let Some(slices) = select_compaction_slices(messages, state.config.compact_keep_recent) else {
        return Ok(messages.to_vec());
    };

    let summary = match summarize_old_messages(
        state,
        context,
        chat_id,
        slices.old_messages,
        llm,
        usable_context,
        target_ratio,
    )
    .await
    {
        SummarizeOutcome::Summary(summary) => summary,
        SummarizeOutcome::KeepOriginal => return Ok(messages.to_vec()),
    };

    let compacted = build_compaction_result(CompactionResultInput {
        context,
        chat_id,
        llm,
        original_count: messages.len(),
        old_count: slices.old_messages.len(),
        recent_messages: slices.recent_messages,
        summary: &summary,
        usable_context,
        target_ratio,
    });

    Ok(compacted)
}

async fn archive_current_conversation(
    state: &AppState,
    context: &SurfaceContext,
    chat_id: i64,
    messages: &[Message],
) {
    archive_conversation(
        &state.config.groups_dir(),
        &context.channel,
        chat_id,
        messages,
    )
    .await;
}

fn select_compaction_slices(
    messages: &[Message],
    compact_keep_recent: usize,
) -> Option<CompactionSlices<'_>> {
    let keep_recent = compact_keep_recent.min(messages.len());
    if keep_recent == messages.len() {
        return None;
    }

    let split_at = compaction_split_at(messages, keep_recent);
    (split_at != 0).then_some(CompactionSlices {
        old_messages: &messages[..split_at],
        recent_messages: &messages[split_at..],
    })
}

async fn summarize_old_messages(
    state: &AppState,
    context: &SurfaceContext,
    chat_id: i64,
    old_messages: &[Message],
    llm: &std::sync::Arc<dyn LlmProvider>,
    usable_context: usize,
    target_ratio: f64,
) -> SummarizeOutcome {
    let mut summary_input = build_summary_input(old_messages, usable_context, target_ratio);
    summary_input = redact_summary_text(&summary_input, state);
    let timeout_secs = state.config.compaction_timeout_secs;

    let summary_result = send_summary_request(llm, summary_input, timeout_secs).await;
    let summary = match summary_result {
        Ok(response) => {
            log_summarizer_usage(state, context, chat_id, llm, &response);
            strip_thinking(&response.content)
        }
        Err(SummarizeError::Provider(error)) => {
            warn!("safety_compact summarization failed: {error}; keeping original messages");
            log_compaction_metrics(context, chat_id, llm, 0, 0, old_messages.len(), false);
            return SummarizeOutcome::KeepOriginal;
        }
        Err(SummarizeError::Timeout) => {
            warn!(
                "safety_compact timed out after {timeout_secs}s for {}:{}; keeping original messages",
                context.channel, chat_id
            );
            log_compaction_metrics(context, chat_id, llm, 0, 0, old_messages.len(), false);
            return SummarizeOutcome::KeepOriginal;
        }
    };

    if summary.trim().is_empty() {
        warn!("safety_compact returned empty text; keeping original messages");
        log_compaction_metrics(context, chat_id, llm, 0, 0, old_messages.len(), false);
        return SummarizeOutcome::KeepOriginal;
    }

    SummarizeOutcome::Summary(redact_summary_text(&summary, state))
}

enum SummarizeError {
    Provider(LlmError),
    Timeout,
}

async fn send_summary_request(
    llm: &std::sync::Arc<dyn LlmProvider>,
    summary_input: String,
    timeout_secs: u64,
) -> Result<MessagesResponse, SummarizeError> {
    tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        llm.send_message(
            SUMMARIZER_SYSTEM_PROMPT,
            vec![Message::text("user", summary_input)],
            None,
        ),
    )
    .await
    .map_err(|_| SummarizeError::Timeout)?
    .map_err(SummarizeError::Provider)
}

fn log_summarizer_usage(
    state: &AppState,
    context: &SurfaceContext,
    chat_id: i64,
    llm: &std::sync::Arc<dyn LlmProvider>,
    response: &MessagesResponse,
) {
    let Some(usage) = &response.usage else {
        return;
    };

    let db = std::sync::Arc::clone(&state.db);
    let channel = context.channel.clone();
    let provider = llm.provider_name().to_string();
    let model = llm.model_name().to_string();
    let input_tokens = usage.input_tokens;
    let output_tokens = usage.output_tokens;
    tokio::spawn(async move {
        let _ = crate::storage::call_blocking(db, move |db| {
            db.log_llm_usage(&crate::storage::LlmUsageLogEntry {
                chat_id,
                caller_channel: &channel,
                provider: &provider,
                model: &model,
                input_tokens,
                output_tokens,
                request_kind: "summarize",
            })
        })
        .await
        .inspect_err(|e| warn!(error = %e, "llm usage logging failed"));
    });
}

fn build_compaction_result(input: CompactionResultInput<'_>) -> Vec<Message> {
    let target = compaction_target_tokens(input.usable_context, input.target_ratio);
    let compacted = build_targeted_compacted_messages(input.summary, input.recent_messages, target);

    let new_count = compacted.len();
    log_compaction_metrics(
        input.context,
        input.chat_id,
        input.llm,
        input.old_count,
        new_count,
        input.original_count,
        true,
    );

    let post_tokens = estimate_prompt_tokens("", &compacted, None);
    if post_tokens > target {
        warn!(
            channel = %input.context.channel,
            chat_id = input.chat_id,
            post_tokens,
            target_tokens = target,
            target_ratio = input.target_ratio,
            "compaction exceeded target ratio; context may still be large"
        );
    }

    compacted
}

fn build_targeted_compacted_messages(
    summary: &str,
    recent_messages: &[Message],
    target_tokens: usize,
) -> Vec<Message> {
    let compacted = build_compacted_messages(summary, recent_messages);
    if target_tokens == 0 {
        return compacted;
    }
    if estimate_prompt_tokens("", &compacted, None) <= target_tokens {
        return compacted;
    }

    let mut best = None;
    let mut low = 0;
    let mut high = summary.chars().count();
    while low <= high {
        let mid = low + (high - low) / 2;
        let candidate_summary = truncate_summary_for_target(summary, mid);
        let candidate = build_compacted_messages(&candidate_summary, recent_messages);
        if estimate_prompt_tokens("", &candidate, None) <= target_tokens {
            best = Some(candidate);
            low = mid + 1;
        } else if mid == 0 {
            break;
        } else {
            high = mid - 1;
        }
    }

    best.unwrap_or_else(|| build_compacted_messages("", recent_messages))
}

fn truncate_summary_for_target(summary: &str, max_chars: usize) -> String {
    let total_chars = summary.chars().count();
    if max_chars >= total_chars {
        return summary.to_string();
    }
    if max_chars == 0 {
        return String::new();
    }

    let cutoff = summary
        .char_indices()
        .nth(max_chars)
        .map(|(idx, _)| idx)
        .unwrap_or(summary.len());
    let mut truncated = summary[..cutoff].to_string();
    truncated.push_str("\n... (summary truncated to fit compaction target)");
    truncated
}

fn compaction_split_at(messages: &[Message], keep_recent: usize) -> usize {
    let desired_split = messages
        .len()
        .saturating_sub(keep_recent.min(messages.len()));
    let latest_user_split = messages
        .iter()
        .rposition(|message| message.role == "user")
        .unwrap_or(desired_split);
    let preferred_split = desired_split.min(latest_user_split);
    tool_safe_split_at(messages, preferred_split)
}

fn build_summary_input(
    old_messages: &[Message],
    usable_context: usize,
    target_ratio: f64,
) -> String {
    let target_tokens = compaction_target_tokens(usable_context, target_ratio);
    let budget = target_tokens.saturating_sub(SUMMARIZER_OUTPUT_RESERVE);
    let max_chars = budget * CHARS_PER_TOKEN_ESTIMATE;

    let mut summary_input = String::new();
    for message in old_messages {
        let role = &message.role;
        let text = lighten_message(message);
        summary_input.push_str(&format!("[{role}]: {text}\n\n"));
    }

    if summary_input.chars().count() <= max_chars {
        return summary_input;
    }

    shrink_summary_input(summary_input, budget)
}

fn lighten_message(message: &Message) -> String {
    let text = message_to_text(message);
    if message.role == "tool" && text.chars().count() > 500 {
        let truncated: String = text.chars().take(400).collect();
        format!("{truncated}... (tool result truncated for summary)")
    } else {
        text
    }
}

fn redact_summary_text(text: &str, state: &AppState) -> String {
    let secrets = crate::tools::collect_config_secrets(&state.config);
    crate::tools::sanitize_output_string(text, &secrets)
}

fn build_compacted_messages(summary: &str, recent_messages: &[Message]) -> Vec<Message> {
    let mut compacted = vec![Message::text(
        "user",
        format!("{REFERENCE_ONLY_HEADER}\n\n{summary}"),
    )];

    for message in recent_messages {
        compacted.push(message.clone());
    }

    merge_compacted_skipping_summary(&mut compacted);

    if matches!(compacted.last(), Some(last) if last.role == "assistant") {
        compacted.pop();
    }

    compacted
}

fn merge_compacted_skipping_summary(compacted: &mut Vec<Message>) {
    let start_idx = 1;
    if compacted.len() <= start_idx + 1 {
        return;
    }

    let mut write = start_idx;
    for read in start_idx + 1..compacted.len() {
        let can_merge = {
            let left = &compacted[write];
            let right = &compacted[read];
            can_merge_compacted_messages(left, right)
        };
        if can_merge {
            let merged = format!(
                "{}\n{}",
                compacted[write].content.as_text_lossy(),
                compacted[read].content.as_text_lossy()
            );
            compacted[write].content = crate::llm::MessageContent::text(merged);
        } else {
            write += 1;
            if write != read {
                compacted.swap(write, read);
            }
        }
    }
    compacted.truncate(write + 1);
}

fn log_compaction_metrics(
    context: &SurfaceContext,
    chat_id: i64,
    llm: &std::sync::Arc<dyn crate::llm::LlmProvider>,
    old_count: usize,
    new_count: usize,
    total_count: usize,
    success: bool,
) {
    info!(
        channel = %context.channel,
        chat_id,
        provider = llm.provider_name(),
        model = llm.model_name(),
        old_count,
        new_count,
        total_count,
        success,
        "safety_compact completed"
    );
}

pub(crate) async fn archive_conversation(
    groups_dir: &std::path::Path,
    channel: &str,
    chat_id: i64,
    messages: &[Message],
) {
    let groups_dir = groups_dir.to_path_buf();
    let channel = channel.to_string();
    let messages: std::sync::Arc<[Message]> =
        std::sync::Arc::from(messages.to_vec().into_boxed_slice());
    let join_channel = channel.clone();
    let join_result = tokio::task::spawn_blocking(move || {
        archive_conversation_blocking(&groups_dir, &channel, chat_id, &messages);
    })
    .await;

    if let Err(error) = join_result {
        warn!(
            "failed to join archive task for {}:{}: {error}",
            join_channel, chat_id
        );
    }
}

pub(crate) fn archive_conversation_blocking(
    groups_dir: &std::path::Path,
    channel: &str,
    chat_id: i64,
    messages: &[Message],
) {
    let now = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let unique_suffix = uuid::Uuid::new_v4().simple();
    let channel_dir = if channel.trim().is_empty() {
        "unknown"
    } else {
        channel.trim()
    };
    let dir = groups_dir
        .join(channel_dir)
        .join(chat_id.to_string())
        .join("conversations");

    if let Err(error) = std::fs::create_dir_all(&dir) {
        warn!("failed to create archive dir {}: {error}", dir.display());
        return;
    }

    let path = dir.join(format!("{now}-{unique_suffix}.md"));
    let mut content = String::new();
    for message in messages {
        let role = &message.role;
        let text = message_to_archive_text(message);
        content.push_str(&format!("## {role}\n\n{text}\n\n---\n\n"));
    }

    if let Err(error) = std::fs::write(&path, content) {
        warn!(
            "failed to archive conversation to {}: {error}",
            path.display()
        );
    } else {
        info!(
            "archived conversation ({} messages) to {}",
            messages.len(),
            path.display()
        );
    }
}

pub(crate) fn tool_safe_split_at(messages: &[Message], preferred_split_at: usize) -> usize {
    let mut split_at = preferred_split_at.min(messages.len());

    while split_at < messages.len() && messages[split_at].role == "tool" {
        let Some(tool_call_id) = messages[split_at].tool_call_id.as_deref() else {
            split_at += 1;
            continue;
        };

        let Some(parent_index) = find_tool_call_parent(messages, split_at, tool_call_id) else {
            split_at += 1;
            continue;
        };

        split_at = parent_index;
    }

    split_at
}

fn find_tool_call_parent(
    messages: &[Message],
    before_index: usize,
    tool_call_id: &str,
) -> Option<usize> {
    messages[..before_index].iter().rposition(|message| {
        message.role == "assistant"
            && message
                .tool_calls
                .iter()
                .any(|tool_call| tool_call.id == tool_call_id)
    })
}

fn can_merge_compacted_messages(left: &Message, right: &Message) -> bool {
    left.role == right.role
        && left.tool_calls.is_empty()
        && right.tool_calls.is_empty()
        && left.tool_call_id.is_none()
        && right.tool_call_id.is_none()
        && matches!(left.content, crate::llm::MessageContent::Text(_))
        && matches!(right.content, crate::llm::MessageContent::Text(_))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_loop::process_turn;
    use crate::agent_loop::turn::{
        RecordingProvider, build_state, cli_context, test_config_with_compaction,
    };
    use crate::error::LlmError;
    use crate::llm::{Message, MessagesResponse, ToolCall};
    use crate::storage::call_blocking;
    use serial_test::serial;
    use std::sync::Arc;

    #[test]
    fn shrink_summary_input_keeps_text_under_budget() {
        let budget_tokens = 100;
        let max_chars = budget_tokens * CHARS_PER_TOKEN_ESTIMATE;
        let input = "a".repeat(max_chars);
        let result = shrink_summary_input(input.clone(), budget_tokens);
        assert_eq!(result, input);
    }

    #[test]
    fn shrink_summary_input_truncates_when_over_budget() {
        let budget_tokens = 100;
        let max_chars = budget_tokens * CHARS_PER_TOKEN_ESTIMATE;
        let input = "a".repeat(max_chars + 10);
        let result = shrink_summary_input(input, budget_tokens);
        assert!(result.ends_with("... (truncated to fit summarizer budget)"));
        assert!(result.chars().count() <= max_chars + 50);
    }

    #[test]
    fn estimates_prompt_tokens_from_system_messages_and_tools() {
        let system = "You are a helpful assistant.";
        let messages = vec![
            Message::text("user", "Hello world"),
            Message::text("assistant", "Hi there! How can I help?"),
        ];
        let tools = r#"[{"name":"read","parameters":{}}]"#;

        let tokens = estimate_prompt_tokens(system, &messages, Some(tools));

        let total_chars = system.len()
            + "user".len()
            + "Hello world".len()
            + "assistant".len()
            + "Hi there! How can I help?".len()
            + tools.len();
        let expected = (total_chars / CHARS_PER_TOKEN_ESTIMATE).max(1);
        assert_eq!(tokens, expected);
        assert!(tokens > 0);
    }

    #[test]
    fn computes_usable_context_from_context_window_and_reserves() {
        assert_eq!(
            usable_context_tokens(100_000),
            100_000 - CONTEXT_RESERVE_TOKENS
        );
        assert_eq!(usable_context_tokens(5000), 1);
    }

    #[test]
    fn triggers_when_estimate_reaches_threshold() {
        let usable = 50_000;
        let threshold_ratio = 0.80;
        let estimated = 40_000;
        assert!(should_compact(estimated, usable, threshold_ratio));
    }

    #[test]
    fn does_not_trigger_below_threshold() {
        let usable = 50_000;
        let threshold_ratio = 0.80;
        let estimated = 39_999;
        assert!(!should_compact(estimated, usable, threshold_ratio));
    }

    #[test]
    fn targets_configured_compaction_ratio() {
        let usable = 100_000;
        assert_eq!(compaction_target_tokens(usable, 0.40), 40_000);
        assert_eq!(compaction_target_tokens(usable, 0.30), 30_000);
        assert_eq!(compaction_target_tokens(1, 0.30), 1);
    }

    #[test]
    fn split_preserves_latest_user_message_with_recent_tail() {
        let messages = vec![
            Message::text("user", "old request"),
            Message::text("assistant", "old answer"),
            Message::text("user", "fresh request"),
            Message::text("assistant", "draft answer"),
        ];

        let split_at = compaction_split_at(&messages, 1);

        assert_eq!(split_at, 2);
        assert_eq!(messages[split_at].content.as_text_lossy(), "fresh request");
    }

    #[test]
    fn caps_summary_input_to_summarizer_budget() {
        let usable = 50_000;
        let budget = summarizer_input_budget(usable);
        assert_eq!(budget, usable - SUMMARIZER_OUTPUT_RESERVE);
    }

    #[test]
    fn shrinks_summary_input_until_under_budget() {
        let budget_tokens = 50;
        let max_chars = budget_tokens * CHARS_PER_TOKEN_ESTIMATE;
        let input = "x".repeat(max_chars * 3);
        let result = shrink_summary_input(input, budget_tokens);
        assert!(result.chars().count() < max_chars * 2);
        assert!(result.contains("truncated"));
    }

    #[test]
    fn shrinks_compacted_summary_to_target() {
        let recent = vec![Message::text("user", "fresh question")];
        let full = build_compacted_messages(&"summary ".repeat(1000), &recent);
        let minimum = build_compacted_messages("", &recent);
        let target = estimate_prompt_tokens("", &minimum, None) + 10;

        let result = build_targeted_compacted_messages(&"summary ".repeat(1000), &recent, target);

        assert!(estimate_prompt_tokens("", &full, None) > target);
        assert!(estimate_prompt_tokens("", &result, None) <= target);
        assert!(
            result[0]
                .content
                .as_text_lossy()
                .contains(REFERENCE_ONLY_HEADER)
        );
        assert_eq!(
            result.last().expect("recent").content.as_text_lossy(),
            "fresh question"
        );
    }

    #[test]
    fn keeps_protected_recent_when_target_is_impossible() {
        let recent = vec![Message::text("user", "fresh question".repeat(500))];

        let result = build_targeted_compacted_messages(&"summary ".repeat(1000), &recent, 1);

        assert_eq!(result.last().expect("recent").role, "user");
        assert!(
            result[0]
                .content
                .as_text_lossy()
                .contains(REFERENCE_ONLY_HEADER)
        );
    }

    #[test]
    fn tool_safe_split_at_rewinds_from_tool_output_to_parent_call() {
        let messages = vec![
            Message::text("user", "old"),
            assistant_tool_call("call_1"),
            tool_output("call_1"),
            Message::text("user", "next"),
        ];

        assert_eq!(tool_safe_split_at(&messages, 2), 1);
    }

    #[test]
    fn tool_safe_split_at_keeps_multi_tool_call_block_together() {
        let messages = vec![
            Message::text("user", "old"),
            Message {
                role: "assistant".to_string(),
                content: crate::llm::MessageContent::text(""),
                tool_calls: vec![
                    ToolCall {
                        id: "call_a".to_string(),
                        name: "read".to_string(),
                        arguments: serde_json::json!({}),
                    },
                    ToolCall {
                        id: "call_b".to_string(),
                        name: "grep".to_string(),
                        arguments: serde_json::json!({}),
                    },
                ],
                tool_call_id: None,
            },
            tool_output("call_a"),
            tool_output("call_b"),
            Message::text("assistant", "done"),
        ];

        assert_eq!(tool_safe_split_at(&messages, 3), 1);
    }

    #[test]
    fn tool_safe_split_at_skips_orphan_tool_outputs() {
        let messages = vec![
            Message::text("user", "old"),
            tool_output("call_missing"),
            Message::text("user", "next"),
        ];

        assert_eq!(tool_safe_split_at(&messages, 1), 2);
    }

    fn assistant_tool_call(id: &str) -> Message {
        Message {
            role: "assistant".to_string(),
            content: crate::llm::MessageContent::text(""),
            tool_calls: vec![ToolCall {
                id: id.to_string(),
                name: "read".to_string(),
                arguments: serde_json::json!({}),
            }],
            tool_call_id: None,
        }
    }

    fn tool_output(id: &str) -> Message {
        Message {
            role: "tool".to_string(),
            content: crate::llm::MessageContent::text("result"),
            tool_calls: Vec::new(),
            tool_call_id: Some(id.to_string()),
        }
    }

    #[tokio::test]
    #[serial]
    async fn compaction_summarizes_old_messages_and_persists_summary_context() {
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![
                Ok(MessagesResponse {
                    content: "summary text".to_string(),
                    tool_calls: Vec::new(),
                    usage: None,
                }),
                Ok(MessagesResponse {
                    content: "final answer".to_string(),
                    tool_calls: Vec::new(),
                    usage: None,
                }),
            ],
            vec![0, 0],
        );
        let config =
            test_config_with_compaction(dir.path().to_str().expect("utf8").to_string(), 4, 2);
        let state = build_state(config, Box::new(provider.clone()));
        let context = cli_context("compaction-success");
        let chat_id = call_blocking(Arc::clone(&state.db), move |db| {
            db.resolve_or_create_chat_id(
                "cli",
                "cli:compaction-success",
                Some("compaction-success"),
                "cli",
            )
        })
        .await
        .expect("chat id");
        let seeded = vec![
            Message::text("user", "old-user-1"),
            Message::text("assistant", "old-assistant-1"),
            Message::text("user", "old-user-2"),
            Message::text("assistant", "old-assistant-2"),
        ];
        let seeded_json = serde_json::to_string(&seeded).expect("seeded json");
        call_blocking(Arc::clone(&state.db), move |db| {
            db.save_session(chat_id, &seeded_json)
        })
        .await
        .expect("save session");

        let reply = process_turn(&state, &context, "fresh question")
            .await
            .expect("process turn");
        assert_eq!(reply, "final answer");

        let seen_systems = provider.seen_systems();
        assert_eq!(seen_systems.len(), 2);
        assert_eq!(seen_systems[0], SUMMARIZER_SYSTEM_PROMPT);

        let seen_messages = provider.seen_messages();
        assert_eq!(seen_messages.len(), 2);
        let summary_text = seen_messages[1][0].content.as_text_lossy();
        assert!(summary_text.contains("[CONTEXT COMPACTION — REFERENCE ONLY]"));
        assert!(summary_text.contains("summary text"));
        assert_eq!(
            seen_messages[1]
                .last()
                .expect("final request")
                .content
                .as_text_lossy(),
            "fresh question"
        );

        let loaded = crate::agent_loop::session::load_messages_for_turn(&state, chat_id)
            .await
            .expect("loaded session");
        let loaded_summary = loaded.messages[0].content.as_text_lossy();
        assert!(loaded_summary.contains("[CONTEXT COMPACTION — REFERENCE ONLY]"));
        assert!(loaded_summary.contains("summary text"));
        assert_eq!(
            loaded
                .messages
                .last()
                .expect("session last")
                .content
                .as_text_lossy(),
            "final answer"
        );

        let archive_dir = dir
            .path()
            .join("runtime")
            .join("groups")
            .join("cli")
            .join(chat_id.to_string())
            .join("conversations");
        let archives = std::fs::read_dir(&archive_dir)
            .expect("archive dir")
            .collect::<Result<Vec<_>, _>>()
            .expect("archive entries");
        assert_eq!(archives.len(), 1);
        let archive_body = std::fs::read_to_string(archives[0].path()).expect("archive body");
        assert!(archive_body.contains("old-user-1"));
        assert!(archive_body.contains("fresh question"));
    }

    #[tokio::test]
    #[serial]
    async fn compaction_falls_back_to_recent_messages_when_summary_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![
                Err(LlmError::InvalidResponse("summary failed".to_string())),
                Ok(MessagesResponse {
                    content: "final answer".to_string(),
                    tool_calls: Vec::new(),
                    usage: None,
                }),
            ],
            vec![0, 0],
        );
        let config =
            test_config_with_compaction(dir.path().to_str().expect("utf8").to_string(), 4, 2);
        let state = build_state(config, Box::new(provider.clone()));
        let context = cli_context("compaction-fallback");
        let chat_id = call_blocking(Arc::clone(&state.db), move |db| {
            db.resolve_or_create_chat_id(
                "cli",
                "cli:compaction-fallback",
                Some("compaction-fallback"),
                "cli",
            )
        })
        .await
        .expect("chat id");
        let seeded = vec![
            Message::text("user", "old-user-1"),
            Message::text("assistant", "old-assistant-1"),
            Message::text("user", "old-user-2"),
            Message::text("assistant", "old-assistant-2"),
        ];
        let seeded_json = serde_json::to_string(&seeded).expect("seeded json");
        call_blocking(Arc::clone(&state.db), move |db| {
            db.save_session(chat_id, &seeded_json)
        })
        .await
        .expect("save session");

        let reply = process_turn(&state, &context, "fresh question")
            .await
            .expect("process turn");
        assert_eq!(reply, "final answer");

        let seen_messages = provider.seen_messages();
        assert_eq!(seen_messages.len(), 2);
        assert!(seen_messages[1].iter().all(|message| {
            !message
                .content
                .as_text_lossy()
                .contains("[CONTEXT COMPACTION — REFERENCE ONLY]")
        }));
        assert_eq!(seen_messages[1][0].content.as_text_lossy(), "old-user-1");
        assert_eq!(
            seen_messages[1]
                .last()
                .expect("final request")
                .content
                .as_text_lossy(),
            "fresh question"
        );

        let loaded = crate::agent_loop::session::load_messages_for_turn(&state, chat_id)
            .await
            .expect("loaded session");
        assert!(loaded.messages.iter().all(|message| {
            !message
                .content
                .as_text_lossy()
                .contains("[CONTEXT COMPACTION — REFERENCE ONLY]")
        }));
        assert_eq!(loaded.messages[0].content.as_text_lossy(), "old-user-1");
        assert_eq!(
            loaded
                .messages
                .last()
                .expect("session last")
                .content
                .as_text_lossy(),
            "final answer"
        );
    }

    #[tokio::test]
    #[serial]
    async fn force_compact_runs_regardless_of_threshold() {
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![Ok(MessagesResponse {
                content: "summary text".to_string(),
                tool_calls: Vec::new(),
                usage: None,
            })],
            vec![0],
        );
        let config =
            test_config_with_compaction(dir.path().to_str().expect("utf8").to_string(), 40, 1);
        let state = build_state(config, Box::new(provider.clone()));
        let context = cli_context("force-compact-threshold");
        let llm = state.global_llm().expect("llm");
        let messages = vec![
            Message::text("user", "msg-1"),
            Message::text("assistant", "reply-1"),
            Message::text("user", "msg-2"),
        ];

        let result = force_compact(&state, &context, 1, &messages, &llm)
            .await
            .expect("force_compact");

        assert_eq!(provider.seen_systems().len(), 1);
        assert_eq!(provider.seen_systems()[0], SUMMARIZER_SYSTEM_PROMPT);
        assert!(result.first().is_some_and(|m| {
            m.content
                .as_text_lossy()
                .contains("[CONTEXT COMPACTION — REFERENCE ONLY]")
        }));
    }

    #[tokio::test]
    #[serial]
    async fn force_compact_preserves_recent_messages() {
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![Ok(MessagesResponse {
                content: "summary text".to_string(),
                tool_calls: Vec::new(),
                usage: None,
            })],
            vec![0],
        );
        let config =
            test_config_with_compaction(dir.path().to_str().expect("utf8").to_string(), 40, 2);
        let state = build_state(config, Box::new(provider.clone()));
        let context = cli_context("force-compact-recent");
        let llm = state.global_llm().expect("llm");
        let messages = vec![
            Message::text("user", "old-1"),
            Message::text("assistant", "old-2"),
            Message::text("user", "old-3"),
            Message::text("assistant", "old-4"),
            Message::text("user", "kept-a"),
            Message::text("assistant", "kept-b"),
            Message::text("user", "kept-c"),
        ];

        let result = force_compact(&state, &context, 1, &messages, &llm)
            .await
            .expect("force_compact");

        let text: Vec<String> = result.iter().map(|m| m.content.as_text_lossy()).collect();
        assert!(text.iter().any(|t| t.contains("kept-b")));
        assert!(text.iter().any(|t| t.contains("kept-c")));
    }

    #[tokio::test]
    #[serial]
    async fn force_compact_produces_archive() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_root = dir.path().to_str().expect("utf8").to_string();
        let provider = RecordingProvider::new(
            vec![Ok(MessagesResponse {
                content: "summary text".to_string(),
                tool_calls: Vec::new(),
                usage: None,
            })],
            vec![0],
        );
        let config = test_config_with_compaction(state_root.clone(), 40, 1);
        let state = build_state(config, Box::new(provider.clone()));
        let context = cli_context("force-compact-archive");
        let llm = state.global_llm().expect("llm");
        let chat_id: i64 = 42;
        let messages = vec![
            Message::text("user", "msg-1"),
            Message::text("assistant", "reply-1"),
        ];

        force_compact(&state, &context, chat_id, &messages, &llm)
            .await
            .expect("force_compact");

        let archive_dir = dir
            .path()
            .join("runtime")
            .join("groups")
            .join("cli")
            .join(chat_id.to_string())
            .join("conversations");
        let archives = std::fs::read_dir(&archive_dir)
            .expect("archive dir")
            .collect::<Result<Vec<_>, _>>()
            .expect("archive entries");
        assert_eq!(archives.len(), 1);
        let body = std::fs::read_to_string(archives[0].path()).expect("archive body");
        assert!(body.contains("msg-1"));
    }

    #[tokio::test]
    #[serial]
    async fn compaction_logs_llm_usage_as_summarize() {
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = RecordingProvider::new(
            vec![
                Ok(MessagesResponse {
                    content: "summary text".to_string(),
                    tool_calls: Vec::new(),
                    usage: Some(crate::llm::LlmUsage {
                        input_tokens: 100,
                        output_tokens: 200,
                    }),
                }),
                Ok(MessagesResponse {
                    content: "final answer".to_string(),
                    tool_calls: Vec::new(),
                    usage: None,
                }),
            ],
            vec![0, 0],
        );
        let config =
            test_config_with_compaction(dir.path().to_str().expect("utf8").to_string(), 4, 2);
        let state = build_state(config, Box::new(provider));
        let context = cli_context("compaction-usage");
        let chat_id = call_blocking(Arc::clone(&state.db), move |db| {
            db.resolve_or_create_chat_id(
                "cli",
                "cli:compaction-usage",
                Some("compaction-usage"),
                "cli",
            )
        })
        .await
        .expect("chat id");
        let seeded = vec![
            Message::text("user", "old-user-1"),
            Message::text("assistant", "old-assistant-1"),
            Message::text("user", "old-user-2"),
            Message::text("assistant", "old-assistant-2"),
        ];
        let seeded_json = serde_json::to_string(&seeded).expect("seeded json");
        call_blocking(Arc::clone(&state.db), move |db| {
            db.save_session(chat_id, &seeded_json)
        })
        .await
        .expect("save session");

        let reply = process_turn(&state, &context, "fresh question")
            .await
            .expect("process turn");
        assert_eq!(reply, "final answer");

        for _ in 0..20 {
            let summary = call_blocking(Arc::clone(&state.db), move |db| {
                db.get_llm_usage_summary(Some(chat_id), None, None)
            })
            .await
            .expect("summary");
            if summary.requests > 0 {
                assert_eq!(
                    summary.requests, 1,
                    "compaction LLM call should be logged once"
                );
                assert_eq!(summary.input_tokens, 100);
                assert_eq!(summary.output_tokens, 200);
                assert_eq!(summary.total_tokens, 300);
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("compaction usage log was not written within the polling timeout");
    }
}
