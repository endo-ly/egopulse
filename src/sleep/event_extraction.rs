//! Event extraction step — extracts episode events from session chunks.

use std::str::FromStr;
use std::sync::Arc;

use tracing::warn;

use crate::agent_loop::formatting::strip_thinking;
use crate::agent_loop::tool_phase::MAX_TOOL_RESULT_TEXT_CHARS;
use crate::channels::utils::text::truncate_by_chars;
use crate::llm::{LlmProvider, Message};
use crate::storage::{
    EpisodeEvent, EpisodeEventCertainty, EpisodeEventKind, SenderKind, StoredMessage,
};

use super::SleepBatchError;
use super::memory_update;
use super::prompt::{escape_xml_content, normalize_llm_response};

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
    let mut prompt = include_str!("prompts/extract_prompt.md").replace("{AGENT_NAME}", agent_id);

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
                    "event extraction failed for chunk"
                );
                return Err(e);
            }
        }
    }

    Ok((all_events, total_input, total_output))
}

pub(crate) fn messages_to_extract_text(messages: &[StoredMessage]) -> String {
    messages
        .iter()
        .map(|msg| {
            let role = match msg.sender_kind {
                SenderKind::User => "user",
                SenderKind::Assistant => "assistant",
                SenderKind::System => "system",
                SenderKind::Tool => "tool",
            };
            let content = if msg.sender_kind == SenderKind::Tool {
                truncate_by_chars(&msg.content, MAX_TOOL_RESULT_TEXT_CHARS)
            } else {
                msg.content.clone()
            };
            let content = strip_thinking(&content).replace('\n', "\\n");
            format!("{} [{}]: {}", msg.timestamp, role, content)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn build_extract_chunks(
    db: &crate::storage::Database,
    sources: &[(i64, &str, &str)],
    from: Option<&str>,
    to: Option<&str>,
    max_tokens: usize,
) -> Result<Vec<String>, SleepBatchError> {
    let max_chars = max_tokens.saturating_mul(3);
    let mut chunks = Vec::new();
    let mut current = String::new();

    for &(chat_id, channel, external_chat_id) in sources {
        let messages = db
            .get_messages_between(chat_id, from, to)
            .map_err(SleepBatchError::Storage)?;
        if messages.is_empty() {
            continue;
        }
        let text = messages_to_extract_text(&messages);
        let session_info = crate::storage::AgentSessionInfo {
            chat_id,
            channel: channel.to_string(),
            external_chat_id: external_chat_id.to_string(),
            updated_at: String::new(),
            message_count: 0,
            estimated_tokens: 0,
        };
        let blocks = memory_update::session_blocks(&session_info, &text, max_chars);
        for block in blocks {
            memory_update::append_chunk_block(&mut chunks, &mut current, block, max_chars);
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }
    Ok(chunks)
}

pub(crate) fn to_episode_events(
    events: Vec<ExtractedEvent>,
    agent_id: &str,
    run_id: &str,
) -> Vec<EpisodeEvent> {
    let now = chrono::Utc::now().to_rfc3339();
    events
        .into_iter()
        .map(|e| EpisodeEvent {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent_id.to_string(),
            experienced_at: e.experienced_at,
            encoded_at: now.clone(),
            kind: e.kind,
            title: e.title,
            body_md: e.body_md,
            ripple_strength: e.ripple_strength,
            certainty: e.certainty,
            sleep_run_id: run_id.to_string(),
            source_refs_json: None,
            created_at: now.clone(),
            updated_at: now.clone(),
        })
        .collect()
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

    // --- messages_to_extract_text ---

    #[test]
    fn messages_to_extract_text_formats_user_message() {
        let messages = vec![StoredMessage {
            id: "m1".to_string(),
            chat_id: 1,
            sender_id: "alice".to_string(),
            content: "hello world".to_string(),
            sender_kind: SenderKind::User,
            timestamp: "2025-01-01T00:00:00Z".to_string(),
            message_kind: crate::storage::MessageKind::Message,
            recipient_agent_id: None,
        }];
        let text = messages_to_extract_text(&messages);
        assert!(text.contains("2025-01-01T00:00:00Z [user]: hello world"));
    }

    #[test]
    fn messages_to_extract_text_formats_assistant_message() {
        let messages = vec![StoredMessage {
            id: "m2".to_string(),
            chat_id: 1,
            sender_id: "lyre".to_string(),
            content: "hi there".to_string(),
            sender_kind: SenderKind::Assistant,
            timestamp: "2025-01-01T00:00:01Z".to_string(),
            message_kind: crate::storage::MessageKind::Message,
            recipient_agent_id: None,
        }];
        let text = messages_to_extract_text(&messages);
        assert!(text.contains("[assistant]: hi there"));
    }

    #[test]
    fn messages_to_extract_text_truncates_long_tool_content() {
        let long_content = "A".repeat(300);
        let messages = vec![StoredMessage {
            id: "m3".to_string(),
            chat_id: 1,
            sender_id: "tool-1".to_string(),
            content: long_content.clone(),
            sender_kind: SenderKind::Tool,
            timestamp: "2025-01-01T00:00:02Z".to_string(),
            message_kind: crate::storage::MessageKind::Message,
            recipient_agent_id: Some("lyre".to_string()),
        }];
        let text = messages_to_extract_text(&messages);
        assert!(text.contains("[tool]:"));
        assert!(text.contains("..."));
        assert!(text.contains(&"A".repeat(50)));
    }

    #[test]
    fn messages_to_extract_text_keeps_short_tool_content() {
        let messages = vec![StoredMessage {
            id: "m4".to_string(),
            chat_id: 1,
            sender_id: "tool-1".to_string(),
            content: "short".to_string(),
            sender_kind: SenderKind::Tool,
            timestamp: "2025-01-01T00:00:03Z".to_string(),
            message_kind: crate::storage::MessageKind::Message,
            recipient_agent_id: Some("lyre".to_string()),
        }];
        let text = messages_to_extract_text(&messages);
        assert!(text.contains("[tool]: short"));
        assert!(!text.contains("..."));
    }

    #[test]
    fn messages_to_extract_text_strips_thinking_tags() {
        let messages = vec![StoredMessage {
            id: "m5".to_string(),
            chat_id: 1,
            sender_id: "lyre".to_string(),
            content: "<thinking>internal</thinking>visible".to_string(),
            sender_kind: SenderKind::Assistant,
            timestamp: "2025-01-01T00:00:04Z".to_string(),
            message_kind: crate::storage::MessageKind::Message,
            recipient_agent_id: None,
        }];
        let text = messages_to_extract_text(&messages);
        assert!(text.contains("visible"));
        assert!(!text.contains("thinking"));
        assert!(!text.contains("internal"));
    }

    #[test]
    fn messages_to_extract_text_joins_multiple_messages() {
        let messages = vec![
            StoredMessage {
                id: "a".to_string(),
                chat_id: 1,
                sender_id: "u".to_string(),
                content: "first".to_string(),
                sender_kind: SenderKind::User,
                timestamp: "2025-01-01T00:00:00Z".to_string(),
                message_kind: crate::storage::MessageKind::Message,
                recipient_agent_id: None,
            },
            StoredMessage {
                id: "b".to_string(),
                chat_id: 1,
                sender_id: "a".to_string(),
                content: "second".to_string(),
                sender_kind: SenderKind::Assistant,
                timestamp: "2025-01-01T00:00:01Z".to_string(),
                message_kind: crate::storage::MessageKind::Message,
                recipient_agent_id: None,
            },
        ];
        let text = messages_to_extract_text(&messages);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("[user]: first"));
        assert!(lines[1].contains("[assistant]: second"));
    }

    // --- build_extract_chunks ---

    fn test_db() -> (crate::storage::Database, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime").join("egopulse.db");
        let db = crate::storage::Database::new(&db_path).expect("db");
        (db, dir)
    }

    fn create_chat(db: &crate::storage::Database, agent_id: &str, suffix: &str) -> i64 {
        db.resolve_or_create_chat_id(
            "test",
            &format!("test:chat{suffix}"),
            Some(&format!("chat{suffix}")),
            "direct",
            agent_id,
        )
        .expect("create chat")
    }

    fn store_msg(db: &crate::storage::Database, id: &str, chat_id: i64, content: &str, ts: &str) {
        let conn = db.get_conn().expect("pool");
        conn.execute(
            "INSERT OR REPLACE INTO messages (id, chat_id, sender_id, content, sender_kind, timestamp, message_kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![id, chat_id, "alice", content, "user", ts, "message"],
        )
        .expect("store message");
    }

    #[test]
    fn build_extract_chunks_returns_no_chunks_when_no_messages() {
        let (db, _dir) = test_db();
        let sources: Vec<(i64, &str, &str)> = vec![];
        let chunks = build_extract_chunks(&db, &sources, None, None, 1000).expect("chunks");
        assert!(chunks.is_empty());
    }

    #[test]
    fn build_extract_chunks_includes_messages_from_source() {
        let (db, _dir) = test_db();
        let chat_id = create_chat(&db, "test-agent", "1");
        store_msg(&db, "m1", chat_id, "hello", "2025-01-01T00:00:00Z");
        store_msg(&db, "m2", chat_id, "world", "2025-01-01T00:00:01Z");

        let sources = vec![(chat_id, "test", "test:chat1")];
        let chunks = build_extract_chunks(&db, &sources, None, None, 10000).expect("chunks");
        let combined = chunks.join("\n");
        assert!(combined.contains("hello"));
        assert!(combined.contains("world"));
    }

    #[test]
    fn build_extract_chunks_respects_from_cutoff() {
        let (db, _dir) = test_db();
        let chat_id = create_chat(&db, "test-agent", "2");
        store_msg(&db, "old", chat_id, "old message", "2025-01-01T00:00:00Z");
        store_msg(&db, "new", chat_id, "new message", "2025-01-02T00:00:00Z");

        let sources = vec![(chat_id, "test", "test:chat2")];
        let chunks = build_extract_chunks(&db, &sources, Some("2025-01-01T12:00:00Z"), None, 10000)
            .expect("chunks");
        let combined = chunks.join("\n");
        assert!(!combined.contains("old message"));
        assert!(combined.contains("new message"));
    }

    #[test]
    fn build_extract_chunks_respects_to_cutoff() {
        let (db, _dir) = test_db();
        let chat_id = create_chat(&db, "test-agent", "3");
        store_msg(&db, "early", chat_id, "early msg", "2025-01-01T00:00:00Z");
        store_msg(&db, "late", chat_id, "late msg", "2025-01-03T00:00:00Z");

        let sources = vec![(chat_id, "test", "test:chat3")];
        let chunks = build_extract_chunks(&db, &sources, None, Some("2025-01-02T00:00:00Z"), 10000)
            .expect("chunks");
        let combined = chunks.join("\n");
        assert!(combined.contains("early msg"));
        assert!(!combined.contains("late msg"));
    }

    #[test]
    fn build_extract_chunks_skips_sources_with_no_matching_messages() {
        let (db, _dir) = test_db();
        let chat_id = create_chat(&db, "test-agent", "4");
        store_msg(&db, "m1", chat_id, "old", "2025-01-01T00:00:00Z");

        let sources = vec![(chat_id, "test", "test:chat4")];
        let chunks = build_extract_chunks(&db, &sources, Some("2025-12-31T00:00:00Z"), None, 10000)
            .expect("chunks");
        assert!(chunks.is_empty());
    }

    #[test]
    fn build_extract_chunks_splits_large_sessions() {
        let (db, _dir) = test_db();
        let chat_id = create_chat(&db, "test-agent", "5");
        let long_content = "X".repeat(500);
        for i in 0..10 {
            store_msg(
                &db,
                &format!("m{i}"),
                chat_id,
                &long_content,
                &format!("2025-01-01T00:00:{i:02}Z"),
            );
        }

        let sources = vec![(chat_id, "test", "test:chat5")];
        let chunks = build_extract_chunks(&db, &sources, None, None, 200).expect("chunks");
        assert!(chunks.len() > 1, "should split into multiple chunks");
    }
}
