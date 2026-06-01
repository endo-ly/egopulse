# Plan: src/lib.rs の pub 公開範囲を app facade に集約

> **Note**: 振る舞い（What）は決して変えてはいけないが、より美しい設計があれば実装方法（HOW）だけは変えてもよい。

## 設計方針

- **app facade パターン**: `pub mod app` を `src/lib.rs` に追加。`main.rs` から必要な型・関数を **アイテム単位** で `pub use` 再エクスポートする
- **既存モジュールは全て `pub(crate)` 化**: 単一バイナリ中心のクレートでは外部公開の安定性責任を持つ範囲は限定的にすべきという private-first 方針に沿う
- **モジュール丸ごと re-export は禁止**: Rust の E0365 制約により、`pub(crate) mod X` を `pub use crate::X` で再エクスポートできない。代わりに `pub mod X { pub use crate::X::specific_pub_item; }` の形で **同名パブリックモジュール内にアイテム単位で再エクスポート** する
- **個別 `pub` アイテムも見直す**: モジュールの `pub(crate)` 化だけでなく、その中身の `pub fn` / `pub struct` も必要に応じて `pub(crate)` 化する
- **main.rs の import パスを facade 経由に揃える**: `egopulse::agent_loop` → `egopulse::app::agent_loop` 等
- **既存テストは全て通す**: 振る舞いを変えない refactor
- **新テストの追加は最小限**: 公開範囲の安定性は cargo doc/clippy で担保。新規単体テストは追加しない
- **関連 docs 更新**: `docs/directory.md` に新モジュール構成を反映

## TDD 方針

本 Plan は振る舞いを変えない refactor であり、新規自動テストは追加しない。代わりに「main.rs のコンパイル成功」と「既存テスト全件 PASS」を不変条件とし、各 Step 後に `cargo check --all-targets` と `cargo test` で即時フィードバックを得る。最終 Step で `cargo doc --no-deps -D warnings`, `cargo clippy --all-targets --all-features -- -D warnings` を含む完全検証を実施する。テストリスト項目（T1〜Tn）は公開 API 表面の不変条件として扱う。

## Plan スコープ

WT作成 → 実装 → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 種別 | 既存パターン / 参照元 | 備考 |
| -- | -- | -- | -- |
| `src/lib.rs` | 変更 | 現状 7 つの `pub mod` + 3 つの `pub(crate) mod` | `pub mod app;` を追加し、他を `pub(crate)` 化 |
| `src/app.rs` | **新規** | なし | アイテム単位 re-export の `pub mod` 群 |
| `src/main.rs` | 変更 | `use egopulse::agent_loop;` 等 8 行 | `use egopulse::app::agent_loop;` 経由に置換 |
| `src/runtime/mod.rs` | 変更 | `pub mod gateway; pub mod logging; pub mod status;` | サブモジュールを `pub(crate)` 化 |
| `src/runtime/metrics.rs` | 変更 | `pub fn init_metrics / metrics_output` | `pub(crate) fn` に降格（モジュール自体が `pub(crate)`） |
| `src/storage/mod.rs` | 変更 | `pub struct Database` | `pub(crate) struct Database` に降格 |
| `src/channels/mod.rs` | 変更 | `pub mod cli;` のみ公開 | `pub(crate) mod cli;` に降格 |
| `src/tools/sanitizer.rs` | 変更 | doc 内の `[REDACTED]` が doc link と誤認 | `\[REDACTED\]` に escape |
| `src/storage/queries.rs` | 変更 | doc link `[`resolve_channel_log_chat_id`]` 切れ | crate 内の pub 経由のリンクに修正 |
| `src/tools/send_message.rs` | 変更 | doc link `[`ChatInfo`]` 切れ | crate 内の pub 経由のリンクに修正 |
| `docs/directory.md` | 変更 | `src/app.rs` 未記載 | 追加 |

## テストリスト / 不安リスト

