# Plan: イベントテーブル要約の週/月分離

Sleep Batch Call 2（イベント要約ロールアップ生成）を、週要約と月要約の2段階LLMコールに分離する。月要約は週要約をInputとし、先月の月要約を参照して重複を軽減する。

> **Note**: 振る舞い（What）は決して変えてはいけないが、より美しい設計があれば実装方法（HOW）だけは変えてもよい。

## 設計方針

- **週要約と月要約を別LLMコールに分離**: 現状の単一プロンプト・単一コールから、週用（Call 2a）と月用（Call 2b）の2回コールに変更
- **月要約のInputを週要約に限定**: 生Eventテーブルではなく、その月に含まれる週Rollupの `summary_md` 配列をInputとする
- **月トリガーは「月末以降 + 週要約1つ以上」**: 暦月の全ISO週がクローズ済みではなく、その月に1つ以上の週Rollupが存在すれば生成可能とする（初回生成のみ）
- **先月の月要約参照で重複軽減**: 月要約LLMのInputに `previous_month_summary_md` を含め、跨ぎ週の重複をプロンプトレベルで抑制
- **既存の週トリガー（ルール1-4）を維持**: 週要約の生成条件はそのまま。月トリガーはルール5/6/7を廃止し新設
- **月統計値は週Rollupから集約**: `event_count` と `max_ripple` は対象週Rollupsの値から算出し、LLM Outputの統計値は無視する

## TDD 方針

このPlanでは、テストリスト項目をPlan全体で網羅的に書き出し、各TDD Cycleで1項目のみを選ぶ。Redでは選んだ項目に対応する自動テスト1つを書き、Greenではそのテストを通す最小実装に集中する。Refactorでは全テストが通る状態を保ちながら設計を整える。テストは外から見た振る舞いに寄せ、実装構造への過度な依存を避ける。

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 種別 | 既存パターン / 参照元 | 備考 |
| -- | -- | -- | -- |
| `src/sleep/rollup_week_prompt.md` | 新規 | `rollup_prompt.md` 週部分ベース | 週要約専用 |
| `src/sleep/rollup_month_prompt.md` | 新規 | `rollup_prompt.md` 月部分ベース | 月要約専用。週Rollup Input + 先月参照 |
| `src/sleep/rollup.rs` | 変更 | Planner・Input Builder・プロンプト読み込み | 週/月分離の核心 |
| `src/sleep/batch.rs` | 変更 | Call 2実行・DB保存・統計値算出 | 2回LLMコール化 |
| `src/sleep/rollup_prompt.md` | 削除 | 旧統合プロンプト | 分割後は不要 |
| `src/sleep/episodic_renderer.rs` | 影響確認 | DB経由でrollup取得 | 変更なし（想定） |

## テストリスト / 不安リスト

| ID | 観点 | 期待する振る舞い | 優先 | 対応するCycle | 状態 / 今回対象外理由 |
| -- | -- | -- | -- | -- | -- |
| T1 | プロンプト分割 | `build_call2_system_prompt_week` / `_month` がそれぞれ正しい.mdファイルを読み込む | High | Step 1 | 未着手 |
| T2 | 月トリガー条件 | 月末前は生成されず、月末後かつ週Rollup1つ以上ある月は生成される。既存月はスキップ | High | Step 2 | 未着手 |
| T3 | 月Input構築 | 対象月の週Rollupリスト + 先月要約（あれば）が正しくJSONに組み込まれる | High | Step 3 | 未着手 |
| T4 | batch分離実行 | 週Request→月Requestの順で計2回LLMコールされる。片方だけでも動作 | High | Step 4 | 未着手 |
| T5 | 旧トリガー削除 | Week rolling out でも月Requestが生成されない | High | Step 5 | 未着手 |
| T6 | レンダラー維持 | Rollup分離後も `render_episodic_md` が正常に動作する | Medium | Step 6 | 未着手 |
| T7 | 月統計値算出 | 月Rollup保存時の `event_count` / `max_ripple` が週Rollup群から正しく集約される | High | Step 3 | 未着手 |

---

## Step 0: Worktree 作成

- ブランチ名: `feat/split-week-month-rollups`
- 作成コマンド:
  - `git worktree add ../egopulse-feat-split-rollups feat/split-week-month-rollups`
- 注意: `worktree-create` skill を使用

---

## Step 1: プロンプト分割 TDD Cycle - 週/月プロンプトファイル作成

