# Plan: Multi-Agent Room Phase 1 — Config Schema + DB Migration

Multi-Agent Room 機能の基盤として、Config schema（DiscordChannelConfig / AgentConfig）の新仕様移行と DB messages テーブルへのカラム追加を行う。本 Phase では multi-agent メッセージルーティング等のランタイム動作は実装せず、既存単一 Agent 構成での動作を維持する。

> **Note**: 以下の具体的なコード例・API 設計・構成（How）はあくまで参考である。実装時によりよい設計方針があれば積極的に採用すること。

## 設計方針

- **後方互換は負債**: 旧 `agent` フィールドは削除し、新 `agents` フィールドに一本化。既存 config は手動更新が必要（AGENTS.md 準拠）
- **正規化で補充**: `agents` 未指定 / 空のチャネル設定は、Bot の `default_agent` で `[default_agent]` に自動補充。ユーザーの config 負担を最小化
- **DB は追加のみ**: ALTER TABLE で 3 カラム追加。DEFAULT 値で既存レコードに影響なし。INSERT は当面 DEFAULT に任せる（SELECT のみ更新）
- **MessageKind は先行定義**: `Message`, `AgentSend`, `SystemEvent` の 3 variant を定義。Phase 3 で `AgentSend` が使用されるまでは `Message` のみ使用
- **DiscordBotConfig は不変**: `default_agent` は DM ルーティングと正規化フォールバックに使用。構造を変更しない

## 既知の負債（将来 Phase で解消）

| 負債 | 現状 | 理由 | 解消タイミング |
|---|---|---|---|
| INSERT クエリ未更新 | `message_kind`, `sender/recipient_agent_id` を INSERT に含めない | Phase 1 では新カラムに書き込むランタイム動作がないため DEFAULT に任せる | Phase 2+: `agent_send` 実装時に INSERT クエリを更新 |

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 実装元 |
|---|---|
| MessageKind enum | `src/storage/mod.rs` |
| Migration v7 | `src/storage/migration.rs` |
| StoredMessage + Queries | `src/storage/mod.rs`, `src/storage/queries.rs` |
| DiscordChannelConfig schema | `src/config/types.rs`, `src/config/loader.rs` |
| Config validation | `src/config/loader.rs` |
| AgentConfig.discord_bot | `src/config/types.rs`, `src/config/loader.rs` |
| Discord agent resolution | `src/channels/discord.rs` |
| Config persistence | `src/config/persist.rs` |
| Docs | `docs/config.md`, `docs/db.md`, `docs/channels.md` |

---

## Step 0: Worktree 作成

```bash
git worktree add ../egopulse-multi-agent-phase1 -b feat/multi-agent-phase1
```

---

## Step 1: MessageKind Enum (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `message_kind_display_message` | `MessageKind::Message` → `"message"` |
| `message_kind_display_agent_send` | `MessageKind::AgentSend` → `"agent_send"` |
| `message_kind_display_system_event` | `MessageKind::SystemEvent` → `"system_event"` |
| `message_kind_from_str_valid` | `"message"`, `"agent_send"`, `"system_event"` → 各 variant |
| `message_kind_from_str_unknown` | `"unknown"` → Err |

### GREEN: 実装

`src/storage/mod.rs` に `MessageKind` enum を追加。`SleepRunStatus`（同ファイル既存）のパターンに倣い `Display` + `FromStr` を実装。

### コミット

`feat(storage): add MessageKind enum for multi-agent message classification`

---

## Step 2: DB Migration v7 (TDD)

前提: Step 1

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `migration_v7_adds_columns` | migration 後に `message_kind`, `sender_agent_id`, `recipient_agent_id` が存在 |
| `migration_v7_default_values` | 既存レコードの `message_kind = 'message'`, `sender_agent_id IS NULL`, `recipient_agent_id IS NULL` を確認 |
| `migration_v7_from_v6_db` | v6 DB からの migration が正常完了 |

### GREEN: 実装

`src/storage/migration.rs`:

- `SCHEMA_VERSION` を `6` → `7` にインクリメント
- `if version < 7` ブロック追加
- transaction 内で以下の DDL を実行:
  ```sql
  ALTER TABLE messages ADD COLUMN message_kind TEXT NOT NULL DEFAULT 'message';
  ALTER TABLE messages ADD COLUMN sender_agent_id TEXT;
  ALTER TABLE messages ADD COLUMN recipient_agent_id TEXT;
  ```
- `set_schema_version_in_tx(&tx, 7, "add message_kind, sender_agent_id, recipient_agent_id to messages")` 呼び出し

### コミット

`feat(storage): migration v7 - add message_kind, sender/recipient agent columns to messages`

---

