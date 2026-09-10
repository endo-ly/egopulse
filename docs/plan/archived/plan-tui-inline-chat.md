# Plan: TUI 対話 UI 刷新（Inline Viewport + ストリーミング）

TUI を alternate screen のフルスクリーン型から、主要コーディングエージェント（Claude Code / Codex CLI）と同型の「下端固定入力ボックス + ターミナル本来のスクロールバックに会話が流れる」対話 UI へ作り直す。ratatui 0.30 / crossterm 0.29 は維持し、`Viewport::Inline` + `insert_before` 描画モデルへ移行する。

> **Note**: 振る舞い（What）は決して変えてはいけないが、より美しい設計があれば実装方法（HOW）だけは変えてもよい。

## 設計方針

- **ライブラリ維持**: ratatui 自体が Inline viewport 用の `Terminal::insert_before`（ratatui-core 0.1.0 `src/terminal/terminal.rs`）を提供しており、ライブラリ交代ではなく描画モデル転換で目的を達成する
- **既存配信経路に乗る**: ストリーミングは Web チャネルと同じ `agent_loop::process_turn_with_events(state, context, prompt, on_event)` を使用する。ターン永続化・スケジューリングには触れない
- **tui-textarea 不採用**: 最新版（git main 含む）が ratatui 0.29 依存のため、本プロジェクトの ratatui 0.30 と共存できない。入力エディタは自前実装（`composer`）とし、状態機械として単体テスト可能に保つ
- **状態と描画の分離**: `composer` / `transcript` / `sessions` は描画に依存しない純粋な状態機械として実装し TDD 対象にする。`draw` は薄く保ちテスト対象外とする
- **前提**: `docs/plan/plan-cli-headless-mode.md`（tracing 出力の stderr 移行）を先に取り込んでいることを推奨

## 仕様

### 1. 画面構成と描画モデル

```
... （ターミナル本来のスクロールバック: 確定済みブロックが流れていく）
──────────────────────────────────────────────
 ▸ アクティブターン領域（ストリーミング末尾・ツールカード・spinner） ← ratatui 管理
 > 入力コンポーザ（1〜8 行可変のボックス）
 model/agent/iteration 状態行
 キーバインドヒント行
────────────────────────────────────────────── ← 端末下端
```

- 初期化: `Terminal::with_options(CrosstermBackend::new(stdout), TerminalOptions { viewport: Viewport::Inline(height) })`。alternate screen は使用しない
- 確定出力: ユーザー発言・応答本文・ツールカード確定形は styled `Vec<Line>` に変換し `terminal.insert_before()` で一度だけ出力する。再描画対象は常に下端 viewport のみ
- viewport 高さ = アクティブターン領域 + composer 高さ + 状態 2 行。`/sessions` オーバーレイ中は最大 20 行へ一時拡張する
- 端末高さ 10 行未満では起動時にエラー終了する
- resize は crossterm `Event::Resize` 受信後の draw 内 `autoresize()` で追従
- 終了時クリーンアップ（Drop 含む）: raw mode 解除、カーソル表示、viewport 領域クリア、下端への改行

### 2. モジュール構成

`src/channels/tui.rs`（1008 行）を削除し、以下へ分割する。

| モジュール | 責務 | テスト |
| --- | --- | --- |
| `channels/tui/mod.rs` | `run(state)` エントリ、ターミナル初期化/復帰、メインループ | 手動 |
| `channels/tui/event.rs` | crossterm `EventStream` → `UiEvent` 変換 | 手動 |
| `channels/tui/composer.rs` | 入力状態機械（編集・履歴・補完候補保持） | 単体 |
| `channels/tui/transcript.rs` | 会話ブロック列 + アクティブターン状態。`AgentEvent` 適用 | 単体 |
| `channels/tui/markdown.rs` | Markdown → styled `Vec<Line>` 変換 | 単体 |
| `channels/tui/sessions.rs` | `/sessions` オーバーレイ状態機械 | 単体 |
| `channels/tui/draw.rs` | viewport 描画全般 | 単体（`TestBackend` で描画結果を assert） |

