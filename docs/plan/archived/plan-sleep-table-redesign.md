# Plan: Sleep Batch テーブル再設計

Sleep Batchのstep別実行履歴と処理checkpointを永続化するため、`sleep_run_steps`と`sleep_step_checkpoints`を追加する。併せて`memory_snapshots`の整合性制約、`sleep_runs`の集約status/token更新、関連APIと文書を新責務へ合わせる。

> **Note**: 振る舞い（What）は決して変えてはいけないが、より美しい設計があれば実装方法（HOW）だけは変えてもよい。

## 設計方針

- DB設計の正本は [`docs/sleep-tables.md`](../sleep-tables.md)、実行意味論は [`docs/sleep-execution-model.md`](../sleep-execution-model.md) とする。
- `sleep_runs`はrun集約log、`sleep_run_steps`はrun×step log、`memory_snapshots`はrun×file log、`sleep_step_checkpoints`はagent×step×sourceのcurrent stateとして責務を分離する。
- migrationはv7、v8、v9へ分け、各versionを単独で再実行可能かつtransactionalにする。既存データの暗黙削除や旧仕様フォールバックは行わない。
- run作成と4 stepのpending行作成は同一transactionで行い、step行の欠落を通常状態にしない。
- checkpointは `(cursor_at, cursor_id)` の複合cursorとし、message sourceはchat単位、episode event sourceはagent単位で保持する。
- source行なしの`skipped`ではcheckpointを更新せず、sourceを検査したno-change `success`では更新する。storage APIはこの差を表現できる形にする。
- step tokenを正本とし、run tokenはfinalize時のSUMで更新する。run側とstep側へ別々に加算するAPIは作らない。
- `partial_failure`をrun statusへ追加し、Web API/UIを含む既存consumerが未知statusとして壊れないことを確認する。
- Plan 1「実行モデルの正規化」の前提基盤として先に実装・mergeする。Plan 2単独では現行orchestratorの動作を壊さず、新APIを追加した状態で完了可能にする。

## TDD 方針

migration、制約、query API、集約結果を外部から観測できる振る舞いとしてテストリスト化する。各Cycleは1項目だけを選び、1回のRedでは自動テストを1つだけ追加するが、1項目に必要なテスト総数を1件へ制限しない。複数の移行元、制約、status、transaction失敗を持つ項目は同じ項目でCycleを繰り返す。後続Planでしか使わないAPIもstorage層の契約を直接テストし、実装中に見つかった不安はテストリストへ追加する。

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 種別 | 既存パターン / 参照元 | 備考 |
| -- | -- | -- | -- |
| `src/storage/migration.rs` | 変更 | v2-v6 migration、migration tests | schema v7-v9 |
| `src/storage/mod.rs` | 変更 | `SleepRunStatus`、storage model | step/checkpoint型とenum |
| `src/storage/queries.rs` | 変更 | sleep run、snapshot、event query | lifecycle/checkpoint/finalize API |
| `src/channels/web/sleep.rs` | 変更または影響確認 | sleep run JSON API | `partial_failure`の公開 |
| `web/src/**` | 変更または影響確認 | Sleep Batch run表示・tests | 新status表示 |
| `docs/db.md` | 変更 | DB schema正本 | 新テーブル、制約、API一覧 |
| `docs/api.md` | 変更 | Web API仕様 | status列挙更新 |
| `docs/sleep-tables.md` | 変更 | 本Planの仕様正本 | 実装結果との最終整合 |

## テストリスト / 不安リスト

