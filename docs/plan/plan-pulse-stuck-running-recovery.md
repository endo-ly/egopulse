# Plan: pulse_runs の running 放置問題の解消

`pulse_runs.status='running'` のまま長時間（〜1ヶ月）放置される問題を、実行中の抜け穴（DB エラー握り潰し・タイムアウト無し・パニック非捕捉）を塞ぎ、起動時の最終防波堤（orphan sweep）を追加して解消する。

> **Note**: 振る舞い（What）は決して変えてはいけないが、より美しい設計があれば実装方法（HOW）だけは変えてもよい。

## 設計方針

- **正常時の振る舞いは一切変えない**。pulse 実行の成功経路・出力経路・永続化経路は現状維持。
- **3 つの独立した故障モード（MECE）を独立した防御層で塞ぐ**:
  - A: DB 更新失敗の握り潰し排除（可観測性向上）
  - B: LLM ハングのタイムアウト打ち切り（実行中の抜け穴）
  - C: パニックの catch（実行中の抜け穴）
  - D: 起動時孤立行回収（最終防波堤／プロセスクラッシュ後の復旧）
- **既存の finalizer 経路を再利用**。B/C は既存の `update_run_failed` 経路に乗せるため、`scheduler.rs` の `Err` ハンドラをそのまま活用する。
- **D のスコープ（重要・訂正）**: D は孤立 `running` 行を `failed` 化して「DB の景観をクリーンアップ」するのみ。**当日分の再実行を可能にする効果はない**。なぜなら `has_pulse_due_run`/`try_create_pulse_run` とも `status` を見ず、`(agent_id, intention_id, due_key)` の行が1件でも残っていれば Duplicate 扱いで INSERT を弾くため。sweep 後も行自体は残り、当日分 due_key の再実行は引続き不可。当日分の再実行解除が必要な場合は本 Plan のスコープ外（別 Issue で `status` を考慮した gate への改修、または failed 行の DELETE を検討）。
- **`PulseTurnGuard` の安全性**: `run_activation` は RAII で `active_turns.end_turn` を Drop 时に呼ぶ。timeout/catch_unwind で打ち切っても guard の Drop は走るため安全。
- **設定化は YAGNI**: タイムアウト値は `const PULSE_ACTIVATION_TIMEOUT: Duration = Duration::from_secs(30 * 60)` で固定（ユーザー指定）。正常実行が実測最大6分のため30分は5倍のマージン。設定ファイルへの露出は行わない（実需要が出たら別 PR）。
- **`UnwindSafe` 境界の注意（Step 3）**: `guard_activation` 内で `AssertUnwindSafe(timeout_fut).catch_unwind()` を使うが、`AssertUnwindSafe` は「プログラマ責任で unwind 安全性を保証」する wrapper。`run_activation` 内で `&AppState` の内部状態を partial update する箇所がある場合、unwind 時に一貫性破壊が起き得る。実装時に `run_activation` のコードパスをレビューし、unwind 時の副作用整合性を確認すること。`PulseTurnGuard` の Drop は必ず走るため `active_turns` の整合性は保証されるが、DB や session の中途状態は別途検討。
- **依存**: `futures::FutureExt::catch_unwind` を使用。`futures` は Tokio ecosystem の標準的依存で既存クレートにも含まれる見込みだが、未確認の場合は `Cargo.toml` を確認して追加する。
- **関連 docs / 既存テスト**:
  - `docs/db.md` の `pulse_runs` 操作リスト → D で `reap_orphaned_pulse_runs` を追記
  - `src/storage/pulse.rs` の既存テスト（AAA パターン）→ D のテストもこれに倣う
  - `src/pulse/output.rs` の `handle_notify` → A の対象
  - `src/pulse/scheduler.rs` の `process_intention` 内 `run_activation` 呼び出し（L208-227）→ B/C の対象
  - `src/runtime/mod.rs` の pulse scheduler 起動（L839-847）→ D の呼び出し元

## TDD 方針

