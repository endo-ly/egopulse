# Plan: Sleep Batch 実行モデルの正規化

Sleep Batch を Call 1/2/3 の一体実行から、`event_extraction` / `episodic_update` / `semantic_update` / `prospective_update` の4 stepへ正規化する。各stepは独立したbest-effort実行、固有checkpoint、出力確定、token集計を持ち、runは結果の集約だけを担う。

> **Note**: 振る舞い（What）は決して変えてはいけないが、より美しい設計があれば実装方法（HOW）だけは変えてもよい。

## 設計方針

- 実行意味論の正本は [`docs/sleep-execution-model.md`](../sleep-execution-model.md) とし、step名、状態遷移、成功条件、再実行規則を変更しない。
- 本Planは [`plan-sleep-table-redesign.md`](./plan-sleep-table-redesign.md) で追加するstep log/checkpoint APIを利用する。実装推奨順は **Plan 2（テーブル）→ Plan 1（実行モデル）** とする。
- `sleep_runs.status` は監視用の集約値に限定し、入力cutoffや再実行可否の判断には使わない。
- 各stepは自身の入力をDBから再取得し、前stepの失敗を後続stepの実行条件にしない。通常順序は仕様どおり維持する。
- Call 3のsemantic/prospective一括レスポンスを廃止し、異なる入力source、checkpoint、出力ファイル、失敗境界を持つ2 stepへ分離する。
- DB出力はcheckpoint・step statusとtransactionで確定する。memory fileは既存のbackup/recoveryを整理し、atomic renameとDB確定の不整合を回収可能にする。
- session archiveはrun成功やCall 3成功に従属させず、chatごとのmessage checkpoint最小値以下だけを対象にする。
- 後方互換分岐や旧Call 3フォールバックは残さず、新実行モデルへ置き換える。

## TDD 方針

テストリスト項目は外から見たstepの状態、永続化、副作用、再実行境界として定義する。各TDD Cycleでは1項目だけを選び、1回のRedで自動テストを1つ追加し、Greenで最小実装、Refactorで責務と命名を整える。ただし、1項目に必要な自動テスト総数を1件へ制限しない。複数のstatus、境界値、失敗地点を持つ項目は同じ項目でCycleを繰り返し、実装中に見つかった不安もテストリストへ戻してから次Cycleで扱う。

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 種別 | 既存パターン / 参照元 | 備考 |
| -- | -- | -- | -- |
| `src/sleep/orchestrator.rs` | 変更 | 現行 `execute_batch`、Call 1/2/3、memory write、archive | 4 stepの実行制御とrun集約 |
| `src/sleep/memory_update.rs` | 変更または分割 | 現行Call 3プロンプト・parse・retry | semantic/prospectiveを独立処理へ置換 |
| `src/sleep/event_extraction.rs` | 変更 | message抽出chunk構築 | chat別checkpointとupper bound対応 |
| `src/sleep/episodic_renderer.rs` | 変更 | episodic.md生成 | `episodic_update`の出力確定へ統合 |
| `src/sleep/mod.rs` | 変更 | sleep module公開境界、error | step単位エラーと内部module整理 |
| `src/agent_loop/compaction.rs` | 影響確認・必要時変更 | archive書き込み | boundary以下だけのarchiveを再利用可能にする |
| `src/storage/mod.rs` / `src/storage/queries.rs` | 利用・必要最小限の変更 | Plan 2で追加する型/API | 実行側に必要なtransaction APIだけ補完 |
| `docs/sleep.md` | 変更 | Sleep Batch全体仕様 | 実装完了後の現行仕様へ更新 |
| `docs/sleep-call3.md` | 変更 | Call 3a/3b設計 | 実装結果との差分反映 |
| `docs/sleep-execution-model.md` | 変更 | 本Planの仕様正本 | 実装で判明したHOW以外の差分がないことを確認 |
| `docs/db.md` / `docs/api.md` | 影響確認 | DB/API仕様 | Plan 2の更新内容と整合確認 |

## テストリスト / 不安リスト

