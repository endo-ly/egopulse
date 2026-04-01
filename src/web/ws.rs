use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::agent_loop::{SurfaceContext, process_turn_with_events};

use super::{WebState, sse::AgentEvent, web_session_key};

const PROTOCOL_VERSION: u64 = 1;

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ClientFrame {
    #[serde(rename = "req")]
    Request {
        id: String,
        method: String,
        #[serde(default)]
        params: serde_json::Value,
    },
}

#[derive(Debug, Serialize)]
struct ErrorShape {
    code: &'static str,
    message: String,
}

#[derive(Debug, Serialize)]
struct ResponseFrame<T: Serialize> {
    #[serde(rename = "type")]
    kind: &'static str,
    id: String,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    payload: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorShape>,
}

#[derive(Debug, Serialize)]
struct EventFrame<T: Serialize> {
    #[serde(rename = "type")]
    kind: &'static str,
    event: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    payload: Option<T>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectParams {
    min_protocol: u64,
    max_protocol: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatSendParams {
    #[serde(alias = "session_key", alias = "key")]
    session_key: String,
    message: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConnectPayload {
    protocol: u64,
    server: ConnectServer,
    features: ConnectFeatures,
}

#[derive(Debug, Serialize)]
struct ConnectServer {
    version: String,
    conn_id: String,
}

#[derive(Debug, Serialize)]
struct ConnectFeatures {
    methods: Vec<&'static str>,
    events: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatAckPayload {
    run_id: String,
    status: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GatewayChatEvent {
    run_id: String,
    session_key: String,
    seq: u64,
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<GatewayChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_message: Option<String>,
}

#[derive(Debug, Serialize)]
struct GatewayChatMessage {
    role: &'static str,
    content: Vec<GatewayChatContent>,
}

#[derive(Debug, Serialize)]
struct GatewayChatContent {
    #[serde(rename = "type")]
    kind: &'static str,
    text: String,
}

pub(super) async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<WebState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: WebState) {
    let (mut sender, mut receiver) = socket.split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Message>();
    let writer = tokio::spawn(async move {
        while let Some(message) = out_rx.recv().await {
            if sender.send(message).await.is_err() {
                break;
            }
        }
    });

    let conn_id = Uuid::new_v4().to_string();
    if send_event(
        &out_tx,
        "connect.challenge",
        serde_json::json!({
            "protocol": PROTOCOL_VERSION,
            "connId": conn_id,
        }),
    )
    .is_err()
    {
        let _ = writer.await;
        return;
    }

    let connected = Arc::new(std::sync::atomic::AtomicBool::new(false));

    while let Some(Ok(message)) = receiver.next().await {
        let Message::Text(text) = message else {
            continue;
        };

        let frame = match serde_json::from_str::<ClientFrame>(&text) {
            Ok(frame) => frame,
            Err(error) => {
                let _ = send_error(&out_tx, "invalid", "invalid_frame", error.to_string());
                continue;
            }
        };

        match frame {
            ClientFrame::Request { id, method, params } => match method.as_str() {
                "connect" => {
                    let payload = match serde_json::from_value::<ConnectParams>(params) {
                        Ok(payload) => payload,
                        Err(error) => {
                            let _ = send_error(&out_tx, &id, "invalid_params", error.to_string());
                            continue;
                        }
                    };

                    if payload.min_protocol > PROTOCOL_VERSION
                        || payload.max_protocol < PROTOCOL_VERSION
                    {
                        let _ = send_error(
                            &out_tx,
                            &id,
                            "unsupported_protocol",
                            format!("server supports protocol {PROTOCOL_VERSION}"),
                        );
                        continue;
                    }

                    connected.store(true, Ordering::SeqCst);
                    let _ = send_response(
                        &out_tx,
                        &id,
                        ConnectPayload {
                            protocol: PROTOCOL_VERSION,
                            server: ConnectServer {
                                version: env!("CARGO_PKG_VERSION").to_string(),
                                conn_id: conn_id.clone(),
                            },
                            features: ConnectFeatures {
                                methods: vec!["connect", "chat.send"],
                                events: vec!["connect.challenge", "chat"],
                            },
                        },
                    );
                }
                "chat.send" => {
                    if !connected.load(Ordering::SeqCst) {
                        let _ =
                            send_error(&out_tx, &id, "not_connected", "connect first".to_string());
                        continue;
                    }

                    let payload = match serde_json::from_value::<ChatSendParams>(params) {
                        Ok(payload) => payload,
                        Err(error) => {
                            let _ = send_error(&out_tx, &id, "invalid_params", error.to_string());
                            continue;
                        }
                    };

                    let session_key = web_session_key(&payload.session_key);
                    let message = payload.message.trim().to_string();
                    if message.is_empty() {
                        let _ = send_error(
                            &out_tx,
                            &id,
                            "invalid_params",
                            "message is required".to_string(),
                        );
                        continue;
                    }

                    let run_id = Uuid::new_v4().to_string();
                    let _ = send_response(
                        &out_tx,
                        &id,
                        ChatAckPayload {
                            run_id: run_id.clone(),
                            status: "accepted",
                        },
                    );

                    let state_for_task = state.clone();
                    let out_tx_for_task = out_tx.clone();
                    tokio::spawn(async move {
                        let sequence = Arc::new(AtomicU64::new(1));
                        let context = SurfaceContext {
                            channel: "web".to_string(),
                            surface_user: "web-user".to_string(),
                            surface_thread: session_key.clone(),
                            chat_type: "web".to_string(),
                        };

                        let emit =
                            |state_name: &'static str,
                             text: Option<String>,
                             error_message: Option<String>| {
                                let seq = sequence.fetch_add(1, Ordering::SeqCst);
                                let payload = GatewayChatEvent {
                                    run_id: run_id.clone(),
                                    session_key: session_key.clone(),
                                    seq,
                                    state: state_name,
                                    message: text.map(|text| GatewayChatMessage {
                                        role: "assistant",
                                        content: vec![GatewayChatContent { kind: "text", text }],
                                    }),
                                    error_message,
                                };
                                let _ = send_event(&out_tx_for_task, "chat", payload);
                            };

                        let result = process_turn_with_events(
                            &state_for_task.app_state,
                            &context,
                            &message,
                            |event| match event {
                                AgentEvent::TextDelta { delta } => emit("delta", Some(delta), None),
                                AgentEvent::FinalResponse { text } => {
                                    emit("done", Some(text), None)
                                }
                                AgentEvent::Error { message } => emit("error", None, Some(message)),
                                AgentEvent::Iteration { .. }
                                | AgentEvent::ToolStart { .. }
                                | AgentEvent::ToolResult { .. } => {}
                            },
                        )
                        .await;

                        if let Err(error) = result {
                            emit("error", None, Some(error.to_string()));
                        }
                    });
                }
                _ => {
                    let _ = send_error(
                        &out_tx,
                        &id,
                        "unknown_method",
                        format!("unknown method: {method}"),
                    );
                }
            },
        }
    }

    drop(out_tx);
    let _ = writer.await;
}

fn send_response<T: Serialize>(
    tx: &mpsc::UnboundedSender<Message>,
    id: &str,
    payload: T,
) -> Result<(), ()> {
    let frame = ResponseFrame {
        kind: "res",
        id: id.to_string(),
        ok: true,
        payload: Some(payload),
        error: None,
    };
    send_message(tx, &frame)
}

fn send_error(
    tx: &mpsc::UnboundedSender<Message>,
    id: &str,
    code: &'static str,
    message: String,
) -> Result<(), ()> {
    let frame: ResponseFrame<serde_json::Value> = ResponseFrame {
        kind: "res",
        id: id.to_string(),
        ok: false,
        payload: None,
        error: Some(ErrorShape { code, message }),
    };
    send_message(tx, &frame)
}

fn send_event<T: Serialize>(
    tx: &mpsc::UnboundedSender<Message>,
    event: &'static str,
    payload: T,
) -> Result<(), ()> {
    let frame = EventFrame {
        kind: "event",
        event,
        payload: Some(payload),
    };
    send_message(tx, &frame)
}

fn send_message<T: Serialize>(tx: &mpsc::UnboundedSender<Message>, payload: &T) -> Result<(), ()> {
    let text = serde_json::to_string(payload).map_err(|_| ())?;
    tx.send(Message::Text(text.into())).map_err(|_| ())
}