### 3. イベントループ

```text
loop {
    tokio::select! {
        Some(Ok(ev)) = event_stream.next() => UiEvent へ変換して適用,
        Some(agent_event) = turn_rx.recv()   => transcript に適用,
        _ = shutdown_rx.recv()               => break,
    }
    redraw if dirty;
}
```

- 200ms ポーリングを廃止し、crossterm `event-stream` feature の `EventStream` を使用する
- ターンイベントは送信時に `tokio::spawn(process_turn_with_events(...))` し、`on_event` クロージャから `mpsc::UnboundedSender<AgentEvent>` へ転送する
- 再描画は Delta ごとに行わず dirty フラグ方式とし、最大 60fps に間引いて coalesce する（高頻度トークン到達時のフリッカー・CPU 消費防止）
- 同時ターンは 1 件。実行中の追加 Enter 送信は保留キュー（1 件のみ）に入り、完了後に自動送信される

### 4. コンポーザ仕様

| キー | 動作 |
| --- | --- |
| Enter | 補完ポップアップ表示中なら候補確定。それ以外は送信（空入力は無視） |
| Alt+Enter / Shift+Enter | 改行（ターミナル依存の best effort） |
| Left / Right | 文字単位移動 |
| Up / Down | 行間移動。1 行目での Up / 最終行での Down は入力履歴移動（現入力はドラフト保存、既存挙動踏襲） |
| Home / Ctrl+A | 行頭 |
| End / Ctrl+E | 行末 |
| Ctrl+Left / Alt+B / Ctrl+Right / Alt+F | 単語境界移動（空白と記号区切り、char 単位） |
| Backspace / Delete | 1 文字削除 |
| Ctrl+W | 直前単語削除 |
| Ctrl+U | 行頭まで削除 |
| Ctrl+K | 行末まで削除 |
| Tab | 補完候補確定 |
| Esc | 補完ポップアップを閉じる |
| Ctrl+C | 空入力 → 終了。非空 → 入力全消去 |
| Bracketed paste | テキストとしてそのまま挿入（改行維持） |

- すべて char 単位で UTF-8 安全に扱う（既存 `char_to_byte_index` 相当を継承）。grapheme クラスタ精密編集は非目標
- 送信済みプロンプトの入力履歴はメモリ内のみ（セッション横断の永続化はしない）

### 5. ターン実行とストリーミング表示

`AgentEvent` → UI 状態遷移:

| イベント | UI 遷移 |
| --- | --- |
| （送信時） | ユーザー発言ブロックを即 commit（insert_before）、アクティブターン開始 + spinner |
| `Iteration { n }` | 状態行に iteration 表示 |
| `Delta { text }` | streaming バッファ追記、アクティブ領域へ末尾数行を plain text 表示 |
| `ToolStart { call_id, name, input }` | ツールカード追加（⟳ + 名前 + input 要約 1 行） |
| `ToolResult { call_id, is_error, preview, duration_ms }` | カード確定（✓ / ✗ + preview 1 行 + 所要時間） |
| `FinalResponse { text }` | streaming バッファ破棄し、`text` を Markdown 描画して commit、アクティブ終了 |
| `Error { message }` | エラーブロック commit、アクティブ終了 |

応答はターンパイプライン内で永続化済みのため、TUI 側の保存処理は追加しない。

### 6. Markdown レンダリング

- `pulldown-cmark` でパースし styled `Line` 列へ変換。スタイル: Heading=太字+下線、List=インデント+bullet、code span=黄、blockquote=`> ` prefix+dim、リンク=テキスト+dim URL
- fenced code block は `syntect`（`default-fancy`、C 依存なし）でハイライト。未知言語は plain
- 幅は insert_before 時点の端末幅でハードラップ（`unicode-width` で表示幅計算）
- ストリーミング中表示は plain text のまま（パフォーマンス優先）。FinalResponse 時のみ Markdown 化する

