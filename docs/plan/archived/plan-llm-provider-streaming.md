# Plan: LLM プロバイダのストリーミング実装による narration 復旧

`OpenAiProvider` が `send_message_streaming` を実装していないため、ツール実行前にエージェントが発する一言（narration）が Discord/Telegram に表示されない不具合を、トレイトの両必須化と `OpenAiProvider` への真のストリーミング実装、および内部共有コア抽出で修正する。

> **Note**: 振る舞い（What）は決して変えてはいけないが、より美しい設計があれば実装方法（HOW）だけは変えてもよい。

## 設計方針（C 案：両必須化 ＋ 内部共有コア）

- **根本原因**: `LlmProvider` トレイトの `send_message_streaming` に `let _ = on_delta; self.send_message(...)` というデフォルト実装があり、`OpenAiProvider` がこれに頼っているため `AgentEvent::Delta` が一度も発火しない。narration は `Delta` → `ToolProgressCoordinator::pending_narration` → `💬 <text>` の経路にのみ依存するため、起点が繋がらないと機能全体が死ぬ。
- **プロトコルの違いを尊重**: `send_message`（非ストリーム JSON）と `send_message_streaming`（SSE）は、同じ `MessagesResponse` を返すが**wire protocol が別物**（`stream:true`・`stream_options.include_usage`・SSE 形式・usage/reasoning_content の到着形式・エラー形式が異なる）。よってトレイトでは**両方を必須メソッド**としデフォルトを廃止する。デフォルト廃止により「ストリーミング未実装が黙って非ストリームに化ける」事故がコンパイル時に消滅する。
- **既存パス保護**: `OpenAiProvider::send_message` は既存の非ストリーム JSON 経路を**維持**する。既存の `send_message` 呼び出し（pulse/sleep/slash_commands/各種テスト）と local/OpenRouter/DeepSeek 等の互換エンドポイント対応は影響ゼロ。AGENTS.md の「後方互換は負債」は旧内部フォールバックの排除を指し、現役のプロダクト仕様（互換エンドポイント）を壊せという意味ではない。
- **内部共有コアで乖離を抑える**: 2つのHTTPパスが残るため、レスポンス正規化ロジックを共有し、両パスで usage・tool_calls・reasoning_content の扱いが一致するようにする。共有するのは:
  - 経路判定（`is_codex` / `should_use_responses_api(&messages)` で Chat Completions / Responses / Codex を切替）
  - request body 構築（`build_request_body` / `build_responses_request_body`）
  - headers / status error handling（`build_headers` / `api_error` / `parse_retry_after`）
  - `ToolCall` 正規化・`parse_tool_arguments`（`pub(super)` 化）
  - usage / reasoning_content / raw tool-use rescue の最終 `MessagesResponse` 組み立て
  - **共有しない**: HTTP read（`response.json()` と `bytes_stream()` は本質的に別なので無理に一本化しない）
- **真のストリーミング（逐次処理）**: `response.text().await` 一括読みは `on_delta` がリアルタイムに呼ばれず Web ライブ表示・長ターン narration の即時性が損なわれるため禁止。`reqwest::Response::bytes_stream()` で `Stream<Item = Result<Bytes>>` を受け、`src/llm/sse.rs`（新設）のラインバッファで完整 `data:` 行を都度取り出して `on_delta` を呼ぶ。SSE チャンクは TCP セグメント境界で行途中で切れ得るためバッファリング必須。
- **usage は best-effort（互換性優先）**: Chat Completions のストリームで `stream_options.include_usage` を**既定で送らない**。本修正の本命経路は `send_message_streaming`（agent loop）であり、local/OpenRouter/DeepSeek 等の互換エンドポイントを agent provider として使う場合に `stream_options` 拒否で agent loop 自体が失敗するリスクを排除するため。usage は最終チャンクに含まれていれば抽出（best-effort）、欠落時は `None` でフェイルセーフ。**「usage 維持」より「互換 streaming 成功」を優先**する（本修正の目的は narration）。既存の非ストリーム `send_message` は独自経路で usage を維持するため、既存呼び出し（pulse/sleep 等）の usage ログ/キャリブレーションは What 不変。
- **reasoning_content 維持**: DeepSeek 互換系は `delta.reasoning_content` をストリームする。これを蓄積し最終 `MessagesResponse.reasoning_content` へ（What 不変）。
- **参照元**: 先任 `docs/plan/plan-tool-progress-narration.md`（narration 機能の設計。本 Plan がその前提の「プロバイダはストリーミングする」を初めて真に成立させる）、`src/runtime/tool_progress.rs`、`src/llm/openai.rs`、`src/llm/responses.rs`、`docs/channels.md` §ツール進捗表示、`docs/openai-codex.md`。

