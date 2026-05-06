# Plan: long-term memory Phase 2 — DB 監査基盤

睡眠バッチの実行履歴（sleep_runs）と記憶ファイル更新履歴（memory_snapshots）を追跡する DB テーブル・クエリを実装する。実際のバッチ実行・LLM 呼び出しは含まない。

> **Note**: 以下の具体的なコード例・API 設計・構成（How）はあくまで参考である。実装時によりよい設計方針があれば積極的に採用すること。

## 設計方針

- **設計書（#70）のスキーマに準拠** — sleep_runs / memory_snapshots のカラム・インデックスは設計書の定義に従う。設計書にない追加カラムは入れない。ただし `trigger` は SQLite 予約語のため `trigger_type` にリネームする（この変更は Issue #70 の設計書にも反映する）
- **整合性はアプリケーション層** — 既存 DB 方針（外部キーなし・CASCADE なし）に従い、参照整合性は Rust 側で担保する
- **ID は UUID v4** — 既存コードベースの ID 生成パターン（`uuid::Uuid::new_v4().to_string()`）に統一
- **enum は Rust 型で表現** — status / trigger / phase / file を型安全に扱い、DB には文字列で保存する
- **Phase 1（#53）が merge 済みであることが前提** — Migration v5 は Phase 1 の v4 に依存する。Phase 1 未適用の DB に対しては適用できない

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 実装元 |
|---|---|
| DB Migration（sleep_runs / memory_snapshots テーブル作成） | `src/storage/migration.rs` |
| Rust 構造体定義 | `src/storage/mod.rs` |
| CRUD クエリ | `src/storage/queries.rs` |
| ドキュメント更新 | `docs/db.md` |
| 設計書更新（trigger → trigger_type） | Issue #70 本文 |

---

## Step 0: Worktree 作成

```bash
# Issue #54 ブランチで worktree 作成
```

---

## Step 1: Rust 型定義 (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `sleep_run_status_display` | SleepRunStatus の Display が小文字文字列を返す（"running", "success", ...） |
| `sleep_run_trigger_display` | SleepRunTrigger の Display が小文字文字列を返す（"manual", "scheduled"） |
| `snapshot_phase_display` | SnapshotPhase の Display が小文字文字列を返す（"pruning", ...） |
| `memory_file_display` | MemoryFile の Display が小文字文字列を返す（"episodic", ...） |
| `sleep_run_status_from_str` | 文字列から SleepRunStatus への変換（"running" → Running） |
| `sleep_run_trigger_from_str` | 文字列から SleepRunTrigger への変換 |
| `snapshot_phase_from_str` | 文字列から SnapshotPhase への変換 |
| `memory_file_from_str` | 文字列から MemoryFile への変換 |

### GREEN: 実装

`src/storage/mod.rs` に追加:

- `SleepRun` 構造体
  - `id: String`, `agent_id: String`, `status: SleepRunStatus`, `trigger: SleepRunTrigger`
  - `started_at: String`, `finished_at: Option<String>`
  - `source_chats_json: String`, `source_digest_md: Option<String>`
  - `phases_json: String`, `summary_md: Option<String>`
  - `input_tokens: i64`, `output_tokens: i64`, `total_tokens: i64`
  - `error_message: Option<String>`
- `SleepRunStatus` enum — `Running | Success | Failed | Skipped`
- `SleepRunTrigger` enum — `Manual | Scheduled`
- `MemorySnapshot` 構造体
  - `id: String`, `run_id: String`, `agent_id: String`
  - `phase: SnapshotPhase`, `file: MemoryFile`
  - `content_before: String`, `content_after: String`
  - `created_at: String`
- `SnapshotPhase` enum — `Pruning | Consolidation | Compression`
- `MemoryFile` enum — `Episodic | Semantic | Prospective`
- 各 enum に `Display` + `FromStr` 実装（DB との文字列表現の往復）

### コミット

`feat(storage): add Rust types for sleep runs and memory snapshots`

---

## Step 2: DB Migration v5 (TDD)

前提: Step 1

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `migration_v5_creates_sleep_runs_table` | v5 適用後、sleep_runs テーブルが存在する |
| `migration_v5_creates_memory_snapshots_table` | v5 適用後、memory_snapshots テーブルが存在する |
| `migration_v5_creates_four_indexes` | 4つのインデックスが作成される（agent+started, agent+status, run_id, agent+created） |
| `migration_v5_history_is_recorded` | schema_migrations に v5 レコードが追加される |
| `migration_v5_from_v4_db` | v4 DB に対して v5 が正しく適用される |
| `migration_v5_from_fresh_db` | 新規 DB で v1→v5 まで全マイグレーションが適用される |

### GREEN: 実装

`src/storage/migration.rs`:

- `SCHEMA_VERSION` をインクリメント（Phase 1 が v4 なので `5`）
- `if version < 5` ブロック追加:

