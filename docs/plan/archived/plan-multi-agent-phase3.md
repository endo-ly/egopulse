# Plan: Multi-Agent Room Phase 3 — agent_send Tool

Multi-Agent Room で Agent 間の非同期コミュニケーションを実現する `agent_send` ツールを実装する。現在のチャンネル内で宛先 Agent の次 Turn をキューに積み、`[From → To] message` 形式でチャンネルに表示する。

> **Note**: 以下の具体的なコード例・API 設計・構成（How）はあくまで参考である。実装時によりよい設計方針があれば積極的に採用すること。

## 設計方針

- **Tool trait に従う**: 既存ツール（`send_message` 等）と同じ `Tool` trait パターンで実装。`name()`, `definition()`, `execute()` を提供
- **非同期・非ブロッキング**: `agent_send` は宛先 Agent の応答を待たず、配信ステータスのみ返す。宛先 Agent の Turn は `tokio::spawn` で非同期にキューイング
- **Channel Log + Agent Session 二層保存**: Phase 2 で構築した二層アーキテクチャを活用。agent_send メッセージは Channel Log に `MessageKind::AgentSend` で保存し、宛先 Agent Session には Direct Input として注入
- **宛先 Agent の応答をチャンネルに送信する**: Background worker は `process_turn()` の戻り値を受け取り、`ChannelRegistry.send_text()` でチャンネルに送信する。`process_turn()` 自体は文字列を返すだけでチャンネル送信は行わないため、worker がこの責務を担う
- **現在のチャンネル内で完結**: 送信先チャンネルは選ばない。常に現在実行中のチャンネル内で宛先 Agent を起動する（#76 §6.1）
- **Discord のみ提供**: agent_send ツールは Discord チャンネルが設定されている場合のみ登録。CLI には ChannelAdapter がなく、Web の `send_text` は no-op であるため、Phase 3 では Discord に限定する
- **宛先検証は config.agents 全体**: `channels.discord.channels.<id>.agents` は常駐 Agent の定義であり、agent_send の宛先制限ではない（#76 §6.4）。`config.agents` に存在すれば送信可能
- **自己送信は禁止**: `from == to` の場合は ToolResult::error を返す
- **最小暴走防止（chain depth 制限）**: Phase 3 で `MAX_AGENT_CHAIN_DEPTH = 8` を実装。`PendingAgentTurn` に `chain_depth` を持たせ、background worker が上限超過時は Turn を実行せず tracing で警告を出力。A→B→A→B の無限連鎖を防止する

## Phase 3 での INSERT クエリ更新

Phase 1 で追加した `message_kind`, `sender_agent_id`, `recipient_agent_id` カラムは Phase 1〜2 とも DEFAULT 値のまま。Phase 3 で初めて非 DEFAULT 値（`MessageKind::AgentSend` + 実際の agent_id）を書き込むため、`store_message` 系の INSERT クエリを更新する。新規関数または既存関数の拡張で対応。

## 既知の負債（将来 Phase で解消）

| 負債 | 現状 | 解消タイミング |
|---|---|---|
| chain depth 制限 | Phase 3 で `MAX_AGENT_CHAIN_DEPTH = 8` を実装済み。Phase 4 で `MAX_AGENT_TURNS_PER_INPUT`、同時実行制御を追加 | Phase 4: Runtime Safety |
| system_event 未記録 | 停止条件やエラー時の system event 記録なし | Phase 4 |
| agent_send の同時実行制御 | 同一 Agent Session に複数 Turn が同時実行される可能性 | Phase 4: 楽観排他 + queue |
| 非 Discord チャネル対応 | CLI には ChannelAdapter なし、Web の send_text は no-op。Phase 3 では Discord のみ | 将来: CLI/Web での agent_send |

## 外部フォーマット定義

```
agent_send ツール定義:
  name: "agent_send"
  parameters:
    to: string (required) — 宛先 Agent ID
    message: string (required) — メッセージ内容

Channel Log 保存:
  message_kind: "agent_send"
  sender_agent_id: "<from_agent_id>"
  recipient_agent_id: "<to_agent_id>"
  content: "<message>"

チャンネル表示フォーマット:
  "[Lyre → Vega] この仕様、セッション設計としてどう見ますか？"

宛先 Agent の Direct Input:
  "<direct-input>\n[Lyre → Vega] この仕様、セッション設計としてどう見ますか？\n</direct-input>"

返り値:
  { "delivered": true, "to": "vega" }
  { "delivered": false, "error": "agent 'unknown' not found" }
```

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 実装元 |
|---|---|
| ToolExecutionContext 拡張 | `src/tools/mod.rs` |
| agent_send ツール | `src/tools/agent_send.rs` (**新規**) |
| ToolRegistry 登録 | `src/tools/mod.rs` |
| Channel Log メッセージ保存 | `src/storage/queries.rs` |
| 宛先 Agent Turn キューイング | `src/agent_loop/turn.rs`, `src/agent_loop/mod.rs` |
| process_turn 署名変更（必要に応じて） | `src/agent_loop/turn.rs` |
| Docs | `docs/tools.md`, `docs/mcp.md`, `docs/session-lifecycle.md` |

