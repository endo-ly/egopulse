# Plan: Call2 Episodic View Materialization

Sleep Batch の Call2 を実装する。`episode_events` を正本とし、週次/月次の派生要約 (`episode_rollups`) を LLM で生成し、Rust 側で固定テンプレートにより `episodic.md` を生成する。仕様書: `docs/sleep-call2.md`（875行）。

> **Note**: 以下の具体的なコード例・API 設計・構成（How）はあくまで参考である。実装時によりよい設計方針があれば積極的に採用すること。

## 設計方針

- **正本とビューの分離**: `episode_events` は append-only の正本。`episode_rollups` は再生成可能な派生キャッシュ。`episodic.md` は毎回再生成してよい注入ビュー。この3層構造を厳守する
- **LLM 呼び出しの最小化**: Rollup Planner が Rust 側で更新対象を判定し、`rollup_requests` が非空の場合のみ LLM Call2 を実行する。毎日実行される Episodic Renderer は LLM を使わない
- **既存パターンの踏襲**: Call1 の実装パターン（`batch.rs` 内の best-effort 実行、トークン集計、エラー処理）と Storage のクエリパターン（raw SQL + `prepare_cached` + `params!`）に合わせる
- **既存の古い Plan の取扱い**: `docs/plan/generatedEpisodeView.md` は Call2 設計が旧仕様（LLM が全文生成）であり本仕様と矛盾するため、本 Plan 完了後に `archived/` へ移動する

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 実装元 |
|---|---|
| `episode_rollups` テーブル（DDL + migration v6） | `sleep-call2.md` §5 |
| `EpisodeRollup` 構造体 + `RollupGranularity` enum | `sleep-call2.md` §5 |
| Rollup CRUD クエリ関数（upsert / list / get） | `sleep-call2.md` §15 |
| Rollup Planner（更新対象期間判定） | `sleep-call2.md` §8, §9, §10 |
| Call2 Input ビルダー（`rollup_requests` 構築） | `sleep-call2.md` §6 |
| Call2 System Prompt / User Prompt | `sleep-call2.md` §13 |
| LLM Call2 実行 + 出力パース + 検証 | `sleep-call2.md` §7, §14 |
| Episodic Renderer（`episodic.md` テンプレート生成） | `sleep-call2.md` §11, §12 |
| Sleep Batch への統合（Call1 と Call3 の間に挿入） | `sleep-call2.md` §8 |
| `docs/db.md` 更新（`episode_rollups` 追記） | — |
| 旧 Plan の `archived/` 移動 | — |

---

## Step 0: Worktree 作成

`worktree-create` skill を使用して `feat/call2-episodic-view` ブランチの WT を作成する。

---

## Step 1: DB Schema — `episode_rollups` (TDD)

前提: なし（最初の Step）

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `test_migration_v6_creates_episode_rollups` | migration 実行後に `episode_rollups` テーブルが存在し、カラム・制約が正しい |
| `test_upsert_episode_rollup_insert_new` | 新規 rollup の INSERT |
| `test_upsert_episode_rollup_update_existing` | 同一 `(agent_id, granularity, period_key)` で UPDATE（`summary_md`, `max_ripple`, `event_count`, `updated_at` が更新され、`created_at` は不変） |
| `test_list_episode_rollups_by_granularity` | `granularity = 'week'` でフィルタ＋`period_start` DESC 順 |
| `test_get_episode_rollup_by_period_key` | `(agent_id, granularity, period_key)` で1件取得 |
| `test_list_episode_rollups_period_range` | 期間範囲指定（`period_start` between）で取得 |
| `test_list_episode_rollups_for_background` | `granularity = 'month'`, `max_ripple >= 4`, `period_start` が recent months より古いものを取得 |

### GREEN: 実装

