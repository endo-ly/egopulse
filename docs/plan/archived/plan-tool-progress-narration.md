# 引き継ぎ資料: ツール進捗にアシスタントの「一言」(narration) を追加

## 前提：既存実装の状態（main マージ済み）

Discord / Telegram 向けツール進捗インジケータ（A3 遅延型 × B2 編集式累積ログ）は既に実装・main マージ済み。

### データ経路
- 進捗は **`tool_calls` テーブルではなく `AgentEvent`** を見る（DB 永続化とは別経路）
- `agent_loop/turn.rs` の `DeltaEmittingProvider` が LLM ストリーミングを `AgentEvent` に変換して emit
- `runtime/tool_progress.rs` の `ToolProgressCoordinator` がイベントを消費

### `AgentEvent` の全種類（`src/agent_loop/event.rs`）
| イベント | いつ | 中身 |
|---|---|---|
| `Iteration { iteration }` | 各ループ反復の先頭 | 反復番号 |
| `Delta { text }` | LLM ストリーミングの差分チャンク | 細切れテキスト。全部繋ぐとアシスタント発言全文 |
| `ToolStart { name, input, call_id }` | ツール実行開始 | ツール名・入力・コールID |
| `ToolResult { name, is_error, preview, duration_ms, call_id }` | ツール実行完了 | 成否・時間・コールID |
| `FinalResponse { text }` | 最終応答確定（ツール呼び出しなしで turn 終了） | 最終応答全文 |
| `Error { message }` | エラー | メッセージ |

### 現在の coordinator の消費（`tool_progress.rs` `run()`）
- 消費する: `ToolStart`, `ToolResult`, `FinalResponse`, EOF
- **破棄する**: `Delta`, `Iteration`（`Some(_) => {}` で握り潰し）

### 既存の表示フォーマット（`ProgressLog::render()`）
```
tools running...
✓ web_fetch (1.8s)
... read notes.md
```
- 先頭行 `tools running...`（固定）
- ツール行は時系列（entries リスト）

## 課題

LLM がツールコール前に発する「一言」（例：「ファイルの内容を確認しますね」）は `Delta` として流れるが、coordinator が破棄しているため Discord/Telegram から見えない。

- 一言 **なし**（ただのツールコール）: 現状の進捗で見える → このまま（追加表示不要）
- 一言 **あり**: 現状は見えない → これを表示したい

Web (`channels/web/stream.rs`) は `Delta` を独立 SSE イベントとして配信しており、これが理想形。Discord/Telegram は進捗メッセージ内の1行に畳み込む。

## 目標

coordinator が `Delta` を結合し、ツールコール前の一言を進捗メッセージ内に表示する。

```
tools running...
💬 ファイルの内容を確認しますね
... read notes.md
✓ read (0.3s)
```

## 設計

### Delta 結合の状態機械（coordinator 側）

coordinator に `pending_narration: String` を追加。次の規則で運用：

| トリガ | 動作 |
|---|---|
| `Delta { text }` 受信 | `pending_narration.push_str(&text)`（結合） |
| `ToolStart` 受信 | `pending_narration` が非空なら **narration 行として確定**（時系列の該当位置へ）→ clear → ツール行追加 |
| `Iteration` 受信 | `pending_narration` が非空なら **破棄**（前 iter で ToolStart を伴わなかった＝最終応答の断片だったため）→ clear |
| `FinalResponse` 受信 | `pending_narration` を **破棄**（最終応答と同一、別メッセージで送られるため重複回避）→ close |
| EOF | `pending_narration` を破棄 → close |

### 最終応答との重複回避（キモ）

最終応答も `Delta` で流れる。しかし最終応答は `FinalResponse` イベントで別メッセージとして送られる。よって「ToolStart を伴わなかった Delta（＝最終応答の断片）」は進捗に載せない。上記規則の `Iteration` / `FinalResponse` で破棄することで実現。

### ProgressLog の構造変更

現在 `entries: Vec<ToolEntry>`（ツールのみ）。narration を時系列に挿入するため、行の種別を持たせる：

```rust
enum LogLine {
    Narration(String),              // 💬 <text>
    Tool(ToolEntry),                // 既存
}
struct ProgressLog {
    lines: Vec<LogLine>,            // 時系列
}
```

`render()` は `lines` 順に各行をフォーマット。先頭の `tools running...` は維持。

### narration 行の表示フォーマット
```
💬 <結合したテキスト>
```
- テキストは trim して空なら行自体追加しない
- 長すぎる場合は既存の文字数上限（チャネル制約：Discord 2000 / Telegram 4096）で打ち切り。`render()` 全体の折りたたみロジック（最新 N 行保持）に乗る

### 間引き・遅延タイマー
- 既存 `update_if_due` / delay タイマーにそのまま乗る。Delta が来ても即時編集せず、ToolStart 確定タイミング or 間引き間隔経過で反映
- delay 閾値未満のターンは従来通り表示しない（一言の有無にかかわらず）

## 変更ファイル

| ファイル | 変更 |
|---|---|
| `src/runtime/tool_progress.rs` | coordinator に `pending_narration` 追加・Delta/Iteration 処理・`ProgressLog` を `LogLine` 化・`render()` 更新 |

**この1ファイルのみ**。`AgentEvent`・`turn.rs`・チャネル層は変更不要（Delta は既に emit 済み）。

## 実装ステップ（TDD 簡易）

Step 1: `ProgressLog` を `LogLine` 化（リファクタリング・振る舞い不改変）
- `entries: Vec<ToolEntry>` → `lines: Vec<LogLine>`
- `render()` 更新（ツール行のみ、narration はまだ来ない）
- 既存テスト全 green を確認

Step 2: coordinator の `pending_narration` 追加
- フィールド追加・`Delta` で append・`ToolStart` で確定挿入・`Iteration`/`FinalResponse`/EOF で破棄
- `run()` の `Some(_) => {}` を個別 match arm に分解

Step 3: UT追加（AAA・重点）
- Delta 結合 → ToolStart で narration 行が確定表示
- Delta なし（ただのツールコール）→ narration 行なし（現状維持）
- Delta のみで FinalResponse（最終応答）→ narration 行なし（重複回避）
- 複数 iteration で複数一言 → 時系列で両方表示
- Iteration 変化で pending 破棄

## テスト観点

| ケース | 期待 |
|---|---|
| Delta あり → ToolStart | narration 行 + ツール行が時系列で表示 |
| Delta なし → ToolStart | ツール行のみ（既存挙動） |
| Delta → FinalResponse（最終応答） | narration 行なし（FinalResponse が別メッセージで送られるため） |
| Delta 途中で Iteration 変化 | 前の pending は破棄、新しい narration 開始 |
| delay 閾値未満 | 一言の有無にかかわらず進捗表示なし（既存） |
| 文字数超過 | 既存の折りたたみロジックで対応 |

## 懸念点

- **Delta の粒度**: ストリーミングチャンクが細かい（数文字）と append 回数が多いが、`String::push_str` なので性能問題なし
- **空白・改行の正規化**: LLM が先頭空白や改行を付けることがある。trim + 連続空白圧縮を検討（実装時に判断）
- **narration が極端に長い**: 現実的には1-2文。`render()` の折りたたみ（最新優先）で対応可能

## 作業メモ

- Worktree: 実装開始時に `feat/tool-progress-narration` で作成（main から）
- この機能は単一ファイル変更・小規模なので、スキル（implementation-plan）の完全準拠より、上記ステップで TDD サイクルを回す運用で十分