テストリスト項目（`T1`, `T2`, …）と Red で書く自動テスト（`test_name`）を区別する。1 回の Red で追加する自動テストは 1 件のみ。1 つのテストリスト項目に複数ケースが必要な場合は、同じ項目を対象にした Cycle を複数作る。Green では Red のテストを通す最小実装のみに集中し、別ケース対応やリファクタリングを混ぜない。Refactor は全テストが通る状態で設計を整理する。実装中に新しい不安を見つけたら、その場で実装に混ぜずテストリストへ戻し、次の Cycle で扱う。

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 種別 | 既存パターン / 参照元 | 備考 |
| -- | -- | -- | -- |
| `src/pulse/output.rs` | 変更 | `handle_notify` 内 L131/L153/L184 の `.await.ok()` | A: DB 更新失敗を `error!` ログで可視化。上位伝播は現状維持（既に `Err` を返している） |
| `src/pulse/scheduler.rs` | 変更 | `process_intention` 内 L208-227 の `run_activation` 呼び出し | B/C: `run_activation` を timeout + catch_unwind で包むヘルパ関数を新設し、既存の `Err` ハンドラに接続 |
| `src/storage/pulse.rs` | 変更 | `Database::update_pulse_run_*` の既存パターン | D: `reap_orphaned_pulse_runs` を新設。UPDATE 一発で孤立 running を全て failed 化 |
| `src/runtime/mod.rs` | 変更 | L839 pulse scheduler 起動前 | D: pulse scheduler 起動前に `reap_orphaned_pulse_runs` を1回呼ぶ。sweep 行数を info ログ |
| `docs/db.md` | 変更 | `pulse_runs` の操作リスト | D で追加する `reap_orphaned_pulse_runs` の行を追記 |

## テストリスト / 不安リスト

| ID | 観点 | 期待する振る舞い | 優先 | 対応するCycle | 状態 / 今回対象外理由 |
| -- | -- | -- | -- | -- | -- |
| T1 | 異常系 | A: `handle_notify` の3エラーパス（adapter not found / send failed / persistence failed）のいずれかで `update_pulse_run_failed` が `Err` を返した場合、`error!` ログが出力され、呼び出し元には元の `Err` が伝播する | High | Step 1 | 未着手 |
| T2 | 異常系 | B: `guard_activation` がタイムアウト時間を超過した場合、`Err` に変換される（ヘルパ単体・Future 注入） | High | Step 2 | 未着手 |
| T3 | 異常系 | C: `guard_activation` が panic を捕捉して `Err` に変換する（ヘルパ単体・Future 注入） | High | Step 3 | 未着手 |
| T9 | 統合 | B: `process_intention` が timeout で `update_pulse_run_failed` を呼び、`pulse_runs.status='failed'` になる（実経路の保護を検証） | High | Step 2 | 未着手 |
| T10 | 統合 | C: `process_intention` が activation 内 panic で `update_pulse_run_failed` を呼び、`pulse_runs.status='failed'` になる（実経路の保護を検証） | High | Step 3 | 未着手 |
| T4 | 正常系 | D: `reap_orphaned_pulse_runs` は全ての `status='running'` 行を `status='failed'`（`finished_at`・`error_message` 設定済み）に更新し、sweep した行数を返す | High | Step 4 | 未着手 |
| T5 | 境界値 | D: `reap_orphaned_pulse_runs` は `status` が `success/failed/skipped` の行を一切変更しない | High | Step 4 | 未着手 |
| T6 | 空・ゼロ状態 | D: `reap_orphaned_pulse_runs` は running 行が 0 件の場合 0 を返し、エラーにもならない | Medium | Step 4 | 未着手 |
| T7 | 統合 | D: runtime 起動時に `reap_orphaned_pulse_runs` が1回呼ばれ、sweep 行数が info ログに出る | Medium | Step 5（手動確認・自動テスト対象外） | 未着手（手動確認） |
| T8 | 異常系 | A: 正常時（DB 更新成功時）は従来通り `Err` にならず、ログも出ない | Medium | Step 1 | 未着手 |

---

## Step 0: Worktree 作成

- ブランチ名: `fix/pulse-stuck-running-recovery`
- 作成コマンド:
  - `git worktree add ../egopulse-pulse-stuck-running-recovery -b fix/pulse-stuck-running-recovery`
- ※ `worktree-create` skill を使用してもよい

---

## Step 1: output.rs の `.ok()` 廃止 TDD Cycle - DB 更新失敗を可視化（T1/T8）

### この Step の目的

