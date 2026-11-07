//! semantic_update — folds episode events into `semantic.md` via the LLM.

use std::sync::Arc;

use tracing::warn;

use crate::llm::{LlmProvider, Message};
use crate::storage::EpisodeEvent;

use super::SleepBatchError;
use super::prompt::{
    ESTIMATED_CHARS_PER_TOKEN, SLEEP_BATCH_OVERFLOW_RATIO, escape_xml_content,
    normalize_llm_response,
};

const SEMANTIC_RETRY_GUARD: &str = "\
Your previous response was not valid JSON. \
You must respond with ONLY a JSON object containing exactly one key: \
\"semantic\". \
Do not include any other keys, markdown formatting, code blocks, or explanatory text. \
Output the raw JSON object and nothing else.";

/// Validated input for one semantic LLM call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SemanticInput {
    pub agent_id: String,
    pub semantic: String,
    pub events_json: String,
}

/// Splits events into prompt batches of at most `max_chars` serialized
/// characters, preserving order. An event larger than `max_chars` forms a
/// batch of its own; events are never split.
pub(super) fn build_event_batches(events: &[EpisodeEvent], max_chars: usize) -> Vec<String> {
    let mut batches = Vec::new();
    let mut current: Vec<serde_json::Value> = Vec::new();

    for event in events {
        current.push(serde_json::json!({
            "experienced_at": event.experienced_at,
            "kind": event.kind.to_string(),
            "title": event.title,
            "body_md": event.body_md,
            "ripple_strength": event.ripple_strength,
            "certainty": event.certainty.to_string(),
        }));

        if current.len() > 1 {
            let serialized = serde_json::to_string(&current).expect("events serialize");
            if serialized.len() > max_chars {
                let last = current.pop().expect("at least one event");
                batches.push(serde_json::to_string(&current).expect("events serialize"));
                current = vec![last];
            }
        }
    }

    if !current.is_empty() {
        batches.push(serde_json::to_string(&current).expect("events serialize"));
    }

    batches
}

/// # Errors
///
/// `UnsafeAgentId` for unsafe ids, `ContextOverflow` when
/// semantic + events tokens exceed `context_window_tokens * 0.80`.
pub(super) fn build_semantic_input(
    agent_id: &str,
    semantic: String,
    events_json: String,
    context_window_tokens: usize,
) -> Result<SemanticInput, SleepBatchError> {
    let trimmed = agent_id.trim();
    if trimmed.is_empty()
        || trimmed.contains("..")
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains(':')
    {
        return Err(SleepBatchError::UnsafeAgentId(agent_id.to_string()));
    }

    let semantic_tokens = estimate_text_tokens(&semantic);
    let events_tokens = estimate_text_tokens(&events_json);
    let threshold = (context_window_tokens as f64 * SLEEP_BATCH_OVERFLOW_RATIO) as usize;
    if semantic_tokens.saturating_add(events_tokens) > threshold {
        return Err(SleepBatchError::ContextOverflow {
            agent_id: agent_id.to_string(),
        });
    }

    Ok(SemanticInput {
        agent_id: agent_id.to_string(),
        semantic,
        events_json,
    })
}

pub(super) fn build_semantic_system_prompt(input: &SemanticInput) -> String {
    let mut prompt =
        include_str!("prompts/semantic_prompt.md").replace("{AGENT_NAME}", &input.agent_id);

    let mut input_data = String::new();
    if !input.semantic.is_empty() {
        input_data.push_str(&format!(
            "<memory-semantic>\n{}\n</memory-semantic>\n\n",
            escape_xml_content(&input.semantic)
        ));
    }
    if !input.events_json.is_empty() {
        input_data.push_str(&format!(
            "<episode-events>\n{}\n</episode-events>\n",
            escape_xml_content(&input.events_json)
        ));
    }

    if !input_data.is_empty() {
        prompt.push_str("\n\n## 入力データ\n\n");
        prompt.push_str(&input_data);
    }

    prompt
}

