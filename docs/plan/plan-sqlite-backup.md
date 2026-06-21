# Plan: SQLite DB バックアップ機構の追加

WAL 運用中の EgoPulse 本番 DB に対し、`VACUUM INTO` で一貫性スナップショットを取得するバックアップ機構を追加する。起動時（マイグレーション前）と定期実行（デフォルト週1）の2経路を用意し、`~/.egopulse/runtime/backups/` 配下に世代管理付きで保存する。CLI は追加せず、復元は `cp` ベースの手動手順を docs に残す。

> **Note**: 振る舞い（What）は決して変えてはいけないが、より美しい設計があれば実装方法（HOW）だけは変えてもよい。

## 設計方針

- **バックアップ方式は `VACUUM INTO`**: SQL 一行で一貫性スナップショットを取得できる。WAL モードで稼働中でも schema lock を一瞬取るだけであり、夜間実行なら書き込みブロックは無視できる。Online Backup API はコード量が増える割にメリット薄。
- **保存先は `runtime/` 配下（`~/.egopulse/runtime/backups/`）**: 本番 DB（`runtime/egopulse.db`）と同階層に置くことで、`ls runtime/` で本番もバックアップも一覧でき、運用上の発見性が高い。既存のメモリファイルバックアップパターン（`memory.backup-{uuid}/` を memory/ と同階層に置く）とも整合。**設定で変更はさせず固定（YAGNI）**。ディスク故障対策が必要な場合は別途 rsync / rclone 等で別物理ディスクへ同期する運用をユーザー責任で行う（docs に例を記載）。
- **2 経路のバックアップ**: (1) 起動時（マイグレーション前 = 一番危険な瞬間の保険）、(2) 定期（誤操作・論理破壊対策）。両者とも同じ `run_backup` 関数を呼ぶ。
- **世代管理は件数のみ**: 直近 `max_generations` 件（デフォルト12）を保持、超過分は名前（=タイムスタンプ）降順で古いものから削除。GFS（日/週/月）は導入しない（YAGNI）。
- **復元は CLI 未提供**: `egopulse db restore` 等のサブコマンドは作らない。systemd サービスが稼働中に CLI から DB を置き換えるのは安全でないため。`docs/db.md` に `systemctl stop → cp → systemctl start` の手順を記載する。
- **スケジュール計算は純粋関数**: 既存 `sleep/scheduler.rs` の `next_scheduled_run` と同じ設計。`compute_next_backup_run(config, tz, now, last_run_at) -> Option<DateTime<Utc>>` をテスト可能な純粋関数として切り出す。`chrono_tz::Tz` で DST gap/fold を既存パターン通りに処理。
- **`interval_days` の意味**: 「最後の定期実行から N 日経過した後の、次の HH:MM」。`last_backup_at` を `db_meta` テーブルの `backup_last_run` キーに保存し、次回実行時刻の算出に使う。レコードが無い（初回）場合は「今日の HH:MM（既に過ぎていれば明日）」を返す。
- **起動時バックアップと定期バックアップは独立**: 起動時バックアップは `Database::new_with_backup()` 内で同期的に1回だけ実行し、`db_meta.backup_last_run` は更新**しない**（起動時バックアップは定期サイクルとは無関係のため）。定期バックアップ実行時にのみ `backup_last_run` を更新する。
- **整合性チェック**: バックアップ作成直後に `PRAGMA integrity_check` を実行し、`ok` 以外なら `warn!` ログ出力＋当該バックアップファイルを削除（壊れたファイルを残しても価値がないため）。整合性チェック失敗でプロセス全体を落とすことはしない（best-effort）。
- **`Database::new()` のシグネチャは変更しない**: 既存 `Database::new(db_path)` の呼び出し元が **21 箇所・16 ファイル**に及ぶため（`runtime/mod.rs:293`, `tools/agent_send.rs:248/460/510/597`, `storage/migration.rs:652/695/798`, `pulse/capsule.rs:265`, `pulse/output.rs:446`, `pulse/scheduler.rs:441`, `channels/web/mod.rs:600`, `sleep/event_extraction.rs:535`, `sleep/orchestrator.rs:1910`, `storage/chat.rs:534`, `storage/episode.rs:422`, `storage/mod.rs:631`, `storage/pulse.rs:310`, `storage/sleep.rs:982`, `storage/tool.rs:142`, `test_util.rs:92`）。新設 `Database::new_with_backup(db_path, &BackupSettings) -> Result<Self, StorageError>` を追加する。既存の `Database::new` 呼び出しは維持しつつ、`runtime/mod.rs` だけ `Database::new_with_backup` に切り替える。起動時バックアップ機能は `Database::new_with_backup` 経由でのみ有効になる。
- **`pub(crate)` API 設計**: `BackupSettings`（`enabled: bool`, `dest_dir: PathBuf`, `max_generations: u32`, `tz: String`, `now: DateTime<Utc>`）は `runtime` → `storage` への境界データ。`Config` に直接依存しないことでテスト容易性を確保。`now` を含めることで起動時バックアップのテストで時刻を注入可能。
- **定期 scheduler は `Clock` を DI 可能にする**: `run_backup_scheduler_loop(state)` は `run_backup_scheduler_loop_with_clock(state, clock)` の thin wrapper。`pub(crate) trait Clock: Send + Sync { fn now(&self) -> DateTime<Utc>; }` を導入し、本番は `RealClock`、テストは `MockClock`。**`Clock` は `Arc<dyn Clock + Send + Sync>` で共有する**（値渡し `impl Clock` ではテストから `advance` できないため）。`MockClock` は `Mutex<DateTime<Utc>>`（`RefCell` は別 task から触れないため使わない）を内部持ち `advance()` で進められる。これにより Step 7 T23 で HH:MM 粒度の設定のまま deterministic な scheduler テストが可能になる。`tokio::time::sleep` は実時間進行するため、T23 では短い実時間 sleep で進行させつつ `mock_clock` も並行で進める（詳細は Step 7 で記述）。
- **設定の round-trip 永続化**: `src/config/persist.rs` の `SerializableConfig` に `db: Option<SerializableDb>` フィールドを追加し、`SerializableDb`/`SerializableBackup` を定義する。`db.backup.*` 設定が save → load で失われないようにする。既存の `sleep_batch`/`web_fetch` のパターンに従う。
- **既存パターンの再利用**:
  - `sleep/scheduler.rs` の `next_scheduled_run` / `run_scheduler_loop` / `parse_hhmm` / `try_date` / `resolve_gap` の構造を踏襲
  - `config/types.rs` の `SleepBatchConfig` + `Default` impl + `scheduler_enabled()` の構造を踏襲
  - `storage/mod.rs` の `call_blocking` ラッパで `spawn_blocking` 経由で DB 操作を行う（VACUUM は blocking API）
- **関連 docs / 既存テスト**:
  - `docs/db.md`（最終節に §5 バックアップ・復元 を追加）
  - `docs/config.md`（`db.backup` セクションを追加）
  - `src/storage/mod.rs:553` の `Database::new`（新設 `Database::new_with_backup` の参照元。起動時バックアップのフックポイント）
  - `src/runtime/mod.rs:830-857` の scheduler spawn 群（定期バックアップ scheduler のフックポイント）
  - `src/sleep/scheduler.rs` の `next_scheduled_run` と `run_scheduler_loop`（実装テンプレート）

## TDD 方針

テストリスト項目（`T1`, `T2`, …）と Red で書く自動テスト（`test_name`）を区別する。1 回の Red で追加する自動テストは 1 件のみ。1 つのテストリスト項目に複数ケースが必要な場合は、同じ項目を対象にした Cycle を複数作る。Green では Red のテストを通す最小実装のみに集中し、別ケース対応やリファクタリングを混ぜない。Refactor は全テストが通る状態で設計を整理する。実装中に新しい不安を見つけたら、その場で実装に混ぜずテストリストへ戻し、次の Cycle で扱う。

スケジュール純粋関数は `sleep/scheduler.rs` のテスト群（`next_run_*`）と同じ形式で `chrono::DateTime<Utc>` を使った deterministic なテストにする。`run_backup` は `tempfile::TempDir` 配下の DB に対する統合テストとする。

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 種別 | 既存パターン / 参照元 | 備考 |
| -- | -- | -- | -- |
| `src/storage/backup.rs` | **新規** | `src/storage/mod.rs` の `call_blocking` / `src/storage/migration.rs` | `run_backup` / `prune_old_backups` / `generate_backup_filename` / `compute_next_backup_run` を置く純粋関数ヘビーのモジュール |
| `src/storage/mod.rs` | 変更 | L553-590 の `Database::new` | サブモジュール宣言 `mod backup;` を追加。**`Database::new` のシグネチャは変更しない**（既存21箇所の呼び出し元を温存）。新規 `Database::new_with_backup(db_path, &BackupSettings)` を追加し、`initialize_database_file` 後・`run_migrations` 前に起動時バックアップを実行してから `Database::new` 相当の pool 構築 + migration を行う |
| `src/config/types.rs` | 変更 | L267-296 の `SleepBatchConfig` | `BackupConfig`（`enabled`, `interval_days`, `time`, `max_generations`）と `DatabaseConfig`（`backup: BackupConfig`）を追加。`Default` と `scheduler_enabled()` を実装 |
| `src/config/resolve.rs` | 変更 | L455-460 の `default_state_root` | `default_backup_dir()`（=`state_root/runtime/backups/`）を追加。`Config::backup_dir()` 解決メソッドを追加 |
| `src/config/loader.rs` | 変更 | L16 の import 群 | `DatabaseConfig`, `BackupConfig` を追加で import。`db:` セクションの parse 処理を追加 |
| `src/config/persist.rs` | 変更 | L94-119 の `SerializableConfig`, L73-86 の `SerializableSleepBatch` を参考 | `SerializableDb`（`backup: SerializableBackup`）と `SerializableBackup`（`enabled`, `interval_days`, `time`, `max_generations`）を追加。`SerializableConfig` に `db: Option<SerializableDb>` を追加。`From<&Config>` 等価変換ロジックを更新 |
| `src/runtime/backup_scheduler.rs` | **新規** | `src/sleep/scheduler.rs` | `run_backup_scheduler_loop` / `run_backup_scheduler_loop_with_clock` / `run_periodic_backup_once` / `Clock` trait（`Send + Sync`） / `RealClock` / `MockClock`（テスト用、`Mutex<DateTime<Utc>>` を内部保持、`Arc<dyn Clock>` で共有） |
| `src/runtime/mod.rs` | 変更 | L830-857 の scheduler spawn 群 | `state.config.db.backup.scheduler_enabled()` が true のとき `run_backup_scheduler_loop` を spawn する handle を追加 |
| `docs/db.md` | 変更 | 最終節 §4 の後 | §5「バックアップ・復元」を追加。命名規則・保存先（`~/.egopulse/runtime/backups/`）・`db.backup.*` 設定への参照・復元手順（systemctl stop → cp → rm wal/shm → systemctl start）。ディスク故障対策としての別物理ディスク同期（rsync/rclone）の運用例も併記 |
| `docs/config.md` | 変更 | §2.7 `sleep_batch` 等のセクション | `db.backup` セクションを追加。各キーの意味・デフォルト値・例を記載 |

