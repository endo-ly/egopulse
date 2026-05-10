# Plan: Multi-Agent Room Phase 4 — Runtime Safety + Documentation

Multi-Agent Room における同時実行制御（per-session busy flag + input queue）と暴走防止（chain depth / turn limit + 5停止条件）を実装し、停止時に Channel Log へ `SystemEvent` を記録する。最後に全関連ドキュメント（Phase 4 範囲）を更新する。

> **Note**: 以下の具体的なコード例・API 設計・構成（How）はあくまで参考である。実装時によりよい設計方針があれば積極的に採用すること。

## 設計方針

- **仕様書 §8.2 に忠実な Approach C**: アプリケーション層で per-session busy flag を管理し、実行中の入力は queue に積む。turn 終了後に drain する。DB 層の楽観排他（`sessions.updated_at`）は安全網として残す
- **Phase 3 の PendingAgentTurn を拡張**: Phase 3 が導入する `mpsc::channel` + background worker 基盤を前提とし、TurnScheduler でラップする。background worker は `process_turn()` を直接呼ばず、TurnScheduler に submit する
- **origin_id による追跡**: 人間入力ごとに UUID を発行し、agent_send 連鎖を跨いで同一 origin の turn 数を追跡する。これにより `MAX_AGENT_TURNS_PER_INPUT` を実現する
- **停止条件の評価を関数として分離**: 5停止条件の評価を独立した pure function にし、system event 生成も含めて単体テスト可能にする
- **既存チャネルへの影響最小化**: Discord 以外のチャネル（CLI / Web / TUI / Telegram）は現状直接 `process_turn()` を呼んでいる。Phase 4 では Discord のみ TurnScheduler を経由し、他チャネルは後続フェーズで対応する

## Phase 3 前提（⚠️ 必須: Phase 3 マージ後にのみ実行可能）

**Phase 3（#63, agent_send Tool）は現在 OPEN / 未実装である。** Phase 4 の実装は Phase 3 が main にマージされた後のみ可能。

**進行順序**: Phase 3（#63）実装 → main マージ → Phase 4（#64, 本 Plan）実行

**Phase 3 の PR（未作成）が main にマージされた時点で、以下の成果物が `src/` に存在する前提**:

| Phase 3 成果物 | 場所 | Phase 4 での利用 |
|---|---|---|
| `PendingAgentTurn { context, input, chain_depth, external_chat_id }` | `src/agent_loop/mod.rs` | 拡張して `ScheduledTurn` に統合（`origin_id` 追加） |
| `turn_sender: mpsc::Sender<PendingAgentTurn>` | `src/tools/mod.rs` (ToolExecutionContext) | background worker 経由で TurnScheduler に submit |
| Background turn worker (mpsc receiver) | `src/runtime/mod.rs` | 直接 `process_turn()` 呼び出しを `TurnScheduler::submit()` に変更 |
| `agent_send` ツール | `src/tools/agent_send.rs` (**新規**) | chain_depth が increment されて `PendingAgentTurn` に渡される |
| `MAX_AGENT_CHAIN_DEPTH = 8` (tracing::warn のみ) | `src/runtime/mod.rs` | Phase 4 で system event 記録に強化、値を4に変更 |

**Phase 3 Plan 参照**: `docs/plan/plan-multi-agent-phase3.md`（Phase 3 の設計詳細）

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 実装元 |
|---|---|
| `TurnScheduler` (busy flag + queue) | `src/runtime/turn_scheduler.rs` (**新規**) |
| `ScheduledTurn` (PendingAgentTurn 拡張) | `src/agent_loop/mod.rs` |
| `StopCondition` 評価 + system event 生成 | `src/runtime/turn_scheduler.rs` |
| `TurnTracker` (per-origin turn counting) | `src/runtime/turn_scheduler.rs` |
| AppState への `turn_scheduler` 追加 | `src/runtime/mod.rs` |
| Background worker の TurnScheduler 統合 | `src/runtime/mod.rs` |
| Discord handler の TurnScheduler 経由化 | `src/channels/discord.rs` |
| System Event の Channel Log 保存 | `src/storage/queries.rs` |
| ドキュメント更新 (7ファイル) | `docs/*.md` |

---

## Step 0: Worktree 作成

```bash
git worktree add ../egopulse-multi-agent-phase4 -b feat/multi-agent-phase4
```

