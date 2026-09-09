//! WebSocket ゲートウェイを実装するモジュール。
//!
//! 接続ハンドシェイク、chat.send の受付、RunHub からのイベント転送を担う。

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::time::timeout;
use uuid::Uuid;

use super::auth;
use super::stream::{SendRequest, accept_web_input};
use super::{RunEvent, WEB_ACTOR, WebState};

#[derive(Deserialize)]
struct DeltaData {
    delta: String,
}

#[derive(Deserialize, Default)]
struct DoneData {
    response: Option<String>,
}

#[derive(Deserialize, Default)]
struct ErrorData {
    error: Option<String>,
}

const PROTOCOL_VERSION: u64 = 1;
const MAX_WS_CONNECTIONS: usize = 64;
const MAX_WS_TEXT_BYTES: usize = 64 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

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
#[serde(rename_all = "camelCase")]
struct ChallengePayload {
    protocol: u64,
    conn_id: String,
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
    #[serde(default)]
    auth_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatSendParams {
    #[serde(alias = "session_key", alias = "key")]
    session_key: String,
    message: String,
    /// Agent selected by the WebUI when the session key is not yet persisted.
    agent_id: Option<String>,
    /// Durable request id for deduplication. Mirrors `SendRequest::request_id`
    /// on the REST path; it is independent from the enclosing frame's RPC id.
    /// The Web runtime converts it into `context.request_key` so a re-delivered
    /// `chat.send` maps to the same Turn instead of a duplicate.
    request_id: Option<String>,
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
    session_key: String,
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
    terminal: bool,
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

struct SocketRequestContext<'a> {
    tx: &'a mpsc::UnboundedSender<Message>,
    connected: &'a AtomicBool,
    conn_id: &'a str,
    forwarded_runs: &'a Arc<Mutex<HashSet<String>>>,
}

/// Upgrades an authenticated request into the web gateway WebSocket.
pub(super) async fn ws_handler(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    State(state): State<WebState>,
) -> impl IntoResponse {
    let snapshot = state.app_state.config_manager.current_blocking();
    if !auth::is_ws_origin_allowed(&headers, &snapshot.config) {
        return (
            StatusCode::FORBIDDEN,
            "invalid_origin: websocket origin not allowed",
        )
            .into_response();
    }

    if state
        .active_ws_connections
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
            (current < MAX_WS_CONNECTIONS).then_some(current + 1)
        })
        .is_err()
    {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_connections: websocket connection limit exceeded",
        )
            .into_response();
    }

    ws.max_message_size(MAX_WS_TEXT_BYTES)
        .max_frame_size(MAX_WS_TEXT_BYTES)
        .on_upgrade(move |socket| handle_socket(socket, state))
        .into_response()
}

async fn handle_socket(socket: WebSocket, state: WebState) {
    let _connection_permit = ConnectionPermit::new(state.active_ws_connections.clone());
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
        ChallengePayload {
            protocol: PROTOCOL_VERSION,
            conn_id: conn_id.clone(),
        },
    )
    .is_err()
    {
        let _ = writer.await;
        return;
    }

    let connected = Arc::new(AtomicBool::new(false));
    let forwarded_runs = Arc::new(Mutex::new(HashSet::new()));
    // 接続完了前は connect を期限付きで待ち、以降は通常の受信ループとして扱う。
    while let Some(Ok(message)) = receive_next_message(&mut receiver, &connected).await {
        let Message::Text(text) = message else {
            continue;
        };
        if text.len() > MAX_WS_TEXT_BYTES {
            if send_error(
                &out_tx,
                "invalid",
                "message_too_large",
                format!("message exceeds {MAX_WS_TEXT_BYTES} bytes"),
            )
            .is_err()
            {
                break;
            }
            continue;
        }

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
            ClientFrame::Request { id, method, params } => {
                let request_context = SocketRequestContext {
                    tx: &out_tx,
                    connected: &connected,
                    conn_id: &conn_id,
                    forwarded_runs: &forwarded_runs,
                };
                if handle_request(&state, request_context, id, method, params).await {
                    break;
                }
            }
        }
    }

    // writer を閉じて送信タスクも終了させ、接続ライフサイクルをここで完結させる。
    drop(out_tx);
    let _ = writer.await;
}

async fn handle_request(
    state: &WebState,
    context: SocketRequestContext<'_>,
    id: String,
    method: String,
    params: serde_json::Value,
) -> bool {
    match method.as_str() {
        "connect" => handle_connect(state, context, &id, params),
        "chat.send" => handle_chat_send(state, context, &id, params).await,
        _ => send_error(
            context.tx,
            &id,
            "unknown_method",
            format!("unknown method: {method}"),
        )
        .is_err(),
    }
}

