# Plan: EgoPulse パフォーマンス総合改善

`docs/performance.md` で特定した 7 つのボトルネックを、TDD・段階的コミット・Worktree を用いて一括実装する。

> **Note**: 以下の具体的なコード例・API 設計・構成（How）はあくまで参考である。実装時によりよい設計方針があれば積極的に採用すること。

---

## 設計方針

- **本質解決を優先**: `Arc` による共有、`&[T]` による借用、中間 `Value` の排除など、対症療法ではなく所有権モデルの見直しを徹底する
- **トレイト変更の一貫性**: `LlmProvider::send_message` のシグネチャ変更（Step 6, 7）は、他の Step とは独立して実行し、コンパイルエラーをゼロに收める
- **決定性の維持**: 並列化（Step 2）ではツール呼び出し順序と transcript の決定性を絶対に保つ
- **Worktree 運用**: `perf/core-improvements` ブランチで作業し、main への影響を隔離する
- **段階的検証**: 各 Step 完了後に `cargo test / clippy / fmt` を実行し、必ずパスしてから次へ進む

---

## Plan スコープ

`WT作成` → `実装(TDD)` → `コミット(意味ごとに分離)` → `PR作成`

---

## 対象一覧

| # | 項目 | 影響度 | 修正難易度 | 対象ファイル |
|---|---|---|---|---|
| 1 | SSE ストリーミングパースの不要アロケーション | 低 | 低 | `src/llm/responses.rs` |
| 2 | ツール実行の並列化不足 | 高 | 低 | `src/agent_loop/turn.rs`, `src/tools/mod.rs`, `src/tools/mcp.rs` |
| 3 | ツール定義の毎イテレーション clone | 中 | 低 | `src/agent_loop/turn.rs`, `src/tools/mod.rs`, `src/llm/mod.rs`, `src/pulse/runner.rs` |
| 4 | WebSocket イベント転送の不要な中間パース | 中 | 中 | `src/channels/web/ws.rs`, `src/channels/web/stream.rs` |
| 5 | SQLite 単一コネクションの直列化 | 高 | 中 | `src/storage/mod.rs`, `src/storage/queries.rs` |
| 6 | セッション snapshot の二重デシリアライズ | 中 | 中 | `src/agent_loop/session.rs`, `src/assets.rs` |
| 7 | ホットパスでの過剰な clone() | 高 | 中 | `src/agent_loop/turn.rs`, `src/agent_loop/session.rs`, `src/llm/mod.rs`, `src/pulse/runner.rs` |

---

## Step 1: SSE ストリーミングパースの最適化 (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `sse_parse_avoids_value_clone` | `parse_codex_responses_payload` の SSE 行パースで `Value::clone` が呼ばれないことを検証（`serde_json::Value` の `take()` を使う） |
| `sse_parse_preserves_correctness` | 既存の SSE fixture（completed、streamed text、output item）で出力が一致することを検証 |
| `sse_streamed_text_capacity` | `String::with_capacity(8192)` の導入後、長いストリームで reallocation 回数が減少することを間接検証（deferred、工数内で可能な範囲） |

### GREEN: 実装

- `src/llm/responses.rs` の `parse_codex_responses_payload` を修正
- `serde_json::Value::take()` を使用し、`item_value.clone()` と `response_value.clone()` を排除
- `Vec::with_capacity(32)`、`String::with_capacity(8192)` を追加
- `response.output_text.done` 到達時に `streamed_text.clear(); streamed_text.push_str(done_text)` に変更

### コミット

`perf(llm): avoid Value clones in SSE parser with take()`

---

## Step 2: ツール実行の順序保持部分並列化 (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `parallel_read_only_block` | `read` + `read` + `bash` + `read` の並びで、最初の2つが並列実行されることを検証（順序は保持） |
| `sequential_write_tools` | `bash` + `write` の並びで両方が逐次実行されることを検証 |
| `preserves_transcript_order` | 混在ツール実行後の `messages` の順序が、元の `valid_tool_calls` と一致することを検証 |
| `mcp_tools_classified` | `mcp_read`（仮想）が read-only として扱われ、read-only ブロックに含まれることを検証 |

### GREEN: 実装

