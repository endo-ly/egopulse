# Plan: Sleep Batch Audit WebUI

Sleep batch（記憶整理処理）の実行履歴とメモリbefore/after差分をWebUIで監査できる画面を追加する。Backend API（Rust/Axum）3エンドポイント + Frontend（React/Tailwind）のMaster-Detail UI。

> **Note**: 以下の具体的なコード例・API 設計・構成（How）はあくまで参考である。実装時によりよい設計方針があれば積極的に採用すること。

## 設計方針

- **既存パターン踏襲**: Web APIハンドラは `sessions.rs` のパターン（`State<WebState>` + `call_blocking` + `serde_json::json!`）に従う
- **DBクエリ再利用**: 既存の `get_sleep_run`, `list_sleep_runs`, `get_snapshots_for_run` をそのまま使い、不足分（agent一覧、agent_id未指定時の全件取得）のみ追加する
- **フロントエンド最小依存**: 外部ライブラリ（diffライブラリ、ルーター等）を追加せず、既存のReact + Tailwind + Vite構成のまま実装する
- **TDD**: 各StepでRED→GREEN→コミットを完結させる

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 実装元 |
|---|---|
| Backend: 新規DBクエリ（`list_distinct_agent_ids`, `list_all_sleep_runs`） | `src/storage/queries.rs` |
| Backend: Sleep batch APIハンドラ（3エンドポイント） | `src/channels/web/sleep.rs`（新規） |
| Backend: ルーター登録 | `src/channels/web/mod.rs` |
| Frontend: diff算出ユーティリティ | `web/src/diff.ts`（新規） |
| Frontend: useSleepBatch hook | `web/src/hooks/useSleepBatch.ts`（新規） |
| Frontend: SleepBatchPanel, RunList, RunDetail, DiffViewer | `web/src/components/*.tsx`（新規） |
| Frontend: App, Sidebar, types, api, CSS | 既存ファイル変更 |

---

## Step 0: Worktree 作成

`git worktree add` で `feature/sleep-batch-audit-webui` ブランチを作成。

---

## Step 1: DB query拡張 (TDD)

前提: なし

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `list_distinct_agent_ids_returns_sorted_agents` | 複数agentのsleep_runsを挿入 → `list_distinct_agent_ids()` がソート済みで返す |
| `list_distinct_agent_ids_empty_when_no_runs` | runsなし → 空Vec |
| `list_all_sleep_runs_returns_all_agents` | 複数agentのrunsを挿入 → agent_id指定なしで全件取得、started_at降順 |
| `list_all_sleep_runs_respects_limit` | 5件挿入 → limit=3で3件 |
| `list_all_sleep_runs_empty` | runsなし → 空Vec |

### GREEN: 実装

`src/storage/queries.rs` に2関数を追加:

1. `list_distinct_agent_ids(&self) -> Result<Vec<String>, StorageError>` — `SELECT DISTINCT agent_id FROM sleep_runs ORDER BY agent_id`
2. `list_all_sleep_runs(&self, limit: i64) -> Result<Vec<SleepRun>, StorageError>` — `WHERE` 句なし、`ORDER BY started_at DESC, rowid DESC LIMIT ?1`

既存の `list_sleep_runs` は `WHERE agent_id = ?1` が必須のため、agent未指定クエリは別関数とする。

### コミット

`feat(storage): add list_distinct_agent_ids and list_all_sleep_runs queries`

---

## Step 2: Backend sleep APIハンドラ (TDD)

前提: Step 1

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `api_agents_returns_distinct_agent_ids` | DBにagent挿入 → `GET /api/agents` が `{ ok: true, agents: [...] }` を返す |
| `api_agents_returns_empty_array` | DB空 → `{ ok: true, agents: [] }` |
| `api_sleep_runs_returns_runs` | DBにruns挿入 → `GET /api/sleep/runs` がruns配列を返す、session_count含む |
| `api_sleep_runs_filters_by_agent_id` | `?agent_id=lyre` → 該当agentのみ |
| `api_sleep_runs_respects_limit` | `?limit=1` → 1件 |
| `api_sleep_runs_default_limit` | limit未指定 → デフォルト20件 |
| `api_sleep_runs_empty` | DB空 → 空配列 |
| `api_sleep_run_detail_returns_run_and_snapshots` | run + snapshots挿入 → `GET /api/sleep/runs/:id` がrun + snapshots返す |
| `api_sleep_run_detail_returns_404_for_missing` | 存在しないID → 404 `{ ok: false, error: "not_found" }` |
| `api_sleep_run_detail_snapshots_file_field` | snapshotの `file` フィールドが "episodic" 等の文字列 |