| ID | 観点 | 期待する振る舞い | 優先 | 対応するCycle | 状態 / 今回対象外理由 |
| -- | -- | -- | -- | -- | -- |
| T1 | step独立性 | 1 stepがfailedでも、入力がある後続stepは実行され、成功済みstepは巻き戻らない | High | Step 1 | 未着手 |
| T2 | Call 3分離 | semantic失敗時もprospectiveが実行され、それぞれ異なる入力・出力・token・statusを持つ | High | Step 2 | 未着手 |
| T3 | checkpoint | success/no-change時だけ入力末尾へ進み、failed/skipped時は進まない | High | Step 3 | 未着手 |
| T4 | file commit | memory file公開後のDB確定失敗をrecoveryでき、fileとcheckpointの不整合を通常状態に残さない | High | Step 4 | 未着手 |
| T5 | run集約 | step結果からsuccess/partial_failure/failed/skippedとtoken合計が仕様どおり導出される | High | Step 5 | 未着手 |
| T6 | archive境界 | chatごとのevent/prospective checkpoint最小値以下だけをarchiveし、片方の失敗範囲を消さない | High | Step 6 | 未着手 |
| T7 | 再実行 | 次runは成功済み範囲を再処理せず、failed stepだけ同じ範囲を再試行する | High | Step 7 | 未着手 |
| T8 | backfill | 通常cursorと異なる履歴再抽出の扱いが既存backfillコマンドで壊れない | Medium | Step 7 | T7の統合テスト内で確認 |
| T9 | 並列実行 | agent単位のrunning排他を維持する | Medium | Step 7 | 既存回帰テストを維持 |

---


## Step 0: Worktree 作成

- ブランチ名: `refactor/normalize-sleep-execution`
- 作成コマンド:
  - `git worktree add ../egopulse-refactor-sleep-execution -b refactor/normalize-sleep-execution`
- 注意:
  - `worktree-create` skillを使用する。
  - Plan 2のPRを先にmergeするか、そのcommitを本worktreeへ取り込んでから着手する。

---

## Step 1: Orchestrator TDD Cycle - stepを独立実行する

### この Step の目的

run作成時の4 stepを順に評価し、1 stepの失敗で後続stepを中止しない実行骨格へ置き換える。

### 今回選ぶ項目

- 対象: `T1`
- 選ぶ理由: 以降のcheckpoint、file commit、run集約が依存する最小の実行骨格だから。
- この時点では扱わないこと: Call 3の入出力分離、checkpoint更新、archive、最終run status。

### RED: 失敗する自動テストを書く

- 追加するテスト名: `sleep_batch_continues_after_event_extraction_failure`
- Given: Event Extractionだけ失敗し、既存eventとconversationが後続stepの入力として存在する
- When: Sleep Batchを実行する
- Then: `event_extraction=failed` だが、`episodic_update`、`semantic_update`、`prospective_update` は実行済みの終端statusになる
- 失敗理由の想定: 現行orchestratorはstep lifecycleを持たず、Call 3エラーがrun全体を即時終了させる

このテストを通した後も、`successful_step_is_not_rolled_back_by_later_failure` を別Cycleで追加し、先行stepの出力とcheckpointが後続失敗で巻き戻らないことまで確認してから`T1`を完了とする。

### GREEN: 最小実装

4 stepの実行結果を共通の小さな結果型で扱い、各step開始・終了をPlan 2のstorage APIへ記録する。共通準備失敗だけをrun-level fatal errorとし、step実行中のエラーは記録して次stepへ進む。

### REFACTOR: 設計の整理

- 重複: step開始・成功・失敗・skip記録の共通処理
- 命名: Call番号ではなく仕様の `step_name` を使用
- 責務: orchestratorは順序と集約、各moduleはstep本体を担当
- テストの構造的結合: private関数の呼出順ではなくDBに残るstep結果を検証
- 次の項目へ進める身軽さ: semantic/prospectiveを別executorへ差し替え可能か

### テストリスト更新

- 完了: `T1`
- 追加: 実装中に見つかったstep共通準備の失敗境界
- 次候補: `T2`

### コミット

`refactor: execute sleep steps independently`

---

## Step 2: Memory Update TDD Cycle - semanticとprospectiveを分離する

### この Step の目的

一括Call 3を廃止し、semanticとprospectiveが別々の入力、LLM request、parse、retry、出力を持つようにする。

### 今回選ぶ項目

- 対象: `T2`
- 選ぶ理由: 現行で最も大きく失敗境界が混ざっている箇所だから。
- この時点では扱わないこと: checkpointの永続化、atomic file commit、archive。

### RED: 失敗する自動テストを書く

