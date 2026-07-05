//! ツール進捗コーディネータ。
//!
//! エージェントターン中の [`AgentEvent`] ストリームを購読し、A3 遅延型 × B2 編集式
//! 累積ログのポリシーでチャネルの [`ToolProgressSink`] を駆動する。
//!
//! - ターン開始から `DELAY` 未満の速いターンは進捗を投稿しない（ノイズゼロ）。
//! - 単一の長時間ツールが `DELAY` を跨いだ時点で遅延タイマーが `begin` を発火する。
//! - 進捗本文にはツール名・状態・所要時間のみを含め、`input` / `preview` は絶対に
//!   含めない（公開チャネルへの情報漏洩防止）。
//! - `recv() = None`（イベントストリーム EOF）または `FinalResponse` で確実に close する。

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc::UnboundedReceiver;
use tracing::warn;

use crate::agent_loop::event::AgentEvent;
use crate::channels::adapter::{ToolProgressHandle, ToolProgressSink};

/// ターンがこの時間を超えて実行されると進捗表示を開始する。
const DELAY: Duration = Duration::from_secs(5);

/// 進捗編集（`update`）の最小間隔。Discord/Telegram のレート制限を回避する。
const MIN_EDIT_INTERVAL: Duration = Duration::from_millis(800);

/// 1 ターン分のツール進捗を駆動するコーディネータ。
///
/// `sink = None` のとき（チャネル非対応 or 設定 OFF）は完全な no-op となり、
/// イベントストリームを最後まで読み捨てるだけである。
pub(crate) struct ToolProgressCoordinator {
    sink: Option<Arc<dyn ToolProgressSink>>,
    external_chat_id: String,
    delay: Duration,
    min_edit_interval: Duration,
}

impl ToolProgressCoordinator {
    /// 実運用用のデフォルトタイミング（`DELAY` / `MIN_EDIT_INTERVAL`）で生成する。
    pub(crate) fn new(sink: Option<Arc<dyn ToolProgressSink>>, external_chat_id: String) -> Self {
        Self::with_timings(sink, external_chat_id, DELAY, MIN_EDIT_INTERVAL)
    }

    /// タイミングを明示指定して生成する（単体テストで高速化するため）。
    pub(crate) fn with_timings(
        sink: Option<Arc<dyn ToolProgressSink>>,
        external_chat_id: String,
        delay: Duration,
        min_edit_interval: Duration,
    ) -> Self {
        Self {
            sink,
            external_chat_id,
            delay,
            min_edit_interval,
        }
    }

    /// イベントストリームを消費して進捗表示を駆動し、EOF / FinalResponse で close する。
    pub(crate) async fn run(self, mut evt_rx: UnboundedReceiver<AgentEvent>) {
        let Some(sink) = self.sink else {
            // 非対応チャネル / 設定 OFF: backpressure を回避するためストリームを最後まで抜く。
            while evt_rx.recv().await.is_some() {}
            return;
        };

        let mut state = CoordinatorState::new(
            sink,
            self.external_chat_id,
            self.delay,
            self.min_edit_interval,
        );
        let delay_deadline = state.started + state.delay;
        let delay_timer = tokio::time::sleep_until(tokio::time::Instant::from_std(delay_deadline));
        tokio::pin!(delay_timer);

        loop {
            tokio::select! {
                biased;
                evt = evt_rx.recv() => match evt {
                    None => {
                        state.close().await;
                        return;
                    }
                    Some(AgentEvent::ToolStart { name, .. }) => state.on_tool_start(name).await,
                    Some(AgentEvent::ToolResult { name, is_error, duration_ms, .. }) => {
                        state.on_tool_result(name, is_error, duration_ms).await;
                    }
                    Some(AgentEvent::FinalResponse { .. }) => {
                        state.close().await;
                        return;
                    }
                    Some(_) => {}
                },
                _ = &mut delay_timer, if !state.delay_elapsed => {
                    state.delay_elapsed = true;
                    state.begin_if_pending().await;
                }
            }
        }
    }
}

