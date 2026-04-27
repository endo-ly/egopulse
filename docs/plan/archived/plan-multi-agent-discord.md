# Plan: Discord Multi-Bot Agents

Agent ごとの Discord Bot Token を使い、1 Bot Token = 1 Agent として複数 Discord client を起動する。前提として `plan-multi-agent-core.md` の Agent 設定、prompt、session identity 基盤が実装済みであること。

> **Note**: 以下の具体的なコード例・API 設計・構成（How）はあくまで参考である。実装時によりよい設計方針があれば積極的に採用すること。

## 設計方針

- Discord client は Agent ごとに起動し、Handler は起動時に確定した `agent_id` を保持する。
- `channels.discord.bot_token` / `allowed_channels` は廃止し、`agents.{agent_id}.discord` を唯一の Discord Bot 設定源にする。
- outbound は `ChannelRegistry` に複数 `"discord"` adapter を登録しない。単一の Agent-aware `DiscordAdapter` が `agent_id -> token` を保持し、`external_chat_id` から Agent と channel id を復元する。
- slash command / interaction / normal message のすべてで同じ `SurfaceContext.agent_id` を使う。
- 複数 Bot が同じ channel にいる場合でも、各 Bot の `allowed_channels` と Discord 側 mention/permission に従う。Agent 間会話や呼び分けは実装しない。

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 実装元 |
|---|---|
| Discord Agent config helpers | `src/config/resolve.rs`, `src/config/mod.rs` |
| Discord runtime 起動 | `src/runtime.rs` |
| Discord outbound registry | `src/channel_adapter.rs`, `src/runtime.rs`, `src/channels/discord.rs` |
| Discord handler Agent 化 | `src/channels/discord.rs` |
| Discord outbound parsing | `src/channels/discord.rs` |
| Slash command Agent context | `src/channels/discord.rs`, `src/slash_commands.rs` |
| Setup / WebUI / status 整合 | `src/setup/*`, `src/web/config.rs`, `src/status.rs` |
| Secret redaction | `src/tools/sanitizer.rs` |
| Docs | `docs/config.md`, `docs/deploy.md`, `docs/multi-agent.md` |

---

## Step 0: Worktree 作成

```bash
git worktree add ../egopulse-discord-multi-bot -b feat/discord-multi-bot-agents
cd ../egopulse-discord-multi-bot
```

前提: `feat/multi-agent-core` が main に merge 済み、または worktree に取り込み済み。
特に `handle_slash_command(state, chat_id, context, ...)` のように slash command が `SurfaceContext` を受け取れる状態であること。

---

## Step 1: Discord Agent Config Helpers (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `discord_agents_returns_only_agents_with_token` | token を持つ Agent だけ Discord 起動対象になる |
| `discord_agents_preserve_agent_id_and_label` | 起動対象に `agent_id` / label / allowed channels が含まれる |
| `discord_agents_reject_duplicate_token_env_id` | 同じ SecretRef ID の重複を明示エラーまたは警告対象にする |
| `discord_agents_allow_empty_allowed_channels_as_guild_reject` | 空 `allowed_channels` は既存同様ギルド全拒否として扱う |
| `discord_disabled_returns_no_agents` | `channels.discord.enabled = false` なら起動対象なし |

### GREEN: 実装

`Config::discord_agent_bots()` のような helper を追加し、runtime が直接 `agents` の内部構造を走査しすぎないようにする。token は secret redaction とログ漏えいに注意する。

### コミット

`feat: resolve discord agent bot configs`

---

## Step 2: Runtime Multi-Client Startup (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `runtime_starts_one_discord_task_per_agent_bot` | Discord Bot 設定数ぶん task を起動する |
| `runtime_skips_discord_when_no_agent_tokens` | enabled でも token なしなら警告して起動しない |
| `runtime_names_discord_tasks_with_agent_id` | handle 名やログに Agent ID が含まれる |
| `runtime_continues_other_bots_when_one_start_fails` | 1 Bot 起動失敗が他 Bot 起動を妨げない設計を確認 |
| `runtime_registers_single_discord_adapter` | `ChannelRegistry` には Agent-aware Discord adapter を1つだけ登録する |

### GREEN: 実装

`runtime.rs` の単一 `config.discord_bot_token()` 起動を、Agent bot configs の loop に置き換える。`start_discord_bot(state, token, agent_id, allowed_channels)` のように Agent context を渡す。
`build_app_state_with_path` では複数 Discord adapter を register せず、Agent token map を持つ単一 `DiscordAdapter` を register する。

### コミット

`feat: start discord clients per agent`

---

