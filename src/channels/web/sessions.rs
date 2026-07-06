//! Web UI 用のセッション一覧・履歴取得 API。
//!
//! 保存済みセッションを Web 向けのキー形式に正規化して返す。

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::agent_loop::formatting::is_tool_preview_message;
use crate::storage::{SenderKind, call_blocking};

use super::{WebState, web_external_chat_id, web_session_key};

const MAX_LIMIT: usize = 500;

pub(crate) fn parse_chat_id_from_session_key(key: &str) -> Option<i64> {
    key.strip_prefix("chat:")?.parse::<i64>().ok()
}

#[derive(Debug, Deserialize)]
/// Captures query parameters for loading web chat history.
pub(super) struct HistoryQuery {
    pub session_key: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
/// Describes one persisted session as exposed to the web UI.
pub(super) struct SessionItem {
    pub session_key: String,
    pub label: String,
    pub chat_id: i64,
    pub channel: String,
    pub agent_id: String,
    pub last_message_time: String,
    pub last_message_preview: Option<String>,
}

/// Lists persisted sessions that can be opened from the web UI.
pub(super) async fn list_sessions(
    State(state): State<WebState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let db = Arc::clone(&state.app_state.db);
    let sessions = match call_blocking(db, |db| db.list_sessions()).await {
        Ok(sessions) => sessions,
        Err(error) => {
            tracing::warn!(error = %error, "failed to list sessions");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"ok": false, "sessions": [], "error": error.to_string()})),
            ));
        }
    };

    let items = sessions
        .into_iter()
        .map(|session| {
            // All channels share the canonical `chat:{id}` key so the WebUI can
            // round-trip a selected session back to history via `get_chat_by_id`,
            // regardless of the agent-scoped `external_chat_id` shape.
            let session_key = format!("chat:{}", session.chat_id);
            SessionItem {
                label: session.external_chat_id.clone(),
                session_key,
                chat_id: session.chat_id,
                channel: session.channel,
                agent_id: session.agent_id,
                last_message_time: session.last_message_time,
                last_message_preview: session.last_message_preview,
            }
        })
        .collect::<Vec<_>>();

    Ok(Json(serde_json::json!({"ok": true, "sessions": items})))
}