| ID | 観点 | 期待する振る舞い | 優先 | 対応するCycle | 状態 / 今回対象外理由 |
| -- | -- | -- | -- | -- | -- |
| T1 | ビルド | `cargo build --all-targets` がエラーなく通る | High | Step 1〜3 全般 | 未着手 |
| T2 | テスト回帰 | 既存 `cargo test` が全件 PASS | High | Step 4 完了 | 未着手 |
| T3 | 公開API | `egopulse::app::*` 配下が main.rs で使う全 API を網羅 | High | Step 1 | 未着手 |
| T4 | 公開境界 | `egopulse::` 直下の `pub mod` が `app`, `test_util` のみ（`test_util` は `cfg(test)`） | High | Step 3 | 未着手 |
| T5 | doc lint | `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` が成功 | High | Step 5 | 未着手 |
| T6 | clippy lint | `cargo clippy --all-targets --all-features -- -D warnings` が成功 | High | Step 6 | 未着手 |
| T7 | 公開範囲起因の dead code | 縮小後にも新規 `dead_code` warning が出ない | High | Step 3, 4 完了後 | 未着手 |
| T8 | 既存公開型の互換 | `EgoPulseError`, `ConfigError`, `SleepRunTrigger`, `GatewayAction`, `SleepBatchError` は facade 経由で引き続き使用可能 | High | Step 1, 2 | 未着手 |
| T9 | E0365 回避 | モジュール丸ごと re-export せずアイテム単位 re-export になっている | High | Step 1, 3 | 未着手 |
| T10 | `runtime::status` 参照 | `tools/mcp.rs:187, 299` からの `crate::runtime::status::*` 参照が `pub(crate)` でアクセス可能 | High | Step 3 | 未着手 |

---

## Step 0: Worktree 作成

- ブランチ名: `refactor/narrow-public-surface`
- 作成コマンド:
  - `git worktree add ./wt-narrow-public-surface -b refactor/narrow-public-surface origin/main`
- 状態: **作成済み**（`/root/workspace/egopulse/wt-narrow-public-surface`）

---

## Step 1: `pub mod app` facade 導入（アイテム単位 re-export）

### この Step の目的

main.rs が使う API のみを集約した `pub mod app` を `src/lib.rs` に追加。アイテム単位 re-export で main.rs の必要 API を全てカバーする。

### 今回選ぶ項目

- 対象: T3, T8, T9
- 選ぶ理由: アイテム単位 re-export のパターンを `cargo build` 成功で保証。E0365 回避の最小動作確認単位。
- この時点では扱わないこと: 既存モジュールの `pub(crate)` 化（Step 3 で実施）

### RED → GREEN → REFACTOR

- **RED**: `src/app.rs` を新規作成。以下の構造で `pub mod` を作り、各中でアイテム単位 `pub use` する:

  ```rust
  //! Public facade for the egopulse binary entrypoint.
  //!
  //! Re-exports the API surface used by the CLI binary. The internal
  //! modules remain `pub(crate)`; this facade is the single point of
  //! contact for the binary entrypoint.

  pub mod agent_loop {
      pub use crate::agent_loop::ask_in_session;
  }

  pub mod channels {
      pub mod cli {
          pub use crate::channels::cli::run_chat;
      }
  }

  pub mod config {
      pub use crate::config::{default_config_path, Config};
  }

  pub mod error {
      pub use crate::error::{ConfigError, EgoPulseError};
  }

  pub mod runtime {
      pub use crate::runtime::{
          ask, build_app_state_with_path, build_sleep_app_state_with_path,
          run_tui, start_channels,
      };
      pub mod gateway {
          pub use crate::runtime::gateway::{
              resolve_cli_config_path, run_gateway, GatewayAction,
          };
      }
      pub mod logging {
          pub use crate::runtime::logging::init_logging;
      }
  }

  pub mod setup {
      pub use crate::setup::run_setup_wizard;
  }

  pub mod sleep {
      pub use crate::sleep::{run_events_extract, run_sleep_batch, SleepBatchError};
  }

  pub mod storage {
      pub use crate::storage::SleepRunTrigger;
  }
  ```

- **GREEN**: `src/lib.rs` に `pub mod app;` を追加。`cargo build --bin egopulse` で main.rs 側は旧パスのため失敗する想定。`cargo check --lib` は通るはず
- **REFACTOR**: 各 re-export に doc コメントを追加。`pub use` の順序をアルファベット順に統一

### 注意点

- **E0364/E0365 回避**: 必ず「アイテム単位 re-export」とする。`pub use crate::module_path;` の形でモジュール自体を再エクスポートしない
- **再エクスポート対象が `pub(crate)` の場合は不可**: `pub fn some_helper`（pub(crate)）を `pub use` すると E0364 になる。`pub fn` のみ re-export 可能

### テストリスト更新

- 完了: T3（app.rs の存在・構造）、T8（再エクスポート網羅）、T9（アイテム単位 re-export）
- 追加: なし
- 次候補: T1（main.rs 移行後のビルド成功）