/// # Errors
///
/// `Llm` on provider failure, `ParseFailed` when both the first response and
/// the guarded retry are not `{"semantic": string}`.
pub(super) async fn send_semantic_request(
    provider: &Arc<dyn LlmProvider>,
    agent_id: &str,
    system_prompt: &str,
    batch_index: usize,
    total_batches: usize,
) -> Result<(String, i64, i64), SleepBatchError> {
    let user_message = Message::text(
        "user",
        format!("Please process semantic update batch {batch_index} of {total_batches}."),
    );
    let response = provider
        .send_message(system_prompt, Arc::new(vec![user_message.clone()]), None)
        .await
        .map_err(|e| SleepBatchError::Llm(e.to_string()))?;

    let (semantic, response) = match parse_semantic_response(&response.content) {
        Ok(semantic) => (semantic, response),
        Err(first_error) => {
            warn!(
                agent_id = %agent_id,
                batch_index,
                total_batches,
                error = %first_error,
                "semantic update parse failed; retrying once with JSON guard"
            );

            let first_input = response.usage.as_ref().map_or(0, |u| u.input_tokens);
            let first_output = response.usage.as_ref().map_or(0, |u| u.output_tokens);

            let retry_messages = vec![
                user_message,
                Message::text("assistant", &response.content),
                Message::text("user", SEMANTIC_RETRY_GUARD),
            ];
            let retry_response = provider
                .send_message(system_prompt, Arc::new(retry_messages), None)
                .await
                .map_err(|e| SleepBatchError::Llm(e.to_string()))?;

            let retry_input = retry_response.usage.as_ref().map_or(0, |u| u.input_tokens);
            let retry_output = retry_response.usage.as_ref().map_or(0, |u| u.output_tokens);
            let combined_input = first_input.saturating_add(retry_input);
            let combined_output = first_output.saturating_add(retry_output);

            match parse_semantic_response(&retry_response.content) {
                Ok(semantic) => return Ok((semantic, combined_input, combined_output)),
                Err(retry_error) => {
                    warn!(
                        agent_id = %agent_id,
                        batch_index,
                        total_batches,
                        error = %retry_error,
                        "semantic update retry also failed"
                    );
                    return Err(retry_error);
                }
            }
        }
    };

    let input_tokens = response.usage.as_ref().map_or(0, |u| u.input_tokens);
    let output_tokens = response.usage.as_ref().map_or(0, |u| u.output_tokens);
    Ok((semantic, input_tokens, output_tokens))
}

fn parse_semantic_response(response: &str) -> Result<String, SleepBatchError> {
    let normalized = normalize_llm_response(response);
    let value: serde_json::Value = serde_json::from_str(&normalized)
        .map_err(|e| SleepBatchError::ParseFailed(format!("invalid JSON: {e}")))?;

    let map = value.as_object().ok_or_else(|| {
        SleepBatchError::ParseFailed("response must be a JSON object".to_string())
    })?;

    if map.len() != 1 {
        return Err(SleepBatchError::ParseFailed(format!(
            "expected exactly 1 key, got {}",
            map.len()
        )));
    }

    let semantic = map
        .get("semantic")
        .ok_or_else(|| SleepBatchError::ParseFailed("missing required key: semantic".to_string()))?
        .as_str()
        .ok_or_else(|| SleepBatchError::ParseFailed("semantic must be a string".to_string()))?;

    Ok(semantic.to_string())
}