/// コーディネータの実行時状態（未開始 / 表示中）と累積ログを保持する。
struct CoordinatorState {
    sink: Arc<dyn ToolProgressSink>,
    external_chat_id: String,
    log: ProgressLog,
    display: Option<ActiveDisplay>,
    started: Instant,
    delay: Duration,
    min_edit_interval: Duration,
    delay_elapsed: bool,
}

impl CoordinatorState {
    fn new(
        sink: Arc<dyn ToolProgressSink>,
        external_chat_id: String,
        delay: Duration,
        min_edit_interval: Duration,
    ) -> Self {
        Self {
            sink,
            external_chat_id,
            log: ProgressLog::default(),
            display: None,
            started: Instant::now(),
            delay,
            min_edit_interval,
            delay_elapsed: false,
        }
    }

    async fn on_tool_start(&mut self, name: String) {
        self.log.start(name);
        self.refresh_display().await;
    }

    async fn on_tool_result(&mut self, name: String, is_error: bool, duration_ms: u128) {
        self.log.finish(&name, is_error, duration_ms);
        self.refresh_display().await;
    }

    /// 表示中なら間引き update、遅延閾値超過なら初回 begin、それ以外は保留する。
    async fn refresh_display(&mut self) {
        if self.display.is_some() {
            self.update_if_due().await;
        } else if self.started.elapsed() >= self.delay {
            self.delay_elapsed = true;
            self.begin().await;
        }
    }

    /// 遅延タイマー発火時に、保留中ログがあれば初回 begin する。
    async fn begin_if_pending(&mut self) {
        if self.display.is_none() && !self.log.is_empty() {
            self.begin().await;
        }
    }

    async fn begin(&mut self) {
        let body = self.log.render();
        match self.sink.begin(&self.external_chat_id, &body).await {
            Ok(handle) => self.display = Some(ActiveDisplay::new(handle)),
            Err(error) => warn!(error = %error, "tool progress: begin failed"),
        }
    }

    /// 最小編集間隔を超えていれば本文を編集し、超えていなければ遅延（dirty）扱いにする。
    async fn update_if_due(&mut self) {
        let Some(display) = self.display.as_mut() else {
            return;
        };
        let now = Instant::now();
        let due = display
            .last_edit
            .is_none_or(|last| now.duration_since(last) >= self.min_edit_interval);
        if due {
            let body = self.log.render();
            if let Err(error) = display.handle.update(&body).await {
                warn!(error = %error, "tool progress: update failed");
            }
            display.last_edit = Some(now);
            display.dirty = false;
        } else {
            display.dirty = true;
        }
    }

    /// dirty なら最終本文を反映してから close する（進捗メッセージは常に残置）。
    async fn close(&mut self) {
        let Some(display) = self.display.take() else {
            return;
        };
        let mut handle = display.handle;
        if display.dirty {
            let body = self.log.render();
            if let Err(error) = handle.update(&body).await {
                warn!(error = %error, "tool progress: final update failed");
            }
        }
        if let Err(error) = handle.close().await {
            warn!(error = %error, "tool progress: close failed");
        }
    }
}

/// 表示中の進捗メッセージハンドルと編集間引き状態。
struct ActiveDisplay {
    handle: Box<dyn ToolProgressHandle>,
    last_edit: Option<Instant>,
    dirty: bool,
}

impl ActiveDisplay {
    fn new(handle: Box<dyn ToolProgressHandle>) -> Self {
        Self {
            handle,
            last_edit: None,
            dirty: false,
        }
    }
}

/// ツール実行の累積ログ。本文ビルドに使う情報のみを保持する。
#[derive(Default)]
struct ProgressLog {
    entries: Vec<ToolEntry>,
}

impl ProgressLog {
    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 実行中ツールを末尾に追加する。
    fn start(&mut self, name: String) {
        self.entries.push(ToolEntry {
            name,
            status: ToolStatus::Running,
            duration_ms: None,
        });
    }

