# Plan: チャネル仕様統一 — Telegram Multi-Agent 同等化 + 全チャネル session_key 統一

Telegram チャネルを Discord と同一の Multi-Agent 仕様に引き上げ、
全チャネルの session_key を `channel:thread:agent:id` 形式に統一する。
複数 Bot サポート、チャットごとのエージェント選択、TurnScheduler 経由のターン実行、
Channel Log 二層保存、`agent_send` ツールのチャネル非依存化を含む。

> **Note**: 以下の具体的なコード例・API 設計・構成（How）はあくまで参考である。実装時によりよい設計方針があれば積極的に採用すること。

## 設計方針

- **Discord 構造の踏襲**: 設定構造（`bots` マップ、`channels` マップ、`agents` / `multi_agent`）、Handler ルーティング、BotChainState 等、Discord で実績のあるパターンをそのまま Telegram に適用する。無用な独自設計を避ける
- **`agent_send` のチャネル非依存化**: 現在 `context.channel != "discord"` でハードコード拒否しているガードを、Multi-Agent 設定が存在するチャネルなら許可する形に変更する
- **`session_key` の全チャネル統一**: 全チャネルで `channel:thread:agent:id` 形式を採用。将来的なエージェント切替（`/agent alice` 等）の基盤となる
- **後方互換なし**: 単一 `bot_token` / `bot_username` は廃止し、`bots` マップに一本化する。既存ユーザーは手動で YAML を移行する

## 設計上の決定事項

| ポイント | 決定内容 | 理由 |
|---|---|---|
| `AgentConfig` の Bot 紐付け | `telegram_bot: Option<BotId>` を追加 | Discord / Telegram で別 Bot を紐付ける運用が自然。将来チャネル増時は汎用マップへリファクタ可能だが、現時点では YAGNI |
| 旧 `bot_token` / `bot_username` | **完全廃止**。`bots` マップのみ | フォールバックは負債（AGENTS.md「後方互換は負債」） |
| `session_key` 形式 | 全チャネル `channel:thread:agent:id` に統一 | エージェントごとに独立したセッションが作られ、`/agent alice` 切替の基盤になる。既存セッションは DB マイグレーションで対応 |

## Plan スコープ

WT作成済み → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 種別 |
|---|---|
| `TelegramChatConfig` | `agents` / `multi_agent` フィールド追加 |
| `ChannelConfig` | `telegram_bots` / `telegram_channels` フィールド追加 |
| `AgentConfig` | `telegram_bot: Option<BotId>` 追加 |
| Config loader / persist | Telegram `bots` / `channels` のパース・保存対応 |
| Config resolve | `telegram_bots()` / `telegram_channels()` 追加 |
| `session_key` | 全チャネルで `channel:thread:agent:id` 統一 |
| DB マイグレーション | 全チャネルの既存セッションキー移行 |
| Telegram handler | Multi-Agent ルーティング、TurnScheduler、Channel Log、BotChainState |
| `agent_send` | Discord ガード解除 |
| Runtime 起動 | Telegram 複数 Bot の `tokio::spawn` ループ |
| Setup Wizard | Telegram の複数 Bot 入力対応 |
| Docs | `config.md` / `channels.md` / `tools.md` / `channel-parity.md` 更新 |

---

## Step 0: Worktree 作成

済み（`wt-unify-channel-config` / `refactor/unify-channel-config`）

---

## Step 1: 設定型の拡張 (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `telegram_chat_config_accepts_agents_and_multi_agent` | `TelegramChatConfig` に `agents: Vec<AgentId>`, `multi_agent: bool` を設定できる |
| `channel_config_accepts_telegram_bots` | `ChannelConfig` の `telegram_bots: Option<HashMap<BotId, TelegramBotConfig>>` を設定できる |
| `channel_config_accepts_telegram_channels` | `ChannelConfig` の `telegram_channels: Option<HashMap<i64, TelegramChatConfig>>` を設定できる |
| `agent_config_accepts_telegram_bot` | `AgentConfig` に `telegram_bot: Option<BotId>` を設定できる |

### GREEN: 実装

- `TelegramChatConfig` に `agents: Vec<AgentId>`, `multi_agent: bool` を追加
- `TelegramBotConfig` を新設（`token: Option<ResolvedValue>`, `file_token: Option<yaml_serde::Value>` — `DiscordBotConfig` と同構造）
- `ChannelConfig` に `telegram_bots`, `telegram_channels` を追加
- `AgentConfig` に `telegram_bot: Option<BotId>` を追加
- `Debug` impl を更新

