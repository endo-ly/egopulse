# Conversation Interrupt — Agent向け詳細実装計画

- Repository: `endo-ly/egopulse`
- Base branch: `main`
- Baseline commit: `558a9f21c4f1354777dd76371e0d1f61da86bd9a`
- Repository destination: `docs/plan/plan-conversation-interrupt.md`

---

# 1. 目的

Agent が Tool を実行している最中に、人間が追加メッセージを送れるようにする。

基本動作:

```text
User A
  ↓
LLM
  ↓
Tool Call
  ↓
Tool 実行中

       User B
         ↓
       durable に受理
         ↓
       messages に staged message として保存
       seq = NULL

Tool 完了
  ↓
Tool Result 永続化
  ↓
complete_tools()
  ↓
staged User message を causal history へ commit
  ↓
seq を発行
  ↓
session snapshot へ追加
  ↓
次の LLM request
```

Tool phase 自体は最後まで完了させ、

```text
assistant Tool Call
→ Tool Result
→ User follow-up
```

という会話順序を維持する。

---

# 2. 設計の中心

既存 `messages` table の `seq` を、conversation lifecycle の境界として利用する。

```text
seq IS NULL
→ durable に受理済みだが、まだ causal conversation history に commit されていない message

seq IS NOT NULL
→ committed conversation history
```

`messages.turn_id` には、follow-up を受理した時点で `ToolsPending` の current Turn を設定する。

これにより、

```text
messages
  ├─ committed history
  │    seq = 1, 2, 3...
  │
  └─ staged human input
       seq = NULL
       turn_id = current Turn
```

という1つの message lifecycle として扱える。

---

# 3. Same-Turn Follow-up の適用範囲

same-Turn follow-up の対象は、

```text
TurnRunState::ToolsPending
```

の current Turn。

この状態なら、

```text
follow-up acceptance
↓
current Tool phase
↓
complete_tools()
↓
staged message commit
```

という安全な境界が一意になる。

それ以外の状態では、各 Surface の既存 input handling をそのまま利用する。

```text
Discord / Telegram
→ normal Turn submission

TUI
→ existing busy / pending_prompt handling

Web
→ existing busy handling
```

---

# 4. Safe Boundary

現在の Agent Loop は概念的に、

```text
complete_model
→ begin_tools
→ assistant Tool Call 保存
→ ToolExecutor
→ Tool Result 保存
→ complete_tools
```

と進む。

follow-up commit は `complete_tools()` 成功直後。

```text
persist Tool Results
→ complete_tools
→ staged User messages commit
→ retry guard reset
→ compaction
→ next model iteration
```

複数 Tool がある場合も同じ。

```text
assistant tool_calls [A, B, C]
↓
Tool Result A
Tool Result B
Tool Result C
↓
User B
User C
↓
next LLM
```

---

# 5. Staged Message

Tool 中に human follow-up を受理したら `messages` に保存する。

概念:

```text
id          = request_key
chat_id     = current chat
sender_id   = context.surface_user
content     = normalized input
sender_kind = user
timestamp   = received_at
message_kind= message
seq         = NULL
turn_id     = current target Turn
parent_message_id = NULL
```

`normalized input` は、その Surface が通常 Turn へ渡す直前の入力を指す。
Discord / Telegram の添付ファイル処理などがある場合は、既存の media normalization 後の text をそのまま保存する。

`sender_id` は通常 Turn の User message と同じ意味に合わせ、`SurfaceContext.surface_user` を保存する。
platform message identity は `request_key` が担当する。

`seq = NULL` の間は session snapshot や causal history には含めない。

---

# 6. Request Identity

staged follow-up の `messages.id` には stable ingress identity を使う。

基本:

```text
message.id = request_key
```

既存 `(id, chat_id)` primary key を利用して redelivery を dedupe する。

同じ `(chat_id, request_key)` が再度到着した場合:

```text
same sender + same content
→ idempotent success

different sender/content
→ conflict
```

request-key normalization は既存ロジックを共有する。

normal Turn promotion 時の request hash も既存 `canonical_request_hash(context, input)` を使う。

---

# 7. Sender Provenance

follow-up sender は current Turn starter と一致するとは限らない。

例:

```text
User A
→ Turn start
→ Tool 実行

User B
→ follow-up
```

staged row の `sender_id` に User B を保存する。

follow-up commit では staged row 自身を sender provenance の正準として扱う。

---

# 8. Received Time

staged message の `timestamp` は durable acceptance 時刻。