| ID | 観点 | 期待する振る舞い | 優先 | 対応するCycle | 状態 / 今回対象外理由 |
| -- | -- | -- | -- | -- | -- |
| T1 | v7 migration | fresh DBとv6 DBの両方に正しい制約・index付き `sleep_run_steps` が作成される | High | Step 1 | 未着手 |
| T2 | run作成 | runと4つのpending stepが同一transactionで作成され、重複running runは作られない | High | Step 2 | 未着手 |
| T3 | step lifecycle | pending→running→success/failed/skippedだけを保存でき、token/error/metadataを取得できる | High | Step 3 | 未着手 |
| T4 | v8 migration | 正しいCHECK/PKを持つ `sleep_step_checkpoints` が作成され、不正step/source組合せを拒否する | High | Step 4 | 未着手 |
| T5 | checkpoint API | 複合cursorをsource単位でupsert/readでき、同時刻の別IDを取りこぼさない | High | Step 5 | 未着手 |
| T6 | v9 migration | snapshot既存データを保持しつつUNIQUE/FK/CHECKを追加し、不正重複・file値・run参照を拒否する | High | Step 6 | 未着手 |
| T7 | run集約 | step statusとtokenからrun status、token合計、finished_at、error summaryをtransactionで確定する | High | Step 7 | 未着手 |
| T8 | API/UI | `partial_failure`をWeb APIとUIが正しく返却・表示し、既存statusも回帰しない | Medium | Step 8 | 未着手 |
| T9 | migration再実行 | v7-v9適用後にmigrationを再実行してもschema/dataが変化しない | High | Step 6 | 各migration testと最終確認で実施 |
| T10 | transaction API | 出力保存とcheckpoint/step successを同一transactionで確定できる境界がPlan 1から利用可能 | High | Step 7 | 未着手 |

---

## Step 0: Worktree 作成

- ブランチ名: `feat/sleep-step-storage`
- 作成コマンド:
  - `git worktree add ../egopulse-feat-sleep-step-storage -b feat/sleep-step-storage`
- 注意: `worktree-create` skillを使用する。

---

## Step 1: Migration v7 TDD Cycle - `sleep_run_steps`を追加する

### この Step の目的

run×stepの実行logを保持するテーブルと検索indexを追加する。

### 今回選ぶ項目

- 対象: `T1`
- 選ぶ理由: 以降のrun作成・step lifecycle APIの土台だから。
- この時点では扱わないこと: query API、checkpoint、snapshot制約。

### RED: 失敗する自動テストを書く

- 追加するテスト名: `migration_v6_to_v7_creates_sleep_run_steps`
- Given: schema version 6のDBと既存sleep run
- When: migrationを実行する
- Then: v7へ上がり、4 step名、5 status、FK cascade、複合PK、検索indexを持つテーブルが存在し、既存runは保持される
- 失敗理由の想定: schema versionは6で、対象テーブルが存在しない

続くCycleで `fresh_database_contains_sleep_run_steps_schema` を追加し、upgrade DBだけでなくfresh DBの最終schemaも確認してから`T1`を完了とする。

### GREEN: 最小実装

`SCHEMA_VERSION`と`run_migrations`へv7 transactionを追加する。仕様書のDDLを基準にCHECK、PK、FK、indexを作成し、migration履歴を記録する。

### REFACTOR: 設計の整理

- 重複: migration testのschema検査helper
- 命名: DB値を仕様のstep/status名へ一致
- 責務: migration内にruntime queryを混ぜない
- テストの構造的結合: sqlite_master文字列全体ではなく制約の振る舞いを検証
- 次の項目へ進める身軽さ: model/APIを追加できるschemaか

### テストリスト更新

- 完了: `T1`
- 追加: cascade deleteの明示テスト候補
- 次候補: `T2`

### コミット

`feat: add sleep run steps migration`

---

## Step 2: Run Creation TDD Cycle - 4 stepをpendingで同時作成する

### この Step の目的

Sleep run作成時に4 step行を欠落なく事前作成し、既存のagent単位排他を維持する。

### 今回選ぶ項目

- 対象: `T2`
- 選ぶ理由: step lifecycleの開始状態を一意にするため。
- この時点では扱わないこと: step更新、run finalize。

### RED: 失敗する自動テストを書く