`handle_notify` 内の3つのエラーパス（adapter not found / send failed / persistence failed）で `update_pulse_run_failed` の DB 呼び出し結果を `.ok()` で握り潰している箇所（L131/L153/L184）を、DB エラーを `error!` ログで可視化する形に修正する。

### 今回選ぶ項目

- 対象: `T1`, `T8`
- 選ぶ理由: 最も小さく、可観測性向上という独立した価値があり、後続の B/C/D の設計判断とは独立しているため。
- この時点では扱わないこと: B/C（timeout・catch_unwind）・D（startup sweep）

### RED: 失敗する自動テストを書く

- 追加するテスト名（テストリスト T1）: `handle_notify_logs_error_when_failed_update_db_errors`
- Given: `update_pulse_run_failed` が `Err(StorageError::...)` を返す状況を再現できる形（DB を閉じる／壊す等の既存テストパターンを踏襲）。`handle_notify` に対し channel adapter が見つからない／send 失敗／persistence 失敗のいずれかのエラーパスを踏ませる。
- When: `handle_notify` を実行
- Then:
  - 関数は元の `Err(EgoPulseError::Internal(...))` を返す（上位伝播は変更なし）
  - `error!` ログに pulse_run_id と DB エラー内容が含まれる（`tracing` のテスト用 subscriber で検証）
- 想定される失敗理由: 現状は `.ok()` で握り潰されているため、`error!` ログが出ない

追加するテスト名（テストリスト T8）: `handle_notify_succeeds_silently_when_failed_update_db_succeeds`
- Given: DB が正常
- When: channel adapter 不在等のエラーパスを実行
- Then: `error!` ログが出ず、関数は元の `Err` を返す

> **実装時リスク（T1/T8）**: `Database` は `Mutex<Connection>` で Connection の close API が未公開の可能性がある。その場合は `update_pulse_run_failed` が `Conflict` を返す状況（事前に status を success/failed 化しておく）で代替検証する。また `tracing::error!` ログ検証には `tracing_subscriber` の mock layered subscriber が必要で、既存テストで使われているか確認要。もし難しければ `handle_notify` の adapter-not-found パス＋CAS Conflict の組合せ等に簡略化する。

### GREEN: 最小実装

`output.rs` の3箇所（L131/L153/L184）の `.await.ok()` を外し、`if let Err(update_err) = ... { error!(...) }` でログ出力する。上位への `Err` リターンは現状維持。`tracing::error!` を import に追加。

### REFACTOR: 設計の整理

- 重複: 3箇所のログ出力パターンがほぼ同じ。ヘルパ `log_pulse_finalize_failure(run_id, update_err)` のような private 関数への抽出を検討。ただし3箇所のみで KISS の観点ではインラインでも許容可能。実装時に判断。
- 命名: 変数名 `update_err` と `error_msg`（元のエラー）の混同に注意
- 責務: ロギングのみ。リトライ機能は持たせない（YAGNI）
- テストの構造的結合: `tracing::error!` のログ検証は `tracing_subscriber` の `Vec<AppEvent>` 等の既存パターンがあれば再利用、なければ導入
- 次の項目へ進める身軽さ: T1/T8 が Green になれば B/C/D へ進める

### テストリスト更新

- 完了: `T1`, `T8`
- 追加: なし（実装中に不安が見つかれば追記）
- 次候補: `T2`（B: タイムアウト）

### コミット

`fix(pulse): log DB errors when marking pulse_run as failed`

---

## Step 2: 30 分タイムアウト TDD Cycle - LLM ハングの打ち切り（T2）

### この Step の目的

`run_activation` の呼び出しを `tokio::time::timeout` で包み、30 分を超過した場合に `Err` に変換して既存の `update_run_failed` 経路へ流す。

### 今回選ぶ項目

- 対象: `T2`（Cycle 2-1）→ `T9`（Cycle 2-2）
- 選ぶ理由: B は2層で検証する。Cycle 2-1 で guard 単体の timeout 挙動を最小検証し、Cycle 2-2 で `process_intention` 経由の実行経路保護を検証する。後者を抜かすと「guard を実装したが scheduler 側を差し替え忘れた」時に T2 だけ green になり再発に気づけない。
- この時点では扱わないこと: C（panic）・D（sweep）

### Cycle 2-1: guard_activation 単体（T2）