### この Step の目的

現状の `rollup_prompt.md` を週用・月用に分離し、`rollup.rs` のプロンプト読み込み関数が新ファイルを参照するように変更する。

### 今回選ぶ項目

- 対象: `T1`
- 選ぶ理由: ファイルがなければビルドも進まない。最も基礎的で他のStepに依存しない。
- この時点では扱わないこと: プロンプト内容の品質評価（人間レビューで実施）。月トリガーやInput構築。

### RED: 失敗する自動テストを書く

- 追加するテスト名: `test_system_prompt_reads_split_templates`
- Given: エージェントID `"test-agent"`
- When: `build_call2_system_prompt_week("test-agent")` と `build_call2_system_prompt_month("test-agent")` を呼び出す
- Then: 両方とも `"あなたは test-agent の海馬です"` で始まり、週用には週要約方針が、月用には月要約方針が含まれる
- 失敗理由の想定: 関数 `build_call2_system_prompt_week` / `_month` が未実装、または `.md` ファイルが存在しないためPanic

### GREEN: 最小実装

1. `rollup_prompt.md` の週部分（line 92-116）を抽出して `rollup_week_prompt.md` を作成
2. 月要約部分をベースに、以下を追加して `rollup_month_prompt.md` を作成：
   - Inputスキーマ定義（`week_rollups[]` + `previous_month_summary_md`）
   - 重複排除指示（先月の内容を繰り返さない）
   - 抽象化指示（週の流れ・トレンドを俯瞰）
3. `rollup.rs` に `build_call2_system_prompt_week` / `build_call2_system_prompt_month` を新設

### REFACTOR: 設計の整理

- 重複: `rollup_prompt.md` と新ファイルの内容重複は一旦許容。旧ファイルは最終的に削除。
- 命名: `rollup_week_prompt.md` / `rollup_month_prompt.md` は意思が明確でOK

### テストリスト更新

- 完了: `T1`
- 追加: なし
- 次候補: `T2`

### コミット

`feat: split rollup prompt into week and month templates`

---

## Step 2: 月トリガー TDD Cycle - `complete_months_recent` ヘルパーと月判定

### この Step の目的

`plan_rollup_updates` を週用と月用に分離し、月トリガーを「月末以降 + 週要約1つ以上」に実装する。

### 今回選ぶ項目

- 対象: `T2`
- 選ぶ理由: 月トリガーは分離の核心。月末判定の正確さが後続の全Stepに影響する。
- この時点では扱わないこと: Input構築（T3）、batch分離（T4）、旧トリガー削除（T5）。

### RED: 失敗する自動テストを書く

- 追加するテスト名: `test_month_trigger_conditions`
- Given: 現在日時 `2026-07-15`（7月途中）、既存週/月ロールアップ情報
- When: `plan_month_rollup_updates(...)` を実行
- Then: 7月（月末前）のRequestは生成されない。6月（月末後かつ週あり）のRequestは生成される。既存月がある場合はスキップされる。
- 失敗理由の想定: `plan_month_rollup_updates` 関数が未実装。または `recent_months_from_weeks` を誤用して月末判定がずれる。

### GREEN: 最小実装

1. `complete_months_recent` ヘルパーを新設：
   `recent_months_from_weeks` は最古のrecent weekで月をcapするため**月末判定には使えない**。現在の月の前月から `count` ヶ月分の完全な暦月 `MonthPeriod` を返す。

2. `plan_rollup_updates` を週用と月用に分解：
   - `plan_week_rollup_updates(agent_id, now, tz, input)`: 現行ルール1-4を抽出
   - `plan_month_rollup_updates(agent_id, now, tz, existing_month_rollups, existing_week_rollups)`: 新トリガーのみ

3. `plan_month_rollup_updates` の実装：
   - `complete_months_recent(now, 2, tz)` で対象月を取得
   - `now >= mp.period_end_exclusive` で月末判定
   - `iso_weeks_in_month` で月に含まれるISO週を列挙し、1つ以上の週RollupがDBに存在するか確認
   - 既存月Rollupがある月はスキップ

### REFACTOR: 設計の整理

- 責務: `plan_rollup_updates` の肥大化を解消。月判定の複雑さを月関数に閉じ込め。
- 命名: `plan_month_rollup_updates` / `complete_months_recent` は役割が明確。

### テストリスト更新

- 完了: `T2`
- 追加: なし
- 次候補: `T3`

### コミット

