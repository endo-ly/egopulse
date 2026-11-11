# EgoPulse Session Lifecycle

会話セッションの永続化・復元・圧縮（compaction）のアルゴリズム仕様。

## 目次

1. [Session Identity](#1-session-identity)
2. [保存モデル](#2-保存モデル)
3. [Turn 開始時の復元](#3-turn-開始時の復元)
4. [Turn 中の保存](#4-turn-中の保存)
5. [Context Compaction](#5-context-compaction)
6. [Fallback と Context Overflow](#6-fallback-と-context-overflow)
7. [Archive](#7-archive)
8. [Conversation Scope による DB Routing](#8-conversation-scope-による-db-routing)
9. [Conflict Retry](#9-conflict-retry)
10. [Durable Turn State](#10-durable-turn-state)
11. [設定リファレンス](#11-設定リファレンス)

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
| Voice | `voice:{surface}:{session_key}` | surface × session 毎 | `voice:stackchan:main` |

### 1.2 エージェント対応セッションアイデンティティ

`SurfaceContext` は `agent_id`（string）を保持し、各会話サーフェスにエージェントの識別情報を持たせる。

- `session_key()` は agent を持つ会話では `channel:surface_thread:agent:agent_id` を返す
- **Discord**: `surface_thread` はチャンネル ID。マルチボットでは同じチャンネルでも `session_key()` の `:agent:{agent_id}` 接尾辞によりエージェントごとに session が分かれる
- **Web**: 新規セッションはWebUIで選択中のagentを使用し、既存セッションは `chats.agent_id` を使用する
- **Telegram / CLI / TUI**: 各チャネルの入力経路で解決されたagentを使用する
- **保存済み session からの再構成**: Web の既存セッション再開や Pulse の Home Surface のように、保存済みの `external_chat_id`（session key 形式）から `SurfaceContext` を組み立てる経路は、`SurfaceContext::surface_thread_from_session_key()` で `{channel}:` 接頭辞と `:agent:{agent_id}` 接尾辞を除いた生の `surface_thread` を復元する。復元した context の `session_key()` は元の `external_chat_id` と一致する

### 1.3 Multi-Agent Room 二層アーキテクチャ

`multi_agent: true` の Discord チャネルでは、二層のログ構造を持つ。

| 層 | external_chat_id | chat_type | session | 用途 |
|---|---|---|---|---|
| Channel Log | `discord:{channel_id}:multi-room-log` | `channel_log` | なし | チャネル全体の会話共有 |
| Agent Session | `discord:{channel_id}:agent:{agent_id}` | `discord` | あり | エージェント個別の会話履歴 |

**Single-Agent Channel**（`multi_agent: false`）は従来の一層構造のまま。

**Channel Context 注入**:
- Multi-Agent Room で mention されたエージェントが `process_turn` を実行する際、対象エージェントへの直接入力と対象エージェント自身の assistant/tool イベントを除外した、公開会話（ユーザー発言、他エージェントの最終応答、`agent_send`）の直近 30 件を一時的な user メッセージとして注入
- system prompt、memory、tool の内部イベントは Shared Context に含めない
- Channel Context は Agent Session の `messages_json` には保存しない（一時注入のみ）

### 1.4 `agent_send` メッセージライフサイクル

エージェント間通信 (`agent_send`) は次のフローで実行される:

1. **送信元エージェント**が `agent_send` ツールを呼び出し (`to`, `message`)
2. **宛先ターンを受付**: `TurnIntake::submit()` がターンを永続的に受け付け、スケジューラへ渡す。受付に失敗したターンは後続処理を行わない
3. **送信メッセージを保存**: `MessageKind::AgentSend` と送信元・宛先を記録する。マルチエージェントルームでは Channel Log へ、それ以外のチャネルでは送信元セッションの chat へ保存する。`[From → To] message` 形式でのチャネル表示は Discord / Telegram で行う
4. **宛先ターンを非同期実行**: `process_turn` が送信元情報を含む Direct Input を宛先 Agent Session に保存する
5. **宛先エージェント応答**: `process_turn` の戻り値を `ChannelRegistry.send_text()` でチャネルに送信する。Web / TUI / CLI では応答は宛先 Agent Session への永続化のみで、チャネル送信は行われない

**制約**:
- チェーン深度 (`chain_depth`) が `MAX_AGENT_CHAIN_DEPTH` (4) を超えるターンは破棄
- 自己送信 (`from == to`) は禁止

**処理順序とログの整合**:
- 宛先ターンは永続受理の時点で実行を開始し得るため、Channel Log への保存が宛先ターンの実行に先行しないことがある
- 受理と Channel Log 保存の間でプロセスが停止した場合、受理済みターンの handoff 記録がログに残らない
- この順序により、受付拒否時に未送信のメッセージがログへ混入することを防ぐ

### 1.5 TurnScheduler による同時実行制御

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

**バックプレッシャー**: `TurnScheduler` のキューは有限容量を持つ。セッション単位で 32 turn、Runtime 全体で 512 turn までキュー可能。超過時は `submit` が `Rejected(SessionQueueFull | GlobalQueueFull)` を返し、ターンは実行されない。拒否は呼出元・構造化ログ・metric で観測可能で、silent drop しない。

- Webhook: queue full 時は `429 Too Many Requests`（`session_queue_full` / `global_queue_full`）を返し、`202` にはならない。`202` は `turn_runs` への accepted commit **完了後**に返る。再起動後に `TurnDispatcher` が再実行するのは `accepted`（受付から再開）、`input_committed`（model loop resume）、`tools_completed`（未commitの staged follow-upを反映してから、Tool を再実行せず次の model iteration だけ resume）である。受付拒否は理由コード違いで一律 `429`（`session_queue_full` / `global_queue_full` / `tracker_full` / `chain_terminated` / `shutdown`）。Webの同一 `request_key` への異なる本文は `409 Conflict` として拒否する。
- Discord / Telegram: 即時応答可能なため、拒否時にユーザーへ busy 通知を送る。
- Agent Send (`agent_send`): 非同期のため、受付拒否時はツール結果 `delivered: false` として呼び出し元へ返る。

**TurnTracker の有界化**: origin ごとの turn count / terminal reason を 1 つの state に統合し、24 時間の TTL で完了済み origin を破棄する。origin 追跡上限は 4096。TTL prunes 後も上限なら新規 origin を明示的に拒否する（既存 origin の更新は上限中も可能）。

**SystemEvent**: 停止条件によりターンが拒否された場合、Channel Log に `MessageKind::SystemEvent` で JSON 形式の理由が記録される（`{"reason": "chain_depth_exceeded"}` 等）。

---

## 2. 保存モデル

会話永続化は SQLite ベースで、役割は次の 4 つに分かれる。

### `chats`

- 役割: chat の論理 ID と surface との対応付け
- 主な列: `chat_id`, `channel`, `external_chat_id`, `chat_title`, `chat_type`, `last_message_time`, `agent_id`, `archived`, `pinned`
- `chat_title` はユーザーが付ける表示名で、セッションの識別には使わない。識別は `chat_id`（canonical key `chat:{id}`）と `external_chat_id` が担い、表示名の変更で `session_key()` や `surface_thread` は変わらない。`resolve_or_create_chat_id` は表示名を書き換えず、表示名の更新は `update_chat_display_meta` に集約する

### `messages`

- 役割: 表示用・一覧用の message レコード
- 主な列: `id`, `chat_id`, `sender_id`, `sender_kind`, `content`, `timestamp`, `message_kind`, `recipient_agent_id`, `seq`, `turn_id`
- `/new` や compaction では削除されない。セッションクリアは `sessions.messages_json` のリセットのみ行う

`seq IS NOT NULL` の committed conversation history は append-only かつ immutable であり、履歴、セッション一覧の preview、session fallback、Channel Log projection、Sleep の message source に含まれる。`seq IS NULL` は durable に受理済みだが、まだ因果履歴へ commit されていない staged message である。staged row は commit 時に `seq` が付与され、terminal Turn への promotion 時には削除される場合がある。

Tool 実行中の human follow-up は staged message として保存され、`turn_id` に current Turn（`tools_pending` または `tools_completed`）を持つ。Tool Result の保存と `complete_tools()` が成功した直後に、staged rows を timestamp と message ID の FIFO で committed history と session snapshot へ同一トランザクションで反映する。プロセス停止でこの反映が中断された場合も、`tools_completed` の recovery が同じ処理を冪等に再実行する。

### セッションの整理状態

`archived` と `pinned` は一覧の見え方を制御するメタデータで、会話の内容には影響しない。

- `pinned = 1` のセッションは一覧の先頭に固定される（`pinned` 降順 → `last_message_time` 降順 → `chat_id` 降順）
- `archived = 1` のセッションはアーカイブ扱いになる。履歴・記憶・Turn は保持され、開いて送信することもできる
- 両方が真の場合はアーカイブを優先して表示し、ピン状態は保持する（アーカイブ解除で復元される）

### `sessions`

- 役割: 次ターン再開用の session snapshot
- 主な列: `chat_id`, `messages_json`, `updated_at`

`messages_json` は `ContextSnapshot` エンベロープ（`version` / `checkpoint` / `messages` / `last_usage`）を JSON で持つ。

- `messages`: LLM 入力に近い保持メッセージ。tool call、tool result、multimodal image ref を含む
- `checkpoint`: 畳んだ履歴の要約とファイル一覧。`null` なら未圧縮
- `last_usage`: 直近の実測 usage と、それがどのメッセージまでを覆うかのアンカー

`messages_json` には checkpoint をメッセージとして重複保存しない。checkpoint はリクエスト構築時に参照用メッセージへ投影する。

### `tool_calls`

- 役割: assistant が要求した tool call と、その実行状態（Tool 実行台帳）の追跡
- 主な列: `id`, `chat_id`, `message_id`, `tool_name`, `tool_input`, `tool_output`, `timestamp`, `turn_id`, `state`, `input_hash`, `idempotency_class`, `idempotency_key`, `started_at`, `finished_at`, `error_kind`, `error_message`

---

## 3. Turn 開始時の復元

turn 開始時は次の順で session を復元する。

1. `sessions.messages_json` があればそれを `ContextSnapshot` として読む
2. `messages` を `Message` 配列へ戻し、`checkpoint` と `last_usage` を取り出す
3. **`messages` が空（意図的クリア）でもフォールバックせず、そのまま返す**
4. image は `input_image_ref` から asset store 経由で hydrate する
5. assistant tool call に対応する tool output が欠けている場合は synthetic error tool output を補う（補った場合は usage anchor を破棄する）
6. snapshot が無い（`messages_json = None`）場合だけ `messages` テーブルから recent history を組み立てる
7. snapshot が存在するのに解析できない場合は、データ欠落を隠さず明示的な保存データエラーとして扱う

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
- error が明示的に retryable（429 / 5xx / 接続失敗 / 読取り timeout / iteration deadline 到達）
- 試行回数が上限（3 回）以内
- 次の試行が iteration deadline 内に収まる

**再試行禁止**（いずれか該当で `failed` または `uncertain` へ遷移）:

- partial output を外部へ公開済み（`output_published = true`）
- Tool Call を受信・保存済み
- Tool 実行開始済み
- response event または assistant message を commit 済み
- request hash が一致しない
- error の retryability が不明

### Agent Loop の実行境界

durable Turn の外側の順序は `agent_loop/turn/mod.rs` の `TurnExecutor` が管理し、LLM と Tool の反復は `agent_loop/loop_runner.rs` の `AgentLoop` が管理する。1 回の model step は `agent_loop/model_step.rs`、Tool の ledger claim・実行・結果生成は `agent_loop/tool_execution.rs`、Message と Session snapshot の保存は `agent_loop/turn/persistence.rs`、DB 上の Turn 状態遷移は `agent_loop/turn/lifecycle.rs` が担当する。

```text
accept / resume validation
  → persist user input
  → AgentLoop
       → model step
       → persist assistant tool call
       → execute tool
       → persist tool result
       → next model iteration
  → persist final message
  → complete Turn
```

AgentLoop は final response と最新の session snapshot を `AgentLoopResult` として返す。final message の保存、`output_published`、`completed` への遷移は TurnExecutor が行うため、Loop の反復と durable Turn の完了条件は混在しない。

再試行回数上限に達した場合、または再試行禁止条件に該当した場合は Turn を `failed`（出力未公開）または `uncertain`（出力公開済み）として終了する。Codex 認証の 401 では次回 Turn 向けに token refresh を試みるが、失敗した Turn を再開しない。

### 待ち時間の上限

1 回の model iteration は全体で 300 秒、1 リクエストは 90 秒の無応答で打ち切る。1 回目の試行は全量を使えるため、正常に進行する長い生成は許容し、リトライは残り時間しか使えない。無応答の期限は read のたびにリセットされるので、進行中の生成を落とさず、接続後に何も返らない状態だけを早く検出できる。

この無応答期限は HTTP client 共通で、Turn 以外の LLM 呼び出しにも適用される。本文が終端でしか届かない非ストリーム要求では、無応答期限がそのまま生成時間の上限になる。そのためナレーション先を持たない呼び出し（sleep batch、compaction、単発の ask）も本文をストリームで受け取って組み立て、無応答期限が上流の停止だけを検出するようにする。

ストリーム内で provider が `error` / `response.failed` を通知した場合や、`finish_reason` が `error` の場合は、それまでに届いた本文があっても成功として扱わない。HTTP 200 の後に起きた失敗は、`error_type` があれば `code` より優先し、対応する HTTP status（一時的な失敗は 429/5xx、恒久的な失敗は 4xx）に写像する。未知の失敗は 502 とし、再試行可否は共通の判定に委ねる。完了イベントを受け取る前にストリームが終了した場合は `incomplete_stream` として再試行対象にする。終了イベントが `incomplete` を報告した場合や `response.status` が `failed`、Chat Completions の `finish_reason` が `length` / `content_filter` / 未知の理由の場合は、生成が打ち切られた確定的な失敗として `incomplete_response` を返し、再試行しない。`insufficient_system_resource` / `aborted` は推論側の中断として再試行対象にする。SSE の `data:` が JSON として不正な場合は欠落した本文を完成応答にせず失敗させる。

1 リクエストの総時間には別に 600 秒の上限がある。Turn / Pulse は 300 秒の iteration deadline、compaction は 600 秒の予算、単発 ask は 300 秒の総予算で先に打ち切られるため、この上限が実効するのは Sleep の各試行だけである。Sleep は再試行と待機を含む 20 分の総予算も持つ。

試行回数の記録とバックオフ待機も同じ期限の内側で行う。これらは provider から見えない処理であり、外に置くと DB 待ちが長い場合に上限を超える。期限に到達した場合は、その時点までの実際のエラーをそのまま返す。

turn 中の保存は phase ごとに進む。

1. user message を context 末尾に追加する
2. 必要なら compaction する
3. user-phase snapshot（checkpoint / 保持メッセージ / 使用量アンカー）を 1 トランザクションで保存する
4. assistant reply / tool call / tool result を進める
5. 各 phase の結果を通常の persistence に流す

compaction は保存の別系統ではなく、「保存前に context を整形する段」として扱う。checkpoint・保持メッセージ・使用量アンカーは常に単一の snapshot 更新として保存し、途中結果だけを先に書き込まない。

### Tool 実行中の human follow-up

`turn_runs.state = tools_pending` の Turn に対する通常の human input は、各 Surface の runtime intake がまず staged follow-up として受理できるかを判定する。受理時は request key を message ID として `(chat_id, request_key)` の重複を検査し、sender と本文が一致する再送だけを idempotent success とする。異なる sender または本文の再利用は conflict として拒否する。

staging の成功は SQLite の `COMMIT` 後にだけ Surface へ返す。current Turn の Tool phase が先に完了した場合は `tools_pending` が存在しないため、入力は従来の通常 Turn submission へ進む。staged message の未消費数にも session 単位と DB scope 単位の durable pending 上限を適用する。

Tool phase は途中で中断せず、保存順序は次の通りである。

```text
assistant Tool Call
  → Tool Result 全件
  → complete_tools()
  → staged human follow-up を FIFO で commit
  → 次の model iteration
```

commit 後に `UserInputInjected` event を発行するため、ライブ表示も Tool Result の後に follow-up を描画できる。follow-up の sender、正規化済み本文、受信時刻は staged row の値をそのまま保持する。Direct Input の `[Current time: ...]` は initial input と同じ formatter を使い、follow-up の durable acceptance timestamp を利用する。

---

## 5. Context Compaction

長い会話で context window 上限に近づく前に、畳んだ履歴を構造化された checkpoint 要約へ置き換え、直近の会話は原文のまま残す安全装置。

### 目的

1. context window 上限による API エラーを防ぐ
2. 畳んだ範囲の要点を、後続のモデルが読める参照用 checkpoint として維持する
3. 直近の会話・tool call / tool result の参照関係を原文のまま保つ
4. 長い 1 ターンの途中で切る場合も、そのターンの前半を独立して要約する

### Trigger

次の 5 箇所で判定する。通常・Tool 後・再開はいずれも「これから送るリクエスト」の直前に判定し、判定に使う文脈と実送信の文脈を一致させる。

| 契機 | タイミング |
|---|---|
| 通常のユーザー入力 | ユーザーメッセージを積んだ直後、最初の model 呼び出し前 |
| Tool 実行後 | tool 結果の保存が終わり、次の model 呼び出しを行う直前 |
| Durable Turn 再開 | `input_committed` / `tools_completed` から再開し、次の model 呼び出しを行う直前 |
| 手動 | `/compact [instructions]`。閾値と `enabled` を無視する |
| Context Overflow | Provider が context window 超過を理由に 400/413/422 を返したとき |

発火条件は次の 1 式のみ。

```text
should_compact = enabled && context_tokens > context_window - reserve_tokens
```

- `context_window` は実際に使用する provider / model の `context_window_tokens`。未設定時のみ `default_context_window_tokens`（既定 131072）
- `context_tokens` は推定値。直近の正常な model 応答の実測 usage（input + output）に、その後に追加されたメッセージの推定値を足す
- 実測 usage が無い、または request shape（system prompt / tool 定義 / model）が計測時と変わった場合はリクエスト全体を推定する
- `context_window <= reserve_tokens` は設定エラーとして扱う。暗黙に丸めない

### Token 推定

外部 tokenizer は使わない。メッセージ 1 件の文字数（role / content / reasoning / tool call 引数。画像は 1 枚 4,800 文字相当）を `CHARS_PER_TOKEN_ESTIMATE = 3` で割り、`usage_calibrator` の補正係数を掛ける。推定は常に過大評価側へ倒す。

補正は `(provider, model, request_kind, has_tools)` 単位で、provider が返す実測 `input_tokens` との比を EMA（α = 0.3、0.5〜3.0 に clamp）で学習する。実測が返らない間は `DEFAULT_FACTOR = 1.3`。観測は `llm_usage_logs` に `estimated_tokens` として残し、再起動時に最近の観測から再構築する。

### 保持範囲の選択

新しい側から 1 件ずつ推定 token を積み上げ、`keep_recent_tokens`（既定 20000）に達した位置を境界候補にする。

1. 境界は `user` または `assistant` のメッセージに限る。`tool` 結果を先頭とする分割は禁止する
2. 積み上げが境界に達したら、その位置以降で最初の候補を境界にする
3. 末尾の tool 結果だけで予算を超える場合は、手前の assistant tool call を境界にする（先頭へは戻さない）
4. 履歴全体が予算内なら境界は先頭になり、要約対象が無いので No-op

境界が `user` なら通常のターン境界、`assistant` なら 1 ターンの途中分割（split turn）になる。

### 要約範囲

| 範囲 | 内容 |
|---|---|
| 履歴 | 前回 checkpoint より後の、保持範囲より前の全メッセージ |
| ターン前半 | split turn のとき、そのターン開始 `user` から境界までのメッセージ |
| 保持 | 境界以降のメッセージ。tool call / tool result は対応を保ったまま原文で残す |

- 履歴は初回なら新規要約、前回 checkpoint があれば `<previous-summary>` として別入力に渡し更新する。前回要約を会話の 1 件目として再利用しない
- split turn では履歴要約とターン前半要約を別リクエストで生成し、`\n\n---\n\n**Turn Context (split turn):**\n\n` で連結する。片方でも失敗したら何も commit しない
- 要約入力は role ごとにタグ付けしたプレーンテキスト（`[User]:` / `[Assistant]:` / `[Assistant tool calls]:` / `[Tool result]:` / `[Tool error]:`）。reasoning は含めない
- 1 件の tool 結果は 2,000 文字で切り、`[... N more characters truncated]` を付ける
- 要約対象全体を固定文字数で切る処理は行わない

### 要約プロンプト

`src/agent_loop/compaction.rs` 内に 3 種類の定数を置く。形式は日常会話・相談・技術作業・Tool 実行に共通で、Goal や Next Steps の捏造を促さない。

| 用途 | 定数 |
|---|---|
| 要約システム | `SUMMARIZATION_SYSTEM_PROMPT` |
| 初回 | `INITIAL_SUMMARY_PROMPT` |
| 更新 | `UPDATE_SUMMARY_PROMPT` |
| ターン前半 | `TURN_PREFIX_SUMMARY_PROMPT` |

要約本文は `## 会話の経緯` / `## 共有された情報` / `## 重要な参照情報` の 3 節。該当が無い節は省略し、空の箇条書きを作らない。会話の使用言語を引き継ぐ。

`/compact` の追加指示は各プロンプトの末尾へ `Additional focus: ...` として付与する。

### Tool ファイル操作の追跡

要約対象の `read` / `write` / `edit` tool call から `path` 引数を集める。`write` と `edit` は modified、それ以外の `read` は read-only。modified に含まれるファイルは read-only に重複計上しない。前回 checkpoint の一覧を引き継ぎ、要約末尾へ `<read-files>` / `<modified-files>` として付ける。該当が無ければ何も付けない。

### Checkpoint と保持メッセージ

- checkpoint は `summary` / `tokens_before` / `read_files` / `modified_files` を持つ
- 次のリクエストでは checkpoint を参照用の 1 メッセージへ投影し、その後ろに保持メッセージを連結する
- 投影メッセージの先頭には `[CONTEXT CHECKPOINT — REFERENCE ONLY]` ヘッダーを付け、過去の指示を現在の指示へ昇格させない
- 保持メッセージの結合・並べ替え・末尾 assistant の削除は行わない
- 要約の長さ調整は行わない。長さはプロンプトで誘導し、事後の切り詰めをしない

### 使用量アンカー

`ContextUsageAnchor` に「どのメッセージまでの計測か」（`after_message_index`）、`input_tokens` / `output_tokens`、`model_key`、`request_shape_hash` を保存する。次の場合に破棄し、全体推定へ戻す。

- checkpoint を新規作成したとき
- snapshot 競合で保持メッセージを組み直したとき
- tool output の補修でメッセージ数が変わったとき
- Sleep Batch が snapshot を切り詰めたとき
- request shape（system prompt / tool 定義 / model）が変わったとき

## 6. Fallback と Context Overflow

要約は best effort。失敗時は snapshot を壊さないことを優先する。

| 障害パターン | 動作 |
|---|---|
| 要約対象なし | No-op。checkpoint と保持メッセージを変更しない |
| 要約リクエスト失敗（retry 枯渇を含む） | 元の context をそのまま保持 |
| 要約が空 | 元の context をそのまま保持 |
| 要約が tool call を返した | 元の context をそのまま保持 |
| 要約が出力上限で途切れた（`IncompleteResponse`） | 元の context をそのまま保持。再試行しない |
| split turn の片方だけ成功 | 何も commit しない |
| 要約中に snapshot が更新された | 古い結果を適用せず、最新 snapshot で選択し直す |

要約リクエストは LLM client のリクエスト上限（600 秒）と試行回数上限（3 回）を共有し、backoff と `Retry-After` 待機もその内側で行う。

### Context Overflow からの復旧

Provider の 400 を一律に context 超過とはみなさない。`LlmError::ApiError` の body が context 上限を示す場合（`context_length_exceeded`、`maximum context length`、`prompt is too long` 等）だけ overflow と分類する。

1. 出力を公開していない場合に限り、`CompactionReason::Overflow` で圧縮する
2. 圧縮できたら同じ model iteration を 1 回だけ再送する。`turn_runs.model_request_hash` は新しいリクエストの hash へ更新する
3. 圧縮が No-op または失敗、あるいは再送も overflow なら、エラーをそのまま返す（無限に繰り返さない）

Tool 実行済みのターンでも再実行するのは model 推論だけであり、実行済み Tool は再実行しない。公開済みの途中出力がある場合は再送しない。

## 7. Archive

compaction 発火時は、compact 前の context を markdown として archive する。

### 出力先

```text
<state_root>/runtime/groups/<channel>/<chat_id>/conversations/<timestamp>-<unique_suffix>.md
```

（`state_root` は通常 `~/.egopulse`。Secret スコープは `runtime/secret_groups/` 配下）

### 内容

- 既存 checkpoint があれば、その参照メッセージを先頭に含める
- 保持メッセージを続けて `## role` + 本文 + `---` の形式で保存する
- 三層 redaction（値・prefix・文脈）を適用し、credential を `[REDACTED:key]` / `[REDACTED:secret]` へ置換する
- ファイルパーミッションは `0600`
- temp ファイルへ書いてから rename する原子的書き込み。同名ファイルは上書きしない（冪等キー）
- archive 失敗は warn に留め、compaction 自体は続行する（`messages` テーブルが全文履歴を保持している）

### Sleep Batch での Archive

Sleep Batch も session クリア前に `archive_conversation_blocking`（compaction モジュールの共有関数）を呼び、同じ形式でアーカイブする。checkpoint がある場合はその参照メッセージも archive に含める。

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

### tool_call 永続化のルーティング

`ConversationScope::Secret` では、Tool 実行台帳（claim・input hash・状態遷移・結果保存）も `secret.db` 側の `tool_calls` へ書かれる。Secret Tool の入出力が通常 DB へ漏れないよう、Tool 実行開始時の claim と結果保存の両方が `state.db_for(ctx.scope)` 経由で Secret DB にルーティングされる（[architecture.md §7.1](./architecture.md#71-conversationscopeストレージ境界) 参照）。tool call block は `sessions.messages_json` に包含されており、LLM context 復元には影響しない。

### Compaction Archive の出力先分離

`TurnDependencies::storage_for(scope)` で解決される archive root に従い、Secret スコープの compaction アーカイブは `runtime/secret_groups/` に出力される。Normal スコープは `runtime/groups/` のまま。

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
| 最初の user-phase retry | compaction-aware（最新 snapshot の checkpoint と使用量アンカーから再評価） |
| 以降の assistant / tool phase retry | compaction はすでに終わっている前提で通常 persist。使用量アンカーはメッセージ位置が変わるため破棄する |
| `/compact` | `chats.revision` の CAS で保存し、競合したら何も保存せず再実行を促す |

## 10. Durable Turn State

Turn の受付・入力保存・model iteration・Tool 実行・完了を `turn_runs` テーブルへ永続化し、重複受付防止・安全な再試行・crash 後の復旧判断を DB から行う。

### 10.1 request_key と重複受付防止

各 Ingress はプラットフォーム安定 ID から `request_key` を生成する。

| Ingress | request_key 候補 |
|---|---|
| Discord | channel／thread／platform message ID |
| Telegram | chat ID／platform message ID |
| Web | client request ID または user message ID |
| Webhook | receiver ID／payload の `event_id`・`id`・`message_id` のいずれか／存在しない場合は payload の SHA-256（先頭 8 hex、ゼロ hash 時のみ受付時 UUID） |
| CLI / TUI | 1回の明示入力ごとに UUID |
| Agent 間 Turn | origin ID／派生元 Turn ID／派生 sequence |

`UNIQUE(chat_id, request_key)` 制約により、同じ受付を再受付しても新規 Turn を作らず既存 Turn を返す。既存 Turn が `completed` なら保存済み最終結果を LLM を呼ばずに返す。実行中（非端末）の Turn に重複受付した場合は二重実行を始動せず、「処理中である」旨の終端応答を返す。以前の実行が `failed` / `uncertain` / `cancelled` で停止している場合も再実行せず、その旨を終端応答で伝える。いずれの受付結果も呼び出し元へ明確に終端し、Web `done` イベントやチャネル応答として観測される。

### 10.2 状態機械

`turn_runs.state` は中央定義した遷移ルールで管理する。許可されていない遷移は DB 更新前に拒否する。

```text
accepted → input_committed → model_pending → model_completed → tools_pending → tools_completed → completed
                                                                     ↓
                                                            (error) → failed / uncertain / cancelled
```

各状態の意味は [db.md §turn_runs](./db.md#turn_runs) を参照。

### 10.2.1 失敗の可視化

`failed` / `uncertain` への遷移は、会話にも `message_kind = turn_failure` のメッセージとして記録する。Web UI は履歴だけを情報源とするため、失敗が `turn_runs` にしか無いとリロード後に消える。

- 状態遷移と会話エントリは同一 transaction で確定する。分けると「Turn は端末なのに履歴が無い」状態が残り、起動時 recovery も端末 Turn を対象にしないため二度と補完されない
- `turn_runs` に残す内部診断と、会話に書く文言は分ける。会話に出る以上、利用者が読んで意味の通る文でなければならない
- `uncertain` は出力や Tool 副作用が既に発生している可能性がある。再送すると操作が重複し得るため、再実行を促さず結果の確認を促す
- 会話エントリの文言は push チャネルにもそのまま使う。2箇所で組み立てると、片方だけが再実行を促す状態になり得る
- LLM に渡す文脈からは除外する。エージェントが自分のエラーを会話として読まない

### 10.3 crash recovery

起動時の recovery は、Tool と Turn の状態を復元した後、terminal Turn に残った staged follow-up を通常の durable Turn として再投入し、続いて `TurnDispatcher` が実行可能な Turn を再実行する。再開条件を証明できない Turn を推測で再開せず、すべて停止状態へ移す。

#### `recover_interrupted_turns()` の分類

| 状態 | 処理 |
|---|---|
| `accepted` / `input_committed`（`scheduled_request_json` あり） | **そのまま残す**。要求が永続化されているため `TurnDispatcher` が再実行する（accepted は受付から、input_committed は model loop から resume） |
| `accepted`（`scheduled_request_json` なし・旧式直接実行） | `failed`（input 未 commit。再受付で安全に再開可能） |
| `input_committed`（`scheduled_request_json` なし・旧式直接実行） | `failed`（model 未実行。再受付で安全に再開可能） |
| `tools_completed`（`scheduled_request_json` あり） | **そのまま残す**。Tool Results は永続化済みで、未commitの staged follow-up があれば先に同じTurnへ反映し、Tool を再実行せず次の model iteration だけを `TurnDispatcher` が resume する |
| `model_pending` / `model_completed` / `tools_pending` / `tools_completed`（`scheduled_request_json` なし） | `uncertain`（外部出力や Tool 副作用が及んでいる可能性があり、再開の安全性を証明できない） |
| `completed` / `failed` / `uncertain` / `cancelled` | 端末状態。変更しない |

起動時の recovery 分類は Turn の状態と durable payload の有無に基づいて行う。`input_committed` / `tools_completed` Turn を resume する際は、保存済み Config fingerprint と resume 時の Config snapshot が一致することも検証する。不一致の場合は安全な再開条件を満たさないため終了する。`input_committed` は `failed`、`tools_completed` は Tool の副作用が既に発生している可能性があるため `uncertain` とする。`tools_completed` の resume は未commitの staged follow-upを先にsession snapshotへ反映し、元の input を再保存せず、完了済み Tool を再実行しない。

同時に `Database::recover_running_tools()` が `running` の Tool をすべて `uncertain` へ移行する（[tools.md](./tools.md) の Tool 実行台帳を参照）。

#### TurnDispatcher による再実行

`TurnDispatcher` は起動後、定期的（既定 5s）に Normal / Secret 両 DB を走査する。まず terminal Turn に紐づく `seq IS NULL` の human message を、target Turn の `scheduled_request_json` から復元した routing context に重ねて新しい root Turn へ promotion する。staged message の sender、本文、message ID、受信 timestamp は新しい `ScheduledTurn` に引き継ぐ。新しい Turn の durable acceptance 後にだけ元の staged row を削除するため、accept 後の再起動でも同じ request key の Turn を再利用できる。

続いて `accepted` / `input_committed` / `tools_completed` で残された Turn を検出して再投入する。同じ `turn_id` は当プロセス内で1回だけ再投入し、executor 側の CAS（`accepted → input_committed`、`input_committed` / `tools_completed → model_pending`）が重複実行を安全に排除する。`input_committed` / `tools_completed` で resume 検証（input message、session snapshot、保存済み Config fingerprint の整合性）に失敗した Turn は `failed` へ移行する。dispatcher の terminal staged promotion は通常 scan と startup recovery で同じ operation を使い、各 DB scope を独立して処理する。

Dispatcher が保存済み `scheduled_request_json` の JSON 構文、必須フィールド、型、または version を解釈できない場合は、決定的な復旧不能として Turn を終端化する。`accepted` / `input_committed` は `failed`、`tools_completed` は `output_published` の状態に応じて `failed` または `uncertain` へ移行する。このとき `error_kind` は `durable_payload_invalid`、`error_message` は固定の sanitized 文言とし、payload の内容や deserializer の詳細をログ・DBへ保存しない。`origin_id` がある場合は Turn の失敗と origin の terminal reason を同一 transaction で記録する。DB 読み取り失敗と scheduler の容量不足は一時障害として扱い、Turn を `accepted` / `input_committed` / `tools_completed` のまま次の tick で再試行する。

### 10.4 安全停止の原則

再開条件を証明できない場合は、推測して処理を続けず `failed` または `uncertain` で停止する。

- partial output をすでに公開済み（`output_published = true`）。ただし `tools_completed` は Tool Results が揃っており、未commitの staged follow-upも先に反映できるため、次の model iteration だけを再開できる
- 同じ Tool Call ID に異なる input が渡された
- Tool 実行結果が不明（`running` のまま停止）
- 同一 `request_key` へ異なる本文が再受付された

これらは人手による確認を促す安全弁として機能する。`accepted` / `input_committed` は外部出力が一切発生していないことが前提のため、要求が永続化されていれば再実行（§10.3）、そうでなければ `failed` として再受付による安全な再実行を許す。`tools_completed` は未commitの staged follow-upをsession snapshotへ反映したうえで、完了済み Tool の再実行を伴わず、次の model iteration だけを再開できる。それ以外の状態は出力または副作用が及んでいる可能性があるため `uncertain` とする。

---

## 11. 設定リファレンス

Session Lifecycle に関連する設定フィールドは [config.md §3.1](./config.md#31-グローバル設定) を参照。

| 設定 | デフォルト | 役割 |
|------|-----------:|------|
| `compaction.enabled` | `true` | 自動 compaction の有効化。`/compact` と overflow 復旧は影響を受けない |
| `compaction.reserve_tokens` | `16384` | context window のうち出力・tool schema・安全マージンとして確保する token 数 |
| `compaction.keep_recent_tokens` | `20000` | 原文のまま保持する直近履歴の下限 token 数 |
| `max_history_messages` | `50` | snapshot が使えない時に `messages` テーブルから復元する件数 |
| `default_context_window_tokens` | `131072` | モデル別 `context_window_tokens` が無い場合の context window |

### Validation

- `compaction.reserve_tokens >= 1`
- `compaction.keep_recent_tokens >= 1`
- `max_history_messages >= 1`
- `default_context_window_tokens >= 1` かつ `<= 1,000,000`
- `default_context_window_tokens > compaction.reserve_tokens`
- モデル別 `context_window_tokens > compaction.reserve_tokens`
