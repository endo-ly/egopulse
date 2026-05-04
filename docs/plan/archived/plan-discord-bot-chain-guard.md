# Plan: Discord Bot-to-Bot ループ防止

Discord で Bot-to-Bot の明示 mention 会話を許可しつつ、チャンネル / スレッド単位の内部チェーン状態で無限ループを防止する。Issue #26 の embed footer 方式ではなく、ユーザー向け設定を増やさない内部状態方式で実装する。

> **Note**: 以下の具体的なコード例・API 設計・構成（How）はあくまで参考である。実装時によりよい設計方針があれば積極的に採用すること。

## 設計方針

- **送信者種別で受信判定を分離**: 現在の `msg.author.bot` 全拒否をやめ、自分自身 / 人間 / 他 Bot の順に判定を明確化する。
- **Bot 発言は明示 mention 必須**: `require_mention` は人間メッセージ向けの既存設定として維持し、Bot 発言は常にこの Bot への mention がある場合だけ受け付ける。
- **チェーン状態は内部実装に閉じる**: `BOT_CHAIN_MAX_DEPTH = 5`、`BOT_CHAIN_TTL_SECS = 300` は `src/channels/discord.rs` 内の定数とし、設定仕様には追加しない。
- **複数 Bot 間で depth を共有**: `runtime.rs` で共有状態を 1 つ作成し、同一プロセス内の Discord Handler に渡すことで Bot A/B 間の往復を同じチェーンとして数える。
- **既存 slash command 挙動を維持**: text slash command は既存どおり `require_mention` 判定より前に処理し、Interaction slash command も allowed channel では mention 不要のままにする。

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 実装元 |
|---|---|
| Discord 受信判定 | `src/channels/discord.rs` |
| Bot チェーン状態管理 | `src/channels/discord.rs` |
| 複数 Discord Bot 間の状態共有 | `src/runtime.rs` |
| Discord チャネル仕様 | `docs/channels.md` |

---

## Step 0: Worktree 作成

```bash
git worktree add ../egopulse-discord-bot-chain-guard -b feat/discord-bot-chain-guard
cd ../egopulse-discord-bot-chain-guard
```

---

## Step 1: Bot チェーン状態管理 (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `bot_chain_starts_at_one` | 状態なしの Bot mention は `depth = 1` として許可される |
| `bot_chain_allows_at_max_depth` | `depth == BOT_CHAIN_MAX_DEPTH` までは許可される |
| `bot_chain_rejects_after_max_depth` | `depth > BOT_CHAIN_MAX_DEPTH` で拒否される |
| `bot_chain_resets_on_human_message` | 受理された人間メッセージで対象 channel/thread の状態がリセットされる |
| `bot_chain_ttl_expiry_restarts_at_one` | TTL 超過後は状態なしとして扱い、次の Bot mention が `depth = 1` になる |
| `bot_chain_scopes_by_thread_id` | channel/thread id が異なる状態は互いに影響しない |

### GREEN: 実装

`src/channels/discord.rs` に `BotChainState` と状態管理 helper を追加する。状態キーは `msg.channel_id.get()` とし、Discord thread では thread id がキーになる。更新時刻には `Instant` を使い、wall clock 変更の影響を避ける。

### コミット

`feat: add Discord bot chain state guard`

---

## Step 2: Discord メッセージ受信判定 (TDD)

前提: Step 1

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `self_message_is_ignored` | `msg.author.id` が自 Bot の UserId と一致するメッセージは無視される |
| `human_message_obeys_require_mention_false` | 人間の mention なし発言は `require_mention: false` なら受理される |
| `human_message_obeys_require_mention_true` | 人間の mention なし発言は `require_mention: true` なら無視される |
| `human_mentioning_this_bot_is_allowed` | 人間がこの Bot を mention している場合は受理される |
| `human_mentioning_other_bot_only_is_ignored` | 人間が他 Bot のみ mention している場合は無視される |
| `accepted_human_message_resets_bot_chain` | 受理された人間メッセージは mention 有無に関係なくチェーン状態をリセットする |
| `bot_mentioning_this_bot_is_allowed_within_depth` | Bot がこの Bot を mention し、depth 上限内なら受理される |
| `bot_without_this_bot_mention_is_ignored` | Bot の mention なし、または他 Bot のみ mention は無視される |
| `bot_mentioning_this_bot_is_ignored_after_depth_limit` | Bot mention が depth 上限を超える場合は無視される |
| `text_slash_command_keeps_existing_pre_mention_behavior` | text slash command は既存どおり `require_mention` 判定より前に処理される |

### GREEN: 実装

`Handler::message()` の冒頭にある `msg.author.bot` 全拒否を置き換え、送信者種別ごとに受信判定を行う。`Message.mentions` の `User.id` と `User.bot` を使い、この Bot mention / 他 Bot mention を判定する。Bot 発言には `require_mention` を適用せず、チェーンガードで許可された明示 mention のみ処理する。

### コミット

`feat: allow mentioned Discord bot messages with loop guard`

---

## Step 3: 複数 Discord Bot 間のチェーン状態共有 (TDD)

前提: Step 1, Step 2

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `discord_handlers_share_bot_chain_state` | 同一 runtime 内の複数 Handler が同じ channel/thread の depth を共有する |
| `discord_handlers_keep_chain_state_per_thread` | 共有状態でも channel/thread id が違えば depth は混ざらない |

### GREEN: 実装

