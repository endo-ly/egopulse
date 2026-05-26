# Plan: messagesテーブル sender_name/is_from_bot/sender_agent_id を sender_id/sender_kind に統合

messages テーブルの送信者表現を整理し、エージェントファーストな統一識別子設計に置き換える。
`is_from_bot`（boolean）、`sender_name`（表示名）、`sender_agent_id`（エージェント識別子）の3カラムを `sender_id`（統一識別子）と `sender_kind`（enum）の2カラムに集約する。

> **Note**: 以下の具体的なコード例・API 設計・構成（How）はあくまで参考である。実装時によりよい設計方針があれば積極的に採用すること。

## 設計方針

1. **エージェントファースト**: エージェントがメッセージ履歴を読む際に不要な表示名DB保持を廃止。識別子は常に技术IDで統一し、表示名は参照時に解決する。
2. **単一ID空間**: `sender_id` は `"lyre"`（エージェント）と `"user:discord:123456789"`（人間）と `"system"` を同じString列に格納する。名前空間衝突を `sender_kind` で区別する。
   - **旧データの扱い**: マイグレーション時、既存の user message には安定した user ID が存在しないため、`sender_name` の値をそのまま `sender_id` に移行し、`sender_kind=User` とする。これは「エージェントファースト設計」の新規データでは解消されるが、旧データでは表示名をID代わりとする債務となる。
3. **SQLite制約対応**: SQLiteは `DROP COLUMN` に非対応のため、マイグレーションでは `CREATE TABLE ... AS SELECT ...` によるテーブル再構築を行う。
4. **段階的破壊的変更**: DBスキーマ変更とAPI応答変更を同PRに含め、フロントエンド・バックエンドを同時に更新する。WebUIの `MessageItem` 型も変更対象。
5. **TDD基底**: 各モジュールごとにテスト先行で実装。特にマイグレーションの既存データ変換ロジックは丁寧に検証する。

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | カテゴリ | 変更内容 |
|---|---|---|
| `storage/mod.rs` | Rust | `StoredMessage` フィールド変更、`SenderKind` enum 新規追加、factory関数変更 |
| `storage/migration.rs` | Rust | `SCHEMA_VERSION` 4 へ更新、v3→v4 マイグレーション（テーブル再構築） |
| `storage/queries.rs` | Rust | SELECT/INSERT/UPDATE のカラム調整、row mapping 変更、テスト更新 |
| `agent_loop/session.rs` | Rust | `is_from_bot` 判定 → `sender_kind` 判定、StoredMessage 構築箇所変更 |
| `agent_loop/turn.rs` | Rust | チャンネルコンテキスト構築時の `[Bot]` vs `[Name]` 判定変更 |
| `channels/discord.rs` | Rust | Discordユーザー → `sender_id="user:discord:{id}"`、`sender_kind=User` |
| `channels/telegram.rs` | Rust | Telegramユーザー → `sender_id="user:telegram:{id}"`、`sender_kind=User` |
| `channels/web/sessions.rs` | Rust | API レスポンス JSON の `sender_name/is_from_bot` → `sender_id/sender_kind` |
| `tools/agent_send.rs` | Rust | agent_send ツールの `StoredMessage` 構築変更 |
| `pulse/output.rs` | Rust | Pulse synthetic/assistant message の `sender_id/sender_kind` 設定 |
| `slash_commands.rs` | Rust | CLIコマンドからの message 構築変更 |
| `memory.rs` | Rust | INSERT文カラム調整 |
| `sleep/batch.rs` | Rust | INSERT文カラム調整 |
| `web/src/types.ts` | TypeScript | `MessageItem` 型変更 |
| `web/src/components/MessageBubble.tsx` | TypeScript | `is_from_bot/sender_name` → `sender_kind/sender_id` 参照変更 |
| `web/src/hooks/useStream.ts` | TypeScript | ストリーミングメッセージの構造変更 |
| `web/src/app.css` | CSS | `bubble-bot` / `bubble-user` に加え `bubble-system` `bubble-tool` を追加 |

---

## Step 0: Worktree 作成