- `src/agent_loop/turn.rs`: `execute_tool_calls` を修正。`valid_tool_calls` を先頭からスキャンし、**連続する read-only ブロック**だけを `join_all` で並列実行。write ツールに到達したら `await` して結果を統合し、残りを処理
- `src/tools/mod.rs`: `is_read_only()` に MCP ツールの判定を追加（`mcp_` prefix の場合、`mcp_manager` から metadata を読むか、安全側に倒して `false` を返すが**対象に含める**）
- `src/tools/mcp.rs`: MCP Tool の metadata（あれば `readOnlyHint` 類）を `is_read_only` 判定に反映

### コミット

`feat(agent): parallelize contiguous read-only tool blocks while preserving order`

---

## Step 3: LLM ツール定義の共有参照化 (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `definitions_async_returns_arc` | `ToolRegistry::definitions_async()` が `Arc<Vec<ToolDefinition>>` を返すことを検証 |
| `send_message_accepts_arc_tools` | `LlmProvider::send_message` が `Option<Arc<Vec<ToolDefinition>>>` を受け付けることを検証 |
| `no_tool_def_clone_per_iteration` | `process_turn_inner` の複数イテレーションで `ToolDefinition` の deep clone が発生しないことを検証 |

### GREEN: 実装

- `src/llm/mod.rs`: `LlmProvider::send_message` の `tools` パラメータを `Option<Vec<ToolDefinition>>` → `Option<Arc<Vec<ToolDefinition>>>` に変更
- `src/tools/mod.rs`: `definitions_async()` の戻り値を `Arc<Vec<ToolDefinition>>` に変更。内部で `Arc::new(definitions)` を返す
- `src/agent_loop/turn.rs`, `src/pulse/runner.rs`, `src/sleep/batch.rs` などの呼び出し元を修正: `Arc::clone(&tool_defs)` で渡す
- 全 `LlmProvider` 実装（`openai.rs` など）で `tools` を `Arc` から `&[ToolDefinition]` またはイテレータとして利用

### コミット

`perf(llm): share tool definitions via Arc to avoid per-iteration clones`

---

## Step 4: WebSocket イベント転送の中間パース排除 (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `ws_delta_without_intermediate_value` | `forward_run_event("delta", ...)` で中間 `serde_json::Value` が生成されず、かつ出力 JSON が正しいことを検証 |
| `ws_done_event_structure` | `done` イベントの JSON 構造が変更前と同等であることを検証 |
| `stream_event_format_matches` | `stream.rs` が発行するイベント JSON が、WS 側で envelope + payload として扱える形であることを検証 |

### GREEN: 実装

- `src/channels/web/stream.rs`: `AgentEvent` → `run_hub.publish` の際、クライアントが期待する `GatewayChatEvent` の `message` フィールドに近い JSON を直接生成して `event.data` に格納
- `src/channels/web/ws.rs`: `forward_run_event` で `serde_json::from_str::<Value>` を排除。`event.data` を envelope 構造体の `String` フィールドに埋め込み、`#[serde(flatten)]` または専用 envelope 型で一括シリアライズ
- `send_event` 内での `serde_json::to_string` は維持するが、中間 `Value` を経由しない

### コミット

`perf(web): eliminate intermediate Value parsing in WS event forwarding`

---

## Step 5: SQLite コネクションプール導入 (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `concurrent_reads_do_not_block` | 複数タスクから同時に `SELECT` を発行しても、Mutex 待ちが発生しない（タイムアウトなしで完了）ことを検証 |
| `writes_are_serialized` | 同時書き込みが WAL モードで正しく直列化されることを検証 |
| `pool_reuses_connections` | コネクションプールが接続を再利用することを検証 |

### GREEN: 実装

- `src/storage/mod.rs`:
  - `Database` 構造体を修正：`Mutex<Connection>` → `r2d2::Pool<SqliteConnectionManager>`（または `deadpool_sqlite`）
  - 読み取り用・書き込み用のメソッド分離（またはプールから `get()` で取得）
  - WAL モード・busy_timeout は接続初期化時に設定
- `src/storage/queries.rs`: 各種クエリメソッドで `conn.prepare_cached` を使用し、Prepared Statement の再利用を促進
- `call_blocking` はプール対応に調整

### コミット

`perf(storage): introduce SQLite connection pool for concurrent reads`

---