## Step 3: Discord Handler Agent Context (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `handler_uses_bound_agent_id_for_message_context` | 通常メッセージの `SurfaceContext` に handler の Agent ID が入る |
| `handler_uses_bound_agent_id_for_text_slash_command` | テキスト slash command の chat 解決に Agent ID が入る |
| `handler_uses_bound_agent_id_for_interaction` | Interaction command の chat 解決に Agent ID が入る |
| `handler_checks_agent_allowed_channels` | Agent 固有 `allowed_channels` で応答可否を判定する |
| `handler_allows_dm_even_when_allowed_channels_empty` | DM は既存通り許可する |
| `handler_passes_surface_context_to_slash_command` | slash command API に channel 文字列ではなく Agent context を渡す |

### GREEN: 実装

`Handler` に `agent_id`, `agent_label`, `allowed_channels` を持たせる。`external_chat_id` は `"{channel_id}:agent:{agent_id}"` で統一し、slash command と normal turn の二重解決も同じ helper を使う。
テキスト slash command と Interaction command は、PR1 で Agent-aware 化した `handle_slash_command` に同じ `SurfaceContext` を渡す。

### コミット

`feat: bind discord handlers to agents`

---

## Step 4: Discord Adapter External ID Parsing (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `parse_discord_chat_id_accepts_agent_suffix` | `123:agent:developer` から `123` を取り出す |
| `parse_discord_agent_id_from_external_chat_id` | `123:agent:developer` から `developer` を取り出す |
| `parse_discord_chat_id_accepts_legacy_raw` | 既存 raw id を引き続き送信できる |
| `parse_discord_chat_id_accepts_legacy_prefixed` | `discord:123` を引き続き送信できる |
| `parse_discord_chat_id_rejects_bad_agent_suffix` | `abc:agent:developer` はエラー |
| `parse_discord_chat_id_rejects_empty_channel` | `:agent:developer` はエラー |
| `discord_adapter_uses_agent_token_for_outbound` | Agent suffix に対応する token で送信する |
| `discord_adapter_rejects_unknown_agent_for_outbound` | token map にない Agent への outbound を拒否する |

### GREEN: 実装

`parse_discord_chat_id` を Agent suffix 対応にする。あわせて `parse_discord_agent_id` を追加し、`DiscordAdapter` が outbound 時に Agent token を選べるようにする。
legacy raw / `discord:` external id は default Agent token を使う。

### コミット

`fix: parse discord agent chat identities`

---

## Step 5: Setup / WebUI / Status 整合 (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `setup_writes_default_agent_discord_token` | setup が Discord 有効時に `agents.default.discord.bot_token` を生成する |
| `web_config_does_not_drop_agents` | WebUI config PUT が未知扱いで `agents` を落とさない |
| `status_reports_discord_agent_count` | status に Discord Agent Bot 数が表示される |
| `sanitizer_redacts_agent_discord_tokens` | `agents.*.discord.bot_token` が redaction 対象になる |

### GREEN: 実装

Setup は最小では `default` Agent の Discord token を生成する。WebUI は Agent 編集を提供しない場合でも、persist 時に `agents` を保持する。status は詳細 token を出さず count / agent label 程度に留める。

### コミット

`feat: align setup and status with discord agents`

---

## Step 6: Discord Docs Update (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `docs_explain_agent_discord_tokens` | `docs/config.md` が `agents.*.discord` を説明している |
| `docs_explain_multi_bot_deploy` | deploy docs が複数 Bot Token 運用を説明している |
| `docs_remove_channel_discord_token_as_primary` | `channels.discord.bot_token` が主要仕様として残っていない |

### GREEN: 実装

`docs/config.md`, `docs/deploy.md`, `docs/multi-agent.md` を更新する。既存 config からの移行メモも短く入れる。

### コミット

`docs: document discord agent bot configuration`

---

## Step 7: 動作確認

- `cargo fmt --check`
- `cargo test channels::discord`
- `cargo test runtime`
- `cargo test config`
- `cargo test setup`
- `cargo test status`
- `cargo check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- 手動確認: 2つの Discord Bot Token を別 Agent に設定し、別チャンネルでそれぞれ応答すること

---

## Step 8: PR 作成

- ブランチ: `feat/discord-multi-bot-agents`
- PR title: `feat: run discord bots per agent`
- PR description は日本語で作成する。
- PR1 の merge 後に作成する。
- 該当 Issue がある場合は `Close #XX` を明記する。

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
|---|---|---|
| `src/config/resolve.rs` | 変更 | Discord Agent Bot config helper |
| `src/channel_adapter.rs` | 変更 | 複数同名 adapter 登録を避ける前提のテスト補強 |
| `src/runtime.rs` | 変更 | Agent ごとの Discord client 起動 |
| `src/channels/discord.rs` | 変更 | Handler の Agent binding、allowed channel 判定、Agent-aware outbound adapter、external id parsing |
| `src/slash_commands.rs` | 変更 | Discord 呼び出し側の Agent context 適用確認 |
| `src/setup/channels.rs` | 変更 | setup 出力を `agents.default.discord` へ変更 |
| `src/setup/summary.rs` | 変更 | setup summary の Agent config 対応 |
| `src/web/config.rs` | 変更 | WebUI config PUT で Agent 設定保持 |
| `src/status.rs` | 変更 | Discord Agent Bot 状態表示 |
| `src/tools/sanitizer.rs` | 変更 | Agent token redaction の統合確認 |
| `docs/config.md` | 変更 | Agent Discord config 仕様 |
| `docs/deploy.md` | 変更 | 複数 Bot Token 運用 |
| `docs/multi-agent.md` | 変更 | 実装後の最終仕様へ調整 |

