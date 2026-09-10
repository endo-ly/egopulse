# Agent Loop Architecture Refactor Plan

## 1. 背景

現在の `src/agent_loop/` は、Agent の 1 Turn を成立させるための機能が揃っている一方で、`turn.rs` と `tool_phase.rs` に複数の責務が集中している。

特に `turn.rs` には、次の異なる責務が同居している。

- Turn の受付・重複判定
- Config snapshot の固定
- Session の復元
- User Message の永続化
- LLM 呼び出しと Retry
- Agent Loop
- Tool Call の処理
- Tool 実行との接続
- Tool Call / Tool Result の永続化
- Turn の durable state 遷移
- Crash Resume
- Final Message の永続化
- Failure / Uncertain の記録
- Event 通知
- Compaction との接続

`tool_phase.rs` も同様に、実際には以下をまとめて持っている。

- LLM request
- LLM response の分類
- empty response retry
- Tool Call validation
- usage logging / calibration
- Tool Result の構築
- Tool 実行
- read-only Tool の並列実行
- Tool execution ledger
- idempotency 制御

その結果、Agent Loop の本質である次の流れがコード構造から読み取りにくい。

```text
LLM
 ↓
Tool Call
 ↓
Tool
 ↓
Tool Result
 ↓
LLM
```

今回のリファクタでは、現在すでに暗黙的に存在している責務境界を、module・`struct`・`enum` の境界として明示する。

単なるファイル分割ではなく、**「どの状態を誰が所有するか」「どの処理がどの責務に属するか」をコード構造へ反映すること**を目的とする。

---

## 2. ゴール

最終的な責務構造は以下とする。

```text
                process_turn()
                     │
                     ▼
               TurnExecutor
                     │
          ┌──────────┴──────────┐
          │                     │
      Turn Lifecycle        Agent Loop
          │                     │
      accept / resume           LLM
      persist / fail             ↓
      complete                Tool Call
                                ↓
                              Tool
                                ↓
                           Tool Result
                                ↓
                               LLM
```

完成後は、初見の開発者が `src/agent_loop/` を見て次のように理解できる状態にする。

```text
loop.rs
    Agent Loop そのもの

model_step.rs
    LLM を 1 回呼び、結果を分類する

tool_execution.rs
    Tool Call を安全に実行する

turn/
    Agent Loop を durable な 1 Turn として安全に実行する

session.rs / compaction.rs
    会話履歴を支える

prompt_builder.rs
    LLM に渡す指示を構築する
```

また、Agent Loop に属さない共有概念を `agent_loop` から外へ出す。

```text
conversation.rs
    Conversation Context

runtime/scheduled_turn.rs
    Runtime が受付・queue・recoveryする Scheduled Turn
```

---

# 3. 完成後のディレクトリ構造

```text
src/
├── conversation.rs
│
├── runtime/
│   ├── scheduled_turn.rs
│   ├── channel_input.rs
│   ├── turn_scheduler.rs
│   ├── turn_dispatch.rs
│   └── ...
│
└── agent_loop/
    ├── mod.rs
    │
    ├── loop.rs
    ├── model_step.rs
    ├── tool_execution.rs
    │
    ├── turn/
    │   ├── mod.rs
    │   ├── lifecycle.rs
    │   └── persistence.rs
    │
    ├── compaction.rs
    ├── event.rs
    ├── formatting.rs
    ├── guards.rs
    ├── prompt_builder.rs
    ├── prompts/
    ├── session.rs
    ├── session_snapshot.rs
    ├── soul_agents.rs
    └── turn_runtime.rs
```

責務は次の通り。

| Module | 責務 |
|---|---|
| `conversation.rs` | Conversation を識別する共有 Context |
| `runtime/scheduled_turn.rs` | Runtime に投入される Turn と durable representation |
| `agent_loop/turn/mod.rs` | 1 Turn 全体の orchestration |
| `agent_loop/turn/lifecycle.rs` | Turn の durable state 遷移・Resume・Failure |
| `agent_loop/turn/persistence.rs` | Turn 中の Message / Session persistence |
| `agent_loop/loop.rs` | Agent Loop 本体 |
| `agent_loop/model_step.rs` | LLM を 1 回呼び、結果を分類 |
| `agent_loop/tool_execution.rs` | Tool Call を安全に実行 |
| `agent_loop/session.rs` | Session の低レベル操作 |
| `agent_loop/compaction.rs` | Context 圧縮 |
| `agent_loop/prompt_builder.rs` | System Prompt 構築 |
| `agent_loop/event.rs` | Agent 実行 Event / EventEmitter |
| `agent_loop/turn_runtime.rs` | Turn 実行に必要な既存 dependency bundle |

---

# 4. 設計原則

このリファクタでは以下を一貫して適用する。

## 4.1 状態と不変条件を `struct` に閉じ込める

複数の処理が同じ依存や状態を共有する場合、引数を繰り返し渡すのではなく `struct + impl` で所有者を明確にする。

今回導入・整理する主な型:

```text
TurnExecutor
TurnLifecycle
TurnPersistence

AgentLoop
LoopState
AgentLoopResult

ModelRunner
ModelStep

ToolExecutor
```

## 4.2 固定依存と mutable state を分離する

例:

```rust
struct AgentLoop<'a> {
    // Loop 中に固定される依存
}

struct LoopState {
    // iteration ごとに変化する状態
}
```

`AgentLoop` に mutable な会話状態を何でも詰め込まない。

## 4.3 排他的な状態は `enum` で表す

LLM response が、

- Final
- Malformed Tool Call
- Valid Tool Call

のどれかであるなら、複数の boolean ではなく `enum` で表現する。

現在の `ToolPhaseResponse` の方向性は維持し、責務に合う名前へ整理する。

## 4.4 純粋な変換は普通の関数のままにする

状態を持たない処理まで無理に `struct` 化しない。

例:

- response text の sanitize
- retry backoff 計算
- output classification
- format helper

## 4.5 DB 上の `TurnRunState` を正本とする

`Turn<Accepted>` / `Turn<InputCommitted>` のような Typestate は導入しない。

Turn の durable state は crash recovery も含め DB 上に存在しているため、Rust 型との二重管理にはしない。

状態遷移を安全に扱う責務は `TurnLifecycle` に集約する。

---

# 5. `conversation.rs` を新設する

## 5.1 移動対象

現在 `src/agent_loop/mod.rs` にある以下を移動する。

```rust
ConversationScope
SurfaceContext
```

## 5.2 理由

これらは Agent Loop 内部だけの型ではなく、現在すでに以下から共有されている。

- Channel Input
- Runtime
- Pulse
- Session resolution
- Agent Turn execution

したがって `crate::agent_loop::SurfaceContext` という所属は責務と一致していない。

変更後は以下とする。

