//! Call 1 — Event extraction from session chunks.

use std::str::FromStr;
use std::sync::Arc;

use tracing::warn;

use crate::llm::{LlmProvider, Message};
use crate::storage::{EpisodeEventCertainty, EpisodeEventKind};

use super::batch::SleepBatchError;
use super::prompt::{escape_xml_content, normalize_llm_response, preview_raw_response};

/// Guard message injected on retry when the event extraction response is not valid JSON.
const EVENTS_RETRY_GUARD: &str = "\
Your previous response was not valid JSON. \
You must respond with ONLY a JSON object containing exactly one key: \
\"events\" (an array of episode event objects). \
Do not include any other keys, markdown formatting, code blocks, or explanatory text. \
Output the raw JSON object and nothing else.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExtractedEvent {
    pub experienced_at: String,
    pub kind: EpisodeEventKind,
    pub title: String,
    pub body_md: String,
    pub ripple_strength: i64,
    pub certainty: EpisodeEventCertainty,
}

#[derive(Debug, Clone)]
pub(crate) struct ExtractEventsOutput {
    pub events: Vec<ExtractedEvent>,
}

pub(crate) fn parse_extract_events_response(
    response: &str,
) -> Result<ExtractEventsOutput, SleepBatchError> {
    let normalized = normalize_llm_response(response);
    let value: serde_json::Value = serde_json::from_str(&normalized)
        .map_err(|e| SleepBatchError::ParseFailed(format!("invalid JSON: {e}")))?;

    let map = value.as_object().ok_or_else(|| {
        SleepBatchError::ParseFailed("response must be a JSON object".to_string())
    })?;

    let events_val = map
        .get("events")
        .ok_or_else(|| SleepBatchError::ParseFailed("missing required key: events".to_string()))?;

    let events_arr = events_val
        .as_array()
        .ok_or_else(|| SleepBatchError::ParseFailed("events must be an array".to_string()))?;

    let mut events = Vec::with_capacity(events_arr.len());
    for (i, ev) in events_arr.iter().enumerate() {
        let obj = ev.as_object().ok_or_else(|| {
            SleepBatchError::ParseFailed(format!("events[{i}] must be an object"))
        })?;

        let experienced_at = obj
            .get("experienced_at")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                SleepBatchError::ParseFailed(format!(
                    "events[{i}]: missing or invalid experienced_at"
                ))
            })?
            .to_string();

        let kind_str = obj.get("kind").and_then(|v| v.as_str()).ok_or_else(|| {
            SleepBatchError::ParseFailed(format!("events[{i}]: missing or invalid kind"))
        })?;
        let kind = EpisodeEventKind::from_str(kind_str).map_err(SleepBatchError::ParseFailed)?;

        let title = obj
            .get("title")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                SleepBatchError::ParseFailed(format!("events[{i}]: missing or invalid title"))
            })?
            .to_string();

        let body_md = obj
            .get("body_md")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                SleepBatchError::ParseFailed(format!("events[{i}]: missing or invalid body_md"))
            })?
            .to_string();

        let ripple_strength = obj
            .get("ripple_strength")
            .and_then(|v| v.as_i64())
            .unwrap_or(3);
        if !(1..=5).contains(&ripple_strength) {
            return Err(SleepBatchError::ParseFailed(format!(
                "events[{i}]: ripple_strength must be 1-5, got {ripple_strength}"
            )));
        }

        let certainty_str = obj
            .get("certainty")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                SleepBatchError::ParseFailed(format!("events[{i}]: missing or invalid certainty"))
            })?;
        let certainty =
            EpisodeEventCertainty::from_str(certainty_str).map_err(SleepBatchError::ParseFailed)?;

        events.push(ExtractedEvent {
            experienced_at,
            kind,
            title,
            body_md,
            ripple_strength,
            certainty,
        });
    }

    Ok(ExtractEventsOutput { events })
}

