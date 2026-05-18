# チャネル間仕様パリティ

Web / Discord / Telegram / TUI / CLI の各チャネルにおける機能対応状況と、
実装・設定・ドキュメント間のズレを整理する。

## 目次

1. [概要](#1-概要)
2. [機能比較表](#2-機能比較表)
3. [設定構造比較](#3-設定構造比較)
4. [実装アーキテクチャ比較](#4-実装アーキテクチャ比較)
5. [ズレの詳細分析](#5-ズレの詳細分析)
6. [改善フェーズ](#6-改善フェーズ)

---

## 1. 概要

EgoPulse は 5 つのチャネル（Web / Discord / Telegram / TUI / CLI）を持つが、
Discord が最も機能が充実しており、Multi-Agent Room や `agent_send` などの高度な機能は
Discord のみに実装されている。他チャネルは single-agent・直接 `process_turn` 呼び出しの
シンプルな構造にとどまっている。

本ドキュメントは、チャネル間の仕様ズレを可視化し、統合に向けた改善フェーズの基準とする。

---

## 2. 機能比較表

| 機能 | Discord | Telegram | Web | TUI | CLI |
|---|:---:|:---:|:---:|:---:|:---:|
| Multi-Agent Room | ○ | ✗ | ✗ | ✗ | ✗ |
| `agent_send` ツール | ○ | ✗ | ✗ | ✗ | ✗ |
| TurnScheduler 経由の実行 | ○ | ✗ | ✗ | ✗ | ✗ |
| Channel Log（二層保存） | ○ | ✗ | ✗ | ✗ | ✗ |
| `origin_id` 発行 | ○ | ✗ | ✗ | ✗ | ✗ |
| 複数 Bot インスタンス | ○ | ✗ | N/A | N/A | N/A |
| BotChainState（連鎖ガード） | ○ | ✗ | N/A | N/A | N/A |
| `session_key` agent 分離 | ○ | ✗ | ✗ | ✗ | ✗ |
| 添付ファイル受信 | ○ | ○ | ✗ | ✗ | ✗ |
| Typing Indicator | ○ (trait) | ○ (独自) | ✗ | ✗ | ✗ |
| スラッシュコマンド | ○ | ○ | ○ | ○ | ○ |

---

## 3. 設定構造比較

### 3.1 チャンネル/チャットごとの設定

| フィールド | Discord (`DiscordChannelConfig`) | Telegram (`TelegramChatConfig`) |
|---|---|---|
| `require_mention` | ○ | ○ |
| `agents` | ○ (`list<string>`) | ✗ |
| `multi_agent` | ○ (`bool`) | ✗ |

Telegram は `require_mention` のみ。エージェントはグローバル `default_agent` に固定。

### 3.2 Bot 定義

| フィールド | Discord (`bots` マップ) | Telegram (単一) |
|---|---|---|
| Bot ID | ○ (`map<BotId, BotConfig>`) | ✗ |
| トークン | ○ (`SecretRef` 対応) | ○ (`SecretRef` 対応) |
| 複数 Bot | ○ | ✗ |

### 3.3 エージェントの Bot 紐付け

| フィールド | Discord | Telegram |
|---|---|---|
| `agents.<id>.discord_bot` | ○ | ✗（対応フィールドなし） |

---

## 4. 実装アーキテクチャ比較

### 4.1 ターン実行パス

```text
Discord:  Handler::message → route_message → TurnScheduler.submit → execute_scheduled_turn
Telegram: handle_message → process_turn（直接）
Web:      HTTP/WS handler → process_turn_with_events（直接）
TUI:      key Enter → send_turn（直接）
CLI:      stdin line → process_turn（直接）
```

Discord だけ TurnScheduler を経由する。他チャネルは `process_turn` / `send_turn` を直接呼び出す。

### 4.2 session_key の生成

```rust
// SurfaceContext::session_key()
if self.channel == "discord" && !self.agent_id.is_empty() {
    format!("{}:{}:agent:{}", self.channel, self.surface_thread, self.agent_id)
} else {
    format!("{}:{}", self.channel, self.surface_thread)
}
```

Discord のみ agent_id でセッションを分離。他チャネルは agent_id が異なっても同じセッションキーになる。

### 4.3 Channel Log

Discord の Multi-Agent Room では、チャット ID に `:multi-room-log` サフィックスを付けた
共有 Channel Log に全エージェントのメッセージを保存し、各エージェントセッションには
`<direct-input>` として注入する。他チャネルにこの仕組みはない。

### 4.4 Typing Indicator

| チャネル | 実装方法 |
|---|---|
| Discord | `ChannelAdapter::begin_turn_activity()` → `TurnActivity` trait |
| Telegram | handler 内で `tokio::spawn` による独自タイマー |

Discord は trait レベルで抽象化されているが、Telegram は handler 内にベタ書き。
`TelegramAdapter` は `begin_turn_activity()` をオーバーライドしていない（デフォルトの no-op）。

---

## 5. ズレの詳細分析

### 5.1 Telegram → Discord 同等化に必要な変更（大）

| 項目 | 現状 | 必要な変更 |
|---|---|---|
| 複数 Bot | 単一 `bot_token` | `bots` マップ + `BotId` |
| チャットごとエージェント | `default_agent` 固定 | `TelegramChatConfig` に `agents` / `multi_agent` 追加 |
| エージェントの Bot 紐付け | なし | `telegram_bot` フィールド or `discord_bot` の一般化 |
| `session_key` | `telegram:thread` | `telegram:thread:agent:id` |
| ターン実行 | `process_turn` 直呼び | `TurnScheduler.submit` 経由 |
| Channel Log | なし | 二層保存アーキテクチャ |
| `origin_id` | 空 | UUID 発行 |
| BotChainState | なし | 連鎖ガード実装 |
| `agent_send` ガード | Discord ハードコード | チャネル非依存に変更 |
| Handler ルーティング | なし | Single/Multi-Agent + ObserveOnly |
| Typing Indicator | handler 内ベタ書き | `begin_turn_activity()` trait 実装 |

### 5.2 `session_key` のハードコード（中）

`self.channel == "discord"` で条件分岐しているため、Telegram 対応時にもハードコード追加が必要。
Multi-Agent 対応チャネルかどうかを設定から判定するか、常に agent 分離するかの設計判断が必要。

### 5.3 Web / TUI / CLI のズレ（小〜中）

| 項目 | 影響 |
|---|---|
| `session_key` agent 分離なし | Multi-Agent 環境で Web/TUI が同じエージェントを使う限り問題なし。将来エージェント切替を追加する場合は対応が必要 |
| TurnScheduler 非使用 | Multi-Agent 環境で TUI/Web から同時アクセスした場合、同一エージェントセッションの直列化がない。single-agent 運用では実質問題なし |
| Typing Indicator 未対応 | TUI/CLI はローカルのため不要。Web はストリーミングで代替 |

---

## 6. 改善フェーズ

### Phase A: Telegram → Discord 同等化

Telegram を Discord と同一の Multi-Agent 仕様に引き上げる。

**設定**:
- `TelegramChatConfig` に `agents` / `multi_agent` 追加
- `bots` マップによる複数 Bot 定義
- エージェントの Bot 紐付け

**実装**:
- Handler ルーティング（Single/Multi-Agent、ObserveOnly）
- TurnScheduler 経由のターン実行
- Channel Log 二層保存
- `origin_id` 発行
- BotChainState
- `agent_send` の Discord ガード解除
- `session_key` の agent 分離
- Typing Indicator の trait 実装

**Docs**:
- `config.md`: Telegram 設定フィールド更新
- `channels.md`: Telegram 仕様更新
- `tools.md`: `agent_send` のチャネル条件更新

### Phase B: `session_key` 一般化

Discord のハードコード (`self.channel == "discord"`) を排除し、
Multi-Agent 対応チャネル全般で agent 分離された `session_key` を生成する仕組みにする。

### Phase C: Web / TUI / CLI の仕様統合

Web / TUI / CLI を TurnScheduler 経由に移行し、
将来的なエージェント切替や Multi-Agent 対応の基盤を整備する。