- 追加するテスト名: `prospective_update_runs_when_semantic_update_fails`
- Given: semantic requestはretry後もparse失敗し、prospective requestは正常なMarkdownを返す
- When: 2 stepを含むSleep Batchを実行する
- Then: semanticはfailed、prospectiveはsuccessとなり、prospective出力とtokenだけが確定する
- 失敗理由の想定: 現行は1レスポンスに `semantic` と `prospective` の両方を要求し、片方だけ成功できない

続くCycleでは `semantic_update_succeeds_when_prospective_update_fails` を追加する。失敗方向を入れ替えたときもstatus、出力、tokenが混線しないことを確認してから`T2`を完了とする。

### GREEN: 最小実装

semantic/prospective用の入力構築、system prompt、単一出力parse、retry処理を分離する。共通のLLM送信処理は共有してよいが、status・token・出力値はstepごとに返す。

### REFACTOR: 設計の整理

- 重複: retryとusage集計だけを共有し、prompt/output型は混ぜない
- 命名: `memory_update`という曖昧な一括名を残す場合も内部責務を明示
- 責務: semanticはevent、prospectiveはmessageを入力とする
- テストの構造的結合: prompt全文ではなく入力sourceと出力契約を検証
- 次の項目へ進める身軽さ: 各stepへcheckpointを接続できる形か

### テストリスト更新

- 完了: `T2`
- 追加: no-change出力の判定
- 次候補: `T3`

### コミット

`refactor: split semantic and prospective sleep updates`

---

## Step 3: Checkpoint TDD Cycle - success時だけ入力位置を進める

### この Step の目的

各consumerが固定upper boundまでのsource行を読み、処理結果に応じてcheckpointを正しく更新する。

### 今回選ぶ項目

- 対象: `T3`
- 選ぶ理由: 重複処理と取りこぼしを防ぐ実行モデルの中心だから。
- この時点では扱わないこと: file公開失敗のrecovery、archive削除。

### RED: 失敗する自動テストを書く

- 追加するテスト名: `successful_sleep_step_advances_composite_checkpoint`
- Given: 同一timestampを含む複数message/eventと既存checkpointがある
- When: 対象stepを実行する
- Then: success時に実入力末尾の `(cursor_at, cursor_id)` へ進む
- 失敗理由の想定: 現行はlatest successful runのfinished_atをCall 1 cutoffに使い、step別checkpointを参照しない

同じ`T3`について、`no_change_sleep_step_advances_checkpoint`、`failed_sleep_step_keeps_checkpoint`、`skipped_sleep_step_keeps_checkpoint`をそれぞれ独立したCycleで追加する。4状態を1テストへ詰め込まず、すべて通ってから完了とする。

### GREEN: 最小実装

step開始時にupper boundを固定し、Plan 2のcheckpoint APIから開始cursorを取得する。入力queryは複合cursor順で決定し、出力確定と同じ成功経路だけでcheckpointを更新する。

### REFACTOR: 設計の整理

- 重複: message consumer共通のchat別範囲計算
- 命名: cutoff/watermarkをcheckpoint/upper boundへ統一
- 責務: input selectionとLLM処理を分離
- テストの構造的結合: SQLそのものではなく取得範囲と永続checkpointを検証
- 次の項目へ進める身軽さ: file stepのcommit protocolを挿入可能か

### テストリスト更新

- 完了: `T3`
- 追加: source時刻単調性を破るbackfillの扱い
- 次候補: `T4`

### コミット

`feat: drive sleep inputs from step checkpoints`

---

## Step 4: Memory File TDD Cycle - fileとDB確定をrecovery可能にする

### この Step の目的

episodic/semantic/prospectiveのfile公開、snapshot、checkpoint、step successを一つの回収可能なcommit protocolとして扱う。

### 今回選ぶ項目

- 対象: `T4`
- 選ぶ理由: fileだけ新しくcheckpointだけ古い状態は再実行時のデータ損失につながるため。
- この時点では扱わないこと: archive境界、run status集約。

### RED: 失敗する自動テストを書く

- 追加するテスト名: `memory_file_commit_recovers_when_database_finalize_fails`
- Given: 新memory fileの一時書込みとrenameは成功するが、snapshot/checkpoint/step successのDB確定が失敗する
- When: 次回run開始時にrecoveryを実行する
- Then: 旧fileへ戻り、checkpointは進まず、同じ入力を再試行できる
- 失敗理由の想定: 現行backup処理はrun全体の一括file writeを前提とし、step単位DB確定との整合を持たない

