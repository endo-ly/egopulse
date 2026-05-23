# Plan: Episode Event Ledger — Phase 1 (LLM Call 1)

Episode Event 台帳を追加し、Sleep Batch フローに「LLM Call 1（Event Extraction）」を導入する。
この Phase では LLM Call 1（Event 抽出 → DB 登録）までを実装する。
既存の LLM Call（episodic/semantic/prospective 一括更新）は変更しない。

> **Note**: 以下の具体的なコード例・API 設計・構成（How）はあくまで参考である。実装時によりよい設計方針があれば積極的に採用すること。
> 特に「現行の LLM Call は変えないで、新 Call 1 を追加する」という方針に従うこと。

---

## 設計方針

1. **追加のみ・既存変更は最小限**: 現行 Sleep Batch の LLM Call（`send_sleep_request` / `parse_sleep_response`）は一切変更しない。Call 1 は追加する新しい処理として実装する。
2. **append-only DB**: `episode_events` は蓄積のみ。merge/update/削除は行わない。同一 `sleep_run_id` の Event は全件 skip（重複排除は run 単位で行う）。
3. **DB マイグレーションはバージョンベース**: `SCHEMA_VERSION` をインクリメントし、`run_migrations` に新規ブロックを追加。既存パターン（v1→v2）に従う。
4. **TDD で進める**: 各 Step で RED（テスト先行）→ GREEN（実装）→ コミット を完結させる。
5. **Rust 規約に従う**: `thiserror` による構造化、小文字 `Display`、`tracing` ログ、ドキュメントコメント（`# Errors` / `# Panics`）を含める。
6. **Call 1 は現行 chunking に乗せる**: `build_session_text_chunks` を使い、コンテキスト溢れを防ぐ。各 chunk で独立に Event Extraction を行い、結果をマージして DB に投入する。
7. **Call 1 failure は既存フローを止めない**: Call 1 が失敗しても、それを warn ログに記録し、既存の memory-update LLM Call は継続して実行する。Call 1 は「ベストエフォート」で運用する。

---

## Plan スコープ

WT 作成 → 実装(TDD) → コミット(意味ごとに分離) → PR 作成

---

## 対象一覧

| 対象 | 内容 | 新規/変更 |
|---|---|---|
| `src/storage/migration.rs` | `episode_events` テーブル DDL | 変更 |
| `src/storage/mod.rs` | `EpisodeEvent`, `EpisodeEventKind` 構造体/enum | **新規** |
| `src/storage/queries.rs` | `episode_events` CRUD クエリ | 変更 |
| `src/sleep/batch.rs` | LLM Call 1（Event Extraction）実装・統合 | 変更 |
| `src/sleep/extract_prompt.md` | Event Extraction 用システムプロンプト | **新規** |
| `docs/db.md` | スキーマドキュメント更新 | 変更 |
| `docs/plan/generatedEpisodeView.md` | 仕様書（実装済み反映） | 変更 |

---

## Step 1: DB マイグレーション（episode_events テーブル追加）(TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `fresh_db_includes_episode_events` | 新規 DB 作成時に `episode_events` テーブルが存在すること |
| `migration_from_v2_to_v3_adds_episode_events` | v2 → v3 マイグレーションでテーブルが追加されること |
| `episode_events_all_columns_exist` | 全カラム（id, agent_id, experienced_at 等）が存在すること |
| `episode_events_indexes_exist` | 4つのインデックスが作成されること |

### GREEN: 実装

`src/storage/migration.rs` に以下を追加：

- `SCHEMA_VERSION` を `2` → `3` に更新
- `if version < 3` ブロックを追加
- `episode_events` テーブル DDL（仕様書 §4.2 の DDL をそのまま使用）
- 4つのインデックス作成

