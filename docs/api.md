# EgoPulse HTTP API

WebUI と外部 voice client が使用する REST API、および WebSocket の仕様。

## 目次

1. [認証](#1-認証)
2. [REST API](#2-rest-api)
    - [ヘルスチェック](#21-ヘルスチェック)
    - [メトリクス](#22-メトリクス)
    - [設定](#23-設定)
    - [セッション一覧](#24-セッション一覧)
    - [メッセージ履歴](#25-メッセージ履歴)
    - [ストリーミングチャット](#26-ストリーミングチャット)
    - [Voice turn](#27-voice-turn)
    - [Webhook](#29-webhook)
3. [WebSocket](#3-websocket)
4. [エラーレスポンス](#4-エラーレスポンス)
5. [静的アセット](#5-静的アセット)

---

## 1. 認証

保護された `/api/*` エンドポイントは Bearer トークン認証が必要。

```text
Authorization: Bearer <token>
```

### Web API 認証

`/api/config`、`/api/sessions`、`/api/history`、`/api/send_stream`、`/api/stream`、`/api/agents`、`/api/sleep/*` は `channels.web.auth_token` で認証する。

### Voice API 認証

`POST /api/voice/turn` は `channels.voice.auth_token` で認証する。Web token と Voice token は別 credential であり、相互に代用できない。

`channels.voice.enabled: false` の場合、Voice route は公開されず 404 となる。

### Webhook API 認証

`POST /api/webhooks/{receiver_id}` は receiver ごとの Bearer token で認証する。`webhooks.receivers.<id>.token` で設定し、Web API token・Voice token とは独立に運用する。Webhook route は Web API auth middleware の管轄外であり、receiver token のみで処理される。

---

## 2. REST API

### 2.1 ヘルスチェック

**認証不要**

チャネル状態、DB 接続、MCP 接続、アクティブターン数、直近エラーを含むフルヘルス情報。

```text
GET /health
```

#### レスポンス (200)

```json
{
  "ok": true,
  "version": "0.1.0",
  "uptime_secs": 86400,
  "pid": 12345,
  "db": { "ok": true },
  "accepting_inputs": true,
  "shutdown_started": false,
  "critical_task_failure": null,
  "owned_task_count": 4,
  "channels": {
    "web": { "state": "running", "last_error": null, "last_activity": "2026-05-23T10:00:00Z" },
    "discord": { "state": "starting", "last_error": null, "last_activity": null },
    "telegram": { "state": "failed", "last_error": "bot token rejected", "last_activity": null }
  },
  "mcp": {
    "healthy": 1,
    "failed": 0,
    "servers": []
  },
  "active_turns": 2,
  "recent_errors_count": 3
}
```

| フィールド | 型 | 説明 |
|-----------|-----|------|
| `ok` | `boolean` | DB 正常かつ少なくとも 1 チャネル Running かつ受付中（shutdown / critical task failure なし）のとき `true` |
| `version` | `string` | EgoPulse バージョン |
| `uptime_secs` | `number` | 起動からの経過秒数 |
| `pid` | `number` | プロセス ID |
| `db` | `object` | DB 接続状態（`{ "ok": boolean }`） |
| `accepting_inputs` | `boolean` | Runtime が新規入力を受付中か。shutdown 開始で `false` |
| `shutdown_started` | `boolean` | graceful shutdown が開始したか |
| `critical_task_failure` | `string \| null` | critical 長寿命 task の異常終了要約（なければ `null`） |
| `owned_task_count` | `number` | supervisor が所有する長寿命 task 数 |
| `channels` | `object` | 各チャネルの状態。値は `{ state, last_error, last_activity }` |
| `channels.*.state` | `string` | チャネル状態（`starting` / `running` / `failed` / `stopped`） |
| `channels.*.last_error` | `string \| null` | 直近のエラーメッセージ |
| `channels.*.last_activity` | `string \| null` | 直近のアクティビティ時刻（RFC 3339） |
| `mcp` | `object` | MCP サーバーの接続状態（`{ healthy, failed, servers }`） |
| `mcp.servers[]` | `array` | MCP サーバー一覧。各要素は `{ name, connected, error? }` |
| `active_turns` | `number` | 現在実行中のエージェントターン数 |
| `recent_errors_count` | `number` | 直近のエラー数（リングバッファ、最大 100 件、再起動で消失） |

---

### 2.2 テレメトリー

**認証不要**

JSON 形式でメトリクス・直近ターン履歴・エラー詳細を返す。AI エージェントの運用監視向け。

```text
GET /telemetry
```

#### レスポンス (200)

```json
{
  "metrics": {
    "egopulse_turns_total": [
      { "labels": { "agent": "alice", "channel": "discord" }, "value": 42.0 }
    ],
    "egopulse_turn_errors_total": [
      { "labels": { "kind": "llm", "agent": "alice" }, "value": 3.0 }
    ],
    "egopulse_llm_tokens_total": [
      { "labels": { "direction": "input", "provider": "openrouter" }, "value": 15000.0 },
      { "labels": { "direction": "output", "provider": "openrouter" }, "value": 3200.0 }
    ],
    "egopulse_tool_calls_total": [
      { "labels": { "tool": "shell", "status": "ok" }, "value": 28.0 }
    ],
    "egopulse_active_turns": [
      { "labels": {}, "value": 2.0 }
    ]
  },
  "recent_turns": [
    { "trace_id": "abc-123", "agent_id": "alice", "channel": "discord", "started_at": "2025-05-22T10:30:00Z", "duration_secs": 5.2, "ok": true }
  ],
  "recent_errors": [
    { "at": "2025-05-22T10:31:00Z", "trace_id": "def-456", "error_kind": "turn_failure", "agent_id": "alice", "channel": "discord", "summary": "rate limited" }
  ]
}
```

| フィールド | 型 | 説明 |
|-----------|-----|------|
| `metrics` | `object` | メトリクス名 → `[{labels, value}]` のマップ |
| `recent_turns` | `array` | 直近 100 件のターン履歴（新しい順） |
| `recent_errors` | `array` | 直近 100 件のエラー詳細（`trace_id` 付き） |

---

### 2.3 設定

#### 取得

```text
GET /api/config
```

##### レスポンス (200)

```json
{
  "ok": true,
  "config": {
    "default_provider": "openrouter",
    "default_model": null,
    "effective_model": "anthropic/claude-sonnet-4",
    "state_root": "/home/user/.egopulse",
    "workspace_dir": "/home/user/.egopulse/workspace",
    "web_enabled": true,
    "web_host": "127.0.0.1",
    "web_port": 10961,
    "web_auth_enabled": true,
    "has_api_key": true,
    "config_path": "/home/user/.egopulse/egopulse.config.yaml",
    "providers": [
      {
        "id": "openrouter",
        "label": "OpenRouter",
        "base_url": "https://openrouter.ai/api/v1",
        "default_model": "anthropic/claude-sonnet-4",
        "models": ["anthropic/claude-sonnet-4", "google/gemini-2.5-pro"],
        "has_api_key": true
      }
    ],
    "channel_overrides": {
      "discord": { "provider": "openrouter", "model": null },
      "telegram": { "provider": null, "model": null }
    }
  }
}
```

| フィールド | 型 | 説明 |
|-----------|-----|------|
| `default_provider` | `string` | デフォルトプロバイダー ID |
| `default_model` | `string \| null` | グローバルモデルオーバーライド |
| `effective_model` | `string` | 解決後の実際のモデル名 |
| `state_root` | `string` | `~/.egopulse/` の絶対パス |
| `workspace_dir` | `string` | エージェント作業ディレクトリ |
| `web_enabled` | `boolean` | Web サーバー有効状態 |
| `web_host` | `string` | バインドホスト |
| `web_port` | `number` | バインドポート |
| `web_auth_enabled` | `boolean` | 認証の有無（`auth_token` 設定時 `true`） |
| `has_api_key` | `boolean` | デフォルトプロバイダーの API キー設定有無 |
| `config_path` | `string` | 設定ファイルパス |
| `providers[].has_api_key` | `boolean` | プロバイダーごとの API キー有無 |

#### 更新

```text
PUT /api/config
```

##### リクエスト

```json
{
  "default_provider": "openrouter",
  "default_model": null,
  "providers": {
    "openrouter": {
      "label": "OpenRouter",
      "base_url": "https://openrouter.ai/api/v1",
      "api_key": "sk-or-v1-...",
      "default_model": "anthropic/claude-sonnet-4",
      "models": ["anthropic/claude-sonnet-4"]
    }
  },
  "web_enabled": true,
  "web_host": "127.0.0.1",
  "web_port": 10961,
  "channel_overrides": {
    "discord": { "provider": "openrouter", "model": null },
    "telegram": { "provider": null, "model": null }
  }
}
```

| フィールド | 必須 | 備考 |
|-----------|:---:|------|
| `default_provider` | 必須 | |
| `default_model` | 任意 | `null` 可 |
| `providers` | 任意 | プロバイダー追加/削除は非対応。既存プロバイダーの編集のみ |
| `providers.<id>.api_key` | 任意 | 省略時は変更なし。実値は `.env` に保存、YAML には SecretRef として記録 |
| `web_enabled` | 必須 | |
| `web_host` | 必須 | |
| `web_port` | 必須 | |
| `channel_overrides` | 任意 | |

##### レスポンス (200)

GET と同一形式。

---

### 2.4 セッション一覧

```text
GET /api/sessions
```

#### レスポンス (200)

```json
{
  "ok": true,
  "sessions": [
    {
      "session_key": "chat:1",
      "label": "web:main:agent:lyre",
      "chat_id": 1,
      "channel": "web",
      "agent_id": "lyre",
      "last_message_time": "2026-04-12T14:03:58Z",
      "last_message_preview": "最新メッセージの先頭..."
    }
  ]
}
```

| フィールド | 型 | 説明 |
|-----------|-----|------|
| `session_key` | `string` | セッション識別キー。全チャネルで `chat:{chat_id}` 形式に統一されており、`GET /api/history` や WebSocket `chat.send` でそのまま指定できる |
| `label` | `string` | 表示ラベル（`external_chat_id`） |
| `chat_id` | `number` | 内部チャット ID |
| `channel` | `string` | チャネル種別（`web`, `discord`, `telegram` 等） |
| `agent_id` | `string` | セッションを所有する agent ID |
| `last_message_time` | `string` | 当該チャットの最新メッセージのタイムスタンプ (RFC 3339)。メッセージが無い場合はチャット作成時刻。レスポンスはこの値の降順（同一値は `chat_id` 降順）でソートされる |
| `last_message_preview` | `string \| null` | 最終メッセージの先頭プレビュー |

---

### 2.5 メッセージ履歴

```text
GET /api/history?session_key=main&limit=100
```

#### クエリパラメータ

| パラメータ | 必須 | デフォルト | 最大 | 説明 |
|-----------|:---:|----------|------|------|
| `session_key` | 任意 | `"main"` | | セッション識別キー |
| `limit` | 任意 | `100` | `500` | 取得メッセージ数 |

#### レスポンス (200)

```json
{
  "ok": true,
  "session_key": "chat:1",
  "messages": [
    {
      "id": "m1",
      "sender_id": "user:web",
      "sender_kind": "user",
      "content": "こんにちは",
      "timestamp": "2026-04-12T14:00:00Z",
      "message_kind": "message"
    },
    {
      "id": "m2",
      "sender_id": "lyre",
      "sender_kind": "assistant",
      "content": "こんにちは！何かお手伝いできますか？",
      "timestamp": "2026-04-12T14:00:05Z",
      "message_kind": "message"
    },
    {
      "id": "tool:call_1",
      "sender_id": "assistant",
      "sender_kind": "assistant",
      "content": "{\"tool\":\"read\",\"status\":\"success\",\"result\":\"...\",\"input\":{\"path\":\"a.txt\"}}",
      "timestamp": "2026-04-12T14:00:06Z",
      "message_kind": "tool_call"
    }
  ]
}
```

`session_key` は `chat:{id}` を指定するとそのままチャットを特定できる。新規セッション（未永続）の `session_key` を指定した場合はメッセージなし（空配列）で返る。

メッセージの `sender_kind` は `user` / `assistant` / `system`。`message_kind` は `message` / `agent_send` / `system_event` / `tool_call`。`message_kind: "tool_call"` のメッセージはツール実行結果で、`content` に JSON 文字列（`{tool, status, result, input}`）を持ち、WebUI は折りたたみ可能なツールカードとして描画する。

エントリの並び順は次の 2 層で決まる:

1. `messages` を `timestamp` 昇順にソートする
2. 各メッセージの直後に、それを親（`tool_calls.message_id`）とするツール呼び出しを `timestamp` 昇順で挿入する

これにより、`tool_calls` と発行元メッセージ間の timestamp ズレによらずツールカードは親メッセージの直後に固定される。`messages` 同士の順序は timestamp に依存するため、一括永続化パス（Pulse など）では永続化の都度新鮮な timestamp を採番し、保存順と時系列順が一致するよう保証している。いずれの履歴も LLM コンテキストには含まれない。

ツール呼び出しを伴うターンでは、テキストベースチャネル（TUI / Discord など）向けに `messages` テーブルへ tool プレビューが assistant メッセージとして保存される。WebUI はツール情報を `tool_calls` テーブルから構造化されたツールカードとして描画するため、`GET /api/history` では次のプレビューを除外する: ツール結果プレビュー（`[tool_result]: ...` / `[tool_error]: ...`。Markdown でリンク参照定義として解釈されて空描画され、かつ `tool_calls` テーブルと完全重複）と、発言を含まないツール呼び出しプレビュー（`[tool_call] {name}`。ツールカードと完全重複）。エージェントの発言を伴うもの（`{text} [tool_call] {name}`）は発言内容を残すために返却される。

---

### 2.6 ストリーミングチャット

#### リクエスト送信

```text
POST /api/send_stream
```

##### リクエスト

```json
{
  "session_key": "main",
  "message": "今日の天気は？"
}
```

##### レスポンス (200)

```json
{
  "ok": true,
  "run_id": "550e8400-e29b-41d4-a716-446655440000",
  "session_key": "main"
}
```

#### SSE イベント受信

```text
GET /api/stream?run_id=550e8400-e29b-41d4-a716-446655440000&last_event_id=0
```

`Content-Type: text/event-stream`

##### クエリパラメータ

| パラメータ | 必須 | 説明 |
|-----------|:---:|------|
| `run_id` | 必須 | `POST /api/send_stream` で返却された UUID |
| `last_event_id` | 任意 | 再接続時に指定。指定した ID 以降のイベントを再送する |

##### SSE イベント一覧

| イベント | 説明 |
|---------|------|
| `replay_meta` | 再接続時の truncated / complete 情報 |
| `status` | 実行状態の変化（`started`, `completed`, `error`） |
| `iteration` | エージェントループのイテレーション番号 |
| `tool_start` | ツール実行開始。ツール名と入力パラメータを含む |
| `tool_result` | ツール実行完了。出力と成否を含む |
| `delta` | LLM からのストリーミングテキスト差分 |
| `done` | 最終応答。完全なアシスタントメッセージ |
| `error` | エラー発生時 |

##### イベント例

```text
event: delta
data: {"text": "今日の東京の天気は"}

event: delta
data: {"text": "晴れです。気温は..."}

event: done
data: {"message": {"role": "assistant", "content": "今日の東京の天気は晴れです。..."}, "session_key": "main"}
```

##### 再接続

切断時に `last_event_id` を指定して再接続すると、途中から再開できる。
RunHub は最大 512 イベント、5 分間の TTL でイベントを保持する。

---

### 2.7 Voice turn

STT 済みテキストを通常の agent turn として処理し、応答テキストを同期的に返す。

```text
POST /api/voice/turn
Authorization: Bearer <channels.voice.auth_token>
Content-Type: application/json
```

#### リクエスト

```json
{
  "surface": "stackchan",
  "session_key": "main",
  "user_id": "local-speaker",
  "text": "聞こえていますか",
  "source": "stackchan-wake",
  "agent_id": "default"
}
```

| フィールド | 必須 | デフォルト | 説明 |
|---|:---:|---|---|
| `text` | 必須 | なし | STT 結果。trim 後の空文字は拒否 |
| `surface` | 任意 | `channels.voice.default_surface` | Voice surface。空文字と `:` を拒否 |
| `session_key` | 任意 | `channels.voice.default_session` | surface 内のセッション。空文字と `:` を拒否 |
| `user_id` | 任意 | `voice-user` | `surface_user` に使用する安定した発話者名 |
| `source` | 任意 | `unknown` | Wake 等の発生源。session identity や認可には使用しない |
| `agent_id` | 任意 | `default_agent` | turn を処理する agent |

#### レスポンス (200)

```json
{
  "ok": true,
  "response": "はい、聞こえています。",
  "session_key": "main",
  "surface": "stackchan",
  "surface_thread": "stackchan:main",
  "agent_id": "default",
  "trace_id": "550e8400-e29b-41d4-a716-446655440000"
}
```

履歴は `channel=voice`、`surface_thread={surface}:{session_key}` の通常 user / assistant message として保存される。LLM の user message 本文へ渡すのは `text` のみ。

#### Voice API エラー

| コード | HTTP | 説明 |
|---|:---:|---|
| `unauthorized` | 401 | Voice token がない、または一致しない |
| `invalid_params` | 400 | text または identity component が不正 |
| `surface_not_allowed` | 403 | surface が `allowed_surfaces` に含まれない |
| `turn_failed` | 500 | agent turn 処理に失敗 |

責務境界、session identity、voice client との接続契約は [voice-channel.md](./voice-channel.md) を正本とする。

---

### 2.8 Sleep Batch

Sleep Batch の実行履歴とメモリ変更差分を確認するためのエンドポイント。

#### Agent 一覧

```text
GET /api/agents
```

Sleep Batch 実行履歴がある agent ID の一覧を返す。

##### レスポンス (200)

```json
{
  "ok": true,
  "agents": ["default", "alice"]
}
```

---

#### Sleep Run 一覧

```text
GET /api/sleep/runs?agent_id={agent_id}&limit={limit}
```

##### クエリパラメータ

| パラメータ | 必須 | デフォルト | 説明 |
|-----------|:---:|----------|------|
| `agent_id` | 必須 | — | エージェント ID |
| `limit` | 任意 | `20` | 最大取得件数 |

##### レスポンス (200)

```json
{
  "ok": true,
  "runs": [
    {
      "id": "550e8400-e29b-41d4-a716-446655440000",
      "agent_id": "default",
      "status": "success",
      "trigger_type": "scheduled",
      "started_at": "2026-06-01T04:00:00+09:00",
      "finished_at": "2026-06-01T04:02:30+09:00",
      "input_tokens": 15000,
      "output_tokens": 5000,
      "session_count": 3
    }
  ]
}
```

| フィールド | 型 | 説明 |
|-----------|-----|------|
| `status` | `string` | `running`, `success`, `partial_failure`, `failed`, `skipped` |
| `trigger_type` | `string` | `manual`, `scheduled`, `backfill` |
| `session_count` | `number` | `source_chats_json` から算出した対象セッション数 |

---

#### Sleep Run 詳細

```text
GET /api/sleep/runs/{run_id}
```

##### レスポンス (200)

```json
{
  "ok": true,
  "run": {
    "id": "550e8400-e29b-41d4-a716-446655440000",
    "agent_id": "default",
    "status": "success",
    "trigger_type": "scheduled",
    "started_at": "2026-06-01T04:00:00+09:00",
    "finished_at": "2026-06-01T04:02:30+09:00",
    "input_tokens": 15000,
    "output_tokens": 5000,
    "error_message": null
  },
  "snapshots": [
    {
      "file": "semantic",
      "content_before": "# Semantic\n\n- fact A",
      "content_after": "# Semantic\n\n- fact A\n- fact B"
    }
  ]
}
```

| フィールド | 型 | 説明 |
|-----------|-----|------|
| `run` | `object` | sleep_runs テーブルの内容 |
| `snapshots` | `array` | この run で変更された memory_snapshots（`content_before` ≠ `content_after` のもの）。ファイルは `episodic`, `semantic`, `prospective` |

---

### 2.9 Webhook

外部イベントを trigger として受け取り、receiver ごとに設定した target channel 上で agent turn を enqueue する。Webhook は会話チャネルではなく、応答は target channel の通常配送経路で送信される。

```text
POST /api/webhooks/{receiver_id}
Authorization: Bearer <webhooks.receivers.<id>.token>
Content-Type: application/json
```

`receiver_id` は設定済み receiver 名。未設定の場合は `404 webhook_receiver_not_found`。

成功時は turn 完了を待たず `202 Accepted` を返す。`202` は `turn_runs` への accepted commit が完了した後に返る。commit 後に in-memory scheduler の同時実行上限に達しても拒否とは扱わず、dispatcher への deferred（容量が空き次第の再投入）として同じ `202` を返す。再起動後に `TurnDispatcher` が再実行するのは `accepted`（受付から再開）と `input_committed`（model loop から resume）の2状態のみで、モデル反復開始後（`model_pending` 以降）は再実行対象外となる。

受付拒否時は理由コード違いで一律 `429` を返す。拒否はすべて accepted commit と同一トランザクション内で判定され、`429` を返した turn は `turn_runs` に書き込まれない（`session_queue_full` / `global_queue_full` / `tracker_full` / `chain_terminated` / `shutdown`、受付処理の内部エラーや同一 `request_key` へ異なる本文の再受付は `internal`）。

```json
{
  "ok": true,
  "receiver": "egograph",
  "status": "accepted"
}
```

#### Target 解決

receiver の `target.channel / target.thread / target.agent` から `SurfaceContext` を構築し、`TurnScheduler` へ投入する。

| フィールド | 値 |
|---|---|
| `channel` | `target.channel` |
| `surface_user` | `webhook:{receiver_id}` |
| `surface_thread` | `target.thread`（Web target は `web:` prefix 剥離・空値 `main` 正規化） |
| `chat_type` | `target.channel` |
| `agent_id` | `target.agent`（省略時 `default_agent`） |
| `origin_id` | webhook event ごとの UUID |
| `scope` | Discord / Telegram target は `target.thread` が対応する channel map に解決できた場合、その channel の `secret` 値から決定（`secret: true` なら `Secret`、そうでなければ `Normal`）。Web target は常に `Normal`。解決できない場合は受付を拒否する（下記 Validation / Webhook エラー参照） |

#### Payload 整形

payload format は設定項目化しない。JSON payload を受け、既知 payload は整形し、未知 payload は generic JSON として扱う。

- **EgoGraph Pipelines**: `source == "urn:egograph:pipelines"` または `type == "egograph.pipelines.workflow_failed"` の場合、`type / workflow_id / run_id / error_message / custom_message` を抽出して agent 向け入力文に整形する。
- **Generic JSON**: 上記以外は receiver id と pretty JSON を含む入力文に整形する。

#### Validation

受信時に以下を検証する。いずれかを満たさない場合は turn を投入せずエラーを返す。

- `receiver_id` が定義済み
- receiver token が一致
- payload size が 64KB 以下
- `target.channel` が `ChannelRegistry` に登録済みで `voice` ではない
- 解決後 agent が `config.agents` に存在
- 非 Web target の `target.thread` が空でない
- Discord / Telegram target の `target.thread` が `channels.<channel>` の登録エントリに解決できること（数値として parse 可能・未登録 thread・channel map 欠落は拒否。`Normal` への降格なし）

#### Webhook エラー

| コード | HTTP | 説明 |
|---|:---:|---|
| `webhook_receiver_not_found` | 404 | receiver が未定義 |
| `unauthorized` | 401 | receiver token 不一致 |
| `invalid_params` | 400 | JSON 不正 |
| `payload_too_large` | 413 | payload が 64KB を超過 |
| `invalid_target` | 400 | target channel 未登録・voice・agent 不在・thread 空 |
| `invalid_target_scope` | 400 | Discord / Telegram target thread が channel map に解決できない |
| `session_queue_full` | 429 | 対象セッションの durable pending（`accepted`/`input_committed`）が上限（32）に達し、INSERT と同一トランザクションで受付を拒否した |
| `global_queue_full` | 429 | Runtime 全体の durable pending が上限（512）に達し、INSERT と同一トランザクションで受付を拒否した |
| `tracker_full` | 429 | origin の turn tracker が追跡上限（同時追跡可能な origin 数）に達し、新規 origin の受付を拒否した |
| `chain_terminated` | 429 | 同一 origin の turn chain が既に終了（terminal reason 記録済み）しており、受付を拒否した |
| `shutdown` | 429 | Runtime が shutdown 中であり、新規 Turn の受付を拒否した |
| `internal` | 429 | 受付処理の内部エラー（同一 `request_key` へ異なる本文の再受付による hash 不一致を含む） |

---

## 3. WebSocket

### 接続

```text
WS /ws
```

- 最大接続数: 64
- 最大メッセージサイズ: 64KB
- Origin 検証あり

### プロトコル

JSON-RPC 風の双方向メッセージング。

#### ハンドシェイク

サーバーが接続時に `connect.challenge` を送信：

```json
{
  "type": "event",
  "event": "connect.challenge",
  "payload": {
    "protocol": 1,
    "connId": "uuid"
  }
}
```

#### 認証

クライアントは `connect` メソッドで認証：

```json
{
  "type": "req",
  "id": "1",
  "method": "connect",
  "params": {
    "minProtocol": 1,
    "maxProtocol": 1,
    "auth_token": "<web_auth_token>"
  }
}
```

##### 成功レスポンス

```json
{
  "type": "res",
  "id": "1",
  "ok": true,
  "payload": {
    "protocol": 1,
    "server": { "version": "0.1.0", "connId": "uuid" },
    "features": {
      "methods": ["connect", "chat.send"],
      "events": ["connect.challenge", "chat", "tool_start", "tool_result"]
    }
  }
}
```

#### チャット送信

```json
{
  "type": "req",
  "id": "2",
  "method": "chat.send",
  "params": {
    "sessionKey": "main",
    "message": "こんにちは"
  }
}
```

##### 成功レスポンス

```json
{
  "type": "res",
  "id": "2",
  "ok": true,
  "payload": {
    "runId": "uuid",
    "status": "accepted"
  }
}
```

#### チャットイベント受信

サーバーからは `chat` イベントでストリーミング結果が送られる：

```json
{
  "type": "event",
  "event": "chat",
  "payload": {
    "runId": "uuid",
    "sessionKey": "main",
    "seq": 1,
    "state": "delta",
    "message": {
      "role": "assistant",
      "content": [{"type": "text", "text": "こんにちは！"}]
    }
  }
}
```

| state | 説明 |
|-------|------|
| `delta` | テキストの差分。`message` を含む |
| `done` | 完了。`message` に最終応答を含む。新規セッションの場合は `sessionKey` が永続化された `chat:{id}` に切り替わる |
| `error` | エラー。`errorMessage` を含む |

#### ツールイベント受信

エージェントがツールを実行すると、専用のイベントで通知される。WebUI はこれらを受信して折りたたみ可能なツールカードを描画する。

```json
{
  "type": "event",
  "event": "tool_start",
  "payload": {
    "callId": "call_1",
    "name": "read",
    "input": {"path": "a.txt"}
  }
}
```

```json
{
  "type": "event",
  "event": "tool_result",
  "payload": {
    "callId": "call_1",
    "name": "read",
    "isError": false,
    "preview": "ファイル内容の先頭...",
    "durationMs": 87
  }
}
```

| イベント | 説明 |
|---------|------|
| `tool_start` | ツール実行開始。`callId`, `name`, `input` を含む |
| `tool_result` | ツール実行完了。`callId`, `name`, `isError`, `preview`, `durationMs` を含む。同じ `callId` の `tool_start` を更新する |

`preview` はツール出力の先頭（最大 200 文字）。完全な出力は履歴取得時に `tool_calls` テーブルから復元される。

---

## 4. エラーレスポンス

全エンドポイント共通のエラーフォーマット：

```json
{
  "ok": false,
  "error": "error_code",
  "message": "エラーの詳細"
}
```

### 主なエラーコード

| コード | HTTP ステータス | 説明 |
|-------|:---:|------|
| `unauthorized` | 401 | 認証トークンが無効または未設定 |
| `web_auth_not_configured` | 500 | サーバーに `auth_token` が設定されていない |
| `invalid_params` | 400 | リクエストパラメータが不正 |
| `surface_not_allowed` | 403 | Voice surface が allowlist 外 |
| `turn_failed` | 500 | Voice agent turn の処理失敗 |
| `internal_error` | 500 | サーバー内部エラー |

---

## 5. 静的アセット

| パス | 内容 |
|------|------|
| `GET /` | `index.html` (WebUI のエントリポイント) |
| `GET /favicon.ico` | ファビコン |
| `GET /icon.png` | アプリアイコン |
| `GET /assets/*` | JavaScript, CSS, 画像等の静的ファイル |

その他のパスはすべて `index.html` にフォールバック（SPA ルーティング）。
