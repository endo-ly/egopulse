//! メッセージの圧縮 (compaction) と会話アーカイブ。

use crate::agent_loop::SurfaceContext;
use crate::agent_loop::formatting::{message_to_archive_text, message_to_text, strip_thinking};
use crate::error::EgoPulseError;
use crate::llm::Message;
use crate::runtime::AppState;
use tracing::{info, warn};

const MAX_COMPACTION_SUMMARY_CHARS: usize = 20_000;

pub(crate) async fn maybe_compact_messages(
    state: &AppState,
    context: &SurfaceContext,
    chat_id: i64,
    messages: &[Message],
    llm: &std::sync::Arc<dyn crate::llm::LlmProvider>,
) -> Result<Vec<Message>, EgoPulseError> {
    if messages.len() <= state.config.max_session_messages {
        return Ok(messages.to_vec());
    }

    archive_conversation(&state.config.data_dir, &context.channel, chat_id, messages).await;

    let keep_recent = state.config.compact_keep_recent.min(messages.len());
    if keep_recent == messages.len() {
        return Ok(messages.to_vec());
    }

    let split_at = messages.len() - keep_recent;
    let old_messages = &messages[..split_at];
    let recent_messages = &messages[split_at..];

    let mut summary_input = String::new();
    for message in old_messages {
        let role = &message.role;
        let text = message_to_text(message);
        summary_input.push_str(&format!("[{role}]: {text}\n\n"));
    }
    summary_input = truncate_compaction_summary_input(summary_input);

    let summarize_prompt = "Summarize the following conversation concisely, preserving key facts, decisions, tool results, and context needed to continue the conversation. Be brief but thorough.";
    let summarize_messages = vec![Message::text(
        "user",
        format!("{summarize_prompt}\n\n---\n\n{summary_input}"),
    )];
    let timeout_secs = state.config.compaction_timeout_secs;
    let summary_result = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        llm.send_message("You are a helpful summarizer.", summarize_messages, None),
    )
    .await;

    let summary = match summary_result {
        Ok(Ok(response)) => strip_thinking(&response.content),
        Ok(Err(error)) => {
            warn!("compaction summarization failed: {error}; falling back to recent messages");
            return Ok(recent_messages.to_vec());
        }
        Err(_) => {
            warn!(
                "compaction summarization timed out after {timeout_secs}s for {}:{}; falling back to recent messages",
                context.channel, chat_id
            );
            return Ok(recent_messages.to_vec());
        }
    };
    if summary.trim().is_empty() {
        warn!("compaction summarization returned empty text; falling back to recent messages");
        return Ok(recent_messages.to_vec());
    }

    let mut compacted = vec![Message::text(
        "user",
        format!("[Conversation Summary]\n{summary}"),
    )];
    if !matches!(recent_messages.first(), Some(message) if message.role == "assistant") {
        compacted.push(Message::text(
            "assistant",
            "Understood, I have the conversation context. How can I help?",
        ));
    }

    for message in recent_messages {
        append_compacted_message(&mut compacted, message);
    }

    if matches!(compacted.last(), Some(last) if last.role == "assistant") {
        compacted.pop();
    }

    Ok(compacted)
}

pub(crate) async fn archive_conversation(
    data_dir: &str,
    channel: &str,
    chat_id: i64,
    messages: &[Message],
) {
    let data_dir = data_dir.to_string();
    let channel = channel.to_string();
    let messages = messages.to_vec();
    let join_channel = channel.clone();
    let join_result = tokio::task::spawn_blocking(move || {
        archive_conversation_blocking(&data_dir, &channel, chat_id, &messages);
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
    data_dir: &str,
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
    let dir = std::path::PathBuf::from(data_dir)
        .join("groups")
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

/// Truncate the compaction summary input by character count, not by bytes.
///
/// The limit keeps UTF-8 text intact and appends `\n... (truncated)` when the
/// input exceeds `MAX_COMPACTION_SUMMARY_CHARS` characters.
pub(crate) fn truncate_compaction_summary_input(mut summary_input: String) -> String {
    if summary_input.chars().count() <= MAX_COMPACTION_SUMMARY_CHARS {
        return summary_input;
    }

    let cutoff = summary_input
        .char_indices()
        .nth(MAX_COMPACTION_SUMMARY_CHARS)
        .map(|(idx, _)| idx)
        .unwrap_or(summary_input.len());
    summary_input.truncate(cutoff);
    summary_input.push_str("\n... (truncated)");
    summary_input
}

pub(crate) fn append_compacted_message(compacted: &mut Vec<Message>, message: &Message) {
    let Some(last) = compacted.last_mut() else {
        compacted.push(message.clone());
        return;
    };

    if can_merge_compacted_messages(last, message) {
        let merged = format!(
            "{}\n{}",
            last.content.as_text_lossy(),
            message.content.as_text_lossy()
        );
        last.content = crate::llm::MessageContent::text(merged);
        return;
    }

    compacted.push(message.clone());
}

pub(crate) fn can_merge_compacted_messages(left: &Message, right: &Message) -> bool {
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

    #[test]
    fn truncate_compaction_summary_input_keeps_exact_character_limit() {
        let input = format!("{}{}{}", "a".repeat(19_998), "あ", "い");
        let truncated = truncate_compaction_summary_input(input.clone());

        assert_eq!(truncated, input);
    }

    #[test]
    fn truncate_compaction_summary_input_truncates_by_character_count() {
        let input = format!("{}{}{}", "a".repeat(19_999), "あ", "い");
        let truncated = truncate_compaction_summary_input(input);

        let expected = format!("{}\n... (truncated)", "a".repeat(19_999) + "あ");
        assert_eq!(truncated, expected);
    }
}