```sql
CREATE TABLE IF NOT EXISTS episode_events (
    id               TEXT PRIMARY KEY,
    agent_id         TEXT NOT NULL,
    experienced_at   TEXT NOT NULL,
    encoded_at       TEXT NOT NULL,
    kind             TEXT NOT NULL,
    title            TEXT NOT NULL,
    body_md          TEXT NOT NULL,
    ripple_strength  INTEGER NOT NULL DEFAULT 3,
    certainty        TEXT NOT NULL DEFAULT 'observed',
    sleep_run_id     TEXT NOT NULL,
    source_refs_json TEXT,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL,
    CHECK (kind IN (
        'self', 'relationship', 'world', 'feat',
        'anomaly', 'decision', 'insight', 'rhythm'
    )),
    CHECK (ripple_strength BETWEEN 1 AND 5),
    CHECK (certainty IN ('observed', 'inferred', 'uncertain'))
);

CREATE INDEX IF NOT EXISTS idx_episode_events_agent_experienced
    ON episode_events(agent_id, experienced_at);
CREATE INDEX IF NOT EXISTS idx_episode_events_agent_kind_experienced
    ON episode_events(agent_id, kind, experienced_at);
CREATE INDEX IF NOT EXISTS idx_episode_events_agent_ripple_experienced
    ON episode_events(agent_id, ripple_strength, experienced_at);
CREATE INDEX IF NOT EXISTS idx_episode_events_sleep_run
    ON episode_events(sleep_run_id);
```

### コミット

```
feat(migration): add episode_events table for event ledger

- SCHEMA_VERSION v2 -> v3
- CREATE TABLE episode_events with CHECK constraints
- 4 indexes for agent-kind-happened queries
```

---

## Step 2: データモデル定義（EpisodeEvent, EpisodeEventKind）(TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `episode_event_kind_display` | `EpisodeEventKind::Self` → "self" 等、全 8 kind の Display |
| `episode_event_kind_from_str_valid` | 8 kind の from_str が正しくパース |
| `episode_event_kind_from_str_invalid` | 不正 kind は Err |
| `episode_event_default_ripple` | 未指定 ripple_strength は 3 |
| `episode_event_ripple_bounds` | ripple_strength 1〜5 の範囲外は不正（DB CHECK で防ぐ） |

### GREEN: 実装

`src/storage/mod.rs` に追加：

- `EpisodeEventKind` enum（8種の kind）
  - `Self`, `Relationship`, `World`, `Feat`, `Anomaly`, `Decision`, `Insight`, `Rhythm`
  - `Display`, `FromStr` derive（手動実装）
- `EpisodeEventCertainty` enum
  - `Observed`, `Inferred`, `Uncertain`
- `EpisodeEvent` 構造体（全カラム対応）

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EpisodeEventKind { ... }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EpisodeEventCertainty { ... }

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EpisodeEvent {
    pub id: String,
    pub agent_id: String,
    pub experienced_at: String,
    pub encoded_at: String,
    pub kind: EpisodeEventKind,
    pub title: String,
    pub body_md: String,
    pub ripple_strength: i64,
    pub certainty: EpisodeEventCertainty,
    pub sleep_run_id: String,
    pub source_refs_json: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}
```

### コミット

```
feat(storage): add EpisodeEvent data model

- EpisodeEventKind enum with 8 kinds
- EpisodeEventCertainty enum
- EpisodeEvent struct matching DB schema
```

---

## Step 3: DB クエリ（episode_events CRUD）(TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `insert_episode_event_succeeds` | 正常 insert、id を返す |
| `insert_duplicate_sleep_run_id_skips` | 同一 sleep_run_id の Event は全件 skip（重複は run 単位で判定） |
| `list_episode_events_by_agent_experienced_desc` | agent_id + experienced_at 降順で取得 |
| `list_events_by_agent_kind` | agent_id + kind フィルタ |
| `list_episode_events_by_agent_ripple` | agent_id + ripple_strength フィルタ |
| `count_events_by_agent` | agent_id の総件数 |
| `list_events_by_sleep_run_id` | sleep_run_id でフィルタ |

### GREEN: 実装

`src/storage/queries.rs` に `Database` impl ブロックを追加：

```rust
impl Database {
    /// Inserts episode events for a given sleep_run_id.
    /// Skips ALL events if any event with the same sleep_run_id already exists.
    pub(crate) fn insert_episode_events(
        &self,
        sleep_run_id: &str,
        events: &[EpisodeEvent],
    ) -> Result<(), StorageError>;