fn handle_connect(
    state: &WebState,
    context: SocketRequestContext<'_>,
    id: &str,
    params: serde_json::Value,
) -> bool {
    if context.connected.load(Ordering::SeqCst) {
        return send_error(
            context.tx,
            id,
            "already_connected",
            "connection already established".to_string(),
        )
        .is_err();
    }

    let payload = match serde_json::from_value::<ConnectParams>(params) {
        Ok(payload) => payload,
        Err(error) => {
            return send_error(context.tx, id, "invalid_params", error.to_string()).is_err();
        }
    };

    if payload.min_protocol > PROTOCOL_VERSION || payload.max_protocol < PROTOCOL_VERSION {
        return send_error(
            context.tx,
            id,
            "unsupported_protocol",
            format!("server supports protocol {PROTOCOL_VERSION}"),
        )
        .is_err();
    }

    let snapshot = state.app_state.config_manager.current_blocking();
    if !auth::is_valid_ws_token(&snapshot.config, payload.auth_token.as_deref()) {
        return send_error(
            context.tx,
            id,
            "unauthorized",
            "invalid web auth token".to_string(),
        )
        .is_err();
    }

    context.connected.store(true, Ordering::SeqCst);
    send_response(
        context.tx,
        id,
        ConnectPayload {
            protocol: PROTOCOL_VERSION,
            server: ConnectServer {
                version: env!("CARGO_PKG_VERSION").to_string(),
                conn_id: context.conn_id.to_string(),
            },
            features: ConnectFeatures {
                methods: vec!["connect", "chat.send"],
                events: vec![
                    "connect.challenge",
                    "chat",
                    "tool_start",
                    "tool_result",
                    "user_input",
                ],
            },
        },
    )
    .is_err()
}

async fn handle_chat_send(
    state: &WebState,
    context: SocketRequestContext<'_>,
    id: &str,
    params: serde_json::Value,
) -> bool {
    if !context.connected.load(Ordering::SeqCst) {
        return send_error(context.tx, id, "not_connected", "connect first".to_string()).is_err();
    }

    let payload = match serde_json::from_value::<ChatSendParams>(params) {
        Ok(payload) => payload,
        Err(error) => {
            return send_error(context.tx, id, "invalid_params", error.to_string()).is_err();
        }
    };

    let request = SendRequest {
        session_key: Some(payload.session_key),
        message: payload.message,
        agent_id: payload.agent_id,
        request_id: payload.request_id,
    };

    let accepted = match accept_web_input(state.clone(), request, WEB_ACTOR).await {
        Ok(accepted) => accepted,
        Err((status, message)) => {
            return send_error(
                context.tx,
                id,
                if status == StatusCode::BAD_REQUEST {
                    "invalid_params"
                } else if status == StatusCode::TOO_MANY_REQUESTS {
                    "busy"
                } else if status == StatusCode::CONFLICT {
                    "request_conflict"
                } else {
                    "internal_error"
                },
                message,
            )
            .is_err();
        }
    };
    let super::stream::AcceptedWebInput { started, status } = accepted;

    if send_response(
        context.tx,
        id,
        ChatAckPayload {
            run_id: started.run_id.clone(),
            session_key: started.session_key.clone(),
            status,
        },
    )
    .is_err()
    {
        return true;
    }

    let should_forward = context
        .forwarded_runs
        .lock()
        .expect("forwarded runs lock")
        .insert(started.run_id.clone());
    if should_forward {
        spawn_chat_stream_forwarder(
            state.clone(),
            context.tx.clone(),
            started.run_id,
            started.session_key,
            Arc::clone(context.forwarded_runs),
        );
    }
    false
}

fn spawn_chat_stream_forwarder(
    state: WebState,
    tx: mpsc::UnboundedSender<Message>,
    run_id: String,
    session_key: String,
    forwarded_runs: Arc<Mutex<HashSet<String>>>,
) {
    tokio::spawn(async move {
        forward_chat_stream(state, tx, run_id, session_key, forwarded_runs).await;
    });
}