- 追加するテスト名: `try_create_sleep_run_inserts_four_pending_steps_atomically`
- Given: running runがないagent
- When: `try_create_sleep_run`を呼ぶ
- Then: run 1行と仕様の4 stepがpending・時刻NULL・token 0で作成され、2回目は既存排他により作成されない
- 失敗理由の想定: 現行APIは`sleep_runs`だけをinsertする

次のCycleでは `try_create_sleep_run_rolls_back_when_step_initialization_fails` を追加し、runだけが残らないことを確認する。

### GREEN: 最小実装

既存Immediate transaction内でrun insert後に4 stepをinsertする。step名は型で列挙し、runtime側が任意文字列を渡さなくてよいAPIにする。

### REFACTOR: 設計の整理

- 重複: `create_sleep_run`と`try_create_sleep_run`のinsert処理
- 命名: step一覧の単一正本
- 責務: run creation transactionに初期step作成を閉じ込める
- テストの構造的結合: SQL回数ではなくDB最終状態を検証
- 次の項目へ進める身軽さ: lifecycle APIが型を再利用できるか

### テストリスト更新

- 完了: `T2`
- 追加: run insert後のstep insert失敗時rollback
- 次候補: `T3`

### コミット

`feat: initialize sleep steps with each run`

---

## Step 3: Step Lifecycle TDD Cycle - 状態遷移と監査値を保存する

### この Step の目的

stepの開始・終端状態、token、error、metadataを型安全に更新・取得できるようにする。

### 今回選ぶ項目

- 対象: `T3`
- 選ぶ理由: Plan 1のorchestratorが直接利用する最小APIだから。
- この時点では扱わないこと: checkpoint、run status集約。

### RED: 失敗する自動テストを書く

- 追加するテスト名: `sleep_step_lifecycle_persists_terminal_result`
- Given: pendingの4 stepを持つrun
- When: 1 stepをrunningにし、retry分を含むtoken・metadata付きsuccessへ更新する
- Then: started_at/finished_at/status/token/metadataが取得でき、不正な終端状態からの再遷移はConflictになる
- 失敗理由の想定: step modelとquery APIが存在しない

続くCycleで `sleep_step_lifecycle_rejects_invalid_transition` を追加する。正常なsuccess保存と不正遷移拒否を別々のRedで確認する。

### GREEN: 最小実装

`SleepStepName`、`SleepStepStatus`、`SleepRunStep`を追加し、start、finish、list/get APIを実装する。公開範囲はsleep runtimeから必要な最小の`pub(crate)`とする。

### REFACTOR: 設計の整理

- 重複: enumのDisplay/FromStr実装
- 命名: `finish`にsuccess固定の意味を持たせない
- 責務: status遷移検証をquery呼出側へ分散させない
- テストの構造的結合: private helperではなく公開storage契約を検証
- 次の項目へ進める身軽さ: checkpoint更新transactionと結合可能か

### テストリスト更新

- 完了: `T3`
- 追加: metadata JSON不正値の扱い
- 次候補: `T4`

### コミット

`feat: persist sleep step lifecycle`

---

## Step 4: Migration v8 TDD Cycle - `sleep_step_checkpoints`を追加する

### この Step の目的

agent×step×sourceの現在処理位置を保持し、不正なstep/source組合せをDB制約で拒否する。

### 今回選ぶ項目

- 対象: `T4`
- 選ぶ理由: checkpoint APIの前提schemaだから。
- この時点では扱わないこと: cursor query/upsert、source取得。

### RED: 失敗する自動テストを書く

- 追加するテスト名: `migration_v7_to_v8_creates_validated_sleep_checkpoints`
- Given: schema version 7のDB
- When: migrationを実行する
- Then: v8へ上がり、複合PKとstep/source CHECKを持つテーブルが作られ、`episodic_update`やsemantic+messagesのinsertを拒否する
- 失敗理由の想定: 対象テーブルと制約が存在しない