    /// Lists events for an agent, ordered by experienced_at DESC.
    pub(crate) fn list_episode_events(
        &self,
        agent_id: &str,
        kind: Option<EpisodeEventKind>,
        ripple_min: Option<i64>,
        limit: i64,
    ) -> Result<Vec<EpisodeEvent>, StorageError>;

    /// Counts total events for an agent.
    pub(crate) fn count_episode_events(&self, agent_id: &str) -> Result<i64, StorageError>;

    /// Lists events by sleep_run_id.
    pub(crate) fn list_episode_events_by_run(
        &self,
        sleep_run_id: &str,
    ) -> Result<Vec<EpisodeEvent>, StorageError>;
}
```

重複 skip の仕様：同じ `sleep_run_id` の Event が既に存在する場合、**その run の全 Event を skip** する。実装は `INSERT` 前に `SELECT COUNT(*) FROM episode_events WHERE sleep_run_id = ?` で確認し、0 件なら bulk insert、>0 なら何もしない。これにより LLM が UUID を生成しても再実行時の重複を防ぐ。

**追加変更：`get_agent_sessions_since` クエリ**

```rust
pub(crate) fn get_agent_sessions_since(
    &self,
    agent_id: &str,
    since: Option<&str>,
    limit: usize,
) -> Result<Vec<AgentSessionInfo>, StorageError>;
```

このクエリに以下の変更を加える：

- `AgentSessionInfo` に `message_ids: Vec<String>` を含めるため、JOIN messages して `GROUP_CONCAT(m.id)` または Rust 側で `Vec<StoredMessage>` を取得し、`message_ids` を抽出する
- `source_chats_json = serde_json::to_string(&sessions)` に含まれる `message_ids` は、将来 `source_refs_json` の紐付けに使用する可能性がある
- **⚠️ 注意**: 仕様書 §6.3 の「同一 source の重複は skip」は `source_refs_json` レベル（message_id 単位）での重複排除を意図している。本 Plan の `sleep_run_id` 単位 skip は「同一 Sleep Run の再実行時の重複防止」としては有効だが、別の Sleep Run で同一 source から同一 Event が抽出されるケースはカバーしない。実装上許容できるか判断すること。

### コミット

```
feat(storage): add episode_events CRUD queries