## テストリスト / 不安リスト

| ID | 観点 | 期待する振る舞い | 優先 | 対応するCycle | 状態 / 今回対象外理由 |
| -- | -- | -- | -- | -- | -- |
| T1 | 正常系 | `generate_backup_filename(now)` が `now` を UTC ではなく**設定 TZ** で解釈した `egopulse-YYYYMMDD-HHMMSS.db` を返す（夜間実行で日付が JST 基準になるよう） | High | Step 1 | 未着手 |
| T2 | 正常系 | `run_backup(db, dest_dir, tz, now, max_generations)` が `dest_dir/egopulse-YYYYMMDD-HHMMSS.db` を作成する | High | Step 2 | 未着手 |
| T3 | 正常系 | バックアップファイルは正当な SQLite DB として開け、元 DB の全テーブル・行が含まれる（VACUUM INTO で内容がコピーされている） | High | Step 2 | 未着手 |
| T4 | 正常系 | `run_backup` は `PRAGMA integrity_check` を実行し `ok` なら正常終了する | High | Step 2 | 未着手 |
| T5 | 異常系 | `PRAGMA integrity_check` が `ok` 以外を返す場合、`warn!` ログを出し、作成したバックアップファイルを削除する | Medium | Step 2 | 未着手 |
| T6 | 正常系 | `prune_old_backups(dir, max_generations)` は `max_generations` を超える分を名前降順で古いものから削除する | High | Step 3 | 未着手 |
| T7 | 境界値 | `prune_old_backups` はファイル数が `max_generations` 以下なら一切削除しない | High | Step 3 | 未着手 |
| T8 | 空・ゼロ状態 | `prune_old_backups` はディレクトリが存在しない・空の場合でもエラーにならず 0 件削除を返す | Medium | Step 3 | 未着手 |
| T9 | 境界値 | `prune_old_backups` は `egopulse-*.db` パターンに一致しないファイル（例: ユーザーが手動で置いたメモ）を削除対象から除外する | Medium | Step 3 | 未着手 |
| T10 | 正常系 | `compute_next_backup_run(config, tz, now, None)`（初回）は今日の HH:MM が未来なら今日、過ぎていれば明日の HH:MM を返す | High | Step 4 | 未着手 |
| T11 | 正常系 | `compute_next_backup_run(config, tz, now, Some(last_run))` は `last_run + interval_days` 日後の HH:MM を返す（`now` が既にそれを過ぎている場合はさらに次の日） | High | Step 4 | 未着手 |
| T12 | 異常系 | `compute_next_backup_run` は `config.enabled=false` なら `None` を返す | High | Step 4 | 未着手 |
| T13 | 異常系 | `compute_next_backup_run` は `time` が不正フォーマット（例: `"25:99"`, `"abc"`）なら `None` を返す | Medium | Step 4 | 未着手 |
| T14 | 境界値 | `compute_next_backup_run` は DST gap（存在しないローカル時刻）を次の有効時刻へ移動する（`sleep/scheduler.rs` の `resolve_gap` と同挙動） | Medium | Step 4 | 未着手 |
| T15 | 境界値 | `compute_next_backup_run` は DST fold（2 回発生するローカル時刻）で最早瞬間を使用する（`sleep/scheduler.rs` の `try_date` と同挙動） | Medium | Step 4 | 未着手 |
| T16 | 正常系 | `Config` に `db:` セクションが無い場合、`BackupConfig::default()` が適用される（`enabled=true, interval_days=7, time="03:00", max_generations=12`） | High | Step 5 | 未着手 |
| T17 | 異常系 | `db.backup.enabled: false` の場合、`scheduler_enabled()` が false を返す | High | Step 5 | 未着手 |
| T18 | 境界値 | `db.backup.interval_days: 0` や `max_generations: 0` は設定読み込み時にバリデーションエラー（`ConfigError`）になる | Medium | Step 5 | 未着手（実装時判断: clippy 的には `u32::MAX` 許容でも実用上 0 は無効。設定バリデーションで弾く） |
| T19 | 正常系 | `Database::new_with_backup(db_path, backup_settings)` で DB ファイルが既存かつ `enabled=true` のとき、マイグレーション前にバックアップファイルが作成される | High | Step 6 | 未着手 |
| T20 | 空・ゼロ状態 | `Database::new_with_backup(db_path, backup_settings)` で DB ファイルが未存在（初回起動）のとき、バックアップはスキップされる（バックアップ元が無いため） | High | Step 6 | 未着手 |
| T21 | 異常系 | `Database::new_with_backup` 内のバックアップ失敗（ディスクフル等）は `warn!` ログを出し、`new_with_backup` 自体は成功する（バックアップ失敗で起動を止めない。best-effort） | High | Step 6 | 未着手 |
| T22 | 正常系 | 定期バックアップ実行後、`db_meta.backup_last_run` に実行時刻（RFC3339）が書き込まれる | High | Step 7 | 未着手 |
| T23 | 統合 | `run_backup_scheduler_loop_with_clock(state, mock_clock)` は `enabled=true` のとき `mock_clock` を進めるとバックアップを実行し、`db_meta.backup_last_run` を更新する（DI された Clock で deterministic 検証） | High | Step 7 | 未着手 |
| T24 | 統合 | `run_backup_scheduler_loop(state)` は `enabled=false` のとき即座にリターンする | Medium | Step 7 | 未着手 |
| T25 | 正常系 | `Config` に `db.backup.*` を設定して save し再 load すると、同じ値が復元される（round-trip） | High | Step 5 | 未着手 |

---

## Step 0: Worktree 作成

- ブランチ名: `feat/sqlite-backup`
- 作成コマンド:
  - `git worktree add ../egopulse-sqlite-backup -b feat/sqlite-backup`
- ※ `worktree-create` skill を使用してもよい

---

## Step 1: `generate_backup_filename` TDD Cycle - タイムスタンプ命名（T1）

### この Step の目的

`src/storage/backup.rs` を新設し、タイムスタンプベースのファイル名生成関数を実装する。`run_backup` より先に独立してテストできる最小単位。

### 今回選ぶ項目

- 対象: `T1`
- 選ぶ理由: ファイル名生成は副作用がなく純粋関数として切り出しやすい。これを最初に確定させると、後続の `run_backup` が「生成したファイル名で実際にファイルを作る」という責務に集中できる。
- この時点では扱わないこと: `run_backup`（T2-T5）、`prune_old_backups`（T6-T9）、`compute_next_backup_run`（T10-T15）、Config（T16-T18）

### RED: 失敗する自動テストを書く

- 追加するテスト名: `generate_backup_filename_uses_configured_timezone`
- Given: `now = "2026-06-20T18:00:00Z"`（UTC 18:00 = JST 03:00 翌日）、`tz = "Asia/Tokyo"`
- When: `generate_backup_filename(now, &tz)`
- Then: 戻り値は `"egopulse-20260621-030000.db"`（JST で日付を取る）
- 失敗理由の想定: `backup` モジュール未実装のためコンパイルエラー

### GREEN: 最小実装

`src/storage/backup.rs` を新規作成し、`pub(crate) fn generate_backup_filename(now: DateTime<Utc>, tz: &str) -> String` を実装。`chrono_tz::Tz` で `now` をローカル時刻へ変換し、`format!("{:%Y%m%d-%H%M%S}", local)` で組み立てる。`tz` の parse 失敗時は UTC にフォールバック（ログ出力はしない、純粋関数のため）。

`src/storage/mod.rs` に `mod backup;` を追加。

### REFACTOR: 設計の整理

- 重複: なし（最初の関数）
- 命名: `generate_backup_filename` は責務が明確
- 責務: ファイル名の文字列組み立てのみ。ディスク I/O は持たない
- テストの構造的結合: `DateTime<Utc>` と `&str`（TZ）を渡す純粋関数のため、テストは密結合なし
- 次の項目へ進める身軽さ: Green になれば `run_backup` の実装に進める

### テストリスト更新

- 完了: `T1`
- 追加: なし
- 次候補: `T2`（バックアップ作成・内容コピー）

### コミット

`feat(storage): add backup module skeleton with filename generator`

---

## Step 2: `run_backup` TDD Cycle - VACUUM INTO による一貫性スナップショット（T2/T3/T4/T5）

### この Step の目的