```rust
use crate::conversation::{ConversationScope, SurfaceContext};
```

## 5.3 移動時に行うこと

- `src/lib.rs` に `conversation` module を追加
- `agent_loop` 内の import を更新
- `runtime` 内の import を更新
- `pulse` 内の import を更新
- `channels` 等の利用箇所を更新
- `SurfaceContext::new`
- `SurfaceContext::session_key`
- `ConversationScope` の `Display`
- Serialize / Deserialize 等の既存実装

を挙動変更なしでそのまま移す。

---

# 6. `runtime/scheduled_turn.rs` を新設する

## 6.1 移動対象

現在 `src/agent_loop/mod.rs` にある以下をまとめて移動する。

```rust
ScheduledTurn

PersistedScheduledTurnV1
SCHEDULED_TURN_VERSION

canonical_request_hash()
serialize_scheduled_turn()
deserialize_scheduled_turn()
```

`CanonicalRequest` のような serialization / hash 用 private type も同時に移す。

## 6.2 理由

現在の実際の処理経路は以下である。

```text
Channel Input
    ↓
durable accept
    ↓
ScheduledTurn
    ↓
TurnScheduler
    ↓
TurnDispatcher
    ↓
Turn execution
```

`ScheduledTurn` は Agent Loop の内部状態ではなく、Runtime が durable に受付し、queue / dispatch / recovery する単位である。

## 6.3 `turn_scheduler.rs` へ直接入れない

`turn_scheduler.rs` はすでに以下を担当している。

- queue
- per-session ordering
- concurrency
- origin tracking
- capacity
- runaway prevention

serialization / hash まで同居させず、`scheduled_turn.rs` を独立させる。

## 6.4 更新対象

主に以下の import を更新する。

```text
runtime/channel_input.rs
runtime/turn_scheduler.rs
runtime/turn_dispatch.rs
agent_loop/turn/*
その他 ScheduledTurn 利用箇所
```

Runtime 側から `agent_loop::ScheduledTurn` を参照する状態をなくす。

---

# 7. `agent_loop/mod.rs` を薄くする

`ConversationScope` / `SurfaceContext` / `ScheduledTurn` 等を外へ移した後、`agent_loop/mod.rs` は module facade にする。

主に以下だけを持たせる。

```rust
pub(crate) mod compaction;
pub(crate) mod event;
pub(crate) mod formatting;
pub(crate) mod guards;
pub(crate) mod loop;
pub(crate) mod model_step;
pub(crate) mod prompt_builder;
pub(crate) mod session;
pub(crate) mod session_snapshot;
pub(crate) mod soul_agents;
pub(crate) mod tool_execution;
pub(crate) mod turn;
pub(crate) mod turn_runtime;
```

必要な public / crate-private entrypoint の re-export は残す。

`mod.rs` 自身に request hash・serialization・Turn execution logic を置かない。

---

# 8. `turn.rs` を `turn/` へ分解する

現在:

```text
src/agent_loop/turn.rs
```

変更後:

```text
src/agent_loop/turn/
├── mod.rs
├── lifecycle.rs
└── persistence.rs
```

最終的に旧 `turn.rs` は削除する。

---

# 9. `turn/mod.rs` — Turn 全体の orchestration

## 9.1 移動対象

現在 `turn.rs` にある以下を基本的にここへ移す。

```text
ask_in_session
send_turn

process_turn
process_turn_with_events
process_turn_with_events_and_snapshot
process_turn_inner

TurnExecutor
PreparedTurn
ActiveTurnGuard

Turn preparation
Config snapshot 固定
LLM provider 解決
System Prompt 構築
Tool Definition 取得
ToolExecutionContext 構築
Channel Context 読み込み
```

## 9.2 `TurnExecutor` は維持する

現在の `TurnExecutor` は、1 Turn 中に共有する以下の依存を保持する構造として意味がある。

```text
TurnRuntime
SurfaceContext
EventEmitter
ConfigSnapshot
```

そのため削除せず、責務を狭める。

責務:

> 1 Turn の開始から終了までの大きな orchestration

最終的に `TurnExecutor::run()` を上から読めば、次が見える状態にする。

```text
Accept Turn
    ↓
Prepare Turn
    ↓
Persist User Input
    ↓
Run Agent Loop
    ↓
Persist Final
    ↓
Complete Turn
```

失敗時:

```text
Error
 ↓
Record Turn Failure
 ↓
return Err
```

## 9.3 `PreparedTurn` は維持する

現在の `PreparedTurn` が保持している、

- `turn_id`
- `chat_id`
- `ToolExecutionContext`
- `system_prompt`
- `channel_llm`
- `tool_defs`
- `tools_json`
- `user_message`
- `input_message_id`
- `config_snapshot`

は「Turn開始時に固定し、そのTurn中で使う値」のまとまりとして妥当。

今回無理に分割しない。

---

# 10. `turn/lifecycle.rs` — `TurnLifecycle`

## 10.1 新設する型

```rust
struct TurnLifecycle<'a> {
    runtime: &'a TurnRuntime,
    scope: ConversationScope,
    turn_id: &'a str,
}
```

正確な field は lifetime / ownership に合わせて調整してよいが、Turn単位の durable state 操作をこの型へ集約する。

## 10.2 移動対象

現在 `turn.rs` にある以下を移す。

```text
TurnAcceptance

resolve_request_key
accept_turn

resume validation
fail_resume_permanently

fail_turn
record_failure_excluding_conflict

mark_output_published
complete_model
begin_tools
complete_tools
complete_turn
```

必要に応じて `TurnRun` のロード等も lifecycle helper としてここへ寄せる。

## 10.3 期待する API

概念的には以下のように使える状態を目標とする。

```rust
lifecycle.model_completed().await?;
lifecycle.tools_started().await?;
lifecycle.output_published().await?;
lifecycle.tools_completed().await?;
lifecycle.complete(final_message_id).await?;
lifecycle.fail(error).await?;
```

現在のように各関数へ毎回同じ `turn_id` / `scope` / `runtime` を渡す構造を減らす。

## 10.4 Resume

`resume_input_committed_turn()` の外部 entrypoint は `turn/mod.rs` に置いてよい。

ただし、現在その中にある以下は lifecycle 側へ移す。

```text
TurnRunState == InputCommitted の確認
scheduled_request_json の検証
output_published の検証
Config fingerprint の検証
input message の存在確認
permanent failure の記録
concurrency conflict の判定
```

構造:

```text
turn/mod.rs
    resume_input_committed_turn
            ↓
turn/lifecycle.rs
    resume validation / durable state handling
            ↓
TurnExecutor
    resume execution
```

---

# 11. `turn/persistence.rs` — `TurnPersistence`

## 11.1 新設する型

概念例:

```rust
struct TurnPersistence<'a> {
    runtime: &'a TurnRuntime,
    context: &'a SurfaceContext,
    turn_id: &'a str,
    chat_id: i64,
}
```

実装上必要なら agent_id / scope 等を field として保持してよい。

目的は、Session / Message persistence に毎回同じTurn情報を渡さないこと。

## 11.2 移動対象

現在 `turn.rs` にある以下を移す。

```text
persist_user_input
persist_user_turn_with_compaction

persist_and_finalize

persist_tool_call_assistant_message
persist_tool_result_messages
```

これらから呼ばれる Turn 固有 persistence helper も同じ責務なら移動する。

## 11.3 `session.rs` との境界

`turn/persistence.rs`:

> Turnとして「何を保存するか」を決める

`session.rs`:

> Sessionへ「どう保存するか」の低レベル操作

という関係にする。

```text
TurnPersistence
     ↓
session.rs
```

`session.rs` 自体を今回別物へ作り替えない。

---

# 12. `execute_and_persist_tools()` を解体する

現在の `execute_and_persist_tools()` は、

```text
Assistant Tool Call を永続化
        ↓
Tool を実行
        ↓
Tool Result を構築
        ↓
Tool Result を永続化
```

を一つの関数で持っている。

今回この関数は廃止する。

ただし**処理順序は変更しない**。

変更後:

```text
AgentLoop
    ↓
TurnPersistence
    persist assistant tool call
    ↓
TurnLifecycle
    tools_started
    ↓
ToolExecutor
    execute
    ↓
TurnPersistence
    persist tool results
    ↓
TurnLifecycle
    tools_completed
    ↓
次の iteration
```

重要なのは「execute と persist の所有者を分ける」ことであり、Crash Safety を変えることではない。

---

# 13. `loop.rs` — `AgentLoop`

ここを今回の中心とする。

## 13.1 新設する型

```rust
struct AgentLoop<'a> {
    // Loop 中に固定される依存
}

struct LoopState {
    // iteration ごとに変化する状態
}

struct AgentLoopResult {
    // Loop 完了時に Turn 側へ返す結果
}
```

---

# 14. `LoopState`

現在の `TurnLoopState` を `loop.rs` へ移し、`LoopState` へ rename する。

現在保持している以下は維持する。

```rust
messages: Arc<Vec<Message>>
session_revision: Option<i64>
retry_messages: Option<Arc<Vec<Message>>>
declarative_retry_attempted: bool
```

既存の、

```text
request_messages()
reset_retry_guards_after_tool_phase()
```

に相当する状態操作も `impl LoopState` に残す。

固定依存は `LoopState` に入れない。

---

# 15. `AgentLoop`

`AgentLoop` は Agent Loop 中に固定される依存と協調オブジェクトを保持する。

概念的には以下。

```rust
struct AgentLoop<'a> {
    model: ModelRunner<'a>,
    tools: ToolExecutor<'a>,
    lifecycle: TurnLifecycle<'a>,
    persistence: TurnPersistence<'a>,
    // compaction に必要な既存依存
    // event emitter 等
}
```

厳密な field 構成は、不要な wrapper を作らず最小になるよう実装時に調整する。

重要なのは次の分離。

```text
AgentLoop
    Loop をどう実行するか

LoopState
    Loop が今どの状態か
```

---

# 16. `AgentLoopResult`

Agent Loop が final response を得たら、Turn completion そのものまで行わず、Turn側へ結果を返す。

概念例:

```rust
struct AgentLoopResult {
    final_content: String,
    reasoning_content: Option<String>,
    messages: Arc<Vec<Message>>,
    session_revision: Option<i64>,
}
```

流れ:

```text
AgentLoop
    ↓
AgentLoopResult
    ↓
TurnPersistence
    final message を保存
    ↓
TurnLifecycle
    output_published / complete
```

現在 `finish_turn()` が一つで持っている、

- final response persistence
- output published
- Turn complete

を Turn 側の責務として整理する。

---

# 17. `loop.rs` へ移す既存処理

現在 `turn.rs` から主に以下を移す。

```text
run_model_loop
TurnLoopState
TurnAction
evaluate_end_turn
evaluate_malformed_response
request_messages_for_iteration
```

`PhaseOutcome` は新構造で不要になるなら削除する。

削除する場合、単なる rename で別の中間 enum を増やさない。

Agent Loop の制御が `ModelStep` と `AgentLoopResult` で明確に書けるなら、それを優先する。

---

# 18. Loop Policy も `loop.rs` へ移す

現在 `tool_phase.rs` にある以下は Tool Execution ではなく Loop Policy なので `loop.rs` へ移す。

```text
MAX_TOOL_ITERATIONS
FINAL_RESPONSE_WARNING_ITERATION
FINAL_RESPONSE_WARNING_GUARD
FINAL_RESPONSE_GUARD
messages_for_iteration
```

通常TurnとPulseの両方から共有されている挙動は維持する。

Pulseから必要な項目は `pub(crate)` で参照できるようにする。

---

# 19. `AgentLoop::run()` の目標形

実装詳細はhelperへ委譲し、中心を読めば次の流れが見えること。

```rust
for iteration in 1..=MAX_TOOL_ITERATIONS {
    let step = model.run(...).await?;

    match step {
        ModelStep::Final(response) => {
            // final 判定
            // 必要なら corrective retry
            // 完了なら AgentLoopResult を返す
        }

        ModelStep::MalformedToolCalls(response) => {
            // 現行ルールで retry / final を判断
        }

        ModelStep::ToolCalls(assistant_phase) => {
            // assistant tool call を保存
            // lifecycle state を進める
            // ToolExecutor で実行
            // tool results を保存
            // lifecycle state を進める
        }
    }

    // 現行条件で compaction
}
```

DB write の詳細、Tool Ledger の詳細、LLM transport retry の詳細がこの関数へ展開されないようにする。

---

# 20. `model_step.rs` — `ModelRunner`

現在の `tool_phase.rs` 前半と、`turn.rs` の LLM retry 処理を統合する。

## 20.1 新設する型

```rust
struct ModelRunner<'a> {
    llm: &'a dyn LlmProvider,
    system_prompt: &'a str,
    tools: Option<Arc<Vec<ToolDefinition>>>,
    // request metadata / usage logging に必要な依存
}
```

正確な field は既存依存に合わせる。

## 20.2 責務

> LLMを1回呼び、そのiterationの結果をAgent Loopが扱える形へ変換する

「1回」には現在同一iteration内部で行っている transport retry / empty response recovery を含む。

Agent Loop 自体の繰り返しは `loop.rs` が担当する。

---

# 21. `ModelStep` enum

現在の、

```rust
enum ToolPhaseResponse {
    Final(MessagesResponse),
    MalformedToolCalls(MessagesResponse),
    ToolCalls(AssistantToolPhase),
}
```