Tool 完了時刻ではない。

LLM 用 direct-input の、

```text
[Current time: ...]
```

にも staged row の `timestamp` を利用する。

---

# 9. Storage Responsibility

`messages` lifecycle の所有は既存 `src/storage/chat.rs`。

follow-up acceptance と staged→committed transition は、既存 conversation persistence の延長として実装する。

Storage access は `SurfaceContext.scope` / target Turn の scope に対応する `db_for(scope)` を通し、Normal / Secret の双方で同じ lifecycle を使う。

Storage API は、不変条件単位の操作にする。

概念:

```text
stage_tool_followup
commit_staged_user_messages
list_terminal_staged_user_messages
delete_staged_user_message_after_promotion
```

具体的な関数名は既存命名と責務に合わせて実装時に調整する。

---

# 10. Atomic Staging

staging は対象 scope のDBで1 SQLite transactionとして行う。

```text
BEGIN IMMEDIATE

1. (chat_id, request_key) の existing message を確認

2. existing row があれば payload consistency を確認

3. chat_id の ToolsPending Turn を取得

   0件
   → NoToolPhase

   1件
   → target Turn

   2件以上
   → Conflict
      session ownership invariant violation

4. target Turn に対して staged message を INSERT
   seq = NULL
   turn_id = target Turn

COMMIT
```

`Accepted` はこの COMMIT が成功した後にだけ返す。
TUI の composer clear、Web の queued ACK、channel 側の受付成功扱いも `Accepted` の後に行う。

これにより Tool completion との race と受付 durability を同じ transactional boundary で決定できる。

### staging が先に commit

```text
ToolsPending 確認
↓
seq=NULL message INSERT
↓
COMMIT

Tool completion
↓
complete_tools
↓
staged commit
```

### Tool completion が先に commit

```text
ToolsPending
↓
ToolsCompleted

その後 follow-up acceptance
↓
current ToolsPending Turn なし
↓
Surface の通常 input handling
```

---

# 11. Capacity

staged human input の未消費数は、

```text
seq IS NULL
AND sender_kind = 'user'
AND message_kind = 'message'
```

で数える。

閾値は既存 durable pending limits に合わせる。

```text
MAX_DURABLE_PENDING_PER_SESSION
MAX_DURABLE_PENDING_PER_SCOPE
```

Surface には、accepted / no active tool phase / capacity full を区別して返せるようにする。

---

# 12. Committed History Boundary

conversation history reader は、

```text
seq IS NOT NULL
```

の message を対象にする。

実装前に repo 全体で、

```text
FROM messages
JOIN messages
messages m
```

を検索し、各 query を以下に分類する。

```text
committed-history reader
exact-ID lookup
staging/recovery reader
Channel Log reader
```

重点確認箇所:

```text
get_recent_messages
get_all_messages
load_session_snapshot

list_sessions:
  last_message_time
  last_message_preview
  message_count

Channel Log projections

Sleep:
  pending-message extraction
  checkpoint source queries
  session/message counts

Web/TUI transcript history
export/archive/history query
```

history/session/Sleep/preview 系 query では committed rows のみを対象にする。

---

# 13. Staged → Committed Transition

follow-up commit は current Turn の conversation persistence として扱う。

呼び出し位置:

```text
persist Tool Results
→ complete_tools
→ commit staged User messages
```

1 transaction で:

```text
1. target Turn が ToolsCompleted であることを確認

2. turn_id=? AND seq IS NULL の staged User rows を FIFO 取得

3. next_message_seq を順番に割り当て

4. staged rows の seq を更新

5. final session snapshot を保存
   snapshot_through_seq = 最後に割り当てた seq

6. chats.next_message_seq を staged件数分進める

7. chats.revision を committed staged message件数分進める

8. COMMIT
```

FIFO:

```text
ORDER BY timestamp ASC, id ASC
```

batch 全体は1 SQLite transactionだが、既存 conversation invariant に合わせて各 committed User message が1つの causal `seq` と1つの revision advancement を持つ。

`SessionSnapshotConflict` の場合は既存 phase persistence と同じ方針で、最新 committed snapshot と現在の staged rows を読み直して candidate messages を再構成し、同じ atomic commit を1回再試行する。

committed row へ移った後は、その message の content / sender / timestamp / turn_id を通常の conversation history として扱う。

---

# 14. Conversation Persistence の共有

現行 `commit_message_locked()` は、

```text
message insert
seq allocation
session snapshot
revision CAS
```

を担っている。

