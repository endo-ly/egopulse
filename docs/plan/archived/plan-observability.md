# Plan: Observability 3層モデル + gateway status 再設計

エラー発生時の監査を容易にするため、構造化ログ（tracing span）/ Live Health API（/health, /ready）/ Prometheus（/metrics）の3層を導入する。あわせて `status.json` を廃止し、`egopulse gateway status` にランタイム live 状態を統合する。

> **Note**: 以下の具体的なコード例・API 設計・構成（How）はあくまで参考である。実装時によりよい設計方針があれば積極的に採用すること。

## 設計方針

- **`RuntimeStatus` を AppState に注入** — ランタイムの live health summary を保持する。詳細状態の source of truth は各 subsystem に残す（active turns → `ActiveTurnTracker`、MCP 状態 → `McpManager`、systemd 状態 → OS、永続履歴 → DB/log）。`/ready` と `gateway status` では `RuntimeStatus` と各 subsystem snapshot を合成する。`RuntimeStatus` 自体は神オブジェクトにせず、軽量な summary に留める
- **`tracing` span で turn ライフサイクルを囲む** — `trace_id`, `agent_id`, `channel`, `session` を span フィールドに付与し、全ログイベントに自動伝播。エラー発生時に trace_id で grep → 全イベント時系列追跡。trace_id は turn execution の入口で一度だけ生成し、実行コンテキストを通じて `process_turn_inner`、LLM call、tool execution、channel send、`RuntimeStatus.push_error` まで同じ値を伝播する。下位レイヤーでは新しい trace_id を生成しない
- **Health/Readiness/Metrics エンドポイント** — `/health` (liveness), `/ready` (readiness), `/metrics` (Prometheus)。いずれも認証不要だが、ローカル運用または trusted network 前提とする。外部公開する場合は reverse proxy 等でアクセス制限する。レスポンスや metrics label には secret、user input、raw error detail を含めない
- **`metrics` クレートでカウンター/ゲージを計装** — turn 数・エラー数・LLM token・tool 呼び出し数を `/metrics` に公開
- **`status.json` 廃止** — `egopulse status` を削除し、`egopulse gateway status` に systemd 状態 + ランタイム live 状態を統合する。これは破壊的変更として扱い、`docs/commands.md` と `docs/deploy.md` で移行先を明記する

### `/ready.ok` の判定方針

readiness は「このプロセスが仕事を受けて処理できる状態か」を表す。判定方針:

- DB が不健康なら `ok: false`
- configured channel が1つも Running でないなら `ok: false`
- 一部チャネルだけ Failed の場合、全体 `ok` を即 `false` にするのではなく、`channels` に個別状態として表示する（`ok: true` だが `channels.telegram.state: "failed"` のように詳細に残す）
- MCP の一部失敗は詳細表示に留め、全体 `ok` には含めない（MCP がなくても turn 自体は動作するため）
- `/ready` は単なる bool ではなく「全体判定 + 個別状態」を返すものとして設計する

### Metrics label cardinality policy

Prometheus metrics の label は低カーディナリティな固定的値に限定する。

- **許容**: `agent`, `channel`, `tool`, `provider`, `error_kind`, `request_kind`, `direction` (input/output)
- **禁止**: `trace_id`, `session`, `origin_id`, raw error message, URL, user input, tool arguments, message id

特に `trace_id` は logs で使うものであり、metrics label に入れてはならない。

### recent_errors の位置付け

`RuntimeStatus.recent_errors` はリングバッファであり、再起動で消える。容量超過でも古いエントリが破棄される。

- 監査の主役は structured log / tracing log
- `recent_errors` は `/ready` と `gateway status` に表示するための直近エラー要約
- 永続性や完全性は期待しない

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 種別 |
|---|---|
| `RuntimeStatus` 構造体（AppState に注入） | 新規 |
| `tracing` span による turn トレーサビリティ | 変更 |
| `/health`, `/ready`, `/metrics` API | 変更 |
| `status.json` 読み書き + `egopulse status` コマンド | 削除 |
| `egopulse gateway status` の再設計 | 変更 |
| `metrics` クレートによる Prometheus 計装 | 新規 |
| JSON ログフォーマット対応 | 変更 |
| ドキュメント更新 | 変更 |

