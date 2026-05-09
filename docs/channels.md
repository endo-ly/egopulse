# EgoPulse チャネル仕様

Web / Discord / Telegram / TUI / CLI の各チャネルの接続方式、メッセージフロー、制約を記述する。
設定フィールドの型・デフォルト値は [config.md](./config.md) を参照。

## 目次

1. [共通アーキテクチャ](#1-共通アーキテクチャ)
2. [Web](#2-web)
3. [Discord](#3-discord)
4. [Telegram](#4-telegram)
5. [TUI](#5-tui)
6. [CLI](#6-cli)

---

## 1. 共通アーキテクチャ

### ChannelAdapter trait

全チャネルは `ChannelAdapter` trait を実装し、`ChannelRegistry` に登録される。
エージェントループはチャネルを意識せず、`ChannelRegistry` を通じて応答を返送する。

```rust
#[async_trait]
pub(crate) trait ChannelAdapter: Send + Sync {
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

### チャネル横断の設定オーバーライド

各チャネルはグローバル設定に対して以下をオーバーライドできる：

| 設定項目 | Web | Discord | Telegram |
|---------|:---:|:-------:|:--------:|
| プロバイダー | ○ | ○ | ○ |
| モデル | ○ | ○ | ○ |
| 人格 (SOUL.md) | ○ | ○ | ○ |

モデル解決の優先順位は [config.md §3](./config.md#3-モデル解決チェーン) を参照。

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
  - DM → グローバル `default_agent`
  - Single-Agent チャネル（`multi_agent: false`）→ `channels[channel_id].agents[0]` に紐づく Bot のみ受信し、その先頭 Agent が応答。`agents[1..]` は人間入力の入口にはならない
  - Multi-Agent Room（`multi_agent: true`）→ @mention された Bot に紐づくエージェントを特定。mention なし → 応答なし（Channel Log のみ保存）
- **メッセージ振り分け**:
  - **ヒューマンメッセージ**: `require_mention` 設定に従う。受け入れられたヒューマンメッセージは該当チャンネル/スレッドのボットチェーン状態をリセットする
  - **他ボットからのメッセージ**: 自ボットへの明示的な @mention がある場合のみ受け入れる（`require_mention` 設定は適用されない）。ボット間チェーン深度が上限に達している場合は拒否される
  - **自ボットのメッセージ**: 常に無視される
- **ボット間チェーンガード**: チャンネル/スレッドごとにボット→ボットの連鎖を追跡。チェーン深度は内部上限（5）と TTL（300 秒）で管理され、ヒューマンメッセージの受信でリセットされる
- **応答**: 2000 文字制限に合わせて自動分割送信。本文中の明示的なユーザー mention (`<@user_id>`) だけを Discord の `allowed_mentions.users` に指定し、role / everyone mention は許可しない。429 (Rate Limit) 時は Retry-After で 3 回リトライ

### 制約

- 1 メッセージ 2000 文字（自動分割）
- HTTP タイムアウト 10 秒
- レート制限時は 3 回までリトライ
- `channels` マップに含まれるチャンネルのみ応答。マップが空の場合ギルドメッセージは全拒否（DM は常に許可）
- Single-Agent チャネルでは `agents[0].discord_bot` に紐づく Bot のみギルドメッセージを処理する。`agents[1..]` に紐づく Bot や別 Bot は、同じチャンネルに参加していても無視する
- **ヒューマンメッセージ**: `require_mention: true` のチャンネルでは自ボットへの @mention なしで応答しない。`require_mention` 設定はヒューマンメッセージにのみ適用される
- **ボットメッセージ**: 自ボットへの明示的な @mention がある場合のみ処理される。自ボット自身のメッセージは常に無視される
- **送信 mention**: Bot 応答に含まれる明示的なユーザー mention は Discord 側で mention として解釈される。role / everyone mention は送信時に許可しない
- **ボット間チェーンガード**: チャンネル/スレッドごとにボット→ボットの連鎖を追跡し、内部深度上限（5）または TTL（300 秒）で自動停止する。受け入れられたヒューマンメッセージは該当チャンネル/スレッドのチェーン状態をリセットする
- Slash command は `channels` に含まれるチャンネルであれば常に有効（mention やチェーンガードの対象外）
- **既知の挙動**: 同一 Bot に複数 Agent が紐づく Multi-Agent Room では、mention だけでは Agent を一意に決定できないため、該当 Bot に紐づく候補のうち `agents` 順で最初の Agent が応答する

### Multi-Agent Room (Phase 2)

`multi_agent: true` の Discord チャネルは、複数エージェントが同居するルームとして動作する。

#### メンションベースのエージェント解決

```text
ユーザーメッセージ受信
  ├─ mention あり → 対応するエージェントを特定
  │   ├─ Channel Log に保存 + Agent Session に保存
  │   └─ process_turn() 実行（Channel Context 注入あり）
  └─ mention なし → Channel Log にのみ保存 → 応答なし
```

#### Channel Context

Agent Session の LLM 呼び出し時に、Channel Log の直近 30 件を一時的に注入。エージェントはルーム全体の会話を背景情報として認識するが、実際に応答するのは `<direct-input>` でラップされたユーザー入力に対してのみ。

#### メッセージ保存ルール

| シナリオ | Channel Log | Agent Session |
|---|---|---|
| mention されたエージェント | 保存 | 保存（Channel Context 注入） |
| mention なしのヒューマンメッセージ | 保存のみ | — |
| ボット応答 | 保存 | 保存 |
| Single-Agent チャネル | — | 保存（従来通り） |

### Bot 作成手順

1. [Discord Developer Portal](https://discord.com/developers/applications) でアプリケーション作成
2. Bot ページでトークンを生成
3. Privileged Gateway Intents で `MESSAGE CONTENT INTENT` を有効化
4. OAuth2 ページを開き、URL Generator で以下を設定：
   - **Scopes**: `bot` にチェック
   - **Bot Permissions**: `Send Messages`, `Read Message History`, `Attach Files` にチェック
5. 生成された URL をブラウザで開き、招待先サーバーを選択して認証

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
  - `chats` マップに含まれるチャットのみ応答
  - `require_mention: false`（デフォルト）= 即応答
  - `require_mention: true` = @mention 必須
- **応答**: 4096 文字制限に合わせて自動分割送信


### 制約

- 1 メッセージ 4096 文字（自動分割）
- キャプション 1024 文字
- グループでは `chats` マップ外のチャットに応答しない
- `require_mention: true` のチャットでは @mention なしで応答しない
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

### 制約

- ブロッキング I/O（同期的に読み取り）
- ストリーミング非対応
- ファイル添付非対応