テスト方針: Axumのハンドラを直接呼び出す単体テスト。`WebState` 構築は `src/sleep_batch.rs` テスト内の `build_test_state()` パターンを参考にする（`Database` は `tempfile::tempdir` + `Database::new` で実DBを使用、`AppState` は `test_util::test_config` で構築）。ハンドラの戻り値は `Json<serde_json::Value>` をデシリアライズして検証。Axum path param は `{run_id}` 構文を使用。

### GREEN: 実装

`src/channels/web/sleep.rs`（新規）に3ハンドラを実装:

1. `list_agents(State) -> Json<Value>` — `list_distinct_agent_ids()` 呼び出し
2. `list_sleep_runs(State, Query) -> Json<Value>` — `agent_id` 有無で `list_sleep_runs` / `list_all_sleep_runs` 切替、`session_count` を `source_chats_json` のlengthから計算
3. `get_sleep_run_detail(State, Path) -> Result<Json<Value>, (StatusCode, Json<Value>)>` — `get_sleep_run` + `get_snapshots_for_run`、404ハンドリング

`src/channels/web/mod.rs` にルートを登録:
```rust
.route("/api/agents", get(sleep::list_agents))
.route("/api/sleep/runs", get(sleep::list_sleep_runs))
.route("/api/sleep/runs/{run_id}", get(sleep::get_sleep_run_detail))
```

### コミット

`feat(web): add sleep batch audit API endpoints`

---

## Step 3: Frontend diff算出ユーティリティ (TDD)

