# Plan: long-term memory Phase 4 — 手動 sleep batch の骨格

agent_id を指定して sleep batch を手動実行できるオーケストレーションを実装する。Phase 5-7 で LLM 呼び出しを1回にする方針に合わせ、Phase 4 の時点で phase 前提の監査スキーマを整理し、run 単位の aggregate snapshot を保存する骨格にする。

> **Note**: 以下の具体的なコード例・API 設計・構成（How）はあくまで参考である。実装時によりよい設計方針があれば積極的に採用すること。

## 設計方針

- **Phase 1-3 の成果物を統合** — MemoryLoader（Phase 1）・sleep_runs/memory_snapshots クエリ（Phase 2）・collect_sleep_input（Phase 3）を呼び出し、入力収集から run 完了までの一連の流れを繋ぐ。
- **1回 LLM 呼び出し前提の監査スキーマへ先に整理する** — Phase 4 では LLM を呼ばないが、Phase 5-7 で phase ごとの中間状態を持たない設計に進むため、`phases_json`, `summary_md`, `memory_snapshots.phase`, `SnapshotPhase`, `PhaseResult` をここで削除する。
- **Skeleton は run 単位の no-op batch とする** — memory ファイルを読み込み、content_before == content_after の aggregate snapshot を file ごとに1件保存する。Pruning / Consolidation / Compression の dummy phase は作らない。
- **排他制御は `create_sleep_run` の INSERT で判定** — `create_sleep_run` 実行時に既存 running レコードの有無を同一トランザクション内で確認し、あれば `AlreadyRunning` エラーを返す。
- **CLI サブコマンドとして起動** — `egopulse sleep --agent lyre` で手動実行。`--agent` 省略時は config の default_agent を使用する。

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 実装元 | 備考 |
|---|---|---|
| Sleep audit schema 整理 | `src/storage/migration.rs`, `src/storage/mod.rs`, `src/storage/queries.rs` | phase / summary 系を削除 |
| Sleep batch オーケストレーター | `src/sleep_batch.rs`（新規） | run 作成、snapshot、success/failed |
| CLI サブコマンド（sleep） | `src/main.rs` | `egopulse sleep --agent` |
| ドキュメント更新 | `docs/commands.md`, `docs/architecture.md`, `docs/db.md` | 手動 sleep batch と監査スキーマ |

## 前提

- **Phase 1（#53）merge 済み** — `MemoryLoader`, `src/memory.rs`, `chats.agent_id`, migration v4
- **Phase 2（#54）merge 済み** — `SleepRun`, `SleepRunStatus`, `SleepRunTrigger`, `MemorySnapshot`, `SnapshotPhase`, `MemoryFile`, migration v5, CRUD クエリ
- **Phase 3（#55）merge 済み** — `InputDecision`, `AgentSessionInfo`, `collect_sleep_input`, `count_agent_messages_since`, `get_agent_sessions_since`

> **重要**: Phase 4 は Phase 1-3 が全て main に merge された後のブランチをベースに適用する。sleep batch はまだ運用されていないため、監査スキーマ整理では既存データ互換を持たせない。

## Step 0: Worktree 作成

```bash
# Issue #56 ブランチで worktree 作成
```

## Step 1: Storage — One-call Sleep Batch Audit Schema (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `migration_sleep_runs_has_no_phases_json` | `sleep_runs` に `phases_json` が存在しない |
| `migration_sleep_runs_has_no_summary_md` | `sleep_runs` に `summary_md` が存在しない |
| `migration_memory_snapshots_has_no_phase` | `memory_snapshots` に `phase` が存在しない |
| `sleep_run_type_has_no_phases_json` | `SleepRun` 型に `phases_json` がない |
| `sleep_run_type_has_no_summary_md` | `SleepRun` 型に `summary_md` がない |
| `memory_snapshot_type_has_no_phase` | `MemorySnapshot` 型に `phase` がない |
| `snapshot_phase_type_is_removed` | `SnapshotPhase` 型が不要になっている |
| `create_memory_snapshot_takes_no_phase` | snapshot 作成 API が phase 引数を取らない |
| `update_sleep_run_success_takes_no_summary_or_phases` | success 更新 API が summary/phases を取らない |

### GREEN: 実装

`sleep_runs` から以下を削除する:

- `phases_json`
- `summary_md`

`memory_snapshots` から以下を削除する:

- `phase`

Rust 型/API から以下を削除する:

- `SnapshotPhase`
- `SleepRun.phases_json`
- `SleepRun.summary_md`
- `MemorySnapshot.phase`
- `create_memory_snapshot(..., phase, ...)`
- `update_sleep_run_success(..., phases_json, summary_md, ...)`

