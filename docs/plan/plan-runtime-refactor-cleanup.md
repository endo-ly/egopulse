# Runtime Refactor Cleanup

外部レビューで指摘された項目を、**挙動を維持したリファクタ**としてすべて実施する。ロジックはApprove済み前提で、Rust の型設計・可読性・性能を改善する。

## 方針

- レビューに書かれた項目はすべて本PRで扱う（独断での対象外は行わない）。
- 挙動は変更しない。やむを得ず意味論が変わる箇所（メモリキャッシュによる手動編集の反映タイミングなど）は本ファイルに明記する。
- レビュー自身が「最適化しなくてよい（誤差）」と結論づけた `canonical_request_hash` も、「全部対応」指示に従い**安全な形（byte 等価テスト付き）で適用**する。

## 対象一覧

### A. 性能 / 実行モデル

#### A1. Memory のメモリキャッシュ化（hot path の同期 I/O 削除）
- 場所: `src/memory.rs`、呼び出し側 `src/agent_loop/prompt_builder.rs`（warn 化は後述 C1 で対応済）
- 現状: `MemoryLoader::load_bundle()` が Turn ごとに 3 ファイルを同期的に disk 読み出し。RwLock の read guard を保持したまま I/O。
- 対策: per-agent で公開済み `Arc<MemoryBundle>` をメモリ上にキャッシュ。
  - `load_bundle()` はキャッシュ命中なら `Arc` clone のみ返却（disk I/O なし）。初回ミス時のみ disk 読み出し→キャッシュ充填。
  - `publish_bundle()` / `recover_publication()` は disk 出版成功後にキャッシュを新しい bundle で差し替え。
  - 既存の per-agent RwLock（reader/writer 直列化）はそのまま維持し、キャッシュ読み書きも同じロック順序（file_lock → cache）で行う。出版中の torn read は発生しない。
- 挙動: single-writer モデル（runtime が排他 instance lock で唯一の書き手）。外部からの手動 disk 編集は**次の sleep 出版または restart までキャッシュに反映されない**点のみ従来と異なる（出版時の conflict 検知は disk を直接読むので従来通り維持）。これを意味論の変更点として明記する。

#### A2. `finalize_published_run_blocking` を blocking スレッドへ（✅ 実装済）
- 場所: `src/sleep/orchestrator.rs` の `finalize_batch()`
- 対策: 同期 helper 全体を `tokio::task::spawn_blocking` へ逃がし、async sleep batch が disk レイテンシで worker を止めないようにする。startup recovery は同期コンテキストなので非対称性は残さない。

### B. 型設計 / 長期保守性

#### B1. AgentSendTool の AppState 依存除去（Turn intake interface 化）
- 場所: `src/tools/agent_send.rs`、`src/tools/mod.rs`（Tool trait / ToolRegistry）、`src/runtime/mod.rs`（構築）、新設 `src/runtime/channel_input.rs` 内 `TurnIntake`
- 現状の循環: `AppState →(owns) ToolRegistry →(owns) AgentSendTool → Weak<AppState>`。そのため `Tool::init_app_state` という**汎用 trait への専用フック**と `ToolRegistry::init_agent_send_app_state` という**特定ツール専用ロジック**が存在し、構築時 `Arc::get_mut(&mut state).expect("unique app state at build time")` の二段階初期化（将来の無邪気な clone で startup panic になりうる）を要する。
- 対策: 狭い capability として `TurnIntake` を新設し、`AgentSendTool` は `Arc<TurnIntake>` を保持して `submit(turn)` を呼ぶだけにする。
  - `Tool::init_app_state` を削除。
  - `ToolRegistry::init_agent_send_app_state` を削除。
  - `AgentSendTool` の `OnceLock<Weak<AppState>>` / `init_app_state` を削除。
  - 構築を線形化: `TurnIntake`（`OnceLock<Weak<AppState>>` のみ保持する遅延ハンドル）を先に作り、`AgentSendTool` に渡して **`tools` を `Arc` 化する前に** `register_tool` する。`Arc::get_mut` の二段階初期化を廃止。`AppState` 構築後に `TurnIntake` へ `Weak<AppState>` を一度だけ注入。
  - `TurnIntake::submit` は `submit_scheduled_turn`（既存の free function）へ委譲。`execute_scheduled_turn` が `&AppState` を必要とするため、`TurnIntake` 内に 1 つの `Weak<AppState>`（注入式）が残るが、これは従来の「ツール内 OnceLock<Weak> + trait フック + get_mut」をすべて排除した上での最小の後退リンクであり、構造的に厳密に改善される。
  - 代替（`AppState` 本体の core 分割による weak 完全排除）は call-site 数百箇所に及ぶ大改修で「挙動非変更」の担保リスクが高いため本PRでは採用せず、将来候補として記録する。