### 7. スラッシュコマンド補完

- `slash_commands.rs` にカタログ API を新設: `SlashCommandSpec { names, usage, description }` と `pub(crate) const SLASH_COMMANDS: &[SlashCommandSpec]`（new / compact / status / skills / restart / providers / provider / models / model）
- dispatch 実装をこのカタログ基準に整理する（振る舞い変更なし）
- コンポーザ先頭が `/` の 1 語目入力中、prefix 一致候補をポップアップ表示（↑↓ 選択、Tab 確定、Esc 閉じる）

### 8. セッション選択

- 起動時: `--session <NAME>` 指定があればそれを開く。未指定なら `list_sessions()` 先頭（直近）を開く。0 件なら新規コンテキスト
- `/sessions`: viewport を拡張したオーバーレイで一覧表示。j/k・↑↓ 移動、Enter 切替、n 新規、Esc 閉じる。既存 Browser ビューの選択ロジックを流用
- 切替時は `SurfaceContext` を構築し直し、既存履歴を `load_session_messages` で読み込む

### 9. ロギングと非目標

- tracing は stderr（別 Plan 済み）。raw mode 中の warn 以上の出力で viewport 外が一瞬崩れても次回 draw で自己修復することを許容とする
- **非目標**: ターン割り込み（Esc abort、cancel API 未整備のため別 Plan）、マウス独自処理、画像・ファイル添付、IME 変換中プレビュー、Windows 固有調整、alt-screen モードの併存

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成 → レビュー待機・レビューバック

## 対象一覧

| 対象 | 種別 | 既存パターン / 参照元 | 備考 |
| --- | --- | --- | --- |
| `Cargo.toml` | 変更 | 既存依存定義 | crossterm `event-stream`、pulldown-cmark、syntect(default-fancy)、unicode-width 追加 |
| `src/channels/tui.rs` | **削除** | — | 新モジュール群へ置換 |
| `src/channels/tui/{mod,event,composer,transcript,markdown,sessions,draw}.rs` | **新規** | 旧 tui.rs の状態定義・UTF-8 処理を移植 | §2 参照 |
| `src/slash_commands.rs` | 変更 | 既存 dispatch 表 | カタログ API 新設（§7） |
| `src/main.rs` | 変更 | clap 定義 | TUI 用 `--session` オプション |
| `docs/channels.md` / `docs/commands.md` / `docs/directory.md` / `AGENTS.md` | 変更 | 各現行仕様 | TUI 章 全面書き換え |

---

## Step 0: Worktree 作成

- ブランチ名: `feat/tui-inline-chat`
- `worktree-create` skill を使用

---

## Step 1: モジュール雛形とターミナルセッション

Inline viewport の初期化・復帰（旧 `TuiSession` の Drop パターン移植）、`UiEvent` 定義、`EventStream` 接続、空 transcript + composer の最小ループで起動→q 終了ができる状態を作る。単体テスト対象外（Step N+1 手動確認）。

コミット: `feat: scaffold inline viewport tui session`

---

## Step 2: コンポーザ — 編集操作 (TDD)

### RED: テスト先行

| ID | テストケース | 内容 |
| --- | --- | --- |
| C1 | `insert_char_keeps_cjk_boundary` | 日本語混在文字列への挿入・カーソル整合 |
| C2 | `backspace_deletes_previous_char` | 境界安全な削除 |
| C3 | `cursor_moves_by_word` | Ctrl+Left/Right の空白・記号区切り移動 |
| C4 | `kill_word_line_edits` | Ctrl+W / U / K の削除範囲 |
| C5 | `multiline_cursor_navigation` | 複数行間の Up/Down と列位置維持 |
| C6 | `enter_emits_send_effect` | 空は無視・非空で Send 効果を返す |

### GREEN: 最小実装

`composer.rs`: テキストバッファ（行ベクトル）+ カーソル(行, 列) + `handle(InputEvent) -> Effect` 純粋関数。

