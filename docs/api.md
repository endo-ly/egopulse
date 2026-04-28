# EgoPulse WebUI API

WebUI が使用する REST API および WebSocket の仕様。

## 目次

1. [認証](#1-認証)
2. [REST API](#2-rest-api)
   - [ヘルスチェック](#21-ヘルスチェック)
   - [設定](#22-設定)
   - [セッション](#23-セッション)
   - [メッセージ履歴](#24-メッセージ履歴)
   - [ストリーミングチャット](#25-ストリーミングチャット)
3. [WebSocket](#3-websocket)
4. [エラーレスポンス](#4-エラーレスポンス)
5. [静的アセット](#5-静的アセット)

---

## 1. 認証

すべての `/api/*` エンドポイントは Bearer トークン認証が必要。

```
Authorization: Bearer <token>
```

トークンは `channels.web.auth_token` で設定する。

---

## 2. REST API

### 2.1 ヘルスチェック

**認証不要**

```
GET /health
```

#### レスポンス (200)

```json
{
  "ok": true,
  "version": "0.1.0"
}
```

---

### 2.2 設定

#### 取得

```
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

```
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

### 2.3 セッション一覧

```
GET /api/sessions
```

#### レスポンス (200)

```json
{
  "ok": true,
  "sessions": [
    {
      "session_key": "main",
      "label": "Web Chat",
      "chat_id": 1,
      "channel": "web",
      "last_message_time": "2026-04-12T14:03:58Z",
      "last_message_preview": "最新メッセージの先頭..."
    }
  ]
}
```

| フィールド | 型 | 説明 |
|-----------|-----|------|
| `session_key` | `string` | セッション識別キー |
| `label` | `string` | 表示ラベル |
| `chat_id` | `number` | 内部チャット ID |
| `channel` | `string` | チャネル種別（`web`, `discord`, `telegram` 等） |
| `last_message_time` | `string` | 最終メッセージ時刻 (RFC 3339) |
| `last_message_preview` | `string \| null` | 最終メッセージの先頭プレビュー |

---

### 2.4 メッセージ履歴

```
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
  "session_key": "main",
  "messages": [
    {
      "id": 1,
      "sender_name": "User",
      "content": "こんにちは",
      "is_from_bot": false,
      "timestamp": "2026-04-12T14:00:00Z"
    },
    {
      "id": 2,
      "sender_name": "EgoPulse",
      "content": "こんにちは！何かお手伝いできますか？",
      "is_from_bot": true,
      "timestamp": "2026-04-12T14:00:05Z"
    }
  ]
}
```

---

### 2.5 ストリーミングチャット

#### リクエスト送信

```
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

```
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

```
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

## 3. WebSocket

### 接続

```
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
      "events": ["connect.challenge", "chat"]
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
| `done` | 完了。`message` に最終応答を含む |
| `error` | エラー。`errorMessage` を含む |

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