`worktree-create` skill を使用して、安全なワークツリー上で作業する。

```bash
# WT作成後、実装ブランチに切り替えて作業
```

---

## Step 1: `storage/mod.rs` — SenderKind enum と StoredMessage 定義 (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `sender_kind_display` | `SenderKind::User.to_string() == "user"` |
| `sender_kind_display_assistant` | `SenderKind::Assistant.to_string() == "assistant"` |
| `sender_kind_display_system` | `SenderKind::System.to_string() == "system"` |
| `sender_kind_display_tool` | `SenderKind::Tool.to_string() == "tool"` |
| `sender_kind_from_str` | `"assistant"` → `SenderKind::Assistant` |
| `sender_kind_from_str_invalid` | `"unknown"` → `Err` |
| `stored_message_assistant_factory` | `StoredMessage::assistant(...)` が正しい `sender_id/sender_kind` を持つ |
| `stored_message_user_factory` | `StoredMessage::user("user:cli:default", ...)` が正しい値を持つ |
| `stored_message_system_factory` | `StoredMessage::system(...)` が `sender_id="system", sender_kind=System` |

### GREEN: 実装

- `SenderKind` enum を追加（`User`, `Assistant`, `System`, `Tool`）。`Display` / `FromStr` を実装し、DBテキスト保存対応にする。
- `StoredMessage` struct を変更：
  - 削除: `sender_name: String`, `is_from_bot: bool`, `sender_agent_id: Option<String>`
  - 追加: `sender_id: String`, `sender_kind: SenderKind`
  - 維持: `recipient_agent_id: Option<String>`
- factory関数を刷新：
  - `StoredMessage::bot()` → `StoredMessage::assistant(chat_id, sender_id, content)`
  - `StoredMessage::user(chat_id, sender_id, content)` — `sender_id` は呼び出し元で "user:<channel>:<id>" を生成して渡す
  - `StoredMessage::system(chat_id, content)` を新設
  - `StoredMessage::tool_wrapper(chat_id, sender_id, recipient_id, content)` を新設

### コミット

`feat(storage): add SenderKind enum and refactor StoredMessage fields`

---

## Step 2: `storage/migration.rs` — スキーマ v3→v4 マイグレーション (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `migration_v3_to_v4_adds_sender_id` | v3 DBを作成 → マイグレーション実行 → `messages` に `sender_id`/`sender_kind` カラムが存在することを確認 |
| `migration_v3_to_v4_removes_is_from_bot` | マイグレーション後、`is_from_bot` カラムが存在しないことを確認 |
| `migration_v3_to_v4_converts_bot_message` | `is_from_bot=1` + `sender_name="egopulse"` → `sender_id="egopulse"`, `sender_kind="assistant"` |
| `migration_v3_to_v4_converts_agent_message` | `sender_agent_id="lyre"` + `is_from_bot=1` → `sender_id="lyre"`, `sender_kind="assistant"` |
| `migration_v3_to_v4_converts_user_message` | `is_from_bot=0` + `sender_name="Alice"` → `sender_id="Alice"`, `sender_kind=User`（旧データには安定した user ID がないため、`sender_name` の値をそのまま移行） |
| `migration_v3_to_v4_converts_system_event` | `sender_name="system"` + `is_from_bot=1` + `message_kind="system_event"` → `sender_id="system"`, `sender_kind="system"` |
| `migration_v3_to_v4_preserves_recipient_agent_id` | `recipient_agent_id` がマイグレーション後も保持される |
| `migration_v3_to_v4_preserves_data_count` | マイグレーション前後で messages 行数が変わらない |

### GREEN: 実装