## Step 6: セッション Snapshot の二重デシリアライズ解消 (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `text_only_snapshot_skips_hydration` | テキストのみの snapshot JSON で `hydrate_message` が呼ばれない（または即座に return する）ことを検証 |
| `image_snapshot_hydrates_refs` | `InputImageRef` を含む snapshot が正しく `InputImage` に解決されることを検証 |
| `identical_output_to_two_pass` | Custom Deserializer（または遅延解決）の出力が、従来の2回走査と完全に一致することを検証 |
| `large_session_load_performance` | 1000 メッセージのセッション読み込みが一定時間内に完了することを検証（regression test） |

### GREEN: 実装

- `src/agent_loop/session.rs`:
  - `restore_snapshot_messages` を修正：デシリアライズの最中に `InputImageRef` を検出した場合のみ `hydrate_part` を適用。テキストのみならば2回目のイテレーションをスキップ
  - または、`hydrate_message` を遅延評価にし、`MessageContent::Parts` のアクセス時に初めて解決する `Lazy<T>` パターンを導入
- `src/assets.rs`: `hydrate_part` を `pub(crate)` に変更し、デシリアライザから直接呼び出せるように調整

### コミット

`perf(session): unify deserialization and hydration to eliminate double iteration`

---

## Step 7: エージェントターンのメッセージ履歴共有参照化 (TDD)

**前提**: Step 3 で `LlmProvider::send_message` シグネチャが一部変更済み。本 Step で `messages` パラメータをさらに変更する。

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `send_message_accepts_shared_messages` | `LlmProvider::send_message` が `Arc<Vec<Message>>` を受け付けることを検証 |
| `persist_phase_borrows_messages` | `persist_phase` が `&[Message]` を受け取り、内部で JSON シリアライズできることを検証 |
| `turn_loop_avoids_full_clone` | `process_turn_inner` のツールイテレーションで `messages.clone()` が呼ばれないことを検証 |
| `retry_messages_uses_arc` | `retry_messages` の型が `Option<Arc<Vec<Message>>>` になり、所有権移動を伴わないことを検証 |

### GREEN: 実装

- `src/llm/mod.rs`: `send_message` の `messages` パラメータを `Vec<Message>` → `Arc<Vec<Message>>` に変更
- `src/agent_loop/turn.rs`:
  - `messages` の型を `Vec<Message>` → `Arc<Vec<Message>>` に変更
  - `retry_messages` も同様に `Arc` 化
  - `persist_phase` 系関数に `&[Message]` を渡して所有権を維持
  - `request_messages` の組み立て時も `Arc::clone` のみにする
- `src/agent_loop/session.rs`: `load_messages_for_turn` の戻り値を `Arc<Vec<Message>>` に変更。`persist_phase` は `&[Message]` を受け取る
- `src/pulse/runner.rs` など、呼び出し元を `Arc` 対応に修正

### コミット

`perf(agent): share message history via Arc to avoid per-turn clones`

---

## 動作確認

全 Step 完了後、以下を必ず実行してパスすることを確認する。

```bash
cargo fmt --check
cargo check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

---

## PR 作成

- **ブランチ**: `perf/core-improvements`
- **タイトル**: `perf: EgoPulse パフォーマンス総合改善`
- **本文**:
  - `docs/performance.md` で特定した 7 項目を一括実装
  - 各項目の概要と効果
  - `Close #<issue_if_any>`
- **レビュー依頼**: Codex レビューを実行 (`codex exec ...`)

---

## 変更ファイル一覧

| ファイル | 状態 |
|---|---|
| `src/agent_loop/turn.rs` | 変更 |
| `src/agent_loop/session.rs` | 変更 |
| `src/llm/mod.rs` | 変更 |
| `src/llm/responses.rs` | 変更 |
| `src/tools/mod.rs` | 変更 |
| `src/tools/mcp.rs` | 変更 |
| `src/storage/mod.rs` | 変更 |
| `src/storage/queries.rs` | 変更 |
| `src/channels/web/ws.rs` | 変更 |
| `src/channels/web/stream.rs` | 変更 |
| `src/pulse/runner.rs` | 変更 |
| `src/assets.rs` | 変更 |
| `docs/performance.md` | 変更（更新） |

---

## コミット分割

