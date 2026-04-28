# EgoPulse チャネル仕様

Web / Discord / Telegram / TUI / CLI の各チャネルの接続方式、メッセージフロー、設定、制約を記述する。

## 目次

1. [共通アーキテクチャ](#1-共通アーキテクチャ)
2. [Web](#2-web)
3. [Discord](#3-discord)
4. [Telegram](#4-telegram)
5. [TUI](#5-tui)
6. [CLI](#6-cli)
7. [チャネル横断の設定オーバーライド](#7-チャネル横断の設定オーバーライド)

---

## 1. 共通アーキテクチャ

### ChannelAdapter trait

全チャネルは `ChannelAdapter` trait を実装し、`ChannelRegistry` に登録される。
エージェントループはチャネルを意識せず、`ChannelRegistry` を通じて応答を返送する。

```rust
pub trait ChannelAdapter: Send + Sync {
    fn name(&self) -> &str;
    fn chat_type_routes(&self) -> Vec<(&str, ConversationKind)>;
    async fn send_text(&self, external_chat_id: &str, text: &str) -> Result<(), String>;
    async fn send_attachment(&self, external_chat_id: &str, text: Option<&str>, file_path: &Path, caption: Option<&str>) -> Result<(), String>;
}
```

### メッセージフロー（全チャネル共通）

```text
[プラットフォーム] → メッセージ受信
    │
    ├─ SurfaceContext 生成 (channel, user, thread, agent_id)
    │
    ├─ agent_loop::process_turn(state, ctx, message)
    │    ├ chat_id 解決 → セッション復元 → compaction
    │    ├ system prompt 構築 → LLM 呼び出し → ツール実行
    │    └ メッセージ永続化
    │
    └─ ChannelAdapter::send_text() で応答を返送
```

### 起動

`egopulse run` 実行時、有効化されたチャネルがそれぞれ `tokio::spawn` で並列起動される。
いずれかのチャネルが異常終了すると全チャネルが停止する。

---

## 2. Web

### 接続方式

Axum HTTP サーバー。`channels.web.host:port` で待ち受け。
WebSocket (`/ws`) と SSE (`/api/stream`) の 2 種類のストリーミング方式を提供。

### データフロー

- **メッセージ受信**: HTTP POST `/api/send_stream` または WebSocket `chat.send`
- **ストリーミング**: RunHub を介した publish/subscribe モデル
- **再接続**: `last_event_id` によるイベントリプレイ対応（最大 512 イベント、5 分 TTL）
- **応答**: WebAdapter は local-only で送信不可（Web クライアントへの送信は SSE/WS が直接行う）

### 設定

| フィールド | 型 | デフォルト | 説明 |
|-----------|-----|----------|------|
| `channels.web.enabled` | `bool` | `false` | Web UI の有効化 |
| `channels.web.host` | `string` | `"127.0.0.1"` | バインドホスト |
| `channels.web.port` | `u16` | `10961` | バインドポート |
| `channels.web.auth_token` | `string` | なし | 認証トークン（**Web 有効時は必須**） |
| `channels.web.allowed_origins` | `[string]` | `[]` | CORS 許可オリジン |

### 制約

- 認証トークン未設定時は `/api/*` へのアクセスができない
- WebSocket 最大接続数: 64
- WebSocket 最大メッセージサイズ: 64KB
- WebSocket の 1 接続あたり同時 `chat.send` は 1 つまで

---

## 3. Discord

### 接続方式

`serenity` フレームワークで Discord Gateway WebSocket に接続。
`channels.discord.bots` に定義されたボットごとにクライアントを起動。
必要なインテント: `GUILD_MESSAGES`, `DIRECT_MESSAGES`, `MESSAGE_CONTENT`。

### データフロー

- **メッセージ受信**: `Handler::message()` がトリガー
- **添付ファイル**: `workspace/media/inbound/` に自動ダウンロード
- **エージェント選択**:
  - DM → ボットの `default_agent`
  - ギルドチャンネル → `channel_agents[channel_id]` が設定されていればそれ、なければ `default_agent`
- **応答**: 2000 文字制限に合わせて自動分割送信。429 (Rate Limit) 時は Retry-After で 3 回リトライ

### 設定

| フィールド | 型 | デフォルト | 説明 |
|-----------|-----|----------|------|
| `channels.discord.enabled` | `bool` | `false` | Discord Bot の有効化 |
| `channels.discord.bots.<bot_id>.token` | `string` | なし | Bot トークン（必須） |
| `channels.discord.bots.<bot_id>.default_agent` | `string` | なし | DM に使うエージェント（必須） |
| `channels.discord.bots.<bot_id>.allowed_channels` | `[u64]` | `[]` | 応答するギルドチャンネル ID。空の場合ギルド全拒否 |
| `channels.discord.bots.<bot_id>.channel_agents` | `map` | なし | チャンネル ID → エージェント ID のマッピング |
| `channels.discord.provider` | `string \| null` | `null` | チャネル別プロバイダーオーバーライド |
| `channels.discord.model` | `string \| null` | `null` | チャネル別モデルオーバーライド |

### 制約

- 1 メッセージ 2000 文字（自動分割）
- HTTP タイムアウト 10 秒
- レート制限時は 3 回までリトライ
- ボット自身のメッセージは無視
- `allowed_channels` 設定時はリスト外のギルドメッセージに応答しない

### Bot 作成手順

1. [Discord Developer Portal](https://discord.com/developers/applications) でアプリケーション作成
2. Bot ページでトークンを生成
3. Privileged Gateway Intents で `MESSAGE CONTENT INTENT` を有効化
4. OAuth2 URL で `bot` + `Send Messages` / `Read Message History` / `Attach Files` スコープを付与し、サーバーに招待

---

## 4. Telegram

### 接続方式

`teloxide` フレームワークの Long Polling モードで接続。
起動時に既存の Webhook を削除し、ポーリングに切り替える。

### データフロー

- **メッセージ受信**: `handle_message()` がトリガー
- **添付ファイル**: 写真（最大サイズのものを選択）・ドキュメント・音声を `workspace/media/inbound/` にダウンロード
- **チャット種別判定**: `private`, `group`, `supergroup` で処理を分岐
- **アクセス制御**:
  - グループ: `allowed_chat_ids` に含まれる chat_id のみ即応答。それ以外は `@mention` 必須
- **応答**: 4096 文字制限に合わせて自動分割送信

### 設定

| フィールド | 型 | デフォルト | 説明 |
|-----------|-----|----------|------|
| `channels.telegram.enabled` | `bool` | `false` | Telegram Bot の有効化 |
| `channels.telegram.bot_token` | `string` | なし | Bot トークン（有効時は必須）。環境変数 `TELEGRAM_BOT_TOKEN` でも指定可 |
| `channels.telegram.bot_username` | `string` | なし | Bot のユーザー名（有効時は必須。グループ内 `@mention` 検知に使用） |
| `channels.telegram.allowed_chat_ids` | `[i64]` | `[]` | 即応答するグループの chat ID 一覧 |
| `channels.telegram.provider` | `string \| null` | `null` | チャネル別プロバイダーオーバーライド |
| `channels.telegram.model` | `string \| null` | `null` | チャネル別モデルオーバーライド |

### 制約

- 1 メッセージ 4096 文字（自動分割）
- キャプション 1024 文字
- グループでは許可リスト外の場合 `@mention` 必須
- チャネル（Channel）メッセージは無視
- チャネルプロバイダーが Telegram 未対応の場合あり（プロバイダー側の制限）

### Bot 作成手順

1. [@BotFather](https://t.me/BotFather) とチャットし `/newbot` を実行
2. 表示名とユーザー名（末尾 `bot` 必須）を設定
3. 発行されたトークンを `bot_token` に設定
4. ユーザー名（`@` なし）を `bot_username` に設定
5. グループに追加する場合は Bot のプライバシーモードを無効化（`/setprivacy` → Disable）

---

## 5. TUI

### 接続方式

Ratatui + crossterm。ターミナルの代替スクリーンモードで動作。

### 画面構成

- **Browser ビュー**: 全セッション一覧。`j/k` で移動、`Enter` で選択、`n` で新規セッション
- **Chat ビュー**: 選択したセッションの会話。メッセージ入力と履歴表示
- スラッシュコマンドは TUI 内で同期的に処理される

### データフロー

- **メッセージ受信**: TUI 入力フィールド + Enter
- **並列処理**: `tokio::spawn` で agent_loop を呼び出し、メインスレッドは 200ms 間隔でポーリング
- **応答**: チャットビューに直接表示

### 設定

TUI 固有の設定項目はない。グローバルの `default_agent` を使用する。

### 制約

- ファイル添付非対応
- シングルスレッドのイベントループ（描画は非同期不可）
- 同時に 1 つの送信のみ処理可能

---

## 6. CLI

### 接続方式

標準入出力。`egopulse chat [--session <name>]` で起動。

### データフロー

- **メッセージ受信**: stdin から行読み取り
- **応答**: stdout に直接出力
- `/exit` で終了

### 設定

CLI 固有の設定項目はない。グローバルの `default_agent` を使用する。

### 制約

- ブロッキング I/O（同期的に読み取り）
- ストリーミング非対応
- ファイル添付非対応

---

## 7. チャネル横断の設定オーバーライド

各チャネルはグローバル設定に対して以下をオーバーライドできる：

| 設定項目 | Web | Discord | Telegram |
|---------|:---:|:-------:|:--------:|
| プロバイダー | ○ | ○ | ○ |
| モデル | ○ | ○ | ○ |
| 人格 (SOUL.md) | ○ | ○ | ○ |

モデル解決の優先順位は以下の通り：

```text
agent.model（エージェント固有）
    ↓ null の場合
channel.model（チャネル固有）
    ↓ null の場合
config.default_model（グローバル）
    ↓ null の場合
provider.default_model（プロバイダーのデフォルト）
```

詳細は [config.md](./config.md) を参照。