- `SCHEMA_VERSION` を `4` に更新。
- `if version < 4` ブロックを追加。
- マイグレーション手順（SQLite制約下）：
  1. `BEGIN TRANSACTION`
  2. `CREATE TABLE messages_v4 (...)` 新スキーマで作成
  3. `INSERT INTO messages_v4 SELECT ...` で既存データ変換
     - `sender_id`: CASE文で `sender_agent_id` があればそれ、なければ `sender_name`（ただし `sender_name` がエージェント名の場合と人間名の場合を区別する必要がある）
     - 実際にはRust側でフェッチ→変換→INSERTの方が安全（CASE文が複雑になりすぎるため）
  4. `DROP TABLE messages`
  5. `ALTER TABLE messages_v4 RENAME TO messages`
  6. `CREATE INDEX idx_messages_chat_timestamp ON messages(chat_id, timestamp)`
  7. `set_schema_version_in_tx(&tx, 4, "unify sender fields into sender_id + sender_kind")`
  8. `COMMIT`
- 既存データ変換ロジック（Rust側）：
   - `is_from_bot=1` かつ `message_kind="system_event"` → `sender_id="system"`, `sender_kind=System`
   - `is_from_bot=1` かつ `sender_agent_id IS NOT NULL` → `sender_id = sender_agent_id`, `sender_kind=Assistant`
   - `is_from_bot=1` かつ `sender_agent_id IS NULL` かつ `sender_name != "system"` → `sender_id=sender_name`, `sender_kind=Assistant`（Tool か Assistant かの区別は不可なので Assistant で統一）
   - `is_from_bot=0` → `sender_id=sender_name`, `sender_kind=User`（旧データには安定した user ID がないため、`sender_name` の値をそのまま移行）

### コミット

`feat(storage): add v3→v4 migration for sender_id/sender_kind`

---

## Step 3: `storage/queries.rs` — CRUD クエリ更新 (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `store_message_with_sender_kind` | `SenderKind::Assistant` のメッセージを保存・読み出しできる |
| `store_message_user_kind` | `SenderKind::User` のメッセージを保存・読み出しできる |
| `store_message_system_kind` | `SenderKind::System` のメッセージを保存・読み出しできる |
| `store_message_tool_kind` | `SenderKind::Tool` のメッセージを保存・読み出しできる |
| `get_recent_messages_returns_sender_id` | 取得時に `sender_id` が正しく復元される |
| `find_message_by_content_finds_system_event` | `sender_kind=System` のメッセージが検索対象になる |
| `store_system_event_sets_system_kind` | `store_system_event_message()` が `sender_id="system", sender_kind=System` を設定する |
| `store_agent_response_sets_assistant_kind` | `store_agent_response_message()` が `sender_kind=Assistant` を設定する |
| `roundtrip_recipient_agent_id` | `recipient_agent_id` の保存・読み出しが正しく機能する |

### GREEN: 実装

- 全 SQL の SELECT/INSERT/UPDATE 文から `sender_name`, `is_from_bot`, `sender_agent_id` を除外。
- `sender_id`, `sender_kind` を追加。
- Row mapping: `sender_kind = row.get::<_, String>(...)?` → `SenderKind::from_str(&s)?`。
- `store_system_event_message()` を更新: `sender_id="system"`, `sender_kind=System`。
- `store_agent_response_message()` を更新: `sender_id=agent_id`, `sender_kind=Assistant`。
- WebUI API 向けのレスポンス構築箇所も変更（`channels/web/sessions.rs` から呼ばれる箇所）。

### コミット

`feat(storage): update queries for sender_id/sender_kind schema`

---

## Step 4: `agent_loop/session.rs` — セッションローディング更新 (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `load_session_maps_assistant_to_assistant` | `sender_kind=Assistant` → LLM role `"assistant"` |
| `load_session_maps_user_to_user` | `sender_kind=User` → LLM role `"user"` |
| `load_session_maps_system_to_system` | `sender_kind=System` → LLM role `"system"` |
| `load_session_maps_tool_to_assistant` | `sender_kind=Tool` → LLM role `"assistant"`（agent_sendはエージェント間通信でassistant扱い） |
| `loaded_from_recent_preserves_sender_kind` | `load_session_snapshot` 経由でも `sender_kind` が正しく反映される |

### GREEN: 実装

- `load_session_messages()`: `if message.is_from_bot` → `match message.sender_kind { Assistant | Tool => "assistant", User => "user", System => "system" }`
- `loaded_from_recent()`: 同様に変更。
- 各 `StoredMessage` 構築箇所（ユーザーメッセージ保存、ツール応答保存、システムメッセージ保存）を新 factory に置き換え。