次のCycleでは `sleep_checkpoint_schema_rejects_invalid_step_source_pairs` を追加し、table存在確認とCHECK制約の検証を分離する。

### GREEN: 最小実装

仕様DDLどおりのv8 migrationをtransactionで追加する。source_idは外部sourceを表す汎用IDとして保持し、存在しないchatの履歴checkpointを保持できるようFKは追加しない。

### REFACTOR: 設計の整理

- 重複: migration version fixture
- 命名: `cursor_at`と`cursor_id`を他のwatermark名と混在させない
- 責務: source組合せ制約をruntimeだけに委ねない
- テストの構造的結合: CREATE文ではなく許可/拒否されるinsertを検証
- 次の項目へ進める身軽さ: source別APIを共通型で表現可能か

### テストリスト更新

- 完了: `T4`
- 追加: checkpoint行がない場合の未処理表現
- 次候補: `T5`

### コミット

`feat: add sleep step checkpoint migration`

---

## Step 5: Checkpoint Query TDD Cycle - 複合cursorをread/upsertする

### この Step の目的

message/event consumerがsource単位のcheckpointを取得・前進できるstorage契約を提供する。

### 今回選ぶ項目

- 対象: `T5`
- 選ぶ理由: 同一timestampの取りこぼし防止をAPIレベルで保証するため。
- この時点では扱わないこと: 実際のstep入力選択とarchive境界計算。

### RED: 失敗する自動テストを書く

- 追加するテスト名: `sleep_checkpoint_preserves_composite_cursor_per_source`
- Given: 同一agentで2 chatのmessage checkpointとsemantic event checkpoint
- When: 各sourceを異なる `(cursor_at, cursor_id)` へupsertしてreadする
- Then: source間で混線せず、同時刻でもcursor_idが新しい位置を保持し、存在しないsourceはNoneになる
- 失敗理由の想定: checkpoint model/query APIが存在しない

続くCycleで `sleep_checkpoint_rejects_backward_cursor_update` を追加し、通常実行用APIがcheckpointを後退させないことを確認する。

### GREEN: 最小実装

checkpoint key/cursor型、get/upsert/list APIを追加する。step/sourceの組合せはenum constructorで表現し、不正値はDB到達前にも防ぐ。後退更新を許可する専用backfill操作と通常の前進操作は混同しない。

### REFACTOR: 設計の整理

- 重複: message/event source keyの組立て
- 命名: timestamp/idではなくcursorとしてAPIを抽象化
- 責務: monotonic前進検証を通常upsertへ閉じ込める
- テストの構造的結合: SQL tuple比較ではなく保存・取得結果を検証
- 次の項目へ進める身軽さ: Plan 1のtransaction APIへcursor型を渡せるか

### テストリスト更新

- 完了: `T5`
- 追加: 通常APIによるcursor後退拒否
- 次候補: `T6`

### コミット

`feat: add composite sleep checkpoint queries`

---

## Step 6: Migration v9 TDD Cycle - snapshot制約を追加する

### この Step の目的

`memory_snapshots`をrun×fileで一意にし、run参照とfile値をDBで保証しながら既存データを保持する。

### 今回選ぶ項目

- 対象: `T6`
- 選ぶ理由: Plan 1のfile recoveryがsnapshotを一意な監査・復旧情報として使うため。
- この時点では扱わないこと: file commit protocol本体。

### RED: 失敗する自動テストを書く

- 追加するテスト名: `migration_v8_to_v9_rebuilds_memory_snapshots_with_constraints`
- Given: schema version 8で、有効な既存snapshotを持つDB
- When: migrationを実行し、再度migrationを実行する
- Then: 既存snapshotが保持され、同一run/file重複、不正file、存在しないrunを拒否し、再実行でも変化しない
- 失敗理由の想定: 現行tableにUNIQUE/FK/CHECKがない