### コミット

`refactor(config): extend Telegram config types for multi-agent parity with Discord`

---

## Step 2: Config Loader / Persist の対応 (TDD)

前提: Step 1

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `telegram_bots_parse_from_yaml` | `channels.telegram.bots` マップをパースして `telegram_bots` に格納 |
| `telegram_channels_parse_from_yaml` | `channels.telegram.channels` マップをパースして `telegram_channels` に格納（`agents` / `multi_agent` 含む） |
| `telegram_chat_config_defaults_agents_to_default_agent` | `agents` 未設定時は `default_agent` が設定される |
| `telegram_bot_token_secret_ref` | Telegram Bot トークンの SecretRef 解決 |
| `save_load_round_trip_preserves_telegram_bots` | save → load で `telegram_bots` / `telegram_channels` が往復保存される |
| `legacy_bot_token_field_rejected` | 旧 `bot_token` / `bot_username` 単独指定はパースエラーまたは警告 |

### GREEN: 実装

- `FileTelegramChatConfig` に `agents`, `multi_agent` 追加
- `FileTelegramBotConfig` を新設（`token: Option<StringOrRef>`, `username: Option<String>` — Telegram は username が API から取得できないため必須）
- `FileChannelConfig` の `bots` / `channels` フィールドを共用構造とし、Telegram の `channels.telegram.bots` / `channels.telegram.channels` としてパース
- `normalize_telegram_bots()` / `normalize_telegram_channels()` を追加
- 旧 `bot_token` / `bot_username` フィールドは削除（persist 側も新形式のみ出力）
- persist 側の YAML シリアライズ対応

### コミット

`refactor(config): add Telegram multi-bot/channel parsing and persistence, remove legacy single-bot fields`

---

## Step 3: Config Resolve メソッド追加 (TDD)

前提: Step 2

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `telegram_bots_returns_only_bots_with_token` | トークン解決済みの Bot のみ返却 |
| `telegram_bots_disabled_channel_returns_empty` | Telegram 無効時は空ベクタ |
| `telegram_channels_returns_configured_map` | チャンネル設定マップを返却 |
| `telegram_channels_empty_when_not_configured` | 未設定時は空マップ |

### GREEN: 実装

- `Config::telegram_bots()` 追加（`TelegramBotRuntime` を返す。`username` も含む）
- `Config::telegram_channels()` 追加
- `Config::telegram_bot_token()` / `telegram_bot_username()` を `bots` マップの先頭要素から解決する形に更新（旧フィールド参照を削除）

### コミット

`feat(config): add telegram_bots() and telegram_channels() resolve methods`

---

## Step 4: `session_key` の全チャネル統一 (TDD)

前提: Step 1

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `discord_session_key_includes_agent` | Discord の `session_key` が `discord:123:agent:alice` 形式（既存と同じ） |
| `telegram_session_key_includes_agent` | Telegram の `session_key` が `telegram:-1001234:agent:default` 形式 |
| `cli_session_key_includes_agent` | CLI の `session_key` が `cli:mysession:agent:default` 形式 |
| `tui_session_key_includes_agent` | TUI の `session_key` が `tui:local-xxx:agent:default` 形式 |
| `web_session_key_includes_agent` | Web の `session_key` が `web:s1:agent:default` 形式 |

### GREEN: 実装

- `SurfaceContext::session_key()` からチャネル条件分岐（`self.channel == "discord"` 等）を削除
- 常に `format!("{}:{}:agent:{}", channel, thread, agent_id)` を返す

### コミット

`refactor(agent-loop): unify session_key format for all channels`

---

## Step 5: DB マイグレーション — 全チャネルの既存セッションキー移行 (TDD)

前提: Step 4

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `migration_renames_telegram_session_keys` | `telegram:xxx` → `telegram:xxx:agent:default` |
| `migration_renames_cli_session_keys` | `cli:xxx` → `cli:xxx:agent:default` |
| `migration_renames_tui_session_keys` | `tui:xxx` → `tui:xxx:agent:default` |
| `migration_renames_web_session_keys` | `web:xxx` → `web:xxx:agent:default` |
| `migration_skips_already_migrated_keys` | 既に `:agent:` を含むキーはスキップ |
| `migration_skips_discord_keys` | `discord:` は既に `:agent:` を含むため変更しない |
| `migration_is_idempotent` | 複数回実行しても安全 |

### GREEN: 実装