`run_backup(db: &Database, dest_dir: &Path, tz: &str, now: DateTime<Utc>, max_generations: u32) -> Result<BackupOutcome, StorageError>` を実装する。`VACUUM INTO` でスナップショットを作成し、`PRAGMA integrity_check` を実行、`max_generations` を超える古いバックアップを削除するまでを一連の処理とする。ただし世代削除は Step 3 で独立して TDD するため、この Step では `max_generations` 引数を受け取るが `prune_old_backups` は未実装（`max_generations=0` で無効扱い）で通す。

### 今回選ぶ項目

- 対象: `T2`, `T3`, `T4`, `T5`
- 選ぶ理由: バックアップの中核機能。4 つの観点（作成・内容コピー・整合性・異常時削除）を1 Cycle で扱う。各観点で Red 自動テストを1件ずつ（合計4 Cycle 相当）追加する。
- この時点では扱わないこと: 世代管理の独立テスト（T6-T9）、スケジュール計算（T10-T15）、Config（T16-T18）、起動時 hook（T19-T21）、scheduler（T22-T24）

### RED-1: 失敗する自動テストを書く（T2 正常系・作成）

- 追加するテスト名: `run_backup_creates_file_at_destination`
- Given: `tempfile::TempDir` 配下に `Database::new_unchecked` で空の DB を作成。`dest_dir` は別の `TempDir`。`tz="UTC"`, `now=2026-06-20T03:00:00Z`, `max_generations=0`（この時点では世代削除しない）
- When: `run_backup(&db, dest_dir, "UTC", now, 0)`
- Then: `dest_dir/egopulse-20260620-030000.db` が存在する。戻り値は `Ok(BackupOutcome { path, integrity_ok: true })`
- 失敗理由の想定: `run_backup` 未実装のためコンパイルエラー

### GREEN-1: 最小実装

`run_backup` を実装:
1. `dest_dir` が存在しない場合は `std::fs::create_dir_all`
2. `generate_backup_filename(now, tz)` でファイル名生成
3. `dest_path = dest_dir.join(filename)`
4. プールからコネクションを取得（`db.get_conn()?`）
5. `conn.execute_batch(&format!("VACUUM INTO '{}'", dest_path.display()))?` （パスのエスケープはシングルクォート内の `'` を `''` に置換）
6. `BackupOutcome { path: dest_path, integrity_ok: true }` を返す

`BackupOutcome` 構造体を定義（`path: PathBuf`, `integrity_ok: bool`）。`integrity_ok` は RED-3 で使う。

### RED-2: 失敗する自動テストを書く（T3 内容コピー）

- 追加するテスト名: `run_backup_copies_all_tables_and_rows`
- Given: 元 DB に適当なチャットを INSERT（`store_message` 等の既存ヘルパを使用）。その後 `run_backup`
- When: バックアップファイルを別途 `Connection::open` で開く
- Then: バックアップ側の `messages` テーブルに同じ行が存在する（`SELECT COUNT(*)` で件数一致）
- 失敗理由の想定: GREEN-1 の実装で `VACUUM INTO` を使っていれば成功するはず。もし `VACUUM`（=`INTO` 無し）を実装していたら別の DB に書き出せず失敗

### GREEN-2: 最小実装

GREEN-1 で `VACUUM INTO` を使っていれば特段追加作業なし。`VACUUM` 単体を使っていた場合は `VACUUM INTO 'path'` へ修正。

### RED-3: 失敗する自動テストを書く（T4 整合性チェック正常）

- 追加するテスト名: `run_backup_runs_integrity_check_on_success`
- Given: 正常な DB
- When: `run_backup`
- Then: 戻り値の `BackupOutcome.integrity_ok == true`。また、`dest_path` を開いて `PRAGMA integrity_check` を実行すると `ok` が返る（これは検証として入れる）
- 失敗理由の想定: GREEN-1 で整合性チェックを実装していない場合、`integrity_ok` は初期値のままか常に `false` になるため、このテストが通りやすい。ただし整合性チェック処理自体が無いと RED-4 で検証できないため、ここで実装する意義を確認

> 実装時メモ: T4 は「整合性チェックが走ること」の検証であり、T5 は「チェック失敗時の処理」の検証。GREEN-1 の時点で整合性チェック未実装なら GREEN-3 で実装する。

### GREEN-3: 最小実装

`run_backup` の末尾に、バックアップファイルを別コネクションで開いて `PRAGMA integrity_check` を実行する処理を追加:
```rust
let check_conn = Connection::open(&dest_path)?;
let result: String = check_conn.query_row("PRAGMA integrity_check;", [], |row| row.get(0))?;
let integrity_ok = result == "ok";
```
`BackupOutcome.integrity_ok` にセット。

### RED-4: 失敗する自動テストを書く（T5 整合性チェック異常）

- 追加するテスト名: `run_backup_deletes_file_when_integrity_check_fails`
- Given: 整合性チェックが失敗する状況を強制的に作る。`VACUUM INTO` 後にバックアップファイルへバイト追記や truncate で壊すモック、または `run_backup` に整合性チェック用のフックを渡せる設計にする（実装時判断）。シンプルには「バックアップファイルの一部をランダム書き換えしてから整合性チェック」するテストヘルパを用意
- When: 整合性チェックが `ok` 以外を返す状況で `run_backup`
- Then: `warn!` ログが出力される。`dest_path` にファイルが存在しない（削除されている）。戻り値は `Ok(BackupOutcome { path: dest_path, integrity_ok: false })` または `Err(...)`（実装時に判断、推奨は `Ok` で `integrity_ok=false` を返し、呼び出し元で warn log を出す設計）

> **実装時リスク**: 整合性チェック失敗を人工的に作るのは難しい。`VACUUM INTO` 直後のファイルは原則として正常であるため。`pub(crate)` な境界に `run_backup_with_integrity_checker` のようなDI可能なシグネチャを用意し、テストから壊れたファイルを返す checker を注入できるようにする設計が妥当。実装時に「DIするか、テスト用にファイルを壊すか」を判断。

### GREEN-4: 最小実装

T5 の戦略に合わせて実装:
- DI 方式: `run_backup_with_integrity_checker(db, dest_dir, tz, now, max_generations, checker)` を本体にし、`run_backup` は `checker=run_pragma_integrity_check` を渡すラッパ
- ファイル破壊方式: `run_backup` 末尾の整合性チェックで `ok` 以外なら `std::fs::remove_file(&dest_path)` し、`warn!` ログ、`integrity_ok=false` で return

推奨は DI 方式（テスト容易性が高く、本質的な振る舞いを明示できるため）。

### REFACTOR: 設計の整理

- 重複: `VACUUM INTO` のパスエスケープ（`'` → `''`）が1箇所。必要なら `escape_sqlite_path_string` のような private ヘルパに切り出す
- 命名: `run_backup` は `docs/db.md` の既存 `store_*` / `update_*` 系と対比して「アクション」のニュアンスが強いが、 backup モジュール内では主体が明確なため `run_` 接頭辞で OK
- 責務: `run_backup` は「バックアップ作成 + 整合性チェック + 失敗時削除」までを一貫。世代削除（`prune_old_backups`）は別関数で Step 3 で追加
- テストの構造的結合: `BackupOutcome` を戻り値にすることで、呼び出し元が「ファイルが出来たか・整合性 OK か」を両方取れる。ログ出力の検証はDIで注入可能
- 次の項目へ進める身軽さ: Green になれば `prune_old_backups` へ

### テストリスト更新

- 完了: `T2`, `T3`, `T4`, `T5`
- 追加: なし
- 次候補: `T6`（世代削除・正常系）

### コミット

`feat(storage): run backup via VACUUM INTO with integrity check`

---

## Step 3: `prune_old_backups` TDD Cycle - 世代管理（T6/T7/T8/T9）

### この Step の目的

`prune_old_backups(dir: &Path, max_generations: u32) -> Result<usize, std::io::Error>` を実装し、`run_backup` の最後に呼び出す。`egopulse-*.db` パターンに一致するファイルを名前降順でソートし、`max_generations` を超える古いものを削除する。

### 今回選ぶ項目

- 対象: `T6`, `T7`, `T8`, `T9`
- 選ぶ理由: 世代管理は独立した純粋なディレクトリ操作。`run_backup` と分離することでテストが容易
- この時点では扱わないこと: スケジュール計算（T10-T15）、Config（T16-T18）、起動時 hook（T19-T21）、scheduler（T22-T24）

### RED-1: 失敗する自動テストを書く（T6 正常系）

- 追加するテスト名: `prune_old_backups_deletes_oldest_beyond_max`
- Given: `tempfile::TempDir` に `egopulse-20260601-030000.db`, `egopulse-20260602-030000.db`, ..., `egopulse-20260605-030000.db` の5ファイルを作成
- When: `prune_old_backups(dir, 3)`
- Then: 戻り値 `Ok(2)`。`dir` には新しい3件（20260602/03/04/05 ではなく降順で新しい方3件 = 20260603/04/05）が残る。**注意**: 名前（タイムスタンプ）降順で新しい方から3件を残すため、残るのは `20260603/04/05`。`20260601/02` が削除される
- 失敗理由の想定: `prune_old_backups` 未実装のためコンパイルエラー

### GREEN-1: 最小実装

`prune_old_backups` を実装:
1. `std::fs::read_dir(dir)` でエントリ一覧を取得（ディレクトリ自体が無ければ `Ok(0)` でリターン）
2. 各エントリのファイル名が `egopulse-` プレフィックス + `.db` サフィックスに一致するものだけ収集
3. 名前で降順ソート（新しい順）
4. インデックス `max_generations..` 以降を `std::fs::remove_file`
5. 削除した件数を返す

`run_backup` の末尾（整合性チェック後）で `prune_old_backups(dest_dir, max_generations)` を呼ぶよう修正。

### RED-2: 失敗する自動テストを書く（T7 境界値）

- 追加するテスト名: `prune_old_backups_keeps_all_when_below_max`
- Given: 3件のバックアップファイル
- When: `prune_old_backups(dir, 5)`
- Then: 戻り値 `Ok(0)`。3件すべて残る
- 失敗理由の想定: GREEN-1 の実装が `<` ではなく `<=` になっている場合を検出

