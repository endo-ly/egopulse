# チャネル間仕様パリティ

Web / Discord / Telegram / TUI / CLI の各チャネルにおける機能対応状況と、
実装・設定・ドキュメント間のズレを整理する。

> **Updated**: Telegram Multi-Agent 同等化完了後の状態を反映。

---

## 2. 機能比較表

| 機能 | Discord | Telegram | Web | TUI | CLI |
|---|:---:|:---:|:---:|:---:|:---:|
| Multi-Agent Room | ○ | ○ | ✗ | ✗ | ✗ |
| `agent_send` ツール | ○ | ○ | ✗ | ✗ | ✗ |
| TurnScheduler 経由の実行 | ○ | ○ | ✗ | ✗ | ✗ |
| Channel Log（二層保存） | ○ | ○ | ✗ | ✗ | ✗ |
| `origin_id` 発行 | ○ | ○ | ✗ | ✗ | ✗ |
| 複数 Bot インスタンス | ○ | ○ | N/A | N/A | N/A |
| BotChainState（連鎖ガード） | ○ | ○ | N/A | N/A | N/A |
| `session_key` agent 分離 | ○ | ○ | ○ | ○ | ○ |
| 添付ファイル受信 | ○ | ○ | ✗ | ✗ | ✗ |
| Typing Indicator | ○ (trait) | ○ (独自) | ✗ | ✗ | ✗ |
| スラッシュコマンド | ○ | ○ | ○ | ○ | ○ |

---

## 3. 設定構造比較

### 3.1 チャンネル/チャットごとの設定

| フィールド | Discord (`DiscordChannelConfig`) | Telegram (`TelegramChatConfig`) |
|---|---|---|
| `require_mention` | ○ | ○ |
| `agents` | ○ (`list<string>`) | ○ (`list<string>`) |
| `multi_agent` | ○ (`bool`) | ○ (`bool`) |

### 3.2 Bot 定義

| フィールド | Discord (`bots` マップ) | Telegram (`bots` マップ) |
|---|---|---|
| Bot ID | ○ (`map<BotId, BotConfig>`) | ○ (`map<BotId, TelegramBotConfig>`) |
| トークン | ○ (`SecretRef` 対応) | ○ (`SecretRef` 対応) |
| ユーザー名 | ✗ (API から自動取得) | ○ (必須、API から取得不可) |
| 複数 Bot | ○ | ○ |

### 3.3 エージェントの Bot 紐付け

| フィールド | Discord | Telegram |
|---|---|---|
| `agents.<id>.discord_bot` | ○ | ○ (`agents.<id>.telegram_bot`) |

---

## 4. 実装アーキテクチャ比較

### 4.1 メッセージフロー

Discord / Telegram (共通):
```
Handler::message → route_message → should_process_message
  → (multi-agent: store Channel Log) → TurnScheduler.submit → execute_scheduled_turn
```

Web / TUI / CLI (共通):
```
handle_request → process_turn / process_turn_with_events → send_text
```

Web は `process_turn_with_events` を直接呼び出し、SSE/WS イベントストリームを提供する。
TurnScheduler を介すると `execute_scheduled_turn` → `process_turn` になりイベントが失われるため、
TurnScheduler は適用しない。

### 4.2 セッションキー形式

全チャネル統一: `channel:thread:agent:<agent_id>`

| チャネル | thread の内容 | 例 |
|---|---|---|
| Discord | チャンネル ID | `discord:123:agent:alice` |
| Telegram | chat ID (i64) | `telegram:-100123:agent:default` |
| Web | セッション ID | `web:s1:agent:default` |
| TUI | ローカルセッション | `tui:local-xxx:agent:default` |
| CLI | セッション名 | `cli:mysession:agent:default` |

### 4.3 Channel Log

Discord / Telegram (共通):
Multi-Agent Room では、チャット ID に `:multi-room-log` サフィックスを付けた
共有 Channel Log に全エージェントのメッセージを保存し、各エージェントセッションには
`agent_send` や system event のみを記録する二層アーキテクチャ。

### 4.4 `agent_send` のチャネル条件

`agent_send` は Discord / Telegram チャネルでのみ利用可能。
ランタイムガード: `matches!(channel, "discord" | "telegram")`

---

## 5. CLI / TUI / Web の特記事項

- **TurnScheduler 非使用**: single-user インターフェースであり、同時ターンが発生しないため不要
- **Web のストリーミング**: `process_turn_with_events` が SSE/WS イベントを提供。TurnScheduler を介するとイベントが失われる
- **`session_key` agent 分離済み**: 全チャネルで `channel:thread:agent:id` 形式を採用。将来の `/agent alice` 切替の基盤

---

## 変更履歴

### Phase 1: Telegram Multi-Agent 同等化 + 全チャネル session_key 統一

- Telegram: `bots` マップ + `channels` マップ構造
- Telegram: Handler ルーティング（Single/Multi-Agent、ObserveOnly）
- Telegram: TurnScheduler 経由のターン実行
- Telegram: Channel Log 二層保存
- Telegram: BotChainState 連鎖ガード
- Telegram: 複数 Bot インスタンス起動
- 全チャネル: `session_key` 統一 (`channel:thread:agent:id`)
- `agent_send`: Discord / Telegram 両対応
- DB マイグレーション: v2 で既存セッションキーに `:agent:default` を付与
