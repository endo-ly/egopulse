# Webhook Trigger 仕様

## 目的

Webhook を外部イベントの入口として受け取り、その出来事をきっかけに指定したエージェントを指定したチャネル上で行動させる。

Webhook は EgoPulse の会話チャネルではない。Webhook は trigger であり、実際に発話・応答・履歴保存が行われる場所は receiver ごとに設定された target channel とする。

## ユースケース

第一ユースケースは EgoGraph Pipelines の失敗通知を受け取り、指定チャネルでエージェントに調査・報告・次アクション提示を行わせること。

例:

1. EgoGraph pipeline が失敗する
2. EgoGraph が EgoPulse の Webhook endpoint へ POST する
3. EgoPulse が receiver 設定を解決し、payload をエージェント向け入力文に整形する
4. 指定された Discord / Telegram / Web などの target channel に turn を投入する
5. エージェント応答は target channel の通常配送経路で送信される

## 非目的

- Webhook 専用の `ChannelAdapter` は作らない
- `channels.webhook` は作らない
- Webhook を Web チャンネルへ固定配送しない
- 汎用 event platform や rule engine は作らない
- 初期実装では HMAC 署名、リプレイ防止、複雑な条件分岐ルーティングは扱わない

## HTTP API

```text
POST /api/webhooks/{receiver_id}
Authorization: Bearer <receiver token>
Content-Type: application/json
```

`receiver_id` は設定済み receiver 名に対応する。未設定 receiver は `404 Not Found` を返す。

成功時は turn 完了を待たずに `202 Accepted` を返す。

```json
{
  "ok": true,
  "receiver": "egograph",
  "status": "accepted"
}
```

Webhook route は Web UI API の認証 middleware とは分離して mount する。`/api/config` や `/api/send_stream` の Web auth token ではなく、receiver ごとの token だけで認証する。

## 設定

必要最低限の receiver 単位設定とする。

```yaml
webhooks:
  receivers:
    egograph:
      token:
        source: env
        id: EGOPULSE_WEBHOOK_EGOGRAPH_TOKEN
      target:
        channel: discord
        thread: "1234567890123456789"
        agent: default

    github:
      token:
        source: env
        id: EGOPULSE_WEBHOOK_GITHUB_TOKEN
      target:
        channel: telegram
        thread: "-1001234567890"
        agent: reviewer
```

### フィールド

| フィールド | 必須 | 説明 |
|---|:---:|---|
| `webhooks.receivers.<id>.token` | 必須 | receiver 専用 Bearer token。SecretRef を使用できる |
| `target.channel` | 必須 | turn を投入する既存チャネル名。例: `discord`, `telegram`, `web` |
| `target.thread` | 条件付き | target channel 上の会話先 ID。Discord は channel/thread ID、Telegram は chat ID、Web は session key。`channel != "web"` の場合は必須。Web の空値は `main` に正規化する |
| `target.agent` | 任意 | 行動する agent ID。省略時は `default_agent` |

### Target validation

Webhook 受信時、handler 内で以下を検証する。

- `receiver_id` が定義済みであること
- receiver token が一致すること
- `target.channel` が起動中 `AppState` の `ChannelRegistry` に登録済みであること
- `target.channel` が `voice` ではないこと
- `target.agent` 省略時は `default_agent` に解決し、解決後の agent が `config.agents` に存在すること
- `target.channel != "web"` の場合、`target.thread` が trim 後に空でないこと
- `target.channel == "web"` の場合、空の `target.thread` は `main` に正規化すること

`target.channel` は Config 上の `channels` 定義ではなく、実際に応答配送可能な `ChannelRegistry` 登録状態で判定する。

## データフロー

```text
Webhook sender
  -> POST /api/webhooks/{receiver_id}
  -> receiver token 認証
  -> JSON payload 受信
  -> payload formatter で agent input を生成
  -> target channel / thread / agent から SurfaceContext を生成
  -> TurnScheduler に enqueue
  -> 202 Accepted
  -> agent response は target channel adapter で送信
```

`SurfaceContext.channel` は `webhook` ではなく target channel を使用する。