- insert_episode_events with OR IGNORE duplicate handling
- list_episode_events with optional kind/ripple_strength filters
- count_events and list_by_sleep_run_id
```

---

## Step 4: LLM Call 1 — Event Extraction (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `parse_extract_events_response_valid` | 正規JSON {events:[...]} をパース |
| `parse_extract_events_response_missing_events_key` | "events" キー欠如 → ParseFailed |
| `parse_extract_events_response_invalid_event_kind` | kind が 8種以外 → ParseFailed |
| `parse_extract_events_response_salience_out_of_range` | ripple_strength が 1〜5 以外 → ParseFailed |
| `parse_extract_events_response_certainty_invalid` | certainty が observed/inferred/uncertain 以外 → ParseFailed |
| `parse_extract_events_response_with_thinking_tags` | `<thinking>` タグ付き応答を正規化してパース |
| `parse_extract_events_response_json_code_block` | Markdown コードブロック内 JSON を抽出 |
| `parse_extract_events_response_retry_guard` | 初回失敗後に JSON retry guard で再試行。guard 内容：`"Your previous response was not valid JSON. You must respond with ONLY a JSON object containing exactly one key: \\"events\\". ..."` |
| `build_extract_system_prompt_includes_sessions` | プロンプトに sessions XML が含まれる |
| `build_extract_system_prompt_includes_kinds` | プロンプトに kind 定義が含まれる |
| `parse_extract_events_valid_source_message_ids` | 正規の source_message_ids をパース |
| `parse_extract_events_empty_source_message_ids` | 空配列 `[]` を許容 |
| `parse_extract_events_missing_source_message_ids` | source_message_ids キー欠如 → ParseFailed |

### GREEN: 実装

`src/sleep/extract_prompt.md` を新規作成：

- 役割：{AGENT_NAME} の海馬 — 会話ログから記憶すべき出来事を抽出する
- 入力：**1 つの chunk**（`<sessions>` XML）。chunk 単位で独立に処理する。
- 出力形式：必ず JSON オブジェクト。キーは `events` のみ
- 各 Event のスキーマ（`id` は LLM 出力に含めない。システムが UUID を生成する）：
  - `experienced_at`: RFC3339（会話中の出来事時刻）
  - `kind`: 8種のいずれか
  - `title`: 短い見出し
  - `body_md`: Markdown 本文
  - `ripple_strength`: 1〜5（デフォルト3）
  - `certainty`: "observed" | "inferred" | "uncertain"
  - `source_message_ids`: `<message id="...">` の id 配列（根拠となったメッセージ、1〜5件想定。特定メッセージに紐づかない場合は `[]` でよい）
- LLM 出力例：
  ```json
  {
    "events": [
      {
        "experienced_at": "2026-05-23T12:34:56+09:00",
        "kind": "decision",
        "title": "認証にJWTを採用することに決定",
        "body_md": "ユーザーからセッション方式とJWT方式の比較を求められ、議論の結果JWT方式を選択。理由はスケーラビリティとステートレス性。",
        "ripple_strength": 4,
        "certainty": "observed",
        "source_message_ids": ["msg_abc123", "msg_def456"]
      },
      {
        "experienced_at": "2026-05-23T13:00:00+09:00",
        "kind": "insight",
        "title": "thiserrorとanyhowの使い分けを理解",
        "body_md": "ライブラリではthiserrorによる構造化エラー、アプリケーション層ではanyhowを使うのがRustの定石であることを理解した。",
        "ripple_strength": 3,
        "certainty": "inferred",
        "source_message_ids": []
      }
    ]
  }
  ```
- やらないことの明記：
  - episodic.md を生成しない
  - semantic.md / prospective.md を更新しない
  - 何でも意味記憶に昇華しない
  - 過去 Event を削除しない

`src/sleep/batch.rs` に追加：

- `ExtractEventsOutput` / `ExtractedEvent` 型定義（Step 4 のまま）
- `parse_extract_events_response`：JSON → Vec<ExtractedEvent>（バリデーション付き）
- `build_extract_system_prompt(agent_id, sessions_text)`：sessions_text を 1 つの chunk のみ受け取る
- **新規: `SourceRef` / `ExtractionChunk` 構造体**
  ```rust
  struct SourceRef {
      chat_id: i64,
      message_id: String,
      timestamp: String,
      role: String,
  }
  
  struct ExtractionChunk {
      text: String,                           // <message id="..." ts="..." role="...">...</message> 形式
      message_map: HashMap<String, SourceRef>, // id → SourceRef
  }
  ```
- **新規: `build_extraction_chunks`**：`snapshot.recent_messages`（`Vec<StoredMessage>`）から各メッセージを `<message id="..." ts="..." role="...">{content}</message>` 形式でシリアライズし、`message_map` に `id → SourceRef` を登録。メッセージ境界をまたがないように chunk 分割。
- **新規: `run_extract_events_for_chunks`**：chunk ごとに LLM Call 1 を実行し、LLM 出力の `source_message_ids` を `chunk.message_map` で `SourceRef` に解決。解決できたものだけを `serde_json::to_string` して DB の `source_refs_json` に格納。存在しない id は `warn!` ログ＋スキップ。

```rust
async fn run_extract_events_for_chunks(
    provider: &Arc<dyn LlmProvider>,
    agent_id: &str,
    session_chunks: Vec<ExtractionChunk>,
) -> Result<Vec<ExtractedEvent>, SleepBatchError> {
    let mut all_events = Vec::new();
    for (index, chunk) in session_chunks.into_iter().enumerate() {
        let prompt = build_extract_system_prompt(agent_id, &chunk.text);
        let result = send_extract_events_request(provider, agent_id, &prompt, index + 1).await?;
        
        for event in result.events {
            let mut refs = Vec::new();
            for msg_id in &event.source_message_ids {
                if let Some(sr) = chunk.message_map.get(msg_id) {
                    refs.push(serde_json::to_value(sr)?);
                } else {
                    warn!(msg_id = %msg_id, "unknown message_id in extract event, skipping");
                }
            }
            all_events.push(ExtractedEvent {
                source_refs_json: if refs.is_empty() { None } else { Some(serde_json::to_string(&refs)?) },
                ..event
            });
        }
    }
    Ok(all_events)
}
```

- `send_extract_events_request`：
  - 初回：`provider.send_message(system_prompt, [user_message], None)` で送信
  - `parse_extract_events_response(&response.content)` でパース
  - 失敗時：warn! ログ出力（エラー + raw_preview）
  - リトライ：`provider.send_message(system_prompt, [user_message, assistant_msg, retry_user_msg], None)`
    - `assistant_msg` = 前回の不正な応答全文
    - `retry_user_msg` = `EVENTS_RETRY_GUARD`（下記参照）
  - リトライ結果を再度パース
  - 再失敗時：warn! ログ + Err 返却
  - **system_prompt は初回・リトライで同じ**（`build_extract_system_prompt` で生成したもの）
- `EVENTS_RETRY_GUARD`（定数）：
  ```
  Your previous response was not valid JSON. You must respond with ONLY a JSON object containing exactly one key: "events" (an array of episode event objects). Do not include any other keys, markdown formatting, code blocks, or explanatory text. Output the raw JSON object and nothing else.
  ```
- `normalize_llm_response` は既存関数を共有利用、ただし `extract_json_from_code_block` を強化して以下のパターンにも対応：
  - ` ```json ` ` ```JSON ` ` ```Json ` 等大文字小文字不問
  - ` ``` `（言語指定なしのコードブロック）→ 中身から `{…}` を抽出
  - 言語指定部分は無視して、最初の ` ``` ` 〜 次の ` ``` ` を抽出

### コミット

```
feat(sleep): add LLM Call 1 — Event Extraction with chunking