## TDD 方針

テストリスト項目（`T1`...）と Red で書く自動テスト（`test_*`）を区別する。1回の Red では自動テスト1件のみ追加し、Green はそのテストを通す最小実装、Refactor は全テスト緑を保ったまま構造を整える。Green 中に別ケースやリファクタを混ぜない。実装中に新たな不安を見つけたらテストリストへ追記して Cycle を続ける。1項目に複数の失敗境界があれば同じ項目を複数 Cycle に分ける。Step 1（両必須化）は振る舞い不変の構造リファクタであり、`cargo test` 全緑を前提に Red を書かず進める（既存テスト群が安全網）。

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成 → レビュー待機・レビューバック

## 対象一覧

| 対象 | 種別 | 既存パターン / 参照元 | 備考 |
| -- | -- | -- | -- |
| `src/llm/mod.rs` | 変更 | `LlmProvider` トレイト定義（158-174行） | `send_message_streaming` のデフォルト実装を削除し必須化。`send_message` も既存通り必須 |
| `src/llm/openai.rs` | 変更 | `impl LlmProvider for OpenAiProvider`（216行） | `send_message` は既存非ストリームを維持。`send_message_streaming` を新規真のSSE実装（Chat Completions + Responses/Codex）。内部共有コア抽出 |
| `src/llm/sse.rs` | **新規** | なし | 共有SSEラインパーサ（`bytes_stream` → 完整 `data:` 行）。`pub(crate)` |
| `src/llm/responses.rs` | 変更 | `parse_codex_responses_payload`（232行） | `on_delta` 貫通。`parse_tool_arguments` 等の抽出ヘルパを `pub(super)` 化 |
| `src/llm/messages.rs` | 変更 | `build_request_body`（4行） | streaming 側で `Some(true)` を渡すのみ。`stream_options` は既定で追加しない（互換性優先） |
| 21件のテスト `impl LlmProvider` | 変更 | 後述「コミット分割」にファイル一覧 | 機械的移行：trivial な `send_message_streaming`（`send_message` への `on_delta` 無視委譲）を追加。既存 `send_message` は温存 |
| `src/llm/openai.rs`（テスト） | 新規 | 既存 wiremock テスト（`sends_openai_request` 等） | SSE ボディを返すモックで `on_delta`・usage・reasoning_content・tool_calls を検証 |
| `src/runtime/tool_progress.rs` 付近 | 新規 | coordinator のテスト群 | プロバイダ→Delta→coordinator の end-to-end テスト1本 |
| `docs/channels.md` | 変更 | §3/§4 ツール進捗表示の narration 記述 | 実装と照合し必要なら整合 |
| `docs/openai-codex.md` | 変更 | ストリーミング記述があれば整合 | SSE の取扱説明を更新 |

## テストリスト / 不安リスト

