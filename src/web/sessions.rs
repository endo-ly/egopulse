//! Web UI 用のセッション一覧・履歴取得 API。
//!
//! 保存済みセッションを Web 向けのキー形式に正規化して返す。

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::storage::call_blocking;

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
    pub last_message_time: String,
    pub last_message_preview: Option<String>,
}

/// Lists persisted sessions that can be opened from the web UI.
pub(super) async fn list_sessions(
    State(state): State<WebState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let db = state.app_state.db.clone();
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
            let session_key = if session.channel == "web" {
                web_session_key(&session.surface_thread)
            } else {
                format!("chat:{}", session.chat_id)
            };
            SessionItem {
                label: session
                    .chat_title
                    .clone()
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or_else(|| session_key.clone()),
                session_key,
                chat_id: session.chat_id,
                channel: session.channel,
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
    let db = state.app_state.db.clone();

    let chat_id = match parsed_chat_id {
        Some(chat_id) => chat_id,
        None => {
            let external_chat_id = web_external_chat_id(&session_key);
            match call_blocking(db.clone(), {
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

    let messages = match call_blocking(db, move |db| db.get_recent_messages(chat_id, limit)).await {
        Ok(messages) => messages,
        Err(error) => {
            tracing::warn!(chat_id, error = %error, "failed to load message history");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"ok": false, "messages": [], "error": error.to_string()})),
            ));
        }
    };

    Ok(Json(serde_json::json!({
        "ok": true,
        "session_key": session_key,
        "messages": messages.into_iter().map(|message| serde_json::json!({
            "id": message.id,
            "sender_name": message.sender_name,
            "content": message.content,
            "is_from_bot": message.is_from_bot,
            "timestamp": message.timestamp,
        })).collect::<Vec<_>>()
    })))
}

#[cfg(test)]
mod tests {
    use super::parse_chat_id_from_session_key;

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
}