#### RED: 失敗する自動テストを書く

- 追加するテスト名: `guard_activation_returns_err_on_timeout`
- Given: 永久に完了しない Future（`std::future::pending::<Result<ActivationResult, EgoPulseError>>()`）と短縮 duration（例: 100ms）
- When: `guard_activation(pending_future, short_duration)` を実行
- Then: `Err(EgoPulseError::Internal("pulse activation timeout after ..."))` を返す
- 想定される失敗理由: `guard_activation` 未実装のためコンパイルエラー

#### GREEN: 最小実装

`scheduler.rs` に **Future を受け取る純粋な guard helper** `guard_activation<Fut>(fut: Fut, timeout: Duration) -> Result<ActivationResult, EgoPulseError>` を新設する（Step 3 で catch_unwind を重ねる）。Cycle 2-1 では内部で `tokio::time::timeout(timeout, fut)` を包み、`Err(Elapsed)` を `EgoPulseError::Internal` へ変換するのみ。`const PULSE_ACTIVATION_TIMEOUT: Duration = Duration::from_secs(30 * 60)` をモジュール直下に定義。

> **設計判断**: guard を Future 注入可能な形に切り出すことで、Cycle 2-1 で `pending()` を直接渡して検証可能にする。`run_activation(...)` 専用ヘルパにするとテスト注入経路がなくなり TDD が成立しないため。

#### REFACTOR: 設計の整理

- 重複: なし
- 命名: `PULSE_ACTIVATION_TIMEOUT` はモジュール直下の const として配置
- 責務: タイムアウトのみ。panic 回復は Step 3 で追加
- テストの構造的結合: ヘルパが `Duration` を受け取ることで本番値とテスト値を分離
- 次の項目へ進める身軽さ: Green になれば Cycle 2-2 へ

### Cycle 2-2: process_intention 接続（T9）

#### RED: 失敗する自動テストを書く

- 追加するテスト名: `process_intention_marks_run_failed_on_activation_timeout`
- Given: `process_intention` を実行できる AppState を構築（`src/pulse/scheduler.rs` の既存テストヘルパ `build_pulse_state` 等を流用）。LLM は「永続的に応答しないモック」（`pending()` を返す LLM provider、または既存のモック機構で代用）。**`pulse_runs` レコードは未作成**（gate が `Allow` を返す状態。`try_create_pulse_run` は `process_intention` 内で呼ばれるため、テスト側で事前 INSERT しない）。
- When: テスト全体を短い wall-clock timeout（例: `tokio::time::timeout(Duration::from_secs(3), process_intention(...))`）で包んで実行。未実装時にテストランナーを即座に失敗に落とすための仕掛け。
- Then: `pulse_runs.status='failed'`、`error_message` に timeout を示す文言。外側 wall-clock timeout は発火せず、`process_intention` 本体が内部タイムアウトで戻る。
- 想定される失敗理由: Cycle 2-1 の guard が `process_intention` に接続されていない場合、`run_activation` が永続 pending となり外側 wall-clock timeout が発火してテストが失敗する（`Elapsed`）。これにより「差し替え忘れ」を確実に検出できる。

#### GREEN: 最小実装

`process_intention` の L208-227 の `runner::run_activation(...)` 直接呼び出しを `guard_activation(runner::run_activation(state, agent_id_str, &capsule, &home_surface), PULSE_ACTIVATION_TIMEOUT)` に変更。既存の `Err` ハンドラ（L218 `update_run_failed`）はそのまま再利用。

> **実装時の調査事項**: `src/pulse/scheduler.rs` の既存テストモジュール（L320 以降）に `process_intention` を回すヘルパがあるか確認。無ければ、永続 `pending` を返す最小のモック LLM provider を用意する。

> **実装時リスク（T9/T10）**: `state.llm_for_context(&context)` が返す型が trait object か concrete 型か未確認。concrete 型の場合、T9/T10 の実現には `LLM provider` を抽象化する trait の導入、またはテスト専用のモック AppState 構築ヘルパの新設が必要で、Step 2/3 の工数が膨らむ可能性がある。実装最初に `state.llm_for_context` の型と既存 mock の有無を調査し、必要に応じて Plan を更新する。

#### REFACTOR: 設計の整理