前提: Phase 3 (`feat/multi-agent-phase3`) が main にマージ済み。

---

## Step 1: ScheduledTurn + TurnScheduler Core (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `scheduled_turn_from_surface_context` | `ScheduledTurn` が `SurfaceContext` + input + chain_depth + origin_id を保持する |
| `scheduled_turn_session_key_matches_surface` | `ScheduledTurn::session_key()` が `context.session_key()` と一致する |
| `turn_scheduler_new_is_empty` | 新規 `TurnScheduler` は slot を持たない |
| `turn_scheduler_submit_first_turn_sets_busy` | 最初の submit → slot が busy になる |
| `turn_scheduler_submit_second_turn_enqueues` | busy 中の submit → queue に積まれる |
| `turn_scheduler_drain_after_completion` | turn 完了後の drain → queue の次が実行される |
| `turn_scheduler_drain_empty_clears_busy` | turn 完了後、queue が空 → busy が解除される |
| `turn_scheduler_different_sessions_independent` | 異なる session は独立して busy/queue を管理する |

### GREEN: 実装

**`src/agent_loop/mod.rs`** — `ScheduledTurn` 定義:

```rust
/// A turn submitted to the TurnScheduler for ordered execution.
pub(crate) struct ScheduledTurn {
    /// Surface context identifying the agent session.
    pub context: SurfaceContext,
    /// The input text for this turn.
    pub input: String,
    /// Chain depth: how many agent_send hops led to this turn.
    pub chain_depth: usize,
    /// Origin ID: UUID tracking all turns caused by a single human input.
    pub origin_id: String,
    /// External chat ID for sending responses back to the channel.
    pub external_chat_id: String,
}

impl ScheduledTurn {
    pub(crate) fn session_key(&self) -> String {
        self.context.session_key()
    }
}
```

**`src/runtime/turn_scheduler.rs`** (**新規**):

```
TurnScheduler struct:
  slots: std::sync::Mutex<HashMap<String, TurnSlot>>

TurnSlot struct:
  busy: bool
  queue: VecDeque<ScheduledTurn>

TurnScheduler::submit(&self, turn: ScheduledTurn, executor: impl Fn(ScheduledTurn)):
  lock slots
  get or create TurnSlot for session_key
  if slot.busy → enqueue, return
  if !slot.busy → set busy=true, drop lock, call executor(turn)

TurnScheduler::on_turn_completed(&self, session_key: &str) -> Option<ScheduledTurn>:
  lock slots
  get slot
  if queue not empty → pop front, return it (caller re-executes)
  if queue empty → set busy=false, return None
```

**参考**: `TurnSlot` の busy flag は `std::sync::Mutex` で保護する。`executor` は `tokio::spawn` で非同期実行するクロージャを想定。

### QA

```bash
cargo test -p egopulse scheduled_turn turn_scheduler
```

期待結果: 8 テスト全通過。`TurnScheduler` の busy/enqueue/drain が正しく動作する。

### コミット

`feat(runtime): add TurnScheduler with per-session busy flag and input queue`

---

## Step 2: Safety Constants + Stop Condition Evaluator (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `stop_condition_chain_depth_exceeded` | chain_depth > MAX → `StopReason::ChainDepthExceeded` |
| `stop_condition_turn_count_exceeded` | turn count >= MAX → `StopReason::TurnCountExceeded` |
| `stop_condition_agent_not_found` | agent_id が config に存在しない → `StopReason::AgentNotFound` |
| `stop_condition_llm_failure` | LLM error が渡された場合 → `StopReason::LlmFailure` |
| `stop_condition_session_unprocessable` | session 回復不能エラー → `StopReason::SessionUnprocessable` |
| `stop_condition_none_when_all_ok` | 全条件クリア → `None`（停止不要） |
| `system_event_contains_stop_reason` | 生成される system event の content に停止理由が含まれる |
| `system_event_message_kind_is_system_event` | `MessageKind::SystemEvent` が設定される |
| `turn_tracker_increments_per_origin` | 同一 origin_id でカウントが増加する |
| `turn_tracker_different_origins_independent` | 異なる origin_id のカウントは独立する |
| `turn_tracker_count_returns_zero_for_unknown` | 未登録 origin_id の count は 0 |

### GREEN: 実装

**`src/runtime/turn_scheduler.rs`** — 追加:

```rust
const MAX_AGENT_CHAIN_DEPTH: usize = 4;
const MAX_AGENT_TURNS_PER_INPUT: usize = 12;
```