- `src/storage/migration.rs`: `SCHEMA_VERSION` を 6 に更新、`if version < 6` ブロックで `episode_rollups` テーブル + 2インデックスを作成
- `src/storage/mod.rs`: `EpisodeRollup` 構造体、`RollupGranularity` enum（`Week`, `Month`）を追加
- `src/storage/queries.rs`: 以下の関数を追加
  - `upsert_episode_rollup(conn, &rollup)` — `INSERT ... ON CONFLICT(agent_id, granularity, period_key) DO UPDATE SET ...`
  - `list_episode_rollups(conn, agent_id, granularity, limit)` — `ORDER BY period_start DESC`
  - `get_episode_rollup(conn, agent_id, granularity, period_key)` — 1件取得
  - `list_episode_rollups_in_range(conn, agent_id, granularity, start, end_exclusive)` — 期間範囲
  - `list_background_episode_rollups(conn, agent_id, min_ripple, before_period_start)` — Background Months 用

### コミット

`feat(schema): add episode_rollups table with migration v6`

---

## Step 2: Rollup Planner (TDD)

前提: Step 1

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `test_current_week_monday_start` | `now` から current week の `week_key`, `period_start`, `period_end_exclusive` を正しく計算（月曜始まり） |
| `test_recent_weeks_identifies_4_closed` | 現在週を除く直近4週の period_key を列挙 |
| `test_recent_months_identifies_2` | Recent Weeks の直前2暦月を列挙 |
| `test_detects_new_closed_week` | 先週の rollup が存在しない場合、closed_week として検出 |
| `test_detects_missing_week_rollup` | 直近4週のうち要約未生成の週を検出 |
| `test_detects_delayed_events_in_closed_week` | `encoded_at` が新しいが `experienced_at` が過去週の Event が存在する場合、該当週を更新対象に含める |
| `test_detects_event_count_mismatch` | 既存 rollup の `event_count` と実際の Event 数が異なる週を検出 |
| `test_detects_week_rolling_out` | W-5 になった週（Recent Weeks から外れる）を検出し、対応月の month rollup 更新対象に含める |
| `test_detects_missing_month_rollup` | Recent Months 用の月 rollup が未生成の月を検出 |
| `test_detects_background_candidates` | `max_ripple >= 4` の古い月で rollup が未生成のものを検出 |
| `test_returns_empty_when_no_updates_needed` | すべての rollup が最新の場合、空の `rollup_requests` を返す |
| `test_excludes_current_week_events_from_month_rollup` | Recent Months の event 収集時に Recent Weeks に含まれる Event を除外する |

### GREEN: 実装

- 新規モジュール `src/sleep/call2.rs`（または `batch.rs` 内の新セクション）に以下を実装:
  - `WeekPeriod` / `MonthPeriod` の計算ヘルパー（月曜始まりの週、暦月）
  - `RollupPlanner` 構造体: DB クエリ結果と now/timezone から更新対象期間を決定する純粋な Rust ロジック
  - `RollupRequest` 構造体: granularity, period_key, period_start/end, reason, previous_summary_md, events
  - `plan_rollup_updates(agent_id, now, timezone, db)` → `Vec<RollupRequest>`

### コミット

`feat(sleep): add Rollup Planner for Call2 period detection`

---

## Step 3: Call2 LLM Integration (TDD)

前提: Step 1, Step 2

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `test_build_call2_input_json_structure` | `rollup_requests` から Call2 入力 JSON が正しい構造で構築される（run メタ情報 + rollup_requests 配列） |
| `test_build_call2_input_includes_previous_summary` | 既存 rollup の `summary_md` が `previous_summary_md` に含まれる |
| `test_build_call2_input_includes_events` | 各 request に該当期間の Event が含まれる（id, experienced_at, kind, title, body_md, ripple_strength, certainty） |
| `test_parse_call2_output_valid_json` | 正常な JSON 出力をパースして `Vec<RollupOutput>` を取得 |
| `test_parse_call2_output_missing_rollups_key` | `rollups` キーがない場合にエラー |
| `test_parse_call2_output_invalid_granularity` | `granularity` が `week`/`month` 以外の場合にエラー |
| `test_parse_call2_output_unknown_period_key` | 入力 request にない `period_key` が返された場合にエラー |
| `test_validate_summary_md_empty` | `summary_md` が空の場合、その rollup をスキップ |
| `test_validate_summary_md_too_long` | `summary_md` が過度に長い場合にエラーまたは切り詰め |
| `test_validate_no_event_ids_in_output` | Event ID らしき文字列（`evt_` prefix 等）が含まれる場合にエラー |
| `test_validate_max_ripple_range` | `max_ripple` が 1-5 の範囲外の場合にエラー |
| `test_validate_event_count_non_negative` | `event_count` が負の場合にエラー |
| `test_call2_retry_on_json_parse_failure` | JSON parse 失敗時に1回リトライする |
| `test_call2_retry_on_missing_field` | 必須フィールド欠落時に1回リトライする |
| `test_call2_fallback_on_retry_exhaustion` | リトライ後も失敗した場合、既存 rollup を維持して次へ進む |
| `test_security_redaction_in_input` | 入力 Event の `body_md` に API key / token パターンが含まれる場合に redaction される |
| `test_security_redaction_in_output` | LLM 出力に API key / token パターンが含まれる場合に redaction される |