の方向性を維持し、責務に合わせて `ModelStep` へ rename する。

```rust
enum ModelStep {
    Final(MessagesResponse),
    MalformedToolCalls(MessagesResponse),
    ToolCalls(AssistantToolPhase),
}
```

`is_final` / `has_tools` / `is_malformed` のような複数 boolean へ戻さない。

---

# 22. `model_step.rs` へ移すもの

## `turn.rs` から

```text
ModelRequestError
send_model_request_with_retry
llm_retry_backoff
LLM request retry 関連 helper
```

## `tool_phase.rs` から

```text
ToolPhaseRequest
ToolPhaseRequestError

send_tool_phase_request
send_tool_phase_request_with_empty_retry

filter_valid_tool_calls
tool_phase_response_is_empty

empty_response_after_retry
empty_response_after_published_output

build_assistant_tool_phase

log_llm_usage
```

型名は責務に合わせて以下のように整理してよい。

```text
ToolPhaseRequest      → ModelStepRequest
ToolPhaseRequestError → ModelStepError
ToolPhaseResponse     → ModelStep
```

ただし名称変更自体を目的に不要なwrapperを増やさない。

---

# 23. `model_step.rs` が維持する挙動

以下を変更しない。

- `send_message_streaming` を使うこと
- Streaming delta handling
- 同一iteration内の LLM retry 回数
- Retry-After
- exponential backoff
- empty assistant response の guard retry
- thinking-only response の扱い
- output がすでに publish された後の unsafe retry 禁止
- Tool Call の empty name / id validation
- duplicate Tool Call ID の扱い
- Token estimate
- Usage calibration
- Usage logging

---

# 24. `tool_execution.rs` — `ToolExecutor`

現在 `tool_phase.rs` 後半を移す。

## 24.1 新設する型

```rust
struct ToolExecutor<'a> {
    runtime: &'a TurnRuntime,
    context: &'a ToolExecutionContext,
    hooks: ToolExecutionHooks<'a>,
}
```

必要なら `assistant_message_id` を `execute()` 引数にするかfieldにするか、lifetimeが自然な方を選ぶ。

## 24.2 呼び出し側

概念的には以下まで単純化する。

```rust
let outcomes = tool_executor
    .execute(assistant_message_id, tool_calls)
    .await?;
```

---

# 25. `tool_execution.rs` へ移すもの

現在 `tool_phase.rs` から主に以下を移す。

```text
ToolExecutionHooks
ExecutedToolCall
ToolResultPhase

execute_tool_calls
execute_single_tool
read_only_flags

claim_tool_slot
record_tool_outcome

build_tool_result_phase
summarize_tool_result_messages
```

`MAX_TOOL_RESULT_TEXT_CHARS` も Tool Result / event preview の責務側へ置く。

Tool実行に直接必要な formatting helper の利用は維持する。

---

# 26. Tool Execution の安全性は一切変えない

現行の以下を維持する。

```text
read-only Tool
    → 現行条件で並列実行

side-effect Tool
    → 現行順序で実行
    → execution 前に Ledger claim

ClaimOutcome::Acquired
    → 実行
    → outcome 記録

ClaimOutcome::Reused
    → 保存済み結果を再利用し再実行しない

ClaimOutcome::Blocked
    → 現行の blocked result を返す
```

Tool Call ID を `ToolExecutionContext` へbindする挙動も維持する。

Tool Result Message の、

```rust
tool_call_id: Some(tool_call.id.clone())
```

も維持する。

---

# 27. `tool_phase.rs` は最終的に削除する

分解は以下。

```text
tool_phase.rs
    │
    ├─ LLM request / response classification
    │      → model_step.rs
    │
    ├─ Loop limit / final guards
    │      → loop.rs
    │
    └─ Tool execution / ledger
           → tool_execution.rs
```

旧 `tool_phase.rs` を compatibility wrapper として残さない。

---

# 28. Pulse との共有を維持する

現在 Pulse Runner は `tool_phase.rs` の以下を共有している。

```text
MAX_TOOL_ITERATIONS
ToolExecutionHooks
ToolPhaseRequest
ToolPhaseResponse
build_tool_result_phase
messages_for_iteration
send_tool_phase_request_with_empty_retry
execute_tool_calls
```

リファクタ後は参照先を以下へ更新する。

```text
loop.rs
    MAX_TOOL_ITERATIONS
    messages_for_iteration
    final response guard policy

model_step.rs
    ModelRunner / ModelStep
    1 model step の実行

tool_execution.rs
    ToolExecutor
    ToolExecutionHooks
    ToolResultPhase
```

PulseのLoopそのものを今回 `AgentLoop` へ統合しない。

Pulseは通常Turnと persistence contract が異なるため、既存のPulse runner構造は保ちつつ、shared primitive の参照先だけ整理する。

Pulse の以下を変更しない。

- PULSE_OK classification
- normal sessionへpersistしない契約
- notify時のtool phase保持
- Tool実行
- iteration上限
- empty response handling

---

# 29. `event.rs` へ `EventEmitter` を移す

現在 `turn.rs` 内にある、

```rust
EventEmitter
```

を `agent_loop/event.rs` へ移す。

```text
event.rs
├─ AgentEvent
└─ EventEmitter
```

とする。

Normal Turn の Tool event は、`EventEmitter` から `ToolExecutionHooks` を構築して `ToolExecutor` へ渡す。

Pulseは従来通り no-op hooks を利用できる。

`ToolExecutor` 自体をNormal Turn専用の `AgentEvent` に直接結合させない。

---

# 30. `TurnRuntime` は現在の役割を維持する

既存 `TurnRuntime` は、

> `AppState` 全体ではなく、Turn / Agent execution に必要な依存だけを渡す

ための boundary として機能している。

今回の新しい `TurnExecutor` / `AgentLoop` / `ModelRunner` / `ToolExecutor` / `TurnLifecycle` / `TurnPersistence` は、必要な参照だけをfieldとして持つ。

一方で、

```text
AgentLoopDeps
ModelDeps
ToolDeps
LifecycleDeps
```

のような新しい dependency bundle 群を追加しない。

今回の `struct` 化は状態と責務の所有を表すために行い、DI layerを増やすためには行わない。

---

# 31. 現行コード → 移動先一覧