続くCycleでは、rename前の失敗で旧fileを維持する `memory_file_commit_preserves_current_file_before_publish` と、no-change成功でfile/snapshotを作らない `no_change_memory_step_skips_file_and_snapshot_write` を追加する。代表的なDB失敗だけで`T4`を完了扱いしない。

### GREEN: 最小実装

既存backup/recoveryをstep単位のfile commitへ整理する。temp write・sync・backup・atomic rename・DB finalize・cleanupの順序を固定し、未完了markerまたは既存backup情報から起動時に旧状態へ戻せるようにする。

### REFACTOR: 設計の整理

- 重複: 3 memory fileのcommit手順を共通化
- 命名: write/saveではなくprepare/publish/finalize/recoverを区別
- 責務: file I/Oとstep business logicを分離
- テストの構造的結合: 一時ディレクトリ名ではなく最終file/checkpointを検証
- 次の項目へ進める身軽さ: no-change時にfile/snapshotを作らない経路が明瞭か

### テストリスト更新

- 完了: `T4`
- 追加: no-change成功時にsnapshotを作らないこと
- 次候補: `T5`

### コミット

`feat: make sleep memory commits recoverable`

---

## Step 5: Run Finalization TDD Cycle - statusとtokenをstepから集約する

### この Step の目的

run終了時にstepの最終statusとtokenからrun status・集約token・error summaryを一度だけ導出する。

### 今回選ぶ項目

- 対象: `T5`
- 選ぶ理由: runをcheckpoint判断から切り離しつつ、監視情報として正確にするため。
- この時点では扱わないこと: archive対象messageの削除。

### RED: 失敗する自動テストを書く

- 追加するテスト名: `sleep_run_finalization_derives_status_matrix`
- Given: success/failed/skippedの主要な組合せを持つrun
- When: runをfinalizeする
- Then: 仕様表どおりのsuccess/partial_failure/failed/skippedになる
- 失敗理由の想定: 現行はBatchContextへtokenを直接加算し、Call 3成否でrun success/failedを更新する

次のCycleで `sleep_run_finalization_sums_step_tokens_including_retries` を追加し、status集約とは分けてtoken正本と二重計上防止を確認する。

### GREEN: 最小実装

Plan 2のfinalize APIをrun終了時に呼び、step行からstatus、token、error summaryを導出する。個別stepとrunへtokenを二重加算する経路を削除する。

### REFACTOR: 設計の整理

- 重複: status集約規則をstorage/runtimeの複数箇所に持たない
- 命名: `update_sleep_run_success/failed`を集約finalizeへ置換
- 責務: step実行は結果保存、run finalizerは集約のみ
- テストの構造的結合: enum内部ではなく保存後のSleepRunを検証
- 次の項目へ進める身軽さ: archive失敗がrun statusを変更しないか

### テストリスト更新

- 完了: `T5`
- 追加: pending/running残存時のfailed回収
- 次候補: `T6`

### コミット

`refactor: derive sleep run outcome from step results`

---

## Step 6: Session Archive TDD Cycle - consumer共通境界までarchiveする

### この Step の目的

conversationを利用する全consumerが処理済みの範囲だけをarchive/clearし、未処理messageを保持する。

### 今回選ぶ項目

- 対象: `T6`
- 選ぶ理由: prospective失敗時の会話消失を防ぐため。
- この時点では扱わないこと: backfill全体の統合確認。

### RED: 失敗する自動テストを書く

- 追加するテスト名: `session_archive_stops_at_minimum_message_checkpoint`
- Given: 同じchatでevent extraction checkpointがm5、prospective checkpointがm3
- When: archive処理を実行する
- Then: m3以下だけをarchive/clearし、m4以降はDB/sessionに残る
- 失敗理由の想定: 現行はsession snapshot全体をarchiveした後、updated_at一致で一括clearする

続くCycleでは `session_archive_keeps_messages_when_consumer_checkpoint_is_missing` と `session_archive_failure_keeps_messages_for_retry` を追加する。境界の正常系だけでなく、未処理consumerとarchive失敗時の保全を確認してから`T6`を完了とする。

### GREEN: 最小実装

chatごとに2 consumerのcheckpointを読み、最小cursorをboundaryとする。boundary以下のmessageだけをarchive形式へ変換し、削除も同じ範囲へ限定する。archive失敗はstep成功やrun集約を巻き戻さず、次回冪等に再試行可能にする。