### GREEN: 実装

- `src/sleep/call2.rs` に以下を追加:
  - `Call2Input` / `Call2Output` 構造体（Serialize/Deserialize）
  - `build_call2_input(run_context, rollup_requests)` — 入力 JSON 構築
  - `parse_call2_output(json_str)` — 出力 JSON パース + 検証
  - `validate_rollup(rollup, valid_period_keys)` — 内容検証
  - `redact_secrets(text)` — セキュリティ redaction
  - `run_call2(state, agent_id, run_context, rollup_requests)` — LLM 呼び出しのオーケストレーション（リトライ付き）
- `src/sleep/sleep_batch_prompt_2.md` — Call2 System Prompt（`sleep-call2.md` §13.1 の内容）
- Call2 User Prompt はコード内で構築（`sleep-call2.md` §13.2）

### コミット

`feat(sleep): add Call2 LLM integration with validation and retry`

---

## Step 4: Episodic Renderer (TDD)

前提: Step 1

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `test_render_header_metadata` | `generated`, `mode`, `tz` が正しく出力される |
| `test_render_current_week_events_by_date` | Current Week の Event が日付ごとにグループ化され、`### YYYY-MM-DD` 見出しで出力される |
| `test_render_current_week_event_format` | 各 Event が `- [{kind} r{ripple}] {title}` 形式で出力される |
| `test_render_current_week_body_truncation` | `body_md` が長い場合に1〜2行に切り詰められる |
| `test_render_current_week_sorted_by_experienced_at` | 同日内で `experienced_at` 昇順にソートされる |
| `test_render_recent_weeks_from_rollups` | 直近4週の rollup が `### 2026-W21 (2026-05-18..2026-05-24) r5` 形式で出力される |
| `test_render_recent_months_from_rollups` | 直近2月の rollup が `### 2026-04 r4` 形式で出力される |
| `test_render_background_months_filters_low_ripple` | `max_ripple < 4` の古い月が出力されない |
| `test_render_background_months_prioritizes_newer` | 同種の背景月が多い場合、より新しいものが優先される |
| `test_render_empty_current_week` | Current Week に Event がない場合、セクションを出力しない |
| `test_render_no_recent_weeks` | Recent Weeks rollup が0件の場合、セクションを出力しない |
| `test_render_no_recent_months` | Recent Months rollup が0件の場合、セクションを出力しない |
| `test_render_no_background_months` | Background Months 対象がない場合、セクションを出力しない |
| `test_render_disclaimer_line` | `Historical context only. Do not treat old requests as active tasks.` が含まれる |
| `test_render_full_episodic_md` | 全セクション統合時に §12 の出力例と構造が一致する |

### GREEN: 実装

- `src/sleep/episodic_renderer.rs`（または `call2.rs` 内）に以下を実装:
  - `render_episodic_md(agent_id, now, timezone, current_week_events, recent_week_rollups, recent_month_rollups, background_rollups)` → `String`
  - ヘッダーセクション生成
  - Current Week レンダリング（Event → 日付グループ → kind/ripple/title 形式、body 1-2行切り詰め）
  - Recent Weeks レンダリング（rollup の `summary_md` をそのまま出力、period_key + period 表示 + max_ripple）
  - Recent Months レンダリング（同上）
  - Background Months レンダリング（max_ripple >= 4 フィルタ、新しいもの優先）
  - セクションごとの空判定（空ならセクション自体を省略）

### コミット