| 現在 | 移動先 |
|---|---|
| `agent_loop/mod.rs::ConversationScope` | `src/conversation.rs` |
| `agent_loop/mod.rs::SurfaceContext` | `src/conversation.rs` |
| `agent_loop/mod.rs::ScheduledTurn` | `src/runtime/scheduled_turn.rs` |
| `PersistedScheduledTurnV1` | `src/runtime/scheduled_turn.rs` |
| `SCHEDULED_TURN_VERSION` | `src/runtime/scheduled_turn.rs` |
| `CanonicalRequest` | `src/runtime/scheduled_turn.rs` |
| `canonical_request_hash` | `src/runtime/scheduled_turn.rs` |
| `serialize_scheduled_turn` | `src/runtime/scheduled_turn.rs` |
| `deserialize_scheduled_turn` | `src/runtime/scheduled_turn.rs` |
| `turn.rs::TurnExecutor` | `agent_loop/turn/mod.rs` |
| `turn.rs::PreparedTurn` | `agent_loop/turn/mod.rs` |
| `turn.rs::ActiveTurnGuard` | `agent_loop/turn/mod.rs` |
| `turn.rs::ask_in_session` | `agent_loop/turn/mod.rs` |
| `turn.rs::send_turn` | `agent_loop/turn/mod.rs` |
| `turn.rs::process_turn*` | `agent_loop/turn/mod.rs` |
| Turn preparation helpers | `agent_loop/turn/mod.rs` |
| `turn.rs::TurnAcceptance` | `agent_loop/turn/lifecycle.rs` |
| `turn.rs::accept_turn` | `agent_loop/turn/lifecycle.rs` |
| `turn.rs::resolve_request_key` | `agent_loop/turn/lifecycle.rs` |
| Resume validation | `agent_loop/turn/lifecycle.rs` |
| `fail_resume_permanently` | `agent_loop/turn/lifecycle.rs` |
| `fail_turn` | `agent_loop/turn/lifecycle.rs` |
| `record_failure_excluding_conflict` | `agent_loop/turn/lifecycle.rs` |
| `mark_output_published` | `TurnLifecycle` |
| `complete_model` | `TurnLifecycle` |
| `begin_tools` | `TurnLifecycle` |
| `complete_tools` | `TurnLifecycle` |
| `complete_turn` | `TurnLifecycle` |
| `persist_user_input` | `agent_loop/turn/persistence.rs` |
| `persist_user_turn_with_compaction` | `agent_loop/turn/persistence.rs` |
| `persist_and_finalize` | `agent_loop/turn/persistence.rs` |
| `persist_tool_call_assistant_message` | `agent_loop/turn/persistence.rs` |
| `persist_tool_result_messages` | `agent_loop/turn/persistence.rs` |
| `execute_and_persist_tools` | 廃止し、Persistence / ToolExecutor / Lifecycleへ分解 |
| `turn.rs::run_model_loop` | `AgentLoop::run` |
| `TurnLoopState` | `LoopState` |
| `TurnAction` | `agent_loop/loop.rs` |
| `PhaseOutcome` | 新構造で不要なら削除 |
| `evaluate_end_turn` | `agent_loop/loop.rs` |
| `evaluate_malformed_response` | `agent_loop/loop.rs` |
| `request_messages_for_iteration` | `agent_loop/loop.rs` |
| `MAX_TOOL_ITERATIONS` | `agent_loop/loop.rs` |
| final-response guard constants | `agent_loop/loop.rs` |
| `messages_for_iteration` | `agent_loop/loop.rs` |
| `ModelRequestError` | `agent_loop/model_step.rs` |
| `send_model_request_with_retry` | `ModelRunner` / `model_step.rs` |
| `llm_retry_backoff` | `agent_loop/model_step.rs` |
| `ToolPhaseRequest` | `ModelStepRequest` 相当 / `model_step.rs` |
| `ToolPhaseRequestError` | `ModelStepError` 相当 / `model_step.rs` |
| `ToolPhaseResponse` | `ModelStep` |
| `filter_valid_tool_calls` | `agent_loop/model_step.rs` |
| `send_tool_phase_request*` | `ModelRunner` / `model_step.rs` |
| `build_assistant_tool_phase` | `agent_loop/model_step.rs` |
| `log_llm_usage` | `agent_loop/model_step.rs` |
| `ToolExecutionHooks` | `agent_loop/tool_execution.rs` |
| `ExecutedToolCall` | `agent_loop/tool_execution.rs` |
| `ToolResultPhase` | `agent_loop/tool_execution.rs` |
| `execute_tool_calls` | `ToolExecutor` |
| `execute_single_tool` | `ToolExecutor` private method/helper |
| `read_only_flags` | `agent_loop/tool_execution.rs` |
| `claim_tool_slot` | `agent_loop/tool_execution.rs` |
| `record_tool_outcome` | `agent_loop/tool_execution.rs` |
| `build_tool_result_phase` | `agent_loop/tool_execution.rs` |
| `EventEmitter` | `agent_loop/event.rs` |

---

# 32. テスト再編の基本方針

テストだけを先に別ファイルへ移して `turn.rs` の行数を減らす作業は行わない。

責務を移動するとき、その責務を検証しているテストも一緒に所有先へ移す。

また、既存テストを機械的にすべて温存しない。

各テストについて、

```text
このテストはどのInvariantを守っているか
```

を確認し、新しい責務単位で必要性を再評価する。

---

# 33. `loop.rs` のテスト

主にAgent Loop全体の制御を検証する。

残す / 移す代表ケース:

- LLM → Tool → LLM → Final
- 複数 Tool iteration
- Tool Result が次のLLM requestへ入る
- iteration hard limit
- final response warning window
- final response guard
- declarative-only response corrective retry
- malformed Tool Call responseからの既存判定
- Tool phase後に retry guard がresetされる
- Tool Result後の compaction
- first iterationだけChannel Contextが入ること

詳細な LLM transport retry はここで重複検証しない。

---

# 34. `model_step.rs` のテスト

1回のModel Stepに関する契約を検証する。

- Final response分類
- valid Tool Call分類
- malformed Tool Call分類
- empty name / id の除外
- duplicate Tool Call ID の既存挙動
- empty response retry
- thinking-only response
- retry後もemptyならerror
- delta公開後のunsafe retry禁止
- API retry
- Retry-After
- exponential backoff contract
- Streaming delta
- usage logging
- usage calibration
- Tool Definition が各LLM requestへ渡ること

---

# 35. `tool_execution.rs` のテスト

Tool execution単体の契約を検証する。

- read-only Tool群の並列実行
- side-effect Toolの順序維持
- read-only / side-effect混在時の現行順序
- side-effect Toolは実行前にLedger claim
- `ClaimOutcome::Acquired`
- `ClaimOutcome::Reused`
- `ClaimOutcome::Blocked`
- outcome記録
- Tool error
- Tool Result Message生成
- `tool_call_id` 維持
- ToolExecutionHooks の start / result
- Tool Call ID の `ToolExecutionContext` bind

---

# 36. `turn/lifecycle.rs` のテスト

Turn durable stateの契約を検証する。