/// Returns recent messages for a normalized web session.
pub(super) async fn get_history(
    State(state): State<WebState>,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let requested_session_key = query.session_key.as_deref().unwrap_or("main");
    let parsed_chat_id = parse_chat_id_from_session_key(requested_session_key);
    let session_key = parsed_chat_id
        .map(|chat_id| format!("chat:{chat_id}"))
        .unwrap_or_else(|| web_session_key(requested_session_key));
    let limit = std::cmp::min(query.limit.unwrap_or(100), MAX_LIMIT);
    let db = Arc::clone(&state.app_state.db);

    let chat_id = match parsed_chat_id {
        Some(chat_id) => chat_id,
        None => {
            let external_chat_id = web_external_chat_id(&session_key);
            match call_blocking(Arc::clone(&db), {
                let channel = "web".to_string();
                let external_chat_id = external_chat_id.clone();
                move |db| db.resolve_chat_id(&channel, &external_chat_id)
            })
            .await
            {
                Ok(Some(id)) => id,
                Ok(None) => {
                    return Ok(Json(
                        serde_json::json!({"ok": true, "session_key": session_key, "messages": []}),
                    ));
                }
                Err(error) => {
                    tracing::warn!(session_key = %session_key, error = %error, "failed to resolve web session");
                    return Err((
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(
                            serde_json::json!({"ok": false, "messages": [], "error": error.to_string()}),
                        ),
                    ));
                }
            }
        }
    };

    let (messages, tool_calls) = match call_blocking(db, move |db| {
        let messages = db.get_recent_messages(chat_id, limit)?;
        let tool_calls = db.get_tool_calls_for_chat(chat_id)?;
        Ok::<_, crate::error::StorageError>((messages, tool_calls))
    })
    .await
    {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(chat_id, error = %error, "failed to load message history");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"ok": false, "messages": [], "error": error.to_string()})),
            ));
        }
    };

    // Interleave text messages with persisted tool calls by timestamp so the
    // WebUI can render collapsible tool cards inline within the conversation.
    //
    // Tool preview messages (`[tool_call] {name}`, `[tool_result]: ...`,
    // `[tool_error]: ...`) are skipped: the no-narration tool_call form and
    // the result/error forms either duplicate the structured tool cards below
    // or render empty in Markdown (the bracket-prefix parses as a reference
    // link definition). Tool_call previews that lead with agent narration
    // stay.
    let mut entries: Vec<(String, serde_json::Value)> = Vec::new();
    for message in messages {
        if message.sender_kind == SenderKind::Assistant && is_tool_preview_message(&message.content)
        {
            continue;
        }
        let timestamp = message.timestamp.clone();
        entries.push((
            timestamp,
            serde_json::json!({
                "id": message.id,
                "sender_id": message.sender_id,
                "sender_kind": message.sender_kind.to_string(),
                "content": message.content,
                "timestamp": message.timestamp,
                "message_kind": message.message_kind.to_string(),
            }),
        ));
    }
    for tool_call in tool_calls {
        let timestamp = tool_call.timestamp.clone();
        let input = serde_json::from_str::<serde_json::Value>(&tool_call.tool_input)
            .unwrap_or(serde_json::Value::Null);
        let (status, result) = if let Some(ref output) = tool_call.tool_output {
            let parsed = serde_json::from_str::<serde_json::Value>(output)
                .unwrap_or(serde_json::Value::Null);
            let status = parsed
                .get("status")
                .and_then(|s| s.as_str())
                .unwrap_or("success")
                .to_string();
            let result = parsed
                .get("result")
                .and_then(|r| r.as_str())
                .map(String::from)
                .unwrap_or_else(|| output.to_string());
            (status, result)
        } else {
            ("pending".to_string(), String::new())
        };
        entries.push((
            timestamp,
            serde_json::json!({
                "id": format!("tool:{}", tool_call.id),
                "sender_id": "assistant",
                "sender_kind": "assistant",
                "content": serde_json::to_string(&serde_json::json!({
                    "tool": tool_call.tool_name,
                    "status": status,
                    "result": result,
                    "input": input,
                })).unwrap_or_default(),
                "timestamp": tool_call.timestamp,
                "message_kind": "tool_call",
            }),
        ));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let messages_json: Vec<serde_json::Value> =
        entries.into_iter().map(|(_, value)| value).collect();

    Ok(Json(serde_json::json!({
        "ok": true,
        "session_key": session_key,
        "messages": messages_json,
    })))
}

#[cfg(test)]
mod tests {
    use super::{get_history, list_sessions, parse_chat_id_from_session_key};
    use axum::extract::{Query, State as AxumState};

    use crate::channels::web::{RunHub, WebState};
    use crate::error::LlmError;
    use crate::llm::{LlmProvider, Message, MessagesResponse};
    use crate::storage::{MessageKind, StoredMessage};
    use crate::test_util::build_state_with_provider;
    use std::sync::Arc;

    struct DummyLlm;

    #[async_trait::async_trait]
    impl LlmProvider for DummyLlm {
        fn provider_name(&self) -> &str {
            "dummy"
        }

        fn model_name(&self) -> &str {
            "dummy"
        }

        async fn send_message(
            &self,
            _system: &str,
            _messages: Arc<Vec<Message>>,
            _tools: Option<Arc<Vec<crate::llm::ToolDefinition>>>,
        ) -> Result<MessagesResponse, LlmError> {
            panic!("handler tests should not call LLM")
        }
    }

    fn test_web_state(dir: &tempfile::TempDir) -> WebState {
        let state_root = dir.path().to_string_lossy().to_string();
        let app_state = build_state_with_provider(&state_root, Box::new(DummyLlm));
        WebState {
            app_state: Arc::new(app_state),
            config_path: None,
            run_hub: RunHub::default(),
            active_ws_connections: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    fn insert_web_chat(
        db: &crate::storage::Database,
        external_chat_id: &str,
        agent_id: &str,
    ) -> i64 {
        let conn = db.get_conn().expect("pool");
        conn.execute(
            "INSERT INTO chats (channel, external_chat_id, chat_type, agent_id, last_message_time)
             VALUES ('web', ?1, 'dm', ?2, '2024-01-01T00:00:00Z')",
            rusqlite::params![external_chat_id, agent_id],
        )
        .expect("insert chat");
        conn.query_row(
            "SELECT chat_id FROM chats WHERE channel = 'web' AND external_chat_id = ?1",
            rusqlite::params![external_chat_id],
            |row| row.get::<_, i64>(0),
        )
        .expect("get chat_id")
    }

    #[tokio::test]
    async fn api_sessions_returns_agent_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);
        let db = Arc::clone(&web_state.app_state.db);

        insert_web_chat(&db, "web:session-1", "lyre");
        insert_web_chat(&db, "web:session-2", "ace");
        insert_web_chat(&db, "web:session-3", "vega");

        let result = list_sessions(AxumState(web_state)).await.expect("ok");
        let body = result.0;
        let sessions = body["sessions"].as_array().expect("sessions array");
        assert_eq!(sessions.len(), 3);

        let agent_ids: Vec<&str> = sessions
            .iter()
            .map(|s| s["agent_id"].as_str().expect("agent_id present"))
            .collect();
        assert!(agent_ids.contains(&"lyre"));
        assert!(agent_ids.contains(&"ace"));
        assert!(agent_ids.contains(&"vega"));
    }