pub(crate) fn build_extract_system_prompt(agent_id: &str, sessions_text: &str) -> String {
    let mut prompt = include_str!("extract_prompt.md").replace("{AGENT_NAME}", agent_id);

    if !sessions_text.is_empty() {
        prompt.push_str("\n\n## 入力データ\n\n<sessions>\n");
        prompt.push_str(&escape_xml_content(sessions_text));
        prompt.push_str("\n</sessions>\n");
    }

    prompt
}

pub(crate) async fn send_extract_events_request(
    provider: &Arc<dyn LlmProvider>,
    agent_id: &str,
    system_prompt: &str,
    chunk_index: usize,
    total_chunks: usize,
) -> Result<(ExtractEventsOutput, i64, i64), SleepBatchError> {
    let user_message = Message::text(
        "user",
        format!("Extract episode events from chunk {chunk_index} of {total_chunks}."),
    );
    let response = provider
        .send_message(system_prompt, Arc::new(vec![user_message.clone()]), None)
        .await
        .map_err(|e| SleepBatchError::Llm(e.to_string()))?;

    let (output, response) = match parse_extract_events_response(&response.content) {
        Ok(parsed) => (parsed, response),
        Err(first_error) => {
            warn!(
                agent_id = %agent_id,
                chunk_index,
                total_chunks,
                error = %first_error,
                raw_preview = %preview_raw_response(&response.content),
                "event extraction parse failed; retrying once with events guard"
            );
            let first_input = response.usage.as_ref().map_or(0, |u| u.input_tokens);
            let first_output = response.usage.as_ref().map_or(0, |u| u.output_tokens);

            let retry_messages = vec![
                user_message,
                Message::text("assistant", &response.content),
                Message::text("user", EVENTS_RETRY_GUARD),
            ];
            let retry_response = provider
                .send_message(system_prompt, Arc::new(retry_messages), None)
                .await
                .map_err(|e| SleepBatchError::Llm(e.to_string()))?;

            let retry_input = retry_response.usage.as_ref().map_or(0, |u| u.input_tokens);
            let retry_output = retry_response.usage.as_ref().map_or(0, |u| u.output_tokens);
            let combined_input = first_input.saturating_add(retry_input);
            let combined_output = first_output.saturating_add(retry_output);

            match parse_extract_events_response(&retry_response.content) {
                Ok(parsed) => {
                    return Ok((parsed, combined_input, combined_output));
                }
                Err(retry_error) => {
                    warn!(
                        agent_id = %agent_id,
                        chunk_index,
                        total_chunks,
                        error = %retry_error,
                        raw_preview = %preview_raw_response(&retry_response.content),
                        "event extraction retry also failed"
                    );
                    return Err(retry_error);
                }
            }
        }
    };

    let input_tokens = response.usage.as_ref().map_or(0, |u| u.input_tokens);
    let output_tokens = response.usage.as_ref().map_or(0, |u| u.output_tokens);
    Ok((output, input_tokens, output_tokens))
}