- `SCHEMA_VERSION` をインクリメント
- `run_migrations` に新バージョンのブロックを追加
- `chats` テーブルの `external_chat_id` が `:agent:` を含まないレコードを対象に、`|| ':agent:default'` で更新
- `WHERE external_chat_id NOT LIKE '%:agent:%'` で一括処理（discord は既に含むのでスキップされる）
- トランザクション内で実行

### コミット

`feat(storage): add DB migration for session_key format unification across all channels`

---

## Step 6: Telegram Handler の Multi-Agent ルーティング (TDD)

前提: Step 3, Step 4

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `route_accepts_dm_with_default_agent` | DM → デフォルトエージェント |
| `route_rejects_unauthorized_group` | `channels` マップ外のグループを拒否 |
| `route_responds_with_bound_agent_in_single_channel` | Single-Agent チャネルでバインドされたエージェントが応答 |
| `route_rejects_single_channel_for_unbound_bot` | Bot バインディングがないエージェントは拒否 |
| `route_responds_in_multi_agent_room_with_mention` | Multi-Agent Room で @mention された Bot が応答 |
| `route_observes_without_mention_in_multi_room` | Multi-Agent Room で @mention なし → ObserveOnly |
| `should_process_message_human_resets_chain` | 人間メッセージで BotChainState がリセット |
| `should_process_message_bot_within_depth` | Bot メッセージが連鎖深さ制限内で受理 |
| `should_process_message_bot_exceeds_depth` | 連鎖深さ超過で Bot メッセージ拒否 |

### GREEN: 実装

- `TelegramHandler` 構造体を新設（Discord の `Handler` 構造を踏襲）
- `route_message()`, `should_process_message()`, `make_context()` メソッド
- `BotChainState` を Telegram でも使用（既存のものを共有または新規インスタンス）
- Single-Agent / Multi-Agent / DM のルーティング
- `TelegramChatConfig` の `agents` / `multi_agent` を参照
- @mention 判定は既存の `bot_username` ベースのロジックを活用。Multi-Agent Room では mention された Bot に紐づくエージェントを特定

### コミット

`feat(telegram): add multi-agent routing and bot chain guard`

---

## Step 7: Telegram TurnScheduler 経由のターン実行 + Channel Log (TDD)

前提: Step 6

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `telegram_submits_turn_to_scheduler` | メッセージ受信時に `TurnScheduler::submit()` が呼ばれる |
| `telegram_issues_origin_id` | 人間メッセージに UUID `origin_id` が発行される |
| `telegram_stores_channel_log_in_multi_room` | Multi-Agent Room で Channel Log に保存 |
| `telegram_typing_indicator_uses_trait` | `begin_turn_activity()` が typing indicator を制御 |

### GREEN: 実装

- `handle_message` を書き換え: `process_turn` 直接呼び出し → `ScheduledTurn` 構築 → `TurnScheduler::submit()`
- Channel Log 二層保存（Discord と同様）
- `origin_id` 発行
- `TelegramAdapter::begin_turn_activity()` を実装（既存の handler 内タイピングロジックを trait に移行）
- `execute_scheduled_turn` から `ChannelAdapter::begin_turn_activity()` が呼ばれる仕組みは Discord と同じ

### コミット

`feat(telegram): use TurnScheduler and add Channel Log for multi-agent`

---

## Step 8: `agent_send` のチャネル非依存化 (TDD)

前提: Step 4

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `agent_send_works_on_telegram_multi_agent` | Telegram チャネルでも `agent_send` が成功する |
| `agent_send_rejects_on_non_multi_agent_channel` | Multi-Agent 未対応チャネル（CLI 等）では拒否 |
| `agent_send_registered_when_telegram_bots_exist` | Telegram Bot がある場合 `agent_send` が ToolRegistry に登録される |
| `agent_send_existing_discord_tests_still_pass` | 既存の Discord テストが全て通る |

### GREEN: 実装

- `agent_send.rs` の `context.channel != "discord"` ガードを削除
- 代わりに、Discord / Telegram チャネルでは許可する条件に変更（`matches!(channel, "discord" | "telegram")`）
- Runtime の `ToolRegistry` 登録条件を `discord_bots` OR `telegram_bots` に変更

### コミット

`refactor(tools): make agent_send channel-agnostic for Discord and Telegram`

---

## Step 9: Runtime 起動の複数 Bot 対応 (TDD)

前提: Step 3, Step 7

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `runtime_starts_multiple_telegram_bots` | 複数の Telegram Bot がそれぞれ `tokio::spawn` で起動する |
| `runtime_warns_when_telegram_enabled_without_bots` | Telegram 有効だが Bot トークン未設定時に警告ログ |

### GREEN: 実装

