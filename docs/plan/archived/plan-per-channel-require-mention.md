# Plan: Per-Channel require_mention Configuration

Discord / Telegram でチャンネル/チャットごとに `require_mention`（@mention 必須かどうか）を設定できるようにする。既存の `allowed_channels` / `channel_agents` / `allowed_chat_ids` を構造化マップに統合し、拡張性のある per-channel 設定基盤を導入する。

> **Note**: 以下の具体的なコード例・API 設計・構成（How）はあくまで参考である。実装時によりよい設計方針があれば積極的に採用すること。

## 設計方針

- **構造化マップで統合**: `allowed_channels: [u64]` + `channel_agents: map` → `channels: map<u64, DiscordChannelConfig>` に統合。`allowed_chat_ids: [i64]` → `chats: map<i64, TelegramChatConfig>` に置き換え。キー存在 = 許可、値のフィールド = 挙動
- **後方互換は維持しない**: プロジェクト方針「後方互換は負債」に従い、旧フィールドは一括削除し新仕様へ置き換える
- **null 値のカスタムデシリアライズ**: YAML で `channels: { "123": }` のように値が null の場合、`#[serde(default)]` だけでは struct へのデシリアライズに失敗する。専用の `deserialize_with` で null → `Default::default()` に変換する
- **Discord mention 検知の新規実装**: 現在 Discord には mention 検知がない。serenity の `Message.mentions` で Bot 自身の ID を検知する
- **Slash command は allowed channel であれば常に有効**: `require_mention` は通常メッセージのみ適用。`interaction_create` には `guild_allowed` チェックのみ追加し、mention チェックは行わない
- **ホットリロード対象外**: Discord Handler は起動時に設定をコピーする固定スナップショット方式。Telegram も同様。`channels`/`chats` 変更時は再起動が必要

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | ファイル |
|---|---|
| Config 型定義 | `src/config/mod.rs` |
| YAML ローダ | `src/config/loader.rs` |
| YAML パーシスタ | `src/config/persist.rs` |
| ランタイム解決 | `src/config/resolve.rs` |
| エラー型 | `src/error.rs` |
| Discord ハンドラ | `src/channels/discord.rs` |
| Telegram ハンドラ | `src/channels/telegram.rs` |
| ランタイム起動 | `src/runtime.rs` |
| セットアップサマリ | `src/setup/summary.rs` |
| サニタイザ | `src/tools/sanitizer.rs` |
| 設定仕様ドキュメント | `docs/config.md` |
| チャネル仕様ドキュメント | `docs/channels.md` |

---

## Step 0: Worktree 作成

```bash
git worktree add ../egopulse-require-mention -b feat/per-channel-require-mention
```

---

## Step 1: Config 型 + Loader (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `discord_channels_parses_null_value` | `channels: { "123": }` → キー存在、デフォルト値 (require_mention: false, agent: None) |
| `discord_channels_parses_require_mention` | `channels: { "123": { require_mention: true } }` → require_mention が true |
| `discord_channels_parses_agent_override` | `channels: { "123": { agent: bob } }` → agent が Some |
| `discord_channels_empty_means_no_guild_allowed` | channels 未指定 → ギルドメッセージ全拒否 |
| `telegram_chats_parses_null_value` | `chats: { "123": }` → デフォルト値 |
| `telegram_chats_parses_require_mention` | `chats: { "-100456": { require_mention: true } }` → true |
| `discord_channels_invalid_key_not_u64` | キーが u64 でない場合エラー |
| `telegram_chats_invalid_key_not_i64` | キーが i64 でない場合エラー |

### GREEN: 実装

- `DiscordChannelConfig` (require_mention: bool, agent: Option<AgentId>) を新設
- `TelegramChatConfig` (require_mention: bool) を新設
- 両構造体に `#[serde(default, deserialize_with = "deserialize_null_as_default")]` を適用し、YAML 値が `null` でも `Default::default()` にフォールバックするようにする
- `DiscordBotConfig`: `allowed_channels` / `channel_agents` を削除、`channels: Option<HashMap<u64, DiscordChannelConfig>>` を追加
- `ChannelConfig`: `allowed_channels` / `allowed_chat_ids` を削除、`chats: Option<HashMap<i64, TelegramChatConfig>>` を追加
- `FileDiscordBotConfig`: 同様に `channels` フィールドに更新
- `FileChannelConfig`: 同様に `chats` フィールドに更新
- `normalize_discord_bots`: 新構造のパース + バリデーション
- `InvalidChannelAgentsKey` エラーを `InvalidChannelsKey` に更新

### コミット

`feat: replace allowlists with structured channel/chat config maps`

---

## Step 2: Persist (TDD)

前提: Step 1

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `persist_discord_channels_serializes_correctly` | channels マップが YAML に正しく出力される |
| `persist_telegram_chats_serializes_correctly` | chats マップが YAML に正しく出力される |
| `persist_skips_empty_channels` | channels/chats が None のときフィールドを出力しない |