### コミット

`feat: tui composer core editing state machine`

---

## Step 3: コンポーザ — 履歴・ペースト・補完状態 (TDD)

### RED: テスト先行

| ID | テストケース | 内容 |
| --- | --- | --- |
| C7 | `history_roundtrip_preserves_draft` | Up→過去へ、Down→最新、途中編集はドラフト保存 |
| C8 | `history_navigation_only_at_edges` | 1 行目 Up / 最終行 Down のみ履歴発火、他は行間移動 |
| C9 | `paste_inserts_multiline_text` | 改行含む paste がそのまま挿入 |
| C10 | `ctrl_c_clears_or_quits` | 空→Quit、非空→全消去 |

### GREEN / REFACTOR / コミット

`composer.rs` に history スタック + paste 入力経路を追加。
コミット: `feat: tui composer history paste and quit semantics`

---

## Step 4: transcript とターン状態 (TDD)

### RED: テスト先行

| ID | テストケース | 内容 |
| --- | --- | --- |
| R1 | `delta_accumulates_into_active_turn` | Delta 連結が streaming バッファへ蓄積 |
| R2 | `tool_card_transitions_to_result` | ToolStart→⟳カード、ToolResult→✓/✗ 確定形 |
| R3 | `final_response_commits_and_clears_active` | FinalResponse で Markdown ブロック commit・アクティブ解消 |
| R4 | `error_event_commits_error_block` | Error でエラー形ブロック commit |

### GREEN: 最小実装

`transcript.rs`: `enum Block { User, Assistant(Vec<Line>), Tool{..}, Error }` + `ActiveTurn { buffer, tool_cards, iteration }`。`apply_agent_event(&mut self, ev)` を pure に実装。Markdown 変換は trait 境界で注入し Step 6 で実体化。

### コミット

`feat: tui transcript state applies agent events`

---

## Step 5: イベントループ統合と送信経路

`mod.rs` に select! ループ実装、送信で `process_turn_with_events` spawn + mpsc 転送、保留キュー 1 件、シャットダウンモニタ接続。単体テスト対象外とし Step N+1 で実機確認する。

コミット: `feat: wire tui event loop to turn streaming`

---

## Step 6: Markdown レンダリング (TDD)

### RED: テスト先行

| ID | テストケース | 内容 |
| --- | --- | --- |
| M1 | `heading_and_bold_styles` | Heading/強調のスタイル付与 |
| M2 | `list_renders_indented_bullets` | ネスト含む bullet とインデント |
| M3 | `code_span_styled` | インラインコードが黄 |
| M4 | `fenced_code_highlighted_with_fallback` | 既知言語は syntect 色、未知は plain |
| M5 | `long_lines_wrap_at_width` | 表示幅基準のハードラップ（CJK 2 幅考慮） |

### GREEN / コミット

`markdown.rs` 実装し Step 4 の注入ポイントへ接続。
コミット: `feat: markdown rendering for committed tui blocks`

---

## Step 7: スラッシュカタログと補完ポップアップ (TDD)

### RED: テスト先行

| ID | テストケース | 内容 |
| --- | --- | --- |
| S1 | `catalog_covers_dispatch_names` | 実 dispatch が扱う全コマンド名がカタログに存在 |
| S2 | `completion_filters_by_prefix` | `/mo` → models, model |
| S3 | `tab_accepts_candidate` | 候補確定で入力置換 |

### GREEN / コミット

`slash_commands.rs` にカタログ新設（dispatch 整理込み）、`draw.rs` にポップアップ描画。
コミット: `feat: slash command catalog and tui completion popup`

---

## Step 8: セッションオーバーレイと起動時セッション

`sessions.rs` 状態機械（選択移動・切替・新規）を TDD（`startup_opens_latest_context` / `select_session_builds_surface_context`）、`draw.rs` にオーバーレイ描画、`main.rs` に `--session` を追加。
コミット: `feat: tui session overlay and startup context`