`feat: separate month rollup trigger with end-of-month condition`

---

## Step 3: 月Input構築 TDD Cycle - `build_call2_input_month` と統計値算出

### この Step の目的

月要約のLLM Inputを構築し、週Rollup配列・先月要約の組み立てと、月用統計値の算出経路を確立する。

### 今回選ぶ項目

- 対象: `T3`
- 選ぶ理由: Input Builderは月LLMに渡す素材の正確さを担保する。統計値算出（T7）は保存時の整合性に必須。
- この時点では扱わないこと: batch.rsでの実際のコール（T4）。

### RED: 失敗する自動テストを書く

- 追加するテスト名: `test_build_month_input_with_week_rollups_and_stats`
- Given: 2026年7月の月RollupRequest + 対象週Rollups（W27: max_ripple=3, event_count=5 / W28: max_ripple=4, event_count=8）+ 先月要約あり
- When: `build_call2_input_month(...)` と `compute_month_rollup_stats(...)` を実行
- Then: 出力JSONに `week_rollups` 配列が含まれ `previous_month_summary_md` に先月要約が入る。統計値は `max_ripple=4`, `event_count=13`。
- 失敗理由の想定: `build_call2_input_month` / `compute_month_rollup_stats` が未実装。`Call2RollupRequest` に月用フィールドがない。

### GREEN: 最小実装

1. `Call2RollupRequest` 構造体を拡張（月用フィールド追加）：
   - `week_rollups: Option<Vec<Call2WeekRollupSummary>>`
   - `previous_month_summary_md: Option<String>`

2. `build_call2_input_month(rollup_requests, week_rollups_map, previous_month_map)` を新設

3. `compute_month_rollup_stats(week_rollups: &[&ExistingRollupInfo]) -> (i64, i64)` を新設：
   対象週Rollup群から `max_ripple`（最大）と `event_count`（合計）を集約

### REFACTOR: 設計の整理

- 重複: `Call2RollupRequest` が週/月共用なのがやや不格好。別structに分けるのは次のRefactor機会で検討。
- 責務: Input組み立ては `rollup.rs` で、DBアクセスは `batch.rs` で行う境界を維持。

### テストリスト更新

- 完了: `T3`, `T7`
- 追加: なし
- 次候補: `T4`

### コミット

`feat: build month rollup input with weekly rollup summaries, previous month reference, and aggregated stats`

---

## Step 4: batch分離 TDD Cycle - Call 2a/2b の2連コール

### この Step の目的

`execute_batch` の Call 2 部分を、週要約（Call 2a）と月要約（Call 2b）の2回LLMコールに変更する。

### 今回選ぶ項目

- 対象: `T4`
- 選ぶ理由: 分離のエンドツーエンド。週→月の順序が正しく動作することが全設計の成否を分ける。
- この時点では扱わないこと: 旧トリガー削除（T5）は別Stepで対応。

### RED: 失敗する自動テストを書く

- 追加するテスト名: `test_batch_executes_week_then_month_calls`
- Given: SleepBatch実行、週Request1個 + 月Request1個がPlannerで生成される
- When: `execute_batch` を実行
- Then: Mock LLM Providerに対して2回 `send_message` が呼ばれる。1回目は週用プロンプト内容、2回目は月用プロンプト内容。
- 失敗理由の想定: `execute_batch` が現在1回の `send_message` のみ。分離前のコードのため月コールが存在しない。

### GREEN: 最小実装

1. `execute_batch` の Call 2 ブロックをリファクタ：
   - `rollup_requests` を `granularity` で partition
   - 週Requestがある場合: 週用Input構築 → 週用プロンプト → LLMコール → パース → DB upsert（生Eventから算出）
   - 月Requestがある場合: 月用Input構築 → 月用プロンプト → LLMコール → パース → DB upsert（`compute_month_rollup_stats` で週Rollupから算出）

2. `batch.rs` で月Requestに対し先月の key を計算し、既存 `db.get_episode_rollup` で取得してInputに挿入

3. 月Rollup保存時の統計値算出を `compute_month_rollup_stats` に切り替え

### REFACTOR: 設計の整理

- 重複: 週コールと月コールで共通部分（LLMコールラップ、リトライ、パース）が多い。共通関数の抽出は次のRefactor機会で検討。
- 責務: `batch.rs` の肥大化を防ぐため、コール実行部分の切り出しを検討。

### テストリスト更新

- 完了: `T4`
- 追加: なし
- 次候補: `T5`