既に Phase 2 migration が main に入っている場合は、次 migration で `sleep_runs` / `memory_snapshots` を再作成する。sleep batch は未運用のため、旧データ移行や後方互換分岐は追加しない。

### コミット

`feat(storage): simplify sleep batch audit schema for one-call processing`

## Step 2: Sleep Batch オーケストレーター (TDD)

前提: Step 1, Phase 1-3 全て

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `run_sleep_batch_skips_when_input_below_threshold` | InputDecision::Skip の場合、sleep_run を作成せず終了 |
| `run_sleep_batch_creates_run_on_proceed` | InputDecision::Proceed の場合、status=running で run を作成 |
| `run_sleep_batch_rejects_double_execution` | 既に running の run がある場合、AlreadyRunning エラーを返す |
| `run_sleep_batch_saves_aggregate_snapshots` | 存在する memory ファイルごとに before==after snapshot を保存 |
| `run_sleep_batch_does_not_record_phases_json` | phase 結果を記録しない |
| `run_sleep_batch_does_not_record_summary_md` | summary を記録しない |
| `run_sleep_batch_marks_success_on_completion` | 完了時、run が success になる。finished_at・tokens が設定される |
| `run_sleep_batch_marks_failed_on_error` | エラー時、run が failed になる。error_message が設定される |
| `run_sleep_batch_handles_missing_memory_files` | memory ファイルが全て存在しなくても snapshot なしで完了する |
| `run_sleep_batch_handles_no_memory_dir` | memory ディレクトリ自体がなくても完了する |
| `run_sleep_batch_uses_default_agent_when_none` | agent_id が None の場合、config.default_agent を使用 |

### GREEN: 実装

新規ファイル `src/sleep_batch.rs`:

- `SleepBatchError` enum（thiserror）
  - `AlreadyRunning { agent_id: String }`
  - `Storage(#[from] StorageError)`
  - `Internal(String)`
- `pub(crate) async fn run_sleep_batch(state: &AppState, agent_id: Option<&str>) -> Result<(), SleepBatchError>`

処理フロー:

1. agent_id を解決（None → `state.config.default_agent`）
2. `collect_sleep_input(db, agent_id)`
   - `Skip` の場合: tracing::info でスキップ理由を出力し、`Ok(())` で返す（run レコードは作成しない）
   - `Proceed` の場合: source sessions と `source_chats_json` を保持
3. `create_sleep_run(agent_id, SleepRunTrigger::Manual)`
   - 既に running があれば `AlreadyRunning`
4. `state.memory_loader.load(agent_id)`
5. 各 memory file（episodic / semantic / prospective）について、content があれば `create_memory_snapshot(run_id, agent_id, file, &content, &content)` で aggregate snapshot 保存
6. `update_sleep_run_success(run_id, source_chats_json, None, 0, 0)`
7. エラー時は `update_sleep_run_failed(run_id, error_message)`

`PhaseResult` / dummy phase / `execute_dummy_phase` は作らない。

`src/lib.rs` に `pub mod sleep_batch;` を追加する。

### コミット

`feat(sleep-batch): add manual sleep batch skeleton with aggregate snapshots`

## Step 3: CLI サブコマンド (TDD)

前提: Step 2

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `sleep_command_parses_with_agent_flag` | `sleep --agent lyre` が正しくパースされる |
| `sleep_command_parses_without_agent_flag` | `sleep` 単体でもパースされる（agent = None） |
| `sleep_command_rejects_invalid_flags` | `sleep --invalid` がパースエラーになる |

### GREEN: 実装

`src/main.rs`:

- `Command` enum に `Sleep` variant 追加
- `run()` 関数の dispatch に `Sleep` を追加
- `AlreadyRunning` は stderr にわかりやすく出して exit code 1

### コミット

`feat(cli): add egopulse sleep subcommand for manual sleep batch execution`

## Step 4: ドキュメント更新 (TDD)

前提: Step 1-3

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `docs_commands_mentions_sleep_command` | `docs/commands.md` に `egopulse sleep` がある |
| `docs_architecture_mentions_manual_sleep_batch` | `docs/architecture.md` に手動 sleep batch 概要がある |
| `docs_db_has_no_phases_json` | `docs/db.md` に `sleep_runs.phases_json` が残っていない |
| `docs_db_has_no_memory_snapshot_phase` | `docs/db.md` に `memory_snapshots.phase` が残っていない |
| `docs_db_mentions_aggregate_snapshot_policy` | `docs/db.md` に aggregate snapshot 方針がある |

### GREEN: 実装

| ファイル | 更新内容 |
|---|---|
| `docs/commands.md` | コマンド一覧に `egopulse sleep` 追加。説明・引数・設定必須を記載 |
| `docs/architecture.md` | 長期記憶セクションに手動 sleep batch 骨格を追加 |
| `docs/db.md` | phase / summary 系を削除した sleep batch 監査スキーマ、aggregate snapshot 方針 |