同じ`T6`について、`memory_snapshot_constraints_reject_invalid_rows` と `sleep_storage_migrations_are_idempotent_through_v9` を独立したCycleで追加する。既存データ保持、制約、再実行を1テストへ押し込めない。

### GREEN: 最小実装

foreign key設定を確認したtransaction内で新table作成、既存行copy、旧table削除、rename、必要index再作成を行う。既存重複があり得る場合はmigration前検査で明示的Conflictにし、暗黙に1件へ潰さない。

### REFACTOR: 設計の整理

- 重複: table rebuild helperの必要性
- 命名: v9一時table名をmigration内だけに閉じる
- 責務: データ修復をmigrationへ勝手に混ぜない
- テストの構造的結合: pragmaと実際の制約違反を両方確認
- 次の項目へ進める身軽さ: snapshot create/update APIが一意制約を前提に簡潔になるか

### テストリスト更新

- 完了: `T6`, `T9`
- 追加: cascade delete確認
- 次候補: `T7`

### コミット

`feat: enforce memory snapshot integrity`

---

## Step 7: Run Finalization TDD Cycle - step結果を集約する

### この Step の目的

stepの最終status/tokenを正本としてrunをtransactionalにfinalizeし、Plan 1が出力とcheckpointを同時確定できるstorage境界を用意する。

### 今回選ぶ項目

- 対象: `T7`
- 選ぶ理由: 新テーブルをrun集約へ接続し、token二重計上を防ぐため。
- この時点では扱わないこと: orchestratorからの呼出し、file I/O。

### RED: 失敗する自動テストを書く

- 追加するテスト名: `finalize_sleep_run_derives_status_matrix`
- Given: statusの異なる4 step行を持つ複数run
- When: 各runをfinalizeする
- Then: success/partial_failure/failed/skippedとpending/running残存時のfailedが仕様表どおりになる
- 失敗理由の想定: `partial_failure` enumとstep集約APIがなく、現行APIはcallerのtoken/statusを直接保存する

続くCycleで `finalize_sleep_run_sums_step_tokens` と `sleep_step_success_transaction_rolls_back_as_a_unit` を追加し、token集約と出力・checkpoint・step successの原子性をstatus行列から分離して確認する。

### GREEN: 最小実装

`SleepRunStatus::PartialFailure`を追加し、step行を集約してrunを更新するfinalize APIを実装する。併せてPlan 1がDB出力・checkpoint・snapshot・step successを同一transactionで確定するための最小storage境界を設ける。用途のない汎用transaction abstractionは追加しない。

### REFACTOR: 設計の整理

- 重複: status集約規則の単一化
- 命名: caller指定のsuccess/failed更新APIからderive/finalizeへ移行
- 責務: token SUMをruntimeへ漏らさない
- テストの構造的結合: SQL式ではなく全status組合せの結果を検証
- 次の項目へ進める身軽さ: 現行orchestratorを壊さずPlan 1で切替可能か

### テストリスト更新

- 完了: `T7`, `T10`
- 追加: error summaryの最大長・並び順
- 次候補: `T8`

### コミット

`feat: finalize sleep runs from step results`

---

## Step 8: Web/API TDD Cycle - `partial_failure`を公開する

### この Step の目的

既存run一覧APIとWeb UIが新しい集約statusを欠落なく扱う。

### 今回選ぶ項目

- 対象: `T8`
- 選ぶ理由: DBに保存できてもconsumerが未知値で失敗すると監視用途を満たせないため。
- この時点では扱わないこと: step詳細一覧APIや新しいUI画面。

### RED: 失敗する自動テストを書く

- 追加するテスト名: `sleep_runs_api_returns_partial_failure_status`
- Given: `partial_failure`のsleep run
- When: `/api/sleep/runs`相当のhandlerを呼び、Web componentへ渡す
- Then: APIがstatusを返し、UIが既存statusと区別して表示する
- 失敗理由の想定: `SleepRunStatus::from_str`とUI status型が4値しか扱わない