staged commit を追加するときも、

```text
next_message_seq
revision CAS
session snapshot write
chat counter update
```

は既存 conversation commit の共通 primitive を使う。

実装時には、

```text
new message を commit
existing staged message を commit
```

の両方が同じ seq / revision / snapshot logic を通る構造に整理する。

---

# 15. Direct Input Formatting

normal initial user input と follow-up は同じ LLM representation を使う。

既存 initial input の、

```text
<direct-input>
[Current time: ...]
...
</direct-input>
```

生成を shared helper に集約する。

候補:

```text
src/agent_loop/message_format.rs
```

formatter は入力の受信時刻を引数として受け取り、Config timezone で `[Current time: ...]` を生成する。

same-Turn follow-up では staged row の、

```text
content
timestamp
```

を使う。

normal Turn では通常受付時刻を使い、abnormal recovery で promoted された human input では元 staged row の受信時刻を使う。

---

# 16. AgentEvent

conversation history へ follow-up が commit された時点で event を発行する。

```rust
AgentEvent::UserInputInjected {
    message_id: String,
    sender_id: String,
    text: String,
    timestamp: String,
}
```

順序:

```text
DB COMMIT
↓
UserInputInjected
```

Surface はこの event を使って、

```text
Tool Result
→ User follow-up
```

の表示順を保つ。

---

# 17. Abnormal Terminal Recovery

Tool phase 中に process crash / runtime failure が発生すると、

```text
messages
seq = NULL
turn_id = terminal Turn
```

が残る場合がある。

この staged human input は normal durable Turn として再投入する。

必要な情報:

staged message:

```text
id
chat_id
sender_id
content
timestamp
turn_id
```

target `turn_runs`:

```text
scheduled_request_json
```

target Turn の durable request から session routing context を復元し、staged message の human input metadata を重ねて normal human root context を構成する。

```text
channel             = target context
surface_thread      = target context
chat_type           = target context
agent_id             = target context
channel_log_chat_id = target context
scope               = target context

surface_user         = staged.sender_id
request_key          = staged.id
chain_depth          = 0
origin_id            = empty
trace_id             = empty
input                = staged.content
received_at          = staged.timestamp
```

existing normal intake が新しい human root origin を採番し、新しい Turn execution が trace identity を生成する。

promotionされた入力でも受信時刻を維持するため、`ScheduledTurn` に optional `received_at` を持たせ、existing durable scheduled-turn payloadにもその値を保存する。

通常の新規 Turn は通常受付時刻を使い、promotionされた Turn は staged row の `timestamp` を使う。

`prepare_turn()` と User `StoredMessage` persistence は `ScheduledTurn.received_at` がある場合にその時刻を利用する。

構成した `ScheduledTurn` は existing `submit_scheduled_turn` へ渡す。

normal Turn の durable acceptance 成功後、元の staged row を削除する。

---

# 18. Recovery Idempotency

promotion:

```text
staged row
↓
submit_scheduled_turn
↓
normal Turn durable accepted
↓
staged row DELETE
```

Turn accept後、DELETE前にprocessが落ちた場合も、

```text
restart
↓
same staged row
↓
same request_key
↓
existing Turn reuse
↓
staged row DELETE
```

となる。

既存 `(chat_id, request_key)` idempotency を recovery に利用する。

---

# 19. Terminal Promotion / Startup Recovery

terminal target に紐づく staged messages の promotion は runtime の Turn dispatcher が共通 owner になる。

通常稼働中の dispatcher scan:

```text
for each scope
↓
terminal Turn に紐づく seq=NULL User messages を scan
↓
normal durable Turn へ promote
↓
existing durable turn dispatch
```

これにより Scheduled execution / Web direct execution / TUI direct execution のどこで target Turn が terminal になっても、同じ durable recovery path が拾える。

startup:

```text
recover running tools
→ recover interrupted turns
→ terminal staged messages を promote
→ durable turn dispatcher
```

startup と通常稼働中の scan は同じ promotion operation を利用する。

Normal / Secret は `state.scoped_databases()` の各 scope で処理する。

---

# 20. Discord

human normal message の submission point で shared runtime helper を呼ぶ。

```text
try_stage_tool_followup
├─ Accepted
│    → current Turn の staged message
│
└─ NoToolPhase
     → existing submit_agent_turn
```

Discord adapter は request identity / SurfaceContext の組み立てまでを担当し、staging state transition は runtime/storage に委譲する。

---

# 21. Telegram