async fn forward_chat_stream(
    state: WebState,
    tx: mpsc::UnboundedSender<Message>,
    run_id: String,
    session_key: String,
    forwarded_runs: Arc<Mutex<HashSet<String>>>,
) {
    let _registration = ForwardedRunRegistration {
        run_id: run_id.clone(),
        forwarded_runs,
    };
    let Ok((mut rx, replay, done, _, _)) = state
        .run_hub
        .subscribe_with_replay(&run_id, None, WEB_ACTOR, false)
        .await
    else {
        return;
    };

    let sequence = Arc::new(AtomicU64::new(1));
    // まず保持済みイベントを流し、その後に live イベントへ追従する。
    for event in replay {
        if forward_run_event(&tx, &run_id, &session_key, &sequence, event) {
            return;
        }
    }

    if done {
        return;
    }

    loop {
        match rx.recv().await {
            Ok(event) => {
                if forward_run_event(&tx, &run_id, &session_key, &sequence, event) {
                    break;
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}

struct ForwardedRunRegistration {
    run_id: String,
    forwarded_runs: Arc<Mutex<HashSet<String>>>,
}

impl Drop for ForwardedRunRegistration {
    fn drop(&mut self) {
        self.forwarded_runs
            .lock()
            .expect("forwarded runs lock")
            .remove(&self.run_id);
    }
}

async fn receive_next_message(
    receiver: &mut futures_util::stream::SplitStream<WebSocket>,
    connected: &AtomicBool,
) -> Option<Result<Message, axum::Error>> {
    if connected.load(Ordering::SeqCst) {
        return receiver.next().await;
    }

    timeout(CONNECT_TIMEOUT, receiver.next())
        .await
        .ok()
        .flatten()
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
            let Ok(data) = serde_json::from_str::<DeltaData>(&event.data) else {
                return false;
            };
            if data.delta.is_empty() {
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
                    content: vec![GatewayChatContent {
                        kind: "text",
                        text: data.delta,
                    }],
                }),
                error_message: None,
                terminal: event.terminal,
            };
            send_event(tx, "chat", gateway_event).is_err()
        }
        "done" => {
            let data = serde_json::from_str::<DoneData>(&event.data).unwrap_or_default();
            let seq = sequence.fetch_add(1, Ordering::SeqCst);
            let gateway_event = GatewayChatEvent {
                run_id: run_id.to_string(),
                session_key: session_key.to_string(),
                seq,
                state: "done",
                message: data.response.and_then(|text| {
                    if text.is_empty() {
                        None
                    } else {
                        Some(GatewayChatMessage {
                            role: "assistant",
                            content: vec![GatewayChatContent { kind: "text", text }],
                        })
                    }
                }),
                error_message: None,
                terminal: event.terminal,
            };
            if send_event(tx, "chat", gateway_event).is_err() {
                return true;
            }
            event.terminal
        }
        "error" => {
            let data = serde_json::from_str::<ErrorData>(&event.data).unwrap_or_default();
            let seq = sequence.fetch_add(1, Ordering::SeqCst);
            let gateway_event = GatewayChatEvent {
                run_id: run_id.to_string(),
                session_key: session_key.to_string(),
                seq,
                state: "error",
                message: None,
                error_message: Some(data.error.unwrap_or_else(|| "stream error".to_string())),
                terminal: event.terminal,
            };
            if send_event(tx, "chat", gateway_event).is_err() {
                return true;
            }
            event.terminal
        }
        "tool_start" => {
            let Ok(payload) = serde_json::from_str::<serde_json::Value>(&event.data) else {
                return false;
            };
            send_event(
                tx,
                "tool_start",
                routed_event_payload(run_id, session_key, payload),
            )
            .is_err()
        }
        "tool_result" => {
            let Ok(payload) = serde_json::from_str::<serde_json::Value>(&event.data) else {
                return false;
            };
            send_event(
                tx,
                "tool_result",
                routed_event_payload(run_id, session_key, payload),
            )
            .is_err()
        }
        "user_input" => {
            let Ok(payload) = serde_json::from_str::<serde_json::Value>(&event.data) else {
                return false;
            };
            send_event(
                tx,
                "user_input",
                routed_event_payload(run_id, session_key, payload),
            )
            .is_err()
        }
        _ => false,
    }
}

