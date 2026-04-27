# Plan: Multi-Agent Core Foundation

Agent 定義、Agent 固有 SOUL/AGENTS、Agent-aware な LLM 解決、Discord 向け session identity helper を追加する。Discord 複数 Bot 起動は別 Plan に分離し、本 Plan では全チャネル共通の Agent 基盤を先に整える。

> **Note**: 以下の具体的なコード例・API 設計・構成（How）はあくまで参考である。実装時によりよい設計方針があれば積極的に採用すること。

## 設計方針

- `agent_id` を永続 ID として扱い、表示名・Bot Token の変更では変えない。
- 既存 DB schema は変更しない。Agent 別 session 分離は surface 側の `surface_thread` に `:agent:{agent_id}` を含める方式で行う。
- 既存 `channels.*.soul_path` / `souls/` は fallback として残し、Agent 固有 SOUL を最優先にする。
- チャット別 `runtime/groups/{channel}/{chat_id}/AGENTS.md` / `SOUL.md` は読み込み対象から外し、Agent 固有ファイルへ集約する。
- Web / Telegram / CLI は当面 `default_agent` を使い、既存 session identity を維持する。Discord だけ PR2 で `surface_thread = "{channel_id}:agent:{agent_id}"` にする。
- Agent LLM 解決は Config helper だけでなく、`AppState`、turn 処理、slash command の実行経路まで Agent-aware にする。

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 実装元 |
|---|---|
| Config の Agent 定義 | `src/config/mod.rs`, `src/config/loader.rs`, `src/config/persist.rs`, `src/config/resolve.rs` |
| Agent LLM 解決 | `src/config/resolve.rs`, `src/runtime.rs`, `src/agent_loop/turn.rs`, `src/slash_commands.rs`, `src/llm_profile.rs` |
| SurfaceContext の Agent 対応 | `src/agent_loop/mod.rs`, `src/agent_loop/session.rs`, 各 channel 呼び出し元 |
| Agent SOUL / AGENTS 読み込み | `src/soul_agents.rs`, `src/agent_loop/turn.rs` |
| Session identity helper | `src/agent_loop/mod.rs`, `src/agent_loop/session.rs`, Discord/Web/Telegram/TUI/CLI context 生成部 |
| Secret redaction | `src/tools/sanitizer.rs` |
| Docs 整合 | `docs/multi-agent.md`, `docs/config.md`, `docs/system-prompt.md`, `docs/session-lifecycle.md`, `docs/directory.md` |

---

## Step 0: Worktree 作成

```bash
git worktree add ../egopulse-multi-agent-core -b feat/multi-agent-core
cd ../egopulse-multi-agent-core
```

---

## Step 1: Config Agent Model (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `loads_agents_with_default_agent` | YAML の `default_agent` と `agents` を読み込み、既定 Agent を解決できる |
| `default_agent_falls_back_to_default_when_missing` | `default_agent` 未指定時に `default` を使う |
| `rejects_default_agent_not_in_agents` | `default_agent` が `agents` に存在しない場合は構造化エラー |
| `rejects_agent_id_path_traversal` | `../`, `/`, 空文字など危険な Agent ID を拒否 |
| `persists_agents_without_leaking_secret_values` | SecretRef の実値を YAML に出さず `agents.*.discord.bot_token` の参照を保持 |

### GREEN: 実装

`AgentId`, `AgentConfig`, `AgentDiscordConfig` を追加する。`Config` に `default_agent` と `agents` を追加し、loader/persist/debug/redaction を更新する。`ChannelConfig` の `bot_token` は PR1 では互換のため残すが、PR2 で廃止対象にする。

### コミット

`feat: add agent configuration model`

---