例:

```text
receiver_id: egograph
target.channel: discord
target.thread: 1234567890123456789
target.agent: default

SurfaceContext:
  channel: discord
  surface_user: webhook:egograph
  surface_thread: 1234567890123456789
  chat_type: discord
  agent_id: default
```

### Session identity

Webhook は会話チャネルではないため、`SurfaceContext.channel` には target channel を使用する。

| フィールド | 値 |
|---|---|
| `channel` | `target.channel` |
| `surface_user` | `webhook:{receiver_id}` |
| `surface_thread` | channel ごとの正規化済み `target.thread` |
| `chat_type` | `target.channel` |
| `agent_id` | 解決後の target agent |
| `origin_id` | webhook event ごとに採番する UUID |

`target.channel == "web"` の場合、`target.thread` は Web session key として扱う。`web:` prefix が付いている場合は既存 Web session 正規化と同じ規則で剥がし、空の場合は `main` に正規化する。

### Persistence

Webhook payload は Discord / Telegram の Channel Log に人間発話として保存しない。通常の agent turn input として target session の履歴に保存する。

## Payload 整形

payload format は設定項目にしない。初期実装では JSON payload を受け、既知 payload は読みやすく整形し、未知 payload は generic JSON として整形する。

### EgoGraph Pipelines

以下の条件に一致する payload は EgoGraph Pipelines 通知として扱う。

- `source == "urn:egograph:pipelines"`
- または `type == "egograph.pipelines.workflow_failed"`

入力文例:

```text
External webhook event from egograph.

type: egograph.pipelines.workflow_failed
workflow_id: spotify_ingest_workflow
run_id: 722e2f38-def8-4bba-9283-bfe07459935c
error_message: AuthenticationError: Spotify refresh token revoked
custom_message: 認証でエラーが発生しました。再認証スクリプトを実行してください: uv run python scripts/spotify_auth.py

Please inspect the failure, identify the likely cause, assess whether user action is required, and report the recommended next action.
```

### Generic JSON

既知 payload でない場合は、主要メタ情報と JSON body を含む入力文にする。

```text
External webhook event from {receiver_id}.

Payload:
{pretty_json}

Please inspect this event and take the appropriate action.
```

## チャネル別 target

| channel | thread の意味 | 応答配送 |
|---|---|---|
| `discord` | Discord channel ID または thread ID | Discord adapter の `send_text` |
| `telegram` | Telegram chat ID | Telegram adapter の `send_text` |
| `web` | Web session key | Web session 履歴に保存。Web の outbound delivery は local-only |

Voice は初期 target 対象外とする。Voice は同期 HTTP response を正規応答経路とするため、Webhook trigger の非同期出力先としては扱わない。

## エラー

| 条件 | HTTP | code |
|---|:---:|---|
| receiver 未定義 | 404 | `webhook_receiver_not_found` |
| token 不一致 | 401 | `unauthorized` |
| JSON 不正 | 400 | `invalid_params` |
| payload size 超過 | 413 | `payload_too_large` |
| target channel 未登録、voice 指定、agent 不在、非 Web target の thread 空 | 400 | `invalid_target` |
| enqueue 失敗 | 500 | `webhook_enqueue_failed` |

turn 実行中の LLM / tool エラーは HTTP response には反映しない。Webhook sender には受信可否のみを返す。

## セキュリティ

- receiver ごとに token を分離する
- token 比較は constant-time comparison を使う
- payload size は固定上限を設ける。初期値は 64KB とし、設定項目にはしない
- token 値はログに出さない
- payload 全文ログは出さない
- `Authorization` header をログに出さない
- HMAC 署名と timestamp replay protection は必要になった段階で追加する

## 実装配置

- HTTP handler: `src/webhooks/`
- Web router: `src/channels/web/mod.rs` に route を mount
- Config: `src/config/types.rs`, `src/config/loader.rs`, `src/config/resolve.rs`
- Turn 投入: 既存の `SurfaceContext` と `TurnScheduler` を使用

`ChannelRegistry` には Webhook を登録しない。