| ID | 観点 | 期待する振る舞い | 優先 | 対応するCycle | 状態 / 今回対象外理由 |
| -- | -- | -- | -- | -- | -- |
| T1 | 正常系 | Chat Completions の `stream:true` 応答で、各 `delta.content` が到着順に `on_delta` へ転送され、戻り値 `content` は全結合済み | High | Step 2 | 未着手 |
| T2 | 正常系 | Chat Completions ストリームで `delta.tool_calls` が複数チャンク・**複数 index** に分割されていても、`BTreeMap` で **index 昇順**に復元される（実行順序に影響） | High | Step 3 | 未着手 |
| T3 | 境界値 | Chat Completions ストリームで content が空（ツールコールのみ）のとき `on_delta` は一度も呼ばれず、`tool_calls` のみ返る | Medium | Step 3 | 未着手 |
| T4 | 異常系 | SSE に不正行/空行/`[DONE]` が混入してもスキップされ panic しない | Medium | Step 2 | 未着手 |
| T5 | 正常系 | Responses API で `response.output_text.delta` が `on_delta` に転送される（非Codex・マルチモーダル） | High | Step 4 | 未着手 |
| T6 | 正常系 | Codex SSE（`stream:true` 必須）で `response.output_text.delta` が `on_delta` に転送される。現状は一括蓄積のみで未呼び出し | High | Step 4 | 未着手 |
| T7 | 統合 | **wiremock-backed `OpenAiProvider`**（SSE モック）→ `process_turn_with_events` → `AgentEvent::Delta` → `ToolProgressCoordinator` で `💬` narration 行が描画される。モックプロバイダでなく実 `OpenAiProvider` 経路を検証 | High | Step 5 | 未着手 |
| T8 | 設計保証 | いかなる `impl LlmProvider` も `send_message_streaming` を実装し、`on_delta` 黙殺デフォルトに頼れない（コンパイル時保証） | Medium | Step 1 | 未着手 |
| T9 | 正常系 | Chat Completions ストリームで usage が最終チャンクに含まれていれば復元し、含まれていなければ `None` でフェイルセーフ（best-effort・`stream_options` 既定不送） | High | Step 2 | 未着手 |
| T10 | 正常系 | Chat Completions ストリームで `delta.reasoning_content` を蓄積し、`MessagesResponse.reasoning_content` に復元される（DeepSeek 互換系の What 不変） | High | Step 2 | 未着手 |
| T11 | 回帰保証 | 非ストリーム `send_message` は既存通り JSON 経路で動作し、互換エンドポイント（OpenRouter/local）の既存テストが壊れない | High | Step 1 | 未着手 |
| U1 | 不安 | 既存の `send_message` 経路（pulse/sleep/slash_commands/各種テスト）が壊れない | — | Step 1 後 `cargo test` 全緑で払拭 | — |
| U2 | 不安 | reqwest での SSE 逐次読み取りの正確性（行途中での TCP セグメント分割） | — | `bytes_stream()` + ラインバッファで対応。wiremock はボディ一括送信のため遅延チャンクの単体テストは困難だが実装は逐次必須 | — |
| U3 | 不安 | `stream_options.include_usage` が互換エンドポイントで拒否されないか | — | **既定で送らない**ことで agent loop 本命経路の互換性を最優先保証。usage は best-effort。既存 `send_message`（非ストリーム）は無傷で usage 維持 | — |
| U4 | 不安 | 2つの HTTP パスの乖離（今回のバグの遠因）が再発しないか | — | 共有コア（Accumulator）と両パスのテストで抑制。Step 2 Refactor で抽出 | — |

---

## Step 0: Worktree 作成

- ブランチ名: `fix/llm-streaming-narration`
- 作成コマンド: `worktree-create` Skill、または `git worktree add ../egopulse-llm-streaming -b fix/llm-streaming-narration`

---

## Step 1: `send_message_streaming` 必須化（構造リファクタ・振る舞い不変）

### この Step の目的

`send_message_streaming` のデフォルト実装を削除して必須化し、「ストリーミング未実装が黙って非ストリームに化ける」事故をコンパイル時に排除する（T8）。`send_message` は既存の非ストリーム経路のまま維持し、既存パス・テスト・互換エンドポイント対応を一切変えない（T11）。

### 進め方（Red なし・振る舞い不変リファクタ）

1. `src/llm/mod.rs`: `send_message_streaming` からデフォルト実装を削除して必須化。`send_message` は既存の必須メソッドのまま（デフォルト付与しない）。
2. `cargo check` → コンパイラが「`send_message_streaming` 未実装」の全プロバイダを列挙する（T8 の機械的保証）。
3. 各プロバイダに trivial な `send_message_streaming` を追加: `let _ = on_delta; self.send_message(system, messages, tools).await`。既存 `send_message` 実装は温存（C 案）。対象は `OpenAiProvider` 含む全22件中 `DeltaEmittingProvider`（turn.rs:1003・既存）以外の21件。
4. `OpenAiProvider` の一時実装も同様（`send_message` への `on_delta` 無視委譲）。Step 2 でこれを真のストリーミングに置き換える。