`start_discord_bot_for_bot()` に共有チェーン状態を渡す引数を追加する。`runtime.rs` の Discord bot 起動ブロックで共有状態を 1 つ作成し、各 Bot task に clone して渡す。ロック区間は判定と状態更新だけに限定し、LLM 処理や添付ファイル処理はロック外で行う。

### コミット

`feat: share Discord bot chain guard across bots`

---

## Step 4: Discord ドキュメント更新 (TDD)

前提: Step 1, Step 2, Step 3

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `docs_discord_mentions_bot_chain_guard` | `docs/channels.md` に Bot-to-Bot 受信条件とチェーンガードが記載されていることをレビューで確認する |
| `docs_discord_mentions_multi_bot_require_mention_false` | `require_mention: false` で複数 Bot が人間の通常発言に反応し得ることが記載されていることをレビューで確認する |

### GREEN: 実装

`docs/channels.md` の Discord セクションに、Bot 発言はこの Bot への明示 mention のみ受理すること、自分自身の発言は無視すること、人間の受理メッセージでチェーン状態をリセットすること、内部 depth / TTL により Bot チェーンを停止することを追記する。内部定数はユーザー向け設定ではないため、設定テーブルには追加しない。

### コミット

`docs: document Discord bot chain guard behavior`

---

## Step 5: 動作確認

- 全テスト通過コマンド

```bash
cargo test
```

- Lint/フォーマット/型チェック

```bash
cargo fmt --check
cargo check
cargo clippy --all-targets --all-features -- -D warnings
```

---

## Step 6: PR 作成

```bash
git push -u origin feat/discord-bot-chain-guard
gh pr create --draft --base main --head feat/discord-bot-chain-guard
```

PR description は日本語で作成し、Issue #26 対応として `Close #26` を記載する。

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
|---|---|---|
| `src/channels/discord.rs` | 変更 | Bot チェーン状態、受信判定、Handler 統合、単体テストを追加 |
| `src/runtime.rs` | 変更 | 複数 Discord Bot に共有チェーン状態を渡す |
| `docs/channels.md` | 変更 | Discord Bot-to-Bot 受信ルールと制約を追記 |

---

## コミット分割

1. `feat: add Discord bot chain state guard` — `src/channels/discord.rs`
2. `feat: allow mentioned Discord bot messages with loop guard` — `src/channels/discord.rs`
3. `feat: share Discord bot chain guard across bots` — `src/channels/discord.rs`, `src/runtime.rs`
4. `docs: document Discord bot chain guard behavior` — `docs/channels.md`

---

## テストケース一覧（全 20 件）

### Bot チェーン状態管理 (6)
1. `bot_chain_starts_at_one` — 状態なしの Bot mention は `depth = 1` として許可される
2. `bot_chain_allows_at_max_depth` — `depth == BOT_CHAIN_MAX_DEPTH` までは許可される
3. `bot_chain_rejects_after_max_depth` — `depth > BOT_CHAIN_MAX_DEPTH` で拒否される
4. `bot_chain_resets_on_human_message` — 受理された人間メッセージで対象 channel/thread の状態がリセットされる
5. `bot_chain_ttl_expiry_restarts_at_one` — TTL 超過後は状態なしとして扱われる
6. `bot_chain_scopes_by_thread_id` — channel/thread id が異なる状態は互いに影響しない

### Discord メッセージ受信判定 (10)
7. `self_message_is_ignored` — 自 Bot の送信メッセージは無視される
8. `human_message_obeys_require_mention_false` — 人間の mention なし発言は `require_mention: false` なら受理される
9. `human_message_obeys_require_mention_true` — 人間の mention なし発言は `require_mention: true` なら無視される
10. `human_mentioning_this_bot_is_allowed` — 人間がこの Bot を mention している場合は受理される
11. `human_mentioning_other_bot_only_is_ignored` — 人間が他 Bot のみ mention している場合は無視される
12. `accepted_human_message_resets_bot_chain` — 受理された人間メッセージはチェーン状態をリセットする
13. `bot_mentioning_this_bot_is_allowed_within_depth` — Bot がこの Bot を mention し、depth 上限内なら受理される
14. `bot_without_this_bot_mention_is_ignored` — Bot の mention なし、または他 Bot のみ mention は無視される
15. `bot_mentioning_this_bot_is_ignored_after_depth_limit` — Bot mention が depth 上限を超える場合は無視される
16. `text_slash_command_keeps_existing_pre_mention_behavior` — text slash command は既存どおり `require_mention` 判定より前に処理される

### 複数 Discord Bot 状態共有 (2)
17. `discord_handlers_share_bot_chain_state` — 同一 runtime 内の複数 Handler が同じ channel/thread の depth を共有する
18. `discord_handlers_keep_chain_state_per_thread` — 共有状態でも channel/thread id が違えば depth は混ざらない

### ドキュメント (2)
19. `docs_discord_mentions_bot_chain_guard` — `docs/channels.md` に Bot-to-Bot 受信条件とチェーンガードが記載されていることをレビューで確認する
20. `docs_discord_mentions_multi_bot_require_mention_false` — `require_mention: false` で複数 Bot が人間の通常発言に反応し得ることが記載されていることをレビューで確認する

---

## 工数見積もり

| Step | 内容 | 見積もり |
|---|---|---|
| Step 1 | Bot チェーン状態管理 | ~120 行 |
| Step 2 | Discord メッセージ受信判定 | ~170 行 |
| Step 3 | 複数 Discord Bot 間のチェーン状態共有 | ~60 行 |
| Step 4 | Discord ドキュメント更新 | ~30 行 |
| **合計** | | **~380 行** |