- guard 呼び出しのシンプルさ
- ログ出力の有無（timeout 発火時に `warn!` を出すか）

#### テストリスト更新

- 完了: `T2`, `T9`
- 追加: なし
- 次候補: `T3`（C: catch_unwind）

### コミット

`feat(pulse): add 30min timeout to pulse activation`

---

## Step 3: catch_unwind によるパニック回復 TDD Cycle（T3/T10）

### この Step の目的

Step 2 で導入した guard に `FutureExt::catch_unwind` を重ね、`run_activation` 内の panic を捕捉して `Err` に変換し、既存の `update_run_failed` 経路へ流す。Step 2 と同じく guard 単体（T3）と `process_intention` 経由（T10）の2 Cycle を回す。

### 今回選ぶ項目

- 対象: `T3`（Cycle 3-1）→ `T10`（Cycle 3-2）
- 選ぶ理由: Step 2 と対称。Cycle 3-1 で guard 単体の panic 回復を検証し、Cycle 3-2 で process_intention 経由の保護を検証する。
- この時点では扱わないこと: D（sweep）

### Cycle 3-1: guard_activation panic 回復（T3）

#### RED: 失敗する自動テストを書く

- 追加するテスト名: `guard_activation_catches_panic`
- Given: panic する Future（`async { panic!("boom") }` を `ActivationResult` 型に合わせたもの）と十分長い duration
- When: `guard_activation(panicking_future, long_duration)` を実行
- Then: `Err(EgoPulseError::Internal("pulse activation panicked: ..."))` を返し、panic は伝播しない
- 想定される失敗理由: catch_unwind 未実装のため panic が伝播しテストプロセスが abort

#### GREEN: 最小実装

Step 2 で導入した `guard_activation<Fut>` の内部で、`tokio::time::timeout(timeout, fut)` を包んだ後、その外側を `std::panic::AssertUnwindSafe(guarded_fut).catch_unwind()` で包む（`futures::FutureExt`）。Unwind 時は panic メッセージを取り出して `EgoPulseError::Internal("pulse activation panicked: ...")` へ変換。`futures` クレート依存が未存在なら `Cargo.toml` に追加する。

#### REFACTOR: 設計の整理

- 重複: timeout・catch_unwind の2層ラップは `guard_activation` 内部にカプセル化
- 命名: `guard_activation` は Step 2 から一貫して使用（timeout のみ→timeout+panic 回復）。ヘルパ名を変えずに機能拡張する
- 責務: 実行の保護のみ。ビジネスロジックは持たない
- テストの構造的結合: Cycle 2-1 は timeout（`pending()`）・Cycle 3-1 は panic（`async { panic!() }`）と独立。両者が同一ヘルパで検証できる構造
- 次の項目へ進める身軽さ: Green になれば Cycle 3-2 へ

### Cycle 3-2: process_intention panic 回復（T10）

#### RED: 失敗する自動テストを書く

- 追加するテスト名: `process_intention_marks_run_failed_on_activation_panic`
- Given: `process_intention` を実行できる AppState（Cycle 3-1 のヘルパを流用）。LLM は「内部で panic するモック」（既存のモック機構で代用、または最小 panic provider を新設）。**`pulse_runs` レコードは未作成**（gate が `Allow` を返す状態）。
- When: `process_intention` を実行（panic がテストプロセスを巻き込まないよう、テスト側は `tokio::time::timeout` の外側ガードもしくは `#[tokio::test]` の panic キャッシュ機構で保護）
- Then: `pulse_runs.status='failed'`、`error_message` に panic を示す文言。panic は伝播せず、テストプロセスは正常終了。
- 想定される失敗理由: Step 3 Cycle 3-1 の catch_unwind が `process_intention` に接続されていない場合、panic が伝播しテストプロセスが abort する

#### GREEN: 最小実装

Step 2 Cycle 2-2 で `process_intention` は既に guard 経由で呼ぶよう変更されているため、Cycle 3-1 で guard に catch_unwind を追加した時点で特別な作業は不要なはず（T10 は自動的に green になる想定）。もし green にならなければ、guard の接続箇所を見直す。

#### REFACTOR: 設計の整理

- guard 呼び出しの最終形: timeout + catch_unwind を1層で包む
- ログ出力の統一（panic 時にも `warn!` を出すか）

#### テストリスト更新

