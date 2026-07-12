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
8. [Conversation Scope による DB Routing](#8-conversation-scope-による-db-routing)
9. [Conflict Retry](#9-conflict-retry)
10. [Durable Turn State](#10-durable-turn-state)

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
       ├─ slot が idle → busy に設定し、execute_scheduled_turn() を即時実行 (Started)
       ├─ slot が busy かつ容量内 → キューに積んで待機 (Queued)
       └─ 容量超過 → 拒否 (Rejected)

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
    └─ キューが空 → slot を削除
```

**バックプレッシャー (Phase 1)**: `TurnScheduler` のキューは有限容量を持つ。セッション単位で 32 turn、Runtime 全体で 512 turn までキュー可能。超過時は `submit` が `Rejected(SessionQueueFull | GlobalQueueFull)` を返し、ターンは実行されない。拒否は呼出元・構造化ログ・metric で観測可能で、silent drop しない。

- Webhook: queue full 時は `429 Too Many Requests`（`session_queue_full` / `global_queue_full`）を返し、`202` にはならない。`202` は「in-memory scheduler への受付成功（即時開始 or キュー投入）」のみを意味し、永続化は保証しない。
- Discord / Telegram: 即時応答可能なため、拒否時にユーザーへ busy 通知を送る。
- Agent Send (`agent_send`): 非同期のため、拒否時に Channel Log へ SystemEvent を記録する。

**TurnTracker の有界化 (Phase 1)**: origin ごとの turn count / terminal reason を 1 つの state に統合し、24 時間の TTL で完了済み origin を破棄する。origin 追跡上限は 4096。TTL prunes 後も上限なら新規 origin を明示的に拒否する（既存 origin の更新は上限中も可能）。Phase 3 の Supervisor で正確な origin lifecycle を所有するまでは暫定的な有界化。

**SystemEvent**: 停止条件によりターンが拒否された場合、Channel Log に `MessageKind::SystemEvent` で JSON 形式の理由が記録される（`{"reason": "chain_depth_exceeded"}` 等）。

---

## 2. 保存モデル

会話永続化は SQLite ベースで、役割は次の 4 つに分かれる。

### `chats`

- 役割: chat の論理 ID と surface との対応付け
- 主な列: `chat_id`, `channel`, `external_chat_id`, `chat_title`, `chat_type`, `last_message_time`

### `messages`

- 役割: 表示用・一覧用の message レコード（**append-only**）
- 主な列: `id`, `chat_id`, `sender_name`, `content`, `is_from_bot`, `timestamp`
- `/new` や compaction では削除されない。セッションクリアは `sessions.messages_json` のリセットのみ行う

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

### Turn failure と安全な再試行

Runtime は、保存や Tool 実行を含む **Turn 全体** を自動で再実行しない。ただし、同一 model iteration 内で **外部出力が一切発生していない** 場合に限り、安全な範囲で LLM 再試行を行う。

**再試行条件**（すべて満たす場合のみ）:

- 同一 Turn・同一 iteration である
- `model_request_hash` が一致する（同じ request である）
- assistant delta・narration・Tool Call を外部へ公開していない
- assistant message を commit していない
- Tool 実行を開始していない
- error が明示的に retryable（429 / 5xx / 接続失敗 / 読取り timeout）
- 試行回数が上限（3 回）以内

**再試行禁止**（いずれか該当で `failed` または `uncertain` へ遷移）:

- partial output を外部へ公開済み（`output_published = true`）
- Tool Call を受信・保存済み
- Tool 実行開始済み
- response event または assistant message を commit 済み
- request hash が一致しない
- error の retryability が不明

再試行回数上限に達した場合、または再試行禁止条件に該当した場合は Turn を `failed`（出力未公開）または `uncertain`（出力公開済み）として終了する。Codex 認証の 401 では次回 Turn 向けに token refresh を試みるが、失敗した Turn を再開しない。

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

推定は会話量の概算を出し、LLM レスポンスに含まれる実測 usage で補正する。実測が返る provider では、日本語や tool schema の影響で概算が小さく出る場合も、以後の判定が実際の使用量に近づく。

まだ実測がない場合（未知の provider/model の初回ターン等）は、保守係数 `DEFAULT_FACTOR`（1.3）を適用して少し多めに見積もる。補正値は `llm_usage_logs` に観測（生推定値 `estimated_tokens` と実測 `input_tokens` のペア）として永続化し、再起動時に最近 N 件から EMA で再構築する。これにより再起動後も学習済みの補正状態が維持され、コールドスタート時の過大 DEFAULT による誤発火を防ぐ。外部 tokenizer は使わない。

### Algorithm

**usable context 算出**: `context_window_tokens - CONTEXT_RESERVE_TOKENS(8192)`。reserve は出力生成・tool schema・system/margin の内部予約。

**分割**: `tool_safe_split_at` で message list を old / recent の 2 領域に分ける。境界は tool call/result block を不可分として保護。

| 領域 | 説明 |
|---|---|
| **old** | 古いメッセージ。summary 対象 |
| **recent** | `compact_keep_recent`（下限）以上の直近メッセージ。最新 user message と tool call/result block を保護 |

**要約入力**: old を text 化。画像は `[image]`、tool call は `[tool_use: ...]`、tool result は要点化（古いものは内容を軽量化）。`compaction_target_ratio` に基づく summarizer budget を超えないよう全文を切り詰める。summary 生成後も目標サイズを超える場合は、recent を保護したまま summary 本文だけをさらに縮める。

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
<state_root>/runtime/groups/<channel>/<chat_id>/conversations/<timestamp>-<unique_suffix>.md
```

（`state_root` は通常 `~/.egopulse`。`runtime/groups/` 配下に配置される）

### 目的

- compact 前の verbatim context を後から追えるようにする
- デバッグや監査で元の会話を確認できるようにする

### 秘匿情報に関する注意

archive はローカル監査用の sensitive artifact であるが、**secret redaction を適用する**。
要約入力・出力と同様に、起動時に Config から収集したシークレット値（API キー、auth トークン等）と well-known パターン（`sk-`, `ghp_` 等）の二層リダクションを archive 全文に適用し、`[REDACTED:key]` / `[REDACTED:secret]` に置換する。

さらに、archive ファイルのパーミッションは `0600`（owner 読み書きのみ）に設定される。

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

## 8. Conversation Scope による DB Routing

`SurfaceContext.scope`（`ConversationScope::Normal` | `ConversationScope::Secret`）が turn 全体のストレージ境界を決定する。スコープはコンテキスト構築時にチャネル設定の `secret: true` から `ConversationScope::Secret` へとマッピングされ、turn 中の全永続化操作が同じスコープの DB にルーティングされる（[architecture.md §7.1](./architecture.md#71-conversationscopeストレージ境界) 参照）。

| 操作 | ルーティング |
|---|---|
| chat_id 解決（`resolve_or_create_chat_id`） | `state.db_for(ctx.scope)` |
| session snapshot 読込（`load_session` / `load_session_snapshot`） | `state.db_for(ctx.scope)` |
| message 保存（`store_message` / `store_message_with_session`） | `state.db_for(ctx.scope)` |
| session snapshot 保存（`save_session`） | `state.db_for(ctx.scope)` |
| LLM usage log（`log_llm_usage`） | `state.db_for(ctx.scope)` |
| compaction 中の LLM usage log | `state.db_for(ctx.scope)` |
| slash command handlers（`/new`, `/compact`, `/status`） | `state.db_for(context.scope)` |

### tool_call 永続化のスキップ

秘密モードでは `store_pending_tool_call` / `update_tool_call_output` をスキップする。`secret.db` に `tool_calls` テーブルが存在しないため。tool call block は `sessions.messages_json` に包含されており、LLM context 復元には影響しない。

### Compaction Archive の出力先分離

`TurnRuntime::storage_for(scope)` で解決される archive root に従い、Secret スコープの compaction アーカイブは `runtime/secret_groups/` に出力される。Normal スコープは `runtime/groups/` のまま。

```text
Normal: <state_root>/runtime/groups/<channel>/<chat_id>/conversations/
Secret: <state_root>/runtime/secret_groups/<channel>/<chat_id>/conversations/
```

`runtime/groups/` 配下はデバッグ・監査用の artifact でトラブルシュート時に共有されることが想定される。秘匿内容のアーカイブが混入するリスクを防ぐため、ディレクトリを分離する。

---

## 9. Conflict Retry

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

## 10. Durable Turn State

Turn の受付・入力保存・model iteration・Tool 実行・完了を `turn_runs` テーブルへ永続化し、重複受付防止・安全な再試行・crash 後の復旧判断を DB から行う。

### 9.1 request_key と重複受付防止

各 Ingress はプラットフォーム安定 ID から `request_key` を生成する。

| Ingress | request_key 候補 |
|---|---|
| Discord | channel／thread／platform message ID |
| Telegram | chat ID／platform message ID |
| Web | client request ID または user message ID |
| Webhook | receiver ID／外部 event ID（存在しない場合は受付時 UUID） |
| CLI / TUI | 1回の明示入力ごとに UUID |
| Agent 間 Turn | origin ID／派生元 Turn ID／派生 sequence |

`UNIQUE(chat_id, request_key)` 制約により、同じ受付を再受付しても新規 Turn を作らず既存 Turn を返す。既存 Turn が `completed` なら保存済み最終結果を LLM を呼ばずに返す。

### 9.2 状態機械

`turn_runs.state` は中央定義した遷移ルールで管理する。許可されていない遷移は DB 更新前に拒否する。

```text
accepted → input_committed → model_pending → model_completed → tools_pending → tools_completed → completed
                                                                     ↓
                                                            (error) → failed / uncertain / cancelled
```

各状態の意味は [db.md §turn_runs](./db.md#turn_runs) を参照。

### 9.3 crash recovery

起動時に `recover_interrupted()` が未端末 Turn を次の規則で処理する。

| 状態 | 処理 |
|---|---|
| `accepted` | input 未保存の受付のみ。`failed` へ移行（input 未 commit のため再受付で再開可能） |
| `input_committed` | Config fingerprint が一致すれば再受付で再開可能。不一致は `uncertain` |
| `model_pending` / `model_completed` / `tools_pending` / `tools_completed` | 中間状態は結果が不明のため `uncertain` |
| `completed` / `failed` / `uncertain` / `cancelled` | 端末状態。変更しない |

同時に `Database::recover_running_tools()` が `running` の Tool を `uncertain` へ移行する（[tools.md](./tools.md) の Tool 実行台帳を参照）。

### 9.4 安全停止の原則

再開条件を証明できない場合は、推測して処理を続けず `uncertain` で停止する。

- Config fingerprint が不一致
- partial output をすでに公開済み（`output_published = true`）
- 同じ Tool Call ID に異なる input が渡された
- Tool 実行結果が不明（`running` のまま停止）

これらは Phase 3 の Supervisor / Durable Ingress が導入されるまで、人手による確認を促す安全弁として機能する。

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