---

## Step 0: Worktree 作成

`worktree-create` skill で `feat/observability` ブランチの WT を作成。

---

## Step 1: RuntimeStatus (TDD)

AppState に注入する in-memory ライブヘルス summary。チャネル状態・DB 健全性・直近エラー要約を保持する。詳細状態の source of truth は各 subsystem に残す。

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `new_sets_initial_state` | `started_at`, `pid`, `version`, `db_healthy=true`, `channels` 空, `recent_errors` 空 |
| `update_channel_sets_state` | `update_channel("web", Running)` → `channel_health("web")` が Running を返す |
| `update_channel_error_sets_failed` | `update_channel_error("discord", "timeout")` → state=Failed, last_error=Some |
| `touch_channel_activity_updates_timestamp` | `touch_channel_activity("web")` → `last_activity` が設定される |
| `set_db_healthy_toggles` | `set_db_healthy(false)` → `snapshot().db_healthy == false` |
| `push_error_appends_to_ring_buffer` | push 後に `recent_errors()` に含まれる |
| `push_error_respects_capacity` | capacity=5 で 7 件 push → 古い 2 件が破棄、最新 5 件のみ |
| `push_error_records_all_fields` | trace_id, error_kind, agent_id, channel, summary が正しく記録される |
| `snapshot_returns_independent_copy` | snapshot 取得後に update しても snapshot 側に影響しない |
| `channel_health_returns_none_for_unknown` | 未登録チャネル名 → None |

### GREEN: 実装

新規ファイル `src/runtime/runtime_status.rs`。

```rust
pub(crate) struct RuntimeStatus {
    inner: std::sync::RwLock<RuntimeStatusInner>,
}

struct RuntimeStatusInner {
    started_at: chrono::DateTime<Utc>,
    pid: u32,
    version: String,
    db_healthy: bool,
    channels: HashMap<String, ChannelHealth>,
    recent_errors: VecDeque<AuditError>,
    error_capacity: usize,  // default 100
}

pub(crate) enum ChannelState { Starting, Running, Failed, Stopped }

pub(crate) struct ChannelHealth {
    pub state: ChannelState,
    pub last_error: Option<String>,
    pub last_activity: Option<chrono::DateTime<Utc>>,
}

pub(crate) struct AuditError {
    pub at: chrono::DateTime<Utc>,
    pub trace_id: String,
    pub error_kind: String,
    pub agent_id: String,
    pub channel: String,
    pub summary: String,
}
```

主要メソッド:
- `RuntimeStatus::new() -> Self`
- `update_channel(name, state)`
- `update_channel_error(name, error_msg)` — state を Failed に設定
- `touch_channel_activity(name)` — last_activity を現在時刻に更新
- `set_db_healthy(bool)`
- `push_error(trace_id, error_kind, agent_id, channel, summary)`
- `snapshot() -> StatusSnapshot`（全フィールドのコピーを返す）
- `recent_errors() -> Vec<AuditError>`
- `channel_health(name) -> Option<ChannelHealth>`

`StatusSnapshot` は `serde::Serialize` を実装し `/ready` のレスポンスに直接使う。

### コミット

`feat: add RuntimeStatus with live channel health and error ring buffer`

---

## Step 2: AppState 統合 + チャネルライフサイクル通知 (TDD)

### 前提: Step 1

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `build_app_state_includes_runtime_status` | `build_app_state` 後に `runtime_status` が存在し started_at が設定されている |
| `cloned_app_state_shares_runtime_status` | clone 後も同じ `Arc<RuntimeStatus>` を共有（`Arc::ptr_eq`） |
| `build_sleep_app_state_includes_runtime_status` | sleep 用 AppState にも runtime_status が含まれる |