### GREEN-2: 最小実装

GREEN-1 で境界値が正しければ特段追加作業なし。オフバイワンがあれば修正。

### RED-3: 失敗する自動テストを書く（T8 空・ゼロ状態）

- 追加するテスト名: `prune_old_backups_handles_missing_or_empty_dir`
- Given: (a) 存在しないディレクトリ、(b) 空のディレクトリ
- When: `prune_old_backups`
- Then: どちらも `Ok(0)` を返し、エラーにならない
- 失敗理由の想定: GREEN-1 で「ディレクトリが無い」場合にエラー伝播している場合

### GREEN-3: 最小実装

`prune_old_backups` の先頭でディレクトリ存在チェックを追加:
```rust
if !dir.exists() {
    return Ok(0);
}
```
`read_dir` が空イテレータを返す分には自然と `Ok(0)` になるはず。

### RED-4: 失敗する自動テストを書く（T9 除外パターン）

- 追加するテスト名: `prune_old_backups_ignores_non_backup_files`
- Given: 3件の正規バックアップ + `notes.txt` + `egopulse-manual.db`（パターンに合致しない）+ サブディレクトリ `old/`
- When: `prune_old_backups(dir, 2)`
- Then: 正規バックアップの古い1件だけ削除。`notes.txt`、`egopulse-manual.db`、`old/` は残る
- 失敗理由の想定: GREEN-1 のプレフィックス/サフィックスチェックが緩い場合、`egopulse-manual.db` まで削除対象になる

### GREEN-4: 最小実装

プレフィックスは `egopulse-`、その後に数字8桁（`YYYYMMDD`）+ `-` + 数字6桁（`HHMMSS`）+ `.db`、という正確なパターンマッチにする。`std::path::Path::is_file()` でディレクトリ除外も入れる。実装時は `str::starts_with("egopulse-") && str::ends_with(".db")` の緩いチェックでも T9 を通せるが、より厳密な正規表現 or 手動パースのどちらが良いか実装時に判断。

### REFACTOR: 設計の整理

- 重複: `run_backup` と `prune_old_backups` で `egopulse-*.db` パターンを使う箇所が増えるなら、`is_backup_filename(s: &str) -> bool` のような private predicate を抽出
- 命名: `prune_old_backups` は `max_generations` を超えるものを削除、と名前で意図が伝わる
- 責務: ファイル名のパターンマッチ + ソート + 削除のみ。バックアップ作成はしない
- テストの構造的結合: `tempfile::TempDir` ベースのテストで副作用を閉じ込めている
- 次の項目へ進める身軽さ: Green になればスケジュール計算へ

### テストリスト更新

- 完了: `T6`, `T7`, `T8`, `T9`
- 追加: なし
- 次候補: `T10`（スケジュール計算・初回）

### コミット

`feat(storage): prune old backup files by max_generations`

---

## Step 4: `compute_next_backup_run` TDD Cycle - スケジュール純粋関数（T10/T11/T12/T13/T14/T15）

### この Step の目的

`src/storage/backup.rs` に `pub(crate) fn compute_next_backup_run(config: &BackupConfig, timezone: &str, now: DateTime<Utc>, last_run_at: Option<DateTime<Utc>>) -> Option<DateTime<Utc>>` を実装する。`sleep/scheduler.rs` の `next_scheduled_run` / `next_run_for_time` / `try_date` / `resolve_gap` / `parse_hhmm` の構造をベースに、`interval_days` を加味した次回時刻を計算する。

### 今回選ぶ項目

- 対象: `T10`, `T11`, `T12`, `T13`, `T14`, `T15`
- 選ぶ理由: scheduler loop が正しく次回実行時刻を計算できることは、定期バックアップが適切な間隔で走るための前提。純粋関数として `now` を注入可能なので tokio 不要でテストできる。
- この時点では扱わないこと: Config の parse・default（T16-T18）、`Database::new_with_backup` への統合（T19-T21）、scheduler loop 自体（T22-T24）

### RED-1: 失敗する自動テストを書く（T10 初回・今日/明日）

- 追加するテスト名: `compute_next_backup_run_first_run_returns_today_or_tomorrow`
- Given: `config = enabled(interval_days=1, time="14:00")`, `tz="Asia/Tokyo"`, `last_run_at=None`
- When (a): `now = 2026-01-15T04:00:00Z`（JST 13:00）
- Then (a): `2026-01-15T05:00:00Z`（JST 14:00 同日）
- When (b): `now = 2026-01-15T06:00:00Z`（JST 15:00、既に過ぎ）
- Then (b): `2026-01-16T05:00:00Z`（JST 14:00 翌日）
- 失敗理由の想定: `compute_next_backup_run` 未実装のためコンパイルエラー

### GREEN-1: 最小実装

`compute_next_backup_run` を実装:
1. `config.enabled` が false なら `None`
2. `tz.parse::<Tz>()` 失敗なら `None`
3. `parse_hhmm(&config.time)` 失敗なら `None`（既存 `sleep/scheduler.rs::parse_hhmm` を `pub(crate)` にして再利用 or 同等の private 関数を backup.rs に実装。実装時に判断、重複を避けるなら `pub(crate)` 化）
4. `last_run_at` が `None` の場合: `next_run_for_time(tz, time, now)` 相当を呼ぶ（既存関数の再利用 or 同等実装）
5. `Some(last)` の場合: `local_last = last.with_timezone(&tz)`、候補日 = `local_last.date_naive() + Duration::days(interval_days)`、その日の HH:MM を `try_date` で検証。過ぎていれば翌日以降を探索

### RED-2: 失敗する自動テストを書く（T11 interval_days 加算）

- 追加するテスト名: `compute_next_backup_run_with_last_run_uses_interval`
- Given: `config = enabled(interval_days=7, time="03:00")`, `tz="UTC"`, `last_run_at = Some(2026-06-14T03:00:00Z)`
- When: `now = 2026-06-16T12:00:00Z`（最終実行から2日後、次回は 6/21 03:00）
- Then: `2026-06-21T03:00:00Z`
- 失敗理由の想定: GREEN-1 の `interval_days` 加算が未実装なら今日/明日を返してしまう

### GREEN-2: 最小実装

`compute_next_backup_run` の `Some(last)` ブランチを実装:
- `earliest_eligible_date = local_last.date_naive() + Duration::days(interval_days as i64)`
- `earliest_eligible_date` の HH:MM を `try_date` で検証
- 過ぎていれば1日ずつ進めて再検証（最大120回 = `resolve_gap` と同じ上限）

### RED-3: 失敗する自動テストを書く（T12 disabled）

- 追加するテスト名: `compute_next_backup_run_returns_none_when_disabled`
- Given: `config = disabled`
- When: `compute_next_backup_run(config, "UTC", now, None)`
- Then: `None`
- 失敗理由の想定: GREEN-1 の最初の分岐で弾けていれば成功

### GREEN-3: 最小実装

GREEN-1 で `if !config.enabled { return None; }` を入れていれば特段追加作業なし。

### RED-4: 失敗する自動テストを書く（T13 不正 time）

- 追加するテスト名: `compute_next_backup_run_returns_none_for_invalid_time_format`
- Given: `config = enabled(time="25:99")` および `config = enabled(time="abc")`
- When: `compute_next_backup_run`
- Then: どちらも `None`
- 失敗理由の想定: `parse_hhmm` が `25:99` を通す場合

### GREEN-4: 最小実装

`parse_hhmm` の境界チェック（`hour > 23 || minute > 59`）が効いていれば特段追加作業なし。

### RED-5: 失敗する自動テストを書く（T14 DST gap）

- 追加するテスト名: `compute_next_backup_run_handles_dst_gap`
- Given: `config = enabled(time="02:30")`, `tz="America/New_York"`、DST 開始時刻 2026-03-08 02:30 は存在しない
- When: `now = 2026-03-08T06:00:00Z`（DST 開始前）
- Then: `2026-03-08T07:00:00Z`（03:00 EDT = 最初の有効時刻）
- 失敗理由の想定: `resolve_gap` を再利用していれば成功。`try_date` の `LocalResult::None` 分岐未対応だと失敗

### GREEN-5: 最小実装

`sleep/scheduler.rs::resolve_gap` と同等のロジックを使う。関数を `pub(crate)` で共有するか、backup.rs 内に同等実装を置く（実装時に判断、共有が望ましい）。

### RED-6: 失敗する自動テストを書く（T15 DST fold）

- 追加するテスト名: `compute_next_backup_run_handles_dst_fold`
- Given: `config = enabled(time="01:30")`, `tz="America/New_York"`、DST 終了時刻 2026-11-01 01:30 は2回発生
- When: `now = 2026-11-01T04:00:00Z`（fold 前）
- Then: `2026-11-01T05:30:00Z`（EDT 01:30 = 最早瞬間）
- 失敗理由の想定: `try_date` の `LocalResult::Ambiguous` 分岐で earliest を選んでいなければ失敗

### GREEN-6: 最小実装

`sleep/scheduler.rs::try_date` と同等の `Ambiguous(earliest, latest)` → `earliest` 優先ロジックを使う。

### REFACTOR: 設計の整理

- 重複: `parse_hhmm`, `try_date`, `resolve_gap` は `sleep/scheduler.rs` と重複。`pub(crate)` 化して共有するか、`src/runtime/schedule_util.rs` のような共通モジュールへ切り出す。実装時に判断（共有が望ましいが、独立している方が testability が高い場合もある）
- 命名: `compute_next_backup_run` は `sleep/scheduler.rs::next_scheduled_run` と対になる名前
- 責務: 純粋関数。`now` と `last_run_at` を注入することで deterministic にテスト可能
- テストの構造的結合: `DateTime<Utc>` を使うことで内部実装を意識せずにテスト可能
- 次の項目へ進める身軽さ: Green になれば Config 実装へ