---

## コミット分割

1. `feat: resolve discord agent bot configs` — `src/config/resolve.rs`, config tests
2. `feat: start discord clients per agent` — `src/runtime.rs`, `src/channel_adapter.rs`
3. `feat: bind discord handlers to agents` — `src/channels/discord.rs`
4. `fix: parse discord agent chat identities` — `src/channels/discord.rs` の parsing / outbound token selection
5. `feat: align setup and status with discord agents` — `src/setup/*`, `src/web/config.rs`, `src/status.rs`
6. `docs: document discord agent bot configuration` — `docs/*`

---

## テストケース一覧（全 31 件）

### Discord Agent Config Helpers (5)
1. `discord_agents_returns_only_agents_with_token` — token あり Agent のみ起動対象
2. `discord_agents_preserve_agent_id_and_label` — Agent metadata を保持
3. `discord_agents_reject_duplicate_token_env_id` — token 参照重複の扱い
4. `discord_agents_allow_empty_allowed_channels_as_guild_reject` — 空 allowed の意味を維持
5. `discord_disabled_returns_no_agents` — Discord disabled なら起動なし

### Runtime Multi-Client Startup (5)
6. `runtime_starts_one_discord_task_per_agent_bot` — Agent 数ぶん起動
7. `runtime_skips_discord_when_no_agent_tokens` — token なしは起動しない
8. `runtime_names_discord_tasks_with_agent_id` — task/log 名に Agent ID
9. `runtime_continues_other_bots_when_one_start_fails` — 失敗分離
10. `runtime_registers_single_discord_adapter` — registry には Discord adapter を1つだけ登録

### Discord Handler Agent Context (6)
11. `handler_uses_bound_agent_id_for_message_context` — 通常 message context
12. `handler_uses_bound_agent_id_for_text_slash_command` — text slash context
13. `handler_uses_bound_agent_id_for_interaction` — interaction context
14. `handler_checks_agent_allowed_channels` — Agent allowed 判定
15. `handler_allows_dm_even_when_allowed_channels_empty` — DM 許可
16. `handler_passes_surface_context_to_slash_command` — slash command API に Agent context を渡す

### Discord Adapter Parsing / Outbound (8)
17. `parse_discord_chat_id_accepts_agent_suffix` — Agent suffix 対応
18. `parse_discord_agent_id_from_external_chat_id` — Agent ID 抽出
19. `parse_discord_chat_id_accepts_legacy_raw` — raw id 互換
20. `parse_discord_chat_id_accepts_legacy_prefixed` — `discord:` 互換
21. `parse_discord_chat_id_rejects_bad_agent_suffix` — 不正 suffix 拒否
22. `parse_discord_chat_id_rejects_empty_channel` — 空 channel 拒否
23. `discord_adapter_uses_agent_token_for_outbound` — Agent token 選択
24. `discord_adapter_rejects_unknown_agent_for_outbound` — 不明 Agent 拒否

### Setup / WebUI / Status (4)
25. `setup_writes_default_agent_discord_token` — setup 出力
26. `web_config_does_not_drop_agents` — WebUI persist 保持
27. `status_reports_discord_agent_count` — status 表示
28. `sanitizer_redacts_agent_discord_tokens` — token redaction

### Docs / Manual (3)
29. `docs_explain_agent_discord_tokens` — config docs 確認
30. `docs_explain_multi_bot_deploy` — deploy docs 確認
31. `manual_two_discord_bots_reply_as_separate_agents` — 手動 E2E

---

## 工数見積もり

| Step | 内容 | 見積もり |
|---|---|---|
| Step 1 | Discord Agent Config Helpers | ~120 行 |
| Step 2 | Runtime Multi-Client Startup | ~170 行 |
| Step 3 | Discord Handler Agent Context | ~240 行 |
| Step 4 | Discord Adapter External ID Parsing | ~150 行 |
| Step 5 | Setup / WebUI / Status 整合 | ~260 行 |
| Step 6 | Discord Docs Update | ~160 行 |
| Step 7 | 動作確認の修正余地 | ~80 行 |
| **合計** | | **~1180 行** |