### コミット

`refactor: split Call 2 into week and month rollup LLM calls`

---

## Step 5: 旧トリガー削除 TDD Cycle - ルール5/6とBackground candidateの削除

### この Step の目的

`plan_rollup_updates` 内のルール5（Week rolling out）とルール6（Missing month）を削除し、`batch.rs` 内の Background candidate ロジックも削除する。

### 今回選ぶ項目

- 対象: `T5`
- 選ぶ理由: 旧トリガーが残っていると、新月トリガーと競合して誤動作する。クリーンアップが必須。
- この時点では扱わないこと: エピソードレンダラー確認（T6）。

### RED: 失敗する自動テストを書く

- 追加するテスト名: `test_no_month_request_on_week_rolling_out`
- Given: W-5 が直近4週ウィンドウから出る状態
- When: `plan_week_rollup_updates` と `plan_month_rollup_updates` を実行
- Then: 週Requestはルール1-4で通常生成される。月Requestは `plan_month_rollup_updates` のみで判定され、W-5 rolling_outでは生成されない。
- 失敗理由の想定: 旧 `plan_rollup_updates` にルール5が残っているため、月Requestが生成される。

### GREEN: 最小実装

1. `rollup.rs` の旧 `plan_rollup_updates` からルール5/6相当のコードを削除
   - `plan_week_rollup_updates` はルール1-4のみ
   - `plan_month_rollup_updates` はStep 2で実装済みの新トリガーのみ
2. `batch.rs` の Background candidate ブロック（line 535-574）を削除
3. 旧統合 `plan_rollup_updates` を撤廃し、週/月を直接呼び出すように変更

### REFACTOR: 設計の整理

- 重複: `make_month_request` は `plan_month_rollup_updates` でのみ使用される。OK。
- 責務: Plannerから月トリガーが完全に排除され、新月関数だけが責任を持つ。

### テストリスト更新

- 完了: `T5`
- 追加: なし
- 次候補: `T6`

### コミット

`refactor: remove old month trigger rules 5 and 6 and background candidate logic`

---

## Step 6: レンダラー確認 TDD Cycle - エピソードレンダラー影響確認

### この Step の目的

`episodic_renderer.rs` が月要約の取得・表示に影響を受けないことを確認する。

### 今回選ぶ項目

- 対象: `T6`
- 選ぶ理由: エピソードレンダラーは外部インターフェースに近い。影響があれば回帰バグ。
- この時点では扱わないこと: なし（最終確認Step）。

### RED: 失敗する自動テストを書く

- 追加するテスト名: `test_episodic_renderer_after_split`
- Given: 週Rollup2個 + 月Rollup1個がDBに存在
- When: `render_episodic_md` を実行
- Then: 正常にMarkdownが生成される。月要約セクションに1つの月Rollupが含まれる。
- 失敗理由の想定: エピソードレンダラーはRollupのInput構造を知らないため影響なしのはずだが、念のため確認。

### GREEN: 最小実装

- `episodic_renderer.rs` はDB経由で `list_episode_rollups` を呼ぶため、Call 2の分離そのものに影響を受けない
- `recent_month_rollups` は `db.list_episode_rollups(..., RollupGranularity::Month, 2)` で取得。これは既存メソッドのまま動作する
- 変更が不要なことを確認し、テストのみ追加

### REFACTOR: 設計の整理

- 変更なし

### テストリスト更新

- 完了: `T6`
- 追加: なし
- 次候補: なし

### コミット

`check: verify episodic renderer unaffected by rollup split`

---

## Step 7: 動作確認

- 全テスト通過コマンド:
  ```bash
  cargo test sleep::rollup::
  cargo test sleep::batch::
  cargo test --lib
  ```
- Lint / フォーマット / 型チェック:
  ```bash
  cargo fmt --check
  cargo check
  cargo clippy --all-targets --all-features -- -D warnings
  ```
- 失敗時に戻る Step: 該当Stepへ

---

## Step 8: PR 作成