### 動作保証

- `cargo test` 全緑（振る舞い不変）。`agent_loop/turn.rs`・`pulse/scheduler.rs`・`runtime/mod.rs`・`sleep/scheduler.rs`・`sleep/orchestrator.rs`・`slash_commands.rs`・`channels/web/*`・`llm/mod.rs`（OpenRouter/local 等の既存 send_message テスト含む）が全て通る（T11）。

### REFACTOR

- デフォルト実装が完全に削除され、`send_message_streaming` 未実装がコンパイルエラーになることを確認。
- 既存 `send_message` 経路のコードが一切変更されていないことを確認（diff は全て「新規 `send_message_streaming` 追加」のみ）。

### テストリスト更新

- 完了: `T8`（コンパイラが強制）、`T11`（既存テスト全緑）
- 次候補: `T1`

### コミット

`refactor(llm): make send_message_streaming required to prevent silent fallback`

---

## Step 2: Chat Completions テキストストリーミング TDD Cycle

### この Step の目的

`OpenAiProvider::send_message_streaming` の Chat Completions 分岐を真のSSE実装にし、`delta.content` を `on_delta` で転送する（T1, T4, T9, T10）。

### 今回選ぶ項目

- 対象: `T1`（+境界 `T4`・usage `T9`・reasoning `T10`）
- 選ぶ理由: narration 復旧の最も直接的な経路。最も小さく価値がある。
- この時点では扱わないこと: `T2`/`T3`（tool_calls 蓄積）は Step 3、Responses/Codex は Step 4。

### RED: 失敗する自動テストを書く

- 追加するテスト名: `streams_chat_completions_text_deltas_via_on_delta`
- Given: wiremock が `/v1/chat/completions` で SSE ボディ（`Content-Type: text/event-stream`）を返す（`stream_options` は送らない）。チャンクは:
  - `data: {"choices":[{"delta":{"content":"ファイルを"}}]}`
  - `data: {"choices":[{"delta":{"content":"確認します","reasoning_content":"考えている"}}]}`
  - `data: {"choices":[]}\n\ndata: {"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5}}`
  - `data: [DONE]`
  - 不正行（`garbage line`）も1行混ぜる（T4）
- When: `provider.send_message_streaming(.., on_delta)` を呼ぶ。`on_delta` は受信文字列を `Arc<Mutex<Vec<String>>>` に記録。
- Then: 記録は `["ファイルを", "確認します"]`。戻り値 `content == "ファイルを確認します"`、`reasoning_content == Some("考えている")`（T10）、`usage == Some(LlmUsage{10,5})`（T9・サーバが自発的に送った場合の best-effort 抽出）、`tool_calls` 空。不正行で panic しない（T4）。
- 失敗理由の想定: Step 1 の時点では `on_delta` が無視され usage/reasoning も抽出されないため記録が空・両者 None。

### GREEN: 最小実装

- `OpenAiProvider::send_message_streaming` の Chat Completions 分岐（`is_codex` / `should_use_responses_api` が共に false のとき）で `build_request_body(.., Some(true), ..)` に変更（`stream_options` は**送らない**・互換性優先）。
- レスポンスを `response.bytes_stream()` で受け取り、`src/llm/sse.rs` のラインバッファで完整 `data:` 行を取り出しながら都度処理（`text().await` 一括読み禁止）。
- 各行を JSON パースし:
  - `choices[0].delta.content` があれば `on_delta(&content)`、content バッファへ蓄積
  - `choices[0].delta.reasoning_content` があれば reasoning バッファへ蓄積（T10）
  - `choices` 空で `usage` がある最終チャンクから usage を抽出（T9）
- content・reasoning_content・usage を蓄積し `MessagesResponse` を組み立てて返す。