- New prompt: src/sleep/extract_prompt.md (per-chunk processing)
- ExtractEventsOutput / ExtractedEvent data types
- run_extract_events_for_chunks: process each chunk independently
- parse_extract_events_response with validation
- send_extract_events_request with retry logic
```

---

## Step 5: Sleep Batch フロー改修 — Call 1 統合 (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `run_sleep_batch_extracts_events_before_memory_update` | Execute フロー内で Call 1 → Call 既存 の順で実行される |
| `run_sleep_batch_saves_extracted_events_to_db` | Call 1 の結果が episode_events に insert される |
| `run_sleep_batch_skips_duplicate_sleep_run_events` | 同一 sleep_run_id の Event は全件 skip される |
| `run_sleep_batch_extract_call_failure_continues` | Call 1 失敗時、warn ログを出して既存フローを継続（run は failed にしない） |
| `run_sleep_batch_extract_call_tokens_logged` | Call 1 の token 消費が sleep_runs に加算される |

### GREEN: 実装

`src/sleep/batch.rs` の `execute_batch` 関数を改修：

現行のフロー：
```
1. sleep_run 作成
2. BEFORE snapshot 保存
3. chunk ループ → LLM Call → 既存の memory update
4. AFTER snapshot 保存
5. run success update
```

新フロー（Call 1 を追加）：
```
1. sleep_run 作成（既存）
2. BEFORE snapshot 保存（既存）
3. [NEW] LLM Call 1: Event Extraction（chunk ごとに実行）
   - build_session_text_chunks(db, sessions) → chunks
   - run_extract_events_for_chunks(provider, agent_id, chunks)
   - DB insert マージ結果（sleep_run_id 重複 skip）
   - Call 1 失敗 → warn ログ、既存フローは継続