### GREEN: 実装

- `AppState` / `AppStateParts` に `pub(crate) runtime_status: Arc<RuntimeStatus>` を追加
- `from_parts` で注入
- `start_channels` でチャネル起動後に `runtime_status.update_channel("web", Running)` 等を呼ぶ
  - Web: `run_server` の bind 成功後
  - Discord: 各 bot の shard ready 後
  - Telegram: 各 bot の起動成功後
- `execute_scheduled_turn` 内で:
  - turn 開始時に `runtime_status.touch_channel_activity(channel)` を呼ぶ
  - エラー発生時に `runtime_status.push_error(...)` を呼ぶ
- `process_turn_inner` 内のエラー分岐で `push_error` を呼ぶ
- `write_startup_status` の `StatusSnapshot` 構築部分は残す（まだ status.json 書き出しを残す。Step 5 で廃止）

### コミット

`feat: integrate RuntimeStatus into AppState and channel lifecycle`

---

## Step 3: tracing span による turn トレーサビリティ (TDD)

### 前提: Step 2

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `process_turn_emits_span_with_context_fields` | `tracing-subscriber` の `MockLayer` で span 作成をキャプチャし、`agent_id`, `channel`, `session`, `trace_id` フィールドを検証 |
| `span_propagates_to_child_events` | span 内の warn! イベントが親 span のフィールドを継承していること |

※ テスト手法: `tracing-subscriber` の `layer::Layer` trait を実装した `MockCollector` で `on_event` / `new_span` をフックしてフィールドを検証。

### GREEN: 実装

- trace_id は turn execution の入口である `execute_scheduled_turn` で一度だけ生成する
- `SurfaceContext` に `trace_id: String` フィールドを追加し、生成した trace_id を設定して下流に伝播
- `process_turn_inner` は `SurfaceContext` から trace_id を読み取って span に設定する。新たな trace_id は生成しない
- これにより `execute_scheduled_turn` → `process_turn_inner` → LLM call → tool execution → channel send → `push_error` まで同一の trace_id で追跡可能
- span の設定:

```rust
// process_turn_inner の先頭
let span = tracing::info_span!(
    "agent_turn",
    trace_id = %context.trace_id,
    agent_id = %context.agent_id,
    channel = %context.channel,
    session = %context.surface_thread,
    origin_id = %context.origin_id,
    chain_depth = context.chain_depth,
);
let _enter = span.enter();
```

- 既存の `warn!` 呼び出しは span 内で実行されるため、自動的にフィールドが伝播する
- `error_kind` フィールドを主要な warn/error 呼び出しに追加

### コミット

`feat: wrap agent turn lifecycle in tracing span for correlation`

---

## Step 4: /health /ready /metrics API (TDD)

### 前提: Step 2

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `health_returns_ok_version_uptime` | `GET /health` → `{ ok, version, uptime_secs }` のみ（認証不要） |
| `ready_returns_runtime_status` | `GET /ready` → RuntimeStatus スナップショット + MCP 状態 + active_turns が含まれる |
| `ready_ok_when_all_healthy` | db healthy, 全チャネル Running → `ok: true` |
| `ready_not_ok_when_db_unhealthy` | `set_db_healthy(false)` → `ok: false` |
| `ready_not_ok_when_all_channels_failed` | 全チャネル Failed → `ok: false` |
| `ready_includes_mcp_status` | MCP manager がある場合、servers 一覧が含まれる |
| `metrics_returns_prometheus_text` | `GET /metrics` → `# HELP` / `# TYPE` ヘッダー付き |
| `metrics_contains_egopulse_prefix` | 全メトリクスが `egopulse_` プレフィックスを持つ |

### GREEN: 実装

#### 4a. `/health` — liveness probe（変更）

`src/channels/web/health.rs` を変更。軽量のまま `uptime_secs` を追加。

```json
{ "ok": true, "version": "0.1.0", "uptime_secs": 86400 }
```