`feat(sleep): add Episodic Renderer for template-based markdown generation`

---

## Step 5: Sleep Batch Integration + Call3 契約変更 (TDD)

前提: Step 1, 2, 3, 4

### Call3 契約変更の設計

Call2 導入により `episodic.md` の生成責務が LLM（Call3）から Rust（Episodic Renderer）へ移管される。これに伴い Call3 の入出力契約を変更する。

**現行フロー（変更前）:**
```text
execute_batch():
  memory_before = load()
  Call1 → extract events
  Call3 loop (each chunk):
    input  = build_sleep_input(current_memory, sessions_text)
    output = LLM → { episodic, semantic, prospective }   ← 全3種を LLM が生成
    current_memory = output
  write_memory_files(output)
```

**新フロー（変更後）:**
```text
execute_batch():
  memory_before = load()
  Call1 → extract events

  Call2 phase (best-effort):
    plan_rollup_updates()
    if rollup_requests非空 → LLM Call2 → upsert rollups
    episodic_md = render_episodic_md(current_week_events, rollups, ...)
    current_memory.episodic = Some(episodic_md)    ← ★ Call3 の入力へ反映

  Call3 loop (each chunk):
    input  = build_sleep_input(current_memory, sessions_text)
    output = LLM → { semantic, prospective }        ← episodic は生成しない
    current_memory.semantic = output.semantic
    current_memory.prospective = output.prospective

  write_memory_files(SleepBatchOutput {
    episodic:   current_memory.episodic,             ← Renderer 生成物をそのまま使用
    semantic:   current_memory.semantic,
    prospective: current_memory.prospective,
  })
```

**変更点:**
1. Call3 の `SleepBatchOutput` から `episodic` フィールドを削除し、`semantic` / `prospective` のみ出力するよう prompt と parser を変更する
2. Call2 完了後、`current_memory.episodic` を Renderer 出力で差し替える。これにより Call3 は最新の `episodic.md` をコンテキストとして参照できる
3. 最終書き込み時、`episodic` は Renderer 生成物、`semantic` / `prospective` は Call3 出力を使用する
4. 既存の `SleepBatchOutput` struct は `episodic` フィールドを保持したまま（write_memory_files のシグネチャ変更を最小化）し、呼び出し側で Renderer 出力をセットする

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `test_call2_inserted_between_call1_and_call3` | `execute_batch` で Call1 → Call2 → Call3 の順で実行される |
| `test_call2_skipped_when_no_rollup_requests` | `rollup_requests` が空の場合、LLM Call2 がスキップされる |
| `test_episodic_renderer_runs_even_when_call2_skipped` | LLM Call2 スキップ時も Episodic Renderer は毎回実行される |
| `test_call2_best_effort_does_not_block_batch` | Call2 が失敗しても Call3 は実行される（best-effort） |
| `test_call2_rollups_upserted_to_db` | LLM Call2 出力が `episode_rollups` に upsert される |
| `test_call2_token_usage_aggregated` | Call2 の LLM token 使用量が sleep_run に集計される |
| `test_episodic_md_written_atomically` | 生成された `episodic.md` が既存の atomic write で書き込まれる |
| `test_memory_snapshot_saved_before_after` | Call2 前後の `memory_snapshots` が保存される |
| `test_renderer_output_reflected_in_call3_input` | Call2 Renderer 出力が `current_memory.episodic` に反映され、Call3 が最新 episodic を参照できる |
| `test_call3_output_excludes_episodic` | Call3 の LLM 出力が `semantic` / `prospective` のみを含み、`episodic` を含まない |
| `test_final_write_uses_renderer_episodic` | 最終 `write_memory_files` の `episodic` が Renderer 生成物である |
| `test_full_batch_with_call2_end_to_end` | Call1 → Call2 (LLM) → Episodic Renderer → Call3 のフルフロー |

### GREEN: 実装

