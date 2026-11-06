# Issue 1 具体化：失敗がユーザーに届かない

## 現状の欠陥（4つ、すべて独立）

| # | 場所 | 欠陥 |
|---|---|---|
| 1-a | `src/channels/web/mod.rs:141` | `WebAdapter::send_text` が無条件 `Ok(())`。dispatch の非observer経路は**何もせず成功扱い** |
| 1-b | `web/src/features/chat/useChatTransport.ts:458` | `state === "error"` で query invalidate のみ。エラー内容をUIに渡さない |
| 1-c | `web/src/app/WebUI.tsx:491` | `transport.state.error` を読んでいない。`chatReducer.ts:228` が作る `error` は**デッドコード** |
| 1-d | `src/agent_loop/turn/lifecycle.rs:232` | 失敗は `turn_runs.error_message` にしか残らない。`/api/history` は `messages` しか読まない → **リロードで消える** |

本番証跡：chat 51 の「いまいる？」(18:13:46) は assistant も error も `messages` に存在しない。

## 修正方針：失敗を transcript の durable エントリにする

UI の唯一の情報源は `GET /api/history`（`messages` テーブル）である。したがって失敗は**メッセージとして永続化**し、`turn_runs` は内部状態として残す。両者は同一トランザクションで確定させる。

### Step 1 — 失敗確定と履歴作成を同一トランザクションにする（backend）

`src/storage/mod.rs:376` の `MessageKind` に追加（`TEXT` 列なので migration 不要）:
```rust
enum MessageKind {
    Message => "message",
    AgentSend => "agent_send",
    SystemEvent => "system_event",
    ToolCall => "tool_call",
    TurnFailure => "turn_failure",   // 追加
}
```

`src/storage/turn.rs` の `fail_turn`（`:736`）と `fail_turn_and_terminate_origin`（`:789`）を拡張する。**両方**が対象（呼び出し元は `lifecycle.rs:239`, `:246`, `:283`, `dispatch.rs:450` の4箇所で、いずれも本物の失敗）。同じ理由で `fail_invalid_durable_turn`（`:864`）と `recover_interrupted_turns`（`:1056`）も対象とする。起動時 recovery は「Turn は端末だが履歴が無い」状態を最も多く作る経路であり、ここを外すと設計意図と矛盾する。

トランザクション内でやること:
1. `read_state_locked` で現在状態を取得（既存）
2. `UPDATE turn_runs SET state, error_kind, error_message, finished_at, updated_at`（既存）
3. `turn_runs.chat_id` を同トランザクションで読み、失敗メッセージを1件挿入
4. （origin 版のみ）`turn_origins` の terminal_reason upsert（既存）

```rust
fn record_turn_failure_message_locked(
    tx: &rusqlite::Transaction<'_>,
    turn_id: &str,
    error_message: &str,
) -> Result<(), StorageError> {
    let chat_id: i64 = tx.query_row(
        "SELECT chat_id FROM turn_runs WHERE turn_id = ?1",
        params![turn_id],
        |row| row.get(0),
    )?;
    let mut message = StoredMessage::system(chat_id, error_message.to_string());
    message.id = format!("turn-failure:{turn_id}");
    message.message_kind = MessageKind::TurnFailure;
    message.turn_id = Some(turn_id.to_string());
    commit_message_locked(tx, &message, None, None)?;
    Ok(())
}
```

- `commit_message_locked`（`chat.rs:706`）は `&Transaction` を受けるのでそのまま合成できる。`seq` 採番・`chats.revision` 更新も同関数が行う
- 決定的 ID `turn-failure:{turn_id}` + `INSERT OR IGNORE` により**冪等**。再実行しても増えない
- `session_json: None` を渡すため LLM セッションスナップショットは進めない（失敗文を文脈に混ぜない）
- `TurnFailure { kind, diagnostic, public }` を導入し、`fail_turn` の引数にする。`kind` は `turn_runs.error_kind`、`diagnostic` は `turn_runs.error_message`、`public` は `messages.content`。LLM/Tool 失敗は `TurnFailure::shared` で同一文言、resume / dispatcher 破棄 / payload 解読失敗は専用の公開文言定数を使う
- `uncertain`（出力公開済み・Tool 副作用の可能性あり）は `failed` と公開文言を分ける。再送すると操作が重複し得るため、再実行を促さず結果の確認を促す
- push チャネル（Discord / Telegram）と observer 通知は、保存済み `turn_failure` の本文をそのまま使う。文言を2箇所で組み立てると片方だけが再実行を促し得る
- 対象関数のトランザクションを `TransactionBehavior::Immediate` に変更する（`messages` / `chats` へ書き込むため、Deferred のアップグレードで BUSY を避ける）
- 起動時 recovery の文言は、出力未公開なら `RECOVERED_FAILED_MESSAGE`、公開済みかもしれないなら `RECOVERED_UNCERTAIN_MESSAGE` を使う（内部診断文字列をユーザーに見せない）