- 完了: `T3`, `T10`
- 追加: なし
- 次候補: `T4`（D: sweep 正常系）

### コミット

`feat(pulse): recover from pulse activation panics via catch_unwind`

---

## Step 4: `reap_orphaned_pulse_runs` TDD Cycle - 起動時孤立行回収（T4/T5/T6）

### この Step の目的

`Database` に `reap_orphaned_pulse_runs(&self) -> Result<usize, StorageError>` を新設し、全ての `status='running'` 行を `status='failed'`（`finished_at`・`error_message='orphaned: process restarted'` 設定済み）に更新して、sweep した行数を返す。

### 今回選ぶ項目

- 対象: `T4`, `T5`, `T6`
- 選ぶ理由: 最終防波堤。3つの観点（正常系・境界・ゼロ状態）をこの Cycle で扱う。各観点で Red 自動テスト 1 件ずつ追加する（合計3 Cycle 相当）。
- この時点では扱わないこと: runtime 起動シーケンスへの組み込みは次 Step（動作確認）

※ 本 Step は Red を3回（T4→T5→T6 の順）に分けて回す。各 Red で自動テスト 1 件を追加し、Green/Refactor を経てから次の観点へ。

### RED-1: 失敗する自動テストを書く（T4 正常系）

- 追加するテスト名: `reap_orphaned_pulse_runs_marks_running_as_failed`
- Given: 2件の running 行と1件の success 行を作成
- When: `reap_orphaned_pulse_runs` を実行
- Then:
  - 戻り値は `Ok(2)`
  - 元 running 行の `status='failed'`、`finished_at` が設定済み、`error_message` に orphaned 由来の文言が含まれる
- 想定される失敗理由: 関数未実装のためコンパイルエラー

### GREEN-1: 最小実装

`storage/pulse.rs` の `impl Database` に `reap_orphaned_pulse_runs` を追加。SQL は `UPDATE pulse_runs SET status='failed', finished_at=?, error_message=? WHERE status='running'`。`finished_at` は `chrono::Utc::now().to_rfc3339()`、`error_message` は `"orphaned: process restarted"`。戻り値は `conn.execute(...)?` の変更行数。

### RED-2（T5 境界）/ GREEN-2

- テスト名: `reap_orphaned_pulse_runs_preserves_terminal_status`
- Given: success/failed/skipped の各1件を作成
- When: `reap_orphaned_pulse_runs` を実行
- Then: 戻り値 `Ok(0)`、各 row の status・finished_at・error_message は一切変更されない

### RED-3（T6 空・ゼロ状態）/ GREEN-3

- テスト名: `reap_orphaned_pulse_runs_returns_zero_on_empty`
- Given: running 行 0 件の DB
- When: `reap_orphaned_pulse_runs` を実行
- Then: 戻り値 `Ok(0)`、エラーにならない

### REFACTOR: 設計の整理

- 重複: なし。既存の `update_pulse_run_*` と比べると where 句が `id` ではなく `status` 一括。CAS（`status='running'` check）の意図は同じ
- 命名: `reap_orphaned_pulse_runs` は `docs/db.md` の既存命名（`try_create_*` / `update_*` / `has_*`）と整合
- 責務: 単一 UPDATE の発行のみ。トランザクションは不要（単文のため）
- テストの構造的結合: `storage/pulse.rs` 既存テスト（AAA パターン）に準拠。`get_pulse_run` は `#[cfg(test)]` で使えるため、検証に利用可能
- 次の項目へ進める身軽さ: Green になれば runtime 起動シーケンスへの組み込み（Step 5）

### テストリスト更新

- 完了: `T4`, `T5`, `T6`
- 追加: なし
- 次候補: `T7`（統合: runtime 起動時呼び出し）

### コミット

`feat(runtime): reap orphaned pulse_runs on startup`

---

## Step 5: 動作確認（T7 含む）

### 全テスト通過コマンド

```bash
cargo fmt --check
cargo check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

### Lint / フォーマット / 型チェック

上記に同じ。`#[allow(dead_code)]` は使用禁止（AGENTS.md）。`reap_orphaned_pulse_runs` が runtime から呼ばれるため dead_code 警告は出ないはず。

### 統合確認（T7）

