# Plan: CLI の headless モード化（`-p` 統合）

対話 UI を TUI 一本に統合し、非対話実行をルート直下の `-p` フラグに集約する。既存サブコマンド `ask` / `chat` を廃止し、`src/channels/cli.rs` の REPL を削除する。

> **Note**: 振る舞い（What）は決して変えてはいけないが、より美しい設計があれば実装方法（HOW）だけは変えてもよい。

## 設計方針

- **主要コーディングエージェント準拠**: 対話=TUI、非対話=`-p`（Claude Code `claude -p` と同型）。「引数なし=対話 / `-p`=headless」の単純なモデルにする
- **ロジックは既存流用**: ターン実行は既存の `runtime::ask`（セッションなし・永続化なし）と `agent_loop::ask_in_session`（セッションあり・永続化あり）にそのままルーティングする。本 Plan でターン処理には触れない
- **stdout 純粋化**: `-p` の出力はパイプで消費されるため、tracing ログをすべて stderr へ移す。stdout には応答テキストのみを出す
- **後方互換は追加しない**: `ask` / `chat` はエイリアス化せず削除する（AGENTS.md 規約）

## 仕様

### コマンドラインインターフェース

```
egopulse                          # TUI を起動（変更なし）
egopulse -p [PROMPT]              # headless 実行。応答を stdout に出して終了
egopulse -p --continue            # 直近セッションで実行
egopulse -p --session <NAME>      # 指定セッションで実行
--config <PATH>                   # 既存グローバルオプション（変更なし）
```

| オプション | 意味 | 制約 |
|---|---|---|
| `-p` / `--print` | headless モード | サブコマンドと排他（run/setup/gateway/sleep/events/update とは併用不可。clap が `conflicts_with_all` で弾く） |
| `--session <NAME>` | 指定セッションで実行（履歴あり・応答は永続化） | `--continue` と排他 |
| `--continue` | 直近更新セッションで実行（`list_sessions()` 先頭） | `--session` と排他。セッションが 0 件ならエラー終了 |
| `PROMPT` | プロンプト本文（位置引数） | 省略可（stdin 解決規則参照） |

### プロンプト解決規則

| 条件 | 結果 |
|---|---|
| `PROMPT` あり + stdin がパイプ | `PROMPT + "\n\n" + stdin 全文` |
| `PROMPT` あり + stdin は TTY | `PROMPT` のみ |
| `PROMPT` なし + stdin がパイプ | stdin 全文 |
| 両方なし | 使用エラー（exit 2）: `no prompt: pass PROMPT or pipe stdin` |

stdin 判定は `std::io::IsTerminal`。TUI 起動時（`-p` なし）に stdin/stdout が非 TTY でも特別扱いしない（現状維持）。

### ルーティングと出力

- ルーティング（判定は純粋関数に切り出してテスト可能にする）
  - `--session NAME` → `agent_loop::ask_in_session(config, name, prompt)`
  - `--continue` → `db.list_sessions()` 先頭のセッション名で `ask_in_session`
  - 未指定 → `runtime::ask(config, prompt)`（現行挙動維持: 会話保存なし）
- 応答は `println!("assistant: {response}")` ではなく `{response}\n` を素で stdout へ（パイプ消費者向けに装飾しない）
- エラー時: メッセージを stderr へ、exit 1。`EgoPulseError::ShutdownRequested`（Ctrl+C）は exit 0
- ログ: `init_logging` の subscriber writer を stderr に変更（全モード共通）

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成 → レビュー待機・レビューバック

## 対象一覧

| 対象 | 種別 | 既存パターン / 参照元 | 備考 |
| --- | --- | --- | --- |
| `src/main.rs` | 変更 | 既存 clap derive 定義 | `Command::Ask` / `Command::Chat` 削除、root に `-p` 系定義追加 |
| `src/channels/cli.rs` | **削除** | — | REPL ごと削除。`lib.rs` の `mod cli` も削除 |
| `src/runtime/logging.rs` | 変更 | `tracing_subscriber::fmt()` | `.with_writer(std::io::stderr)` 追加 |
| `docs/commands.md` | 変更 | コマンド一覧表 | `ask` / `chat` 行を `-p` 仕様に置換 |
| `docs/channels.md` | 変更 | CLI 章 | REPL 記述を headless 仕様へ書き換え |
| `AGENTS.md` | 変更 | エントリポイント列記 | `chat / run / ask / setup / gateway` 表記を更新 |

対象外: `src/tools/send_attachment.rs` 内の `"cli"` チャネルフィクスチャ（チャネル解決のテスト用命名であり REPL と無関係）、`normalize_date_input_*`（events 用のため残置）。

---

## Step 0: Worktree 作成

- ブランチ名: `feat/cli-headless-mode`
- `worktree-create` skill を使用

---

## Step 1: clap 定義と引数パース (TDD)

### RED: テスト先行

| テストケース | 内容 |
| --- | --- |
| `parse_print_with_positional_prompt` | `egopulse -p "hello"` が print=true, prompt=Some("hello") |
| `parse_print_without_prompt` | `egopulse -p` が print=true, prompt=None |
| `parse_session_and_continue_conflict` | `-p --session a --continue` はパースエラー |
| `parse_print_conflicts_with_subcommands` | `-p run` 等はパースエラー |
| `prompt_without_print_rejected` | 位置引数 PROMPT は `-p` なしでは指定不可（clap requires） |