**設計上の判断**:
- `evaluate_stop_conditions` は pure function として定義し、`turn.chain_depth`, `turn_count`, `config.agents` から判定する
- `StopReason::AgentNotFound` は agent_send ツール側で既に検証されるが、TurnScheduler レイヤーでも二重チェックする（防御的設計）
- `StopReason::LlmFailure` と `StopReason::SessionUnprocessable` は `process_turn()` の Result から事後的に判定し、system event を記録する
- `build_system_event` は `MessageKind::SystemEvent` + content に JSON 形式で理由を含める

### QA

```bash
cargo test -p egopulse stop_condition system_event turn_tracker
```

期待結果: 11 テスト全通過。各停止条件が正しく判定され、system event が生成される。

### コミット

`feat(runtime): add stop condition evaluator and turn tracker for runaway prevention`

---

## Step 3: System Event Persistence (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `system_event_saved_to_channel_log` | SystemEvent が Channel Log (channel_log_chat_id) に保存される |
| `system_event_content_is_valid_json` | content が JSON で `reason` キーを含む |
| `system_event_sender_is_system` | `sender_name` が `"system"` である |
| `system_event_is_from_bot_true` | `is_from_bot` が `true` である |
| `store_system_event_for_channel_log` | storage に Channel Log への SystemEvent INSERT ができる |

### GREEN: 実装

**`src/storage/queries.rs`** — 追加:

```rust
/// Saves a system event message to the Channel Log.
/// Uses the channel_log_chat_id as the target chat.
impl Database {
    pub fn store_system_event(
        &self,
        channel_log_chat_id: i64,
        reason: &StopReason,
    ) -> Result<(), StorageError> { ... }
}
```

**参考**: `StoredMessage` の各フィールド:
- `message_kind: MessageKind::SystemEvent`
- `sender_name: "system".to_string()`
- `content: serde_json::json!({"reason": ...}).to_string()`
- `is_from_bot: true`
- `sender_agent_id: None` (system event に agent 固有情報は不要)
- `recipient_agent_id: None`

**注意**: Channel Log に保存するため、`channel_log_chat_id` が必要。`ScheduledTurn` の `context.channel_log_chat_id` を使用する。

### QA

```bash
cargo test -p egopulse store_system_event
```

期待結果: 5 テスト全通過。SystemEvent が Channel Log に正しく保存される。

### コミット

`feat(storage): add system event persistence for Channel Log`

---

## Step 4: Runtime Integration (TDD)

前提: Step 1-3。

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `discord_submit_uses_turn_scheduler` | Discord handler が TurnScheduler に submit する |
| `background_worker_submit_uses_turn_scheduler` | Phase 3 の background worker が TurnScheduler に submit する |
| `concurrent_submits_serialized_by_scheduler` | 同一 session への同時 submit が直列化される |
| `chain_depth_exceeded_logs_system_event` | chain depth 超過時、Channel Log に SystemEvent が記録される |
| `turn_count_exceeded_logs_system_event` | turn count 超過時、Channel Log に SystemEvent が記録される |
| `llm_failure_logs_system_event` | LLM 失敗時、Channel Log に SystemEvent が記録される |
| `full_chain_flow_human_to_agent_to_agent` | 人間 → Agent A → (agent_send) → Agent B の正常フローが完了する |
| `full_chain_stops_at_depth_limit` | chain depth 上限で停止し、SystemEvent が残る |

### GREEN: 実装

**`src/runtime/mod.rs`** — AppState 拡張:

```rust
pub struct AppState {
    // ... existing fields
    /// Per-session turn scheduler for concurrency control.
    pub(crate) turn_scheduler: Arc<TurnScheduler>,
    /// Per-origin turn counter for runaway prevention.
    pub(crate) turn_tracker: Arc<TurnTracker>,
}
```

**`src/runtime/mod.rs`** — Background worker 更新:

Phase 3 の background worker で `PendingAgentTurn` 受信後の処理を変更:

```
Before (Phase 3):
  receiver.recv() → process_turn() → send response to channel

After (Phase 4):
  receiver.recv() → convert to ScheduledTurn → turn_scheduler.submit(turn, executor)
  executor: evaluate_stop_conditions → if stop: log system event
                                → if ok: process_turn() → on_turn_completed
```