- runtime 起動時（`runtime/mod.rs` L839 付近、pulse scheduler 起動前）に `reap_orphaned_pulse_runs` を1回呼ぶ
- 戻り値の行数を `info!("reaped {} orphaned pulse_runs on startup", n)` で出力
- 失敗時は `warn!` で起動は継続（sweep 失敗でプロセスを落とさない）
- 手動確認: 既存の孤立 running を残した DB で起動し、sweep されることを確認（optional）

### 失敗時に戻る Step

- テスト失敗 → 該当 Step の RED/GREEN を見直し
- clippy 警告 → 該当 Step の REFACTOR を見直し
- ビルド失敗 → シグネチャ・import を見直し

---

## Step 6: Plan・仕様書との自己チェック

実装完了後にこの Plan と関連仕様書（`docs/db.md`、`docs/architecture.md` の pulse 関連）を最初から読み直し、実装・自動テスト・文書が要求した振る舞いと一致しているかを照合する。未実装、過剰実装、テスト不足、仕様書との齟齬を見つけた場合は、該当する TDD Cycle へ戻って修正し、動作確認を再実行してからこの Step を完了する。

- Plan のテストリスト（T1-T6, T8-T10 自動 / T7 手動）と各 Cycle が完了条件を満たしている
- `docs/db.md` の `pulse_runs` 操作リストに `reap_orphaned_pulse_runs` が追記されている
- 実装中に変更した設計判断（例: ヘルパ名の最終形）が Plan と docs へ反映されている
- 変更ファイル一覧、コミット分割、自動テスト一覧が実際の変更と一致している
- A/B/C/D いずれも「正常時の振る舞いを変えていない」ことをコード差分で再確認

---

## Step 7: PR 作成

- PR タイトル: `pulse_runs の running 放置問題の解消`
- PR description（日本語）:
  - **概要**: `pulse_runs` が `status='running'` のまま長時間（実例: 88分・31.5日）放置される問題の対処。実行中の抜け穴（DB エラー握り潰し・タイムアウト無し・パニック非捕捉）を塞ぎ、起動時の孤立行回収を追加する。
  - **変更点**:
    - A: `output.rs` の `.ok()` 廃止、DB エラーを `error!` ログで可視化
    - B: `run_activation` 呼び出しを 30 分タイムアウトで保護
    - C: 同呼び出しを `catch_unwind` でパニック保護
    - D: 起動時に孤立 `running` 行を一括 `failed` 化する `reap_orphaned_pulse_runs` を追加
  - **設計メモ**: A/B/C は実行中の抜け穴を塞ぐ防御層。D は孤立 running 行の景観クリーンアップ（プロセスクラッシュ後の回収）。当日分 due_key の再実行解除は本 PR のスコープ外（`has_pulse_due_run` が status 不問で行存在のみで弾くため、sweep 後も行が残る限り再実行不可。別 Issue 検討）
  - **テスト**: T1/T2/T3/T4/T5/T6/T8/T9/T10（自動・全件通過）+ T7（手動確認）。詳細は `docs/plan/plan-pulse-stuck-running-recovery.md` 参照
  - ** Close #<issue-number>**（該当 Issue がある場合）

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
| ---- | ---- | -- |
| `src/pulse/output.rs` | 変更 | L131/L153/L184 の `.await.ok()` を `if let Err(update_err) = ... { error!(...) }` へ置換。`tracing::error!` の import 追加 |
| `src/pulse/scheduler.rs` | 変更 | `process_intention` 内 L208-227 の `run_activation` 呼び出しを新設ヘルパ `guard_activation` 経由に変更。`const PULSE_ACTIVATION_TIMEOUT` 追加。ヘルパは Future を受け取る純粋な guard として実装 |
| `src/storage/pulse.rs` | 変更 | `impl Database` に `reap_orphaned_pulse_runs(&self) -> Result<usize, StorageError>` を追加 + 単体テスト3件 |
| `src/runtime/mod.rs` | 変更 | pulse scheduler 起動前（L839 付近）に `reap_orphaned_pulse_runs` 呼び出しを追加 + `info!` ログ |
| `Cargo.toml` | 変更（必要な場合のみ） | `futures` 依存が未存在なら追加 |
| `docs/db.md` | 変更 | `pulse_runs` の操作リストに `reap_orphaned_pulse_runs` を追記 |