- `src/runtime/mod.rs` の Telegram 起動セクションを Discord と同様のループ構造に書き換え
- `telegram_bots()` で取得した Bot ごとに `start_telegram_bot_for_bot()` を呼ぶ
- `start_telegram_bot_for_bot()` を新設（Discord の `start_discord_bot_for_bot` 相当）
- Bot ごとに独立した `teloxide::Dispatcher` を構築

### コミット

`feat(runtime): start multiple Telegram bots with per-bot dispatchers`

---

## Step 10: Setup Wizard 対応

前提: Step 2

### GREEN: 実装（ウィザードのテストは UI フレームワーク依存のため手動確認）

- Setup wizard の Telegram フォームを `bots` マップ形式に更新
- Bot トークン + Bot username のペアを入力可能にする
- 旧 `bot_token` / `bot_username` の単一入力モードは廃止

### コミット

`feat(setup): support multiple Telegram bots in setup wizard`

---

## Step 11: Docs 更新

### 対象ファイル

- `docs/config.md`: §2.5 Telegram 設定を `bots` / `channels` 構造に更新、旧 `bot_token` / `bot_username` の廃止を明記
- `docs/channels.md`: §4 Telegram を Multi-Agent 対応仕様に更新、§1 共通アーキテクチャの Multi-Agent 対応状況テーブル更新
- `docs/tools.md`: §12 `agent_send` のチャネル条件を更新
- `docs/channel-parity.md`: 完了後の状態に更新

### コミット

`docs: update config, channels, tools, channel-parity docs for unified channel spec`

---

## Step 12: 動作確認