#### 4b. `/ready` — readiness probe（新規）

`RuntimeStatus::snapshot()` + MCP ステータス + `ActiveTurnTracker` から組み立て。

```jsonc
{
  // 判定方針に基づく全体 ok:
  //   ok: db_healthy && configured channel が 1 つ以上 Running
  //   一部チャネル Failed は ok に影響しない（channels に個別表示）
  //   MCP 失敗は ok に影響しない
  "ok": true,
  "version": "0.1.0",
  "uptime_secs": 86400,
  "pid": 12345,
  "db": { "ok": true },
  "channels": {
    "web": { "state": "running", "last_activity": "..." },
    "discord": { "state": "failed", "last_error": "bot token rejected" }
  },
  "mcp": {
    "healthy": 1, "failed": 1,
    "servers": [
      { "name": "context7", "connected": true },
      { "name": "github", "connected": false }
    ]
  },
  "active_turns": 2,
  "recent_errors_count": 3,
  "last_error_at": "..."
}
```

#### 4c. `/metrics` — Prometheus（新規）

`metrics` + `metrics-exporter-prometheus` クレートを使用。
`src/runtime/metrics.rs` を新規作成:

```rust
pub fn init_metrics() -> PrometheusHandle { ... }

// ヘルパーマクロ
pub fn inc_turns_total(agent: &str, channel: &str) { ... }
pub fn inc_turn_errors_total(kind: &str, agent: &str) { ... }
pub fn inc_llm_tokens_total(direction: &str, provider: &str, amount: i64) { ... }
pub fn set_active_turns_gauge(agent: &str, count: u32) { ... }
pub fn inc_tool_calls_total(tool: &str, status: &str) { ... }
```

計装箇所:
- `process_turn_inner`: `inc_turns_total`, `inc_turn_errors_total`
- `execute_tool_call`: `inc_tool_calls_total`
- LLM usage logging: `inc_llm_tokens_total`
- `ActiveTurnTracker::begin_turn` / `end_turn`: `set_active_turns_gauge`
- `RuntimeStatus` の `uptime_secs`: gauge

#### 4d. ルーティング

`src/channels/web/mod.rs`:

```rust
.route("/health", get(health::health))       // 既存、認証なし
.route("/ready", get(health::readiness))      // 新規、認証なし
.route("/metrics", get(health::metrics))      // 新規、認証なし
```

`/api/health` は `/health` にリネーム（既存の `/api/health` ルートを `/health` に変更）。後方互換のため `/api/health` も残すかは実装時に判断。

### コミット

`feat: add /ready and /metrics endpoints with Prometheus instrumentation`

---

## Step 5: status.json 廃止 + egopulse status 削除 + gateway status 再設計 (TDD)

### 前提: Step 2, Step 4

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `gateway_status_shows_systemd_and_runtime_when_running` | サービス稼働時: systemd status + runtime live 状態が表示される |
| `gateway_status_shows_systemd_only_when_not_running` | サービス非稼働時: systemctl status のみ表示 |
| `gateway_status_json_flag` | `--json` で JSON 出力 |
| `status_command_removed_from_clap` | `egopulse status` がパースエラーになること |
| `status_json_not_in_help` | `egopulse --help` に status が表示されないこと |

### GREEN: 実装

#### 5a. `egopulse status` を削除

- `main.rs` の `Command::Status` を削除
- `run()` 内の `Command::Status` 分岐を削除
- `src/runtime/status.rs` の `run_status` 関数を削除

#### 5b. `status.json` 書き出しを廃止

- `write_startup_status()` を削除
- `start_channels()` 内の `write_startup_status(&state).await` 呼び出しを削除
- `status.rs` 内の `write_status`, `read_status`, `format_snapshot` および関連型（`StatusSnapshot`, `ChannelsStatus`, `ProviderStatus` 等）を削除
  - ※ `McpStatus`, `ConnectedMcpServer`, `FailedMcpServer` は `McpManager::status_snapshot()` の戻り型として使われているため残すか、`RuntimeStatus` 側に等価な型を定義して移行