## Step 2: Agent LLM Resolution (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `agent_model_overrides_channel_model` | `agent.model` が `channel.model` より優先される |
| `agent_provider_overrides_channel_provider` | `agent.provider` が `channel.provider` より優先される |
| `agent_null_model_falls_back_to_channel_model` | Agent model が `null` の場合は既存 channel 解決へ fallback |
| `agent_null_provider_falls_back_to_channel_provider` | Agent provider が `null` の場合は既存 channel 解決へ fallback |
| `unknown_agent_returns_config_error` | 存在しない Agent ID の LLM 解決は明示エラー |
| `turn_uses_agent_llm_resolution` | agent context 付き turn が `llm_for_context` 経由で Agent LLM を使う |
| `status_uses_agent_llm_resolution` | `/status` が channel ではなく context の Agent LLM を表示する |
| `compact_uses_agent_llm_resolution` | `/compact` が `global_llm()` ではなく `llm_for_context()` を使う |

### GREEN: 実装

`resolve_llm_for_agent_channel(agent_id, channel)` のような API を追加し、既存 `resolve_llm_for_channel` は `default_agent` を使う互換 wrapper にする。
`AppState::llm_for_context(&SurfaceContext)` を追加し、`agent_loop::turn` の本番経路を `llm_for_channel` から差し替える。
`/status` は context の Agent LLM を表示する。`/compact` は `state.global_llm()` ではなく `state.llm_for_context(context)` を使う。
`/provider` `/model` はこの PR では既存 scope のまま残し、Agent 別切替は将来スコープ外とする。

### コミット

`feat: resolve llm settings through agents`

---

## Step 3: SurfaceContext Agent Identity (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `surface_context_defaults_to_default_agent` | agent 未指定 context が `default_agent` を使う |
| `discord_agent_scoped_thread_includes_agent_id` | Discord 用 helper が `"{channel_id}:agent:{agent_id}"` を作る |
| `same_discord_thread_different_agents_create_different_chats` | helper 適用後は同じ Discord channel でも Agent が違えば別 `chat_id` |
| `same_discord_thread_same_agent_reuses_chat` | helper 適用後は同じ Discord channel/agent で既存 `chat_id` を再利用 |
| `web_and_telegram_keep_existing_identity_with_default_agent` | Web/Telegram は `default_agent` 前提で既存 session を維持する |
| `web_chat_id_reentry_preserves_existing_external_chat_id` | Web の `chat:<id>` 再入場が既存 `external_chat_id` を壊さない |

### GREEN: 実装

`SurfaceContext` に必須 `agent_id: AgentId` を追加する。既存生成箇所は `default_agent` を設定する。
`SurfaceContext::session_key()` は既存通り `channel:surface_thread` を返し、勝手に Agent suffix を付けない。
Discord 向けに `discord_agent_surface_thread(channel_id, agent_id)` のような helper を用意し、PR2 で Discord handler が明示的に使う。

### コミット

`feat: carry agent identity in surface context`

---

## Step 4: Slash Command Agent Context (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `handle_status_receives_surface_context` | `/status` が `caller_channel` 文字列ではなく `SurfaceContext` から Agent を参照する |
| `handle_compact_receives_surface_context` | `/compact` が Agent context 付きで compaction 用 context を作る |
| `handle_compact_uses_agent_llm` | `/compact` が Agent の provider/model で summary を作る |
| `slash_command_callers_pass_default_agent_context` | CLI/Web/Telegram/既存 Discord 呼び出しが default Agent context を渡す |
| `llm_profile_resolved_for_scope_keeps_channel_scope` | `/provider` `/model` の channel scope 既存挙動は維持する |

### GREEN: 実装

`handle_slash_command` の引数を `caller_channel: &str` から `context: &SurfaceContext` へ変更する。
`/status` と `/compact` は Agent-aware context を使い、`/compact` の要約 LLM も `llm_for_context()` で解決する。
`/provider` `/model` は既存 scope 操作を維持する。

### コミット

`feat: pass agent context through slash commands`

---

## Step 5: Agent SOUL / AGENTS Loader (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `load_soul_prefers_agent_soul` | `agents/{agent_id}/SOUL.md` が `channels.*.soul_path` より優先される |
| `load_soul_falls_back_to_channel_soul_path` | Agent SOUL がなければ既存 `soul_path` を使う |
| `load_soul_falls_back_to_global_soul` | Agent SOUL も `soul_path` もなければ `SOUL.md` を使う |
| `load_agents_combines_global_and_agent_agents` | `AGENTS.md` と `agents/{agent_id}/AGENTS.md` を累積注入する |
| `chat_specific_md_is_ignored` | `runtime/groups/.../AGENTS.md` / `SOUL.md` は読み込まれない |
| `agent_id_path_traversal_is_rejected` | Agent ID 由来の path traversal を拒否 |