---

## Step 0: Worktree 作成

```bash
git worktree add ../egopulse-multi-agent-phase3 -b feat/multi-agent-phase3
```

前提: Phase 2 (`feat/multi-agent-phase2`) が main にマージ済み。

---

## Step 1: ToolExecutionContext 拡張 + Turn キュー基盤 (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `tool_context_includes_agent_id` | ToolExecutionContext に `agent_id` が設定される |
| `tool_context_includes_channel_log_chat_id` | ToolExecutionContext に `channel_log_chat_id` が設定される |
| `tool_context_includes_turn_sender` | ToolExecutionContext に `turn_sender` が設定される |
| `process_turn_populates_new_context_fields` | process_turn_inner が新フィールドを正しく生成する |

### GREEN: 実装

**`src/tools/mod.rs`** — `ToolExecutionContext` 拡張:

```rust
pub(crate) struct ToolExecutionContext {
    pub chat_id: i64,
    pub channel: String,
    pub surface_thread: String,
    pub chat_type: String,
    // Phase 3 追加
    pub agent_id: String,
    pub channel_log_chat_id: Option<i64>,
    pub turn_sender: tokio::sync::mpsc::Sender<PendingAgentTurn>,
}
```

**`src/agent_loop/mod.rs`** — `PendingAgentTurn` 定義:

```rust
pub(crate) struct PendingAgentTurn {
    pub context: SurfaceContext,
    pub input: String,
    pub chain_depth: usize,               // agent_send の連鎖深度
    pub external_chat_id: String,         // 応答送信先の external_chat_id
}
```

**`src/agent_loop/turn.rs`** — `process_turn_inner()` 更新:
- ToolExecutionContext 生成時に新フィールドを設定
- `turn_sender` は SurfaceContext または AppState から取得

**Runtime 側（`src/runtime/mod.rs`）**:
- `tokio::sync::mpsc::channel::<PendingAgentTurn>` を作成
- AppState に sender を保持、または SurfaceContext 経由で伝播
- Background task で receiver を listen → `process_turn()` を呼び出し

### コミット

`feat(tools): extend ToolExecutionContext with agent_id, channel_log_chat_id, and turn_sender`

---

## Step 2: agent_send ツール — 定義 + バリデーション + 実行 (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `agent_send_definition_name` | ツール名が `"agent_send"` |
| `agent_send_definition_has_to_and_message` | パラメータに `to` (required) と `message` (required) が含まれる |
| `agent_send_validates_agent_exists` | 存在する Agent → 成功 |
| `agent_send_rejects_unknown_agent` | 存在しない Agent → ToolResult::error |
| `agent_send_rejects_self_send` | `from == to` の自己送信 → ToolResult::error |
| `agent_send_returns_delivered_true` | 成功時: `{ "delivered": true, "to": "vega" }` |
| `agent_send_saves_to_channel_log` | Channel Log に `MessageKind::AgentSend` で保存される |
| `agent_send_sets_sender_recipient_ids` | `sender_agent_id`, `recipient_agent_id` が正しく設定される |
| `agent_send_sends_turn_to_target` | turn_sender に PendingAgentTurn が送られる |
| `agent_send_target_context_uses_source_channel` | 宛先 Agent の SurfaceContext が現在のチャンネルを使用 |
| `agent_send_target_input_format` | 宛先 Agent への input が `[From → To] message` 形式 |
| `agent_send_display_format` | チャンネル表示が `[From → To] message` 形式 |

### GREEN: 実装

**`src/tools/agent_send.rs`** (**新規**):

```
構造体: AgentSendTool
依存: config.agents (validation), db (Channel Log save), channels (display), turn_sender (queuing)

execute() フロー:
  1. parse_params: { to: String, message: String }
  2. validate: config.agents に to が存在するか
  3. validate: to == context.agent_id（自己送信）ならエラー
  3. Channel Log に保存:
     - message_kind: AgentSend
     - sender_agent_id: context.agent_id
     - recipient_agent_id: to
     - content: message
  4. チャンネルに表示:
     - "[{from_label} → {to_label}] {message}"
     - ChannelRegistry経由で send_text()
  5. 宛先 Agent Turn をキューイング:
     - PendingAgentTurn { context, input, chain_depth: current_depth + 1, external_chat_id } を turn_sender に送信
     - context: 現在のチャンネル + target agent_id + 同じ channel_log_chat_id
     - input: "[{from_label} → {to_label}] {message}"
  6. 返り値: { "delivered": true, "to": "{agent_id}" }
```