Discord と同じ shared runtime boundary を利用する。

```text
human message
↓
try_stage_tool_followup
↓
Accepted / normal Turn
```

existing platform request identity を利用する。

---

# 22. Channel Log

multi-agent room の human message は existing Channel Log に保存される。

Agent Session 側では、同じ人間入力を staged input として current Turn に紐づける。

```text
Channel Log
→ existing committed message

Agent Session
→ staged message
   seq = NULL
```

---

# 23. TUI

`submit_prompt` では既存 slash/control classification を先に行い、ordinary chat prompt を shared follow-up acceptance へ渡す。

```text
busy + ordinary prompt + current Turn = ToolsPending
→ durable staged message
→ Accepted 後に composer submission を確定

busy + ordinary prompt + other Turn state
→ existing pending_prompt

slash / session control
→ existing command routing
```

TUI input は submission 時に stable request key を発行して利用する。

Transcript は `UserInputInjected` を受け取った時点で User block を追加する。

表示順:

```text
Tool Result
→ User follow-up
→ Agent response
```

---

# 24. Web

WebSocket の first send と follow-up send は、同じ session/context resolution を使う。

既存 `start_stream_run()` に含まれている、

```text
message normalize
session_key resolve
chat resolve/create
SurfaceContext build
request_key assignment
slash command classification
```

を first send / follow-up 判定の双方から使える形に整理する。

## Connection-local active run

WebSocket connection は現在の active chat send を、

```text
ActiveChatSend {
    run_id,
    session_key,
}
```

として保持する。

first send:

```text
normal run start
↓
run_id / canonical session_key を ActiveChatSend に保存
↓
accepted ACK
↓
existing stream forwarder
```

forwarder が terminal event まで完了した時点で active state を clear する。

## Second send

同じ connection で active run がある場合:

```text
second chat.send
↓
request parse / canonical session resolve
↓
ordinary chat message
↓
active session_key と一致
↓
try_stage_tool_followup
```

### ToolsPending

```text
staged DB COMMIT
↓
Accepted
↓
queued ACK {
    run_id: active run_id
}
↓
existing stream forwarder が継続
```

### Other Turn state

existing busy response を返す。

別 session 宛ての second send も existing busy handling に合わせる。

slash command は existing command routing を使う。

## Web request ID

client ですでに生成している、

```ts
const requestId = crypto.randomUUID();
```

を `params.requestId` にも渡す。

```ts
params: {
    sessionKey,
    message: text,
    requestId,
}
```

## Web event flow

```text
AgentEvent::UserInputInjected
↓
existing RunHub
↓
user_input event
↓
existing WebSocket forwarder
↓
useChatTransport
↓
chat reducer
↓
User ChatMessage
```

abnormal terminal recovery で normal Turn 化されたケースは、existing durable Turn / history model で追跡できる状態にする。

---

# 25. Config Semantics

same-Turn follow-up は current Turn が固定した Config snapshot を継続使用する。

異常終了後に normal Turn 化された follow-up は、その normal Turn intake 時点の既存 Config semantics に従う。

---

# 26. 実装順序

## Step 0 — Baseline / Message Query Audit

最新 `main` を確認。

repo-wide search:

```text
FROM messages
JOIN messages
messages m
seq IS NULL
seq IS NOT NULL
next_message_seq
snapshot_through_seq
```

各 `messages` reader を分類する。

```text
committed-history reader
exact-ID lookup
staging/recovery reader
Channel Log reader
```

---

## Step 1 — Committed History Boundary

history/session/Sleep/preview 系 reader を `seq IS NOT NULL` 基準へ整理する。

重点:

```text
get_recent_messages
get_all_messages
load_session_snapshot
list_sessions
Channel Log projection
Sleep message sources
Sleep message counts
```

---

## Step 2 — Request Identity

既存 request-key resolution を follow-up path でも共有する。

Web client の `requestId` を server params まで流す。

TUI input acceptance に stable request key を持たせる。

---

## Step 3 — Atomic Staging

Storage に、

```text
scope DB resolve
→ current ToolsPending Turn cardinality check
→ duplicate check
→ seq=NULL User message INSERT
→ COMMIT
```

を1 transactionで行う operation を追加。

`src/runtime/channel_input.rs` に shared runtime boundary を追加し、COMMIT後の `Accepted` を Surface へ返す。
---

## Step 4 — Direct Input Formatter

initial user input と follow-up の direct-input formatting を shared helper に集約。

---

## Step 5 — Conversation Commit Primitive