```sql
CREATE TABLE IF NOT EXISTS sleep_runs (
    id                  TEXT PRIMARY KEY,
    agent_id            TEXT NOT NULL,
    status              TEXT NOT NULL,
    trigger_type        TEXT NOT NULL,
    started_at          TEXT NOT NULL,
    finished_at         TEXT,
    source_chats_json   TEXT NOT NULL DEFAULT '[]',
    source_digest_md    TEXT,
    phases_json         TEXT NOT NULL DEFAULT '[]',
    summary_md          TEXT,
    input_tokens        INTEGER NOT NULL DEFAULT 0,
    output_tokens       INTEGER NOT NULL DEFAULT 0,
    total_tokens        INTEGER NOT NULL DEFAULT 0,
    error_message       TEXT
);

CREATE INDEX IF NOT EXISTS idx_sleep_runs_agent_started
    ON sleep_runs(agent_id, started_at);

CREATE INDEX IF NOT EXISTS idx_sleep_runs_agent_status
    ON sleep_runs(agent_id, status);

CREATE TABLE IF NOT EXISTS memory_snapshots (
    id              TEXT PRIMARY KEY,
    run_id          TEXT NOT NULL,
    agent_id        TEXT NOT NULL,
    phase           TEXT NOT NULL,
    file            TEXT NOT NULL,
    content_before  TEXT NOT NULL,
    content_after   TEXT NOT NULL,
    created_at      TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_memory_snapshots_run_id
    ON memory_snapshots(run_id);

CREATE INDEX IF NOT EXISTS idx_memory_snapshots_agent_created
    ON memory_snapshots(agent_id, created_at);
```

- `set_schema_version(conn, 5, "add sleep_runs and memory_snapshots tables for long-term memory audit")`

> **注意**: 設計書（#70）ではカラム名 `trigger` だが、SQLite 予約語との衝突を避けるため `trigger_type` にリネームする。この変更は実装時に Issue #70 の設計書にも反映する。

### コミット

`feat(storage): add sleep_runs and memory_snapshots tables via migration v5`

---

## Step 3: sleep_runs Queries (TDD)

前提: Step 1, Step 2

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `create_sleep_run_inserts_with_running_status` | INSERT 時に status=running で作成される |
| `create_sleep_run_generates_id_and_timestamp` | id が UUID、started_at が RFC3339 で自動生成される |
| `update_sleep_run_to_success` | status を success に更新。finished_at・summary・tokens が設定される |
| `update_sleep_run_to_failed` | status を failed に更新。error_message が設定される |
| `update_sleep_run_to_skipped` | status を skipped に更新 |
| `get_sleep_run_by_id` | id で取得。全フィールドが正しく復元される |
| `get_sleep_run_returns_none_for_missing` | 存在しない id で None を返す |
| `list_sleep_runs_by_agent` | agent_id で絞り込み。started_at 降順 |
| `list_sleep_runs_empty` | データなし時に空 Vec を返す |
| `get_latest_successful_run` | status=success の最新1件を取得 |
| `get_latest_successful_run_returns_none` | success がない場合 None |

### GREEN: 実装

`src/storage/queries.rs` に追加:

- `create_sleep_run(agent_id, trigger) -> String`
  - UUID v4 で id 生成
  - status = "running"
  - started_at = now RFC3339
  - source_chats_json = "[]", phases_json = "[]"
  - INSERT → id を返す
- `update_sleep_run_success(id, source_chats_json, source_digest_md, phases_json, summary_md, input_tokens, output_tokens)`
  - status = "success", finished_at = now
  - total_tokens = input + output
- `update_sleep_run_failed(id, error_message)`
  - status = "failed", finished_at = now
- `update_sleep_run_skipped(id)`
  - status = "skipped", finished_at = now
- `get_sleep_run(id) -> Option<SleepRun>`
- `list_sleep_runs(agent_id, limit) -> Vec<SleepRun>`
- `get_latest_successful_run(agent_id) -> Option<SleepRun>`

### コミット

`feat(storage): add CRUD queries for sleep_runs table`

---

## Step 4: memory_snapshots Queries (TDD)

前提: Step 1, Step 2

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `create_memory_snapshot_inserts_record` | INSERT で正しく格納される |
| `create_memory_snapshot_generates_id_and_timestamp` | id が UUID、created_at が RFC3339 |
| `get_snapshots_for_run` | run_id で絞り込み。created_at 昇順 |
| `get_snapshots_for_run_empty` | 該当なし時に空 Vec を返す |
| `get_snapshots_for_agent` | agent_id で絞り込み。created_at 降順 |
| `get_snapshots_filters_by_phase` | 特定 phase だけ取得できる |
| `get_snapshots_filters_by_file` | 特定 file だけ取得できる |
| `get_latest_snapshot_for_file` | 特定 agent+file の最新 snapshot を1件取得 |
| `get_latest_snapshot_returns_none` | 該当なし時に None |

### GREEN: 実装

`src/storage/queries.rs` に追加:

- `create_memory_snapshot(run_id, agent_id, phase, file, content_before, content_after) -> String`
  - UUID v4 で id 生成
  - created_at = now RFC3339
  - INSERT → id を返す
- `get_snapshots_for_run(run_id) -> Vec<MemorySnapshot>`
  - created_at 昇順（phase 実行順を保持）