前提: なし（Frontend内で独立）

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `diff_lines_identical_content` | 同じ文字列 → 全行 unchanged |
| `diff_lines_added_line` | before=[A] after=[A,B] → B が add |
| `diff_lines_removed_line` | before=[A,B] after=[A] → B が remove |
| `diff_lines_changed_line` | before=[A] after=[A'] → A' が add、A が remove |
| `diff_lines_empty_before` | before=[] after=[A,B] → 全行 add |
| `diff_lines_empty_after` | before=[A,B] after=[] → 全行 remove |
| `diff_lines_both_empty` | before=[] after=[] → 空配列 |
| `diff_lines_multiline_mixed` | 複数行の追加・削除・維持が混在 |

### GREEN: 実装

`web/src/diff.ts`（新規）:

```typescript
export type DiffLine =
  | { type: 'add'; content: string }
  | { type: 'remove'; content: string }
  | { type: 'unchanged'; before: string; after: string };

export function computeLineDiff(before: string, after: string): DiffLine[];
```

LCS（Longest Common Subsequence）ベースの行レベルdiff。`before.split('\n')` / `after.split('\n')` → LCS → DiffLine[] を生成。

### コミット

`feat(web): add line-level diff computation utility`

---

## Step 4: Frontend types, api, hook (TDD)

前提: Step 3

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `format_tokens_below_1000` | 999 → '999' |
| `format_tokens_above_1000` | 1247 → '1.2k' |
| `format_tokens_exact_1000` | 1000 → '1.0k' |
| `format_tokens_zero` | 0 → '0' |
| `api_sleep_functions_call_correct_paths` | `fetchSleepRuns`, `fetchRunDetail`, `fetchAgents` のパス検証 |

### GREEN: 実装

**`web/src/types.ts`** — 型追加:
```typescript
export type SleepRun = {
  id: string; agent_id: string; status: string;
  trigger_type: string; started_at: string; finished_at: string | null;
  input_tokens: number; output_tokens: number; total_tokens: number;
  error_message: string | null; session_count: number;
};

export type MemorySnapshot = {
  file: string; content_before: string; content_after: string;
};
```

**`web/src/api.ts`** — API関数追加:
- `fetchAgents(authToken)` → `GET /api/agents`
- `fetchSleepRuns(authToken, agentId?, limit?)` → `GET /api/sleep/runs`
- `fetchRunDetail(authToken, runId)` → `GET /api/sleep/runs/:run_id`
- `formatTokens(n: number): string` — >999 なら k 表記

**`web/src/hooks/useSleepBatch.ts`**（新規） — データフェッチ + state管理hook

### コミット

`feat(web): add sleep batch API client and types`

---

## Step 5: Frontend UIコンポーネント (TDD)

前提: Step 4

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `DiffViewer_renders_split_diff` | splitモードで2カラム表示 |
| `DiffViewer_renders_unified_diff` | unifiedモードで1カラム表示 |
| `DiffViewer_shows_no_changes` | before==after → 'No changes' |
| `DiffViewer_toggle_switches_mode` | トグルクリックで split↔unified 切替 |
| `RunList_renders_run_cards` | runs配列からカード一覧表示 |
| `RunList_shows_empty_state` | runs=[] → 'No sleep batch runs yet' |
| `RunList_shows_status_icons` | success=✅, failed=❌, skipped=⏭, running=🔄 |
| `RunDetail_renders_meta_info` | status/時刻/トークン等の表示 |
| `RunDetail_shows_error_for_failed` | failed run → error_message赤字表示 |
| `RunDetail_collapsible_sections` | ファイル名クリックで折りたたみ |

### GREEN: 実装

新規コンポーネント:

1. **`web/src/components/DiffViewer.tsx`** — split/unified切替 + 行ハイライト
2. **`web/src/components/RunList.tsx`** — agent選択 + runカード一覧
3. **`web/src/components/RunDetail.tsx`** — メタ情報 + 3ファイルのDiffViewer + 折りたたみ
4. **`web/src/components/SleepBatchPanel.tsx`** — RunList ↔ RunDetail のMaster-Detail切替

**`web/src/app.css`** — スタイル追加:
- `.run-card`, `.run-card-meta`, `.run-status-icon`
- `.diff-container`, `.diff-split`, `.diff-unified`
- `.diff-line-add`, `.diff-line-remove`, `.diff-line-unchanged`
- `.diff-file-header`（折りたたみ対応）
- レスポンシブ: 768px未満でデフォルトunified

### コミット

`feat(web): add Sleep Batch audit UI components`

---

## Step 6: Sidebar + App統合 (TDD)

前提: Step 5

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `Sidebar_renders_sleep_batch_button` | Sleep Batchボタンの存在確認 |
| `Sidebar_sleep_batch_button_triggers_view_change` | クリックで onOpenSleepBatch が呼ばれる |
| `App_switches_to_sleep_batch_view` | MainView state が sleep-batch → SleepBatchPanel表示 |
| `App_returns_to_chat_on_session_select` | セッション選択 → ChatPanelに戻る |

### GREEN: 実装

**`web/src/components/Sidebar.tsx`** — Sleep Batchボタン追加（`onOpenSleepBatch` prop追加）

**`web/src/components/App.tsx`** — MainView state追加:
```typescript
type MainView =
  | { type: 'chat' }
  | { type: 'sleep-batch' };
```
- Sidebarに `onOpenSleepBatch` を渡す
- `mainView.type === 'sleep-batch'` のとき `SleepBatchPanel` をレンダリング
- セッション選択で `setMainView({ type: 'chat' })` に戻す

### コミット

`feat(web): integrate Sleep Batch panel into App shell`

---

## Step 7: 動作確認

```bash
# Rust
cargo fmt --check -p egopulse
cargo check -p egopulse
cargo clippy --all-targets --all-features -- -D warnings
cargo test -p egopulse

# WebUI
npm install --prefix web
npm run build --prefix web
npm test --prefix web
```

---

## Step 8: PR 作成

ブランチ `feature/sleep-batch-audit-webui` から main へPR作成。description は日本語。設計仕様書 `docs/webui/sleep-batch-audit-webui-design.md` へのリンクを含む。

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
|---|---|---|
| `src/storage/queries.rs` | 変更 | `list_distinct_agent_ids`, `list_all_sleep_runs` 追加 |
| `src/channels/web/sleep.rs` | **新規** | Sleep batch APIハンドラ3つ |
| `src/channels/web/mod.rs` | 変更 | ルーター登録 + `mod sleep` 追加 |
| `web/src/diff.ts` | **新規** | 行レベルdiff算出 |
| `web/src/hooks/useSleepBatch.ts` | **新規** | データフェッチhook |
| `web/src/components/SleepBatchPanel.tsx` | **新規** | Master-Detailパネル |
| `web/src/components/RunList.tsx` | **新規** | Run一覧 |
| `web/src/components/RunDetail.tsx` | **新規** | Run詳細 + diff |
| `web/src/components/DiffViewer.tsx` | **新規** | Split/Unified diffレンダラ |
| `web/src/components/App.tsx` | 変更 | MainView state + SleepBatchPanel統合 |
| `web/src/components/Sidebar.tsx` | 変更 | Sleep Batchボタン追加 |
| `web/src/types.ts` | 変更 | SleepRun, MemorySnapshot型追加 |
| `web/src/api.ts` | 変更 | API関数 + formatTokens追加 |
| `web/src/app.css` | 変更 | run-card, diff関連スタイル追加 |

---

## コミット分割

1. `feat(storage): add list_distinct_agent_ids and list_all_sleep_runs queries`
2. `feat(web): add sleep batch audit API endpoints`
3. `feat(web): add line-level diff computation utility`
4. `feat(web): add sleep batch API client and types`
5. `feat(web): add Sleep Batch audit UI components`
6. `feat(web): integrate Sleep Batch panel into App shell`

---

## テストケース一覧（全 42 件）

> **仕様書**: `docs/webui/sleep-batch-audit-webui-design.md`

### storage queries (5)
1. `list_distinct_agent_ids_returns_sorted_agents` — 複数agentの昇順ソート
2. `list_distinct_agent_ids_empty_when_no_runs` — runsなし時の空Vec
3. `list_all_sleep_runs_returns_all_agents` — agent横断で全件取得
4. `list_all_sleep_runs_respects_limit` — limit制限
5. `list_all_sleep_runs_empty` — 空DB時の空Vec

### web sleep API handlers (10)
6. `api_agents_returns_distinct_agent_ids` — agent一覧取得
7. `api_agents_returns_empty_array` — 空DB時の空配列
8. `api_sleep_runs_returns_runs` — run一覧取得 + session_count
9. `api_sleep_runs_filters_by_agent_id` — agent_id絞り込み
10. `api_sleep_runs_respects_limit` — limit制限
11. `api_sleep_runs_default_limit` — デフォルト20件
12. `api_sleep_runs_empty` — 空配列
13. `api_sleep_run_detail_returns_run_and_snapshots` — run詳細 + snapshots
14. `api_sleep_run_detail_returns_404_for_missing` — 存在しないID → 404
15. `api_sleep_run_detail_snapshots_file_field` — fileフィールドの文字列表現

### diff utility (8)
16. `diff_lines_identical_content` — 同一内容 → 全行unchanged
17. `diff_lines_added_line` — 追加行検出
18. `diff_lines_removed_line` — 削除行検出
19. `diff_lines_changed_line` — 変更行（remove + add）
20. `diff_lines_empty_before` — 空before → 全行add
21. `diff_lines_empty_after` — 空after → 全行remove
22. `diff_lines_both_empty` — 両方空 → 空配列
23. `diff_lines_multiline_mixed` — 追加・削除・維持の混在

### api helpers (5)
24. `format_tokens_below_1000` — 999 → '999'
25. `format_tokens_above_1000` — 1247 → '1.2k'
26. `format_tokens_exact_1000` — 1000 → '1.0k'
27. `format_tokens_zero` — 0 → '0'
28. `api_sleep_functions_call_correct_paths` — APIパス検証

### UI components (10)
29. `DiffViewer_renders_split_diff` — 2カラムdiff表示
30. `DiffViewer_renders_unified_diff` — 1カラムdiff表示
31. `DiffViewer_shows_no_changes` — 変更なし表示
32. `DiffViewer_toggle_switches_mode` — split↔unified切替
33. `RunList_renders_run_cards` — カード一覧
34. `RunList_shows_empty_state` — 空状態表示
35. `RunList_shows_status_icons` — ステータスアイコン
36. `RunDetail_renders_meta_info` — メタ情報表示
37. `RunDetail_shows_error_for_failed` — エラー表示
38. `RunDetail_collapsible_sections` — 折りたたみ動作

### integration (4)
39. `Sidebar_renders_sleep_batch_button` — ボタン存在確認
40. `Sidebar_sleep_batch_button_triggers_view_change` — クリックイベント
41. `App_switches_to_sleep_batch_view` — ビュー切替
42. `App_returns_to_chat_on_session_select` — チャットに戻る

> ※ テストケース番号はカテゴリ内で連番。合計 5+10+8+5+10+4 = 42件。

---

## 工数見積もり

| Step | 内容 | 見積もり |
|---|---|---|
| Step 0 | Worktree作成 | ~10行 |
| Step 1 | DB query拡張（テスト+実装） | ~120行 |
| Step 2 | Backend APIハンドラ（テスト+実装） | ~280行 |
| Step 3 | diffユーティリティ（テスト+実装） | ~150行 |
| Step 4 | types/api/hook（テスト+実装） | ~180行 |
| Step 5 | UIコンポーネント（テスト+実装） | ~500行 |
| Step 6 | 統合（テスト+実装） | ~120行 |
| **合計** | | **~1,360行** |