#### 5c. `egopulse gateway status` を再設計

- `GatewayAction::Status` に `--json` フラグを追加
- 処理フロー:
  1. `systemctl --user is-active egopulse.service` を実行して systemd 状態を取得
  2. active の場合、config から web port を解決し `http://127.0.0.1:{port}/ready` に GET
  3. レスポンスを人間可読テキスト（または JSON）にフォーマットして表示
  4. inactive/failed の場合、`systemctl --user status egopulse.service` の出力のみ表示

フォーマット例（テキスト）:
```
Service: active (systemd)

EgoPulse v0.1.0  PID 293918  uptime 24h 3m
Provider: openrouter / gpt-5

Channels
  web      ● running
  discord  ● running
  telegram ✗ failed   "bot token rejected"

MCP Servers (1/2 connected)
  ✓ context7  stdio   2 tools
  ✗ github    "connection timed out after 30s"

Recent Errors (last 1h: 3)
  10:05:00Z  llm       alice  discord  "status=429 rate limit"
  10:03:00Z  channel   -      telegram "bot token rejected"
  10:01:00Z  mcp       -      -        "github: connection timed out"
```

### コミット

`feat: replace status.json with live gateway status and remove egopulse status`

---

## Step 6: JSON ログフォーマット対応 (TDD)

### 前提: Step 3

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `init_logging_default_is_text` | デフォルトで `fmt()` テキスト形式が初期化される |
| `init_logging_json_format` | `EGOPULSE_LOG_FORMAT=json` で `fmt().json()` が初期化される |
| `init_logging_invalid_format_falls_back` | 不正な値はテキスト形式にフォールバック |

### GREEN: 実装

`src/runtime/logging.rs` を変更:

- `init_logging` 内で `std::env::var("EGOPULSE_LOG_FORMAT")` をチェック
- `"json"` の場合 `tracing_subscriber::fmt().json().with_env_filter(...)` を使用
- それ以外は現状の `fmt()` のまま

### コミット

`feat: support EGOPULSE_LOG_FORMAT=json for structured logging`

---

## Step 7: 監査ログ強化 (TDD)

### 前提: Step 2, Step 3, Step 4

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `turn_failure_pushes_error` | `process_turn` が Err を返した時に `RuntimeStatus.recent_errors` に記録される |
| `stop_condition_pushes_error` | stop condition evaluator が発火した時に push_error される |
| `channel_send_failure_pushes_error` | `adapter.send_text` 失敗時に push_error される |
| `compaction_request_kind_is_compaction` | compaction の `log_llm_usage` に `request_kind="compaction"` が渡される |
| `sleep_batch_request_kind` | sleep batch の LLM 呼び出しに `log_llm_usage` が追加される（`request_kind="sleep_batch"`） |
| `pulse_request_kind` | pulse の LLM 呼び出しに `log_llm_usage` が追加される（`request_kind="pulse"`） |

### GREEN: 実装

- `execute_scheduled_turn`:
  - `Err` ブランチ → `runtime_status.push_error(trace_id, error_kind, agent_id, channel, summary)`
  - stop condition 発火 → `push_error`
  - `send_text` 失敗 → `push_error`
- `process_turn_inner`:
  - LLM send_message 失敗 → `push_error`
  - tool loop 超過 → `push_error`
- `agent_loop/compaction.rs` の `log_llm_usage` 呼び出しの `request_kind` を `"summarize"` → `"compaction"` に修正
- `sleep/batch.rs` の LLM 呼び出し後に `log_llm_usage` 追加（`request_kind="sleep_batch"`）
- `pulse/runner.rs` の LLM 呼び出し後に `log_llm_usage` 追加（`request_kind="pulse"`）

### コミット

`feat: enrich audit logging with error tracking and request_kind differentiation`

---

## Step 8: ドキュメント更新