### テストリスト更新

- 完了: `T10`, `T11`, `T12`, `T13`, `T14`, `T15`
- 追加: なし
- 次候補: `T16`（Config default）

### コミット

`feat(storage): compute next backup run with DST support`

---

## Step 5: `BackupConfig` TDD Cycle - 設定の parse / default / round-trip（T16/T17/T18/T25）

### この Step の目的

`src/config/types.rs` に `BackupConfig` と `DatabaseConfig` を追加し、`Config.db: DatabaseConfig` を生やす。`src/config/loader.rs` で `db:` セクションを parse する。`src/config/resolve.rs` に `default_backup_dir()` を追加。**`src/config/persist.rs` に `SerializableDb`/`SerializableBackup` を追加し、save → load の round-trip を保証する**。

### 今回選ぶ項目

- 対象: `T16`, `T17`, `T18`, `T25`
- 選択理由: scheduler / startup backup が設定値に依存するため、先に Config を確定させる。T25（round-trip）をこの Step に入れる理由は、`persist.rs` の更新が Config 追加と機能的に一体だから（後 Step に切り出すとコミット分割で中途半端な Config が生じる）
- この時点では扱わないこと: `Database::new_with_backup` 統合（T19-T21）、scheduler（T22-T24）

### RED-1: 失敗する自動テストを書く（T16 default）

- 追加するテスト名: `backup_config_default_when_db_section_missing`
- Given: `egopulse.config.yaml` に `db:` セクションが無い YAML
- When: `Config::load_from_str`
- Then: `config.db.backup.enabled == true`, `interval_days == 7`, `time == "03:00"`, `max_generations == 12`
- 失敗理由の想定: `DatabaseConfig` フィールド未実装のためコンパイルエラー

### GREEN-1: 最小実装

`src/config/types.rs` に追加:
```rust
#[derive(Clone, Debug)]
pub(crate) struct BackupConfig {
    pub enabled: bool,
    pub interval_days: u32,
    pub time: String,
    pub max_generations: u32,
}

impl Default for BackupConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_days: 7,
            time: "03:00".to_string(),
            max_generations: 12,
        }
    }
}

impl BackupConfig {
    pub(crate) fn scheduler_enabled(&self) -> bool {
        self.enabled
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct DatabaseConfig {
    pub backup: BackupConfig,
}
```

`Config` 構造体に `pub(crate) db: DatabaseConfig` フィールドを追加。`src/config/loader.rs` で `db:` セクションを parse（無ければ `Default`）。

### RED-2: 失敗する自動テストを書く（T17 enabled=false）

- 追加するテスト名: `backup_config_scheduler_enabled_returns_false_when_disabled`
- Given: `db: { backup: { enabled: false } }` の YAML
- When: `Config::load_from_str` 後に `config.db.backup.scheduler_enabled()`
- Then: `false`
- 失敗理由の想定: GREEN-1 で `scheduler_enabled()` が正しく実装されていれば成功

### GREEN-2: 最小実装

GREEN-1 で実装済みのはず。特段追加作業なし。

### RED-3: 失敗する自動テストを書く（T18 バリデーション）

- 追加するテスト名: `backup_config_rejects_zero_interval_and_generations`
- Given: `db: { backup: { interval_days: 0 } }` および `db: { backup: { max_generations: 0 } }` の YAML
- When: `Config::load_from_str`
- Then: どちらも `Err(ConfigError::...)`
- 失敗理由の想定: バリデーション未実装

### GREEN-3: 最小実装

`Config::validate` 相当のフェーズ（loader 内の既存バリデーション）に `backup` のチェックを追加:
```rust
if config.db.backup.interval_days == 0 {
    return Err(ConfigError::InvalidBackupConfig("interval_days must be >= 1"));
}
if config.db.backup.max_generations == 0 {
    return Err(ConfigError::InvalidBackupConfig("max_generations must be >= 1"));
}
```
`ConfigError` に `InvalidBackupConfig(String)` バリアントを追加。

### RED-4: 失敗する自動テストを書く（T25 round-trip）

- 追加するテスト名: `db_backup_config_round_trips_through_save_and_load`
- Given: `db: { backup: { enabled: false, interval_days: 3, time: "23:45", max_generations: 7 } }` の YAML を load した `Config`（`enabled: false` にするのは `Default` との差分を明確にするため）
- When: `Config` を一時ファイルへ save し、再度 load する
- Then: 再 load 後の `config.db.backup` が元と全フィールド一致（`enabled`, `interval_days`, `time`, `max_generations`）
- 失敗理由の想定: `src/config/persist.rs` の `SerializableConfig` に `db` フィールドが無く、save 時に `db.backup.*` が失われる

### GREEN-4: 最小実装

`src/config/persist.rs` に `SerializableDb` / `SerializableBackup` を追加:
```rust
#[derive(Serialize)]
struct SerializableBackup {
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    enabled: bool,
    interval_days: u32,
    time: String,
    max_generations: u32,
}

#[derive(Serialize)]
struct SerializableDb {
    backup: SerializableBackup,
}
```

`SerializableConfig` に `#[serde(skip_serializing_if = "Option::is_none")] db: Option<SerializableDb>` を追加。`SerializableConfig` の構築箇所（`From<&Config>` 相当の変換）で `config.db` から `SerializableDb` を組み立てる。

> **実装時の注意**: `skip_serializing_if` のポリシーは既存 `SerializableSleepBatch` 等に合わせる。`Default` と一致する値を `skip` するかどうかは実装時に既存パターンを調査して決定（`Default` 値でも明示的に書き出す方が round-trip 安全性は高い）。

### REFACTOR: 設計の整理

- 重複: `Default` とバリデーションが別れているのは `SleepBatchConfig` と同じパターン
- 命名: `BackupConfig`, `DatabaseConfig`, `scheduler_enabled()`, `SerializableBackup`, `SerializableDb` は既存命名と整合
- 責務: 設定の保持・バリデーション・永続化のみ。スケジュール計算・実行は持たない
- テストの構造的結合: `Config::load_from_str` と save → load の round-trip で end-to-end 検証
- 次の項目へ進める身軽さ: Green になれば `Database::new_with_backup` 統合へ

### テストリスト更新

- 完了: `T16`, `T17`, `T18`, `T25`
- 追加: なし
- 次候補: `T19`（起動時バックアップ）

### コミット

`feat(config): add db.backup configuration section with round-trip persistence`

---

## Step 6: 起動時バックアップ統合 TDD Cycle - `Database::new_with_backup` 新設（T19/T20/T21）

### この Step の目的

**`Database::new(db_path)` のシグネチャは変更せず**、新規 `Database::new_with_backup(db_path, &BackupSettings) -> Result<Self, StorageError>` を追加する。`initialize_database_file` 後・`run_migrations` 前に起動時バックアップを実行する。runtime は `new_with_backup` を呼び、既存21箇所の `new` 呼び出し元は一切変更しない。失敗は `warn!` ログで握り、`new_with_backup` 自体は成功させる。

### 今回選ぶ項目

- 対象: `T19`, `T20`, `T21`
- 選ぶ理由: 起動時バックアップは最も重要な保険（マイグレーション失敗時の回復）。3 观点（実行・初回スキップ・失敗時継続）で検証
- この時点では扱わないこと: scheduler loop（T22-T24）

### RED-1: 失敗する自動テストを書く（T19 正常系）

- 追加するテスト名: `database_new_with_backup_creates_startup_backup_before_migration`
- Given: `tempfile::TempDir` 配下に既存の `runtime/egopulse.db` を作成（`Database::new` で作ってから一旦 drop）。`backup_dir` は別途作成。`BackupSettings { enabled: true, dest_dir, max_generations: 12, tz: "UTC".into(), now: 2026-06-20T03:00:00Z }`
- When: `Database::new_with_backup(db_path, &backup_settings)` を呼ぶ
- Then: `backup_dir` に `egopulse-20260620-030000.db` が1件存在する。戻り値は `Ok(Database)`
- 失敗理由の想定: `Database::new_with_backup` 未実装のためコンパイルエラー

### GREEN-1: 最小実装

`Database::new_with_backup` を追加:
```rust
pub(crate) fn new_with_backup(
    db_path: &Path,
    settings: &BackupSettings,
) -> Result<Self, StorageError> {
    // レガシー DB チェック + parent dir 作成 + initialize_database_file
    // は Database::new と共有。共通の初期化ヘルパを private fn として
    // 抽出するか、`new` の内部処理をこの関数内に展開するかは実装時に判断。
    // いずれにせよ `new(db_path)` のシグネチャ・挙動は温存する。

    if db_path.exists() && settings.enabled {
        if let Err(error) = run_startup_backup(db_path, settings) {
            tracing::warn!(%error, "startup backup failed; continuing");
        }
    }

    // 続けて `new` と同等の pool 構築 + run_migrations を実行。
    // 既存の `Database::new` の後半を reuse する。
}
```

`BackupSettings` 構造体を `src/storage/backup.rs` に定義（`pub(crate)`、`enabled: bool`, `dest_dir: PathBuf`, `max_generations: u32`, `tz: String`, `now: DateTime<Utc>`）。`now` はテストで注入できるようにフィールドに持つ（本番では `Utc::now()` を呼び出し元が入れる）。

`run_startup_backup(db_path: &Path, settings: &BackupSettings) -> Result<(), StorageError>` は:
1. 一時的な `Connection::open(db_path)` で直接開く（pool 構築前のため）
2. `generate_backup_filename(settings.now, &settings.tz)` でファイル名生成
3. `dest_dir` が存在しない場合は `create_dir_all`
4. `conn.execute_batch(&format!("VACUUM INTO '{}'", escaped_path))?`
5. 別コネクションで `PRAGMA integrity_check`、`ok` 以外なら `remove_file` + `warn!`