### REFACTOR（共有コア抽出・U4 抑制）

- 2つのHTTPパスの乖離を抑えるため、レスポンス正規化を共有コアへ抽出:
  - `ChatCompletionAccumulator`: content/reasoning_content/tool_calls/usage を蓄積し `MessagesResponse` を組み立てる（raw tool-use rescue 含む）。非ストリーム `parse_openai_response` と stream 両方がこの組立を利用。
  - `parse_tool_arguments`（`pub(super)` 化）・`rescue_raw_tool_calls` 等の既存ヘルパを `openai.rs` からも利用可能に。
- HTTP read（`response.json()` vs `bytes_stream()`）は共有しない（プロトコル本質差）。
- 経路判定（`is_codex` / `should_use_responses_api`）・request body 構築・headers・status error handling は既存関数をそのまま両パスで再利用。

### テストリスト更新

- 完了: `T1`、`T4`、`T9`、`T10`
- 次候補: `T2`

### コミット

`feat(llm): stream chat completions text deltas via on_delta`

---

## Step 3: Chat Completions ツールコール蓄積 TDD Cycle

### この Step の目的

ストリーミングでも `tool_calls` が正しく復元されるようにする（T2, T3）。順序保持は実行順序に影響するため致命的。

### 今回選ぶ項目

- 対象: `T2`（+境界 `T3`）
- 選ぶ理由: narration はツール前の一言なので、ツールコールを含む応答でのストリーミングが必須。
- 扱わないこと: Responses/Codex は Step 4。

### RED

- テスト名: `accumulates_streaming_tool_calls_by_index_in_order`
- Given: SSE で `delta.tool_calls` が **`index:0` と `index:1` の2つ**にわたり、各 `id`/`name`（初回）→`arguments` 断片（複数）として届く。到着順は index 順とは限らない（index:1 の断片が先に来ることも許容）。
- When: `send_message_streaming`。
- Then: 戻り値 `tool_calls` は **index 昇順**（`[index:0, index:1]`）で、各 `{id, name, arguments=結合JSON}` が正しく復元される。`content` 空でも `on_delta` 呼ばれず（T3 別テストで境界確認）。
- 失敗理由: Step 2 では tool_calls を見ていないため空。

### GREEN

- `delta.tool_calls` を `BTreeMap<usize, {id,name,args_buf}>` で蓄積（**HashMap 不可・イテレーション順が非決定ため**）。最後に index 昇順で `Vec` 化して `parse_tool_arguments`（`pub(super)` 化済み）。

### REFACTOR

- 蓄積ロジックを `ChatCompletionAccumulator` の一部に統合。

### コミット

`feat(llm): accumulate streaming tool calls in chat completions`

---

## Step 4: Responses API / Codex ストリーミング TDD Cycle

### この Step の目的

Responses API（非Codex・マルチモーダル: T5）と Codex（`stream:true` 必須: T6）の両方で `response.output_text.delta` を `on_delta` へ。

### 今回選ぶ項目

- 対象: `T5`、`T6`（2 Cycle に分ける：T5→T6 の順で各 Red 1件）
- 選ぶ理由: Codex 利用時の narration も必要。`parse_codex_responses_payload` が既に抽出ロジックを持つため差分が小さい。
- 扱わないこと: end-to-end（Step 5）。

### RED (T5)

- テスト名: `streams_responses_api_text_deltas_via_on_delta`
- Given: `/v1/responses`（非Codex・マルチモーダル）が `response.output_text.delta` を含む SSE を返す。
- Then: `on_delta` が各 delta で呼ばれる。

### RED (T6)

- テスト名: `streams_codex_text_deltas_via_on_delta`
- Given: Codex 想定の SSE（既存の `send_codex_with_retry` 経路）。
- Then: `on_delta` が呼ばれる（現状は `parse_codex_responses_payload` が蓄積するのみで未呼び出し）。

### GREEN