pub(crate) async fn run_extract_events_for_chunks(
    provider: &Arc<dyn LlmProvider>,
    agent_id: &str,
    session_chunks: Vec<String>,
    total_chunks: usize,
) -> Result<(Vec<ExtractedEvent>, i64, i64), SleepBatchError> {
    let mut all_events = Vec::new();
    let mut total_input = 0_i64;
    let mut total_output = 0_i64;

    for (index, sessions_text) in session_chunks.into_iter().enumerate() {
        let system_prompt = build_extract_system_prompt(agent_id, &sessions_text);
        match send_extract_events_request(
            provider,
            agent_id,
            &system_prompt,
            index + 1,
            total_chunks,
        )
        .await
        {
            Ok((output, in_tok, out_tok)) => {
                total_input = total_input.saturating_add(in_tok);
                total_output = total_output.saturating_add(out_tok);
                all_events.extend(output.events);
            }
            Err(e) => {
                warn!(
                    agent_id = %agent_id,
                    chunk_index = index + 1,
                    total_chunks,
                    error = %e,
                    "event extraction failed for chunk, skipping"
                );
            }
        }
    }

    Ok((all_events, total_input, total_output))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_extract_events_response_valid() {
        let response = r#"{"events":[{"experienced_at":"2025-01-01T00:01:00Z","kind":"decision","title":"test","body_md":"body","ripple_strength":3,"certainty":"stated"}]}"#;
        let result = parse_extract_events_response(response).expect("parse");
        assert_eq!(result.events.len(), 1);
        assert_eq!(result.events[0].title, "test");
        assert_eq!(result.events[0].kind, EpisodeEventKind::Decision);
        assert_eq!(result.events[0].ripple_strength, 3);
        assert_eq!(result.events[0].certainty, EpisodeEventCertainty::Stated);
    }

    #[test]
    fn parse_extract_events_response_missing_events_key() {
        let response = r#"{"not_events":[]}"#;
        let err = parse_extract_events_response(response).unwrap_err();
        assert!(matches!(err, SleepBatchError::ParseFailed(_)));
    }

    #[test]
    fn parse_extract_events_response_invalid_event_kind() {
        let response = r#"{"events":[{"experienced_at":"2025-01-01T00:01:00Z","kind":"unknown","title":"t","body_md":"b","ripple_strength":3,"certainty":"stated"}]}"#;
        let err = parse_extract_events_response(response).unwrap_err();
        assert!(matches!(err, SleepBatchError::ParseFailed(_)));
    }

    #[test]
    fn parse_extract_events_response_salience_out_of_range() {
        let response = r#"{"events":[{"experienced_at":"2025-01-01T00:01:00Z","kind":"decision","title":"t","body_md":"b","ripple_strength":6,"certainty":"stated"}]}"#;
        let err = parse_extract_events_response(response).unwrap_err();
        assert!(matches!(err, SleepBatchError::ParseFailed(_)));
    }

    #[test]
    fn parse_extract_events_response_certainty_invalid() {
        let response = r#"{"events":[{"experienced_at":"2025-01-01T00:01:00Z","kind":"decision","title":"t","body_md":"b","ripple_strength":3,"certainty":"invalid"}]}"#;
        let err = parse_extract_events_response(response).unwrap_err();
        assert!(matches!(err, SleepBatchError::ParseFailed(_)));
    }

    #[test]
    fn parse_extract_events_response_with_thinking_tags() {
        let response = "<thinking>let me think</thinking>{\"events\":[{\"experienced_at\":\"2025-01-01T00:01:00Z\",\"kind\":\"decision\",\"title\":\"t\",\"body_md\":\"b\",\"ripple_strength\":3,\"certainty\":\"stated\"}]}";
        let result = parse_extract_events_response(response).expect("parse");
        assert_eq!(result.events.len(), 1);
    }

    #[test]
    fn parse_extract_events_response_json_code_block() {
        let response = "```json\n{\"events\":[{\"experienced_at\":\"2025-01-01T00:01:00Z\",\"kind\":\"insight\",\"title\":\"t\",\"body_md\":\"b\",\"ripple_strength\":4,\"certainty\":\"derived\"}]}\n```";
        let result = parse_extract_events_response(response).expect("parse");
        assert_eq!(result.events.len(), 1);
        assert_eq!(result.events[0].kind, EpisodeEventKind::Insight);
    }

    #[test]
    fn build_extract_system_prompt_includes_sessions() {
        let prompt = build_extract_system_prompt("test-agent", "session data here");
        assert!(prompt.contains("session data here"));
    }

    #[test]
    fn build_extract_system_prompt_includes_kinds() {
        let prompt = build_extract_system_prompt("test-agent", "");
        assert!(
            prompt.contains("decision") || prompt.contains("insight") || prompt.contains("anomaly")
        );
    }
}
