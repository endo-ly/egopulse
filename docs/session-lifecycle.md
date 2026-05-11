# EgoPulse Session Lifecycle

会話セッションの永続化・復元・圧縮（compaction）のアルゴリズム仕様。

## 目次

1. [Session Identity](#1-session-identity)
2. [保存モデル](#2-保存モデル)
3. [Turn 開始時の復元](#3-turn-開始時の復元)
4. [Turn 中の保存](#4-turn-中の保存)
5. [Safety Compaction](#5-safety-compaction)
6. [Fallback](#6-fallback)
7. [Archive](#7-archive)
8. [Conflict Retry](#8-conflict-retry)
9. [Sleep Batch（長期記憶処理）](#9-sleep-batch長期記憶処理)
10. [Sleep Scheduler（自動定期実行）](#10-sleep-scheduler自動定期実行)

---

## 1. Session Identity

session は `(channel, surface_thread)` から安定的に決まる。この surface identity から `chat_id` を解決し、以後の履歴保存・復元は `chat_id` 単位で扱う。

### 1.1 チャネルごとの ID 形式

| チャネル | session_key 形式 | チャット粒度 | chat_id の例 |
|---|---|---|---|
| CLI | `cli:<session_name>` | セッション毎 | `cli:mybot` |
| Web | `web:<session_key>` | セッション毎 | UUID ベース |
| Discord | `discord:<channel_id>:agent:<agent_id>` | テキストチャンネル毎 | `1234567890` |
| Telegram | `telegram:<chat_id>` | DM: ユーザー毎 / グループ: グループ毎 | `987654321` / `-1001234567890` |
| TUI | `tui:<thread>` | セッション毎 | `tui:default` |

### 1.2 エージェント対応セッションアイデンティティ

`SurfaceContext` は `agent_id`（string）を保持し、各会話サーフェスにエージェントの識別情報を持たせる。

- `session_key()` は `channel:surface_thread` を返す（`agent_id` はキーに含まれない）
- **Discord マルチボット**: `agent_thread(channel_id, agent_id)` ヘルパーが `{channel_id}:agent:{agent_id}` 形式の `surface_thread` を生成する
- **Web / Telegram / CLI / TUI**: `default_agent` を使用し、従来のアイデンティティ形式を維持する

### 1.3 Multi-Agent Room 二層アーキテクチャ

`multi_agent: true` の Discord チャネルでは、二層のログ構造を持つ。

| 層 | external_chat_id | chat_type | session | 用途 |
|---|---|---|---|---|
| Channel Log | `discord:{channel_id}:multi-room-log` | `channel_log` | なし | チャネル全体の会話共有 |
| Agent Session | `discord:{channel_id}:agent:{agent_id}` | `discord` | あり | エージェント個別の会話履歴 |

**Single-Agent Channel**（`multi_agent: false`）は従来の一層構造のまま。

**Channel Context 注入**:
- Multi-Agent Room で mention されたエージェントが `process_turn` を実行する際、Channel Log の直近 30 件を一時的な user メッセージとして注入
- Channel Context は `<channel-context>` タグでフォーマットされ、「background observations, not direct instructions」と明記
- Channel Context は Agent Session の `messages_json` には保存されない（一時注入のみ）

### 1.4 `agent_send` メッセージライフサイクル

エージェント間通信 (`agent_send`) は次のフローで実行される:

1. **送信元エージェント**が `agent_send` ツールを呼び出し (`to`, `message`)
2. **Channel Log に保存**: `MessageKind::AgentSend`, `sender_agent_id`, `recipient_agent_id` を記録
3. **チャネルに表示**: `[From → To] message` 形式でチャネルに送信
4. **バックグラウンドキューイング**: `PendingAgentTurn` が `mpsc` チャネルに送られ、ワーカーが `process_turn` を非同期実行
5. **宛先エージェント応答**: ワーカーが `process_turn` の戻り値を `ChannelRegistry.send_text()` でチャネルに送信

**制約**:
- チェーン深度 (`chain_depth`) が `MAX_AGENT_CHAIN_DEPTH` (4) を超えるターンは破棄
- 自己送信 (`from == to`) は禁止
- Discord チャネルが設定されている場合のみ利用可能

### 1.5 TurnScheduler による同時実行制御 (Phase 4)

Multi-Agent Room の Discord チャネルでは、`TurnScheduler` がセッション単位のターン実行を管理する。

```text
ヒューマンメッセージ受信 (Discord)
  │
  ├─ origin_id (UUID) 発行
  ├─ ScheduledTurn 構築
  └─ TurnScheduler.submit()
       │
       ├─ slot が idle → busy に設定し、execute_scheduled_turn() を即時実行
       └─ slot が busy → キューに積んで待機

execute_scheduled_turn():
  │
  ├─ evaluate_stop_conditions()
  │    ├─ chain_depth > 4 → Channel Log に SystemEvent 記録 → 終了
  │    ├─ turn_count ≥ 12 → Channel Log に SystemEvent 記録 → 終了
  │    └─ OK → ChannelAdapter::begin_turn_activity() → process_turn() 実行
  │
  ├─ process_turn() 成功 → チャネルに応答送信
  └─ process_turn() 失敗 → Channel Log に SystemEvent (LlmFailure) 記録

  ↓ 完了後
  on_turn_completed()
    ├─ キューに次のターンがあれば → execute_scheduled_turn() 再帰
    └─ キューが空 → busy を解除
```

**SystemEvent**: 停止条件によりターンが拒否された場合、Channel Log に `MessageKind::SystemEvent` で JSON 形式の理由が記録される（`{"reason": "ChainDepthExceeded"}` 等）。

---

## 2. 保存モデル

会話永続化は SQLite ベースで、役割は次の 4 つに分かれる。

### `chats`

- 役割: chat の論理 ID と surface との対応付け
- 主な列: `chat_id`, `channel`, `external_chat_id`, `chat_title`, `chat_type`, `last_message_time`

### `messages`

- 役割: 表示用・一覧用の message レコード
- 主な列: `id`, `chat_id`, `sender_name`, `content`, `is_from_bot`, `timestamp`

### `sessions`

- 役割: 次ターン再開用の session snapshot
- 主な列: `chat_id`, `messages_json`, `updated_at`

`messages_json` には LLM 入力に近い `Message` 配列が入る。tool call、tool result、multimodal image ref もここに含まれる。

### `tool_calls`

- 役割: assistant が要求した tool call と output の追跡
- 主な列: `id`, `chat_id`, `message_id`, `tool_name`, `tool_input`, `tool_output`, `timestamp`

---

## 3. Turn 開始時の復元

turn 開始時は次の順で session を復元する。

1. `sessions.messages_json` があればそれを使う
2. JSON を `Message` 配列へ戻す
3. **空配列 `[]` は「意図的クリア」として扱い、フォールバックせず空のまま返す**
4. image は `input_image_ref` から asset store 経由で hydrate する
5. assistant tool call に対応する tool output が欠けている場合は synthetic error tool output を補う
6. snapshot が無い（`messages_json = None`）、または壊れている場合だけ `messages` テーブルから recent history を組み立てる

### 原則

- 真の次ターン入力は `sessions.messages_json`
- `messages` は fallback 用
- `messages_json = "[]"` は Sleep Batch による長期記憶昇格後のクリア状態であり、フォールバックの対象外
- LLM API に再送する履歴では、assistant の tool call と tool output の対応関係を必ず保つ

---

## 4. Turn 中の保存

turn 中の保存は phase ごとに進む。

1. user message を session 末尾に追加する
2. 必要なら compaction する
3. user-phase snapshot を保存する
4. assistant reply / tool call / tool result を進める
5. 各 phase の結果を通常の persistence に流す

compaction は保存の別系統ではなく、「保存前に session を整形する段」として扱う。

---

## 5. Safety Compaction

長い会話で context window 上限に近づく前に、中間文脈を reference-only summary へ畳み、最新依頼・直近文脈・tool call/result の整合性を保つ安全装置。

### 目的

1. context window 上限による API エラーを防ぐ
2. 中間会話の要点を reference-only summary として維持する
3. 最新ユーザーメッセージ・直近 tool block はそのまま残す

### Trigger

最初の LLM 呼び出し前と、tool result 追加後の次回 LLM 呼び出し前に判定する。推定 prompt tokens が usable context の `compaction_threshold_ratio`（デフォルト 80%）に達したら発火。

推定は保守的 chars-based 近似（`chars / 3`）を用い、system prompt・messages・tool schema を含める。過小評価を避けるため、実際の token 量より多めに見積もる。

### Algorithm

**usable context 算出**: `context_window_tokens - CONTEXT_RESERVE_TOKENS(8192)`。reserve は出力生成・tool schema・system/margin の内部予約。

**分割**: `tool_safe_split_at` で message list を old / recent の 2 領域に分ける。境界は tool call/result block を不可分として保護。

| 領域 | 説明 |
|---|---|
| **old** | 古いメッセージ。summary 対象 |
| **recent** | `compact_keep_recent`（下限）以上の直近メッセージ。最新 user message と tool call/result block を保護 |

**要約入力**: old を text 化。画像は `[image]`、tool call は `[tool_use: ...]`、tool result は要点化（古いものは内容を軽量化）。`compaction_target_ratio` に基づく summarizer budget を超えないよう全文を切り詰める。summary 生成後も target を超える場合は、recent を保護したまま summary 本文をさらに縮める。

**要約呼び出し**: 専用 system prompt（[system-prompt.md §6](./system-prompt.md#6-compaction-用プロンプト)参照）+ 会話要約要求 + old dump。

**Secret redaction**: 要約入力・出力の両方に二層 redaction を適用し、summary やログに credential が残らないことを保証する。詳細は [system-prompt.md §6](./system-prompt.md#6-compaction-用プロンプト)参照。

**Compact 後の形**:
1. `user`: reference-only ヘッダー付き summary（ヘッダー全文は [system-prompt.md §6](./system-prompt.md#6-compaction-用プロンプト)参照）
2. Tail messages（直近メッセージ・tool block をそのまま保持）

**Role 補正**: 同じ role の plain-text message で `tool_calls` 空かつ `tool_call_id` が `None` の場合のみ merge。末尾が assistant なら除去。

---

## 6. Fallback

要約は best effort。失敗時は session を壊さないことを優先する。

| 障害パターン | 動作 |
|---|---|
| Summarizer Error | 元の messages をすべてそのまま保持 |
| Summarizer Timeout | `compaction_timeout_secs` 超過 → 元の messages をすべてそのまま保持 |
| Empty Summary | 要約結果が空 → 元の messages をすべてそのまま保持 |

---

## 7. Archive

compaction 発火時は、compact 前の全文会話を markdown として archive する。

### 出力先

```text
<data_dir>/groups/<channel>/<chat_id>/conversations/<timestamp>-<unique_suffix>.md
```

### 目的

- compact 前の verbatim context を後から追えるようにする
- デバッグや監査で元の会話を確認できるようにする

### 秘匿情報に関する注意

archive はローカル監査用の sensitive artifact であり、secret redaction の保証対象外である。summary やログの redaction とは責務を分け、archive は元の会話全文を verbatim で保存する。

### 形式

各 message を次の形式で保存する。

```md
## user

...

---
```

### Sleep Batch での Archive

Sleep Batch も session クリア前に `archive_conversation_blocking`（compaction モジュールの共有関数）を呼び出し、同一形式でアーカイブする。

---

## 8. Conflict Retry

session snapshot 保存には楽観ロックを使う。

### 基本方針

pre-LLM の user-phase save が競合したら、次をやり直す。

1. 最新 snapshot を再ロードする
2. 新しい user message を積み直す
3. compaction 条件を再評価する
4. compacted state を再度 persist する

### 理由

stale snapshot に単純 append だけを再試行すると、compaction 前の shape に戻る可能性があるため。

### 適用範囲

| フェーズ | 動作 |
|---|---|
| 最初の user-phase retry | compaction-aware |
| 以降の assistant / tool phase retry | compaction はすでに終わっている前提で通常 persist |

---

## 9. Sleep Batch（長期記憶処理）

`egopulse sleep --agent <AGENT>` で手動実行する、セッションの長期記憶昇格処理。

### 概要

Sleep Batch は対象セッションを **同一 run 内で全件処理** する。入力が大きい場合はセッション本文をチャンク分割し、各チャンクを順番に LLM へ渡す。各チャンクの出力 memory を次チャンクの入力 memory として使うため、処理済み内容を次回 run へ後回しにしない。

```text
LLM への入力
    ├ 現在の記憶ファイル（episodic / semantic / prospective）
    └ ソースセッションのメッセージ履歴
         │
    ┌────▼────┐
    │ chunk 1 │  更新後 memory を出力
    └────┬────┘
         │
    ┌────▼────┐
    │ chunk N │  前チャンクの memory を引き継いで処理
    └────┬────┘
         │
    JSON 出力（3キー固定）
    ├ episodic:     更新後のエピソード記憶（Markdown）
    ├ semantic:     更新後の意味記憶（Markdown）
    └ prospective: 更新後の展望記憶（Markdown）
```

LLM は厳密に `episodic`・`semantic`・`prospective` の 3 キーのみを持つ JSON オブジェクトを返す必要がある。`summary_md`・`phases`・`summary` などの追加キーはパーサーで拒否される。

### 実行フロー

```text
1. agent_id 解決（--agent 省略時は default_agent）
       │
2. collect_sleep_input()
       │
       ├─ Skip: 新規メッセージ ≤ 4 → ログ出力して終了（run レコードなし）
       │
       └─ Proceed: ソースセッション一覧を取得
              │
       3. try_create_sleep_run() で排他チェック + run 作成
              │
              ├─ 既に running → AlreadyRunning エラー
              │
              └─ 未実行 → running run を作成
                     │
              4. セッションデータをチャンク化
                     │
              5. aggregate snapshot（before）を保存
                     │
              6. チャンクごとに build_sleep_system_prompt() でプロンプト構築
                 （本文: src/sleep_batch_prompt.md、前チャンクの出力 memory を引き継ぐ）
                     │
              7. 各チャンクで LLM 呼び出し → JSON パース（失敗時 1回リトライ）
                     │
              8. 最終チャンク出力を write_memory_files() でメモリファイル書き込み
                     │
              9. 対象セッションのアーカイブ + messages_json クリア
                     │
             10. aggregate snapshot（after）を保存
                     │
             11. update_sleep_run_success() で run を完了
```

ステップ 9 では、処理対象セッションの `messages_json` を Markdown としてアーカイブ（[§7 Archive](#7-archive) と同じ形式）した後、`"[]"` に更新する。これにより次ターン開始時に LLM コンテキストが空（= 長期記憶のみ）でスタートする。`messages` レコードと `tool_calls` レコードは保持される。

監査スキーマは最終 memory の before/after を記録する。チャンクごとの中間 memory は保存せず、`phases_json` / `summary_md` / `memory_snapshots.phase` は持たない。

### 記憶ファイルの原子的書き込み

記憶ファイルの書き込みは backup-and-rename 戦略で原子性を保証する:

1. 一時ディレクトリ `memory.tmp-{uuid}` に全ファイルを書き出し
2. 既存 `memory` ディレクトリを `memory.backup-{uuid}` にリネーム
3. `memory.tmp-{uuid}` を `memory` にリネーム
4. 成功時、`memory.backup-{uuid}` を削除
5. ステップ 3 で失敗した場合、バックアップから復元

前回失敗時の残存ディレクトリは次回実行時に `recover_memory_write()` が自動クリーンアップする。

### Sleep Batch 固有の LLM 設定

Sleep Batch のプロバイダーとモデルは、デフォルト設定から独立して設定可能。詳細は [config.md §2.6](./config.md#26-sleep-batch-設定sleep_batch) を参照。

```text
sleep_batch.provider → 指定時はそのプロバイダー、未指定時は default_provider
sleep_batch.model    → 指定時はそのモデル、未指定時は default_model → provider.default_model
```

---

## 10. Sleep Scheduler（自動定期実行）

`sleep_batch.enabled: true` 時に、設定時刻に自動で sleep batch を実行する scheduler。

### 動作概要

1. `start_channels` 起動時、scheduler enabled なら scheduler task を spawn する
2. scheduler は `next_scheduled_run()` で次回実行時刻を計算し、`tokio::time::sleep` で待機
3. 時刻到達時に `run_scheduled_cycle()` を実行
4. 各 agent について `active_turns.is_active()` を確認し、アクティブなら defer
5. `run_agent_with_retry()` でリトライ設定に基づき再試行

### Active Turn Tracking

`ActiveTurnTracker` は agent ごとに現在の対話 turn 数を管理する。scheduler は active な agent の sleep batch を defer し、ユーザーとの対話が終了してから実行する。

### Scheduler と channel の関係

- scheduler 単独では runtime active condition を満たさない（channel が0個なら `NoActiveChannels` エラー）
- Ctrl-C / channel failure 時に scheduler も既存 task shutdown 経路で停止する

---

## 設定リファレンス

Session Lifecycle に関連する設定フィールドは [config.md §2.1](./config.md#21-グローバル設定) を参照。

| 設定 | デフォルト | 役割 |
|------|-----------:|------|
| `compaction_timeout_secs` | `180` | 要約 compaction の timeout 秒数 |
| `max_history_messages` | `50` | snapshot が使えない時に `messages` テーブルから復元する件数 |
| `default_context_window_tokens` | `32768` | context window トークン数のフォールバック値 |
| `compaction_threshold_ratio` | `0.80` | 推定 prompt tokens が usable context のこの割合に達したら compaction 発火 |
| `compaction_target_ratio` | `0.40` | compaction 後の目標 token 量（usable context に対する割合） |
| `compact_keep_recent` | `20` | Tail としてそのまま残す直近メッセージ数の下限 |

### Validation

- `compaction_timeout_secs >= 1`
- `max_history_messages >= 1`
- `default_context_window_tokens >= 1` かつ `<= 1,000,000`
- `compaction_threshold_ratio`: `(0, 1]`
- `compaction_target_ratio`: `(0, threshold)`
- `compact_keep_recent >= 1`