    /// 同名の最も新しい実行中エントリを完了/エラーに遷移させる。
    /// 対応する開始がない場合は完了状態のエントリを追加する（防御的）。
    fn finish(&mut self, name: &str, is_error: bool, duration_ms: u128) {
        let status = if is_error {
            ToolStatus::Error
        } else {
            ToolStatus::Done
        };
        if let Some(entry) = self
            .entries
            .iter_mut()
            .rev()
            .find(|e| e.name == name && matches!(e.status, ToolStatus::Running))
        {
            entry.status = status;
            entry.duration_ms = Some(duration_ms);
        } else {
            self.entries.push(ToolEntry {
                name: name.to_string(),
                status,
                duration_ms: Some(duration_ms),
            });
        }
    }

    /// 累積ログ本文を構築する。`input` / `preview` は絶対に含めない。
    fn render(&self) -> String {
        let mut lines: Vec<String> = Vec::with_capacity(self.entries.len() + 1);
        lines.push("tools running...".to_string());
        for entry in &self.entries {
            lines.push(match entry.status {
                ToolStatus::Running => format!("... {}", entry.name),
                ToolStatus::Done => {
                    format!("✓ {} ({})", entry.name, format_duration(entry.duration_ms))
                }
                ToolStatus::Error => {
                    format!(
                        "✗ {} ({}) エラー",
                        entry.name,
                        format_duration(entry.duration_ms)
                    )
                }
            });
        }
        lines.join("\n")
    }
}

struct ToolEntry {
    name: String,
    status: ToolStatus,
    duration_ms: Option<u128>,
}

#[derive(Clone, Copy)]
enum ToolStatus {
    Running,
    Done,
    Error,
}