Rust APIのCycle完了後、`sleep_batch_ui_renders_partial_failure_status` を別Cycleで追加する。APIとUIを1つのテストコマンドで完了扱いにしない。

### GREEN: 最小実装

Rust enum/parser、API serialization、Web側type/label/style、既存fixtureを更新する。step詳細APIは今回追加せず、run一覧の集約status対応に限定する。

### REFACTOR: 設計の整理

- 重複: status label/color mapping
- 命名: `partial_failure`を別表記へ変換せずAPI値を統一
- 責務: UIがstatus集約規則を再計算しない
- テストの構造的結合: class名ではなく表示statusを検証
- 次の項目へ進める身軽さ: 将来step詳細を追加できるが今回のscopeを広げていないか

### テストリスト更新

- 完了: `T8`
- 追加: なし
- 次候補: なし

### コミット

`feat: expose partial sleep run failures`

---

## Step 9: 動作確認

- 対象テスト:
  ```bash
  cargo test storage::migration::
  cargo test storage::queries::
  cargo test channels::web::sleep::
  npm test --prefix web -- SleepBatch
  ```
- 全体検証:
  ```bash
  cargo fmt --check
  cargo test
  cargo check
  cargo clippy --all-targets --all-features -- -D warnings
  npm run build --prefix web
  ```
- schema確認:
  - fresh DBがschema v9まで到達する
  - v6 fixtureからv7→v8→v9へ順次移行できる
  - migration再実行でdata/schemaが変化しない
  - `PRAGMA foreign_key_check`が空である
- 文書確認:
  - `docs/db.md`へ4テーブルの責務、DDL、query APIを反映
  - `docs/api.md`へ`partial_failure`を反映
  - `docs/sleep-tables.md`と実装のDDL/status/token規則を照合
- 失敗時に戻るStep: 対応するmigration/query/API Cycleへ戻る

---

## Step 10: PR 作成

- PR タイトル: `feat: Sleep Batchのstep logとcheckpointを追加`
- PR description:
  - 概要: `sleep_run_steps`、`sleep_step_checkpoints`、snapshot制約、run集約finalizeを追加
  - 後続: Plan 1「Sleep Batch実行モデルの正規化」で新storage APIをruntimeへ接続
  - テスト: v6→v9 migration、run/step lifecycle、checkpoint、snapshot制約、run集約、Web status
  - Close対象: なし（Issueが割り当てられた場合はPR作成時に追記）

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
| ---- | ---- | -- |
| `src/storage/migration.rs` | 変更 | schema v7-v9とmigration tests |
| `src/storage/mod.rs` | 変更 | run/step/checkpoint enum・model |
| `src/storage/queries.rs` | 変更 | run作成、step lifecycle、checkpoint、snapshot、finalize API |
| `src/channels/web/sleep.rs` | 変更または影響確認 | `partial_failure` runのAPI test |
| `web/src/**` | 変更または影響確認 | status型・表示・component test |
| `docs/db.md` | 変更 | schema/API仕様 |
| `docs/api.md` | 変更 | sleep run status仕様 |
| `docs/sleep-tables.md` | 変更 | 実装との最終整合 |

---

## コミット分割

1. `feat: add sleep run steps migration` - schema v7
2. `feat: initialize sleep steps with each run` - atomic run/step作成
3. `feat: persist sleep step lifecycle` - modelとquery API
4. `feat: add sleep step checkpoint migration` - schema v8
5. `feat: add composite sleep checkpoint queries` - checkpoint model/API
6. `feat: enforce memory snapshot integrity` - schema v9とsnapshot API整理
7. `feat: finalize sleep runs from step results` - `partial_failure`、token/status集約、transaction境界
8. `feat: expose partial sleep run failures` - API/Web対応
9. `docs: document sleep step storage schema` - DB/API/設計文書

---

## 自動テスト一覧（最低限 18 件）