### REFACTOR: 設計の整理

- 重複: compactionとsleep archiveのmessage整形共有
- 命名: session全消去を示す既存名を範囲削除に合わせる
- 責務: boundary計算、archive書込み、DB削除を分離
- テストの構造的結合: archive file名ではなく内容と残存messageを検証
- 次の項目へ進める身軽さ: 再実行統合テストを組みやすいか

### テストリスト更新

- 完了: `T6`
- 追加: archive成功・DB削除失敗時の冪等性
- 次候補: `T7`

### コミット

`feat: archive sessions through shared sleep checkpoints`

---

## Step 7: Integration TDD Cycle - failed stepだけを次runで再試行する

### この Step の目的

2回のrunを通じて、成功済み範囲の重複処理を避け、failed stepだけが同じ入力範囲を再試行することを確認する。

### 今回選ぶ項目

- 対象: `T7`
- 選ぶ理由: step独立性、checkpoint、run集約、archiveが結合した最終的な利用者価値だから。
- この時点では扱わないこと: scheduler設定変更やUIへのstep詳細表示。

### RED: 失敗する自動テストを書く

- 追加するテスト名: `next_sleep_run_retries_only_failed_step_input`
- Given: 1回目はsemanticだけfailed、他stepはsuccess。2回目は全LLM応答が成功する
- When: 同じagentでSleep Batchを2回実行する
- Then: 2回目のsemanticは1回目と同じevent範囲を受け取り、成功済みmessage consumerはcheckpoint後の新規入力だけを受け取る
- 失敗理由の想定: 現行はlatest successful runの時刻またはrun全体statusへcutoffが依存する

`T8`と`T9`はこのテストへ同梱せず、続く独立Cycleで `event_backfill_does_not_advance_normal_sleep_checkpoint` と `sleep_batch_rejects_concurrent_run_for_same_agent` を追加する。

### GREEN: 最小実装

残る旧cutoff、Call番号依存、run-level再実行判断を除去し、step checkpointだけを次回入力の正本にする。既存backfillとagent単位running排他の回帰を同じ統合fixtureで確認する。

### REFACTOR: 設計の整理

- 重複: test fixtureのmessage/event/checkpoint準備
- 命名: 旧Call 1/2/3コメントと関数名をstep名へ統一
- 責務: orchestratorのContextがstep間の可変出力を抱えすぎていないか
- テストの構造的結合: LLM call回数だけでなく入力範囲とDB結果を検証
- 次の項目へ進める身軽さ: scheduler/Web APIが集約statusをそのまま扱えるか

### テストリスト更新

- 完了: `T7`, `T8`, `T9`
- 追加: なし
- 次候補: なし

### コミット

`test: verify sleep step retry boundaries`

---

## Step 8: 動作確認

- 対象テスト:
  ```bash
  cargo test sleep::orchestrator::
  cargo test sleep::memory_update::
  cargo test storage::queries::
  cargo test sleep::scheduler::
  ```
- 全体検証:
  ```bash
  cargo fmt --check
  cargo test
  cargo check
  cargo clippy --all-targets --all-features -- -D warnings
  ```
- 文書確認:
  - `docs/sleep.md`を実装済みの現行仕様へ更新
  - `docs/sleep-call3.md`と`docs/sleep-execution-model.md`のstatus/checkpoint/archive規則が実装と一致することを確認
  - `rg 'Call 1|Call 2|Call 3|latest_successful.*run' src/sleep` で意図しない旧実行モデル依存が残っていないことを確認
- 失敗時に戻るStep: 対応するTDD Cycleへ戻り、新しい不安はテストリストへ追加する

---

## Step 9: PR 作成