**`src/channels/discord.rs`** — Discord handler 更新:

人間メッセージ受信時の処理を変更:

```
Before:
  resolve agent → process_turn() → send response

After:
  resolve agent → generate origin_id → create ScheduledTurn
  → turn_scheduler.submit(turn, executor)
  executor: evaluate_stop_conditions → if stop: log system event
                                → if ok: process_turn() → send response → on_turn_completed
```

**参考**: executor の共通化。Discord handler と background worker で同じ executor ロジックを使うため、`TurnScheduler` にコールバックを渡すか、`AppState` に共通 executor メソッドを定義する。

**参考**: `origin_id` は人間メッセージ受信時に `uuid::Uuid::new_v4().to_string()` で生成し、agent_send で新しく作られる `ScheduledTurn` に引き継ぐ。Phase 3 の `PendingAgentTurn` にも `origin_id` フィールドを追加する必要がある（Phase 3 側の拡張ポイントとして Plan に明記）。

### QA

```bash
cargo test -p egopulse full_chain concurrent_submits discord_submit background_worker
```

期待結果: 8 テスト全通過。人間→Agent→Agent の正常フローと、各停止条件での system event 記録が確認できる。

### コミット

`feat(runtime): integrate TurnScheduler into AppState, Discord handler, and background worker`

---

## Step 5: Documentation Updates

Phase 4 範囲（同時実行制御 + 暴走防止 + system event）のみを反映。

| ファイル | 更新内容 |
|---|---|
| `docs/architecture.md` | TurnScheduler のアーキテクチャ説明追加。runtime レイヤーに TurnScheduler を追記 |
| `docs/channels.md` | Multi-Agent Room の同時実行制御説明。input queue の動作説明 |
| `docs/session-lifecycle.md` | TurnScheduler による session busy 管理。queue drain のライフサイクル。system event の記録タイミング |
| `docs/db.md` | `MessageKind::SystemEvent` の説明。system event の content 形式（JSON） |
| `docs/config.md` | 内部定数 `MAX_AGENT_CHAIN_DEPTH=4`, `MAX_AGENT_TURNS_PER_INPUT=12` の説明（設定ファイルには出さない旨も明記。制限到達時は人間が新メッセージを送ることで自然に再開できる旨も記載） |
| `docs/system-prompt.md` | Channel Context 注入における system event の扱い（system event は Direct Input には含まれない等） |
| `docs/tools.md` | `agent_send` の runtime safety（chain depth 制限、turn 制限による停止可能性）の説明 |

### QA

各ドキュメントの該当セクションに Phase 4 の内容（同時実行制御、暴走防止、system event）が記載されていることを目視確認。`cargo doc --no-deps` で public item の doc comment が壊れていないことも確認。

### コミット

`docs: update 7 docs for Multi-Agent Room Phase 4 runtime safety`

---

## Step 6: 動作確認

```bash
cargo fmt --check
cargo check -p egopulse
cargo test -p egopulse
cargo clippy --all-targets --all-features -- -D warnings
```

期待結果: 全コマンド exit code 0。lint 警告なし、テスト全通過（32 件）。
cargo clippy --all-targets --all-features -- -D warnings
```

---

## Step 7: PR 作成

PR description は日本語。`Close #64` を明記。親 Issue #76 の全 Phase 完了を PR 本文に記載。

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
|---|---|---|
| `src/runtime/turn_scheduler.rs` | **新規** | TurnScheduler, TurnSlot, StopReason, TurnTracker, evaluate_stop_conditions, build_system_event |
| `src/runtime/mod.rs` | 変更 | AppState に turn_scheduler / turn_tracker 追加。background worker の TurnScheduler 統合 |
| `src/agent_loop/mod.rs` | 変更 | ScheduledTurn 定義 |
| `src/channels/discord.rs` | 変更 | メッセージ受信時の TurnScheduler 経由化 |
| `src/storage/queries.rs` | 変更 | store_system_event 追加 |
| `docs/architecture.md` | 変更 | TurnScheduler 説明 |
| `docs/channels.md` | 変更 | 同時実行制御説明 |
| `docs/session-lifecycle.md` | 変更 | TurnScheduler lifecycle + system event |
| `docs/db.md` | 変更 | SystemEvent 説明 |
| `docs/config.md` | 変更 | 内部定数説明 |
| `docs/system-prompt.md` | 変更 | system event の扱い |
| `docs/tools.md` | 変更 | agent_send の runtime safety 説明 |