**表示の補足**:
- Discord: ChannelRegistry → DiscordAdapter.send_text()（REST API）
- agent_send の表示はツール実行中に発生。LLM の応答とは別にチャンネルに送信される
- agent_send ツール自体が Discord 限定で登録されるため、非 Discord チャネルでは呼ばれない

**Agent label 解決**:
- `config.agents[agent_id].label` または `agent_id` をフォールバック

### コミット

`feat(tools): implement agent_send tool for inter-agent communication`

---

## Step 3: ToolRegistry 登録 + ターン実行基盤 (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `agent_send_registered_in_tool_registry` | ToolRegistry に agent_send が含まれる |
| `agent_send_not_registered_without_discord` | Discord ボット設定がない場合は登録されない |
| `pending_turn_executed_by_background_task` | PendingAgentTurn が process_turn に渡される |
| `pending_turn_sends_response_to_channel` | Background worker が process_turn の戻り値をチャンネルに送信する |
| `pending_turn_rejected_at_max_chain_depth` | chain_depth >= MAX_AGENT_CHAIN_DEPTH の場合、Turn が実行されず警告ログ |
| `pending_turn_uses_target_agent_session` | 宛先 Agent の Session が正しく解決される |

### GREEN: 実装

**`src/tools/mod.rs`** — `ToolRegistry::new()` 更新:
- Discord チャンネルが設定されている（`config.discord_bots()` が非空）場合のみ `AgentSendTool` を登録
- CLI/Web では agent_send ツールは LLM に露出しない

**`src/runtime/mod.rs`** — Background turn worker:
- `mpsc::channel::<PendingAgentTurn>(16)` を作成
- AppState に sender を保持
- `tokio::spawn` で background task を起動
- Background task の処理:
  1. receiver から `PendingAgentTurn` を受信
  2. `chain_depth >= MAX_AGENT_CHAIN_DEPTH` なら tracing::warn でログ出力してスキップ
  3. `process_turn(&app_state, &context, &input)` を実行
  4. 戻り値（宛先 Agent の応答）を `ChannelRegistry.send_text(&external_chat_id, &response)` でチャンネルに送信
  5. エラー時は tracing でログ出力（Phase 4 で system_event に昇格）
- `MAX_AGENT_CHAIN_DEPTH = 8` は内部定数（#76 §8.3 準拠。設定ファイルには出さない）

**`src/agent_loop/turn.rs`**:
- `process_turn_inner()` で ToolExecutionContext に `turn_sender` を設定
- AppState または SurfaceContext 経由で sender を取得

### コミット

`feat(runtime): register agent_send tool and add background turn execution worker`

---

## Step 4: 統合テスト (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `agent_send_full_flow_multi_room` | Multi-Agent Room: mention → agent 応答 → agent_send → 宛先 Agent 応答 |
| `agent_send_in_single_agent_channel` | Single-Agent Channel: agent が agent_send で別 Agent を呼び出し |
| `agent_send_to_non_existent_agent` | 存在しない Agent への agent_send → エラー |
| `agent_send_display_in_channel` | チャンネルに [From → To] が表示される |
| `agent_send_channel_log_saved` | Channel Log に正しく保存される |
| `agent_send_target_session_independent` | 宛先 Agent の Session が送信元 Agent とは独立 |
| `existing_tools_not_affected` | 既存ツール（bash, read 等）に影響なし（回帰） |

### GREEN: 実装

統合テストの追加。テストヘルパーで in-memory DB + mock config を構築し、エンドツーエンドのフローを検証。

### コミット

`test: add integration tests for agent_send tool`

---

## Step 5: Docs Update

### 対象

| ファイル | 変更内容 |
|---|---|
| `docs/tools.md` | agent_send ツールの説明、パラメータ、使用例を追加 |
| `docs/session-lifecycle.md` | Agent 間通信のセッションライフサイクル追記 |
| `docs/channels.md` | agent_send のチャンネル表示仕様追記 |
| `docs/db.md` | MessageKind::AgentSend、sender/recipient_agent_id の説明追記 |

### コミット

`docs: update tools, session-lifecycle, channels, db docs for agent_send`

---

## Step 6: 動作確認

```bash
cargo fmt --check
cargo check -p egopulse
cargo test -p egopulse
cargo clippy --all-targets --all-features -- -D warnings
```

---

## Step 7: PR 作成