> **設計判断**: `run_startup_backup` は pool 構築前なので `Database` インスタンスを作らず、生の `Connection` を使う。これは `run_backup`（pool 経由）とは異なる経路になるが、マイグレーション前という制約上やむを得ない。共通化できるのは `generate_backup_filename` と `VACUUM INTO` のパスエスケープ程度。実装時に共通化を検討。

`runtime/mod.rs:293` の既存 `Database::new(&config.db_path())?` を `Database::new_with_backup(&config.db_path(), &backup_settings)?` へ更新。`backup_settings` は `runtime/mod.rs` 側で `Config` から組み立てる（`Config::db_path()`, `Config::backup_dir()`, `Config::db.backup.*`, `Utc::now()` を統合）。**その他の20箇所の `Database::new` 呼び出し元は変更しない**。

### RED-2: 失敗する自動テストを書く（T20 初回スキップ）

- 追加するテスト名: `database_new_with_backup_skips_backup_when_db_file_missing`
- Given: `tempfile::TempDir` 配下で `runtime/egopulse.db` が**存在しない**（初回起動）
- When: `Database::new_with_backup(db_path, &backup_settings)` を呼ぶ（`enabled=true`）
- Then: `backup_dir` にファイルは1件も存在しない。戻り値は `Ok(Database)`
- 失敗理由の想定: GREEN-1 の `db_path.exists()` チェックが無ければ、存在しない DB に対して VACUUM INTO を試してエラー

### GREEN-2: 最小実装

GREEN-1 で `if db_path.exists() && settings.enabled` ガードを入れていれば特段追加作業なし。

### RED-3: 失敗する自動テストを書く（T21 失敗時継続）

- 追加するテスト名: `database_new_with_backup_continues_when_startup_backup_fails`
- Given: `backup_settings.dest_dir` を**書き込み不可**なパスに設定（例: 既存ファイルと同名のパス、または親ディレクトリを作れないパス）。`db_path` は既存
- When: `Database::new_with_backup(db_path, &backup_settings)`
- Then: 戻り値は `Ok(Database)`（バックアップ失敗で起動を止めない）。`warn!` ログが出力される（ログ検証はオプション、戻り値検証を主とする）
- 失敗理由の想定: GREEN-1 の `if let Err(error) = ... { warn!(...) }` で握っていなければ `new_with_backup` が `Err` を返す

### GREEN-3: 最小実装

GREEN-1 で `if let Err(error) = run_startup_backup(...) { warn!(...) }` で握んでいれば特段追加作業なし。

### REFACTOR: 設計の整理

- 重複: `run_backup`（pool 経由・定期実行）と `run_startup_backup`（生 Connection・起動時）が似た処理。共通化の可能性を検討。ただし pool 構築前という制約上、完全な共通化は難しい。`VACUUM INTO` 部分 + ファイル名生成 + 整合性チェックを共有関数に切り出せるか実装時に判断
- 命名: `BackupSettings` は `Config` から変換される中間データ。`Database` が `Config` に直接依存しないようにするための境界
- 責務: `Database::new_with_backup` はバックアップの成否には関与しない（best-effort）。マイグレーションと pool 構築が本体。`Database::new` は従来通りバックアップ機能を持たない
- テストの構造的結合: `BackupSettings` を通じてテストから制御可能
- 次の項目へ進める身軽さ: Green になれば scheduler へ

### テストリスト更新

- 完了: `T19`, `T20`, `T21`
- 追加: なし
- 次候補: `T22`（定期実行 scheduler）

### コミット

`feat(runtime): add Database::new_with_backup for startup backup before migration`

---

## Step 7: 定期バックアップ scheduler TDD Cycle - `run_backup_scheduler_loop` + `Clock` DI（T22/T23/T24）

### この Step の目的

`src/runtime/backup_scheduler.rs` を新設し、`run_backup_scheduler_loop(state) -> Result<(), EgoPulseError>` と `run_backup_scheduler_loop_with_clock(state, clock)` を実装する。`compute_next_backup_run` で次回時刻を計算し sleep、`run_backup` を pool 経由で呼び出す。実行後に `db_meta.backup_last_run` を RFC3339 で更新する。`start_channels` から spawn する。

`Clock` trait を導入することで、テストから `now` を制御可能にする。HH:MM 粒度の設定でも deterministic な scheduler テストを実現するための必須の seam。

### 今回選ぶ項目

- 対象: `T22`, `T23`, `T24`
- 選ぶ理由: 定期実行の中核。3 观点（last_run 記録・loop 統合・disabled リターン）で検証
- この時点では扱わないこと: docs 更新（Step 8）、手動確認（Step 9）

### RED-1: 失敗する自動テストを書く（T22 last_run 記録）

- 追加するテスト名: `periodic_backup_writes_last_run_to_db_meta`
- Given: テスト用 `Database`、`backup_settings`、固定 `now = 2026-06-20T03:00:00Z`
- When: periodic backup 実行関数（loop から切り出した単発実行ヘルパ `run_periodic_backup_once(state, now)`) を呼ぶ
- Then: `db_meta` テーブルの `key='backup_last_run'` の `value` が `now` の RFC3339 文字列と一致
- 失敗理由の想定: `run_periodic_backup_once` 未実装のためコンパイルエラー

> **設計判断**: loop から「1回分の実行」を切り出した `run_periodic_backup_once(state: &AppState, now: DateTime<Utc>) -> Result<BackupOutcome, ...>` をテスト可能にする。loop は `compute_next_backup_run` → sleep → `run_periodic_backup_once` の繰り返し。

### GREEN-1: 最小実装

`src/runtime/backup_scheduler.rs` に以下を実装:
- `pub(crate) trait Clock: Send + Sync { fn now(&self) -> DateTime<Utc>; }`
- `pub(crate) struct RealClock;`（`impl Clock for RealClock { fn now(&self) -> DateTime<Utc> { Utc::now() } }`）
- `pub(crate) async fn run_backup_scheduler_loop(state: AppState) -> Result<(), EgoPulseError>`: `Arc::new(RealClock)` を使う thin wrapper で `run_backup_scheduler_loop_with_clock(state, Arc::new(RealClock)).await` を呼ぶ
- `pub(crate) async fn run_backup_scheduler_loop_with_clock(state: AppState, clock: Arc<dyn Clock>) -> Result<(), EgoPulseError>`: 本体。`compute_next_backup_run` → `tokio::time::sleep` → `run_periodic_backup_once` を `clock.now()` で駆動。**`clock` は `Arc<dyn Clock>` で受け取る**（値渡し `impl Clock` ではテストから `advance` できないため）
- `pub(crate) async fn run_periodic_backup_once(state: &AppState, now: DateTime<Utc>) -> Result<BackupOutcome, EgoPulseError>`: `call_blocking` で `run_backup` を呼び、成功後に `db_meta.backup_last_run` を upsert

`src/storage/backup.rs` に `pub(crate) fn upsert_backup_last_run(db: &Database, last_run: DateTime<Utc>) -> Result<(), StorageError>` を追加（既存 `set_schema_version` と同じ key-value upsert パターン）。同様に `pub(crate) fn get_backup_last_run(db: &Database) -> Result<Option<DateTime<Utc>>, StorageError>` を追加。

### RED-2: 失敗する自動テストを書く（T23 loop 統合・Clock DI）

- 追加するテスト名: `run_backup_scheduler_loop_with_clock_executes_backup_when_delay_elapses`
- Given:
  - `interval_days=1`, `time="03:00"` に設定した `BackupConfig`
  - `tz="UTC"`（DST の影響を排除）
  - `backup_dir` は `TempDir`
  - `MockClock::new("2026-06-20T02:59:59.500Z")`（**目標時刻 `03:00:00` の 500ms 前** に固定）
  - テスト用の `AppState`（最小構成）
- When:
  - `run_backup_scheduler_loop_with_clock(state, Arc::clone(&mock_clock))` を `tokio::spawn` で起動
  - `tokio::time::timeout(Duration::from_secs(5), wait_for_backup_file(&backup_dir)).await` でバックアップファイル出現を待つ（`wait_for_backup_file` は50ms間隔で `read_dir` をポーリング）
- Then:
  - scheduler は起動直後に `compute_next_backup_run(config, "UTC", mock_now, None)` を呼び、`next = 2026-06-20T03:00:00Z`（`mock_now` が 02:59:59.500 なので、`HH:MM:00 = 03:00:00` は未来と判定される）
  - `delay = (next - mock_now) = 500ms` が実時間 `tokio::time::sleep(500ms)` に渡る
  - 500ms 実時間経過後に dispatch が走り、`run_periodic_backup_once(&state, clock.now() = 02:59:59.500)` が実行される
  - `backup_dir` に `egopulse-20260620-025959.db` が1件出来る
  - `db_meta.backup_last_run = "2026-06-20T02:59:59.500Z"` が書き込まれる
- テスト終了時: `handle.abort()` で scheduler task を中止（次 cycle は `delay ≈ 24h` に入るため、それを待たずに abort）
- 失敗理由の想定: GREEN-1 の scheduler が正しく sleep → execute していなければ、ファイルが出来ない。または `clock.now()` を見ていなければ次回時刻計算が実時間ベース（数時間〜数日後）になり、5秒の timeout でテストが失敗する