### GREEN: 実装

`SoulAgentsLoader` に Agent 用パス解決を追加し、`build_system_prompt` が `SurfaceContext.agent_id` と `agent_label` を使うようにする。チャット別 MD 読み込みは削除または未使用化する。

### コミット

`feat: load prompt files from agent directories`

---

## Step 6: Core Docs Update (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `docs_reference_agent_config_fields` | `docs/config.md` に Agent 設定フィールドが記載されていることをレビューで確認 |
| `docs_reference_agent_prompt_order` | `docs/system-prompt.md` が Agent SOUL/AGENTS の順序を説明していることをレビューで確認 |
| `docs_reference_agent_session_identity` | `docs/session-lifecycle.md` が「Discord は Agent suffix、Web/Telegram は既存 identity 維持」を説明していることをレビューで確認 |

### GREEN: 実装

関連 docs を更新する。ドキュメント確認は自動テストではなく、PR チェックリストで明示する。

### コミット

`docs: document multi-agent core behavior`

---

## Step 7: 動作確認

- `cargo fmt --check`
- `cargo test config`
- `cargo test soul_agents`
- `cargo test agent_loop::session`
- `cargo test agent_loop::turn`
- `cargo check`
- `cargo clippy --all-targets --all-features -- -D warnings`

---

## Step 8: PR 作成

- ブランチ: `feat/multi-agent-core`
- PR title: `feat: add multi-agent core foundation`
- PR description は日本語で作成する。
- 該当 Issue がある場合は `Close #XX` を明記する。

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
|---|---|---|
| `src/config/mod.rs` | 変更 | Agent ID / AgentConfig / AgentDiscordConfig を追加 |
| `src/config/loader.rs` | 変更 | YAML `default_agent` / `agents` 読み込みと validation |
| `src/config/persist.rs` | 変更 | Agent 設定の保存、SecretRef 維持 |
| `src/config/resolve.rs` | 変更 | Agent 経由 LLM 解決と helper |
| `src/tools/sanitizer.rs` | 変更 | `agents.*.discord.bot_token` の redaction |
| `src/agent_loop/mod.rs` | 変更 | `SurfaceContext` に Agent identity を追加 |
| `src/agent_loop/session.rs` | 変更 | 既存 session identity 維持と Agent-aware helper のテスト |
| `src/agent_loop/turn.rs` | 変更 | prompt 生成で Agent context を使用 |
| `src/slash_commands.rs` | 変更 | slash command に `SurfaceContext` を渡す |
| `src/llm_profile.rs` | 変更 | 既存 channel scope 操作を維持しつつ Agent context と共存 |
| `src/soul_agents.rs` | 変更 | Agent 固有 SOUL/AGENTS 読み込み |
| `src/channels/*.rs` | 変更 | 既存 context 生成に `default_agent` を設定 |
| `docs/config.md` | 変更 | Agent config 仕様を追記 |
| `docs/system-prompt.md` | 変更 | Agent prompt 順序を追記 |
| `docs/session-lifecycle.md` | 変更 | Agent session identity を追記 |
| `docs/directory.md` | 変更 | `agents/` ディレクトリを追記 |
| `docs/multi-agent.md` | 変更 | 実装 plan と整合するよう更新 |

---

## コミット分割

1. `feat: add agent configuration model` — `src/config/*`, `src/tools/sanitizer.rs`
2. `feat: resolve llm settings through agents` — `src/config/resolve.rs`, `src/llm_profile.rs`
3. `feat: carry agent identity in surface context` — `src/agent_loop/mod.rs`, `src/agent_loop/session.rs`, channel context 生成部
4. `feat: pass agent context through slash commands` — `src/slash_commands.rs`, slash command 呼び出し元
5. `feat: load prompt files from agent directories` — `src/soul_agents.rs`, `src/agent_loop/turn.rs`
6. `docs: document multi-agent core behavior` — `docs/*`