- 挙動: 本番では `AppState` は常に生存し weak は常に upgrade されるため従来同等。未注入時は `delivered=false`（従来の「AppState 未バインド」挙動と同等）。関連テストを `TurnIntake` 生成・注入へ更新。

#### B2. `ScheduleResult` の状態型を明確化
- 場所: `src/runtime/turn_scheduler.rs`（`ScheduleResult`）、`src/runtime/channel_input.rs`（`schedule_and_spawn`）
- 現状: `Queued` が「本当に待機列入れ」「同一 turn_id の dedup no-op」を同一視。caller で capacity 不足の `Rejected` も `SubmitOutcome::Queued` に集約され意味が曖昧。
- 対策:
  ```rust
  enum ScheduleResult {
      Started(Box<ScheduledTurn>),
      Enqueued,         // 既存 turn の後ろに待機列入れ
      AlreadyOwned,     // 同一 turn_id が既に running/queued（idempotent no-op）
      DeferredCapacity, // per-session / global キャパシティ不足 → dispatcher 待ち
  }
  ```
  `schedule_and_spawn` は `Enqueued` / `AlreadyOwned` / `DeferredCapacity` をすべて `SubmitOutcome::Queued` に写像（外部 API は不変）。観測性のため適宜 debug ログ。
- 挙動: 外向き `SubmitOutcome` と各分岐の効果は完全に同一。テストを新 variant へ更新。

#### B3. `accept_or_get_turn` の引数を parameter object へ（✅ 実装済・テスト変換中）
- 場所: `src/storage/turn.rs`、呼び出し側 `src/runtime/channel_input.rs`
- 対策: `AcceptTurnParams<'a>` 構造体へ集約し `#[allow(clippy::too_many_arguments)]` を削除。

#### B4. `MemoryError::Io(String)` を構造化エラーへ（✅ 実装済）
- 場所: `src/memory.rs`、`src/sleep/orchestrator.rs`
- 対策: `Io { path: PathBuf, #[source] source: std::io::Error }`。`From<io::Error>` の blanket 実装を廃止し各 I/O 呼び出し点で path を明示付与。`NotFound` 折り畳みは維持。

#### B5. `InstanceGuard::is_valid()` の偽 API を削除（✅ 実装済）
- 場所: `src/runtime/supervisor.rs`、`src/runtime/mod.rs`
- 対策: `file` を `_file` に改名し、`metadata()` を呼ぶだけの偽 API `is_valid()` と対応する `assert!` を削除。`path` / `path()` は supervisor が使用するため維持。

### C. 可観測性

#### C1. `load_bundle(...).ok()?` で I/O エラーを黙殺しない（✅ 実装済）
- 場所: `src/agent_loop/prompt_builder.rs`
- 対策: `Err` を warn ログ出力のうえ memory section を省略（戻り値は従来と同じ `None`）。A1 のキャッシュ化でもこのログは維持。

### D. 性能（微細）

#### D1. `canonical_request_hash` の allocation 削減
- 場所: `src/agent_loop/mod.rs`
- 現状: `BTreeMap<&str, Value>` + `json!()` で都度組み立てて serialize。
- 対策: borrowed 構造体 `CanonicalRequest<'a>` を `#[derive(Serialize)]` で直接serialize。
  - **重要**: 本ハッシュは durable idempotency（`turn_runs.request_payload_hash`）に使用されるため、**JSON バイト列が現状と完全一致**することを保証する（struct のフィールド宣言順 = BTreeMap のソート順、フィールド名 = 従来キーと同一）。byte 等価性を UT で検証する。
- レビューは「誤差なので最適化不要」とするが、本指示に従い安全な形で適用する。

## 対象外

- なし。レビューに書かれた項目はすべて本PRで扱う。

## 検証

- `cargo fmt --check`
- `cargo clippy --lib`（実装中）→ PR前に `cargo clippy --all-targets --all-features -- -D warnings`
- UT（対象モジュール中心）:
  - `cargo test --lib memory::`（キャッシュ・出版・並発）
  - `cargo test --lib turn_scheduler::`（ScheduleResult 新 variant）
  - `cargo test --lib channel_input`
  - `cargo test --lib agent_loop::`（canonical_request_hash byte 等価 / prompt_builder memory）
  - `cargo test --lib tools::agent_send`（TurnIntake 経路）
  - `cargo test --lib storage::turn`（AcceptTurnParams）
  - `cargo test --lib supervisor`（InstanceGuard）
  - `cargo test --lib sleep::orchestrator`
- PR前に `cargo test`