> **設計判断（重要）**:
> - **HH:MM 粒度問題の回避**: `compute_next_backup_run` は `time` を `NaiveTime::from_hms_opt(H, M, 0)` で秒=0 として扱う。そのため `mock_now` を「目標 `HH:MM:00` の直前（例: 500ms前）」に固定すれば、`next - mock_now = 500ms` となり、実時間 `tokio::time::sleep(500ms)` で確実に dispatch される。**`MockClock` をテスト中に `advance` する必要はない**（初期値の mock 時間から一度 `delay` が計算された後は sleep が実時間で進むため）
> - **scheduler を `tokio::spawn` で起動し、副作用観測後に `abort()`**: loop は無限のため、テストは副作用（バックアップファイル作成）を観測したら明示的に止める。`clock: Arc<dyn Clock>` でテストと scheduler が同じ `MockClock` を共有できる（`Arc::clone` で両者から触れる）
>
> **実装時リスク**:
> - **AppState 構築**: 既存 `test_util` 等のヘルパを流用。`channels` は空で OK（scheduler は channel を触らない）
> - **ポーリングの安定性**: `wait_for_backup_file` が CI 環境等で遅延した場合にテストが不安定になる可能性。`timeout` を長め（5秒）に取り、ポーリング間隔を短く（50ms）することで緩和
> - **簡略化案（代替）**: もし上記の「spawn + abort」形式が不安定になる場合、T23 をスキップして `compute_next_backup_run` 単体（T10-T15）+ `run_periodic_backup_once` 単体（T22）+ Step 8 手動確認 でカバーする構成も検討する。ただし scheduler loop 全体の接続は自動保証されなくなるため、実装時に判断

### GREEN-2: 最小実装

`run_backup_scheduler_loop_with_clock` を実装:
```rust
pub(crate) async fn run_backup_scheduler_loop_with_clock(
    state: AppState,
    clock: Arc<dyn Clock>,
) -> Result<(), EgoPulseError> {
    if !state.config.db.backup.scheduler_enabled() {
        info!("backup scheduler: disabled, exiting loop");
        return Ok(());
    }
    loop {
        let now = clock.now();
        let last_run = call_blocking(Arc::clone(&state.db), |db| get_backup_last_run(db))
            .await?
            .flatten();
        let next = match compute_next_backup_run(
            &state.config.db.backup,
            &state.config.timezone,
            now,
            last_run,
        ) {
            Some(t) => t,
            None => {
                info!("backup scheduler: no next run, exiting loop");
                return Ok(());
            }
        };

        let delay = (next - now).to_std().unwrap_or(Duration::ZERO);
        info!(next_run = %next.to_rfc3339(), delay_secs = delay.as_secs(),
              "backup scheduler: waiting");
        tokio::time::sleep(delay).await;

        match run_periodic_backup_once(&state, clock.now()).await {
            Ok(outcome) => info!(path = %outcome.path.display(), "backup scheduler: created"),
            Err(error) => warn!(%error, "backup scheduler: failed"),
        }
    }
}

pub(crate) async fn run_backup_scheduler_loop(state: AppState) -> Result<(), EgoPulseError> {
    run_backup_scheduler_loop_with_clock(state, Arc::new(RealClock)).await
}
```

`MockClock`（テスト用、`Mutex<DateTime<Utc>>` を内部保持。`Send + Sync` のため `RefCell` ではなく `Mutex`）を同ファイルの `#[cfg(test)]` 配置または `tests` モジュールに定義:

```rust
#[cfg(test)]
pub(crate) struct MockClock {
    now: std::sync::Mutex<DateTime<Utc>>,
}

#[cfg(test)]
impl MockClock {
    pub(crate) fn new(now: DateTime<Utc>) -> Self {
        Self { now: Mutex::new(now) }
    }
}

#[cfg(test)]
impl Clock for MockClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.lock().expect("mockclock poisoned")
    }
}
```

> T23 では `MockClock::new` で初期時刻を設定した後は `advance` を呼ばない（目標 HH:MM の直前に固定し、`delay` を小さく保ったまま実時間 `tokio::time::sleep` で進行させる）。そのため `advance` メソッドは用意せず、読み取り専用の `MockClock` で十分。今後 `advance` が必要な別テストを追加する場合は、そのタイミングで `advance` メソッドを追加する（YAGNI）。

### RED-3: 失敗する自動テストを書く（T24 disabled）

- 追加するテスト名: `run_backup_scheduler_loop_exits_immediately_when_disabled`
- Given: `BackupConfig` を `enabled=false` に設定
- When: `run_backup_scheduler_loop(state).await`（`Arc::new(RealClock)` の wrapper 経由）
- Then: 即座に `Ok(())` が返る（spawn せず直接 await して戻り値を確認）。`backup_dir` にファイルは無い
- 失敗理由の想定: GREEN-2 の最初の `scheduler_enabled()` チェックが抜けている場合、`compute_next_backup_run` が `None` を返して結果として即リターンにはなるが、明示チェックがある方が意図が明確

### GREEN-3: 最小実装

GREEN-2 の冒頭で `if !state.config.db.backup.scheduler_enabled() { return Ok(()); }` を入れていれば特段追加作業なし。

### start_channels への統合

`src/runtime/mod.rs:830` 付近（sleep scheduler spawn の直前または直後）に backup scheduler spawn を追加:
```rust
if state.config.db.backup.scheduler_enabled() {
    let backup_state = state.clone();
    info!("Starting backup scheduler");
    let handle = tokio::spawn(async move {
        crate::runtime::backup_scheduler::run_backup_scheduler_loop(backup_state).await
    });
    handles.push(("backup-scheduler".to_string(), handle));
}
```

`src/runtime/mod.rs` に `mod backup_scheduler;` を追加。

### REFACTOR: 設計の整理

- 重複: `compute_next_backup_run` → sleep → execute の構造は `sleep/scheduler.rs::run_scheduler_loop` とほぼ同一。共通化の可能性を検討するが、両者はスケジュール計算ロジックが異なるため、現状は独立を許容
- 命名: `run_backup_scheduler_loop`, `run_backup_scheduler_loop_with_clock`, `run_periodic_backup_once` は既存 `run_scheduler_loop`, `run_scheduled_cycle` と対になる
- 責務: scheduler は「次回時刻計算 → sleep → 実行」の制御のみ。バックアップ作成・世代管理は `storage/backup.rs` に任せる
- テストの構造的結合: `run_periodic_backup_once` を loop から切り出したことで単体テストが容易。さらに `Clock` を DI したことで loop テストも deterministic
- 次の項目へ進める身軽さ: Green になれば docs 更新へ

### テストリスト更新

- 完了: `T22`, `T23`, `T24`
- 追加: なし
- 次候補: なし（実装完了）

### コミット

`feat(runtime): add periodic DB backup scheduler with Clock DI`

---

## Step 8: 動作確認

### 全テスト通過コマンド

```bash
cargo fmt --check
cargo check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

### Lint / フォーマット / 型チェック

上記に同じ。`#[allow(dead_code)]` は使用禁止（AGENTS.md）。実装した `pub(crate)` 関数が呼び出し元から使われていることを確認。未使用になったヘルパは削除。

### 手動確認

- 既存の本番 DB を `~/.egopulse/runtime/egopulse.db` に置いた状態で `cargo run -- run` を実行
- 起動時に `~/.egopulse/runtime/backups/egopulse-YYYYMMDD-HHMMSS.db` が1件作成されることを確認
- ログに `backup scheduler: waiting next_run=...` が出ることを確認
- `egopulse.config.yaml` で `interval_days: 1`, `time: <5分後>` 等に短縮設定し、定期実行されることを確認
- `db.backup.enabled: false` で scheduler が即終了することを確認

### 失敗時に戻る Step

- テスト失敗 → 該当 Step の RED/GREEN を見直し
- clippy 警告 → 該当 Step の REFACTOR を見直し
- ビルド失敗 → シグネチャ・import を見直し

---

## Step 9: Plan・仕様書との自己チェック

実装完了後にこの Plan と関連仕様書（`docs/db.md`, `docs/config.md`）を最初から読み直し、実装・自動テスト・文書が要求した振る舞いと一致しているかを照合する。未実装、過剰実装、テスト不足、仕様書との齟齬を見つけた場合は、該当する TDD Cycle へ戻って修正し、動作確認を再実行してからこの Step を完了する。

- Plan のテストリスト（T1-T25）と各 Cycle が完了条件を満たしている
- `docs/db.md` の最終節に §5「バックアップ・復元」が追加されている。命名規則・保存先・復元手順（`systemctl stop → cp → rm wal/shm → systemctl start`）が記載されている
- `docs/config.md` に `db.backup` セクションが追加されている。各キーの意味・デフォルト値・例が記載されている
- 実装中に変更した設計判断（例: `Database::new_with_backup` の新設、`Clock` DI 採用、関数の共通化有無）が Plan と docs へ反映されている
- `src/config/persist.rs` の round-trip が `db.backup.*` すべてのフィールドで成立していることを手動で再確認（save → load で値が欠けない）
- `Database::new(db_path)` の既存呼び出し元21箇所が**一切変更されていない**ことを `git diff` で確認
- 変更ファイル一覧、コミット分割、自動テスト一覧が実際の変更と一致している
- 「正常時の振る舞いを変えていない」（既存のバックアップ対象外処理への影響がない）ことをコード差分で再確認

---

## Step 10: PR 作成