fn estimate_text_tokens(text: &str) -> usize {
    text.len().div_ceil(ESTIMATED_CHARS_PER_TOKEN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{EpisodeEventCertainty, EpisodeEventKind};

    fn event(id: &str, title: &str, body_md: &str) -> EpisodeEvent {
        EpisodeEvent {
            id: id.to_string(),
            agent_id: "agent-a".to_string(),
            experienced_at: "2026-01-01T00:00:00Z".to_string(),
            encoded_at: "2026-01-01T00:00:00Z".to_string(),
            kind: EpisodeEventKind::Insight,
            title: title.to_string(),
            body_md: body_md.to_string(),
            ripple_strength: 3,
            certainty: EpisodeEventCertainty::Stated,
            sleep_run_id: "run-1".to_string(),
            source_refs_json: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn parse_semantic_response_extracts_single_file() {
        // Arrange
        let response = serde_json::json!({"semantic": "# Semantic\n\n- fact"}).to_string();

        // Act
        let semantic = parse_semantic_response(&response).expect("should parse");

        // Assert
        assert_eq!(semantic, "# Semantic\n\n- fact");
    }

    #[test]
    fn parse_semantic_response_rejects_extra_keys_and_non_string_values() {
        // Arrange
        let responses = [
            r#"{"semantic":"s","prospective":"p"}"#,
            r#"{"summary_md":"s"}"#,
            r#"{"semantic":42}"#,
            r#"{"semantic":"s","extra":null}"#,
        ];

        // Act / Assert
        for response in responses {
            let error = parse_semantic_response(response).expect_err("should fail");
            assert!(matches!(error, SleepBatchError::ParseFailed(_)));
        }
    }

    #[test]
    fn parse_semantic_response_normalizes_wrapped_json() {
        // Arrange
        let responses = [
            "```json\n{\"semantic\": \"s\"}\n```",
            "<thinking>hmm</thinking>{\"semantic\": \"s\"}",
            "Here is the result:\n{\"semantic\": \"s\"}",
        ];

        // Act / Assert
        for response in responses {
            let semantic = parse_semantic_response(response).expect("should parse");
            assert_eq!(semantic, "s");
        }
    }

    #[test]
    fn build_event_batches_preserves_order_and_splits_within_bound() {
        // Arrange
        let events = vec![
            event("e-1", "first", "body-1"),
            event("e-2", "second", "body-2"),
            event("e-3", "third", "body-3"),
        ];

        // Act
        let batches = build_event_batches(&events, 200);

        // Assert
        assert_eq!(batches.len(), 3);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&batches[0]).expect("batch json");
        assert_eq!(parsed[0]["title"], "first");
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&batches[2]).expect("batch json");
        assert_eq!(parsed[0]["title"], "third");
        for batch in &batches {
            assert!(batch.len() <= 200, "batch exceeds bound: {}", batch.len());
        }
    }

    #[test]
    fn build_event_batches_keeps_oversized_event_as_its_own_batch() {
        // Arrange
        let events = vec![
            event("e-1", "small", "x"),
            event("e-2", "huge", &"y".repeat(100)),
        ];

        // Act
        let batches = build_event_batches(&events, 220);

        // Assert
        assert_eq!(batches.len(), 2);
        assert_eq!(
            serde_json::from_str::<Vec<serde_json::Value>>(&batches[1]).expect("json")[0]["title"],
            "huge"
        );
        assert!(batches[1].len() > 220);
    }

    #[test]
    fn build_event_batches_omits_event_ids() {
        // Arrange
        let events = vec![event("secret-event-id", "title", "body")];

        // Act
        let batches = build_event_batches(&events, 10_000);

        // Assert
        assert_eq!(batches.len(), 1);
        assert!(!batches[0].contains("secret-event-id"));
        assert!(!batches[0].contains("\"id\""));
    }

    #[test]
    fn build_semantic_input_rejects_context_overflow() {
        // Arrange / Act
        let error = build_semantic_input("agent-a", "x".repeat(300), "y".repeat(300), 100)
            .expect_err("should overflow");

        // Assert
        assert!(matches!(error, SleepBatchError::ContextOverflow { .. }));
    }

    #[test]
    fn build_semantic_input_rejects_unsafe_agent_id() {
        // Arrange / Act
        let error = build_semantic_input("../escape", String::new(), String::new(), 100_000)
            .expect_err("should reject");

        // Assert
        assert!(matches!(error, SleepBatchError::UnsafeAgentId(_)));
    }

    #[test]
    fn build_semantic_system_prompt_escapes_and_contains_only_memory_and_events() {
        // Arrange
        let input = SemanticInput {
            agent_id: "agent-a".to_string(),
            semantic: "# Semantic <old>".to_string(),
            events_json: r#"[{"title":"<event> & more"}]"#.to_string(),
        };

        // Act
        let prompt = build_semantic_system_prompt(&input);

        // Assert
        assert!(prompt.contains("<memory-semantic>"));
        assert!(prompt.contains("<episode-events>"));
        assert!(!prompt.contains("<sessions>"));
        assert!(!prompt.contains("<memory-prospective>"));
        assert!(!prompt.contains("<old>"));
        assert!(!prompt.contains("<event>"));
        assert!(prompt.contains("&lt;old&gt;"));
        assert!(prompt.contains("&lt;event&gt;"));
    }
}