**分離トランザクションにしてはならない理由**: `fail_turn` 成功後にメッセージ挿入が失敗すると、Turn は terminal なのに履歴が無い状態が恒久化する。以後の `fail()` は terminal 判定で早期 return し、起動時 recovery も terminal Turn を対象にしないため、二度と補完されない。UI は履歴しか読まないので、主目的が満たされないまま残る。

### Step 2 — LLM 文脈から除外（必須）

LLM に渡す経路は2つあり、両方から除外する。

1. `Database::load_session_snapshot`（`chat.rs:1006`）のクエリに `AND message_kind <> 'turn_failure'`
2. `Database::get_recent_context_messages` を新設し、Pulse の `load_recent_messages`（`pulse/scheduler.rs:433`）を切り替える。`get_recent_messages` は `message_kind` を制限しないため、直前に失敗した Turn のエラー文が次の Pulse Activation の文脈に入る

これを入れないと、次ターンのプロンプトに自分のエラー文が混入する（`session.rs:536 loaded_from_recent` は `message_kind` を見ていない）。

### Step 3 — Web UI で表示（frontend）

- `chatReducer.ts` / `timelineEntries.ts` で `message_kind === "turn_failure"` をエラー項目として描画
- `chatReducer.ts` の未使用 `error` フィールドと `errorMessage` を削除（デッドコード）
- `useChatTransport.ts:458` は現状維持 — 既に `invalidateQueries("history")` を呼ぶので、新エントリが降ってくる
- backend の `GET /api/history` は `message_kind` を既に返している（`sessions.rs:325`）ので**変更不要**

### Step 4 — 文言の不整合を解消

`dispatch.rs:1024` は `error.user_message()`（`Error: llm_api_error: status=429 body_preview=...` という内部文字列）を WS に流している。DB 側（`sanitize_error_message`）は `user_facing_summary()`。→ `user_facing_summary()` に統一。

### Step 5 — `WebAdapter::send_text` の嘘を消す

`dispatch.rs:1057` の `send_turn_failure_to_channel` を web では呼ばない（`turn.context.channel.as_str() != "web"`）。web は Step 1 の永続化 + 既存 `error` イベントで配信が完結する。Discord/Telegram は従来どおり push。

## テスト

| 層 | テスト |
|---|---|
| storage | `fail_turn` が `turn_runs` を terminal にし、**同一トランザクションで** `turn_failure` メッセージを1件作る |
| storage | `fail_turn_and_terminate_origin` も同じくメッセージを作る |
| storage | 同じ turn に対して `fail_turn` を2回呼んでも `turn_failure` は1件のまま（冪等） |
| storage | メッセージの `chat_id` / `turn_id` / `content` / `message_kind` が正しく、`seq` が採番される |
| storage | `load_session_snapshot` が `turn_failure` を**除外**する |
| web api | `GET /api/history` に `turn_failure` が含まれる |
| frontend | `timelineEntries` が `turn_failure` をエラー項目にする |
| dispatch | web では `send_turn_failure_to_channel` を呼ばない／discord では呼ぶ |

---

# Issue 2 具体化：リトライ予算の設計

## 現状の欠陥

| # | 場所 | 欠陥 |
|---|---|---|
| 2-a | `src/llm/openai.rs:10,24` | `timeout(300s)` のみ。**接続後にバイトが来ない状態を300秒待つ** |
| 2-b | `src/agent_loop/model_step.rs:183` | タイムアウトが `MAX_LLM_RETRIES=3` の1回を消費 → 最大 **300×3 = 900秒** |
| 2-c | `src/agent_loop/model_step.rs:472` | バックオフに jitter なし。vega/lyre が同時に叩くと同時リトライ |

## 修正方針

「待ち時間の上限」は2段で作る。**attempt の開始を制限するだけでは上限にならない**ため、attempt 自体を残時間で打ち切る。

### Step 1 — `read_timeout` を追加（`src/llm/openai.rs`）

```rust
/// Total request deadline: bounds one generation end to end.
const REQUEST_TIMEOUT_SECS: u64 = 300;

/// Inactivity deadline between reads. A provider that accepts the connection
/// but never produces a byte (saturated prefill) fails here instead of waiting
/// out the total deadline.
const READ_TIMEOUT_SECS: u64 = 90;
```
```rust
.timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
.read_timeout(Duration::from_secs(READ_TIMEOUT_SECS))
```

reqwest 0.13.4 の `read_timeout` は公式docのとおり **「各 read でリセット」** なので、長い生成は通し、ストールだけ落とせる。実測の prefill は 7〜30秒（`llm_usage_logs` とログのタイムスタンプ差）なので 90秒は 3倍マージン。