### コミット

`feat(agent_loop): adapt session loading to SenderKind`

---

## Step 5: `agent_loop/turn.rs` — ターン実行時のプロンプト構築更新 (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `build_channel_context_formats_user` | `sender_kind=User` のメッセージ（人間・Pulse含む）が `[Alice] content` や `[Pulse] content` とフォーマットされる |
| `build_channel_context_formats_assistant` | `sender_kind=Assistant` のメッセージが `[lyre] content` とフォーマットされる |
| `build_channel_context_formats_system` | `sender_kind=System` のメッセージが `[system] content` とフォーマットされる |
| `build_channel_context_formats_tool` | `sender_kind=Tool` のメッセージが `[tool/lyre] content` とフォーマットされる |

### GREEN: 実装

- `build_channel_context()`: `if m.is_from_bot` → `match m.sender_kind`
- フォーマット例:
  - `User`: `[user:discord:123] {content}`
  - `Assistant`: `[{sender_id}] {content}`（例: `[lyre] hello`）
  - `System`: `[system] {content}`
  - `Tool`: `[tool/{sender_id}] {content}`（例: `[tool/lyre] sent to vega`）

### コミット

`feat(agent_loop): update turn prompt formatting for SenderKind`

---

## Step 6: `channels/discord.rs`、`channels/telegram.rs` — チャンネル受信更新 (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `discord_user_message_sender_id` | Discordユーザー発言が `sender_id="user:discord:{user_id}"` になる |
| `telegram_user_message_sender_id` | Telegramユーザー発言が `sender_id="user:telegram:{user_id}"` になる |
| `discord_stored_message_has_user_kind` | 保存されたメッセージが `sender_kind=User` |
| `telegram_stored_message_has_user_kind` | 保存されたメッセージが `sender_kind=User` |

### GREEN: 実装

- Discord: `msg.author.name` → `msg.author.id`（または `msg.author.id.to_string()`）、`sender_id = format!("user:discord:{}", msg.author.id)`, `sender_kind = SenderKind::User`
- Telegram: `sender_name` 引数 → `sender_id = format!("user:telegram:{}", msg.from.id)`, `sender_kind = SenderKind::User`
- Channel Log保存（Discord Multi-Agent Room）も同様に更新。

### コミット

`feat(channels): update Discord/Telegram to use sender_id/sender_kind`

---

## Step 7: `channels/web/sessions.rs` — Web API レスポンス更新 (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `api_messages_returns_sender_id` | `/api/session/messages` が `sender_id` を含む |
| `api_messages_returns_sender_kind` | `/api/session/messages` が `sender_kind` を含む |
| `api_messages_excludes_sender_name` | レスポンスが `sender_name` を含まない（または含んでも無視される） |
| `api_messages_excludes_is_from_bot` | レスポンスが `is_from_bot` を含まない |

### GREEN: 実装

- JSON レスポンス: `sender_id`, `sender_kind` を追加、`sender_name`, `is_from_bot` を除外。
- SSE streaming, WebSocket 等のイベントメッセージ構築も同様に変更（該当箇所があれば）。

### コミット

`feat(channels/web): update API response to use sender_id/sender_kind`

---

## Step 8: `tools/agent_send.rs`、`pulse/output.rs`、`slash_commands.rs`、`memory.rs`、`sleep/batch.rs` — 残モジュール更新

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `agent_send_sets_tool_kind` | `agent_send` ツールが `sender_kind=Tool` を設定する |
| `agent_send_sets_sender_id_as_agent_id` | `sender_id` に呼び出しエージェントのIDが入る |
| `pulse_synthetic_sets_user_kind` | Pulse synthetic message（`message_kind=SystemEvent`）が `sender_kind=User`、`sender_id="pulse"` を持つ |
| `slash_command_cli_user_sets_user_kind` | CLIスラッシュコマンドからのメッセージが `sender_kind=User` |

### GREEN: 実装