### コミット

`refactor(lib): introduce app facade module`

---

## Step 2: main.rs を app facade 経由に移行

### この Step の目的

`main.rs` の use 群を facade 経由に置換し、facade を介したビルド成功を保証する。

### 今回選ぶ項目

- 対象: T1
- 選ぶ理由: main.rs のコンパイル成功が facade 設計の正しさを担保するため。
- この時点では扱わないこと: 旧パスの削除（main.rs 移行後に即削除する）

### RED → GREEN → REFACTOR

- **RED**: `src/main.rs` の use 群を以下に置換:

  ```rust
  use std::path::PathBuf;

  use chrono::Datelike;
  use chrono::TimeZone;
  use clap::{Parser, Subcommand};
  use egopulse::app::agent_loop;
  use egopulse::app::channels::cli;
  use egopulse::app::config::{default_config_path, Config};
  use egopulse::app::error::{ConfigError, EgoPulseError};
  use egopulse::app::runtime;
  use egopulse::app::runtime::gateway::{self, GatewayAction};
  use egopulse::app::runtime::logging::init_logging;
  use egopulse::app::setup;
  ```

- **GREEN**: `cargo build --all-targets` で成功
- **REFACTOR**: use グルーピングを整理（アルファベット順、`use` のグルーピング慣行に従う）

### テストリスト更新

- 完了: T1
- 追加: なし
- 次候補: T4, T7

### コミット

`refactor(main): use app facade for binary entrypoint`

---

## Step 3: 既存モジュールの `pub mod` → `pub(crate) mod` 化

### この Step の目的

Step 1〜2 で facade 経由のビルドが成功しているため、既存モジュールの `pub mod` を `pub(crate) mod` に降格しても main.rs は壊れない（crate 内部アクセスは維持される）。

### 今回選ぶ項目

- 対象: T4, T7, T10
- 選ぶ理由: 公開境界を最小化する本 refactor のコア。`cargo check --all-targets` で即時検証。
- この時点では扱わないこと: 個別 `pub` アイテムの降格（Step 4）

### RED → GREEN → REFACTOR

- **RED**: 変更前の状態（`pub mod agent_loop` 等 7 モジュール + サブモジュール `pub mod gateway, logging, status` + `pub mod cli`）で cargo build 成功を確認
- **GREEN**:
  - `src/lib.rs`: `pub mod agent_loop;` → `pub(crate) mod agent_loop;`（他 6 つも同様: channels, config, error, runtime, setup, sleep, storage）
  - `src/runtime/mod.rs`: `pub mod gateway;` → `pub(crate) mod gateway;`、`pub mod logging;` → `pub(crate) mod logging;`、`pub mod status;` → `pub(crate) mod status;`（**`mod status` には降格しない**：`tools/mcp.rs:187, 299` から `crate::runtime::status::*` への参照があり、`pub(crate)` がないと sibling module からアクセス不可）
  - `src/channels/mod.rs`: `pub mod cli;` → `pub(crate) mod cli;`
- **REFACTOR**: `pub(crate)` を `crate` 経由アクセスに統一できているか再確認

### 注意点

- **`runtime::status` を `mod status`（完全 private）にしない**: tools/mcp.rs から `crate::runtime::status::TransportType`, `crate::runtime::status::McpStatus` への参照あり。`pub(crate) mod status` にとどめる
- **facade 経由のアクセスは維持**: 内部モジュールが `pub(crate)` でも、`app` モジュール内の `pub use` 経由で外部公開は維持される

### テストリスト更新

- 完了: T4, T7, T10
- 追加: なし
- 次候補: T7 詳細（個別 `pub` アイテム）

### コミット

`refactor(lib): narrow module visibility to pub(crate)`

---

## Step 4: 個別 `pub` アイテムの `pub(crate)` 降格

### この Step の目的

モジュール自体が `pub(crate)` 化された後に、その中で過剰に `pub` になっている関数・型を降格する。cargo dead_code lint を活用。

### 今回選ぶ項目

- 対象: T7（詳細）
- 選ぶ理由: clippy / dead_code で機械的に発見できる過剰公開を確実に潰す。
- この時点では扱わないこと: 内部エラー型（Step 4 完了後に cargo doc -D warnings で再評価）

### RED → GREEN → REFACTOR