- `send_message_streaming` の Responses 分岐で `stream:true` を付与（非Codexも）。
- `send_codex_with_retry` と非Codex Responses の両経路で、ボディを行単位消費し `response.output_text.delta` の `delta` を `on_delta` へ。最終 `response.completed`/`response.done` で `MessagesResponse` を組み立て。
- 既存 `parse_codex_responses_payload` の抽出ロジックを、`on_delta` を貫通させる形にリファクタ（Step 2 の `src/llm/sse.rs` ラインパーサを再利用）。

### REFACTOR

- Chat Completions と Responses/Codex の SSE 行パーサを `src/llm/sse.rs` で共通化。
- Responses 側も `ResponsesAccumulator` で最終 `MessagesResponse` 組立を共有コア化（U4 抑制）。

### コミット

`feat(llm): stream responses api and codex text deltas via on_delta`

---

## Step 5: End-to-End narration 統合テスト TDD Cycle

### この Step の目的

プロバイダのストリーミング → `AgentEvent::Delta` → `ToolProgressCoordinator` の `💬` 描画までが繋がることを統合的に検証する（T7）。これにより先任プラン `plan-tool-progress-narration.md` が前提とした経路が初めて真に成立する。

### 今回選ぶ項目

- 対象: `T7`
- 選ぶ理由: 単体では緑でも結合で死んでいた（今回のバグの本質）結合ギャップを塞ぐ。

### RED

- テスト名: `provider_streaming_drives_coordinator_narration`
- Given: **wiremock が `/v1/chat/completions` で SSE を返し、実 `OpenAiProvider`（モックプロバイダでない）を構築**。1回目の応答は narration の `delta.content`（「ファイルを確認します」）+ ツールコール。2回目は最終応答。
- When: `process_turn_with_events`（`OpenAiProvider` 使用）を実行し、イベントを `ToolProgressCoordinator`（モック sink）へ流す。
- Then: 投稿された進捗本文に `💬 ファイルを確認します` が含まれ、ツール行より前に出現する。
- 重要: モックプロバイダ（既に `Delta` を吐くもの）を使うと今回の根本原因（`OpenAiProvider` がストリーミングしない）を検証できない。実 `OpenAiProvider` + wiremock SSE でなければ結合ギャップを塞げない。

### GREEN

- Steps 2-4 の実装で通るはず。通らない場合は `OpenAiProvider` のストリーミング経路のどこかが切れているため定位修復。

### コミット

`test(llm): cover end-to-end narration from provider to coordinator`

---

## Step 6: 動作確認