---

## コミット分割

1. `feat(runtime): add TurnScheduler with per-session busy flag and input queue` — `src/runtime/turn_scheduler.rs`, `src/agent_loop/mod.rs`
2. `feat(runtime): add stop condition evaluator and turn tracker for runaway prevention` — `src/runtime/turn_scheduler.rs`
3. `feat(storage): add system event persistence for Channel Log` — `src/storage/queries.rs`
4. `feat(runtime): integrate TurnScheduler into AppState, Discord handler, and background worker` — `src/runtime/mod.rs`, `src/channels/discord.rs`
5. `docs: update 7 docs for Multi-Agent Room Phase 4 runtime safety` — `docs/*.md`

---

## テストケース一覧（全 32 件）

### ScheduledTurn + TurnScheduler Core (8)

1. `scheduled_turn_from_surface_context` — ScheduledTurn が context + input + chain_depth + origin_id を保持
2. `scheduled_turn_session_key_matches_surface` — session_key() が context.session_key() と一致
3. `turn_scheduler_new_is_empty` — 新規 TurnScheduler は slot 無し
4. `turn_scheduler_submit_first_turn_sets_busy` — 最初の submit → busy
5. `turn_scheduler_submit_second_turn_enqueues` — busy 中の submit → enqueue
6. `turn_scheduler_drain_after_completion` — turn 完了 → drain → 次が実行
7. `turn_scheduler_drain_empty_clears_busy` — turn 完了 → queue 空 → busy 解除
8. `turn_scheduler_different_sessions_independent` — 異 session は独立管理

### Stop Condition Evaluator + TurnTracker (11)

9. `stop_condition_chain_depth_exceeded` — chain_depth > 4 → ChainDepthExceeded
10. `stop_condition_turn_count_exceeded` — turn count >= 12 → TurnCountExceeded
11. `stop_condition_agent_not_found` — agent 不在 → AgentNotFound
12. `stop_condition_llm_failure` — LLM error → LlmFailure
13. `stop_condition_session_unprocessable` — session 回復不能 → SessionUnprocessable
14. `stop_condition_none_when_all_ok` — 全条件クリア → None
15. `system_event_contains_stop_reason` — content に停止理由 JSON 含む
16. `system_event_message_kind_is_system_event` — MessageKind::SystemEvent 設定
17. `turn_tracker_increments_per_origin` — 同一 origin でカウント増加
18. `turn_tracker_different_origins_independent` — 異 origin のカウント独立
19. `turn_tracker_count_returns_zero_for_unknown` — 未登録 origin → count 0

### System Event Persistence (5)

20. `system_event_saved_to_channel_log` — Channel Log に保存される
21. `system_event_content_is_valid_json` — content が JSON で reason キー含む
22. `system_event_sender_is_system` — sender_name が "system"
23. `system_event_is_from_bot_true` — is_from_bot が true
24. `store_system_event_for_channel_log` — storage INSERT 動作確認

### Runtime Integration (8)

25. `discord_submit_uses_turn_scheduler` — Discord handler → TurnScheduler
26. `background_worker_submit_uses_turn_scheduler` — background worker → TurnScheduler
27. `concurrent_submits_serialized_by_scheduler` — 同一 session 直列化
28. `chain_depth_exceeded_logs_system_event` — chain depth 超過 → SystemEvent 記録
29. `turn_count_exceeded_logs_system_event` — turn count 超過 → SystemEvent 記録
30. `llm_failure_logs_system_event` — LLM 失敗 → SystemEvent 記録
31. `full_chain_flow_human_to_agent_to_agent` — 人間 → A → B 正常フロー
32. `full_chain_stops_at_depth_limit` — chain depth 上限で停止 + SystemEvent

---

## 工数見積もり

| Step | 内容 | テスト行数 | 実装行数 | 合計 |
|---|---|---|---|---|
| Step 0 | WT 作成 | — | — | 0 |
| Step 1 | TurnScheduler Core | 120 | 100 | 220 |
| Step 2 | Stop Condition + TurnTracker | 160 | 130 | 290 |
| Step 3 | System Event Persistence | 80 | 60 | 140 |
| Step 4 | Runtime Integration | 200 | 150 | 350 |
| Step 5 | Documentation (7 files) | — | 200 | 200 |
| **合計** | | **560** | **640** | **~1200** |
