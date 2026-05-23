# Plan: Episode View Generation (Phase 1 — Event Extraction)

Session ログから LLM を使ってエピソード記憶を抽出し `episode_events` テーブルに保存する Phase 1。
Sleep Batch の流れに Call 1（Event Extraction）を追加し、episode_events テーブルを新規作成する。

> **Note**: 以下の具体的なコード例・API 設計・構成（How）はあくまで参考である。実装時によりよい設計方針があれば積極的に採用すること。

## 設計方針

- **append-only 台帳**: `episode_events` は不変の追記専用テーブル。同一 sleep_run_id の再実行時のみ冪等に全削除→再挿入
- **Event Extraction は best-effort**: 抽出失敗時もメモリ更新（Call 2）は続行する。抽出結果が空でも run としては成功
- **chunk 対応**: 大量メッセージを chunk 分割し、各 chunk に対して個別に Event Extraction を呼び出す
- **CHECK 制約で入力を検証**: kind（8種）、ripple_strength（1〜5）、certainty（3種）を DB レベルで強制

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 種別 | 備考 |
|---|---|---|
| DB migration v3 | migration | `episode_events` テーブル + 4 インデックス + CHECK 制約 |
| `EpisodeEventKind` enum (8種) | struct | self / relationship / world / feat / anomaly / decision / insight / rhythm |
| `EpisodeEventCertainty` enum (3種) | struct | observed / inferred / uncertain |
| `EpisodeEvent` struct | struct | episode_events の行マッピング |
| CRUD queries | storage | insert / list / count / list-by-run |
| Event Extraction (LLM Call 1) | sleep/batch | 新規 LLM 呼び出し + パース + DB 保存 |
| System prompt for extraction | sleep | `extract_prompt.md` |
| JSON parsing | sleep | `parse_extract_events_response` |
| Chunking | sleep | `build_extraction_chunks` + `run_extract_events_for_chunks` |
| ドキュメント | docs | `db.md` 更新、`generatedEpisodeView.md` 作成 |

## 前提

- Sleep Batch 骨格（Phase 4）が実装済み
- `sleep_runs` / `memory_snapshots` テーブルが存在
- `collect_sleep_input` でセッション収集可能

## Sleep Batch 全体像（Phase 1 時点）

```
1.  対象セッション収集
2.  Sleep Run 作成
3.  [LLM Call 1]  Event Extraction（Phase 1 新規追加）
4.  episode_events へ append-only insert
5.  [既存 LLM Call]  episodic / semantic / prospective 一括更新
6.  episodic.md / semantic.md / prospective.md 保存
7.  対象セッションを archive
8.  messages_json = []
9.  Sleep Run 完了
```

> **将来 Phases**: Call 2（Episodic View Generation）と Call 3（Semantic / Prospective Consolidation）への分割は vNext で対応予定。Phase 1 では Event Extraction + 従来の一括更新の 2 段構成。

## Step 0: Worktree 作成

```bash
# Episode Events Phase 1 ブランチで worktree 作成
```

## Step 1: Storage — episode_events テーブル (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `fresh_db_includes_episode_events` | 新規 DB に episode_events テーブルが存在する |
| `migration_from_v2_to_v3_adds_episode_events` | v2 → v3 マイグレーションで episode_events が作成される |
| `episode_events_all_columns_exist` | 全カラム（id, agent_id, … updated_at）が存在する |
| `episode_events_indexes_exist` | 4 つのインデックスがすべて存在する |
| `insert_and_list_episode_events` | イベントを insert → list で取得できる |
| `insert_episode_events_dedup` | 同一 run_id の再 insert で既存レコードが削除される |
| `list_episode_events_kind_filter` | kind でフィルタリングできる |
| `list_episode_events_ripple_filter` | ripple_strength でフィルタリングできる |
| `count_episode_events` | 件数カウントが正しい |
| `list_episode_events_by_run` | run_id 単位の取得が正しい |

### GREEN: 実装

1. `SCHEMA_VERSION` を 2 → 3 にインクリメント
2. `run_migrations()` に `if version < 3` ブロックを追加
3. DDL: `CREATE TABLE IF NOT EXISTS episode_events (...)` + 4 indexes + CHECK 制約
4. `mod.rs` に `EpisodeEventKind` enum（8種）、`EpisodeEventCertainty` enum（3種）、`EpisodeEvent` struct を追加
5. `queries.rs` に CRUD 操作を追加:
   - `insert_episode_events(run_id, events)` — 同一 run_id を DELETE してから batch INSERT
   - `list_episode_events(agent_id, kind, min_ripple, limit)` — kind + ripple フィルタ、experienced_at DESC
   - `count_episode_events(agent_id)` — COUNT(*)
   - `list_episode_events_by_run(run_id)` — experienced_at ASC

### REFACTOR: 不要コード確認

- `#[allow(dead_code)]` が適切か確認（Phase 1 では Sleep Batch からの呼び出しは dead_code になる場合がある）
- 公開範囲: `EpisodeEvent` / `EpisodeEventKind` / `EpisodeEventCertainty` / CRUD 関数は `pub(crate)`

## Step 2: Sleep Batch — Event Extraction (LLM Call 1)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `parse_extract_events_response_valid` | 正常な JSON がパースできる |
| `parse_extract_events_response_invalid` | 不正な JSON でエラー |
| `parse_extract_events_response_empty` | 空配列が扱える |
| `build_extraction_chunks` | メッセージが chunk 分割される |
| `build_extract_system_prompt` | プロンプトに agent_id が埋め込まれる |
| `extract_events_retry_on_failure` | 初回パース失敗 → リトライ動作 |

### GREEN: 実装

1. `extract_prompt.md` を作成（Event Extraction 用システムプロンプト）
2. `ExtractedEvent` / `ExtractEventsOutput` / `ExtractionChunk` / `SourceRef` 構造体を追加
3. `parse_extract_events_response` — JSON パース + バリデーション
4. `build_extract_system_prompt` — prompt.md + 入力セッション
5. `build_extraction_chunks` — メッセージを max_chars で分割
6. `send_extract_events_request` — LLM 呼び出し + リトライ
7. `run_extract_events_for_chunks` — chunk ごとに実行して統合
8. `execute_batch` 内で Call 1 → insert → Call 2 の順に呼び出し

### アーキテクチャ上の決定

- Event Extraction は best-effort: 失敗時は warn ログを出力し Call 2 に進む
- 抽出が成功した場合のみ `episode_events` に insert（空配列の場合は insert しない）
- source_refs_json は LLM が返した source_message_ids を実際の chat_id/message_id に解決して生成

## Step 3: ドキュメント更新

1. `docs/db.md` — episode_events テーブル定義 + インデックス + migration v3 + struct mapping
2. `docs/plan/generatedEpisodeView.md` — 本ファイル

## コミット分割案

| # | コミットメッセージ | 内容 |
|---|---|---|
| 1 | `feat(db): add episode_events table with v3 migration` | DDL + indexes + CHECK 制約 |
| 2 | `feat(db): add EpisodeEvent struct, enums, and CRUD queries` | EpisodeEvent, EpisodeEventKind, EpisodeEventCertainty + queries |
| 3 | `feat(sleep): implement event extraction LLM call` | extract_prompt.md, parsing, chunking, orchestration |
| 4 | `docs(db): document episode_events table and v3 migration` | db.md + generatedEpisodeView.md |

## 工数見積もり

| Step | 工数 |
|------|------|
| Step 1 (Storage) | 2h |
| Step 2 (Sleep Batch) | 3h |
| Step 3 (Documentation) | 0.5h |
| **合計** | **5.5h** |