- **RED**: `cargo build` と `cargo check --all-targets` で `pub fn init_metrics` / `pub fn metrics_output` への参照（crate 内の `channels/web/health.rs` の `metrics::metrics_output()` 等）が `pub(crate)` でも解決できることを確認
- **GREEN**:
  - `src/runtime/metrics.rs`: `pub fn init_metrics` → `pub(crate) fn init_metrics`、同様に `pub fn metrics_output` → `pub(crate) fn metrics_output`（モジュール自体は既に `pub(crate)`）
  - `src/storage/mod.rs`: `pub struct Database` → `pub(crate) struct Database`（外部使用なし、AppState 経由でのみアクセス）
- **REFACTOR**: 残った `pub` アイテムを `rg "^pub " src/` で再走査して過不足を最終確認

### テストリスト更新

- 完了: T7（完全）
- 追加: なし
- 次候補: T5（doc lint）

### コミット

`refactor(storage,metrics): tighten pub items no longer exposed externally`

---

## Step 5: 既存 doc lint（broken link 切れ）の修正

### この Step の目的

`cargo doc --no-deps` で残っている 5 つの警告を潰し、`-D warnings` での clean 化に備える。

### 今回選ぶ項目

- 対象: T5
- 選ぶ理由: 公開範囲縮小後に doc lint を strict 化する場合に既存 warning がノイズになるため、先に潰す。
- この時点では扱わないこと: 公開範囲起因の新規 doc warning（Step 4 の doc コメント追加で吸収）

### RED → GREEN → REFACTOR

- **RED**: `cargo doc --no-deps` で 5 警告出る状態を確認
  - `src/tools/sanitizer.rs:10` `[REDACTED]` 誤認
  - `src/storage/queries.rs:581` `[`resolve_channel_log_chat_id`]` 切れ
  - `src/tools/send_message.rs:162` `[`ChatInfo`]` 切れ
  - その他 2 件
- **GREEN**:
  - `src/tools/sanitizer.rs:10`: `[REDACTED]` → `\[REDACTED\]`
  - `src/storage/queries.rs:581`: `pub(crate)` 化された `resolve_channel_log_chat_id` へのリンクを crate パス経由 (`[`crate::storage::Database::resolve_channel_log_chat_id`]`) または doc link 削除
  - `src/tools/send_message.rs:162`: 同様に crate パス経由に修正
  - 残り 2 件も同様に対応
- **REFACTOR**: 他の doc 内 `[]` パターンも `rg` で走査し再発防止

### テストリスト更新

- 完了: T5
- 追加: なし
- 次候補: T6（clippy）

### コミット

`docs: fix broken intra-doc links surfaced by visibility tightening`

---

## Step 6: 最終検証（動作確認）

### この Step の目的

全 lint・test・doc・build が警告ゼロで通ることを保証する。

### 検証コマンド

```bash
# 1. フォーマット
cargo fmt --check

# 2. ビルド（feature 全有効）
cargo build --all-targets --all-features

# 3. clippy
cargo clippy --all-targets --all-features -- -D warnings

# 4. テスト
cargo test --all-features

# 5. doc lint strict
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps

# 6. 公開 API 境界の手動確認
rg "^pub mod" src/lib.rs  # app と test_util (cfg test) のみであることを確認
rg "^pub mod" src/runtime/mod.rs src/channels/mod.rs  # pub(crate) のみであることを確認
```

### 失敗時の対応

- clippy warning → 即修正（`#[allow(...)]` は禁止、根本対応）
- test 失敗 → 直前の Step の変更を疑い、`git diff` でリグレッションを確認
- doc warning → Step 5 を再実施

### 完了条件

- 上記 6 コマンドがすべて exit 0
- `rg "^pub mod" src/lib.rs` の結果が `pub mod app;` と `pub mod test_util;`（cfg test）のみ

---

## Step 7: コミット追加（docs 更新）& PR 作成

### 変更ファイル

- `docs/directory.md`: `src/app.rs` の追記

### コミット

`docs(directory): document new app facade module`

### PR 作成