- `tools/agent_send.rs`: `sender_id = context.agent_id`, `sender_kind = SenderKind::Tool`, `recipient_agent_id = target_id`
- `pulse/output.rs`:
  - synthetic: `sender_id="pulse"`, `sender_kind=SenderKind::User`, `message_kind=MessageKind::SystemEvent`（message_kindは維持。synthetic message は LLM 入力時に `"user"` role として扱われるため sender_kind=User で挙動を保持）
  - assistant: `sender_id=agent_id`, `sender_kind=SenderKind::Assistant`
- `slash_commands.rs`: `sender_id="user:cli:default"`, `sender_kind=SenderKind::User`
- `memory.rs`, `sleep/batch.rs`: INSERT文のカラムリストとパラメータ順を調整。

### コミット

`feat: update agent_send, pulse, slash_commands, memory, sleep for new schema`

---

## Step 9: WebUI フロントエンド更新 (TDD)

注意: ライブ送信経路（SSE streaming）と履歴APIの整合性を保つため、両方のメッセージ構造を新スキーマに統一する。送信API（`/api/send_stream`）のリクエストボディは変更しない（サーバー側で `sender_id` を自動設定）。クライアント側の仮想メッセージ構築とSSEイベントのメッセージパースを `sender_id/sender_kind` に統一する。

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `message_bubble_renders_user_kind` | `sender_kind="user"` のメッセージがユーザー側スタイルで表示される |
| `message_bubble_renders_assistant_kind` | `sender_kind="assistant"` のメッセージがボット側スタイルで表示される |
| `message_bubble_renders_system_kind` | `sender_kind="system"` のメッセージが特別なスタイル（薄いグレー等）で表示される |
| `message_bubble_shows_sender_id` | 表示名の代わりに `sender_id` が表示される（または表示名解決ロジックが動作する） |
| `useStream_sends_correct_sender_id` | ユーザー送信時に `sender_id="user:web:default"` と設定される |
| `useStream_sets_user_kind` | ユーザー送信時に `sender_kind="user"` と設定される |

### GREEN: 実装

- `web/src/types.ts`: `MessageItem` を変更。
  ```typescript
  export type MessageItem = {
    id: string;
    sender_id: string;
    sender_kind: "user" | "assistant" | "system" | "tool";
    content: string;
    timestamp: string;
  };
  ```
- `web/src/components/MessageBubble.tsx`:
  - `message.is_from_bot` → `message.sender_kind`
  - スタイルクラス: `bubble-${message.sender_kind}`（CSS で `.bubble-user`, `.bubble-assistant`, `.bubble-system`, `.bubble-tool` を定義）
  - `message.sender_name` → `message.sender_id`（表示名解決は後回し、まずIDそのまま表示）
  - Markdown rendering: `assistant` と `tool` は markdown、`user` は `<pre>`、 system は特殊表示
- `web/src/hooks/useStream.ts`:
  - ユーザーメッセージ構築（ローカル仮想メッセージ）: `sender_id: "user:web:default"`, `sender_kind: "user"`
  - ボット応答構築（ストリーミング中の仮メッセージ）: `sender_id: "egopulse"`, `sender_kind: "assistant"`
  - SSEイベントからのメッセージ更新も `sender_id/sender_kind` を参照するように変更
  - **API リクエストボディは変更しない**（`/api/send_stream` は `session_key` と `message` のみ送信。サーバー側で `sender_id="user:web:default", sender_kind=User` を自動設定）
- `web/src/app.css`:
  - `.bubble-system`（薄いグレー背景）
  - `.bubble-tool`（assistant と同じか、わずかに異なるスタイル）

### コミット

`feat(web): update MessageItem type and MessageBubble for sender_id/sender_kind`

---

## Step 10: 動作確認

### 実行コマンド

```bash
cargo fmt --check
cargo check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

```bash
cd web && npm run build
```

### 検証項目

- [ ] `cargo test` が全て通過する
- [ ] `cargo clippy` が `0 warnings` である
- [ ] `cargo fmt --check` がクリーンである
- [ ] WebUI のビルドが成功する
- [ ] `cargo run -- chat` で CLI セッションが正常に動作する（最低限の手動確認）

### コミット

`chore: fix clippy warnings and formatting`

---

## Step 11: PR 作成

### PR Description（日本語）

```markdown
## 概要