PR description は日本語。`Close #63` を明記。

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
|---|---|---|
| `src/tools/agent_send.rs` | **新規** | agent_send ツール実装 |
| `src/tools/mod.rs` | 変更 | ToolExecutionContext 拡張、ToolRegistry 登録 |
| `src/agent_loop/mod.rs` | 変更 | PendingAgentTurn 定義、SurfaceContext 拡張（必要に応じて） |
| `src/agent_loop/turn.rs` | 変更 | ToolExecutionContext 生成時の新フィールド設定 |
| `src/runtime/mod.rs` | 変更 | Background turn worker + mpsc channel |
| `src/storage/queries.rs` | 変更 | MessageKind::AgentSend での保存クエリ対応（INSERT の更新） |
| `docs/tools.md` | 変更 | agent_send ツールドキュメント |
| `docs/session-lifecycle.md` | 変更 | Agent 間通信セッション追記 |
| `docs/channels.md` | 変更 | agent_send 表示仕様追記 |
| `docs/db.md` | 変更 | MessageKind::AgentSend 説明追記 |

---

## コミット分割

1. `feat(tools): extend ToolExecutionContext with agent_id, channel_log_chat_id, and turn_sender` — `src/tools/mod.rs`, `src/agent_loop/mod.rs`, `src/agent_loop/turn.rs`, `src/runtime/mod.rs`
2. `feat(tools): implement agent_send tool for inter-agent communication` — `src/tools/agent_send.rs`, `src/storage/queries.rs`
3. `feat(runtime): register agent_send tool and add background turn execution worker` — `src/tools/mod.rs`, `src/runtime/mod.rs`
4. `test: add integration tests for agent_send tool` — `src/tools/agent_send.rs`, `src/agent_loop/turn.rs`
5. `docs: update tools, session-lifecycle, channels, db docs for agent_send` — `docs/*.md`

---

## テストケース一覧（全 29 件）

### ToolExecutionContext 拡張 (4)

1. `tool_context_includes_agent_id` — agent_id フィールド設定確認
2. `tool_context_includes_channel_log_chat_id` — channel_log_chat_id フィールド設定確認
3. `tool_context_includes_turn_sender` — turn_sender フィールド設定確認
4. `process_turn_populates_new_context_fields` — process_turn_inner での新フィールド生成確認

### agent_send ツール (12)

5. `agent_send_definition_name` — ツール名 "agent_send"
6. `agent_send_definition_has_to_and_message` — パラメータ定義確認
7. `agent_send_validates_agent_exists` — 存在する Agent → 成功
8. `agent_send_rejects_unknown_agent` — 存在しない Agent → エラー
9. `agent_send_rejects_self_send` — 自己送信 → エラー
10. `agent_send_returns_delivered_true` — 返り値 { delivered: true, to: "vega" }
11. `agent_send_saves_to_channel_log` — Channel Log に AgentSend で保存
12. `agent_send_sets_sender_recipient_ids` — sender/recipient agent_id 設定
13. `agent_send_sends_turn_to_target` — turn_sender に PendingAgentTurn 送信
14. `agent_send_target_context_uses_source_channel` — 宛先 Agent の SurfaceContext が現在のチャンネルを使用
15. `agent_send_target_input_format` — input が [From → To] 形式
16. `agent_send_display_format` — チャンネル表示が [From → To] 形式

### ToolRegistry + Turn 実行 (6)

17. `agent_send_registered_in_tool_registry` — ToolRegistry に含まれる
18. `agent_send_not_registered_without_discord` — Discord ボット設定なしでは登録なし
19. `pending_turn_executed_by_background_task` — Background task が process_turn を実行
20. `pending_turn_sends_response_to_channel` — Worker が応答をチャンネルに送信
21. `pending_turn_rejected_at_max_chain_depth` — chain_depth 上限でスキップ + 警告ログ
22. `pending_turn_uses_target_agent_session` — 宛先 Agent の Session が正しく解決

### 統合テスト (7) — ※ Step 4 に含まれるが番号は連番

23. `agent_send_full_flow_multi_room` — Multi-Agent Room のエンドツーエンド
24. `agent_send_in_single_agent_channel` — Single-Agent Channel からの agent_send
25. `agent_send_to_non_existent_agent` — 不在 Agent へのエラー
26. `agent_send_display_in_channel` — チャンネル表示確認
27. `agent_send_channel_log_saved` — Channel Log 保存確認
28. `agent_send_target_session_independent` — セッション独立性確認
29. `existing_tools_not_affected` — 既存ツールへの回帰確認

> 注: 統合テスト 23-29 は Step 4 で実装。番号は全体通し。

---

## 工数見積もり

| Step | 内容 | テスト行数 | 実装行数 | 合計 |
|---|---|---|---|---|
| Step 0 | WT 作成 | — | — | 0 |
| Step 1 | ToolExecutionContext 拡張 + キュー基盤 | 60 | 80 | 140 |
| Step 2 | agent_send ツール実装 | 150 | 120 | 270 |
| Step 3 | ToolRegistry 登録 + Turn worker | 60 | 80 | 140 |
| Step 4 | 統合テスト | 120 | — | 120 |
| Step 5 | Docs | — | 80 | 80 |
| **合計** | | **390** | **360** | **~750** |
