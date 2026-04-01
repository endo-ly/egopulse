//! Sessions API handlers.

use axum::Json;
use axum::extract::{Query, State};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::runtime::AppState;
use crate::storage::call_blocking;

#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    pub session_key: Option<String>,
    pub limit: Option<usize>,
}

/// Session item for API response.
#[derive(Debug, Serialize)]
pub struct SessionItem {
    pub session_key: String,
    pub label: String,
    pub chat_id: i64,
    pub channel: String,
    pub last_message_time: String,
    pub last_message_preview: Option<String>,
}

/// List all sessions.
pub async fn list_sessions(state: State<AppState>) -> Json<serde_json::Value> {
    let db = state.db.clone();
    let sessions = call_blocking(db, |db| db.list_sessions())
        .await
        .unwrap_or_default();

    let items: Vec<SessionItem> = sessions
        .into_iter()
        .map(|s| SessionItem {
            session_key: s.surface_thread.clone(),
            label: s.chat_title.unwrap_or(s.surface_thread),
            chat_id: s.chat_id,
            channel: s.channel,
            last_message_time: s.last_message_time,
            last_message_preview: s.last_message_preview,
        })
        .collect();

    Json(json!({
        "ok": true,
        "sessions": items
    }))
}

/// Get message history for a session.
pub async fn get_history(
    state: State<AppState>,
    Query(query): Query<HistoryQuery>,
) -> Json<serde_json::Value> {
    let session_key = query.session_key.unwrap_or_else(|| "main".to_string());
    let limit = query.limit.unwrap_or(50);

    // Resolve chat_id from session_key
    let db = state.db.clone();
    let session_key_for_resolve = session_key.clone();
    let chat_id = match call_blocking(db.clone(), move |db| {
        db.resolve_or_create_chat_id(
            "web",
            &session_key_for_resolve,
            Some(&session_key_for_resolve),
            "web",
        )
    })
    .await
    {
        Ok(id) => id,
        Err(_) => {
            return Json(json!({
                "ok": false,
                "error": "Failed to resolve session"
            }));
        }
    };

    let messages = call_blocking(db, move |db| db.get_recent_messages(chat_id, limit))
        .await
        .unwrap_or_default();

    Json(json!({
        "ok": true,
        "session_key": session_key,
        "messages": messages.iter().map(|m| json!({
            "id": m.id,
            "sender_name": m.sender_name,
            "content": m.content,
            "is_from_bot": m.is_from_bot,
            "timestamp": m.timestamp
        })).collect::<Vec<_>>()
    }))
}