fn routed_event_payload(
    run_id: &str,
    session_key: &str,
    payload: serde_json::Value,
) -> serde_json::Value {
    let mut object = payload.as_object().cloned().unwrap_or_default();
    object.insert("runId".to_string(), serde_json::json!(run_id));
    object.insert("sessionKey".to_string(), serde_json::json!(session_key));
    serde_json::Value::Object(object)
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
    let frame: ResponseFrame<()> = ResponseFrame {
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

struct ConnectionPermit {
    active_ws_connections: Arc<AtomicUsize>,
}

impl ConnectionPermit {
    fn new(active_ws_connections: Arc<AtomicUsize>) -> Self {
        Self {
            active_ws_connections,
        }
    }
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.active_ws_connections.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::ws::Message;

    use crate::channels::web::RunHub;
    use crate::channels::web::stream::resolve_send_request;
    use crate::error::LlmError;
    use crate::llm::{LlmProvider, Message as LlmMessage, MessagesResponse};
    use crate::test_util::build_state_with_provider;

    fn collect_text_messages(rx: &mut mpsc::UnboundedReceiver<Message>) -> Vec<String> {
        let mut result = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            if let Message::Text(text) = msg {
                result.push(text.to_string());
            }
        }
        result
    }

    struct StubLlm;

    #[async_trait::async_trait]
    impl LlmProvider for StubLlm {
        fn provider_name(&self) -> &str {
            "stub"
        }

        fn model_name(&self) -> &str {
            "stub-model"
        }

        async fn send_message(
            &self,
            _system: &str,
            _messages: Arc<Vec<LlmMessage>>,
            _tools: Option<Arc<Vec<crate::llm::ToolDefinition>>>,
        ) -> Result<MessagesResponse, LlmError> {
            Ok(MessagesResponse {
                content: "stub reply".to_string(),
                reasoning_content: None,
                tool_calls: Vec::new(),
                usage: None,
            })
        }

        async fn send_message_streaming(
            &self,
            system: &str,
            messages: Arc<Vec<LlmMessage>>,
            tools: Option<Arc<Vec<crate::llm::ToolDefinition>>>,
            on_delta: &(dyn Fn(String) + Send + Sync),
        ) -> Result<MessagesResponse, LlmError> {
            let _ = on_delta;
            self.send_message(system, messages, tools).await
        }
    }

    fn test_web_state(dir: &tempfile::TempDir) -> WebState {
        let state_root = dir.path().to_string_lossy().to_string();
        let app_state = build_state_with_provider(&state_root, Box::new(StubLlm));
        WebState {
            app_state: Arc::new(app_state),
            config_path: None,
            run_hub: RunHub::default(),
            active_ws_connections: Arc::new(AtomicUsize::new(0)),
        }
    }

    #[test]
    fn ws_chat_event_includes_session_key() {
        let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
        let seq = AtomicU64::new(0);

        let test_session = "test-session";

        let delta_event = RunEvent {
            id: 1,
            event: "delta".to_string(),
            data: r#"{"delta":"chunk"}"#.to_string(),
            terminal: false,
        };
        forward_run_event(&tx, "run-1", test_session, &seq, delta_event);

        let done_event = RunEvent {
            id: 2,
            event: "done".to_string(),
            data: r#"{"response":"final"}"#.to_string(),
            terminal: true,
        };
        forward_run_event(&tx, "run-1", test_session, &seq, done_event);

        let messages = collect_text_messages(&mut rx);
        assert_eq!(messages.len(), 2);

        for msg in &messages {
            let parsed: serde_json::Value = serde_json::from_str(msg).unwrap();
            assert_eq!(parsed["event"], "chat");
            assert_eq!(
                parsed["payload"]["sessionKey"], "test-session",
                "sessionKey must be present in every chat event"
            );
        }

        let (tx2, mut rx2) = mpsc::unbounded_channel::<Message>();
        let seq2 = AtomicU64::new(0);
        let error_event = RunEvent {
            id: 1,
            event: "error".to_string(),
            data: r#"{"error":"fail"}"#.to_string(),
            terminal: true,
        };
        forward_run_event(&tx2, "run-2", test_session, &seq2, error_event);

        let error_messages = collect_text_messages(&mut rx2);
        assert_eq!(error_messages.len(), 1);
        let parsed: serde_json::Value = serde_json::from_str(&error_messages[0]).unwrap();
        assert_eq!(parsed["payload"]["sessionKey"], "test-session");
    }

    #[test]
    fn ws_delta_without_intermediate_value() {
        let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
        let seq = AtomicU64::new(1);

        let delta_event = RunEvent {
            id: 1,
            event: "delta".to_string(),
            data: r#"{"delta":"hello world"}"#.to_string(),
            terminal: false,
        };

        let should_stop = forward_run_event(&tx, "run-1", "sess-1", &seq, delta_event);
        assert!(!should_stop, "delta event should not terminate the stream");

        let messages = collect_text_messages(&mut rx);
        assert_eq!(messages.len(), 1);

        let parsed: serde_json::Value = serde_json::from_str(&messages[0]).unwrap();
        assert_eq!(parsed["type"], "event");
        assert_eq!(parsed["event"], "chat");

        let payload = &parsed["payload"];
        assert_eq!(payload["runId"], "run-1");
        assert_eq!(payload["sessionKey"], "sess-1");
        assert_eq!(payload["seq"], 1);
        assert_eq!(payload["state"], "delta");
        assert_eq!(payload["message"]["role"], "assistant");
        assert_eq!(payload["message"]["content"][0]["type"], "text");
        assert_eq!(payload["message"]["content"][0]["text"], "hello world");
        assert!(payload.get("errorMessage").is_none());

        assert_eq!(seq.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn ws_delta_with_empty_text_is_skipped() {
        let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
        let seq = AtomicU64::new(1);

        let delta_event = RunEvent {
            id: 1,
            event: "delta".to_string(),
            data: r#"{"delta":""}"#.to_string(),
            terminal: false,
        };

        let should_stop = forward_run_event(&tx, "run-1", "sess-1", &seq, delta_event);
        assert!(!should_stop);
        let messages = collect_text_messages(&mut rx);
        assert!(messages.is_empty());
        assert_eq!(seq.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn ws_done_event_structure() {
        let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
        let seq = AtomicU64::new(5);

        let done_event = RunEvent {
            id: 10,
            event: "done".to_string(),
            data: r#"{"response":"final answer"}"#.to_string(),
            terminal: true,
        };

        let should_stop = forward_run_event(&tx, "run-42", "sess-done", &seq, done_event);
        assert!(should_stop, "done event should terminate the stream");

        let messages = collect_text_messages(&mut rx);
        assert_eq!(messages.len(), 1);

        let parsed: serde_json::Value = serde_json::from_str(&messages[0]).unwrap();
        assert_eq!(parsed["type"], "event");
        assert_eq!(parsed["event"], "chat");

        let payload = &parsed["payload"];
        assert_eq!(payload["runId"], "run-42");
        assert_eq!(payload["sessionKey"], "sess-done");
        assert_eq!(payload["seq"], 5);
        assert_eq!(payload["state"], "done");
        assert_eq!(payload["message"]["role"], "assistant");
        assert_eq!(payload["message"]["content"][0]["text"], "final answer");
    }

    #[test]
    fn ws_done_without_response_has_no_message() {
        let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
        let seq = AtomicU64::new(1);

        let done_event = RunEvent {
            id: 1,
            event: "done".to_string(),
            data: r#"{"response":""}"#.to_string(),
            terminal: true,
        };

        let should_stop = forward_run_event(&tx, "run-1", "sess-1", &seq, done_event);
        assert!(should_stop);

        let messages = collect_text_messages(&mut rx);
        assert_eq!(messages.len(), 1);

        let parsed: serde_json::Value = serde_json::from_str(&messages[0]).unwrap();
        let payload = &parsed["payload"];
        assert!(payload.get("message").is_none());
    }

    #[test]
    fn ws_error_event_structure() {
        let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
        let seq = AtomicU64::new(1);

        let error_event = RunEvent {
            id: 1,
            event: "error".to_string(),
            data: r#"{"error":"something went wrong"}"#.to_string(),
            terminal: true,
        };

        let should_stop = forward_run_event(&tx, "run-1", "sess-1", &seq, error_event);
        assert!(should_stop, "error event should terminate the stream");

        let messages = collect_text_messages(&mut rx);
        assert_eq!(messages.len(), 1);

        let parsed: serde_json::Value = serde_json::from_str(&messages[0]).unwrap();
        let payload = &parsed["payload"];
        assert_eq!(payload["state"], "error");
        assert_eq!(payload["errorMessage"], "something went wrong");
        assert!(payload.get("message").is_none());
    }

    #[test]
    fn ws_nonterminal_error_does_not_stop_the_shared_run() {
        let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
        let seq = AtomicU64::new(1);

        let error_event = RunEvent {
            id: 1,
            event: "error".to_string(),
            data: r#"{"error":"parent failed"}"#.to_string(),
            terminal: false,
        };
        assert!(!forward_run_event(
            &tx,
            "run-shared",
            "sess-1",
            &seq,
            error_event
        ));

        let done_event = RunEvent {
            id: 2,
            event: "done".to_string(),
            data: r#"{"response":"follow-up response"}"#.to_string(),
            terminal: true,
        };
        assert!(forward_run_event(
            &tx,
            "run-shared",
            "sess-1",
            &seq,
            done_event
        ));

        let messages = collect_text_messages(&mut rx);
        assert_eq!(messages.len(), 2);
        let parent_error: serde_json::Value = serde_json::from_str(&messages[0]).unwrap();
        assert_eq!(parent_error["payload"]["terminal"], false);
        let child_done: serde_json::Value = serde_json::from_str(&messages[1]).unwrap();
        assert_eq!(child_done["payload"]["terminal"], true);
        assert_eq!(
            child_done["payload"]["message"]["content"][0]["text"],
            "follow-up response"
        );
    }

    #[test]
    fn ws_nonterminal_done_does_not_stop_the_shared_run() {
        let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
        let seq = AtomicU64::new(1);

        assert!(!forward_run_event(
            &tx,
            "run-shared",
            "sess-1",
            &seq,
            RunEvent {
                id: 1,
                event: "done".to_string(),
                data: r#"{"response":"follow-up A"}"#.to_string(),
                terminal: false,
            }
        ));
        assert!(!forward_run_event(
            &tx,
            "run-shared",
            "sess-1",
            &seq,
            RunEvent {
                id: 2,
                event: "error".to_string(),
                data: r#"{"error":"follow-up A failed later"}"#.to_string(),
                terminal: false,
            }
        ));
        assert!(forward_run_event(
            &tx,
            "run-shared",
            "sess-1",
            &seq,
            RunEvent {
                id: 3,
                event: "done".to_string(),
                data: r#"{"response":"follow-up B"}"#.to_string(),
                terminal: true,
            }
        ));

        let messages = collect_text_messages(&mut rx);
        assert_eq!(messages.len(), 3);
        let first_done: serde_json::Value = serde_json::from_str(&messages[0]).unwrap();
        assert_eq!(first_done["payload"]["terminal"], false);
        let last_done: serde_json::Value = serde_json::from_str(&messages[2]).unwrap();
        assert_eq!(last_done["payload"]["terminal"], true);
    }

    #[tokio::test]
    async fn ws_replays_parent_error_and_child_events_from_one_run() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_web_state(&dir);
        state
            .run_hub
            .create("shared-replay", WEB_ACTOR.to_string())
            .await;
        state
            .run_hub
            .publish_agent_error(
                "shared-replay",
                r#"{"error":"parent failed"}"#.to_string(),
                false,
            )
            .await;
        state
            .run_hub
            .publish(
                "shared-replay",
                "user_input",
                r#"{"messageId":"follow-up","senderId":"web-user","text":"continue","timestamp":"2026-09-09T00:00:00Z"}"#.to_string(),
            )
            .await;
        state
            .run_hub
            .publish(
                "shared-replay",
                "done",
                r#"{"response":"continued"}"#.to_string(),
            )
            .await;
        let (tx, mut rx) = mpsc::unbounded_channel::<Message>();

        // Act
        forward_chat_stream(
            state,
            tx,
            "shared-replay".to_string(),
            "sess-1".to_string(),
            Arc::new(Mutex::new(HashSet::new())),
        )
        .await;

        // Assert
        let messages = collect_text_messages(&mut rx);
        assert_eq!(messages.len(), 3);
        let parent_error: serde_json::Value = serde_json::from_str(&messages[0]).unwrap();
        assert_eq!(parent_error["payload"]["terminal"], false);
        let user_input: serde_json::Value = serde_json::from_str(&messages[1]).unwrap();
        assert_eq!(user_input["event"], "user_input");
        let child_done: serde_json::Value = serde_json::from_str(&messages[2]).unwrap();
        assert_eq!(child_done["payload"]["terminal"], true);
    }

    #[tokio::test]
    async fn ws_chat_send_accepts_message_and_returns_run_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_web_state(&dir);

        let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
        let connected = AtomicBool::new(true);
        let forwarded_runs = Arc::new(Mutex::new(HashSet::new()));

        let context = SocketRequestContext {
            tx: &tx,
            connected: &connected,
            conn_id: "test-conn",
            forwarded_runs: &forwarded_runs,
        };

        let params = serde_json::json!({
            "sessionKey": "main",
            "agentId": "default",
            "message": "hello",
            "requestId": "durable-request-1"
        });

        let _ = handle_chat_send(&state, context, "rpc-attempt-1", params).await;

        let messages = collect_text_messages(&mut rx);
        assert_eq!(messages.len(), 1, "exactly one response frame expected");

        let parsed: serde_json::Value = serde_json::from_str(&messages[0]).unwrap();
        assert_eq!(parsed["type"], "res");
        assert_eq!(parsed["id"], "rpc-attempt-1");
        assert_eq!(parsed["ok"], true);

        let payload = &parsed["payload"];
        assert!(payload["runId"].as_str().is_some(), "runId must be present");
        assert_eq!(payload["status"], "accepted");
    }

    #[tokio::test]
    async fn ws_chat_send_forwards_one_run_once_per_connection() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_web_state(&dir);
        let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
        let connected = AtomicBool::new(true);
        let forwarded_runs = Arc::new(Mutex::new(HashSet::new()));
        let params = serde_json::json!({
            "sessionKey": "duplicate-forward-session",
            "agentId": "default",
            "message": "hello",
            "requestId": "duplicate-forward-request"
        });

        // Act: the second delivery is idempotent and must reuse the same
        // connection-local forwarding registration.
        let first_context = SocketRequestContext {
            tx: &tx,
            connected: &connected,
            conn_id: "test-conn",
            forwarded_runs: &forwarded_runs,
        };
        handle_chat_send(&state, first_context, "req-1", params.clone()).await;
        let second_context = SocketRequestContext {
            tx: &tx,
            connected: &connected,
            conn_id: "test-conn",
            forwarded_runs: &forwarded_runs,
        };
        handle_chat_send(&state, second_context, "req-2", params).await;

        // Assert
        assert_eq!(forwarded_runs.lock().unwrap().len(), 1);
        let responses = collect_text_messages(&mut rx)
            .into_iter()
            .filter_map(|message| {
                let parsed: serde_json::Value = serde_json::from_str(&message).unwrap();
                (parsed["type"] == "res").then_some(parsed)
            })
            .collect::<Vec<_>>();
        assert_eq!(responses.len(), 2);
        assert_eq!(
            responses[0]["payload"]["runId"],
            responses[1]["payload"]["runId"]
        );
    }

    #[tokio::test]
    async fn ws_chat_send_queues_ordinary_message_behind_active_turn() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_web_state(&dir);
        let active_context = resolve_send_request(
            &state,
            &SendRequest {
                session_key: Some("queued-session".to_string()),
                message: "active".to_string(),
                agent_id: Some("default".to_string()),
                request_id: Some("active-request".to_string()),
            },
            WEB_ACTOR,
        )
        .await
        .expect("resolve active turn")
        .context;
        assert_eq!(
            active_context.session_key(),
            "web:queued-session:agent:default"
        );
        assert!(matches!(
            state
                .app_state
                .turn_scheduler
                .submit(crate::runtime::turn::ScheduledTurn {
                    turn_id: "active-turn".to_string(),
                    origin_id: "active-origin".to_string(),
                    context: active_context,
                    input: "active".to_string(),
                    config_snapshot: None,
                    received_at: None,
                    response_delivery: crate::runtime::turn::ResponseDelivery::Channel,
                }),
            crate::runtime::turn::ScheduleResult::Started(_)
        ));

        let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
        let connected = AtomicBool::new(true);
        let forwarded_runs = Arc::new(Mutex::new(HashSet::new()));
        let context = SocketRequestContext {
            tx: &tx,
            connected: &connected,
            conn_id: "test-conn",
            forwarded_runs: &forwarded_runs,
        };

        // Act
        let stopped = handle_chat_send(
            &state,
            context,
            "req-queued",
            serde_json::json!({
                "sessionKey": "queued-session",
                "agentId": "default",
                "message": "queued message",
                "requestId": "queued-request"
            }),
        )
        .await;

        // Assert
        assert!(!stopped);
        let messages = collect_text_messages(&mut rx);
        assert_eq!(messages.len(), 1);
        let response: serde_json::Value = serde_json::from_str(&messages[0]).unwrap();
        assert_eq!(response["ok"], true);
        assert_eq!(response["payload"]["status"], "queued");
        assert_ne!(response["payload"]["runId"], "active-run");
    }

    #[tokio::test]
    async fn ws_chat_send_stages_same_session_follow_up_with_raw_session_key() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_web_state(&dir);
        let chat_id = state
            .app_state
            .db
            .resolve_or_create_chat_id(
                "web",
                "web:active-follow-up:agent:default",
                None,
                "web",
                "default",
            )
            .expect("create chat");
        let turn_id = match state
            .app_state
            .db
            .accept_or_get_turn(crate::storage::AcceptTurnParams {
                chat_id,
                request_key: "active-turn-request",
                config_revision: 1,
                config_fingerprint: Some("fingerprint"),
                request_payload_hash: "payload",
                origin_id: None,
                scheduled_request_json: None,
            })
            .expect("accept active turn")
        {
            crate::storage::AcceptOutcome::Created(run) => run.turn_id,
            crate::storage::AcceptOutcome::Existing(_) => panic!("expected new turn"),
        };
        state
            .app_state
            .db
            .get_conn()
            .expect("connection")
            .execute(
                "UPDATE turn_runs SET state = 'tools_pending' WHERE turn_id = ?1",
                rusqlite::params![&turn_id],
            )
            .expect("seed tools pending state");

        let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
        let connected = AtomicBool::new(true);
        let forwarded_runs = Arc::new(Mutex::new(HashSet::new()));
        let context = SocketRequestContext {
            tx: &tx,
            connected: &connected,
            conn_id: "test-conn",
            forwarded_runs: &forwarded_runs,
        };
        let resolved = resolve_send_request(
            &state,
            &SendRequest {
                session_key: Some("active-follow-up".to_string()),
                message: "follow-up".to_string(),
                agent_id: Some("default".to_string()),
                request_id: Some("follow-up-request".to_string()),
            },
            WEB_ACTOR,
        )
        .await
        .expect("resolve follow-up");
        assert_eq!(resolved.session_key, format!("chat:{chat_id}"));

        // Act
        let stopped = handle_chat_send(
            &state,
            context,
            "req-follow-up",
            serde_json::json!({
                "sessionKey": "active-follow-up",
                "agentId": "default",
                "message": "follow-up",
                "requestId": "follow-up-request"
            }),
        )
        .await;

        // Assert
        assert!(!stopped);
        let messages = collect_text_messages(&mut rx);
        assert_eq!(messages.len(), 1);
        let response: serde_json::Value = serde_json::from_str(&messages[0]).unwrap();
        assert_eq!(response["ok"], true, "response={response}");
        assert_eq!(response["payload"]["runId"], turn_id);
        assert_eq!(response["payload"]["status"], "queued");
        let staged = state
            .app_state
            .db
            .list_staged_user_messages(&turn_id)
            .expect("staged message");
        assert_eq!(staged.len(), 1);
        assert_eq!(staged[0].id, "web:follow-up-request");
        assert_eq!(staged[0].content, "follow-up");
    }

    #[tokio::test]
    async fn ws_chat_send_accepts_unknown_session_while_another_session_runs() {
        // Arrange
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_web_state(&dir);
        let chat_count = |state: &WebState| {
            state
                .app_state
                .db
                .get_conn()
                .expect("connection")
                .query_row("SELECT COUNT(*) FROM chats", [], |row| row.get::<_, i64>(0))
                .expect("count chats")
        };
        let before = chat_count(&state);
        let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
        let connected = AtomicBool::new(true);
        let forwarded_runs = Arc::new(Mutex::new(HashSet::new()));
        let context = SocketRequestContext {
            tx: &tx,
            connected: &connected,
            conn_id: "test-conn",
            forwarded_runs: &forwarded_runs,
        };

        // Act
        let stopped = handle_chat_send(
            &state,
            context,
            "req-unknown-session",
            serde_json::json!({
                "sessionKey": "not-yet-created",
                "agentId": "default",
                "message": "follow-up"
            }),
        )
        .await;

        // Assert
        assert!(!stopped);
        let messages = collect_text_messages(&mut rx);
        assert_eq!(messages.len(), 1);
        let response: serde_json::Value = serde_json::from_str(&messages[0]).unwrap();
        assert_eq!(response["ok"], true);
        assert_eq!(response["payload"]["status"], "accepted");
        assert_eq!(chat_count(&state), before + 1);
    }

    #[test]
    fn ws_forwards_user_input_events() {
        let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
        let sequence = AtomicU64::new(1);
        let event = RunEvent {
            id: 1,
            event: "user_input".to_string(),
            data: serde_json::json!({
                "messageId": "web:req-2",
                "senderId": "web-user",
                "text": "follow-up",
                "timestamp": "2026-08-28T12:00:00Z"
            })
            .to_string(),
            terminal: false,
        };

        assert!(!forward_run_event(&tx, "run-1", "sess-1", &sequence, event));

        let messages = collect_text_messages(&mut rx);
        assert_eq!(messages.len(), 1);
        let parsed: serde_json::Value = serde_json::from_str(&messages[0]).unwrap();
        assert_eq!(parsed["event"], "user_input");
        assert_eq!(parsed["payload"]["messageId"], "web:req-2");
        assert_eq!(parsed["payload"]["runId"], "run-1");
        assert_eq!(parsed["payload"]["sessionKey"], "sess-1");
    }

    #[test]
    fn ws_forwards_tool_start_and_result_events() {
        let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
        let seq = AtomicU64::new(1);

        let start_event = RunEvent {
            id: 1,
            event: "tool_start".to_string(),
            data: serde_json::to_string(&serde_json::json!({
                "callId": "call-1",
                "name": "read",
                "input": {"path": "a.txt"}
            }))
            .unwrap(),
            terminal: false,
        };
        assert!(!forward_run_event(
            &tx,
            "run-1",
            "sess-1",
            &seq,
            start_event
        ));

        let result_event = RunEvent {
            id: 2,
            event: "tool_result".to_string(),
            data: serde_json::to_string(&serde_json::json!({
                "callId": "call-1",
                "name": "read",
                "isError": false,
                "preview": "done",
                "durationMs": 42
            }))
            .unwrap(),
            terminal: false,
        };
        assert!(!forward_run_event(
            &tx,
            "run-1",
            "sess-1",
            &seq,
            result_event
        ));

        let messages = collect_text_messages(&mut rx);
        assert_eq!(messages.len(), 2);

        let start: serde_json::Value = serde_json::from_str(&messages[0]).unwrap();
        assert_eq!(start["type"], "event");
        assert_eq!(start["event"], "tool_start");
        assert_eq!(start["payload"]["callId"], "call-1");
        assert_eq!(start["payload"]["input"]["path"], "a.txt");
        assert_eq!(start["payload"]["runId"], "run-1");
        assert_eq!(start["payload"]["sessionKey"], "sess-1");

        let result: serde_json::Value = serde_json::from_str(&messages[1]).unwrap();
        assert_eq!(result["event"], "tool_result");
        assert_eq!(result["payload"]["isError"], false);
        assert_eq!(result["payload"]["durationMs"], 42);
        assert_eq!(result["payload"]["runId"], "run-1");
        assert_eq!(result["payload"]["sessionKey"], "sess-1");
    }
}