existing conversation persistence を整理し、

```text
new message commit
staged message batch commit
```

の双方で、

```text
seq allocation
revision CAS
session snapshot
chat counters
```

を共有する。

staged batch は1 transactionで各messageにseqを割り当て、revision / next_message_seq を件数分進める。

CAS conflict時は最新 committed snapshotをreloadして1回再試行する。
---

## Step 6 — TurnPersistence Integration

TurnPersistence に current Turn の staged User messages を commit する operation を追加。

入力:

```text
turn_id
current loop messages
current session revision
```

処理:

```text
staged rows FIFO
→ direct-input Message 作成
→ atomic DB commit
```

出力:

```text
updated loop messages
updated session revision
committed User messages
```

---

## Step 7 — AgentLoop Safe Boundary

`complete_tools()` 成功直後に staged message commit を実行する。

commitされた各 User messageについて `UserInputInjected` を送る。

---

## Step 8 — Terminal Recovery

terminal Turn に紐づく staged User messages を normal Turn intake へ渡す共通 operation を追加。

staged `timestamp` を `ScheduledTurn.received_at` として durable request に引き継ぐ。

成功後に staged row を削除する。

Turn dispatcher の通常scanと startup recovery が同じ operation を利用する。
---

## Step 9 — Discord / Telegram

human normal input に shared follow-up acceptance を接続する。

---

## Step 10 — TUI

busy ordinary prompt を ToolsPending 時は durable staging へ接続する。

command classification は existing routing を利用する。

`UserInputInjected` を transcript へ反映する。
---

## Step 11 — Web

- session/context resolution の共有
- connection-local `ActiveChatSend { run_id, session_key }`
- second send staging
- durable COMMIT後の queued ACK
- requestId forwarding
- RunHub event forwarding
- reducer User message append

を実装する。
---

# 27. Production Code Review

実装後に、production code の責務と重複を確認する。

## Message lifecycle

search:

```text
seq IS NULL
seq IS NOT NULL
```

確認:

- staged / committed の意味が一貫
- committed-history reader が staged row を含まない

## Request identity

search:

```text
resolve_request_key
request_key.is_empty
Uuid::new_v4
canonical_request_hash
```

同じ意味の normalization が複数実装になっていないことを確認。

## Conversation persistence

search:

```text
next_message_seq
SessionSnapshotConflict
revision
snapshot_through_seq
```

seq allocation / revision CAS / snapshot write の責務が共有されていることを確認。

## Tool phase detection

search:

```text
ToolsPending
tools_pending
```

follow-up acceptance の state判断が shared storage/runtime operation に集約されていることを確認。

## Direct input

search:

```text
<direct-input>
[Current time:
```

format生成の正準を確認。

## Recovery

search:

```text
seq IS NULL
submit_scheduled_turn
```

live / startup recovery が同じ promotion operation を使っていることを確認。

---

# 28. Test Strategy

既存 test の保証範囲を確認し、

```text
A. 既に保証されている
B. existing testへのassertion追加で保証できる
C. 新しいscenarioが必要
```

に分類する。

## Committed History Boundary

最低限確認:

```text
seq=NULL staged message
→ transcript/history に出ない
→ session snapshot load に出ない
→ Sleep source に出ない
→ last-message preview に出ない
```

existing query tests を更新して保証する。

## Staging

確認:

```text
ToolsPending 0件
→ NoToolPhase

ToolsPending 1件
→ seq=NULL User row durable保存
→ COMMIT後に Accepted

ToolsPending 2件
→ Conflict

same request key + same content
→ idempotent Accepted

same request key + different content
→ conflict

Tool completionが先
→ normal input path
```

Normal / Secret の両scopeで同じ storage operation が成立することも確認する。

## 最重要 Integration Scenario

1本の強いscenario:

```text
LLM #1
→ Tool Call A + B
→ Tool block

User B
User C

Tool release
↓
Tool Result A/B
↓
User B
↓
User C
↓
LLM #2
```

同時に確認:

- multi Tool
- safe boundary
- staged `seq=NULL`
- commit後 `seq IS NOT NULL`
- FIFO
- sender provenance
- received timestamp
- session snapshot
- next LLM request
- UserInputInjected
- history query

## Atomicity

既存 fault injection で、

```text
staged commit途中にStorage failure
→ seq assignment / snapshot / revisionが rollback
```

を確認する。

## Recovery

確認:

```text
terminal target + seq=NULL
→ dispatcher scan
→ existing normal Turn intake

promoted request
→ original staged timestamp が direct-input と StoredMessage に残る

Turn accept後 / staged DELETE前のretry
→ existing Turn再利用
→ duplicateなし
```

## Surface

Discord / Telegram は shared runtime behavior の wiring を確認。

TUI:

```text
Tool Result
→ UserInputInjected
→ transcript User
```

Web:

```text
first send
→ connection active run_id/session_key を保持

same-session second send + ToolsPending
→ staged COMMIT
→ queued ACK uses same run_id

second send + other state
→ busy

requestId
→ params.requestId

user_input
→ User ChatMessage

terminal forwarder
→ active run state clear
```

---

# 29. Verification

## Targeted

```bash
cargo fmt --check
cargo clippy --lib
cargo test --lib <relevant filter>
```

Web:

```bash
npm run typecheck --prefix web
npm test --prefix web -- <relevant filter>
```

## Manual — TUI

```text
long Tool
↓
follow-up
↓
Tool Result
↓
User follow-up
↓
next Agent response
```

## Manual — Web

```text
Tool実行中
↓
second message
↓
queued
↓
Tool Result
↓
User follow-up
↓
same active run継続
```

## Manual — Discord / Telegram

- human follow-up の durable commit 後に受付成功となる
- human follow-up が current Tool phase 後に反映される
- attachment/media normalization 後の input がそのまま反映される
- duplicate delivery が idempotent に処理される
- Normal / Secret の対応scopeに履歴が保存される
- normal input path も維持される

## Restart

```text
Tool中 follow-up staging
↓
process kill
↓
restart
↓
target Turn recovery
↓
staged row normal Turn化
↓
human input / sender / received_at 保持
```

---

# 30. Full Verification

最後に:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features
cargo test
```

Web:

```bash
npm run typecheck --prefix web
npm test --prefix web
npm run build --prefix web
```

repository固有CI commandがある場合はそれも実行する。

---

# 31. 想定変更ファイル

```text
src/storage/chat.rs
src/storage/turn.rs
src/storage/mod.rs

src/runtime/channel_input.rs
src/runtime/turn/scheduled.rs
src/runtime/turn/dispatch.rs

src/agent_loop/message_format.rs
src/agent_loop/event.rs
src/agent_loop/loop_runner.rs
src/agent_loop/turn/persistence.rs

src/storage/sleep.rs

src/channels/discord.rs
src/channels/telegram.rs
src/channels/tui/mod.rs
src/channels/tui/transcript.rs

src/channels/web/ws.rs
src/channels/web/stream.rs

web/src/features/chat/useChatTransport.ts
web/src/features/chat/chatReducer.ts

docs/session-lifecycle.md
```

実際の責務配置に応じて変更対象は絞る。

---

# 32. Commit方針

目安:

```text
1. refactor(storage): define committed message history boundary
2. feat(agent-loop): commit tool-phase followups after tool results
3. feat(runtime): recover staged human followups
4. feat(channels): accept followups during tool execution
5. feat(web): queue and render tool-phase followups
6. docs: document staged conversation followups
```

---

# 33. 完了条件

実装完了時に、次の一連の挙動が成立していること。

```text
1. Tool 実行中に human follow-up を送れる

2. follow-up は durable COMMIT 後に受付成功となる

3. current Tool phase は最後まで完了し、
   assistant Tool Call
   → Tool Result
   → User follow-up
   の順序になる

4. 複数 Tool / 複数 follow-up でも、
   全 Tool Result
   → follow-up FIFO
   → next LLM
   となる

5. next LLM request は follow-up を同じ current Turn の履歴として参照する

6. follow-up の sender / normalized input / received_at が保持される

7. target Turn が safe boundary 前に terminal になった場合、
   dispatcher が staged input を normal durable Turn として処理する

8. process restart 後も同じ recovery operation で input が処理される

9. request redelivery は既存 request identity により idempotent に処理される

10. Discord / Telegram / TUI / Web の ordinary human input で同じ semantics が成立する

11. slash / control / bot / agent-to-agent input は既存の routing semantics で処理される

12. Normal / Secret の対応scopeで同じ lifecycle が成立する

13. conversation history / session snapshot / Sleep は committed message (`seq IS NOT NULL`) を正準として扱う

14. Web の queued follow-up は現在の active run_id に接続され、
    Tool Result → User follow-up → next assistant output を同じ live stream で表示する

15. Rust / Web の full verification が通る
```

この acceptance criteria を最終レビューの基準にする。