- **PR タイトル**: `refactor(lib): narrow public surface via app facade`
- **PR description**:
  - 概要: `src/lib.rs` の `pub mod` 公開範囲を `pub mod app` facade に集約。main.rs は facade 経由のみ参照。private-first 方針に沿う。
  - 背景: 単一バイナリ中心のクレートで過剰な公開境界が API 互換責任と将来のリファクタ制約を生んでいたため。
  - 変更内容:
    - `pub mod app` を新規追加（main.rs の必要 API をアイテム単位 re-export）
    - 既存 7 モジュールを `pub(crate)` 化
    - 個別 `pub` アイテム（`Database`, `init_metrics`, `metrics_output`）も `pub(crate)` 化
    - 既存 doc lint（broken link 3 件以上）修正
    - `docs/directory.md` 更新
  - テスト: `cargo test --all-features` 全件 PASS
  - 検証: `cargo clippy --all-targets --all-features -- -D warnings`, `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` いずれも警告ゼロ
  - 振る舞い変更: なし（pure refactor）
  - 設計上の注意点: モジュール丸ごと re-export（`pub use crate::X`）は Rust E0365 で不可のため、アイテム単位 re-export としている

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
| ---- | ---- | -- |
| `src/lib.rs` | 変更 | `pub mod app;` 追加、他 7 モジュールを `pub(crate)` 化 |
| `src/app.rs` | **新規** | main.rs が必要とする API を `pub mod { pub use crate::X::item; }` 形式で再エクスポート |
| `src/main.rs` | 変更 | `use egopulse::agent_loop;` 等を `use egopulse::app::agent_loop;` 等に置換 |
| `src/runtime/mod.rs` | 変更 | `pub mod gateway/logging/status;` → `pub(crate) mod gateway/logging/status;` |
| `src/runtime/metrics.rs` | 変更 | `pub fn init_metrics/metrics_output` → `pub(crate) fn` |
| `src/storage/mod.rs` | 変更 | `pub struct Database` → `pub(crate) struct Database` |
| `src/channels/mod.rs` | 変更 | `pub mod cli;` → `pub(crate) mod cli;` |
| `src/tools/sanitizer.rs` | 変更 | `[REDACTED]` → `\[REDACTED\]` |
| `src/storage/queries.rs` | 変更 | 壊れた intra-doc link の修正 |
| `src/tools/send_message.rs` | 変更 | 壊れた intra-doc link の修正 |
| `docs/directory.md` | 変更 | `src/app.rs` を追記 |

## コミット分割

1. `refactor(lib): introduce app facade module` - `src/lib.rs` + 新規 `src/app.rs`
2. `refactor(main): use app facade for binary entrypoint` - `src/main.rs`
3. `refactor(lib): narrow module visibility to pub(crate)` - `src/lib.rs`, `src/runtime/mod.rs`, `src/channels/mod.rs`
4. `refactor(storage,metrics): tighten pub items no longer exposed externally` - `src/runtime/metrics.rs`, `src/storage/mod.rs`
5. `docs: fix broken intra-doc links surfaced by visibility tightening` - `src/tools/sanitizer.rs`, `src/storage/queries.rs`, `src/tools/send_message.rs` 等
6. `docs(directory): document new app facade module` - `docs/directory.md`

## 自動テスト一覧

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T1 | （新規追加なし）| Step 2 検証 | `cargo build --all-targets --all-features` |
| T2 | （新規追加なし、既存テストの回帰確認）| Step 6 | `cargo test --all-features` |
| T3 | （新規追加なし、main.rs のコンパイルで担保）| Step 1 | `cargo build --bin egopulse` |
| T4 | （手動確認）| Step 3 | `rg "^pub mod" src/lib.rs` |
| T5 | （新規追加なし、doc lint で担保）| Step 5 | `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` |
| T6 | （新規追加なし、clippy で担保）| Step 6 | `cargo clippy --all-targets --all-features -- -D warnings` |
| T7 | （新規追加なし、clippy dead_code で担保）| Step 3, 4 | 同上 |
| T8 | （main.rs 移行後の binary build で担保）| Step 2 | `cargo build --bin egopulse` |
| T9 | （Rust コンパイラの E0365 で担保）| Step 1, 3 | `cargo build --lib` |
| T10 | （tools/mcp.rs からの参照で担保）| Step 3 | `cargo build --all-targets` |

## 工数見積もり

| Step | 内容 | 見積もり |
| -- | -- | -- |
| Step 1 | app.rs 新規 + lib.rs 修正 | ~50 行追加・修正 |
| Step 2 | main.rs use 置換 | ~10 行修正 |
| Step 3 | 既存 8 モジュールを `pub(crate)` 化 | ~10 行修正 |
| Step 4 | 個別 pub アイテム降格 | ~3 行修正 |
| Step 5 | doc lint 修正 | ~10 行修正 |
| Step 6 | 最終検証 | 5 分 |
| Step 7 | コミット & PR | 10 分 |
| **合計** |  | **約 80 行の差分、合計 1 時間程度** |