### 前提: 全 Step 完了後

更新対象:

| ファイル | 変更内容 |
|---|---|
| `docs/commands.md` | `egopulse status` 削除、`gateway status` の再設計を反映 |
| `docs/channels.md` | `/health`, `/ready`, `/metrics` の仕様追加 |
| `docs/api.md` | `/api/health` → `/health`, `/ready`, `/metrics` の仕様変更を反映 |
| `docs/architecture.md` | Observability Layer の記述追加、`status.json` の記述削除 |
| `docs/directory.md` | `status.json` の記述削除 |
| `docs/deploy.md` | `journalctl` の trace_id grep 例、`/ready` を使ったヘルスチェック例を追加 |

### コミット

`docs: update documentation for observability layer and gateway status redesign`

---

## Step 9: 動作確認

```bash
# 全テスト
cargo test

# Lint / フォーマット
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo check

# 手動確認（起動後）
curl http://127.0.0.1:10961/health
curl http://127.0.0.1:10961/ready
curl http://127.0.0.1:10961/metrics

# JSON ログ確認
EGOPULSE_LOG_FORMAT=json cargo run -- run 2>&1 | head -5 | jq .

# CLI 確認
cargo run -- gateway status
cargo run -- gateway status --json
```

---

## Step 10: PR 作成

PR description は日本語。

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
|---|---|---|
| `Cargo.toml` | 変更 | `metrics`, `metrics-exporter-prometheus` 追加 |
| `src/runtime/runtime_status.rs` | **新規** | `RuntimeStatus`, `ChannelHealth`, `AuditError`, `StatusSnapshot` |
| `src/runtime/metrics.rs` | **新規** | `init_metrics`, counter/gauge ヘルパー |
| `src/runtime/mod.rs` | 変更 | AppState に `runtime_status` 追加、`write_startup_status` 削除 |
| `src/runtime/status.rs` | 変更 | `run_status` / `write_status` / `read_status` / `format_snapshot` / `StatusSnapshot` 削除。`McpStatus` 等は残す |
| `src/runtime/logging.rs` | 変更 | JSON フォーマット対応 |
| `src/runtime/gateway.rs` | 変更 | `GatewayAction::Status` に `--json` 追加、live 情報取得ロジック追加 |
| `src/main.rs` | 変更 | `Command::Status` 削除、`Gateway::Status` の変更に追従 |
| `src/agent_loop/turn.rs` | 変更 | span 追加、metrics counter 追加、push_error 追加 |
| `src/agent_loop/compaction.rs` | 変更 | `request_kind` を `"compaction"` に修正 |
| `src/channels/web/health.rs` | 変更 | `/health` 軽量化 + `readiness()` + `metrics_handler()` 追加 |
| `src/channels/web/mod.rs` | 変更 | `/ready`, `/metrics` ルート追加 |
| `src/sleep/batch.rs` | 変更 | `log_llm_usage` 追加 |
| `src/pulse/runner.rs` | 変更 | `log_llm_usage` 追加 |
| `src/lib.rs` | 変更 | モジュール公開の追加・変更 |
| `docs/commands.md` | 変更 | コマンド体系の更新 |
| `docs/channels.md` | 変更 | Health/Ready/Metrics 仕様 |
| `docs/api.md` | 変更 | API 仕様の更新 |
| `docs/architecture.md` | 変更 | Observability Layer 追加 |
| `docs/directory.md` | 変更 | `status.json` 削除 |
| `docs/deploy.md` | 変更 | 監査ログ活用例 |

---

## コミット分割

1. `feat: add RuntimeStatus with live channel health and error ring buffer`
2. `feat: integrate RuntimeStatus into AppState and channel lifecycle`
3. `feat: wrap agent turn lifecycle in tracing span for correlation`
4. `feat: add /ready and /metrics endpoints with Prometheus instrumentation`
5. `feat: replace status.json with live gateway status and remove egopulse status`
6. `feat: support EGOPULSE_LOG_FORMAT=json for structured logging`
7. `feat: enrich audit logging with error tracking and request_kind differentiation`
8. `docs: update documentation for observability layer and gateway status redesign`