## Step 3: StoredMessage + Queries (TDD)

前提: Step 1, Step 2

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `stored_message_reads_message_kind` | SELECT 結果から MessageKind が正しく読める |
| `stored_message_reads_nullable_agent_ids` | sender / recipient_agent_id が NULL 時に `None` になる |
| `store_message_uses_defaults` | INSERT 時に新フィールド未指定で DEFAULT が入る |

### GREEN: 実装

- `StoredMessage` に `message_kind: MessageKind`, `sender_agent_id: Option<String>`, `recipient_agent_id: Option<String>` を追加
- `row_to_stored_message()` を更新: 新カラムの読み取り（`MessageKind::from_str`, `Option<String>`）
- SELECT クエリ（`get_recent_messages`, `get_all_messages`, `load_session_snapshot` 等）のカラムリストに新 3 カラムを追加
- INSERT クエリは変更しない（DEFAULT に任せる）
- テストヘルパー `store_msg()` を必要に応じて新フィールド対応（MessageKind::Message で INSERT されることを確認）

### コミット

`feat(storage): extend StoredMessage with multi-agent message fields`

---

## Step 4: DiscordChannelConfig Schema + Deserialization (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `parse_channel_config_with_agents` | `agents: [lyre]` をパースできる |
| `parse_channel_config_with_multi_agent` | `multi_agent: true, agents: [lyre, vega]` をパースできる |
| `parse_channel_config_agents_default_empty` | agents 未指定 → 空 Vec (`#[serde(default)]`) |
| `normalize_empty_agents_fills_default` | agents 空 + default_agent → `[default_agent]` に正規化 |
| `normalize_agents_keeps_explicit` | agents 指定済み → そのまま変更しない |
| `channel_config_require_mention_preserved` | require_mention が正しく維持される |

### GREEN: 実装

**`src/config/types.rs`**:

- `DiscordChannelConfig` のフィールド変更:
  - `agent: Option<AgentId>` を削除
  - `agents: Vec<AgentId>` を追加
  - `multi_agent: bool` を追加
  - `require_mention: bool` を維持

**`src/config/loader.rs`**:

- `FileDiscordChannelConfig` のフィールド変更:
  - `agent: Option<String>` を削除
  - `agents: Option<Vec<String>>` を追加（`#[serde(default)]`）
  - `multi_agent: Option<bool>` を追加（`#[serde(default)]`）
  - `require_mention` を維持
- 正規化ロジック更新:
  - `FileDiscordChannelConfig` → `DiscordChannelConfig` 変換時に `String → AgentId`
  - `agents` が空の場合、Bot の `default_agent` で `[default_agent]` に補充

**`src/config/tests.rs`**（旧 `.agent` 参照の更新）:

- `DiscordChannelConfig` の `.agent` 参照 4 箇所 (lines 848, 1178, 1290, 1366) を `.agents` に更新
- YAML テスト文字列内の `agent: xxx` を `agents: [xxx]` に更新
- `DiscordChannelConfig::default()` を使用しているテストが正規化後の値で動作することを確認

### コミット

`feat(config): replace DiscordChannelConfig.agent with agents/multi_agent schema`

---

## Step 5: Config Validation (TDD)

前提: Step 4

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `validation_rejects_multi_agent_with_single_agent` | `multi_agent: true` + `agents.len() == 1` → エラー |
| `validation_rejects_single_mode_with_multiple_agents` | `multi_agent: false` + `agents.len() > 1` → エラー |
| `validation_accepts_single_agent` | `agents: [lyre]`, `multi_agent: false` → OK |
| `validation_accepts_multi_agent` | `agents: [lyre, vega]`, `multi_agent: true` → OK |
| `validation_agents_reference_must_exist` | `agents: [unknown]` → エラー（`config.agents` に存在しない） |
| `validation_empty_agents_after_normalization` | agents が空（正規化後も空）→ エラー（防御的確認） |

### GREEN: 実装

`src/config/loader.rs` の validation を更新:

- `validate_discord_bot_references()` を拡張:
  - 各チャネルの `agents` エントリが `config.agents` に存在することを検証
  - `multi_agent: true` + `agents.len() == 1` → `ConfigError`
  - `multi_agent: false` + `agents.len() > 1` → `ConfigError`
  - `agents` が空 → `ConfigError`
- 必要に応じて `ConfigError` variant を追加（例: `InvalidMultiAgentConfig`）

### コミット

`feat(config): add validation rules for multi-agent channel configuration`

---

## Step 6: AgentConfig.discord_bot (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `parse_agent_config_with_discord_bot` | `discord_bot: lyre` をパースできる |
| `parse_agent_config_without_discord_bot` | discord_bot 未指定 → `None` |
| `validation_discord_bot_must_exist` | 存在しない Bot ID → エラー |
| `validation_discord_bot_null_is_ok` | `discord_bot: null` → OK（Bot なし Agent） |