---

## テストケース一覧（全 35 件）

### Config Agent Model (5)
1. `loads_agents_with_default_agent` — `default_agent` と `agents` を読み込める
2. `default_agent_falls_back_to_default_when_missing` — 未指定時に `default` を使う
3. `rejects_default_agent_not_in_agents` — 存在しない default agent を拒否
4. `rejects_agent_id_path_traversal` — 危険な Agent ID を拒否
5. `persists_agents_without_leaking_secret_values` — SecretRef を漏らさず保存

### Agent LLM Resolution (8)
6. `agent_model_overrides_channel_model` — Agent model 優先
7. `agent_provider_overrides_channel_provider` — Agent provider 優先
8. `agent_null_model_falls_back_to_channel_model` — model fallback
9. `agent_null_provider_falls_back_to_channel_provider` — provider fallback
10. `unknown_agent_returns_config_error` — 不明 Agent をエラー化
11. `turn_uses_agent_llm_resolution` — turn が Agent LLM を使う
12. `status_uses_agent_llm_resolution` — `/status` が Agent LLM を表示する
13. `compact_uses_agent_llm_resolution` — `/compact` が Agent LLM を使う

### SurfaceContext / Session (6)
14. `surface_context_defaults_to_default_agent` — default agent 適用
15. `discord_agent_scoped_thread_includes_agent_id` — Discord helper が Agent suffix を作る
16. `same_discord_thread_different_agents_create_different_chats` — Discord helper 適用時に Agent 別 chat 分離
17. `same_discord_thread_same_agent_reuses_chat` — 同一 Discord Agent は chat 再利用
18. `web_and_telegram_keep_existing_identity_with_default_agent` — 既存 channel の互換確認
19. `web_chat_id_reentry_preserves_existing_external_chat_id` — Web 再入場互換

### Slash Command Agent Context (5)
20. `handle_status_receives_surface_context` — `/status` が Agent context を受け取る
21. `handle_compact_receives_surface_context` — `/compact` が Agent context を受け取る
22. `handle_compact_uses_agent_llm` — `/compact` が Agent LLM を使う
23. `slash_command_callers_pass_default_agent_context` — 既存 caller が default Agent context を渡す
24. `llm_profile_resolved_for_scope_keeps_channel_scope` — `/provider` `/model` 既存 scope 維持

### Prompt Loader (6)
25. `load_soul_prefers_agent_soul` — Agent SOUL 優先
26. `load_soul_falls_back_to_channel_soul_path` — `soul_path` fallback
27. `load_soul_falls_back_to_global_soul` — global fallback
28. `load_agents_combines_global_and_agent_agents` — AGENTS 累積
29. `chat_specific_md_is_ignored` — チャット別 MD 無視
30. `agent_id_path_traversal_is_rejected` — path traversal 拒否

### Docs / Integration (5)
31. `docs_reference_agent_config_fields` — config docs 確認
32. `docs_reference_agent_prompt_order` — system prompt docs 確認
33. `docs_reference_agent_session_identity` — Discord Agent suffix と Web/Telegram 互換の session docs 確認
34. `cargo_check_passes_with_agent_context` — core 統合 check
35. `clippy_passes_with_agent_context` — lint 統合 check

---

## 工数見積もり

| Step | 内容 | 見積もり |
|---|---|---|
| Step 1 | Config Agent Model | ~220 行 |
| Step 2 | Agent LLM Resolution | ~140 行 |
| Step 3 | SurfaceContext Agent Identity | ~160 行 |
| Step 4 | Slash Command Agent Context | ~160 行 |
| Step 5 | Agent SOUL / AGENTS Loader | ~220 行 |
| Step 6 | Core Docs Update | ~180 行 |
| Step 7 | 動作確認の修正余地 | ~80 行 |
| **合計** | | **~1160 行** |