messages テーブルの送信者表現を整理し、3カラム（sender_name, is_from_bot, sender_agent_id）を2カラム（sender_id, sender_kind）に統合する。

## 変更内容

- DBスキーマ v3→v4 マイグレーション
- `SenderKind` enum（User / Assistant / System / Tool）追加
- `StoredMessage` から `sender_name` / `is_from_bot` / `sender_agent_id` を削除
- `StoredMessage` に `sender_id` / `sender_kind` を追加
- 全チャンネル（Discord / Telegram / Web / CLI）の送信者ID生成を統一
- WebUI API レスポンスとフロントエンド表示を新スキーマに対応
- agent_send、Pulse、スラッシュコマンド等の残モジュールを更新

## 影響範囲

- `src/storage/mod.rs`
- `src/storage/migration.rs`
- `src/storage/queries.rs`
- `src/agent_loop/session.rs`
- `src/agent_loop/turn.rs`
- `src/channels/discord.rs`
- `src/channels/telegram.rs`
- `src/channels/web/sessions.rs`
- `src/tools/agent_send.rs`
- `src/pulse/output.rs`
- `src/slash_commands.rs`
- `src/memory.rs`
- `src/sleep/batch.rs`
- `web/src/types.ts`
- `web/src/components/MessageBubble.tsx`
- `web/src/hooks/useStream.ts`
- `web/src/app.css`
```

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
|---|---|---|
| `src/storage/mod.rs` | **変更** | `SenderKind` enum 追加、`StoredMessage` フィールド変更 |
| `src/storage/migration.rs` | **変更** | `SCHEMA_VERSION=4`、v3→v4マイグレーション追加 |
| `src/storage/queries.rs` | **変更** | SQL/row mapping/ヘルパー関数/テスト更新 |
| `src/agent_loop/session.rs` | **変更** | `is_from_bot` → `sender_kind` 判定 |
| `src/agent_loop/turn.rs` | **変更** | プロンプト構築時のフォーマット変更 |
| `src/channels/discord.rs` | **変更** | `sender_id`/`sender_kind` 設定変更 |
| `src/channels/telegram.rs` | **変更** | `sender_id`/`sender_kind` 設定変更 |
| `src/channels/web/sessions.rs` | **変更** | API JSON レスポンス調整 |
| `src/tools/agent_send.rs` | **変更** | `StoredMessage` 構築変更 |
| `src/pulse/output.rs` | **変更** | `StoredMessage` 構築変更 |
| `src/slash_commands.rs` | **変更** | `StoredMessage` 構築変更 |
| `src/memory.rs` | **変更** | INSERT カラム調整 |
| `src/sleep/batch.rs` | **変更** | INSERT カラム調整 |
| `web/src/types.ts` | **変更** | `MessageItem` 型変更 |
| `web/src/components/MessageBubble.tsx` | **変更** | `sender_kind` による表示切り替え |
| `web/src/hooks/useStream.ts` | **変更** | メッセージ構造変更 |
| `web/src/app.css` | **変更** | `bubble-system`/`bubble-tool` 追加 |
| `docs/db.md` | **変更** | messages テーブル定義・カラム説明を新スキーマに更新 |

---

## コミット分割

1. `feat(storage): add SenderKind enum and refactor StoredMessage fields`
2. `feat(storage): add v3→v4 migration for sender_id/sender_kind`
3. `feat(storage): update queries for sender_id/sender_kind schema`
4. `feat(agent_loop): adapt session loading to SenderKind`
5. `feat(agent_loop): update turn prompt formatting for SenderKind`
6. `feat(channels): update Discord/Telegram to use sender_id/sender_kind`
7. `feat(channels/web): update API response to use sender_id/sender_kind`
8. `feat: update agent_send, pulse, slash_commands, memory, sleep for new schema`
9. `feat(web): update MessageItem type and MessageBubble for sender_id/sender_kind`
10. `chore: fix clippy warnings and formatting`

---

## テストケース一覧（全 28 件）

### `storage/mod` (9)
1. `sender_kind_display` — `User` の `Display` が `"user"`
2. `sender_kind_display_assistant` — `Assistant` の `Display` が `"assistant"`
3. `sender_kind_display_system` — `System` の `Display` が `"system"`
4. `sender_kind_display_tool` — `Tool` の `Display` が `"tool"`
5. `sender_kind_from_str` — 文字列から `SenderKind` への変換
6. `sender_kind_from_str_invalid` — 不正な文字列で `Err`
7. `stored_message_assistant_factory` — `assistant` factory が正しい値を生成
8. `stored_message_user_factory` — `user` factory が正しい値を生成
9. `stored_message_system_factory` — `system` factory が正しい値を生成

### `storage/migration` (8)
10. `migration_v3_to_v4_adds_sender_id` — 新カラム追加確認
11. `migration_v3_to_v4_removes_is_from_bot` — 旧カラム削除確認
12. `migration_v3_to_v4_converts_bot_message` — bot message 変換
13. `migration_v3_to_v4_converts_agent_message` — agent message 変換
14. `migration_v3_to_v4_converts_user_message` — user message 変換
15. `migration_v3_to_v4_converts_system_event` — system event 変換
16. `migration_v3_to_v4_preserves_recipient_agent_id` — recipient_agent_id 保持
17. `migration_v3_to_v4_preserves_data_count` — 行数維持

### `storage/queries` (9)
18. `store_message_with_sender_kind` — `Assistant` 保存・読み出し
19. `store_message_user_kind` — `User` 保存・読み出し
20. `store_message_system_kind` — `System` 保存・読み出し
21. `store_message_tool_kind` — `Tool` 保存・読み出し
22. `get_recent_messages_returns_sender_id` — `sender_id` 復元
23. `find_message_by_content_finds_system_event` — システムイベント検索
24. `store_system_event_sets_system_kind` — `store_system_event_message` の検証
25. `store_agent_response_sets_assistant_kind` — `store_agent_response_message` の検証
26. `roundtrip_recipient_agent_id` — `recipient_agent_id` の往復

### `agent_loop` (5)
27. `load_session_maps_assistant_to_assistant` — Assistant → LLM role
28. `load_session_maps_user_to_user` — User → LLM role

### `channels` (2)
29. `discord_user_message_sender_id` — Discord `sender_id` 形式
30. `telegram_user_message_sender_id` — Telegram `sender_id` 形式

### `tools/pulse` (2)
31. `agent_send_sets_tool_kind` — agent_send の `sender_kind=Tool`
32. `pulse_synthetic_sets_user_kind` — Pulse synthetic の `sender_kind=User`（`message_kind=SystemEvent` は維持）

---

## 工数見積もり

| Step | 内容 | 見積もり（行数） |
|---|---|---|
| Step 1 | storage/mod.rs enum + struct + factory + tests | ~120 行 |
| Step 2 | storage/migration.rs v3→v4 + tests | ~100 行 |
| Step 3 | storage/queries.rs SQL調整 + mapping + tests | ~150 行 |
| Step 4 | agent_loop/session.rs 判定変更 + tests | ~60 行 |
| Step 5 | agent_loop/turn.rs フォーマット変更 + tests | ~50 行 |
| Step 6 | channels/discord.rs + telegram.rs 変更 + tests | ~60 行 |
| Step 7 | channels/web/sessions.rs API変更 + tests | ~30 行 |
| Step 8 | tools/agent_send.rs + pulse/output.rs + 残り | ~70 行 |
| Step 9 | WebUI TypeScript/CSS変更 + tests | ~80 行 |
| Step 10 | 動作確認・Lint修正 | ~30 行 |
| **合計** | | **~750 行** |

> 工数見積もりは行数ベース。実装時間の目安は **4〜6 時間**（マイグレーションロジックのデバッグとWebUIの調整を含む）。