### GREEN: 実装

**`src/config/types.rs`**:

- `AgentConfig` に `discord_bot: Option<BotId>` を追加

**`src/config/loader.rs`**:

- `FileAgentConfig` に `discord_bot: Option<String>` を追加（`#[serde(default)]`）
- 正規化で `String → BotId` 変換
- Validation 追加: `discord_bot` が指定されている場合、`channels.discord.bots` に存在することを確認

### コミット

`feat(config): add discord_bot field to AgentConfig with bot reference validation`

---

## Step 7: Discord Runtime Agent Resolution (TDD)

前提: Step 4

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `select_agent_returns_first_agent` | `agents: [lyre, vega]` → `"lyre"` を返す |
| `select_agent_falls_back_to_default` | チャネル設定なし → `default_agent` |
| `select_agent_dm_returns_default` | DM → `default_agent`（既存動作不变） |

### GREEN: 実装

`src/channels/discord.rs`:

- `Handler::select_agent()` (line 299): `c.agent.as_ref()` → `c.agents.first()`
- テスト内の `DiscordChannelConfig` 直接構築（9 箇所: lines 1080, 1097, 1114, 1151, 1158, 1291, 1309, 1327, 1345）を `{ agents: vec![...], multi_agent: false, require_mention: ... }` に更新
- `should_process_message()` 内の `config.require_mention` 参照はそのまま（フィールド名不变のため影響なし）

### コミット

`fix(channels/discord): update agent resolution for new channel config schema`

---

## Step 8: Config Persistence (TDD)

前提: Step 4, Step 6

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `persist_writes_agents_field` | agents フィールドが YAML に正しく書き戻される |
| `persist_writes_multi_agent_field` | multi_agent フィールドが書き戻される |
| `persist_writes_discord_bot_field` | AgentConfig の discord_bot が書き戻される |
| `persist_round_trip` | 読み込み → 書き戻し → 再読み込みで同一内容になる |

### GREEN: 実装

`src/config/persist.rs`:

- DiscordChannelConfig シリアライズ: `agents`, `multi_agent`, `require_mention` を書き出し
- AgentConfig シリアライズ: `discord_bot` を書き出し（`None` の場合はフィールド自体を省略または `null`）
- 既存のシリアライズロジックに新フィールドを追加

### コミット

`feat(config): persist multi-agent channel and agent config fields to YAML`

---

## Step 9: Docs Update

### 対象

| ファイル | 変更内容 |
|---|---|
| `docs/config.md` | DiscordChannelConfig 新フィールド（`agents`, `multi_agent`）, AgentConfig 新フィールド（`discord_bot`）, YAML 例を更新 |
| `docs/db.md` | messages テーブル新カラム, migration v7, MessageKind 説明, ER 図更新, Rust 構造体マッピング更新 |
| `docs/channels.md` | Discord 設定セクションの新フィールド説明, エージェント選択フロー更新 |

### コミット

`docs: update config, db, and channels docs for Multi-Agent Room Phase 1`

---

## Step 10: 動作確認

```bash
cargo fmt --check
cargo check -p egopulse
cargo test -p egopulse
cargo clippy --all-targets --all-features -- -D warnings
```

---

## Step 11: PR 作成

PR description は日本語。`Close #61` を明記。

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
|---|---|---|
| `src/storage/mod.rs` | 変更 | MessageKind enum 追加、StoredMessage 拡張 |
| `src/storage/migration.rs` | 変更 | Migration v7 追加、SCHEMA_VERSION 6 → 7 |
| `src/storage/queries.rs` | 変更 | row_to_stored_message 更新、SELECT クエリ更新 |
| `src/config/types.rs` | 変更 | DiscordChannelConfig（agents/multi_agent）, AgentConfig（discord_bot）更新 |
| `src/config/loader.rs` | 変更 | FileDiscordChannelConfig / FileAgentConfig 更新、正規化・バリデーション追加 |
| `src/config/persist.rs` | 変更 | 新フィールドのシリアライズ |
| `src/channels/discord.rs` | 変更 | select_agent() の agents 参照更新 |
| `docs/config.md` | 変更 | 設定仕様ドキュメント更新 |
| `docs/db.md` | 変更 | DB スキーマドキュメント更新 |
| `docs/channels.md` | 変更 | チャネル仕様ドキュメント更新 |

---

## コミット分割