- PR タイトル: `SQLite DB バックアップ機構の追加`
- PR description（日本語）:
  - **概要**: WAL 運用中の EgoPulse 本番 DB に対し、`VACUUM INTO` で一貫性スナップショットを取得するバックアップ機構を追加する。起動時（マイグレーション前）と定期実行（デフォルト週1・日曜 03:00）の2経路を用意し、`~/.egopulse/runtime/backups/` 配下に世代管理付きで保存する。
  - **変更点**:
    - `src/storage/backup.rs`（新規）: `run_backup` / `prune_old_backups` / `generate_backup_filename` / `compute_next_backup_run` / `BackupSettings` / `BackupOutcome` / `run_startup_backup` 等の純粋関数・副作用ヘルパ
    - `src/runtime/backup_scheduler.rs`（新規）: `run_backup_scheduler_loop` / `run_backup_scheduler_loop_with_clock` / `run_periodic_backup_once` / `Clock` trait / `RealClock`
    - `src/storage/mod.rs`: `mod backup;` 宣言。**`Database::new` シグネチャは変更せず**、新規 `Database::new_with_backup(db_path, &BackupSettings)` を追加。マイグレーション前に起動時バックアップを hook
    - `src/runtime/mod.rs`: `mod backup_scheduler;` 宣言、`start_channels` に backup scheduler の spawn を追加、`Database::new_with_backup` 呼び出しと `BackupSettings` 構築
    - `src/config/types.rs`, `src/config/resolve.rs`, `src/config/loader.rs`, `src/config/persist.rs`: `db.backup.*` 設定追加 + round-trip 永続化（`SerializableDb`/`SerializableBackup`）
    - `docs/db.md`: §5「バックアップ・復元」を追加
    - `docs/config.md`: `db.backup` セクションを追加
  - **設計メモ**:
    - `VACUUM INTO` は SQL 一行で一貫性スナップショットを取得可能。Online Backup API と比較してコード量が少なく、夜間実行で書き込みブロックは無視できる
    - 保存先を `runtime/` の外に置くことで `rm -rf runtime/` からの復元を可能にしている
    - `Database::new` を変更せず `new_with_backup` を新設したことで、既存21箇所のテスト・呼び出し元は一切無変更
    - scheduler に `Clock` trait を DI することで HH:MM 粒度の設定でも deterministic なテストを実現
  - **テスト**: T1-T25（自動）。詳細は `docs/plan/plan-sqlite-backup.md` 参照
  - **Close #<issue-number>**（該当 Issue がある場合）

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
| ---- | ---- | -- |
| `src/storage/backup.rs` | **新規** | `run_backup`, `prune_old_backups`, `generate_backup_filename`, `compute_next_backup_run`, `upsert_backup_last_run`, `get_backup_last_run`, `BackupSettings`, `BackupOutcome`, `run_startup_backup` |
| `src/storage/mod.rs` | 変更 | `mod backup;` 宣言。**`Database::new(db_path)` は変更しない**。新規 `Database::new_with_backup(db_path, &BackupSettings)` を追加 |
| `src/config/types.rs` | 変更 | `BackupConfig`, `DatabaseConfig` 追加、`Config.db` フィールド追加 |
| `src/config/resolve.rs` | 変更 | `default_backup_dir()` 追加、`Config::backup_dir()` 追加 |
| `src/config/loader.rs` | 変更 | `db:` セクションの parse、`interval_days`/`max_generations` のバリデーション |
| `src/config/persist.rs` | 変更 | `SerializableDb`, `SerializableBackup` 追加、`SerializableConfig.db` フィールド追加、round-trip 永続化 |
| `src/runtime/backup_scheduler.rs` | **新規** | `run_backup_scheduler_loop`, `run_backup_scheduler_loop_with_clock`, `run_periodic_backup_once`, `Clock` trait, `RealClock`, `MockClock`（`cfg(test)`） |
| `src/runtime/mod.rs` | 変更 | `mod backup_scheduler;` 宣言、`start_channels` への spawn 追加、`Database::new` → `Database::new_with_backup` への切替 + `BackupSettings` 構築 |
| `docs/db.md` | 変更 | §5「バックアップ・復元」追加（命名規則・保存先・復元手順） |
| `docs/config.md` | 変更 | `db.backup` セクション追加 |

---

## コミット分割

1. `feat(storage): add backup module skeleton with filename generator` - `src/storage/backup.rs`（T1）
2. `feat(storage): run backup via VACUUM INTO with integrity check` - `src/storage/backup.rs`（T2/T3/T4/T5）
3. `feat(storage): prune old backup files by max_generations` - `src/storage/backup.rs`（T6/T7/T8/T9）
4. `feat(storage): compute next backup run with DST support` - `src/storage/backup.rs`（T10-T15）
5. `feat(config): add db.backup configuration section with round-trip persistence` - `src/config/types.rs`, `src/config/resolve.rs`, `src/config/loader.rs`, `src/config/persist.rs`（T16/T17/T18/T25）
6. `feat(runtime): add Database::new_with_backup for startup backup before migration` - `src/storage/mod.rs`, `src/runtime/mod.rs`（T19/T20/T21）
7. `feat(runtime): add periodic DB backup scheduler with Clock DI` - `src/runtime/backup_scheduler.rs`, `src/runtime/mod.rs`（T22/T23/T24）
8. `docs(db): document backup and restore procedures` - `docs/db.md`, `docs/config.md`

※ 同一ファイル（`src/storage/backup.rs` 等）への複数コミットは機能的関心事（命名・作成・世代管理・スケジュール計算）で独立しているため分割。順序は依存関係通り直列。

---

## 自動テスト一覧（全 25 件）

この一覧は Plan 作成時点で必要と判断した最低限の予定であり、最終テスト件数の上限ではない。実装中に追加された不安には、テストリストと TDD Cycle を追加して対応する。

### `storage::backup`（全 18 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T1 | `generate_backup_filename_uses_configured_timezone` | Step 1 | `cargo test --package egopulse generate_backup_filename` |
| T2 | `run_backup_creates_file_at_destination` | Step 2 | `cargo test --package egopulse run_backup_creates_file` |
| T3 | `run_backup_copies_all_tables_and_rows` | Step 2 | `cargo test --package egopulse run_backup_copies_all` |
| T4 | `run_backup_runs_integrity_check_on_success` | Step 2 | `cargo test --package egopulse run_backup_runs_integrity` |
| T5 | `run_backup_deletes_file_when_integrity_check_fails` | Step 2 | `cargo test --package egopulse run_backup_deletes_file` |
| T6 | `prune_old_backups_deletes_oldest_beyond_max` | Step 3 | `cargo test --package egopulse prune_old_backups_deletes_oldest` |
| T7 | `prune_old_backups_keeps_all_when_below_max` | Step 3 | `cargo test --package egopulse prune_old_backups_keeps_all` |
| T8 | `prune_old_backups_handles_missing_or_empty_dir` | Step 3 | `cargo test --package egopulse prune_old_backups_handles_missing` |
| T9 | `prune_old_backups_ignores_non_backup_files` | Step 3 | `cargo test --package egopulse prune_old_backups_ignores_non` |
| T10 | `compute_next_backup_run_first_run_returns_today_or_tomorrow` | Step 4 | `cargo test --package egopulse compute_next_backup_run_first_run` |
| T11 | `compute_next_backup_run_with_last_run_uses_interval` | Step 4 | `cargo test --package egopulse compute_next_backup_run_with_last` |
| T12 | `compute_next_backup_run_returns_none_when_disabled` | Step 4 | `cargo test --package egopulse compute_next_backup_run_returns_none_disabled` |
| T13 | `compute_next_backup_run_returns_none_for_invalid_time_format` | Step 4 | `cargo test --package egopulse compute_next_backup_run_invalid_time` |
| T14 | `compute_next_backup_run_handles_dst_gap` | Step 4 | `cargo test --package egopulse compute_next_backup_run_dst_gap` |
| T15 | `compute_next_backup_run_handles_dst_fold` | Step 4 | `cargo test --package egopulse compute_next_backup_run_dst_fold` |
| T19 | `database_new_with_backup_creates_startup_backup_before_migration` | Step 6 | `cargo test --package egopulse database_new_with_backup_creates_startup` |
| T20 | `database_new_with_backup_skips_backup_when_db_file_missing` | Step 6 | `cargo test --package egopulse database_new_with_backup_skips_missing` |
| T21 | `database_new_with_backup_continues_when_startup_backup_fails` | Step 6 | `cargo test --package egopulse database_new_with_backup_continues` |

### `config`（全 4 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T16 | `backup_config_default_when_db_section_missing` | Step 5 | `cargo test --package egopulse backup_config_default` |
| T17 | `backup_config_scheduler_enabled_returns_false_when_disabled` | Step 5 | `cargo test --package egopulse backup_config_scheduler_disabled` |
| T18 | `backup_config_rejects_zero_interval_and_generations` | Step 5 | `cargo test --package egopulse backup_config_rejects_zero` |
| T25 | `db_backup_config_round_trips_through_save_and_load` | Step 5 | `cargo test --package egopulse db_backup_config_round_trips` |

### `runtime::backup_scheduler`（全 3 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T22 | `periodic_backup_writes_last_run_to_db_meta` | Step 7 | `cargo test --package egopulse periodic_backup_writes_last_run` |
| T23 | `run_backup_scheduler_loop_with_clock_executes_backup_when_delay_elapses` | Step 7 | `cargo test --package egopulse run_backup_scheduler_loop_with_clock_executes` |
| T24 | `run_backup_scheduler_loop_exits_immediately_when_disabled` | Step 7 | `cargo test --package egopulse run_backup_scheduler_loop_exits_disabled` |

---

## 工数見積もり

| Step | 内容 | 見積もり |
| -- | -- | -- |
| Step 0 | Worktree 作成 | ~5 分 |
| Step 1 | ファイル名生成 + テスト1件（T1） | ~30 行 |
| Step 2 | `run_backup` + テスト4件（T2-T5） | ~120 行 |
| Step 3 | `prune_old_backups` + テスト4件（T6-T9） | ~80 行 |
| Step 4 | `compute_next_backup_run` + テスト6件（T10-T15） | ~120 行 |
| Step 5 | `BackupConfig` / `DatabaseConfig` + `SerializableDb` round-trip + テスト4件（T16-T18/T25） | ~100 行 |
| Step 6 | `Database::new_with_backup` + テスト3件（T19-T21） | ~80 行 |
| Step 7 | scheduler loop + `Clock` DI + テスト3件（T22-T24） | ~110 行 |
| Step 8 | 動作確認（fmt/check/clippy/test）+ 手動確認 | ~30 分 |
| Step 9 | Plan・仕様書との自己チェック | ~15 分 |
| Step 10 | PR 作成 | ~10 分 |
| **合計** | | **~640 行 + 調査・検証時間** |