- fresh Turn acceptance
- completed Turn replay
- in-progress duplicate
- terminal Turn handling
- request_key handling
- Resume成功条件
- Resume対象state不一致
- scheduled payload欠落
- invalid scheduled payload
- output published済みResume拒否
- Config fingerprint mismatch
- input message欠落
- permanent resume failure記録
- concurrency conflict
- output published marking
- model complete
- tools begin / complete
- final complete
- failed / uncertain判定
- conflict時にwinnerをfailさせないこと

---

# 37. `turn/persistence.rs` のテスト

Turn中の永続化契約を検証する。

- User Message persistence
- deterministic input message id
- input_committedとの既存transaction contract
- pre-LLM compaction
- Assistant Tool Call persistence
- Tool Result persistence
- Tool Call → Tool Result の順序
- `parent_message_id`
- Session revision
- Final Message persistence
- reasoning_content の保持
- stored session snapshotとの整合

---

# 38. `turn/mod.rs` のテスト

ここにはTurn全体を通した代表的なIntegration Testだけを残す。

例:

```text
process_turn
    ↓
input persist
    ↓
LLM Tool Call
    ↓
Tool execution
    ↓
LLM Final
    ↓
final persist
    ↓
Turn Completed
```

さらに、Turn全体でしか確認できない代表的な失敗ケースが必要なら少数残す。

各内部分岐をここで再度網羅しない。

---

# 39. `conversation.rs` / `runtime/scheduled_turn.rs` のテスト

既存 `agent_loop/mod.rs` にある関連テストは型と一緒に移す。

## `conversation.rs`

- `session_key`
- `ConversationScope` serialization / display 等、既存契約

## `runtime/scheduled_turn.rs`

- canonical request hash
- field orderに依存しない既存hash contract
- serialize / deserialize
- version validation
- durable payload round-trip

---

# 40. Pulse のテスト

`tool_phase.rs` 分割に伴いimportを更新する。

Pulse側でshared primitiveの挙動をNormal Turnと重複して細かく再テストしない。

Pulse固有契約を中心に残す。

- PULSE_OK
- Notify classification
- Tool phase collection
- Normal Sessionへpersistしないこと
- Pulse固有のLoop完了

Model Step / ToolExecutor の詳細はそれぞれのmodule testへ寄せる。

---

# 41. Test Double の整理

現在 `turn.rs` には複数のProvider Test Doubleがある。

例:

```text
FakeProvider
FailingProvider
RecordingProvider
DeltaEmittingProvider
DeltaThenFailProvider
DeltaThenThinkingProvider
```

責務移動時に、役割が重複しているものは統合する。

例えば、

```rust
struct ScriptedProvider {
    responses: ...,
    delays: ...,
    seen_messages: ...,
    seen_systems: ...,
}
```

のような一つの汎用Test Providerで置き換えられるなら置き換える。

一方、

- deltaを出してからfailする
- deltaを出してthinking-onlyを返す

など、特殊挙動そのものがテスト対象のProviderは専用型として残してよい。

Test abstractionを増やすこと自体を目的にしない。

---

# 42. 不要テストを削除する基準

以下は削除または統合候補。

## 42.1 同じInvariantを複数階層で検証している

例:

```text
model_step unit
loop unit
turn integration
```

の3箇所で同じempty-response分岐を検証している場合、

- 詳細挙動 → `model_step`
- end-to-end → 必要なら代表1ケース

へ整理する。

## 42.2 入力値だけ違う同一ケース

table-driven化できるなら統合する。

## 42.3 private helperの実装詳細だけを固定している

外部から観測できる契約で十分なら削除する。

## 42.4 旧module境界を前提にしたテスト

`tool_phase` という旧構造自体を検証しているものは、新しい責務へ移すか削除する。

## 42.5 巨大fixtureの割に追加の保証がない

他ケースとの差分となるInvariantを説明できないなら削除候補。

削除前には、そのテストが守っていた契約を必ず確認する。

単純に「似ている」「テスト数が多い」という理由だけでは削除しない。

---

# 43. 絶対に維持する Agent Loop Invariants

- 各LLM requestで Tool Definition を渡す
- Tool Call があればLoopを継続する
- Tool Callのない有効なfinal responseで終了する
- `MAX_TOOL_ITERATIONS` を変更しない
- final response warning iterationを変更しない
- final response guardの内容・タイミングを意図せず変えない
- first iterationのChannel Context injectionを維持する
- retry_messagesの意味を維持する
- declarative-only retryの挙動を維持する
- malformed Tool Call responseの既存処理を維持する
- Tool phase後のcompactionを維持する

---

# 44. 絶対に維持する Model Invariants

- streaming path
- delta event
- LLM retry回数
- Retry-After
- exponential backoff
- empty response guard
- thinking-only detection
- output publish後のunsafe retry禁止
- Tool Call validation
- duplicate Tool Call ID処理
- usage estimate
- usage calibration
- usage logging
- provider / model selection
- Config snapshot固定

---

# 45. 絶対に維持する Tool Invariants

- Assistant Tool Callを永続化してからToolを実行する
- side-effect ToolはLedger claim後に実行する
- read-only Toolの並列実行条件を変えない
- side-effect Toolの順序を変えない
- `ClaimOutcome::Reused` で再実行しない
- `ClaimOutcome::Blocked` の既存挙動を維持する
- Tool outcome recordingを維持する
- Tool Resultに元の `tool_call_id` を保持する
- Tool Call IDをexecution contextへbindする
- Event hookの発火タイミングを変えない

---

# 46. 絶対に維持する Turn Invariants

- Config snapshotをTurn開始時に固定する
- request_keyによるidempotent acceptance
- completed Turnのsaved final replay
- in-progress duplicateを二重実行しない
- terminal Turnを再実行しない
- User Input persistenceと`input_committed`の既存atomic contract
- `input_committed`からのCrash Resume
- Resume時にUser Messageを再persistしない
- Resume時にpre-LLM compactionを再実行しない
- Config fingerprint validation
- input message validation
- output_published
- failed / uncertain semantics
- concurrency conflict semantics
- final message persistence
- Turn complete
- failure時のorigin terminationとの既存atomic contract

---

# 47. 絶対に維持する Session / Persistence Invariants

- Session revisionの意味を変えない
- optimistic revision conflictの扱いを変えない
- Tool Call assistant messageとTool Resultの順序を変えない
- parent message relationshipを変えない
- Session Snapshot形式を変えない
- Compactionのアルゴリズム・thresholdを変えない
- Secret / Normal scopeの保存先を変えない

---

# 48. 絶対に維持する Runtime / ScheduledTurn Invariants

`ScheduledTurn` の所属を移しても以下の挙動は変えない。