| # | コミットメッセージ | 対象ファイル |
|---|---|---|
| 1 | `perf(llm): avoid Value clones in SSE parser with take()` | `src/llm/responses.rs` |
| 2 | `feat(agent): parallelize contiguous read-only tool blocks while preserving order` | `src/agent_loop/turn.rs`, `src/tools/mod.rs`, `src/tools/mcp.rs` |
| 3 | `perf(llm): share tool definitions via Arc to avoid per-iteration clones` | `src/llm/mod.rs`, `src/tools/mod.rs`, `src/agent_loop/turn.rs`, `src/pulse/runner.rs` |
| 4 | `perf(web): eliminate intermediate Value parsing in WS event forwarding` | `src/channels/web/ws.rs`, `src/channels/web/stream.rs` |
| 5 | `perf(storage): introduce SQLite connection pool for concurrent reads` | `src/storage/mod.rs`, `src/storage/queries.rs` |
| 6 | `perf(session): unify deserialization and hydration to eliminate double iteration` | `src/agent_loop/session.rs`, `src/assets.rs` |
| 7 | `perf(agent): share message history via Arc to avoid per-turn clones` | `src/agent_loop/turn.rs`, `src/agent_loop/session.rs`, `src/llm/mod.rs`, `src/pulse/runner.rs` |

---

## テストケース一覧

### LLM (SSE Parser) (3)

1. `sse_parse_avoids_value_clone` — `take()` による clone 排除を検証
2. `sse_parse_preserves_correctness` — 既存 fixture で出力一致を検証
3. `sse_streamed_text_capacity` — キャパシティ事前確保の効果を検証

### Agent Loop (Tool Parallelization) (4)

4. `parallel_read_only_block` — 連続 read-only ブロックの並列化を検証
5. `sequential_write_tools` — write ツールの逐次実行を検証
6. `preserves_transcript_order` — 実行後の messages 順序が元と一致することを検証
7. `mcp_tools_classified` — MCP ツールが read-only 判定に含まれることを検証

### LLM (ToolDefs) (3)

8. `definitions_async_returns_arc` — `Arc<Vec<ToolDefinition>>` 戻り値を検証
9. `send_message_accepts_arc_tools` — トレイトシグネチャ変更を検証
10. `no_tool_def_clone_per_iteration` — deep clone 発生なしを検証

### Web (WS Forwarding) (3)

11. `ws_delta_without_intermediate_value` — `Value` 経由なしで正しい JSON 出力を検証
12. `ws_done_event_structure` — `done` イベントの構造同一性を検証
13. `stream_event_format_matches` — `stream.rs` と `ws.rs` の JSON 互換性を検証

### Storage (Pool) (3)

14. `concurrent_reads_do_not_block` — 並列読み取りの非ブロッキングを検証
15. `writes_are_serialized` — WAL による書き込み直列化を検証
16. `pool_reuses_connections` — コネクション再利用を検証

### Session (Snapshot) (4)

17. `text_only_snapshot_skips_hydration` — テキストのみで hydration スキップを検証
18. `image_snapshot_hydrates_refs` — 画像参照の正しい解決を検証
19. `identical_output_to_two_pass` — 新旧実装の出力一致を検証
20. `large_session_load_performance` — 大規模セッション読み込みの回帰検証

### Agent Loop (Messages) (4)

21. `send_message_accepts_shared_messages` — `Arc<Vec<Message>>` 受け入れを検証
22. `persist_phase_borrows_messages` — `&[Message]` からの JSON 生成を検証
23. `turn_loop_avoids_full_clone` — イテレーション毎の full clone なしを検証
24. `retry_messages_uses_arc` — `retry_messages` の `Arc` 化を検証

**全 24 件**

---

## 工数見積もり

| Step | 内容 | 推定行数（実装+テスト） |
|---|---|---|
| Step 1 | SSE パース最適化 | 80 |
| Step 2 | ツール並列化 | 150 |
| Step 3 | ツール定義 Arc 化 | 120 |
| Step 4 | WS イベント最適化 | 120 |
| Step 5 | SQLite プール導入 | 200 |
| Step 6 | Snapshot 二重デシリアライズ解消 | 180 |
| Step 7 | メッセージ履歴 Arc 化 | 250 |
| **合計** | | **~1100 行** |