### コミット

`docs: update sleep batch skeleton docs and audit schema`

## Step 5: 動作確認

```bash
cargo fmt --check
cargo test -p egopulse
cargo check -p egopulse
cargo clippy --all-targets --all-features -- -D warnings
```

## Step 6: PR 作成

- ブランチ: `feat/memory-phase4-sleep-batch-skeleton`
- PR description: 日本語。`Close #56` 明記
- Phase 5-7 の1回 LLM 呼び出し方針に合わせ、phase / summary 系の監査スキーマを整理したことを明記する

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
|---|---|---|
| `src/storage/migration.rs` | 変更 | sleep batch 監査テーブル再定義 |
| `src/storage/mod.rs` | 変更 | phase / summary 系型削除 |
| `src/storage/queries.rs` | 変更 | sleep_runs / memory_snapshots query signature 更新 |
| `src/sleep_batch.rs` | **新規** | オーケストレーター・SleepBatchError・テスト |
| `src/lib.rs` | 変更 | `pub mod sleep_batch;` 追加 |
| `src/main.rs` | 変更 | `Sleep` variant 追加、dispatch 追加、テスト追加 |
| `docs/commands.md` | 変更 | `egopulse sleep` コマンド追加 |
| `docs/architecture.md` | 変更 | sleep batch 概要追加 |
| `docs/db.md` | 変更 | one-call 前提の sleep batch 監査スキーマ |

## コミット分割

1. `feat(storage): simplify sleep batch audit schema for one-call processing` — `src/storage/*`
2. `feat(sleep-batch): add manual sleep batch skeleton with aggregate snapshots` — `src/sleep_batch.rs`, `src/lib.rs`
3. `feat(cli): add egopulse sleep subcommand for manual sleep batch execution` — `src/main.rs`
4. `docs: update sleep batch skeleton docs and audit schema` — `docs/`

## テストケース一覧（全 28 件）

### Storage (9)

1. `migration_sleep_runs_has_no_phases_json` — `phases_json` 削除
2. `migration_sleep_runs_has_no_summary_md` — `summary_md` 削除
3. `migration_memory_snapshots_has_no_phase` — snapshot phase 削除
4. `sleep_run_type_has_no_phases_json` — SleepRun 型整理
5. `sleep_run_type_has_no_summary_md` — SleepRun 型整理
6. `memory_snapshot_type_has_no_phase` — MemorySnapshot 型整理
7. `snapshot_phase_type_is_removed` — SnapshotPhase 削除
8. `create_memory_snapshot_takes_no_phase` — snapshot API 整理
9. `update_sleep_run_success_takes_no_summary_or_phases` — success API 整理

### Sleep Batch Orchestrator (11)

10. `run_sleep_batch_skips_when_input_below_threshold` — Skip 時 run 作成なし
11. `run_sleep_batch_creates_run_on_proceed` — Proceed 時 run 作成
12. `run_sleep_batch_rejects_double_execution` — 二重起動で AlreadyRunning エラー
13. `run_sleep_batch_saves_aggregate_snapshots` — run 単位 snapshot 保存
14. `run_sleep_batch_does_not_record_phases_json` — phases_json 不使用
15. `run_sleep_batch_does_not_record_summary_md` — summary_md 不使用
16. `run_sleep_batch_marks_success_on_completion` — 完了時 success
17. `run_sleep_batch_marks_failed_on_error` — エラー時 failed
18. `run_sleep_batch_handles_missing_memory_files` — memory ファイル不在
19. `run_sleep_batch_handles_no_memory_dir` — memory dir 不在
20. `run_sleep_batch_uses_default_agent_when_none` — default agent

### CLI Parser (3)

21. `sleep_command_parses_with_agent_flag` — `--agent` パース
22. `sleep_command_parses_without_agent_flag` — 引数なしパース
23. `sleep_command_rejects_invalid_flags` — 不正フラグ

### Docs (5)

24. `docs_commands_mentions_sleep_command` — commands
25. `docs_architecture_mentions_manual_sleep_batch` — architecture
26. `docs_db_has_no_phases_json` — phases_json 削除
27. `docs_db_has_no_memory_snapshot_phase` — snapshot phase 削除
28. `docs_db_mentions_aggregate_snapshot_policy` — db snapshot policy

## 工数見積もり

| Step | 内容 | 見積もり |
|---|---|---|
| Step 1 | Storage schema 整理 | ~260 行 |
| Step 2 | オーケストレーター + テスト | ~260 行 |
| Step 3 | CLI サブコマンド + テスト | ~50 行 |
| Step 4 | ドキュメント更新 | ~100 行 |
| **合計** | | **~670 行** |