```bash
cargo fmt --check
cargo check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

---

## Step 13: PR 作成

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
|---|---|---|
| `src/config/types.rs` | 変更 | `TelegramChatConfig`, `TelegramBotConfig`, `AgentConfig`, `ChannelConfig` 拡張 |
| `src/config/loader.rs` | 変更 | Telegram `bots` / `channels` パース追加、旧フィールド削除 |
| `src/config/persist.rs` | 変更 | Telegram `bots` / `channels` シリアライズ |
| `src/config/resolve.rs` | 変更 | `telegram_bots()`, `telegram_channels()` 追加、旧メソッド更新 |
| `src/config/tests.rs` | 変更 | 新フィールドのテスト |
| `src/agent_loop/mod.rs` | 変更 | `session_key` 全チャネル統一 |
| `src/storage/migration.rs` | 変更 | `SCHEMA_VERSION` インクリメント + 全チャネルマイグレーション |
| `src/channels/telegram.rs` | 変更 | Multi-Agent Handler、TurnScheduler、Channel Log、BotChainState |
| `src/tools/agent_send.rs` | 変更 | Discord ガード解除 |
| `src/tools/mod.rs` | 変更 | 登録条件更新 |
| `src/runtime/mod.rs` | 変更 | Telegram 複数 Bot 起動ループ |
| `src/setup/summary.rs` | 変更 | 複数 Bot 入力対応 |
| `src/setup/channels.rs` | 変更 | 複数 Bot 入力対応 |
| `src/setup/mod.rs` | 変更 | フォーム定義更新 |
| `docs/config.md` | 変更 | Telegram 設定仕様更新 |
| `docs/channels.md` | 変更 | Telegram 仕様更新、session_key 記載更新 |
| `docs/tools.md` | 変更 | `agent_send` チャネル条件更新 |
| `docs/channel-parity.md` | 変更 | 完了状態に更新 |

---

## コミット分割

1. `refactor(config): extend Telegram config types for multi-agent parity with Discord`
2. `refactor(config): add Telegram multi-bot/channel parsing and persistence, remove legacy single-bot fields`
3. `feat(config): add telegram_bots() and telegram_channels() resolve methods`
4. `refactor(agent-loop): unify session_key format for all channels`
5. `feat(storage): add DB migration for session_key format unification across all channels`
6. `feat(telegram): add multi-agent routing and bot chain guard`
7. `feat(telegram): use TurnScheduler and add Channel Log for multi-agent`
8. `refactor(tools): make agent_send channel-agnostic for Discord and Telegram`
9. `feat(runtime): start multiple Telegram bots with per-bot dispatchers`
10. `feat(setup): support multiple Telegram bots in setup wizard`
11. `docs: update config, channels, tools, channel-parity docs for unified channel spec`

---

## テストケース一覧（全 46 件）

### 設定型 (4)
1. `telegram_chat_config_accepts_agents_and_multi_agent` — `TelegramChatConfig` の新フィールド
2. `channel_config_accepts_telegram_bots` — `ChannelConfig.telegram_bots`
3. `channel_config_accepts_telegram_channels` — `ChannelConfig.telegram_channels`
4. `agent_config_accepts_telegram_bot` — `AgentConfig.telegram_bot`

### Loader / Persist (6)
5. `telegram_bots_parse_from_yaml` — `bots` マップの YAML パース
6. `telegram_channels_parse_from_yaml` — `channels` マップの YAML パース
7. `telegram_chat_config_defaults_agents_to_default_agent` — agents 未設定時フォールバック
8. `telegram_bot_token_secret_ref` — SecretRef 解決
9. `save_load_round_trip_preserves_telegram_bots` — 往復保存
10. `legacy_bot_token_field_rejected` — 旧形式の拒否/警告

### Resolve (4)
11. `telegram_bots_returns_only_bots_with_token` — トークンありBotのみ
12. `telegram_bots_disabled_channel_returns_empty` — 無効時は空
13. `telegram_channels_returns_configured_map` — 設定マップ返却
14. `telegram_channels_empty_when_not_configured` — 未設定時は空

### session_key 全チャネル統一 (5)
15. `discord_session_key_includes_agent` — Discord 既存形式不变
16. `telegram_session_key_includes_agent` — Telegram `:agent:id` 付き
17. `cli_session_key_includes_agent` — CLI `:agent:default` 付き
18. `tui_session_key_includes_agent` — TUI `:agent:default` 付き
19. `web_session_key_includes_agent` — Web `:agent:default` 付き

### DB マイグレーション (7)
20. `migration_renames_telegram_session_keys` — `telegram:xxx` → `telegram:xxx:agent:default`
21. `migration_renames_cli_session_keys` — `cli:xxx` → `cli:xxx:agent:default`
22. `migration_renames_tui_session_keys` — `tui:xxx` → `tui:xxx:agent:default`
23. `migration_renames_web_session_keys` — `web:xxx` → `web:xxx:agent:default`
24. `migration_skips_already_migrated_keys` — 既に `:agent:` 含むキーはスキップ
25. `migration_skips_discord_keys` — `discord:` は既に `:agent:` 含むため変更なし
26. `migration_is_idempotent` — 複数回実行しても安全

### Telegram ルーティング (9)
27. `route_accepts_dm_with_default_agent` — DM → デフォルト
28. `route_rejects_unauthorized_group` — 未許可グループ拒否
29. `route_responds_with_bound_agent_in_single_channel` — Single-Agent バインディング
30. `route_rejects_single_channel_for_unbound_bot` — 未バインドBot拒否
31. `route_responds_in_multi_agent_room_with_mention` — mention 時応答
32. `route_observes_without_mention_in_multi_room` — 非mention 時 ObserveOnly
33. `should_process_message_human_resets_chain` — 人間メッセージでリセット
34. `should_process_message_bot_within_depth` — 連鎖深さ制限内
35. `should_process_message_bot_exceeds_depth` — 連鎖深さ超過

### TurnScheduler + Channel Log (4)
36. `telegram_submits_turn_to_scheduler` — TurnScheduler submit
37. `telegram_issues_origin_id` — origin_id 発行
38. `telegram_stores_channel_log_in_multi_room` — Channel Log 保存
39. `telegram_typing_indicator_uses_trait` — trait 実装

### agent_send (4)
40. `agent_send_works_on_telegram_multi_agent` — Telegram で成功
41. `agent_send_rejects_on_non_multi_agent_channel` — 非対応チャネルで拒否

### Runtime (2)
42. `runtime_starts_multiple_telegram_bots` — 複数 Bot 起動
43. `runtime_warns_when_telegram_enabled_without_bots` — 警告ログ

---

## 工数見積もり

| Step | 内容 | 見積もり |
|---|---|---|
| Step 1 | 設定型拡張 | ~80 行 |
| Step 2 | Loader / Persist | ~220 行 |
| Step 3 | Resolve メソッド | ~80 行 |
| Step 4 | session_key 全チャネル統一 | ~40 行 |
| Step 5 | DB マイグレーション | ~80 行 |
| Step 6 | Telegram ルーティング | ~350 行 |
| Step 7 | TurnScheduler + Channel Log | ~250 行 |
| Step 8 | agent_send 非依存化 | ~60 行 |
| Step 9 | Runtime 複数 Bot | ~100 行 |
| Step 10 | Setup Wizard | ~80 行 |
| Step 11 | Docs 更新 | ~200 行 |
| **合計** | | **~1,540 行** |