- PR タイトル: `refactor: Sleep Batch実行モデルを4 stepへ正規化`
- PR description:
  - 概要: Sleep Batchを独立best-effortの4 stepへ正規化し、Call 3分離、checkpoint駆動、recoverable file commit、run集約、archive境界を実装
  - 依存: Plan 2「Sleep Batchテーブル再設計」のPR
  - テスト: step独立性、Call 3分離、checkpoint、file recovery、run集約、archive、再実行
  - Close対象: なし（Issueが割り当てられた場合はPR作成時に追記）

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
| ---- | ---- | -- |
| `src/sleep/orchestrator.rs` | 変更 | 4 step実行、step lifecycle、run finalization、archive制御 |
| `src/sleep/memory_update.rs` | 変更または分割 | semantic/prospectiveの独立prompt・parse・retry |
| `src/sleep/event_extraction.rs` | 変更 | checkpoint以降・upper bound以下のmessage入力 |
| `src/sleep/episodic_renderer.rs` | 変更 | episodic stepのfile commit統合 |
| `src/sleep/mod.rs` | 変更 | module/error境界整理 |
| `src/agent_loop/compaction.rs` | 必要時変更 | cursor境界付きarchive処理の共有 |
| `src/storage/mod.rs` | 必要最小限の変更 | Plan 2 API利用に必要な型補完 |
| `src/storage/queries.rs` | 必要最小限の変更 | step出力transactionや範囲削除API補完 |
| `docs/sleep.md` | 変更 | 現行仕様を新実行モデルへ更新 |
| `docs/sleep-call3.md` | 変更 | semantic/prospective実装結果反映 |
| `docs/sleep-execution-model.md` | 変更 | 実装との最終整合 |
| `docs/db.md` / `docs/api.md` | 影響確認・必要時変更 | Plan 2との整合 |

---

## コミット分割

1. `refactor: execute sleep steps independently` - orchestrator骨格とstep lifecycle
2. `refactor: split semantic and prospective sleep updates` - Call 3分離
3. `feat: drive sleep inputs from step checkpoints` - input selectionとcheckpoint更新
4. `feat: make sleep memory commits recoverable` - file commit/recovery
5. `refactor: derive sleep run outcome from step results` - run status/token集約
6. `feat: archive sessions through shared sleep checkpoints` - archive boundary
7. `test: verify sleep step retry boundaries` - 2 run統合・backfill・排他回帰
8. `docs: align sleep documentation with normalized execution` - 関連文書

---

## 自動テスト一覧（最低限 19 件）

この一覧はPlan作成時点で必要と判断した最低限の予定であり、最終テスト件数の上限ではない。各行を独立したRedとして扱い、実装中に新しい不安が見つかった場合はテストリストとCycleを追加する。

### Sleep orchestration（全 14 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T1 | `sleep_batch_continues_after_event_extraction_failure` | Step 1 | `cargo test sleep_batch_continues_after_event_extraction_failure` |
| T1 | `successful_step_is_not_rolled_back_by_later_failure` | Step 1 | `cargo test successful_step_is_not_rolled_back_by_later_failure` |
| T3 | `successful_sleep_step_advances_composite_checkpoint` | Step 3 | `cargo test successful_sleep_step_advances_composite_checkpoint` |
| T3 | `no_change_sleep_step_advances_checkpoint` | Step 3 | `cargo test no_change_sleep_step_advances_checkpoint` |
| T3 | `failed_sleep_step_keeps_checkpoint` | Step 3 | `cargo test failed_sleep_step_keeps_checkpoint` |
| T3 | `skipped_sleep_step_keeps_checkpoint` | Step 3 | `cargo test skipped_sleep_step_keeps_checkpoint` |
| T5 | `sleep_run_finalization_derives_status_matrix` | Step 5 | `cargo test sleep_run_finalization_derives_status_matrix` |
| T5 | `sleep_run_finalization_sums_step_tokens_including_retries` | Step 5 | `cargo test sleep_run_finalization_sums_step_tokens_including_retries` |
| T6 | `session_archive_stops_at_minimum_message_checkpoint` | Step 6 | `cargo test session_archive_stops_at_minimum_message_checkpoint` |
| T6 | `session_archive_keeps_messages_when_consumer_checkpoint_is_missing` | Step 6 | `cargo test session_archive_keeps_messages_when_consumer_checkpoint_is_missing` |
| T6 | `session_archive_failure_keeps_messages_for_retry` | Step 6 | `cargo test session_archive_failure_keeps_messages_for_retry` |
| T7 | `next_sleep_run_retries_only_failed_step_input` | Step 7 | `cargo test next_sleep_run_retries_only_failed_step_input` |
| T8 | `event_backfill_does_not_advance_normal_sleep_checkpoint` | Step 7 | `cargo test event_backfill_does_not_advance_normal_sleep_checkpoint` |
| T9 | `sleep_batch_rejects_concurrent_run_for_same_agent` | Step 7 | `cargo test sleep_batch_rejects_concurrent_run_for_same_agent` |