---

## Step 9: 旧実装削除と docs 更新

`tui.rs` 残骸削除、Browser ビュー由来コードの整理、docs 4 点更新。
コミット: `refactor: remove legacy fullscreen tui and update docs`

---

## Step N+1: 動作確認

```bash
cargo fmt --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test
```

手動確認チェックリスト（実 LLM 使用）:

- [ ] 起動で直近セッションが開き、即入力可能
- [ ] ストリーミングで応答が逐次表示され、完了後に Markdown 化してスクロールバックへ流れる
- [ ] ツール実行で ⟳ → ✓/✗ カードが表示される
- [ ] 日本語 IME 確定入力・マルチライン貼り付けが正しく編集できる
- [ ] 実行中の追加送信が完了後自動送信される
- [ ] `/sessions` で切替・新規ができる
- [ ] Ctrl+C（空/非空）、Ctrl+D、`run` との併存、resize 追従
- [ ] 端末高さ 10 行未満でエラー終了

失敗時は該当 Step へ戻る。

---

## Step N+2: Plan・仕様書との自己レビュー

**目的**: 自分のコンテキスト（本 Plan の意図）を活かし、CodeRabbit の前に実装不正をすべて潰す。証跡は手段であり、リスト埋めは完了ではない。自己レビュー未了は成果ゼロ・PR 出さず。**「何も見つからなかった」は未レビュー扱いでやり直す。**

観点（補助）: テストが約束した振る舞いを assert しているか / 可視性最小か / 旧 tui.rs 由来デッドコードが残っていないか / カタログが両経路（dispatch と補完）から使われているか / docs と実装の一致 / diff と対象一覧の照合。

---

## Step N+3: PR 作成

- PR タイトル: `feat: rebuild tui as inline chat ui with streaming`
- description は日本語で概要・画面仕様・テスト結果を記載

---

## Step N+4 / N+5: レビューバック

push 後 `sleep 15m` → `pr-review-back-workflow` skill 実行（無ければ `sleep 5m` 再試行、最大 2 回）。対応 push 後も同様に再レビューバックを実施。

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
| --- | --- | --- |
| `Cargo.toml` | 変更 | 依存追加 4 点 |
| `src/channels/tui.rs` | **削除** | 旧フルスクリーン実装 |
| `src/channels/tui/*.rs` | **新規** | 7 モジュール |
| `src/slash_commands.rs` | 変更 | カタログ API |
| `src/main.rs` | 変更 | TUI `--session` |
| docs ×3 + `AGENTS.md` | 変更 | 仕様反映 |

## 自動テスト一覧（全 27 件）

| 分類 | ID | Step |
| --- | --- | --- |
| composer | C1–C10 | Step 2–3 |
| transcript | R1–R4 | Step 4 |
| markdown | M1–M5 | Step 6 |
| catalog/completion | S1–S3 | Step 7 |
| draw（TestBackend 描画 assert） | D1: `composer_box_renders_cursor_and_text` / D2: `completion_popup_lists_candidates` / D3: `tool_card_shows_pending_and_result_states` | Step 7–8 |
| sessions | 2 件 | Step 8 |

※ 上限ではない。実装中の不安はテストリストへ追加し Cycle を継続する。

## 工数見積もり

| Step | 内容 | 見積もり |
| --- | --- | --- |
| Step 1 | 雛形・セッション管理 | ~200 行 |
| Step 2–3 | composer | ~450 行 |
| Step 4 | transcript | ~250 行 |
| Step 5 | イベントループ統合 | ~250 行 |
| Step 6 | markdown | ~350 行 |
| Step 7 | catalog・補完 | ~200 行 |
| Step 8 | sessions | ~200 行 |
| Step 9 | 削除・docs | ~100 行差し替え |
| Step N+1 以降 | 検証・自己レビュー・PR・レビューバック | ~100 行 |
| **合計** |  | **~2100 行** |