---

## テストケース一覧（全 39 件）

### RuntimeStatus (10)
1. `new_sets_initial_state` — 初期値検証
2. `update_channel_sets_state` — チャネル状態設定
3. `update_channel_error_sets_failed` — エラー付き Failed 設定
4. `touch_channel_activity_updates_timestamp` — activity タイムスタンプ更新
5. `set_db_healthy_toggles` — DB 健全性切替
6. `push_error_appends_to_ring_buffer` — エラー追加
7. `push_error_respects_capacity` — ring buffer 容量制限
8. `push_error_records_all_fields` — 全フィールド記録検証
9. `snapshot_returns_independent_copy` — snapshot の独立性
10. `channel_health_returns_none_for_unknown` — 未登録チャネル

### AppState 統合 (3)
11. `build_app_state_includes_runtime_status` — AppState に含まれる
12. `cloned_app_state_shares_runtime_status` — Arc 共有
13. `build_sleep_app_state_includes_runtime_status` — sleep 用にも含まれる

### tracing span (2)
14. `process_turn_emits_span_with_context_fields` — span フィールド伝播
15. `span_propagates_to_child_events` — 子イベントへの継承

### Health/Ready/Metrics API (8)
16. `health_returns_ok_version_uptime` — liveness レスポンス
17. `ready_returns_runtime_status` — readiness に RuntimeStatus 反映
18. `ready_ok_when_all_healthy` — 健全時 ok: true
19. `ready_not_ok_when_db_unhealthy` — DB 不健全時 ok: false
20. `ready_not_ok_when_all_channels_failed` — 全チャネル Failed 時 ok: false
21. `ready_includes_mcp_status` — MCP 情報含まれる
22. `metrics_returns_prometheus_text` — Prometheus テキスト形式
23. `metrics_contains_egopulse_prefix` — メトリクスプレフィックス

### gateway status 再設計 (5)
24. `gateway_status_shows_systemd_and_runtime_when_running` — 稼働時の統合表示
25. `gateway_status_shows_systemd_only_when_not_running` — 非稼働時
26. `gateway_status_json_flag` — JSON 出力
27. `status_command_removed_from_clap` — status 削除確認
28. `status_not_in_help` — help に表示されない

### JSON ログ (3)
29. `init_logging_default_is_text` — デフォルトテキスト
30. `init_logging_json_format` — JSON 形式
31. `init_logging_invalid_format_falls_back` — フォールバック

### 監査ログ強化 (6)
32. `turn_failure_pushes_error` — turn 失敗時
33. `stop_condition_pushes_error` — stop condition 発火時
34. `channel_send_failure_pushes_error` — チャネル送信失敗時
35. `compaction_request_kind_is_compaction` — compaction request_kind
36. `sleep_batch_request_kind` — sleep batch request_kind
37. `pulse_request_kind` — pulse request_kind

### 既存テスト追従 (2)
38. `status_snapshot_tests_removed_or_migrated` — 旧 status.rs テストの移行
39. `gateway_status_existing_tests_pass` — gateway.rs 既存テスト通過

---

## 工数見積もり

| Step | 内容 | 見積もり |
|---|---|---|
| Step 0 | Worktree 作成 | ~10 行 |
| Step 1 | RuntimeStatus | ~400 行 |
| Step 2 | AppState 統合 | ~180 行 |
| Step 3 | tracing span | ~120 行 |
| Step 4 | /ready /metrics API | ~350 行 |
| Step 5 | status 廃止 + gateway status 再設計 | ~250 行 |
| Step 6 | JSON ログフォーマット | ~80 行 |
| Step 7 | 監査ログ強化 | ~200 行 |
| Step 8 | ドキュメント更新 | ~300 行 |
| **合計** | | **~1,890 行** |