- PR タイトル: `feat: イベントテーブル要約の週/月分離`
- PR description:
  - 概要: Sleep Batch Call 2（イベント要約ロールアップ）を、週要約と月要約の2段階LLMコールに分離する。月要約は週要約をInputとし、先月の月要約を参照して重複を軽減する。
  - テスト: Plannerの週/月分離テスト、月トリガー条件テスト、Input Builderテスト、batch.rs統合テスト
  - Close #<issue-number>（該当する場合）

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
| ---- | ---- | -- |
| `src/sleep/rollup_week_prompt.md` | **新規** | 週要約専用プロンプト |
| `src/sleep/rollup_month_prompt.md` | **新規** | 月要約専用プロンプト |
| `src/sleep/rollup.rs` | 変更 | Plannerの週/月分離、月トリガー新設、`complete_months_recent` 新設、Input Builder分離、統計値算出関数新設 |
| `src/sleep/batch.rs` | 変更 | Call 2 → Call 2a + Call 2b に分離。月統計値は週Rollupから集約 |
| `src/sleep/rollup_prompt.md` | 削除 | 旧統合プロンプト |
| `src/sleep/episodic_renderer.rs` | 影響確認のみ | 変更なし（想定） |

---

## コミット分割

1. `feat: split rollup prompt into week and month templates`
   - `src/sleep/rollup_week_prompt.md`（新規）
   - `src/sleep/rollup_month_prompt.md`（新規）
   - `src/sleep/rollup.rs`（`build_call2_system_prompt_week` / `_month` 新設）

2. `feat: separate month rollup trigger with end-of-month condition`
   - `src/sleep/rollup.rs`（`plan_week_rollup_updates`, `plan_month_rollup_updates`, `complete_months_recent` 新設）
   - テスト追加

3. `feat: build month rollup input with weekly rollup summaries, previous month reference, and aggregated stats`
   - `src/sleep/rollup.rs`（`Call2RollupRequest` 拡張、`build_call2_input_month`、`compute_month_rollup_stats` 新設）
   - テスト追加

4. `refactor: split Call 2 into week and month rollup LLM calls`
   - `src/sleep/batch.rs`（Call 2a/2b 分離、月統計値の週Rollup集約）
   - テスト追加

5. `refactor: remove old month trigger rules 5 and 6 and background candidate logic`
   - `src/sleep/rollup.rs`（ルール5/6削除）
   - `src/sleep/batch.rs`（Background candidate 削除）

6. `check: verify episodic renderer unaffected by rollup split`
   - エピソードレンダラー確認・テスト

---

## 自動テスト一覧（全 6 件）

### `src/sleep/rollup.rs`（全 4 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T1 | `test_system_prompt_reads_split_templates` | Step 1 | `cargo test test_system_prompt_reads_split_templates` |
| T2 | `test_month_trigger_conditions` | Step 2 | `cargo test test_month_trigger_conditions` |
| T3 | `test_build_month_input_with_week_rollups_and_stats` | Step 3 | `cargo test test_build_month_input_with_week_rollups_and_stats` |
| T5 | `test_no_month_request_on_week_rolling_out` | Step 5 | `cargo test test_no_month_request_on_week_rolling_out` |

### `src/sleep/batch.rs`（全 1 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T4 | `test_batch_executes_week_then_month_calls` | Step 4 | `cargo test test_batch_executes_week_then_month_calls` |

### `src/sleep/episodic_renderer.rs`（全 1 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T6 | `test_episodic_renderer_after_split` | Step 6 | `cargo test test_episodic_renderer_after_split` |

---

## 工数見積もり

| Step | 内容 | 見積もり |
| -- | -- | -- |
| Step 1 | プロンプトファイル作成 | ~80 行 |
| Step 2 | Planner分離・月トリガー新設 | ~140 行（テスト含む） |
| Step 3 | 月Input Builder・統計値算出 | ~100 行（テスト含む） |
| Step 4 | batch.rs Call 2分離 | ~150 行（テスト含む） |
| Step 5 | 旧トリガー削除 | ~30 行（削除主体） |
| Step 6 | エピソードレンダラー確認 | ~20 行（テストのみ） |
| Step 7-8 | 動作確認・PR作成 | ~30 行 |
| **合計** |  | **~550 行** |

---

## 未解決・別途対応が必要な項目

| 項目 | 理由 | 対応時期 |
| -- | -- | -- |
| 月要約の更新トリガー（後から週要約が増えた場合の再生成） | Phase 1では初回生成のみ | Phase 2 |
| Background candidate（古い高リップルイベントの月要約） | 複雑性のためPhase 1で除外 | Phase 2 |
| 週/月で異なるモデル/パラメータ設定 | `SleepBatchConfig` の拡張が必要 | Phase 2 |
| 週/月で異なるスケジュール設定 | スケジューラー改修が必要 | Phase 3 |
