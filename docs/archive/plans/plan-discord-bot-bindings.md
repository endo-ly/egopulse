# Plan: Discord Bot内Agent定義への移行

Discordマルチエージェント設定を、`agents.<id>.discord.*` から `channels.discord.bots.<bot_id>` に移行する。Botを外部接続単位、AgentをAI人格単位として分離し、1 Bot = 1 Agent と 1 Bot = 複数Agent の両方を最小構成で扱えるようにする。

> **Note**: 以下の具体的なコード例・API 設計・構成（How）はあくまで参考である。実装時によりよい設計方針があれば積極的に採用すること。

## 設計方針

- Bot token は Agent ではなく Bot に属するものとして、`channels.discord.bots.<bot_id>.token` に集約する。
- Bot は必ず `default_agent` を持ち、通常応答先を明示する。チャンネル別切替は任意の `channel_agents` で上書きする。
- ルーティング優先順位は `channel_agents[channel_id]` → `default_agent` の2段階に限定し、汎用 `routes` はまだ導入しない。
- Discord handler が作る `surface_thread` は `{channel_id}:bot:{bot_id}:agent:{agent_id}` に統一する。`SurfaceContext::session_key()` が channel prefix を付けるため、DBの `external_chat_id` は `discord:{channel_id}:bot:{bot_id}:agent:{agent_id}` になる。
- Setup wizard が作る `channels.discord.bots.default.token` は `source: env, id: DISCORD_BOT_TOKEN` を正規の保存先にする。追加BotはYAMLで任意のenv idを明示する。
- 後方互換フォールバックは追加しない。旧 `agents.<id>.discord.*` と `channels.discord.bot_token` はDiscord起動経路から外し、新仕様へ一直線に置き換える。

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 実装元 |
|---|---|
| Config schema / loader / persist | `src/config/mod.rs`, `src/config/loader.rs`, `src/config/persist.rs`, `src/config/resolve.rs` |
| Discord runtime / adapter / handler | `src/runtime.rs`, `src/channels/discord.rs` |
| Setup wizard | `src/setup/channels.rs`, `src/setup/summary.rs`, `src/setup/mod.rs` |
| Secret redaction / status | `src/tools/sanitizer.rs`, `src/status.rs` |
| Documentation | `docs/config.md`, `docs/multi-agent.md`, `docs/session-lifecycle.md`, `docs/security.md`, 必要に応じて `docs/commands.md` |

---

## Step 0: Worktree 作成

既存PR #16のブランチを更新する場合も、実装前に作業用WTを明示する。

```bash
git fetch origin
git worktree add ../wt-discord-bot-bindings feat/discord-multi-bot-agents
cd ../wt-discord-bot-bindings
git pull
```

### コミット

なし。

---

## Step 1: Config Schema を Bot内定義へ移行 (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `loads_discord_bots_with_default_agent` | `channels.discord.bots.main.default_agent` と token を読み込める |
| `discord_bots_validate_default_agent_exists` | `default_agent` が `agents` に存在しない場合は config load で失敗 |
| `discord_bots_validate_channel_agents_exist` | `channel_agents` の参照先Agentが存在しない場合は config load で失敗 |
| `discord_bots_ignore_agents_discord_tokens` | `agents.<id>.discord.bot_token` だけでは Discord Bot として解決されない |
| `discord_bots_preserve_secret_refs_on_save` | Bot token の SecretRef が保存時に平文化しない |

### GREEN: 実装

`channels.discord.bots` を読み込む構造を追加する。参考形:

```yaml
channels:
  discord:
    enabled: true
    bots:
      main:
        token:
          source: env
          id: DISCORD_BOT_TOKEN_MAIN
        default_agent: assistant
        allowed_channels:
          - 1234567890
        channel_agents:
          "9876543210": reviewer
```

実装の目安:

- `BotId` newtype を追加し、空文字・`..`・`/`・`\`・`:` を拒否する。
- `DiscordBotConfig` を追加し、`token`, `file_token`, `default_agent`, `allowed_channels`, `channel_agents` を持たせる。
- `ChannelConfig` に Discord用 `bots` map を追加するか、よりよい構成があれば同等の責務分離で実装する。
- `AgentDiscordConfig` はDiscord起動経路から外す。不要なら削除する。
- `channels.discord.bot_token` はDiscord runtime用には読まない。Telegram等の既存 channel token 構造は壊さない。
- loaderのプロセス環境変数オーバーライドは、Discordについては `channels.discord.bot_token` へ注入しない。Bot tokenは `channels.discord.bots.*.token` の SecretRef 解決だけで扱う。
- loaderで `default_agent` / `channel_agents` の参照整合性を fail-fast 検証する。
- persistで `channels.discord.bots.*.token` の SecretRef を保持し、`.env` 出力対象に含める。

### コミット

`refactor: move discord bot config under channel bots`

---

## Step 2: Discord Bot Resolver を Bot単位へ置換 (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `discord_bots_returns_only_channel_bots_with_token` | `channels.discord.bots` の token ありBotだけ runtime bot として返す |
| `discord_bots_sort_by_bot_id` | 起動順が安定するよう `bot_id` 順に並ぶ |
| `discord_bots_disabled_channel_returns_empty` | `channels.discord.enabled=false` なら空 |
| `discord_bot_allowed_channels_empty_means_guild_reject` | allowed_channels 未指定は空sliceとして返り、Handler側でギルド拒否に使える |
| `discord_bot_channel_agents_are_preserved` | channel_id → agent_id の対応が runtime に渡る |

### GREEN: 実装

`Config::discord_agent_bots()` を `Config::discord_bots()` のようなBot中心APIに置き換える。

返却構造の参考:

```rust
pub struct DiscordBotRuntime<'a> {
    pub bot_id: &'a BotId,
    pub token: &'a str,
    pub default_agent: &'a AgentId,
    pub allowed_channels: &'a [u64],
    pub channel_agents: &'a HashMap<u64, AgentId>,
}
```

実装の目安:

- runtimeに必要な情報は Agent ではなく Bot から取得する。
- `default_agent` の label 表示が必要なら resolver か handler で AgentConfig から引く。
- 旧 `DiscordAgentBot` は削除または新構造へ置換する。

### コミット

`refactor: resolve discord runtime bots by bot id`

---

## Step 3: Discord Runtime / Handler を Bot中心にする (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `handler_uses_default_agent_without_channel_override` | `channel_agents` にないチャンネルでは `default_agent` を使う |
| `handler_uses_channel_agent_override` | `channel_agents[channel_id]` があればそのAgentを使う |
| `handler_rejects_unlisted_guild_channel` | `allowed_channels` にないギルドチャンネルは無視 |
| `handler_allows_dm_with_default_agent` | DMは `allowed_channels` に関係なく `default_agent` を使う |
| `discord_surface_thread_includes_bot_and_agent` | surface_thread が `{channel_id}:bot:{bot_id}:agent:{agent_id}` になる |
| `discord_session_key_prefixes_channel_once` | `SurfaceContext::session_key()` が `discord:{channel_id}:bot:{bot_id}:agent:{agent_id}` になる |
| `discord_adapter_selects_token_by_bot_id` | outbound送信時に external_chat_id の `bot_id` から token を選ぶ |
| `discord_adapter_rejects_missing_bot_suffix` | legacy suffixなしIDでは token fallback せずエラー |

### GREEN: 実装

`Handler` に `bot_id`, `default_agent`, `channel_agents` を持たせ、入力ごとにAgentを決める。

実装の目安:

- Agent選択関数を小さく切る: `select_agent(channel_id, is_dm) -> AgentId`。
- context生成を `make_context(user, channel_id, agent_id)` に変更する。
- `surface_thread` には `discord:` prefix を手で付けない。channel prefix は `SurfaceContext::session_key()` に一元化する。
- slash message / application interaction / 通常message で同じ context / chat_id 解決を使う。
- outbound adapterは `bot_id -> token` map を持ち、`parse_discord_bot_id()` で token を選ぶ。
- outbound adapterはDB保存済みの `external_chat_id` を受け取るため、`parse_discord_chat_id()` は任意の `discord:` prefix と `:bot:<bot_id>:agent:<agent_id>` suffix を剥がして channel_id を得る。
- `start_discord_bot_for_agent` は `start_discord_bot_for_bot` に置換する。

### コミット

`feat: route discord bot messages to configured agents`

---

## Step 4: Runtime 起動 / Status を Bot単位へ更新 (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `runtime_registers_discord_adapter_when_bots_exist` | Discord Bot設定がある場合、DiscordAdapterが1つ登録される |
| `startup_status_counts_discord_bots` | statusが agent数ではなく bot数を表示する |
| `discord_enabled_without_bots_warns` | Discord enabledだがBot tokenなしなら警告し、active channel扱いしない |

### GREEN: 実装

`runtime.rs` の起動処理を `discord_bots()` ベースに置き換える。

実装の目安:

- `ChannelRegistry` には DiscordAdapter を1つだけ登録する。
- Discord gateway client は Botごとにspawnする。
- handle名は `discord[{bot_id}]` にする。
- statusの `agent_count` は `bot_count` へ改名するか、既存型を使う場合は表示文言を `bot(s)` に変更する。

### コミット

`refactor: start discord clients from bot configs`

---

## Step 5: Setup Wizard を新Configへ移行 (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `setup_writes_discord_default_bot` | Discord有効時に `channels.discord.bots.default` を書く |
| `setup_does_not_write_agent_discord_token` | setup出力に `agents.default.discord.bot_token` が含まれない |
| `setup_preserves_existing_discord_bot_token` | 再設定時に既存 `channels.discord.bots.default.token` をフォームへ復元する |
| `build_channel_configs_keeps_discord_channel_without_legacy_token` | `channels.discord.bot_token` を生成しない |

### GREEN: 実装

Setupの入力UIは当面 `DISCORD_BOT_TOKEN` のままでよいが、保存先は `channels.discord.bots.default.token` にする。

実装の目安:

- env契約は固定する: setupが作る default Bot は必ず `source: env, id: DISCORD_BOT_TOKEN` を使い、`.env` に `DISCORD_BOT_TOKEN=<token>` を保存する。
- `DISCORD_AGENT_BOT_TOKEN_ENV_NAME` は廃止する。`DISCORD_BOT_TOKEN_DEFAULT` は導入しない。
- 複数Botを手動追加する場合は、ユーザーが `DISCORD_BOT_TOKEN_REVIEWER` など任意のenv idをYAMLに明示する。
- 再読み込みは新Configの `channels.discord.bots.default.token` だけを見る。
- 旧 `channels.discord.bot_token` / `agents.default.discord.bot_token` からのフォールバックは追加しない。

### コミット

`fix: write setup discord token to channel bot config`

---

## Step 6: Redaction / Docs を新仕様へ同期 (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `collect_config_secrets_extracts_discord_bot_tokens` | `channels.discord.bots.*.token` がredaction対象になる |
| `collect_config_secrets_ignores_removed_agent_discord_tokens` | 削除後のAgent側Discord tokenに依存しない |
| `docs_describe_bot_default_agent_and_channel_agents` | ドキュメントに最小YAML例と優先順位があることを確認する手動チェック項目 |

### GREEN: 実装

docsとredactionを新仕様に合わせる。

実装の目安:

- `docs/multi-agent.md` はBot内定義を主仕様として書き直す。
- `docs/config.md` から `agents.<id>.discord.*` を削除し、`channels.discord.bots.*` を追加する。
- `docs/session-lifecycle.md` では、Discordの `surface_thread` は `{channel_id}:bot:{bot_id}:agent:{agent_id}`、保存される session key / `external_chat_id` は `discord:{channel_id}:bot:{bot_id}:agent:{agent_id}` と明記する。
- `docs/security.md` / `docs/commands.md` の秘匿フィールド・設定マトリクスを更新する。
- `tools/sanitizer.rs` が `channels.discord.bots.*.token` を漏れなくredactする。

### コミット

`docs: document discord bot based agent routing`

---

## Step 7: 動作確認

- `cargo fmt --check`
- `cargo check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test`
- 必要に応じて `cargo test discord_bots -- --nocapture`
- 必要に応じて `cargo test channels::discord -- --nocapture`

---

## Step 8: PR 作成

- 既存PR #16を更新する場合: 上記コミットを `feat/discord-multi-bot-agents` にpushする。
- 新規PRに分ける場合: `feat/discord-bot-bindings` を作成し、PR本文に「PR #16のConfig shape変更」と明記する。
- PR description には以下を含める:
  - `channels.discord.bots.<bot_id>` へ移行したこと
  - `default_agent` / `channel_agents` の優先順位
  - legacy `agents.<id>.discord.*` と `channels.discord.bot_token` をDiscord起動に使わないこと
  - 実行した検証コマンド

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
|---|---|---|
| `src/config/mod.rs` | 変更 | BotId / DiscordBotConfig / ChannelConfig bots 追加、AgentDiscordConfig削除または非使用化 |
| `src/config/loader.rs` | 変更 | `channels.discord.bots` 読み込み、SecretRef解決、参照検証 |
| `src/config/persist.rs` | 変更 | Bot token SecretRef保存、`.env` 出力 |
| `src/config/resolve.rs` | 変更 | `discord_bots()` runtime resolver 追加 |
| `src/channels/discord.rs` | 変更 | Bot中心Handler、token選択、session identity更新 |
| `src/runtime.rs` | 変更 | BotごとのDiscord client起動、Adapter登録、status count |
| `src/setup/channels.rs` | 変更 | setup読み込み/入力保存先をBot configへ変更 |
| `src/setup/summary.rs` | 変更 | setup出力ConfigをBot configへ変更 |
| `src/setup/mod.rs` | 変更 | UI文言/既存値復元の調整 |
| `src/status.rs` | 変更 | Discord bot数表示 |
| `src/tools/sanitizer.rs` | 変更 | Bot token redaction追加 |
| `docs/config.md` | 変更 | 新YAML仕様へ更新 |
| `docs/multi-agent.md` | 変更 | Bot内定義の運用ガイドへ更新 |
| `docs/session-lifecycle.md` | 変更 | Bot+Agent session identityへ更新 |
| `docs/security.md` | 変更 | 秘匿フィールド一覧更新 |
| `docs/commands.md` | 変更 | 設定マトリクス更新 |

---

## コミット分割

1. `refactor: move discord bot config under channel bots` — config schema / loader / persist
2. `refactor: resolve discord runtime bots by bot id` — resolverとruntime用構造
3. `feat: route discord bot messages to configured agents` — Discord handler / adapter
4. `refactor: start discord clients from bot configs` — runtime / status
5. `fix: write setup discord token to channel bot config` — setup wizard
6. `docs: document discord bot based agent routing` — docs / sanitizer

---

## テストケース一覧（全 27 件）

### Config Schema / Loader / Persist (5)

1. `loads_discord_bots_with_default_agent` — Bot configを読み込む
2. `discord_bots_validate_default_agent_exists` — default_agent参照を検証する
3. `discord_bots_validate_channel_agents_exist` — channel_agents参照を検証する
4. `discord_bots_ignore_agents_discord_tokens` — Agent側Discord tokenをruntime botにしない
5. `discord_bots_preserve_secret_refs_on_save` — SecretRefを保存時に保持する

### Resolver (5)

6. `discord_bots_returns_only_channel_bots_with_token` — tokenありBotだけ返す
7. `discord_bots_sort_by_bot_id` — Bot順序を安定化する
8. `discord_bots_disabled_channel_returns_empty` — disabledなら空
9. `discord_bot_allowed_channels_empty_means_guild_reject` — allowed_channels未指定を空sliceにする
10. `discord_bot_channel_agents_are_preserved` — channel_agentsを保持する

### Discord Runtime / Handler / Adapter (8)

11. `handler_uses_default_agent_without_channel_override` — default_agentを使う
12. `handler_uses_channel_agent_override` — channel_agentsを優先する
13. `handler_rejects_unlisted_guild_channel` — 未許可ギルドを拒否する
14. `handler_allows_dm_with_default_agent` — DMをdefault_agentで許可する
15. `discord_surface_thread_includes_bot_and_agent` — surface_threadにbot_id/agent_idを含める
16. `discord_session_key_prefixes_channel_once` — session_keyに`discord:` prefixを一度だけ付ける
17. `discord_adapter_selects_token_by_bot_id` — bot_idでtokenを選ぶ
18. `discord_adapter_rejects_missing_bot_suffix` — legacy fallbackしない

### Runtime / Status (3)

19. `runtime_registers_discord_adapter_when_bots_exist` — adapter登録を確認する
20. `startup_status_counts_discord_bots` — bot数表示を確認する
21. `discord_enabled_without_bots_warns` — botなしenabledをactive扱いしない

### Setup Wizard (4)

22. `setup_writes_discord_default_bot` — setupがBot configを書く
23. `setup_does_not_write_agent_discord_token` — Agent側tokenを書かない
24. `setup_preserves_existing_discord_bot_token` — 既存Bot tokenを復元する
25. `build_channel_configs_keeps_discord_channel_without_legacy_token` — channel直下tokenを作らない

### Redaction / Docs (2)

26. `collect_config_secrets_extracts_discord_bot_tokens` — Bot tokenを秘匿対象に含める
27. `collect_config_secrets_ignores_removed_agent_discord_tokens` — 旧Agent token依存をなくす

---

## 工数見積もり

| Step | 内容 | 見積もり |
|---|---|---|
| Step 0 | WT作成 | ~0 行 |
| Step 1 | Config schema / loader / persist | ~260 行 |
| Step 2 | Resolver | ~120 行 |
| Step 3 | Discord runtime / handler / adapter | ~250 行 |
| Step 4 | Runtime起動 / Status | ~90 行 |
| Step 5 | Setup wizard | ~130 行 |
| Step 6 | Redaction / Docs | ~220 行 |
| Step 7 | 動作確認 | ~0 行 |
| Step 8 | PR作成 | ~0 行 |
| **合計** | | **~1070 行** |