4. chunk ループ → LLM Call 既存: memory update（変更なし）
5. AFTER snapshot 保存（既存）
6. run success update（トークン = Call1 + Call既存 の合計）
```

重要：
- 現行の `send_sleep_request` / `parse_sleep_response` は一切変更しない
- Call 1 は **ベストエフォート**。失敗しても `warn!` ログに残し、既存メモリ更新は継続
- `source_digest_md` には Call 1 で抽出された Event 数のサマリを入れる（option）
- `input_tokens` / `output_tokens` は Call 1 と Call 既存の合計
- Token 集計：
  ```rust
  let mut input_tokens = extract_result.map_or(0, |r| r.input_tokens);
  let mut output_tokens = extract_result.map_or(0, |r| r.output_tokens);
  // ... chunk ループで加算 ...
  ```

### コミット

```
feat(sleep): integrate Call 1 (Event Extraction) into execute_batch

- Call 1 runs before existing memory-update LLM call
- Extracted events inserted into episode_events
- Token usage aggregated across calls
- Call 1 failure is best-effort: warn log + existing flow continues
```

---

## Step 6: ドキュメント更新 (TDD)

### RED: テスト先行

なし（ドキュメントのみ）

### GREEN: 実装

- `docs/db.md` に `episode_events` テーブル定義を追加
- `docs/db.md` の Rust 構造体マッピングに `EpisodeEvent` を追加
- `docs/db.md` のマイグレーション節に v3 を追加
- `docs/plan/generatedEpisodeView.md` の Sleep Batch フローを修正：
  - §6.1 のフロー図に「3. [LLM Call 1] Event Extraction」「4. episode_events へ append-only insert」を追加
  - §6.2 の詳細を更新

### コミット

```
docs: update db.md and generatedEpisodeView.md for episode_events

- Add episode_events table definition
- Document EpisodeEvent / EpisodeEventKind mapping
- Update sleep batch flow diagram
```

---

## 動作確認

```bash
# 全テスト通過
cargo test -p egopulse

# Lint / 型チェック
cargo fmt --check -p egopulse
cargo check -p egopulse
cargo clippy --all-targets --all-features -- -D warnings -p egopulse