---

## コミット分割

1. `fix(pulse): log DB errors when marking pulse_run as failed` - `src/pulse/output.rs`（A: T1/T8）
2. `feat(pulse): add 30min timeout to pulse activation` - `src/pulse/scheduler.rs`（B: T2/T9）
3. `feat(pulse): recover from pulse activation panics via catch_unwind` - `src/pulse/scheduler.rs` / `Cargo.toml`（C: T3/T10）
4. `feat(runtime): reap orphaned pulse_runs on startup` - `src/storage/pulse.rs` / `src/runtime/mod.rs` / `docs/db.md`（D: T4/T5/T6 自動 + T7 手動確認）

※ B/C は同じファイル・同じヘルパへの拡張だが、機能的理由（タイムアウト・パニック）が独立しているため別コミット。順序は B→C の直列依存。

---

## 自動テスト一覧（全 9 件）

この一覧は Plan 作成時点で必要と判断した最低限の予定であり、最終テスト件数の上限ではない。実装中に追加された不安には、テストリストと TDD Cycle を追加して対応する。

> **T7 は自動テスト対象外**: runtime 起動シーケンス全体を要する統合観点であり、ユニットテストとして切り出すには AppState 構築や channel 起動が深く絡みすぎている。Step 5 で「孤立 running を残した DB で起動し、sweep ログと DB 状態で確認」する手動検証に振り向ける。これでも runtime/mod.rs への hook 忘れを検出できないリスクは残るため、Step 6 の自己チェックでコード差分に `reap_orphaned_pulse_runs` 呼び出しが含まれていることを必ず目視確認する。

### `pulse` モジュール（全 6 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T1 | `handle_notify_logs_error_when_failed_update_db_errors` | Step 1 | `cargo test --package egopulse handle_notify_logs_error` |
| T8 | `handle_notify_succeeds_silently_when_failed_update_db_succeeds` | Step 1 | `cargo test --package egopulse handle_notify_succeeds_silently` |
| T2 | `guard_activation_returns_err_on_timeout` | Step 2 | `cargo test --package egopulse guard_activation_returns_err_on_timeout` |
| T9 | `process_intention_marks_run_failed_on_activation_timeout` | Step 2 | `cargo test --package egopulse process_intention_marks_run_failed_on_activation_timeout` |
| T3 | `guard_activation_catches_panic` | Step 3 | `cargo test --package egopulse guard_activation_catches_panic` |
| T10 | `process_intention_marks_run_failed_on_activation_panic` | Step 3 | `cargo test --package egopulse process_intention_marks_run_failed_on_activation_panic` |

### `storage` モジュール（全 3 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T4 | `reap_orphaned_pulse_runs_marks_running_as_failed` | Step 4 | `cargo test --package egopulse reap_orphaned_pulse_runs_marks_running` |
| T5 | `reap_orphaned_pulse_runs_preserves_terminal_status` | Step 4 | `cargo test --package egopulse reap_orphaned_pulse_runs_preserves_terminal` |
| T6 | `reap_orphaned_pulse_runs_returns_zero_on_empty` | Step 4 | `cargo test --package egopulse reap_orphaned_pulse_runs_returns_zero_on_empty` |

※ T7（統合）は runtime 起動シーケンス全体を要するため自動テスト対象外。Step 5 で手動確認する。

---

## 工数見積もり

| Step | 内容 | 見積もり |
| -- | -- | -- |
| Step 0 | Worktree 作成 | ~5 分 |
| Step 1 | A: output.rs `.ok()` 廃止 + テスト2件（T1/T8） | ~40 行 |
| Step 2 | B: timeout guard + process_intention 接続 + テスト2件（T2/T9） | ~80 行 |
| Step 3 | C: catch_unwind 拡張 + process_intention 検証 + テスト2件（T3/T10） | ~70 行 |
| Step 4 | D: `reap_orphaned_pulse_runs` + テスト3件（T4/T5/T6） + runtime 組込 | ~80 行 |
| Step 5 | 動作確認（fmt/check/clippy/test）+ T7 手動確認 | ~15 分 |
| Step 6 | Plan・仕様書との自己チェック | ~10 分 |
| Step 7 | PR 作成 | ~10 分 |
| **合計** | | **~270 行 + 調査・検証時間** |