```text
Channel Input
    ↓
Config snapshot固定
    ↓
request_key / origin_id確定
    ↓
durable acceptance
    ↓
Scheduler
    ↓
Dispatcher recovery
    ↓
Turn execution
```

- durable accept前後の順序
- request hash
- serialized payload
- version
- origin reservation
- queue capacity behavior
- scheduler dedup
- dispatcher redispatch
- recovery時のdeserialize
- authoritative Turn IDの反映

はそのまま維持する。

---

# 49. 実装手順

大規模な一括書き換えで最後にまとめて直すのではなく、責務単位で移し、その都度testを通す。

ただし「テストだけ先に別ファイルへ移す」段階は作らない。

---

## Phase 1 — Conversation / ScheduledTurn の所有境界を修正

### 作業

新設:

```text
src/conversation.rs
src/runtime/scheduled_turn.rs
```

移動:

```text
ConversationScope
SurfaceContext

ScheduledTurn
PersistedScheduledTurnV1
SCHEDULED_TURN_VERSION
CanonicalRequest
canonical_request_hash
serialize_scheduled_turn
deserialize_scheduled_turn
```

関連importをすべて更新する。

### テスト

関連テストも型と同時に新moduleへ移す。

### 完了条件

- Runtime / Pulse が `agent_loop::SurfaceContext` に依存していない
- Scheduler / Dispatcher が `agent_loop::ScheduledTurn` に依存していない
- `agent_loop/mod.rs` からこれらの定義が消えている
- 全テスト通過

---

## Phase 2 — `tool_phase.rs` の LLM 部分を `model_step.rs` へ移す

### 新設

```text
agent_loop/model_step.rs
```

### 導入

```text
ModelRunner
ModelStep
```

### 移動

- Model request
- response分類
- empty response handling
- Tool Call validation
- usage logging / calibration
- LLM retry
- Retry-After / backoff

を移す。

### テスト

Model Step固有テストも同時に移す。

重複テストを整理する。

### 完了条件

- LLMを1回呼ぶ処理が `model_step.rs` だけで追える
- Tool execution logicが `model_step.rs` に入っていない
- Pulseが新しいModel Step APIを利用できる
- 全テスト通過

---

## Phase 3 — Tool Execution を `tool_execution.rs` へ移す

### 新設

```text
agent_loop/tool_execution.rs
```

### 導入

```text
ToolExecutor
```

### 移動

- ToolExecutionHooks
- ExecutedToolCall
- ToolResultPhase
- read-only判定
- parallel execution
- single Tool execution
- Ledger claim
- outcome record
- Tool Result construction

を移す。

### テスト

Tool execution固有テストも同時に移す。

### 完了条件

- Tool実行の安全性が `tool_execution.rs` だけで追える
- Session persistence logicが入っていない
- Normal Turn / Pulseの両方から利用できる
- 全テスト通過

---

## Phase 4 — Loop Policy を `loop.rs` へ移す

### 新設 / 拡張

```text
agent_loop/loop.rs
```

### まず移すもの

```text
MAX_TOOL_ITERATIONS
FINAL_RESPONSE_WARNING_ITERATION
FINAL_RESPONSE_WARNING_GUARD
FINAL_RESPONSE_GUARD
messages_for_iteration
```

Pulseのimportを更新する。

この段階ではまだ `run_model_loop` の完全移動を行わなくてもよいが、Loop policyの所有先を先に正す。

### 完了条件

- Tool moduleがLoop上限やfinal-response policyを所有していない
- Pulse / Turnが同じpolicyを共有
- 全テスト通過

---

## Phase 5 — `turn/lifecycle.rs` を抽出

### 新設

```text
agent_loop/turn/lifecycle.rs
```

### 導入

```text
TurnLifecycle
```

### 移動

- acceptance
- request key
- durable state transition
- output_published
- failure
- uncertain
- Resume validation
- completion

を移す。

### テスト

Lifecycle関連テストも同時に移す。

重複ケースを整理する。

### 完了条件

- Turn stateのDB操作を追うとき `lifecycle.rs` が中心になる
- LLM / Tool executionの詳細が入っていない
- 全テスト通過

---

## Phase 6 — `turn/persistence.rs` を抽出

### 新設

```text
agent_loop/turn/persistence.rs
```

### 導入

```text
TurnPersistence
```

### 移動

- User Input persistence
- pre-LLM persistence / compaction接続
- Assistant Tool Call persistence
- Tool Result persistence
- Final Message persistence

を移す。

### `execute_and_persist_tools` を解体

以下の順序を保ったまま分解する。

```text
persist assistant tool call
    ↓
begin tools
    ↓
execute tools
    ↓
persist tool result
    ↓
complete tools
```

### テスト

Persistence関連テストを同時に移す。

### 完了条件

- Turn固有のMessage保存が `persistence.rs` から追える
- Tool executionの中身を持っていない
- 全テスト通過

---

## Phase 7 — `AgentLoop` / `LoopState` / `AgentLoopResult` を導入

### 移動

現在 `turn.rs` の、

```text
run_model_loop
TurnLoopState
TurnAction
evaluate_end_turn
evaluate_malformed_response
request_messages_for_iteration
```

を `loop.rs` へ移す。

### 導入

```text
AgentLoop
LoopState
AgentLoopResult
```

### `PhaseOutcome`

新しい制御構造で不要なら削除する。

単なるrenameのための新しい中間型は作らない。

### メインループ

`AgentLoop::run()` を読めば、

```text
Model Step
  ↓
Final / ToolCalls / Malformed
  ↓
必要な処理
  ↓
Compaction
  ↓
次のModel Step
```

が見える状態にする。

### テスト

Agent Loop制御テストを同時に移す。

### 完了条件

- Agent Loop本体が `loop.rs` に存在
- Turn durable stateの具体的DB操作がメインループへ展開されていない
- Tool Ledgerの具体処理がメインループへ展開されていない
- 全テスト通過

---

## Phase 8 — `turn/mod.rs` を最終形へ整理

旧 `turn.rs` の残りを `turn/mod.rs` へ移す。

`TurnExecutor` を最終形へ整理する。

目標:

```text
accept
 ↓
prepare
 ↓
persist input
 ↓
AgentLoop::run
 ↓
persist final
 ↓
complete
```

Resume pathも、

```text
resume validation
 ↓
prepare
 ↓
load committed session
 ↓
AgentLoop::run
 ↓
persist final
 ↓
complete
```

と大きな流れが見えること。

旧 `turn.rs` を削除する。

---

## Phase 9 — `EventEmitter` を `event.rs` へ移す

`AgentEvent` と `EventEmitter` の所有先を揃える。

Normal Turnから `ToolExecutionHooks` への変換もここか `turn/mod.rs` の小さいhelperで行う。

Pulseとの共通ToolExecutorを壊さない。

---

## Phase 10 — `tool_phase.rs` を完全削除