# DB マイグレーション検証（手動）
# 1. 既存 DB でアプリ起動 → v3 マイグレーション適用確認
# 2. 新規 DB でアプリ起動 → episode_events テーブル存在確認
```

---

## PR 作成

- ブランチ名: `feat/episode-events-phase1`
- PR description（日本語）:
  - Episode Event Ledger Phase 1 実装
  - `episode_events` テーブル追加
  - Sleep Batch に LLM Call 1（Event Extraction）を追加
  - 既存のメモリ更新 LLM Call は変更なし

---

## 変更ファイル一覧

| ファイル | 新規/変更 | 内容 |
|---|---|---|
| `src/storage/migration.rs` | 変更 | v3: episode_events DDL + インデックス |
| `src/storage/mod.rs` | 変更 | EpisodeEvent, EpisodeEventKind, EpisodeEventCertainty |
| `src/storage/queries.rs` | 変更 | insert/list/count episode_events |
| `src/sleep/batch.rs` | 変更 | ExtractEventsOutput, parse/build/send 関数, execute_batch 改修 |
| `src/sleep/extract_prompt.md` | **新規** | Event Extraction 用システムプロンプト |
| `docs/db.md` | 変更 | episode_events テーブル定義・マッピング |
| `docs/plan/generatedEpisodeView.md` | 変更 | Sleep Batch フロー更新 |

---

## コミット分割

| コミット | 内容 | 対象ファイル |
|---|---|---|
| `feat(migration): add episode_events table` | v3 マイグレーション | `src/storage/migration.rs` |
| `feat(storage): add EpisodeEvent data model` | 構造体・enum 追加 | `src/storage/mod.rs` |
| `feat(storage): add episode_events CRUD queries` | DB クエリ | `src/storage/queries.rs` |
| `feat(sleep): add LLM Call 1 — Event Extraction` | プロンプト・パース・送信 | `src/sleep/extract_prompt.md`, `src/sleep/batch.rs` |
| `feat(sleep): integrate Call 1 into execute_batch` | フロー統合 | `src/sleep/batch.rs` |
| `docs: update db.md and generatedEpisodeView.md` | ドキュメント | `docs/db.md`, `docs/plan/generatedEpisodeView.md` |

---

## テストケース一覧

### 全 26 件

#### migration (4)

| # | テスト名 | 内容 |
|---|---|---|
| 1 | `fresh_db_includes_episode_events` | 新規 DB にテーブル存在 |
| 2 | `migration_from_v2_to_v3_adds_episode_events` | v2→v3 でテーブル追加 |
| 3 | `episode_events_all_columns_exist` | 全カラム存在 |
| 4 | `episode_events_indexes_exist` | 4 インデックス作成 |

#### data model (5)

| # | テスト名 | 内容 |
|---|---|---|
| 5 | `episode_event_kind_display` | 8 kind の Display |
| 6 | `episode_event_kind_from_str_valid` | 8 kind の from_str |
| 7 | `episode_event_kind_from_str_invalid` | 不正 kind で Err |
| 8 | `episode_event_certainty_display` | 3 certainty の Display |
| 9 | `episode_event_certainty_from_str_invalid` | 不正 certainty で Err |

#### queries (7)

| # | テスト名 | 内容 |
|---|---|---|
| 10 | `insert_episode_event_succeeds` | 正常 insert |
| 11 | `insert_duplicate_skips` | 重複 skip |
| 12 | `list_events_by_agent_happened_desc` | agent + happened_at 降順 |
| 13 | `list_events_by_agent_kind_filter` | kind フィルタ |
| 14 | `list_events_by_agent_salience_filter` | salience フィルタ |
| 15 | `count_events_by_agent` | 件数カウント |
| 16 | `list_events_by_source_run_id` | run_id フィルタ |

#### Call 1 parse/prompt (10)

| # | テスト名 | 内容 |
|---|---|---|
| 17 | `parse_extract_events_response_valid` | 正規 JSON パース |
| 18 | `parse_missing_events_key` | events キー欠如 |
| 19 | `parse_invalid_event_kind` | 不正 kind |
| 20 | `parse_salience_out_of_range` | salience 範囲外 |
| 21 | `parse_certainty_invalid` | 不正 certainty |
| 22 | `parse_with_thinking_tags` | thinking タグ正規化 |
| 23 | `parse_json_code_block` | Markdown code block |
| 24 | `parse_valid_source_message_ids` | 正規の source_message_ids をパース |
| 25 | `parse_empty_source_message_ids` | 空配列 `[]` を許容 |
| 26 | `parse_missing_source_message_ids` | キー欠如 → ParseFailed |

---

## 工数見積もり

| Step | 内容 | 推定行数（実装+テスト） |
|---|---|---|
| Step 1 | DB マイグレーション | 30 + 40 = 70 |
| Step 2 | データモデル | 40 + 50 = 90 |
| Step 3 | DB クエリ | 60 + 80 = 140 |
| Step 4 | LLM Call 1（Event Extraction） | 100 + 90 = 190 |
| Step 5 | フロー統合 | 40 + 60 = 100 |
| Step 6 | ドキュメント | 30 + 0 = 30 |
| **合計** | | **約 620 行** |