この一覧はPlan作成時点の最低限であり、最終テスト件数の上限ではない。各行は独立したRedとして実施し、新しい不安が見つかった場合は同じテストリスト項目のCycleを追加する。

### Storage migration（全 7 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T1 | `migration_v6_to_v7_creates_sleep_run_steps` | Step 1 | `cargo test migration_v6_to_v7_creates_sleep_run_steps` |
| T1 | `fresh_database_contains_sleep_run_steps_schema` | Step 1 | `cargo test fresh_database_contains_sleep_run_steps_schema` |
| T4 | `migration_v7_to_v8_creates_validated_sleep_checkpoints` | Step 4 | `cargo test migration_v7_to_v8_creates_validated_sleep_checkpoints` |
| T4 | `sleep_checkpoint_schema_rejects_invalid_step_source_pairs` | Step 4 | `cargo test sleep_checkpoint_schema_rejects_invalid_step_source_pairs` |
| T6 | `migration_v8_to_v9_rebuilds_memory_snapshots_with_constraints` | Step 6 | `cargo test migration_v8_to_v9_rebuilds_memory_snapshots_with_constraints` |
| T6 | `memory_snapshot_constraints_reject_invalid_rows` | Step 6 | `cargo test memory_snapshot_constraints_reject_invalid_rows` |
| T9 | `sleep_storage_migrations_are_idempotent_through_v9` | Step 6 | `cargo test sleep_storage_migrations_are_idempotent_through_v9` |

### Storage queries（全 9 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T2 | `try_create_sleep_run_inserts_four_pending_steps_atomically` | Step 2 | `cargo test try_create_sleep_run_inserts_four_pending_steps_atomically` |
| T2 | `try_create_sleep_run_rolls_back_when_step_initialization_fails` | Step 2 | `cargo test try_create_sleep_run_rolls_back_when_step_initialization_fails` |
| T3 | `sleep_step_lifecycle_persists_terminal_result` | Step 3 | `cargo test sleep_step_lifecycle_persists_terminal_result` |
| T3 | `sleep_step_lifecycle_rejects_invalid_transition` | Step 3 | `cargo test sleep_step_lifecycle_rejects_invalid_transition` |
| T5 | `sleep_checkpoint_preserves_composite_cursor_per_source` | Step 5 | `cargo test sleep_checkpoint_preserves_composite_cursor_per_source` |
| T5 | `sleep_checkpoint_rejects_backward_cursor_update` | Step 5 | `cargo test sleep_checkpoint_rejects_backward_cursor_update` |
| T7 | `finalize_sleep_run_derives_status_matrix` | Step 7 | `cargo test finalize_sleep_run_derives_status_matrix` |
| T7 | `finalize_sleep_run_sums_step_tokens` | Step 7 | `cargo test finalize_sleep_run_sums_step_tokens` |
| T10 | `sleep_step_success_transaction_rolls_back_as_a_unit` | Step 7 | `cargo test sleep_step_success_transaction_rolls_back_as_a_unit` |

### Web/API（全 2 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T8 | `sleep_runs_api_returns_partial_failure_status` | Step 8 | `cargo test sleep_runs_api_returns_partial_failure_status` |
| T8 | `sleep_batch_ui_renders_partial_failure_status` | Step 8 | `npm test --prefix web -- SleepBatch` |

---

## 工数見積もり

| Step | 内容 | 見積もり |
| -- | -- | -- |
| Step 1 | v7 migration | ~100行 |
| Step 2 | atomic run/step作成 | ~100行 |
| Step 3 | step model/lifecycle API | ~220行 |
| Step 4 | v8 migration | ~90行 |
| Step 5 | checkpoint model/query API | ~200行 |
| Step 6 | v9 snapshot rebuild/制約 | ~150行 |
| Step 7 | run集約とtransaction API | ~220行 |
| Step 8 | API/Web status対応 | ~100行 |
| Step 9-10 | 文書、全体検証、PR | ~100行 |
| **合計** |  | **~1,280行** |