### GREEN: 最小実装

`main.rs` の `Cli` 構造体を再構成: root に `#[arg(short, long)] print`, `#[arg(requires = "print")] prompt: Option<String>`, `--session`, `--continue`（clap `conflicts_with`）。`Command::Ask` / `Chat` バリアント削除。`run_with_config` の分岐を headless 関数呼び出しに置換。

### コミット

`feat!: replace ask/chat subcommands with -p flag`

---

## Step 2: プロンプト解決 (TDD)

### RED: テスト先行

| テストケース | 内容 |
| --- | --- |
| `resolve_joins_positional_and_stdin` | positional + パイプ入力 → `"a\n\nb"` |
| `resolve_uses_stdin_only` | positional None + パイプ入力 → stdin 全文 |
| `resolve_errors_when_no_input` | 両方なし → エラー |

### GREEN: 最小実装

純粋関数 `resolve_prompt(position: Option<String>, stdin_text: Option<String>) -> Result<String, HeadlessError>` を新設（stdin 読み取りは呼び出し側で分離）。`HeadlessError` は thiserror。

### コミット

`feat: resolve -p prompt from argument and stdin`

---

## Step 3: ルーティング (TDD)

### RED: テスト先行

| テストケース | 内容 |
| --- | --- |
| `route_prefers_explicit_session` | `--session a` → Session("a") |
| `route_continue_resolves_latest` | `--continue` + セッション一覧先頭 "s1" → Session("s1") |
| `route_continue_errors_when_no_sessions` | セッション 0 件 → エラー |
| `route_defaults_to_one_shot` | 未指定 → OneShot |

### GREEN: 最小実装

`enum AskTarget { Session(String), OneShot }` と決定関数 `select_ask_target(session, cont, summaries) -> Result<AskTarget, ...>`。実行側は `ask_in_session` / `runtime::ask` に委譲するのみ。

### コミット

`feat: route -p execution target`

---

## Step 4: ログの stderr 移行とクリーンアップ

- `logging.rs`: `fmt()` に `.with_writer(std::io::stderr)` 追加
- `channels/cli.rs` 削除、`lib.rs` の `mod cli` 削除、`use egopulse::channels::cli` 削除
- docs 3 点 + `AGENTS.md` 更新
- コミット: `refactor: move tracing output to stderr and drop cli repl`

---

## Step N+1: 動作確認

```bash
cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test
echo "log tail" | ./target/debug/egopulse -p --continue "要約して" | cat   # 応答のみ stdout
./target/debug/egopulse -p ; echo $?                                       # exit 2
```

- 手動確認: パイプ入力・`--session`・`--continue`・Ctrl+C（exit 0）・TUI 起動の回帰確認
- 失敗時は該当 Step へ戻る

---

## Step N+2: Plan・仕様書との自己レビュー

**目的**: 自分のコンテキスト（本 Plan の意図）を活かし、CodeRabbit の前に実装不正を潰す。完了は「目的を達成したとき」であり、チェックリスト埋めは完了ではない。**「何も見つからなかった」は未レビュー扱いでやり直す。**

観点（補助）: テストが約束した振る舞いを assert しているか / 可視性最小か / `cli.rs` 由来のデッドコードが残っていないか / docs・AGENTS.md と実装の一致 / diff と対象一覧の照合。

---

## Step N+3: PR 作成

- PR タイトル: `feat: unify interactive UI on TUI and add headless -p mode`
- description は日本語で概要・テスト結果・破壊的変更（`ask`/`chat` 廃止）を記載

---

## Step N+4 / N+5: レビューバック

- push 後 `sleep 15m` → `pr-review-back-workflow` skill 実行（レビュー無ければ `sleep 5m` 再試行、最大 2 回）
- 対応 push 後も同様に再レビューバックを実施

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
| --- | --- | --- |
| `src/main.rs` | 変更 | clap 再構成、headless 実装 |
| `src/channels/cli.rs` | **削除** | REPL 削除 |
| `src/lib.rs` | 変更 | `mod cli` 削除 |
| `src/runtime/logging.rs` | 変更 | stderr writer |
| `docs/commands.md` / `docs/channels.md` / `AGENTS.md` | 変更 | 仕様反映 |

## 自動テスト一覧（全 12 件）

| ID | テスト名 | Step |
| --- | --- | --- |
| T1–T5 | `parse_*` 5 件 | Step 1 |
| T6–T8 | `resolve_*` 3 件 | Step 2 |
| T9–T12 | `route_*` 4 件 | Step 3 |

※ 実装中の不安はテストリストへ追加し Cycle を継続する（上限ではない）。

## 工数見積もり

| Step | 内容 | 見積もり |
| --- | --- | --- |
| Step 1–3 | TDD Cycle ×3 | ~250 行 |
| Step 4 | クリーンアップ・docs | ~150 行差し替え |
| Step N+1 以降 | 検証・自己レビュー・PR・レビューバック | ~50 行 |
| **合計** |  | **~450 行** |