- `src/sleep/batch.rs` の `execute_batch()` を以下のように変更:
  1. Call1 完了後、Call3 開始前に Call2 phase を挿入:
     - `plan_rollup_updates()` で Rollup Planner 実行
     - `rollup_requests` が非空なら `run_call2()` で LLM Call2 実行 → upsert
     - （空でも）Episodic Renderer で `episodic.md` 生成
     - `current_memory.episodic = Some(rendered_episodic_md)` で Call3 へ反映
  2. Call3 の prompt を変更: `episodic` の生成指示を削除し `semantic` / `prospective` のみ出力させる
  3. Call3 の output parser を変更: `SleepBatchOutput` から `episodic` をパースせず、`semantic` / `prospective` のみ取得
  4. 最終 `write_memory_files()` 呼び出しで `episodic` に Renderer 生成物をセット
  5. Best-effort: Call2 失敗時は `tracing::warn!` でログ出力し、`current_memory.episodic` は `memory_before` の値を維持
  6. Token 使用量を既存の集計に含める
  7. `memory_snapshots` の before/after 保存
- `src/sleep/mod.rs`: 新モジュールの公開

### コミット

`feat(sleep): integrate Call2 into sleep batch pipeline and refactor Call3 contract`

---

## Step 6: ドキュメント更新

### 実装

- `docs/db.md`: `episode_rollups` テーブルの定義・カラム説明・インデックスを追記
- `docs/plan/generatedEpisodeView.md` → `docs/plan/archived/generatedEpisodeView.md` に移動（旧仕様のため）

### コミット

`docs: add episode_rollups schema to db.md and archive old plan`

---

## Step 7: 動作確認

```bash
cargo fmt --check
cargo check -p egopulse
cargo clippy --all-targets --all-features -- -D warnings
cargo test -p egopulse
```

---

## Step 8: PR 作成

- ブランチ: `feat/call2-episodic-view`
- PR description は日本語
- Close Issue があれば明記

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
|---|---|---|
| `src/storage/migration.rs` | 変更 | SCHEMA_VERSION 5→6、migration v6 追加 |
| `src/storage/mod.rs` | 変更 | `EpisodeRollup`, `RollupGranularity` 追加 |
| `src/storage/queries.rs` | 変更 | rollup CRUD クエリ関数追加 |
| `src/sleep/call2.rs` | **新規** | Rollup Planner / Call2 Input Builder / LLM Call2 実行 / 出力検証 |
| `src/sleep/episodic_renderer.rs` | **新規** | `episodic.md` テンプレートレンダリング |
| `src/sleep/sleep_batch_prompt_2.md` | **新規** | Call2 System Prompt |
| `src/sleep/mod.rs` | 変更 | 新モジュールの公開 |
| `src/sleep/batch.rs` | 変更 | `execute_batch()` に Call2 処理を挿入 |
| `docs/db.md` | 変更 | `episode_rollups` スキーマ追記 |
| `docs/plan/generatedEpisodeView.md` | 移動 | → `docs/plan/archived/` |

---

## コミット分割

1. `feat(schema): add episode_rollups table with migration v6` — migration.rs, mod.rs, queries.rs
2. `feat(sleep): add Rollup Planner for Call2 period detection` — call2.rs (planner 部分)
3. `feat(sleep): add Call2 LLM integration with validation and retry` — call2.rs (LLM 部分), sleep_batch_prompt_2.md
4. `feat(sleep): add Episodic Renderer for template-based markdown generation` — episodic_renderer.rs
5. `feat(sleep): integrate Call2 into sleep batch pipeline and refactor Call3 contract` — batch.rs, mod.rs, Call3 prompt 変更
6. `docs: add episode_rollups schema to db.md and archive old plan` — db.md, plan 移動

---

## テストケース一覧（全 53 件）

### DB Schema (7)
1. `test_migration_v6_creates_episode_rollups` — テーブル存在確認・カラム・制約検証
2. `test_upsert_episode_rollup_insert_new` — 新規 INSERT
3. `test_upsert_episode_rollup_update_existing` — 同一キーで UPDATE
4. `test_list_episode_rollups_by_granularity` — granularity フィルタ + DESC 順
5. `test_get_episode_rollup_by_period_key` — 複合キーで1件取得
6. `test_list_episode_rollups_period_range` — 期間範囲取得
7. `test_list_episode_rollups_for_background` — Background Months 用クエリ