- `cargo fmt --check`
- `cargo test`
- `cargo check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
- 失敗時に戻る Step: 該当 TDD Cycle

---

## Step 7: Plan・仕様書との自己チェック

実装完了後に本 Plan と関連仕様書を最初から読み直し、実装・自動テスト・文書が要求した振る舞いと一致しているかを照合する。未実装・過剰実装・テスト不足・仕様書との齟齬を見つけた場合は該当 Cycle へ戻り、動作確認を再実行してから本 Step を完了する。

- テストリスト `T1`-`T11` の各項目が完了条件を満たしている（未対応は U1-U4 の不安払拭のみ許容）。
- `docs/channels.md` §3/§4 ツール進捗表示の narration 記述（`💬 <text>`）が実装結果と一致している。
- `docs/openai-codex.md` のストリーミング記述があれば SSE の取扱と整合している。
- 先任 `docs/plan/plan-tool-progress-narration.md` の前提（プロバイダがストリーミングする）が本変更で真に成立したことを確認。
- 変更ファイル一覧・コミット分割・自動テスト一覧が実際の変更と一致している。

---

## Step 8: PR 作成

- PR タイトル: `fix: LLMプロバイダのストリーミング実装によるnarration復旧`
- PR description（日本語）:
  - 概要: ツール実行前のアシスタント発言（narration）が Discord/Telegram に表示されない不具合の修正。根本原因は `OpenAiProvider` が `send_message_streaming` を実装しておらず `AgentEvent::Delta` が発火していなかったこと。
  - 設計改善: `LlmProvider` トレイトの `send_message_streaming` を必須化し、オーバーライド忘れの silent 死を構造的に排除。`send_message`（非ストリーム）は既存パス保護のため温存し、内部共有コアで乖離を抑制。
  - テスト: SSE モックによる `on_delta`/usage/reasoning_content/tool_calls 検証 + プロバイダ→coordinator の end-to-end テスト。

---

## Step 9: 初回レビューバック

PR 作成後、レビュー生成を待ってから `pr-review-back-workflow` Skill を実行し、未対応のレビューコメントがあれば修正・検証・コミット・push まで完了する。

- 初回待機: `sleep 15m`
- レビュー対応: `pr-review-back-workflow` Skill を実行する
- まだレビューが無い場合: `sleep 5m` して再実行（最大 2 回まで）
- レビューコメントが無い、または最大待機後もレビューが無い場合は、その結果を PR に記録して完了扱いにする
- レビュー対応で変更した場合は、必要な動作確認を再実行してからコミット・push する

---

## Step 10: レビュー対応後の再レビューバック

レビュー対応を push した後、追加レビュー生成を待ってから `pr-review-back-workflow` Skill を再実行し、残った指摘や新規指摘があれば同じ品質基準で対応する。

- 対象: Step 9 でレビュー対応の変更を push した場合
- 初回待機: `sleep 15m`
- 再レビュー対応: `pr-review-back-workflow` Skill を実行する
- まだ追加レビューが無い場合: `sleep 5m` して再実行（最大 2 回まで）
- 追加レビューコメントが無い、または最大待機後も追加レビューが無い場合は完了扱い
- 再レビュー対応で変更した場合は、必要な動作確認を再実行してからコミット・push する

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
| ---- | ---- | -- |
| `src/llm/mod.rs` | 変更 | `send_message_streaming` デフォルト削除・必須化 |
| `src/llm/openai.rs` | 変更 | `send_message` 既存維持・`send_message_streaming` 新規真のSSE実装・内部共有コア抽出 |
| `src/llm/sse.rs` | **新規** | 共有SSEラインパーサ（`bytes_stream` → 完整 `data:` 行） |
| `src/llm/responses.rs` | 変更 | `on_delta` 貫通・`parse_tool_arguments` 等の `pub(super)` 化 |
| `src/llm/messages.rs` | 変更 | `stream_options.include_usage` 対応（streaming 側のみ） |
| `src/agent_loop/turn.rs` | 変更 | テストプロバイダ（`FakeProvider`/`FailingProvider`/`RecordingProvider`）へ trivial `send_message_streaming` 追加（`DeltaEmittingProvider` は既存） |
| `src/agent_loop/session.rs` | 変更 | `FakeProvider` へ trivial `send_message_streaming` 追加 |
| `src/slash_commands.rs` | 変更 | `NoOpProvider` へ trivial 追加 |
| `src/sleep/orchestrator.rs` | 変更 | `MockLlmProvider`/`SequentialMockProvider`/`SequentialMockWithUsage` へ trivial 追加 |
| `src/sleep/scheduler.rs` | 変更 | `MockLlm` へ trivial 追加 |
| `src/pulse/scheduler.rs` | 変更 | `MockPulseLlm`/`PendingLlm`/`PanickingLlm` へ trivial 追加 |
| `src/runtime/mod.rs` | 変更 | `StubFinalProvider`/`StubFailingProvider` へ trivial 追加 |
| `src/channels/web/ws.rs` | 変更 | `StubLlm` へ trivial 追加 |
| `src/channels/web/sleep.rs` | 変更 | `DummyLlm` へ trivial 追加 |
| `src/channels/web/agents.rs` | 変更 | `DummyLlm` へ trivial 追加 |
| `src/channels/web/sessions.rs` | 変更 | `DummyLlm` へ trivial 追加 |
| `src/llm/mod.rs`（テスト） | 変更 | `StubProvider`/`SharedMessagesProvider` へ trivial 追加 |
| `docs/channels.md` | 変更 | narration 記述と実装の整合確認・必要なら更新 |
| `docs/openai-codex.md` | 変更 | ストリーミング取扱の整合 |

---

## コミット分割

1. `refactor(llm): make send_message_streaming required to prevent silent fallback` - `src/llm/mod.rs`（デフォルト削除）+ 21件のテスト `impl LlmProvider` + `OpenAiProvider`（全て trivial な `send_message` 委譲を追加・振る舞い不変）
2. `feat(llm): stream chat completions text deltas via on_delta` - `src/llm/openai.rs`（Chat Completions 分岐・`bytes_stream`・reasoning_content・usage best-effort）+ `src/llm/sse.rs`（新規）+ `src/llm/responses.rs`（`pub(super)` 化）+ 共有コア抽出
3. `feat(llm): accumulate streaming tool calls in chat completions` - `src/llm/openai.rs`（`BTreeMap` 順序保持 tool_calls 蓄積）
4. `feat(llm): stream responses api and codex text deltas via on_delta` - `src/llm/openai.rs`（Responses/Codex 分岐）+ `src/llm/responses.rs`（`on_delta` 貫通）
5. `test(llm): cover end-to-end narration from provider to coordinator` - wiremock-backed `OpenAiProvider` による統合テスト
6. `docs: align channels/openai-codex docs with provider streaming` - ドキュメント更新（Step 7 で必要と判明した場合）

---

## 自動テスト一覧（全 9 件予定）

この一覧は Plan 作成時点で必要と判断した最低限の予定であり、最終テスト件数の上限ではない。実装中に追加された不安には、テストリストと TDD Cycle を追加して対応する。

### `src/llm/openai.rs`（全 8 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T1 | `streams_chat_completions_text_deltas_via_on_delta` | Step 2 | `cargo test -p egopulse streams_chat_completions_text_deltas` |
| T4 | `skips_malformed_sse_lines_without_panic` | Step 2 | `cargo test -p egopulse skips_malformed_sse_lines` |
| T9 | `extracts_usage_best_effort_in_chat_completions_stream` | Step 2 | `cargo test -p egopulse extracts_usage_best_effort_in_chat_completions_stream` |
| T10 | `preserves_reasoning_content_in_chat_completions_stream` | Step 2 | `cargo test -p egopulse preserves_reasoning_content_in_chat_completions_stream` |
| T2 | `accumulates_streaming_tool_calls_by_index_in_order` | Step 3 | `cargo test -p egopulse accumulates_streaming_tool_calls` |
| T3 | `no_delta_when_content_empty_tool_only` | Step 3 | `cargo test -p egopulse no_delta_when_content_empty` |
| T5 | `streams_responses_api_text_deltas_via_on_delta` | Step 4 | `cargo test -p egopulse streams_responses_api_text_deltas` |
| T6 | `streams_codex_text_deltas_via_on_delta` | Step 4 | `cargo test -p egopulse streams_codex_text_deltas` |

### end-to-end（全 1 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T7 | `provider_streaming_drives_coordinator_narration` | Step 5 | `cargo test -p egopulse provider_streaming_drives_coordinator_narration` |

※ T8（コンパイル時保証）と T11（既存 send_message 回帰保証）は Step 1 で `cargo check` / `cargo test` 全緑により検証。自動テスト一覧の件数には含めない。

---

## 工数見積もり

| Step | 内容 | 見積もり |
| -- | -- | -- |
| Step 1 | `send_message_streaming` 必須化 + 21プロバイダへ trivial 追加 | ~150 行 diff（ほぼ機械的） |
| Step 2 | Chat Completions テキストストリーミング（`bytes_stream`）+ `sse.rs` 新設 + reasoning_content + usage best-effort + 共有コア抽出 + テスト4件 | ~300 行 |
| Step 3 | tool_calls 蓄積（`BTreeMap` 順序保持）+ テスト2件 | ~120 行 |
| Step 4 | Responses/Codex ストリーミング + ヘルパ `pub(super)` 化 + テスト2件 | ~250 行 |
| Step 5 | wiremock-backed `OpenAiProvider` の end-to-end テスト1件 | ~100 行 |
| Step 6 | 動作確認（fmt/test/check/clippy/doc） | 検証のみ |
| Step 7 | docs 整合・自己チェック | ~50 行 |
| Step 8-10 | PR 作成 / レビューバック（×2） | — |
| **合計** | 実装・テスト・docs | **~970 行** |