### GREEN: 実装

- `SerializableDiscordBot` の `allowed_channels` / `channel_agents` を `channels` に置き換え
- `SerializableChannel` に `chats` フィールドを追加、`allowed_channels` / `allowed_chat_ids` を削除
- シリアライズロジックの更新

### コミット

`feat: update config serialization for structured channel maps`

---

## Step 3: Resolve + Runtime (TDD)

前提: Step 1, Step 2

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `discord_bot_runtime_exposes_channels_map` | `DiscordBotRuntime` が channels マップを正しく保持する |
| `discord_bot_runtime_empty_channels_means_no_guild` | channels が None/空 → ギルド全拒否 |
| `discord_bot_runtime_channels_with_require_mention` | channels マップから require_mention を読める |

### GREEN: 実装

- `DiscordBotRuntime`: `allowed_channels: &[u64]` と `channel_agents: HashMap` を `channels: HashMap<u64, DiscordChannelConfig>` に置き換え
- `runtime.rs`: `start_discord_bot_for_bot` の引数更新

### コミット

`feat: update DiscordBotRuntime to use structured channel config`

---

## Step 4: Discord Handler (TDD)

前提: Step 3

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `guild_allowed_channel_in_map` | channels マップに存在するチャンネルは許可 |
| `guild_rejected_channel_not_in_map` | channels マップにないチャンネルは拒否 |
| `select_agent_uses_channel_agent_override` | channels マップの agent フィールドが default_agent を上書き |
| `select_agent_falls_back_to_default` | agent フィールドが None なら default_agent |
| `require_mention_true_rejects_without_mention` | require_mention: true かつメンションなし → 拒否 |
| `require_mention_false_allows_without_mention` | require_mention: false かつメンションなし → 許可 |
| `dm_always_allowed` | DM はチャンネル設定に関わらず常に許可 |
| `interaction_rejected_in_non_allowed_channel` | interaction_create で non-allowed チャンネルは拒否 |
| `interaction_allowed_in_allowed_channel` | interaction_create で allowed チャンネルは mention 不要で許可 |

### GREEN: 実装

- `Handler` 構造体: `allowed_channels: Vec<u64>` と `channel_agents: HashMap` を `channels: HashMap<u64, DiscordChannelConfig>` に置き換え
- `guild_allowed()`: `channels.contains_key()` で判定
- `select_agent()`: `channels[channel_id].agent` → fallback `default_agent`
- `is_mentioned()`: serenity の `msg.mentions` に Bot 自身の ID が含まれるか判定
- `message()` ハンドラ: require_mention チェックを追加（通常メッセージのみ）
- `interaction_create()` ハンドラ: `guild_allowed` チェックを追加。`require_mention` は適用しない

### コミット

`feat: add per-channel require_mention and mention detection to Discord handler`

---

## Step 5: Telegram Handler (TDD)

前提: Step 3

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `chat_allowed_when_in_chats_map` | chats マップに存在するチャットは許可 |
| `chat_rejected_when_not_in_chats_map` | chats マップにないグループは拒否 |
| `require_mention_true_requires_mention` | require_mention: true のグループで @mention なし → 拒否 |
| `require_mention_false_allows_immediately` | require_mention: false のグループ → 即応答 |
| `dm_always_allowed` | DM は常に許可 |

### GREEN: 実装

- `handle_message()`: `allowed_chat_ids` 参照を `chats` マップ参照に置き換え
- `is_in_allowed_chat` ロジックを `chats.contains_key()` に変更
- `is_mentioned` ロジック: `is_in_allowed_chat && !config.require_mention` なら true、それ以外は既存のメンション検知

### コミット

`feat: update Telegram handler to use structured chats map with require_mention`

---

## Step 6: 周辺クリーンアップ (TDD)

前提: Step 1〜5

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `setup_summary_uses_new_fields` | setup で生成される Config が新フィールドを使用 |
| `sanitizer_uses_new_fields` | サニタイザのテスト用 Config が新フィールドを使用 |
| `old_error_variant_removed` | `InvalidChannelAgentsKey` がもう存在しない |

### GREEN: 実装

- `error.rs`: `InvalidChannelAgentsKey` を削除、`InvalidChannelsKey` に置き換え済み確認
- `setup/summary.rs`: `DiscordBotConfig` の新フィールドに更新
- `tools/sanitizer.rs`: 同上
- 既存テストの `allowed_channels` / `channel_agents` / `allowed_chat_ids` 参照を全て新構造に更新

### コミット

`refactor: clean up remaining references to old allowlist fields`

---

## Step 7: ドキュメント更新

### 内容