全利用箇所が、

```text
loop.rs
model_step.rs
tool_execution.rs
```

へ移ったことを確認して削除する。

Compatibility re-exportだけの旧moduleは残さない。

---

## Phase 11 — テスト棚卸し

責務移動が終わった後、新しいmodule単位でテスト全体を確認する。

行うこと:

- 重複ケース削除
- table-driven化
- Test Double統合
- private implementation固定テスト削除
- Integration Testの削減
- 新しい境界で不足したInvariant testの追加

単純な件数削減は目的にしない。

---

## Phase 12 — ドキュメント更新

新しいコード構造と食い違う文書を更新する。

特に以下を確認する。

```text
docs/architecture.md
docs/session-lifecycle.md
AGENTS.md
README / 開発者向け構造説明
```

更新するのは今回変更した責務・ファイル構造に関する記述だけ。

Agent Loop / Turn Lifecycle / Runtime Scheduling の境界が文書でもコードと一致する状態にする。

---

# 50. 実装中の判断基準

新しい処理の置き場所に迷った場合、以下で判断する。

## `turn/mod.rs`

「1 Turnをどう進めるか」

## `turn/lifecycle.rs`

「Turnのdurable stateをどう進めるか」

## `turn/persistence.rs`

「Turnの会話内容をいつ・何として保存するか」

## `loop.rs`

「LLMとToolをどう繰り返してFinalへ到達するか」

## `model_step.rs`

「LLMを1回呼んだ結果は何か」

## `tool_execution.rs`

「Tool Callをどう安全に実行してResultへするか」

## `session.rs`

「Sessionをどう読み書きするか」

## `runtime/scheduled_turn.rs`

「Runtimeへ投入・永続化されるTurn requestとは何か」

## `conversation.rs`

「どのconversationで実行しているか」

この説明に当てはまらない責務を無理に既存moduleへ押し込まない。

---

# 51. 避けるべきリファクタ

今回の作業中に以下へ逸れない。

- ファイル行数だけを基準にさらに細かく分割する
- 何でも `struct` にする
- 何でも Trait 化する
- Agent Frameworkのような汎用抽象化を作る
- DB stateをTypestateでも二重管理する
- Runtime全体のDI architectureを作り直す
- PulseをNormal Turnと同一Loopへ無理に統合する
- `session.rs` / `compaction.rs` を今回の理由なく再設計する
- 挙動変更を「リファクタのついで」に混ぜる
- テストを先に移動して見かけの行数だけ減らす

---

# 52. レビュー時の重点確認

PRレビューでは特に以下を確認する。

1. `loop.rs` を読めばAgent Loopの全体像が分かるか
2. `TurnExecutor` がTurn全体のorchestrationに限定されているか
3. `TurnLifecycle` にdurable state操作が集約されているか
4. `TurnPersistence` と `ToolExecutor` が混ざっていないか
5. `ModelRunner` がTool executionを持っていないか
6. `ToolExecutor` がSession persistenceを持っていないか
7. `AgentLoop` と `LoopState` の固定依存 / mutable stateが分離されているか
8. 排他的状態がbooleanの組み合わせに戻っていないか
9. `tool_phase.rs` の責務が新moduleへ正しく分配されているか
10. Pulseのshared behaviorが壊れていないか
11. Assistant Tool Call persist → Tool execute → Tool Result persist の順序が維持されているか
12. Tool Ledger claimがexecutionより前に維持されているか
13. Resume / failed / uncertain / conflict semanticsが変わっていないか
14. Runtimeが `agent_loop::ScheduledTurn` に依存していないか
15. Channel / Pulseが `agent_loop::SurfaceContext` に依存していないか
16. 不要な `pub(crate)` が増えていないか
17. テスト削除時にInvariantが失われていないか

---

# 53. 最終的なコードの読み方

完成後、コードは次の順で読めることを目標とする。

```text
src/conversation.rs
    ↓
Conversation Context

src/runtime/scheduled_turn.rs
    ↓
Runtimeへ渡るTurn

src/agent_loop/turn/mod.rs
    ↓
1 Turn全体

src/agent_loop/loop.rs
    ↓
Agent Loop本体

src/agent_loop/model_step.rs
    ↓
LLM 1回

src/agent_loop/tool_execution.rs
    ↓
Tool実行

src/agent_loop/turn/lifecycle.rs
    ↓
durable Turn state

src/agent_loop/turn/persistence.rs
    ↓
Message / Session persistence
```

Agent Loopだけを理解したい人は `loop.rs` → `model_step.rs` → `tool_execution.rs` を読めばよい。

Turnの安全性を理解したい人は `turn/mod.rs` → `lifecycle.rs` → `persistence.rs` を読めばよい。

Runtimeの受付・queue・recoveryを理解したい人は `runtime/scheduled_turn.rs` → `channel_input.rs` → `turn_scheduler.rs` → `turn_dispatch.rs` を読めばよい。

この読み分けが成立することが、今回のリファクタの最終ゴールである。

---

# 54. 完了条件

以下をすべて満たして完了とする。

- `src/conversation.rs` が存在し、`SurfaceContext` / `ConversationScope` を所有している
- `src/runtime/scheduled_turn.rs` が存在し、Scheduled Turnとdurable representationを所有している
- `agent_loop/mod.rs` がmodule facadeとして薄くなっている
- 旧 `agent_loop/turn.rs` が削除されている
- `agent_loop/turn/mod.rs` がTurn orchestrationを所有している
- `TurnLifecycle` がdurable state操作を所有している
- `TurnPersistence` がTurnのMessage persistenceを所有している
- `agent_loop/loop.rs` に `AgentLoop` / `LoopState` / `AgentLoopResult` がある
- `agent_loop/model_step.rs` にLLM 1回分の処理が集約されている
- `ModelStep` がLLM responseの排他的状態を表現している
- `agent_loop/tool_execution.rs` に `ToolExecutor` とTool safety処理が集約されている
- 旧 `tool_phase.rs` が削除されている
- `EventEmitter` が `event.rs` に移っている
- Pulseが新しいshared moduleを使用している
- Tool / Turn / Session / Runtimeの既存Invariantが維持されている
- 関連テストが責務の所有先へ移動している
- 重複・過剰なテストが整理されている
- 新しい構造に合わせて関連ドキュメントが更新されている
- `cargo fmt --check` が通る
- `cargo check` が通る
- `cargo test` が通る
- `cargo clippy --all-targets --all-features -- -D warnings` が通る

このリファクタでは、単に巨大なファイルを複数ファイルへ分割するのではなく、

```text
Runtime Scheduling
        ↓
Turn Execution
        ↓
Agent Loop
        ↓
Model / Tool
```

という既存システムの概念境界を、module と型の構造へそのまま反映すること。