ただし `read_timeout` は**バイトが細切れに届き続ける限り発火しない**。これ単体では上限にならないので Step 2 が必要。

### Step 2 — iteration deadline で attempt 自体を打ち切る（`src/agent_loop/model_step.rs`）

```rust
/// Wall-clock ceiling for one model iteration across every transport attempt.
/// Each attempt is capped by whatever remains, so retries can never extend a
/// Turn past this bound.
const MODEL_ITERATION_DEADLINE: Duration = Duration::from_secs(300);
```

`retry_transient_failures`（`:146`）:
```rust
let started = Instant::now();
let deadline = started + MODEL_ITERATION_DEADLINE;
for attempt in 1..=MAX_LLM_RETRIES {
    let remaining = deadline.saturating_duration_since(Instant::now());
    let response = match tokio::time::timeout(remaining, self.run_once(...)).await {
        Ok(result) => result,
        Err(_elapsed) => {
            warn!(attempt, elapsed_ms = ..., "llm iteration deadline exhausted");
            Err(ModelStepError::new(
                EgoPulseError::Llm(LlmError::DeadlineExceeded),
                Arc::clone(&retry_messages),
            ))
        }
    };
    // ... 既存の retry 判定
}
```

- `Instant` は **`tokio::time::Instant`** を使う。`tokio::time::timeout` は tokio の clock で動くため、`std::time::Instant` と混ぜるとテストの時間制御（`tokio::time::pause`）で deadline が効かなくなる
- 1回目の attempt は deadline の全量（300秒）を使える → 「正常な長い生成に300秒を許容する」既存方針を維持
- リトライは残時間しか使えない → **総経過時間は 300秒を超えない**
- リトライ可否の判定も `deadline > Instant::now() + backoff` に変更（deadline を跨ぐ sleep をしない）
- 試行回数の記録（`before_retry`）とバックオフ待機も `tokio::time::timeout_at(deadline, ...)` の内側に入れる。期限の外に置くと DB 待ちが長い場合に window を超える。期限到達時は実際のエラーを返す
- `LlmError::DeadlineExceeded` を追加（`src/error.rs`）
  - `is_retryable() == false`。deadline を食い切った時点で残り時間は 0 であり、次の attempt は走れない
  - `user_facing_summary()` = `"LLM API request timed out. Please try again later."`
  - `Display` は lower-case: `llm_deadline_exceeded: no response within the model iteration deadline`

### Step 3 — jitter（`llm_retry_backoff`, `model_step.rs:464`）

- `Retry-After` がある場合は**そのまま**（サーバー指示を尊重、jitter しない）
- それ以外は full jitter: `rand::random_range(0..=computed)`（`rand 0.10` は既存依存）

### Step 4 — 観測性

- deadline 枯渇時に `llm iteration deadline exhausted` の warn（`attempt`, `elapsed_ms`）
- リトライログに `elapsed_ms` を追加

### 待ち時間の上限

| ケース | 現在 | 修正後 |
|---|---|---|
| prefill 停止（無応答） | 300s × 3 = **900s** | 90s × 3 = 270s、deadline で **300s 上限** |
| 細切れ応答でストール | 300s × 3 = 900s | deadline で **300s 上限** |
| 429 (`Retry-After: 5`) | ~8s | ~8s（変化なし） |
| 正常な長い生成 | 最大300s | 最大300s（変化なし） |

## テスト

| 対象 | テスト |
|---|---|
| `llm_retry_backoff` | jitter が `[0, computed]` に収まる／`Retry-After` は jitter しない |
| `should_retry` | deadline 超過で `false`／published で `false`／非 retryable で `false` |
| retry loop | `tokio::time::pause()` + 応答しない fake provider で、attempt が deadline で打ち切られ、次 attempt を開始しない |
| retry loop | backoff が deadline を跨ぐ場合はリトライしない |
| retry loop | 前処理と待機が deadline で打ち切られる（遅い `before_retry` を注入） |
| storage | `TurnFailure` の `diagnostic` が `turn_runs` に、`public` が `messages` に入る |
| dispatch | `uncertain` の会話エントリが再実行を促さない（lifecycle 経由・dispatcher 破棄の両方）／push 通知が保存済み本文と一致する |
| storage | `get_recent_context_messages` が `turn_failure` を除外する |
| `shared_http_client` 相当 | 接続だけして応答しない TcpListener に対し、`read_timeout` で `is_timeout()` になる |
| 回帰 | 既存の `retryable_llm_error_is_retried_within_same_iteration` が通る |

---

## 補足（スコープ外だが関連）

Issue 2 は「待ち時間の上限」を入れるだけで、429 の**発生確率**は下げません。発生源は 200k+ トークンの prefill（chat 51: 254,674 / chat 53: 209,261、compaction 発火0件）。ここを詰めるのが Issue 3 ですが、今回は 1・2 のみに留めます。