- `docs/config.md`: 2.4 / 2.5 / 2.6 セクションのフィールドテーブルと YAML 例を更新
- `docs/channels.md`: Discord / Telegram の設定テーブルとアクセス制御の説明を更新
- 再起動要否セクションの更新（`channels` / `chats` は再起動が必要なフィールドに追加）

### コミット

`docs: update config and channel docs for structured require_mention`

---

## Step 8: 動作確認

```bash
cargo fmt --check
cargo check -p egopulse
cargo clippy --all-targets --all-features -- -D warnings
cargo test -p egopulse
```

---

## Step 9: PR 作成

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
|---|---|---|
| `src/config/mod.rs` | 変更 | 新型追加、旧型更新、テスト更新 |
| `src/config/loader.rs` | 変更 | FileDiscordBotConfig / FileChannelConfig 更新、パースロジック更新 |
| `src/config/persist.rs` | 変更 | シリアライズ構造体更新 |
| `src/config/resolve.rs` | 変更 | DiscordBotRuntime 更新 |
| `src/error.rs` | 変更 | エラー型更新 |
| `src/channels/discord.rs` | 変更 | Handler 構造体更新、mention 検知追加 |
| `src/channels/telegram.rs` | 変更 | chats マップ参照に更新 |
| `src/runtime.rs` | 変更 | 起動引数更新 |
| `src/setup/summary.rs` | 変更 | 新フィールド参照 |
| `src/tools/sanitizer.rs` | 変更 | 新フィールド参照 |
| `docs/config.md` | 変更 | 設定仕様更新 |
| `docs/channels.md` | 変更 | チャネル仕様更新 |

---

## コミット分割

1. `feat: replace allowlists with structured channel/chat config maps` — config 型 + loader
2. `feat: update config serialization for structured channel maps` — persist
3. `feat: update DiscordBotRuntime to use structured channel config` — resolve + runtime
4. `feat: add per-channel require_mention and mention detection to Discord handler` — discord handler
5. `feat: update Telegram handler to use structured chats map with require_mention` — telegram handler
6. `refactor: clean up remaining references to old allowlist fields` — 周辺クリーンアップ
7. `docs: update config and channel docs for structured require_mention` — ドキュメント

---

## テストケース一覧（全 25 件）

### Config 型 + Loader (8)
1. `discord_channels_parses_null_value` — null 値でデフォルト (require_mention: false, agent: None)
2. `discord_channels_parses_require_mention` — require_mention: true のパース
3. `discord_channels_parses_agent_override` — agent オーバーライドのパース
4. `discord_channels_empty_means_no_guild_allowed` — channels 未指定でギルド全拒否
5. `telegram_chats_parses_null_value` — null 値でデフォルト
6. `telegram_chats_parses_require_mention` — require_mention: true のパース
7. `discord_channels_invalid_key_not_u64` — 不正キーエラー
8. `telegram_chats_invalid_key_not_i64` — 不正キーエラー

### Persist (3)
9. `persist_discord_channels_serializes_correctly` — Discord channels の YAML 出力
10. `persist_telegram_chats_serializes_correctly` — Telegram chats の YAML 出力
11. `persist_skips_empty_channels` — None 時のフィールド省略

### Resolve + Runtime (3)
12. `discord_bot_runtime_exposes_channels_map` — Runtime がマップを保持
13. `discord_bot_runtime_empty_channels_means_no_guild` — 空マップでギルド全拒否
14. `discord_bot_runtime_channels_with_require_mention` — require_mention 読み取り

### Discord Handler (7)
15. `guild_allowed_channel_in_map` — マップ内チャンネル許可
16. `guild_rejected_channel_not_in_map` — マップ外チャンネル拒否
17. `select_agent_uses_channel_agent_override` — agent オーバーライド
18. `select_agent_falls_back_to_default` — デフォルトエージェントフォールバック
19. `require_mention_true_rejects_without_mention` — メンションなし拒否
20. `require_mention_false_allows_without_mention` — メンションなし許可
21. `dm_always_allowed` — DM 常に許可

### Telegram Handler (5)
22. `chat_allowed_when_in_chats_map` — マップ内チャット許可
23. `chat_rejected_when_not_in_chats_map` — マップ外チャット拒否
24. `require_mention_true_requires_mention` — メンション必須
25. `require_mention_false_allows_immediately` — 即応答

### 周辺クリーンアップ (0 — 既存テスト修正のみ)

---

## 工数見積もり

| Step | 内容 | 見積もり |
|---|---|---|
| Step 1 | Config 型 + Loader | ~150 行 |
| Step 2 | Persist | ~60 行 |
| Step 3 | Resolve + Runtime | ~80 行 |
| Step 4 | Discord Handler | ~180 行 |
| Step 5 | Telegram Handler | ~120 行 |
| Step 6 | 周辺クリーンアップ | ~60 行 |
| Step 7 | ドキュメント | ~100 行 |
| **合計** | | **~750 行** |
