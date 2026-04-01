use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::stream::{SendRequest, start_stream_run};
use super::{RunEvent, WEB_ACTOR, WebState};

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

    let connected = Arc::new(AtomicBool::new(false));

    while let Some(Ok(message)) = receiver.next().await {
        let Message::Text(text) = message else {
            continue;
        };

        let frame = match serde_json::from_str::<ClientFrame>(&text) {
            Ok(frame) => frame,
            Err(error) => {
                if send_error(&out_tx, "invalid", "invalid_frame", error.to_string()).is_err() {
                    break;
                }
                continue;
            }
        };

        match frame {
            ClientFrame::Request { id, method, params } => match method.as_str() {
                "connect" => {
                    let payload = match serde_json::from_value::<ConnectParams>(params) {
                        Ok(payload) => payload,
                        Err(error) => {
                            if send_error(&out_tx, &id, "invalid_params", error.to_string())
                                .is_err()
                            {
                                break;
                            }
                            continue;
                        }
                    };

                    if payload.min_protocol > PROTOCOL_VERSION
                        || payload.max_protocol < PROTOCOL_VERSION
                    {
                        if send_error(
                            &out_tx,
                            &id,
                            "unsupported_protocol",
                            format!("server supports protocol {PROTOCOL_VERSION}"),
                        )
                        .is_err()
                        {
                            break;
                        }
                        continue;
                    }

                    connected.store(true, Ordering::SeqCst);
                    if send_response(
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
                    )
                    .is_err()
                    {
                        break;
                    }
                }
                "chat.send" => {
                    if !connected.load(Ordering::SeqCst) {
                        if send_error(&out_tx, &id, "not_connected", "connect first".to_string())
                            .is_err()
                        {
                            break;
                        }
                        continue;
                    }

                    let payload = match serde_json::from_value::<ChatSendParams>(params) {
                        Ok(payload) => payload,
                        Err(error) => {
                            if send_error(&out_tx, &id, "invalid_params", error.to_string())
                                .is_err()
                            {
                                break;
                            }
                            continue;
                        }
                    };

                    let started = match start_stream_run(
                        state.clone(),
                        SendRequest {
                            session_key: Some(payload.session_key),
                            message: payload.message,
                        },
                        WEB_ACTOR,
                    )
                    .await
                    {
                        Ok(started) => started,
                        Err((status, message)) => {
                            if send_error(
                                &out_tx,
                                &id,
                                if status == axum::http::StatusCode::BAD_REQUEST {
                                    "invalid_params"
                                } else {
                                    "internal_error"
                                },
                                message,
                            )
                            .is_err()
                            {
                                break;
                            }
                            continue;
                        }
                    };

                    if send_response(
                        &out_tx,
                        &id,
                        ChatAckPayload {
                            run_id: started.run_id.clone(),
                            status: "accepted",
                        },
                    )
                    .is_err()
                    {
                        break;
                    }

                    let state_for_stream = state.clone();
                    let out_tx_for_stream = out_tx.clone();
                    let run_id = started.run_id.clone();
                    let session_key = started.session_key.clone();
                    tokio::spawn(async move {
                        let Ok((mut rx, replay, done, _, _)) = state_for_stream
                            .run_hub
                            .subscribe_with_replay(&run_id, None, WEB_ACTOR, false)
                            .await
                        else {
                            return;
                        };

                        let sequence = Arc::new(AtomicU64::new(1));
                        for event in replay {
                            if forward_run_event(
                                &out_tx_for_stream,
                                &run_id,
                                &session_key,
                                &sequence,
                                event,
                            ) {
                                return;
                            }
                        }

                        if done {
                            return;
                        }

                        loop {
                            match rx.recv().await {
                                Ok(event) => {
                                    if forward_run_event(
                                        &out_tx_for_stream,
                                        &run_id,
                                        &session_key,
                                        &sequence,
                                        event,
                                    ) {
                                        break;
                                    }
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                    continue;
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                            }
                        }
                    });
                }
                _ => {
                    if send_error(
                        &out_tx,
                        &id,
                        "unknown_method",
                        format!("unknown method: {method}"),
                    )
                    .is_err()
                    {
                        break;
                    }
                }
            },
        }
    }

    drop(out_tx);
    let _ = writer.await;
}

fn forward_run_event(
    tx: &mpsc::UnboundedSender<Message>,
    run_id: &str,
    session_key: &str,
    sequence: &AtomicU64,
    event: RunEvent,
) -> bool {
    match event.event.as_str() {
        "delta" => {
            let payload =
                serde_json::from_str::<serde_json::Value>(&event.data).unwrap_or_default();
            let text = payload
                .get("delta")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            if text.is_empty() {
                return false;
            }
            let seq = sequence.fetch_add(1, Ordering::SeqCst);
            let gateway_event = GatewayChatEvent {
                run_id: run_id.to_string(),
                session_key: session_key.to_string(),
                seq,
                state: "delta",
                message: Some(GatewayChatMessage {
                    role: "assistant",
                    content: vec![GatewayChatContent { kind: "text", text }],
                }),
                error_message: None,
            };
            send_event(tx, "chat", gateway_event).is_err()
        }
        "done" => {
            let payload =
                serde_json::from_str::<serde_json::Value>(&event.data).unwrap_or_default();
            let text = payload
                .get("response")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let seq = sequence.fetch_add(1, Ordering::SeqCst);
            let gateway_event = GatewayChatEvent {
                run_id: run_id.to_string(),
                session_key: session_key.to_string(),
                seq,
                state: "done",
                message: if text.is_empty() {
                    None
                } else {
                    Some(GatewayChatMessage {
                        role: "assistant",
                        content: vec![GatewayChatContent { kind: "text", text }],
                    })
                },
                error_message: None,
            };
            if send_event(tx, "chat", gateway_event).is_err() {
                return true;
            }
            true
        }
        "error" => {
            let payload =
                serde_json::from_str::<serde_json::Value>(&event.data).unwrap_or_default();
            let message = payload
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("stream error")
                .to_string();
            let seq = sequence.fetch_add(1, Ordering::SeqCst);
            let gateway_event = GatewayChatEvent {
                run_id: run_id.to_string(),
                session_key: session_key.to_string(),
                seq,
                state: "error",
                message: None,
                error_message: Some(message),
            };
            if send_event(tx, "chat", gateway_event).is_err() {
                return true;
            }
            true
        }
        _ => false,
    }
}

fn send_response<T: Serialize>(
    tx: &mpsc::UnboundedSender<Message>,
    id: &str,
    payload: T,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let frame = ResponseFrame {
        kind: "res",
        id: id.to_string(),
        ok: true,
        payload: Some(payload),
        error: None,
    };
    let text = serde_json::to_string(&frame)?;
    tx.send(Message::Text(text.into()))
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
    Ok(())
}

fn send_error(
    tx: &mpsc::UnboundedSender<Message>,
    id: &str,
    code: &'static str,
    message: String,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let frame: ResponseFrame<serde_json::Value> = ResponseFrame {
        kind: "res",
        id: id.to_string(),
        ok: false,
        payload: None,
        error: Some(ErrorShape { code, message }),
    };
    let text = serde_json::to_string(&frame)?;
    tx.send(Message::Text(text.into()))
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
    Ok(())
}

fn send_event<T: Serialize>(
    tx: &mpsc::UnboundedSender<Message>,
    event: &'static str,
    payload: T,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let frame = EventFrame {
        kind: "event",
        event,
        payload: Some(payload),
    };
    let text = serde_json::to_string(&frame)?;
    tx.send(Message::Text(text.into()))
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
    Ok(())
}