### Memory update / file commit（全 5 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T2 | `prospective_update_runs_when_semantic_update_fails` | Step 2 | `cargo test prospective_update_runs_when_semantic_update_fails` |
| T2 | `semantic_update_succeeds_when_prospective_update_fails` | Step 2 | `cargo test semantic_update_succeeds_when_prospective_update_fails` |
| T4 | `memory_file_commit_recovers_when_database_finalize_fails` | Step 4 | `cargo test memory_file_commit_recovers_when_database_finalize_fails` |
| T4 | `memory_file_commit_preserves_current_file_before_publish` | Step 4 | `cargo test memory_file_commit_preserves_current_file_before_publish` |
| T4 | `no_change_memory_step_skips_file_and_snapshot_write` | Step 4 | `cargo test no_change_memory_step_skips_file_and_snapshot_write` |

---

## 工数見積もり

| Step | 内容 | 見積もり |
| -- | -- | -- |
| Step 1 | 4 step実行骨格 | ~180行 |
| Step 2 | semantic/prospective分離 | ~260行 |
| Step 3 | checkpoint駆動の入力選択 | ~220行 |
| Step 4 | file commit/recovery | ~220行 |
| Step 5 | run status/token集約接続 | ~100行 |
| Step 6 | archive境界と範囲削除 | ~180行 |
| Step 7 | 2 run統合・回帰テスト | ~180行 |
| Step 8-9 | 文書、全体検証、PR | ~80行 |
| **合計** |  | **~1,420行** |

---

# 追加作業: Call 3分割の差し戻し

ここまでのPlanは実装済みである。今回のPRへ誤って含めたCall 3のSemantic / Prospective分割だけを、分割前の状態へ戻す。

## 方針

新しい設計、関数、抽象化、テストは追加しない。Call 3分割を導入した差分を確認し、分割前に存在した単一Memory Updateの実装を復元する。

`sleep_run_steps`や`sleep_step_checkpoints`など、既に導入済みのテーブル設計は変更しない。将来のCall 3分割で利用するため、そのまま維持する。

[`docs/sleep-call3.md`](../sleep-call3.md)は将来実施する改修の設計書であり、今回のPRの実装対象には含めない。

## 差し戻す内容

Call 3をSemantic / Prospectiveの2回に分けて呼び出す処理を削除し、分割前の次の処理へ戻す。

- 既存memoryとconversationを単一promptへ渡す
- 1回のLLM呼び出しで`semantic`と`prospective`を受け取る
- 既存のparse、retry、token集計、memory file書き込みを利用する
- Semantic専用入力、Prospective専用入力、個別prompt、個別retryを削除する

今回の差し戻しに伴って不要になった分割専用コードと、そのコードだけを検証するテストは削除する。将来利用を理由に未使用コードを残さない。

## 作業手順

1. Call 3分割を導入したコミットと差分を確認する。
2. `src/sleep/memory_update.rs`を分割前の単一Memory Updateへ戻す。
3. `src/sleep/orchestrator.rs`からSemantic / Prospectiveの個別呼び出しを取り除き、分割前の呼び出しフローを復元する。
4. 分割のためだけに追加した関数、型、prompt、テストを削除する。
5. 実行モデル正規化やテーブル対応など、Call 3分割と無関係な変更が巻き戻されていないことを差分で確認する。
6. `docs/sleep-call3.md`を今回のPR差分から外し、PR descriptionからCall 3分割を実装したという記載を削除する。

## 回帰確認

差し戻しなので新規テストは追加しない。分割前から存在するMemory Updateのテストと、今回維持するSleep Batch全体の既存テストを実行する。

```bash
cargo test sleep::memory_update::
cargo test sleep::orchestrator::
cargo fmt --check
cargo test
cargo check
cargo clippy --all-targets --all-features -- -D warnings
```

テスト修正が必要な場合は、分割実装を前提に追加されたテストの削除または分割前の期待値への復元に限定する。差し戻しを成立させるための新しい振る舞いは追加しない。

## 自己チェック

Planと今回のPR差分を見直し、次を確認する。

- Call 3のLLM呼び出しが分割前と同じ1回に戻っている
- Call 3の入力・出力・retry・file書き込みが分割前の契約に戻っている
- 分割専用コードと分割専用テストが残っていない
- テーブル設計とCall 3分割以外の実行モデル改修を巻き戻していない
- `sleep-call3.md`を今回実装したとする文書・PR記載が残っていない

齟齬があれば差し戻し対象を再確認し、既存実装へ戻したうえで回帰確認を再実行する。
