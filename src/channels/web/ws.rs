//! WebSocket ゲートウェイを実装するモジュール。
//!
//! 接続ハンドシェイク、chat.send の受付、RunHub からのイベント転送を担う。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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
use super::stream::{SendRequest, start_stream_run};
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
const MAX_IN_FLIGHT_CHAT_SENDS_PER_CONNECTION: usize = 1;
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

struct SocketRequestContext<'a> {
    tx: &'a mpsc::UnboundedSender<Message>,
    connected: &'a AtomicBool,
    in_flight_chat_sends: &'a Arc<AtomicUsize>,
    conn_id: &'a str,
}

/// Upgrades an authenticated request into the web gateway WebSocket.
pub(super) async fn ws_handler(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    State(state): State<WebState>,
) -> impl IntoResponse {
    if !auth::is_ws_origin_allowed(&headers, &state.app_state.config) {
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
    let in_flight_chat_sends = Arc::new(AtomicUsize::new(0));

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
                    in_flight_chat_sends: &in_flight_chat_sends,
                    conn_id: &conn_id,
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

    if !auth::is_valid_ws_token(&state.app_state.config, payload.auth_token.as_deref()) {
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
                events: vec!["connect.challenge", "chat", "tool_start", "tool_result"],
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

    if !try_acquire_chat_send(context.in_flight_chat_sends) {
        return send_error(
            context.tx,
            id,
            "busy",
            "another chat.send is still running".to_string(),
        )
        .is_err();
    }

    let in_flight_permit = InFlightChatPermit::new(context.in_flight_chat_sends.clone());
    let started = match start_stream_run(
        state.clone(),
        SendRequest {
            session_key: Some(payload.session_key),
            message: payload.message,
            request_id: None,
        },
        WEB_ACTOR,
    )
    .await
    {
        Ok(started) => started,
        Err((status, message)) => {
            drop(in_flight_permit);
            return send_error(
                context.tx,
                id,
                if status == axum::http::StatusCode::BAD_REQUEST {
                    "invalid_params"
                } else {
                    "internal_error"
                },
                message,
            )
            .is_err();
        }
    };

    if send_response(
        context.tx,
        id,
        ChatAckPayload {
            run_id: started.run_id.clone(),
            status: "accepted",
        },
    )
    .is_err()
    {
        return true;
    }

    spawn_chat_stream_forwarder(
        state.clone(),
        context.tx.clone(),
        in_flight_permit,
        started.run_id,
        started.session_key,
    );
    false
}

fn try_acquire_chat_send(in_flight_chat_sends: &AtomicUsize) -> bool {
    in_flight_chat_sends
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
            (current < MAX_IN_FLIGHT_CHAT_SENDS_PER_CONNECTION).then_some(current + 1)
        })
        .is_ok()
}

fn spawn_chat_stream_forwarder(
    state: WebState,
    tx: mpsc::UnboundedSender<Message>,
    stream_permit: InFlightChatPermit,
    run_id: String,
    session_key: String,
) {
    tokio::spawn(async move {
        let _stream_permit = stream_permit;
        forward_chat_stream(state, tx, run_id, session_key).await;
    });
}

async fn forward_chat_stream(
    state: WebState,
    tx: mpsc::UnboundedSender<Message>,
    run_id: String,
    session_key: String,
) {
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
            };
            if send_event(tx, "chat", gateway_event).is_err() {
                return true;
            }
            true
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
            };
            if send_event(tx, "chat", gateway_event).is_err() {
                return true;
            }
            true
        }
        "tool_start" => {
            let Ok(payload) = serde_json::from_str::<serde_json::Value>(&event.data) else {
                return false;
            };
            send_event(tx, "tool_start", payload).is_err()
        }
        "tool_result" => {
            let Ok(payload) = serde_json::from_str::<serde_json::Value>(&event.data) else {
                return false;
            };
            send_event(tx, "tool_result", payload).is_err()
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

struct InFlightChatPermit {
    in_flight_chat_sends: Arc<AtomicUsize>,
}

impl InFlightChatPermit {
    fn new(in_flight_chat_sends: Arc<AtomicUsize>) -> Self {
        Self {
            in_flight_chat_sends,
        }
    }
}

impl Drop for InFlightChatPermit {
    fn drop(&mut self) {
        self.in_flight_chat_sends.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::ws::Message;

    use crate::channels::web::RunHub;
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
        };
        forward_run_event(&tx, "run-1", test_session, &seq, delta_event);

        let done_event = RunEvent {
            id: 2,
            event: "done".to_string(),
            data: r#"{"response":"final"}"#.to_string(),
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

    #[tokio::test]
    async fn ws_chat_send_accepts_message_and_returns_run_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = test_web_state(&dir);

        let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
        let connected = AtomicBool::new(true);
        let in_flight = Arc::new(AtomicUsize::new(0));

        let context = SocketRequestContext {
            tx: &tx,
            connected: &connected,
            in_flight_chat_sends: &in_flight,
            conn_id: "test-conn",
        };

        let params = serde_json::json!({
            "sessionKey": "main",
            "message": "hello"
        });

        let _ = handle_chat_send(&state, context, "req-1", params).await;

        let messages = collect_text_messages(&mut rx);
        assert_eq!(messages.len(), 1, "exactly one response frame expected");

        let parsed: serde_json::Value = serde_json::from_str(&messages[0]).unwrap();
        assert_eq!(parsed["type"], "res");
        assert_eq!(parsed["id"], "req-1");
        assert_eq!(parsed["ok"], true);

        let payload = &parsed["payload"];
        assert!(payload["runId"].as_str().is_some(), "runId must be present");
        assert_eq!(payload["status"], "accepted");
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

        let result: serde_json::Value = serde_json::from_str(&messages[1]).unwrap();
        assert_eq!(result["event"], "tool_result");
        assert_eq!(result["payload"]["isError"], false);
        assert_eq!(result["payload"]["durationMs"], 42);
    }
}