- `get_snapshots_for_agent(agent_id, limit) -> Vec<MemorySnapshot>`
  - created_at 降順
- `get_latest_snapshot_for_file(agent_id, file) -> Option<MemorySnapshot>`
  - 特定 agent + file の最新1件

### コミット

`feat(storage): add CRUD queries for memory_snapshots table`

---

## Step 5: ドキュメント更新

### 実装

| ファイル | 更新内容 |
|---|---|
| `docs/db.md` | sleep_runs / memory_snapshots のテーブル定義・カラム説明・ER 図更新・Rust 構造体マッピング更新・インデックス数更新 |

### コミット

`docs: update db.md with sleep_runs and memory_snapshots schema`

---

## Step 6: 動作確認

```bash
cargo fmt --check
cargo test -p egopulse
cargo check -p egopulse
cargo clippy --all-targets --all-features -- -D warnings
```

---

## Step 7: PR 作成

- ブランチ: `feat/memory-phase2-db-audit`
- PR description: 日本語。`Close #54` 明記
- Issue #54 の DoD チェックリストを PR 本文に記載

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
|---|---|---|
| `src/storage/mod.rs` | 変更 | SleepRun / MemorySnapshot 構造体、各 enum と Display/FromStr 実装、テスト |
| `src/storage/migration.rs` | 変更 | Migration v5 追加、SCHEMA_VERSION 更新、テスト |
| `src/storage/queries.rs` | 変更 | sleep_runs / memory_snapshots CRUD クエリ、テスト |
| `docs/db.md` | 変更 | 新テーブル定義、ER 図更新、構造体マッピング更新 |

---

## コミット分割

1. `feat(storage): add Rust types for sleep runs and memory snapshots` — storage/mod.rs
2. `feat(storage): add sleep_runs and memory_snapshots tables via migration v5` — migration.rs
3. `feat(storage): add CRUD queries for sleep_runs table` — queries.rs
4. `feat(storage): add CRUD queries for memory_snapshots table` — queries.rs
5. `docs: update db.md with sleep_runs and memory_snapshots schema` — docs/

---

## テストケース一覧（全 34 件）

### Rust 型 (8)

1. `sleep_run_status_display` — Display が小文字文字列
2. `sleep_run_trigger_display` — Display が小文字文字列
3. `snapshot_phase_display` — Display が小文字文字列
4. `memory_file_display` — Display が小文字文字列
5. `sleep_run_status_from_str` — 文字列→enum 変換
6. `sleep_run_trigger_from_str` — 文字列→enum 変換
7. `snapshot_phase_from_str` — 文字列→enum 変換
8. `memory_file_from_str` — 文字列→enum 変換

### DB Migration (6)

9. `migration_v5_creates_sleep_runs_table` — テーブル存在確認
10. `migration_v5_creates_memory_snapshots_table` — テーブル存在確認
11. `migration_v5_creates_four_indexes` — 4インデックス確認
12. `migration_v5_history_is_recorded` — schema_migrations 記録
13. `migration_v5_from_v4_db` — v4→v5 移行
14. `migration_v5_from_fresh_db` — 新規DB 全マイグレーション

### sleep_runs Queries (11)

15. `create_sleep_run_inserts_with_running_status` — status=running で INSERT
16. `create_sleep_run_generates_id_and_timestamp` — id/started_at 自動生成
17. `update_sleep_run_to_success` — success 更新
18. `update_sleep_run_to_failed` — failed 更新 + error_message
19. `update_sleep_run_to_skipped` — skipped 更新
20. `get_sleep_run_by_id` — id 取得・全フィールド復元
21. `get_sleep_run_returns_none_for_missing` — 存在しない id で None
22. `list_sleep_runs_by_agent` — agent 絞り込み + 降順
23. `list_sleep_runs_empty` — 空結果
24. `get_latest_successful_run` — success 最新1件
25. `get_latest_successful_run_returns_none` — success なし

### memory_snapshots Queries (9)

26. `create_memory_snapshot_inserts_record` — INSERT 正常
27. `create_memory_snapshot_generates_id_and_timestamp` — id/created_at 自動生成
28. `get_snapshots_for_run` — run_id 絞り込み・昇順
29. `get_snapshots_for_run_empty` — 空結果
30. `get_snapshots_for_agent` — agent_id 絞り込み・降順
31. `get_snapshots_filters_by_phase` — phase フィルタ
32. `get_snapshots_filters_by_file` — file フィルタ
33. `get_latest_snapshot_for_file` — agent+file の最新1件
34. `get_latest_snapshot_returns_none` — 該当なし

---

## 工数見積もり

| Step | 内容 | 見積もり |
|---|---|---|
| Step 1 | Rust 型定義（struct + enum + Display/FromStr + テスト） | ~160 行 |
| Step 2 | Migration v5 | ~80 行（テスト 50 + 実装 30） |
| Step 3 | sleep_runs Queries | ~200 行（テスト 120 + 実装 80） |
| Step 4 | memory_snapshots Queries | ~180 行（テスト 110 + 実装 70） |
| Step 5 | ドキュメント更新 | ~120 行 |
| **合計** | | **~740 行** |