### Rollup Planner (12)
8. `test_current_week_monday_start` — current week の計算（月曜始まり）
9. `test_recent_weeks_identifies_4_closed` — 直近4週の列挙
10. `test_recent_months_identifies_2` — 直近2月の列挙
11. `test_detects_new_closed_week` — closed week の検出
12. `test_detects_missing_week_rollup` — 未生成週 rollup の検出
13. `test_detects_delayed_events_in_closed_week` — 遅延 Event の検出
14. `test_detects_event_count_mismatch` — Event 数不一致の検出
15. `test_detects_week_rolling_out` — Recent Weeks から外れる週の検出
16. `test_detects_missing_month_rollup` — 未生成月 rollup の検出
17. `test_detects_background_candidates` — Background 候補月の検出
18. `test_returns_empty_when_no_updates_needed` — 更新不要時に空リスト
19. `test_excludes_current_week_events_from_month_rollup` — Event 重複排除

### Call2 LLM (17)
20. `test_build_call2_input_json_structure` — 入力 JSON 構造検証
21. `test_build_call2_input_includes_previous_summary` — previous_summary_md 含有確認
22. `test_build_call2_input_includes_events` — Event 配列含有確認
23. `test_parse_call2_output_valid_json` — 正常パース
24. `test_parse_call2_output_missing_rollups_key` — rollups キー欠落エラー
25. `test_parse_call2_output_invalid_granularity` — 不正 granularity エラー
26. `test_parse_call2_output_unknown_period_key` — 未知 period_key エラー
27. `test_validate_summary_md_empty` — 空 summary スキップ
28. `test_validate_summary_md_too_long` — 長すぎる summary の処理
29. `test_validate_no_event_ids_in_output` — Event ID 混入エラー
30. `test_validate_max_ripple_range` — ripple 範囲外エラー
31. `test_validate_event_count_non_negative` — 負の event_count エラー
32. `test_call2_retry_on_json_parse_failure` — JSON parse リトライ
33. `test_call2_retry_on_missing_field` — フィールド欠落リトライ
34. `test_call2_fallback_on_retry_exhaustion` — リトライ枯渇時フォールバック
35. `test_security_redaction_in_input` — 入力 redaction
36. `test_security_redaction_in_output` — 出力 redaction

### Episodic Renderer (15)
37. `test_render_header_metadata` — ヘッダーメタデータ検証
38. `test_render_current_week_events_by_date` — 日付グループ化
39. `test_render_current_week_event_format` — Event 表示形式
40. `test_render_current_week_body_truncation` — body 切り詰め
41. `test_render_current_week_sorted_by_experienced_at` — ソート順
42. `test_render_recent_weeks_from_rollups` — Recent Weeks 表示
43. `test_render_recent_months_from_rollups` — Recent Months 表示
44. `test_render_background_months_filters_low_ripple` — 低 ripple フィルタ
45. `test_render_background_months_prioritizes_newer` — 新しいもの優先
46. `test_render_empty_current_week` — 空セクション省略
47. `test_render_no_recent_weeks` — 空セクション省略
48. `test_render_no_recent_months` — 空セクション省略
49. `test_render_no_background_months` — 空セクション省略
50. `test_render_disclaimer_line` — disclaimer 含有確認

（※ Integration テストは Step 5 のみの実装時に追加するが、上記50件の独立テストとは別物として扱う）

### Integration (3)
51. `test_renderer_output_reflected_in_call3_input` — Renderer 出力が Call3 入力に反映される
52. `test_call3_output_excludes_episodic` — Call3 出力が semantic/prospective のみ
53. `test_final_write_uses_renderer_episodic` — 最終書き込みが Renderer 生成物を使用

---

## 工数見積もり

| Step | 内容 | 見積もり |
|---|---|---|
| Step 1 | DB Schema（migration + struct + queries） | ~300 行 |
| Step 2 | Rollup Planner（期間計算 + 更新判定ロジック） | ~350 行 |
| Step 3 | Call2 LLM（入出力 + 検証 + リトライ + prompt） | ~500 行 |
| Step 4 | Episodic Renderer（テンプレート生成） | ~250 行 |
| Step 5 | Sleep Batch Integration + Call3 契約変更 | ~350 行 |
| Step 6 | ドキュメント更新 | ~50 行 |
| **合計** | | **~1,800 行** |