1. `feat(storage): add MessageKind enum for multi-agent message classification` — `src/storage/mod.rs`
2. `feat(storage): migration v7 - add message_kind, sender/recipient agent columns to messages` — `src/storage/migration.rs`
3. `feat(storage): extend StoredMessage with multi-agent message fields` — `src/storage/mod.rs`, `src/storage/queries.rs`
4. `feat(config): replace DiscordChannelConfig.agent with agents/multi_agent schema` — `src/config/types.rs`, `src/config/loader.rs`
5. `feat(config): add validation rules for multi-agent channel configuration` — `src/config/loader.rs`
6. `feat(config): add discord_bot field to AgentConfig with bot reference validation` — `src/config/types.rs`, `src/config/loader.rs`
7. `fix(channels/discord): update agent resolution for new channel config schema` — `src/channels/discord.rs`
8. `feat(config): persist multi-agent channel and agent config fields to YAML` — `src/config/persist.rs`
9. `docs: update config, db, and channels docs for Multi-Agent Room Phase 1` — `docs/*.md`

---

## テストケース一覧（全 34 件）

### MessageKind (5)

1. `message_kind_display_message` — Display: Message → "message"
2. `message_kind_display_agent_send` — Display: AgentSend → "agent_send"
3. `message_kind_display_system_event` — Display: SystemEvent → "system_event"
4. `message_kind_from_str_valid` — FromStr: 全 3 variant のパース成功
5. `message_kind_from_str_unknown` — FromStr: 未知値 → Err

### DB Migration v7 (3)

6. `migration_v7_adds_columns` — 3 カラムが存在することを確認
7. `migration_v7_default_values` — 既存レコードの DEFAULT 値を確認
8. `migration_v7_from_v6_db` — v6 → v7 の migration が正常完了

### StoredMessage + Queries (3)

9. `stored_message_reads_message_kind` — MessageKind の読み取り
10. `stored_message_reads_nullable_agent_ids` — NULL → None
11. `store_message_uses_defaults` — INSERT 時 DEFAULT 値が入る

### DiscordChannelConfig Schema (6)

12. `parse_channel_config_with_agents` — agents: [lyre] パース
13. `parse_channel_config_with_multi_agent` — multi_agent + agents パース
14. `parse_channel_config_agents_default_empty` — agents 未指定 → 空 Vec
15. `normalize_empty_agents_fills_default` — 空 → [default_agent] 正規化
16. `normalize_agents_keeps_explicit` — 指定済み → そのまま
17. `channel_config_require_mention_preserved` — require_mention 維持

### Config Validation (6)

18. `validation_rejects_multi_agent_with_single_agent` — multi + single → エラー
19. `validation_rejects_single_mode_with_multiple_agents` — !multi + multi agents → エラー
20. `validation_accepts_single_agent` — 正常系: 単体 Agent
21. `validation_accepts_multi_agent` — 正常系: 複数 Agent
22. `validation_agents_reference_must_exist` — 不在 Agent ID → エラー
23. `validation_empty_agents_after_normalization` — agents 空 → エラー（防御的）

### AgentConfig.discord_bot (4)

24. `parse_agent_config_with_discord_bot` — discord_bot パース成功
25. `parse_agent_config_without_discord_bot` — 未指定 → None
26. `validation_discord_bot_must_exist` — 不在 Bot ID → エラー
27. `validation_discord_bot_null_is_ok` — null → OK

### Discord Runtime (3)

28. `select_agent_returns_first_agent` — agents[0] を返す
29. `select_agent_falls_back_to_default` — チャネル設定なし → default
30. `select_agent_dm_returns_default` — DM → default

### Config Persistence (4)

31. `persist_writes_agents_field` — agents 書き戻し
32. `persist_writes_multi_agent_field` — multi_agent 書き戻し
33. `persist_writes_discord_bot_field` — discord_bot 書き戻し
34. `persist_round_trip` — 読み → 書き → 再読み で同一内容

---

## 工数見積もり

| Step | 内容 | テスト行数 | 実装行数 | 合計 |
|---|---|---|---|---|
| Step 0 | WT 作成 | — | — | 0 |
| Step 1 | MessageKind Enum | 40 | 40 | 80 |
| Step 2 | Migration v7 | 60 | 30 | 90 |
| Step 3 | StoredMessage + Queries | 50 | 40 | 90 |
| Step 4 | DiscordChannelConfig Schema | 80 | 60 | 140 |
| Step 5 | Config Validation | 70 | 40 | 110 |
| Step 6 | AgentConfig.discord_bot | 50 | 30 | 80 |
| Step 7 | Discord Runtime | 40 | 15 | 55 |
| Step 8 | Config Persistence | 60 | 40 | 100 |
| Step 9 | Docs | — | 80 | 80 |
| **合計** | | **450** | **375** | **~825** |