    #[tokio::test]
    async fn api_history_returns_message_kind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);
        let db = Arc::clone(&web_state.app_state.db);

        let chat_id = insert_web_chat(&db, "web:main", "default");

        let msg_message = StoredMessage::user(chat_id, "user:web".to_string(), "hello".to_string());
        db.store_message_only(&msg_message).expect("store message");

        let mut msg_event =
            StoredMessage::user(chat_id, "system".to_string(), "system event".to_string());
        msg_event.message_kind = MessageKind::SystemEvent;
        db.store_message_only(&msg_event).expect("store event");

        let query = Query(super::HistoryQuery {
            session_key: Some("main".to_string()),
            limit: None,
        });
        let result = get_history(AxumState(web_state), query).await.expect("ok");
        let body = result.0;

        let messages = body["messages"].as_array().expect("messages array");
        assert_eq!(messages.len(), 2);

        let kinds: Vec<&str> = messages
            .iter()
            .map(|m| m["message_kind"].as_str().expect("message_kind present"))
            .collect();
        assert!(kinds.contains(&"message"));
        assert!(kinds.contains(&"system_event"));
    }

    #[tokio::test]
    async fn api_sessions_exposes_chat_id_key_regardless_of_external_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);
        let db = Arc::clone(&web_state.app_state.db);

        let chat_id = insert_web_chat(&db, "web:session-1:agent:lyre", "lyre");

        let result = list_sessions(AxumState(web_state)).await.expect("ok");
        let body = result.0;
        let sessions = body["sessions"].as_array().expect("sessions array");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["session_key"], format!("chat:{chat_id}"));
    }

    #[tokio::test]
    async fn api_history_interleaves_tool_calls() {
        use crate::storage::ToolCall;

        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);
        let db = Arc::clone(&web_state.app_state.db);

        let chat_id = insert_web_chat(&db, "web:session-tool:agent:lyre", "lyre");

        let user_msg = StoredMessage::user(
            chat_id,
            "user:web".to_string(),
            "please read the file".to_string(),
        );
        db.store_message_only(&user_msg)
            .expect("store user message");

        let tool_call = ToolCall {
            id: "call-1".to_string(),
            chat_id,
            message_id: user_msg.id.clone(),
            tool_name: "read".to_string(),
            tool_input: r#"{"path":"a.txt"}"#.to_string(),
            tool_output: Some("file contents".to_string()),
            timestamp: "2024-01-01T00:00:01Z".to_string(),
        };
        db.store_tool_call(&tool_call).expect("store tool call");

        let query = Query(super::HistoryQuery {
            session_key: Some(format!("chat:{chat_id}")),
            limit: None,
        });
        let result = get_history(AxumState(web_state), query).await.expect("ok");
        let body = result.0;
        let messages = body["messages"].as_array().expect("messages array");
        assert_eq!(messages.len(), 2);

        let tool_message = messages
            .iter()
            .find(|m| m["message_kind"] == "tool_call")
            .expect("tool message present");
        assert_eq!(tool_message["id"], "tool:call-1");
        let content = serde_json::from_str::<serde_json::Value>(
            tool_message["content"].as_str().expect("content string"),
        )
        .expect("content json");
        assert_eq!(content["tool"], "read");
        assert_eq!(content["status"], "success");
        assert_eq!(content["result"], "file contents");
        assert_eq!(content["input"]["path"], "a.txt");
    }

    #[tokio::test]
    async fn api_history_skips_tool_preview_messages() {
        let dir = tempfile::tempdir().expect("tempdir");
        let web_state = test_web_state(&dir);
        let db = Arc::clone(&web_state.app_state.db);

        let chat_id = insert_web_chat(&db, "web:session-preview:agent:lyre", "lyre");

        // Tool previews persisted for text-only channels: filtered because
        // they duplicate the structured tool cards (and the result/error forms
        // render empty in Markdown as reference link definitions).
        db.store_message_only(&StoredMessage::assistant(
            chat_id,
            "lyre".to_string(),
            "[tool_call] read".to_string(),
        ))
        .expect("store tool_call preview");
        db.store_message_only(&StoredMessage::assistant(
            chat_id,
            "lyre".to_string(),
            "[tool_result]: file contents".to_string(),
        ))
        .expect("store tool_result preview");
        db.store_message_only(&StoredMessage::assistant(
            chat_id,
            "lyre".to_string(),
            "[tool_error]: boom".to_string(),
        ))
        .expect("store tool_error preview");

        // A tool_call preview that leads with agent narration stays.
        db.store_message_only(&StoredMessage::assistant(
            chat_id,
            "lyre".to_string(),
            "読みますね [tool_call] read".to_string(),
        ))
        .expect("store tool_call narration");

        // A plain assistant message stays.
        db.store_message_only(&StoredMessage::assistant(
            chat_id,
            "lyre".to_string(),
            "hello there".to_string(),
        ))
        .expect("store plain assistant");

        let query = Query(super::HistoryQuery {
            session_key: Some(format!("chat:{chat_id}")),
            limit: None,
        });
        let result = get_history(AxumState(web_state), query).await.expect("ok");
        let body = result.0;
        let messages = body["messages"].as_array().expect("messages array");

        let contents: Vec<&str> = messages
            .iter()
            .map(|m| m["content"].as_str().expect("content string"))
            .collect();

        assert_eq!(
            messages.len(),
            2,
            "only narration + plain message remain: {contents:?}"
        );
        assert!(contents.contains(&"読みますね [tool_call] read"));
        assert!(contents.contains(&"hello there"));
    }

    #[test]
    fn parses_chat_session_keys() {
        assert_eq!(parse_chat_id_from_session_key("chat:42"), Some(42));
        assert_eq!(parse_chat_id_from_session_key("chat:-7"), Some(-7));
    }

    #[test]
    fn rejects_non_chat_session_keys() {
        assert_eq!(parse_chat_id_from_session_key("main"), None);
        assert_eq!(parse_chat_id_from_session_key("web:main"), None);
        assert_eq!(parse_chat_id_from_session_key("chat:"), None);
        assert_eq!(parse_chat_id_from_session_key("chat:abc"), None);
    }

    #[test]
    fn api_messages_returns_sender_id() {
        let message = crate::storage::StoredMessage::user(
            1,
            "user:discord:123".to_string(),
            "hello".to_string(),
        );
        let json = serde_json::json!({
            "id": message.id,
            "sender_id": message.sender_id,
            "sender_kind": message.sender_kind.to_string(),
            "content": message.content,
            "timestamp": message.timestamp,
        });
        assert_eq!(json["sender_id"], "user:discord:123");
    }

    #[test]
    fn api_messages_returns_sender_kind() {
        let message = crate::storage::StoredMessage::user(
            1,
            "user:discord:123".to_string(),
            "hello".to_string(),
        );
        let json = serde_json::json!({
            "id": message.id,
            "sender_id": message.sender_id,
            "sender_kind": message.sender_kind.to_string(),
            "content": message.content,
            "timestamp": message.timestamp,
        });
        assert_eq!(json["sender_kind"], "user");
    }

    #[test]
    fn api_messages_excludes_sender_name() {
        let message = crate::storage::StoredMessage::user(
            1,
            "user:discord:123".to_string(),
            "hello".to_string(),
        );
        let json = serde_json::json!({
            "id": message.id,
            "sender_id": message.sender_id,
            "sender_kind": message.sender_kind.to_string(),
            "content": message.content,
            "timestamp": message.timestamp,
        });
        assert!(json.get("sender_name").is_none());
    }

    #[test]
    fn api_messages_excludes_is_from_bot() {
        let message = crate::storage::StoredMessage::user(
            1,
            "user:discord:123".to_string(),
            "hello".to_string(),
        );
        let json = serde_json::json!({
            "id": message.id,
            "sender_id": message.sender_id,
            "sender_kind": message.sender_kind.to_string(),
            "content": message.content,
            "timestamp": message.timestamp,
        });
        assert!(json.get("is_from_bot").is_none());
    }
}
