use axum::Json;
use axum::extract::{Query, State};
use serde::{Deserialize, Serialize};

use crate::storage::call_blocking;

use super::{WebState, web_external_chat_id, web_session_key};

#[derive(Debug, Deserialize)]
pub(super) struct HistoryQuery {
    pub session_key: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub(super) struct SessionItem {
    pub session_key: String,
    pub label: String,
    pub chat_id: i64,
    pub channel: String,
    pub last_message_time: String,
    pub last_message_preview: Option<String>,
}

pub(super) async fn list_sessions(State(state): State<WebState>) -> Json<serde_json::Value> {
    let db = state.app_state.db.clone();
    let sessions = match call_blocking(db, |db| db.list_sessions()).await {
        Ok(sessions) => sessions,
        Err(error) => {
            tracing::warn!(error = %error, "failed to list sessions");
            return Json(
                serde_json::json!({"ok": false, "sessions": [], "error": error.to_string()}),
            );
        }
    };

    let items = sessions
        .into_iter()
        .filter(|session| session.channel == "web")
        .map(|session| {
            let session_key = web_session_key(&session.surface_thread);
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

    Json(serde_json::json!({"ok": true, "sessions": items}))
}

pub(super) async fn get_history(
    State(state): State<WebState>,
    Query(query): Query<HistoryQuery>,
) -> Json<serde_json::Value> {
    let session_key = web_session_key(query.session_key.as_deref().unwrap_or("main"));
    let external_chat_id = web_external_chat_id(&session_key);
    let limit = query.limit.unwrap_or(100);
    let db = state.app_state.db.clone();

    let session_key_for_resolve = session_key.clone();
    let chat_id = match call_blocking(db.clone(), move |db| {
        db.resolve_or_create_chat_id(
            "web",
            &external_chat_id,
            Some(&session_key_for_resolve),
            "web",
        )
    })
    .await
    {
        Ok(id) => id,
        Err(error) => {
            tracing::warn!(session_key = %session_key, error = %error, "failed to resolve web session");
            return Json(
                serde_json::json!({"ok": false, "messages": [], "error": error.to_string()}),
            );
        }
    };

    let messages = match call_blocking(db, move |db| db.get_recent_messages(chat_id, limit)).await {
        Ok(messages) => messages,
        Err(error) => {
            tracing::warn!(chat_id, error = %error, "failed to load message history");
            return Json(
                serde_json::json!({"ok": false, "messages": [], "error": error.to_string()}),
            );
        }
    };

    Json(serde_json::json!({
        "ok": true,
        "session_key": session_key,
        "messages": messages.into_iter().map(|message| serde_json::json!({
            "id": message.id,
            "sender_name": message.sender_name,
            "content": message.content,
            "is_from_bot": message.is_from_bot,
            "timestamp": message.timestamp,
        })).collect::<Vec<_>>()
    }))
}