/// ミリ秒を `1.8s` 形式に整形する。不明時は `?` を返す。
fn format_duration(duration_ms: Option<u128>) -> String {
    match duration_ms {
        Some(ms) => format!("{:.1}s", ms as f64 / 1000.0),
        None => "?".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use tokio::sync::Notify;

    /// テスト用の [`ToolProgressSink`] 呼び出し記録。
    #[derive(Default, Clone)]
    struct ProgressCalls {
        begins: Vec<String>,
        updates: Vec<String>,
        closes: usize,
    }

    /// `begin` 完了を待機するための通知付きモック sink。
    struct MockSink {
        calls: Arc<Mutex<ProgressCalls>>,
        begin_notify: Arc<Notify>,
    }

    impl MockSink {
        fn new(calls: Arc<Mutex<ProgressCalls>>, begin_notify: Arc<Notify>) -> Arc<Self> {
            Arc::new(Self {
                calls,
                begin_notify,
            })
        }
    }

    #[async_trait]
    impl ToolProgressSink for MockSink {
        async fn begin(
            &self,
            _external_chat_id: &str,
            body: &str,
        ) -> Result<Box<dyn ToolProgressHandle>, String> {
            self.calls.lock().unwrap().begins.push(body.to_string());
            self.begin_notify.notify_one();
            Ok(Box::new(MockHandle {
                calls: Arc::clone(&self.calls),
            }))
        }
    }

    struct MockHandle {
        calls: Arc<Mutex<ProgressCalls>>,
    }

    #[async_trait]
    impl ToolProgressHandle for MockHandle {
        async fn update(&mut self, body: &str) -> Result<(), String> {
            self.calls.lock().unwrap().updates.push(body.to_string());
            Ok(())
        }

        async fn close(self: Box<Self>) -> Result<(), String> {
            self.calls.lock().unwrap().closes += 1;
            Ok(())
        }
    }

    fn drain_calls(calls: &Arc<Mutex<ProgressCalls>>) -> ProgressCalls {
        let snapshot = calls.lock().unwrap().clone();
        *calls.lock().unwrap() = ProgressCalls::default();
        snapshot
    }

    /// コーディネータを起動し、イベント送信と終了待ちのための部品を返す。
    fn spawn_coordinator(
        sink: Option<Arc<dyn ToolProgressSink>>,
        delay: Duration,
        min_edit_interval: Duration,
    ) -> (
        tokio::sync::mpsc::UnboundedSender<AgentEvent>,
        tokio::task::JoinHandle<()>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
        let coordinator = ToolProgressCoordinator::with_timings(
            sink,
            "discord:1:agent:lyre".to_string(),
            delay,
            min_edit_interval,
        );
        let handle = tokio::spawn(coordinator.run(rx));
        (tx, handle)
    }

    #[tokio::test]
    async fn coordinator_noop_when_sink_none() {
        // Arrange
        let (tx, handle) =
            spawn_coordinator(None, Duration::from_secs(5), Duration::from_millis(800));

        // Act: send events then close the stream
        tx.send(AgentEvent::ToolStart {
            name: "read".to_string(),
            input: serde_json::Value::Null,
        })
        .unwrap();
        tx.send(AgentEvent::FinalResponse {
            text: "done".to_string(),
        })
        .unwrap();
        drop(tx);
        let () = handle.await.unwrap();

        // Assert: no progress posted (sink absent → entire flow is a no-op)
    }

    #[tokio::test]
    async fn coordinator_no_post_below_delay_threshold() {
        // Arrange
        let calls = Arc::new(Mutex::new(ProgressCalls::default()));
        let notify = Arc::new(Notify::new());
        let sink: Arc<dyn ToolProgressSink> =
            MockSink::new(Arc::clone(&calls), Arc::clone(&notify));
        // Fast turn: delay is long so the timer cannot fire before EOF.
        let (tx, handle) = spawn_coordinator(
            Some(sink),
            Duration::from_secs(30),
            Duration::from_millis(800),
        );

        // Act: tool starts and finishes immediately, then the stream ends.
        tx.send(AgentEvent::ToolStart {
            name: "read".to_string(),
            input: serde_json::Value::Null,
        })
        .unwrap();
        tx.send(AgentEvent::ToolResult {
            name: "read".to_string(),
            is_error: false,
            preview: "SECRET".to_string(),
            duration_ms: 10,
        })
        .unwrap();
        drop(tx);
        let () = handle.await.unwrap();

        // Assert: no progress posted for a sub-threshold turn
        let snapshot = calls.lock().unwrap().clone();
        assert!(snapshot.begins.is_empty());
        assert!(snapshot.updates.is_empty());
        assert_eq!(snapshot.closes, 0);
    }

    #[tokio::test]
    async fn coordinator_begins_on_delay_timer_for_long_tool() {
        // Arrange
        let calls = Arc::new(Mutex::new(ProgressCalls::default()));
        let notify = Arc::new(Notify::new());
        let sink: Arc<dyn ToolProgressSink> =
            MockSink::new(Arc::clone(&calls), Arc::clone(&notify));
        // Small delay so the timer fires quickly while the tool is still running.
        let (tx, handle) = spawn_coordinator(
            Some(sink),
            Duration::from_millis(40),
            Duration::from_millis(800),
        );

        // Act: a single long-running tool starts (no result yet) and the delay timer fires.
        tx.send(AgentEvent::ToolStart {
            name: "web_fetch".to_string(),
            input: serde_json::Value::Null,
        })
        .unwrap();
        tokio::time::timeout(Duration::from_secs(2), notify.notified())
            .await
            .expect("begin should fire on delay timer");

        // Assert: begin posted the running tool before any ToolResult arrived
        let snapshot = calls.lock().unwrap().clone();
        assert_eq!(snapshot.begins.len(), 1);
        assert!(snapshot.begins[0].contains("... web_fetch"));

        // Cleanup
        drop(tx);
        let () = handle.await.unwrap();
    }

    #[tokio::test]
    async fn coordinator_builds_cumulative_log() {
        // Arrange
        let calls = Arc::new(Mutex::new(ProgressCalls::default()));
        let notify = Arc::new(Notify::new());
        let sink: Arc<dyn ToolProgressSink> =
            MockSink::new(Arc::clone(&calls), Arc::clone(&notify));
        let (tx, handle) = spawn_coordinator(
            Some(sink),
            Duration::from_millis(20),
            Duration::from_millis(800),
        );

        // Act: tool A starts, delay elapses and begin fires, then B starts and A finishes.
        tx.send(AgentEvent::ToolStart {
            name: "bash".to_string(),
            input: serde_json::Value::Null,
        })
        .unwrap();
        tokio::time::timeout(Duration::from_secs(2), notify.notified())
            .await
            .expect("begin");
        tx.send(AgentEvent::ToolStart {
            name: "read".to_string(),
            input: serde_json::Value::Null,
        })
        .unwrap();
        tx.send(AgentEvent::ToolResult {
            name: "bash".to_string(),
            is_error: false,
            preview: String::new(),
            duration_ms: 1800,
        })
        .unwrap();
        // Allow the (throttled) state to flush on close.
        drop(tx);
        let () = handle.await.unwrap();

        // Assert: the final body reflects completion + duration and a still-running tool.
        let snapshot = calls.lock().unwrap().clone();
        let final_body = snapshot
            .updates
            .last()
            .or(snapshot.begins.last())
            .expect("at least one body");
        assert!(
            final_body.contains("✓ bash (1.8s)"),
            "body was: {final_body}"
        );
        assert!(final_body.contains("... read"), "body was: {final_body}");
        assert!(snapshot.closes >= 1);
    }

    #[tokio::test]
    async fn coordinator_body_excludes_tool_input_and_preview() {
        // Arrange
        let calls = Arc::new(Mutex::new(ProgressCalls::default()));
        let notify = Arc::new(Notify::new());
        let sink: Arc<dyn ToolProgressSink> =
            MockSink::new(Arc::clone(&calls), Arc::clone(&notify));
        let (tx, handle) = spawn_coordinator(
            Some(sink),
            Duration::from_millis(20),
            Duration::from_millis(800),
        );
        let secret_input = "SUPER_SECRET_INPUT_VALUE";
        let secret_preview = "SUPER_SECRET_PREVIEW_VALUE";

        // Act
        tx.send(AgentEvent::ToolStart {
            name: "bash".to_string(),
            input: serde_json::json!({ "command": secret_input }),
        })
        .unwrap();
        tokio::time::timeout(Duration::from_secs(2), notify.notified())
            .await
            .expect("begin");
        tx.send(AgentEvent::ToolResult {
            name: "bash".to_string(),
            is_error: true,
            preview: secret_preview.to_string(),
            duration_ms: 300,
        })
        .unwrap();
        drop(tx);
        let () = handle.await.unwrap();

        // Assert: tool name / status / duration appear, but secrets never do.
        let snapshot = calls.lock().unwrap().clone();
        let bodies: Vec<&str> = snapshot
            .begins
            .iter()
            .chain(snapshot.updates.iter())
            .map(String::as_str)
            .collect();
        assert!(
            bodies.iter().any(|b| b.contains("✗ bash")),
            "missing error line"
        );
        assert!(
            bodies.iter().all(|b| !b.contains(secret_input)),
            "input leaked: {bodies:?}"
        );
        assert!(
            bodies.iter().all(|b| !b.contains(secret_preview)),
            "preview leaked: {bodies:?}"
        );
    }

    #[tokio::test]
    async fn coordinator_closes_on_event_stream_eof() {
        // Arrange
        let calls = Arc::new(Mutex::new(ProgressCalls::default()));
        let notify = Arc::new(Notify::new());
        let sink: Arc<dyn ToolProgressSink> =
            MockSink::new(Arc::clone(&calls), Arc::clone(&notify));
        let (tx, handle) = spawn_coordinator(
            Some(sink),
            Duration::from_millis(20),
            Duration::from_millis(800),
        );

        // Act: drive into the active state, then drop the sender (EOF) without FinalResponse.
        tx.send(AgentEvent::ToolStart {
            name: "bash".to_string(),
            input: serde_json::Value::Null,
        })
        .unwrap();
        tokio::time::timeout(Duration::from_secs(2), notify.notified())
            .await
            .expect("begin");
        drop(tx);
        let () = handle.await.unwrap();

        // Assert: EOF closed the posted progress message
        let snapshot = calls.lock().unwrap().clone();
        assert_eq!(snapshot.begins.len(), 1);
        assert_eq!(snapshot.closes, 1);
    }

    #[tokio::test]
    async fn coordinator_throttles_updates_within_interval() {
        // Arrange
        let calls = Arc::new(Mutex::new(ProgressCalls::default()));
        let notify = Arc::new(Notify::new());
        let sink: Arc<dyn ToolProgressSink> =
            MockSink::new(Arc::clone(&calls), Arc::clone(&notify));
        // Long throttle window so a rapid burst cannot sneak in a second edit.
        let (tx, handle) = spawn_coordinator(
            Some(sink),
            Duration::from_millis(20),
            Duration::from_millis(600),
        );

        // Act: begin, then a rapid burst of state-changing events.
        tx.send(AgentEvent::ToolStart {
            name: "bash".to_string(),
            input: serde_json::Value::Null,
        })
        .unwrap();
        tokio::time::timeout(Duration::from_secs(2), notify.notified())
            .await
            .expect("begin");
        tx.send(AgentEvent::ToolResult {
            name: "bash".to_string(),
            is_error: false,
            preview: String::new(),
            duration_ms: 100,
        })
        .unwrap();
        tx.send(AgentEvent::ToolStart {
            name: "read".to_string(),
            input: serde_json::Value::Null,
        })
        .unwrap();
        tx.send(AgentEvent::ToolResult {
            name: "read".to_string(),
            is_error: false,
            preview: String::new(),
            duration_ms: 50,
        })
        .unwrap();
        // Keep the burst well inside the throttle window before observing.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let mid = drain_calls(&calls);

        // Assert: only one edit reflects the whole burst (the first post-begin update)
        assert_eq!(mid.begins.len(), 1, "begin posted once");
        assert_eq!(mid.updates.len(), 1, "rapid burst coalesced into one edit");

        // Cleanup: close flushes the final dirty state.
        drop(tx);
        let () = handle.await.unwrap();
        let tail = drain_calls(&calls);
        assert_eq!(tail.updates.len(), 1, "close flushes the deferred state");
        assert_eq!(tail.closes, 1);
    }

    #[tokio::test]
    async fn coordinator_keeps_single_progress_across_continuous_stream() {
        // Arrange: simulates a turn whose events flow continuously across what the
        // wiring treats as multiple retry attempts (no FinalResponse until the end).
        // The coordinator must keep a single progress message for the whole stream.
        let calls = Arc::new(Mutex::new(ProgressCalls::default()));
        let notify = Arc::new(Notify::new());
        let sink: Arc<dyn ToolProgressSink> =
            MockSink::new(Arc::clone(&calls), Arc::clone(&notify));
        let (tx, handle) = spawn_coordinator(
            Some(sink),
            Duration::from_millis(20),
            Duration::from_millis(800),
        );

        // Act: attempt-1-like burst, then attempt-2-like burst, then EOF.
        tx.send(AgentEvent::ToolStart {
            name: "bash".to_string(),
            input: serde_json::Value::Null,
        })
        .unwrap();
        tokio::time::timeout(Duration::from_secs(2), notify.notified())
            .await
            .expect("begin");
        // A "retry" happens: more tool events arrive without a FinalResponse.
        tx.send(AgentEvent::ToolStart {
            name: "read".to_string(),
            input: serde_json::Value::Null,
        })
        .unwrap();
        tx.send(AgentEvent::ToolResult {
            name: "read".to_string(),
            is_error: false,
            preview: String::new(),
            duration_ms: 5,
        })
        .unwrap();
        drop(tx);
        let () = handle.await.unwrap();

        // Assert: one progress message (begin) for the entire continuous stream
        let snapshot = calls.lock().unwrap().clone();
        assert_eq!(
            snapshot.begins.len(),
            1,
            "single begin across the whole stream"
        );
        assert_eq!(snapshot.closes, 1, "closed exactly once");
    }
}
