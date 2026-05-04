# Plan: アーキテクチャ横断の重複排除・キャッシュ導入

チャネル層の SurfaceContext 構築・スラッシュコマンドフローの重複排除、PersistedMessage/Message 型統合、LLM プロバイダと Codex 認証のキャッシュ導入、エージェントループの並列ツール実行を行う。

> **Note**: 以下の具体的なコード例・API 設計・構成（How）はあくまで参考である。実装時によりよい設計方針があれば積極的に採用すること。

## 設計方針

- **重複排除は抽出関数で**: チャネル間の同一フローは、レスポンス送信をクロージャ/コールバックで注入する共有関数に抽出。チャネル固有の送信ロジックのみ各チャネルに残す
- **型統合は image variant のみ分離**: `PersistedMessage` は `llm::Message` の `role` / `tool_calls` / `tool_call_id` をそのまま再利用し、画像表現（data URL ↔ asset ref）の差異のみ別型で表現。変換関数は `AssetStore` メソッドに集約し、~100 行の変換ボイラープレートを削減
- **キャッシュは AppState 内 `Mutex<HashMap>`**: `soul_agents.rs` の `Mutex<Option<CachedContent>>` パターンに倣い、AppState に `LlmProviderCache` を追加。**キャッシュキーは `ResolvedLlmConfig` 全フィールドのハッシュ**（`provider`, `model`, `base_url`, `api_key`, `account_id` を全て含む）。設定ファイル変更で解決結果が変われば自動的に異なるキー → キャッシュミス → 新プロバイダ生成となり、明示的な `clear()` が不要
- **Codex 認証は TTL 付きキャッシュ**: `Arc<Mutex<Option<(Instant, CodexAuth)>>>` でキャッシュ。TTL（例: 5 分）経過かファイル mtime 変更で再読込。既存の `LazyLock<Mutex<()>>` リフレッシュロックはそのまま活用
- **並列ツール実行は read-only ツールのみ**: Tool trait に `is_read_only(&self) -> bool` を追加。`read`, `grep`, `find`, `ls`, `activate_skill` は `true`、`bash`, `write`, `edit`, `send_message` は `false`。**全 tool_call が read-only の場合のみ `join_all` で並列化**し、1 つでも副作用ありツールが含まれる場合は従来通り逐次実行

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | ファイル | Issue 項目 |
|---|---|---|
| SurfaceContext 型定義 | `src/agent_loop/mod.rs` | #1 |
| CLI チャネルハンドラ | `src/channels/cli.rs` | #1, #2 |
| Discord チャネルハンドラ | `src/channels/discord.rs` | #1, #2 |
| Telegram チャネルハンドラ | `src/channels/telegram.rs` | #1, #2 |
| TUI チャネルハンドラ | `src/channels/tui.rs` | #1, #2 |
| スラッシュコマンド | `src/slash_commands.rs` | #2 |
| PersistedMessage 型 | `src/agent_loop/session.rs` | #3 |
| llm::Message 型 | `src/llm/mod.rs` | #3 |
| LLM プロバイダ生成 | `src/runtime.rs` | #4 |
| LLM プロバイダ抽象 | `src/llm/mod.rs` | #4 |
| OpenAI プロバイダ | `src/llm/openai.rs` | #4, #5 |
| Codex 認証 | `src/codex_auth.rs` | #5 |
| エージェントループ turn | `src/agent_loop/turn.rs` | #6 |
| ツールレジストリ | `src/tools/mod.rs` | #6 |
| アーキテクチャドキュメント | `docs/architecture.md` | 全般 |

---

## Step 0: Worktree 作成

```bash
git worktree add ../egopulse-dedup-cache -b refactor/arch-dedup-cache
```

---

## Step 1: SurfaceContext コンストラクタ共通化 (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `surface_context_new_sets_all_fields` | `new("cli", "user", "thread", "cli", "default")` が全フィールドに正しく設定される |
| `surface_context_new_clones_strings` | 渡した文字列の所有権が移動せず clone される |

### GREEN: 実装

- `SurfaceContext` に `new(channel, surface_user, surface_thread, chat_type, agent_id) -> Self` を追加
- 各チャネル (cli, discord, telegram, tui) の inline 構築を `SurfaceContext::new(...)` 呼び出しに置き換え
- Discord の `make_context` ヘルパーは `SurfaceContext::new` を内部で呼ぶよう修正
- Telegram の 2 箇所（スラッシュコマンド用 + 通常メッセージ用）の重複構築を単一の変数に統合

### コミット

`refactor: add SurfaceContext::new() and deduplicate channel construction`

---

## Step 2: スラッシュコマンド処理フロー共通化 (TDD)

前提: Step 1

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `process_slash_command_unknown_returns_unknown_response` | 未対応コマンド → `unknown_command_response()` 相当のメッセージ |
| `process_slash_command_known_returns_response` | 既知コマンド → `Some(response)` を返す |
| `process_slash_command_resolve_error_returns_error_message` | chat_id 解決失敗 → エラーメッセージ |

### GREEN: 実装

- `slash_commands.rs` に `process_slash_command(state, context, text, sender_id) -> SlashCommandOutcome` を追加
  - `SlashCommandOutcome` = `Respond(String)` | `Error(String)` | `NotHandled`
  - 内部で `is_slash_command` → `resolve_chat_id` → `handle_slash_command` → unknown フォールバック を一括実行
- 各チャネルの 20-40 行のスラッシュコマンドブロックを `process_slash_command()` 呼び出し + 送信に置き換え
  - CLI: `writeln!(stdout, ...)` で送信
  - Discord: `send_discord_response(...)` で送信
  - Telegram: `send_telegram_response(...)` で送信
  - TUI: `chat.messages.push(RenderedMessage{...})` で送信
- 各チャネルの送信ロジックは残す（共有関数は結果のみ返す）

### コミット

`refactor: extract shared slash command flow into process_slash_command()`

---

## Step 3: PersistedMessage / Message 型統合 (TDD)

前提: なし（Step 1, 2 と独立）

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `persist_restore_text_roundtrip` | テキストメッセージの保存→復元で内容一致 |
| `persist_restore_image_roundtrip` | 画像付きメッセージの保存→復元で asset ref ↔ data URL 変換が正しい |
| `persist_restore_tool_calls_preserved` | tool_calls / tool_call_id が保存・復元で保持される |
| `persist_restore_mixed_content_parts` | テキスト + 画像混在パートの往復変換 |
| `persist_missing_image_falls_back_to_text` | asset 読込失敗時の fallback メッセージ |

### GREEN: 実装

- `PersistedMessage` を削除し、`llm::Message` を直接セッションストレージに使用
- `PersistedMessageContent` / `PersistedMessageContentPart` を削除
- `MessageContentPart` に `InputImageRef` バリアントを追加（または serde カスタムシリアライズで `InputImage` を asset ref 形式に変換）
- `persist_messages` / `restore_snapshot_messages` を `AssetStore` のメソッドに集約:
  - `AssetStore::persist_messages(&self, msgs: &[Message]) -> Result<Vec<Message>>` — `InputImage` を `InputImageRef` に変換した Message を返す
  - `AssetStore::restore_messages(&self, json: &str) -> Result<Vec<Message>>` — `InputImageRef` を `InputImage` に復元
- 変換は image part のみ発生。text / tool_calls / tool_call_id はそのまま通す
- セッションの JSON シリアライズフォーマットは `InputImageRef` を使う形に統一（既存セッションとの後方互換は維持しない）

### コミット

`refactor: unify PersistedMessage and llm::Message types`

---

## Step 4: LLM プロバイダキャッシュ導入 (TDD)

前提: なし（Step 1-3 と独立）

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `llm_cache_returns_same_provider_for_same_config` | 同一 `ResolvedLlmConfig` → 同一 Arc ポインタ |
| `llm_cache_returns_different_provider_for_different_model` | 異なる model → 別インスタンス |
| `llm_cache_returns_different_provider_for_different_base_url` | 同じ model でも base_url が違えば別インスタンス |
| `llm_cache_reflects_config_file_change` | 設定ファイル書き換え後、解決結果が変われば新インスタンス（キーハッシュ違いで自動ミス） |
| `llm_cache_skipped_for_override` | llm_override 設定時はキャッシュ使わず直接返す |

### GREEN: 実装

- `AppState` に `llm_cache: Arc<Mutex<HashMap<u64, Arc<dyn LlmProvider>>>>` を追加
- キャッシュキー: `ResolvedLlmConfig` の全フィールド（`provider`, `model`, `base_url`, `api_key`, `account_id`）をシリアライズしてハッシュ化。`api_key` が変更されればキーも変わるため、設定変更が自動反映される
- `llm_for_channel` / `llm_for_context` / `global_llm` の各メソッドで:
  1. `llm_override` があれば従来通り即返す
  2. `try_current_config()` で設定を読込 → `resolve_llm_*` で `ResolvedLlmConfig` を取得
  3. `ResolvedLlmConfig` のハッシュをキャッシュキーとして検索
  4. ヒット → `Arc::clone` して返す
  5. ミス → `create_provider()` で生成 → キャッシュに保存 → 返す
- **明示的な `clear()` は不要**: 設定ファイル変更で解決結果が変わればハッシュが変わり自動ミス。変わらなければキャッシュヒットで正しい
- `reqwest::Client` はプロバイダに内包されたまま（プロバイダキャッシュで間接的に再利用される）

### コミット

`perf: add LLM provider cache keyed by full ResolvedLlmConfig hash`

---

## Step 5: Codex 認証キャッシュ導入 (TDD)

前提: なし（Step 4 と独立）

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `codex_auth_cache_returns_cached_value_within_ttl` | TTL 内は再読込なし |
| `codex_auth_cache_refreshes_after_ttl` | TTL 経過後は再読込 |
| `codex_auth_env_var_bypasses_cache` | `OPENAI_CODEX_ACCESS_TOKEN` 設定時はキャッシュ使わず |
| `codex_auth_cache_returns_error_when_no_auth` | auth.json なし + env var なし → エラー |

### GREEN: 実装

- `codex_auth.rs` に `static CACHE: LazyLock<Mutex<Option<(Instant, CodexAuth)>>>` を追加
- `resolve_codex_auth()` を修正:
  1. env var チェック（キャッシュ使わず、即返す）
  2. キャッシュ確認 → TTL 内なら `clone` して返す
  3. TTL 経過 → ファイル読込 → キャッシュ更新 → 返す
- TTL 定数: `const CODEX_AUTH_TTL: Duration = Duration::from_secs(300);`（5 分）
- 既存の `REFRESH_LOCK` はそのまま活用（リフレッシュ直列化）

### コミット

`perf: add TTL-based cache for Codex auth resolution`

---

## Step 6: エージェントループ並列ツール実行 (TDD)

前提: なし（Step 1-5 と独立）

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `parallel_tools_read_only_all_parallel` | read-only ツール 3 つ → 並列実行、結果が tool_call 順序で返る |
| `parallel_tools_single_tool_no_change` | 1 ツール時は従来と同じ挙動 |
| `parallel_tools_partial_failure_collects_all` | read-only で一部失敗 → 全結果（成功 + エラー）が返る |
| `parallel_tools_events_emitted_concurrently` | 並列実行時に ToolStart / ToolResult イベントが各ツールで発火 |
| `parallel_tools_mixed_falls_back_to_sequential` | read-only + 副作用あり混在 → 逐次実行（`is_read_only` 判定） |
| `parallel_tools_write_tools_always_sequential` | bash + edit 等の副作用ツールのみ → 逐次実行 |

### GREEN: 実装

- `Tool` trait に `fn is_read_only(&self) -> bool` を追加（デフォルト実装: `false`）
- 各 built-in tool でオーバーライド:
  - `true`: `ReadTool`, `GrepTool`, `FindTool`, `LsTool`, `ActivateSkillTool`
  - `false`（デフォルト）: `BashTool`, `WriteTool`, `EditTool`, `SendMessageTool`
- `ToolRegistry` に `is_read_only(&self, name: &str) -> bool` を追加
- `execute_and_persist_tools()` のループを条件分岐に変更:
  - **全 tool_call が read-only の場合**: `join_all` で並列実行
  - **1 つでも副作用ありツールが含まれる場合**: 従来通り `for` ループで逐次実行
- 並列実行時のエラーハンドリング: `join_all` で全結果を収集後、`Result::err` を抽出して最初のエラーを返す（または全エラーを集約）
- DB の `store_pending_tool_call` / `update_tool_call_output` は異なる tool_call.id で操作するため並列でも競合なし

### コミット

`perf: parallelize independent tool calls with join_all`

---

## Step 7: ドキュメント更新

### 内容

- `docs/architecture.md`: SurfaceContext コンストラクタ、スラッシュコマンド共有フロー、LLM プロバイダキャッシュ、並列ツール実行の記載を更新

### コミット

`docs: update architecture doc for dedup and cache changes`

---

## Step 8: 動作確認

```bash
cargo fmt --check
cargo check -p egopulse
cargo clippy --all-targets --all-features -- -D warnings
cargo test -p egopulse
```

---

## Step 9: PR 作成

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
|---|---|---|
| `src/agent_loop/mod.rs` | 変更 | `SurfaceContext::new()` 追加 |
| `src/channels/cli.rs` | 変更 | コンストラクタ置換、スラッシュコマンド共通化 |
| `src/channels/discord.rs` | 変更 | `make_context` 内部化、スラッシュコマンド共通化 |
| `src/channels/telegram.rs` | 変更 | 重複構築統合、スラッシュコマンド共通化 |
| `src/channels/tui.rs` | 変更 | コンストラクタ置換、スラッシュコマンド共通化 |
| `src/slash_commands.rs` | 変更 | `process_slash_command()` / `SlashCommandOutcome` 追加 |
| `src/agent_loop/session.rs` | 変更 | PersistedMessage 削除、変換関数を AssetStore メソッドに集約 |
| `src/llm/mod.rs` | 変更 | `InputImageRef` バリアント追加（または serde カスタム） |
| `src/runtime.rs` | 変更 | `llm_cache` フィールド追加、3 メソッドのキャッシュ対応 |
| `src/llm/openai.rs` | 変更 | `build_headers` でのキャッシュ利用（Step 5 で間接的に対応） |
| `src/codex_auth.rs` | 変更 | TTL 付きキャッシュ追加 |
| `src/agent_loop/turn.rs` | 変更 | read-only 判定 + `join_all` による並列ツール実行 |
| `src/tools/mod.rs` | 変更 | `Tool` trait に `is_read_only` 追加、`ToolRegistry::is_read_only` 追加 |
| `docs/architecture.md` | 変更 | アーキテクチャ記述の更新 |

---

## コミット分割

1. `refactor: add SurfaceContext::new() and deduplicate channel construction` — `agent_loop/mod.rs`, `channels/cli.rs`, `channels/discord.rs`, `channels/telegram.rs`, `channels/tui.rs`
2. `refactor: extract shared slash command flow into process_slash_command()` — `slash_commands.rs`, `channels/cli.rs`, `channels/discord.rs`, `channels/telegram.rs`, `channels/tui.rs`
3. `refactor: unify PersistedMessage and llm::Message types` — `agent_loop/session.rs`, `llm/mod.rs`
4. `perf: add LLM provider cache keyed by full ResolvedLlmConfig hash` — `runtime.rs`
5. `perf: add TTL-based cache for Codex auth resolution` — `codex_auth.rs`
6. `perf: parallelize read-only tool calls with join_all` — `agent_loop/turn.rs`, `tools/mod.rs`
7. `docs: update architecture doc for dedup and cache changes` — `docs/architecture.md`

---

## テストケース一覧（全 25 件）

### SurfaceContext コンストラクタ (2)
1. `surface_context_new_sets_all_fields` — 全フィールド正しく設定
2. `surface_context_new_clones_strings` — 文字列の所有権保持

### スラッシュコマンド共通化 (3)
3. `process_slash_command_unknown_returns_unknown_response` — 未対応コマンド
4. `process_slash_command_known_returns_response` — 既知コマンド
5. `process_slash_command_resolve_error_returns_error_message` — chat_id 解決失敗

### PersistedMessage / Message 統合 (5)
6. `persist_restore_text_roundtrip` — テキスト往復変換
7. `persist_restore_image_roundtrip` — 画像往復変換
8. `persist_restore_tool_calls_preserved` — tool_calls 保持
9. `persist_restore_mixed_content_parts` — テキスト+画像混在
10. `persist_missing_image_falls_back_to_text` — asset 読込失敗フォールバック

### LLM プロバイダキャッシュ (5)
11. `llm_cache_returns_same_provider_for_same_config` — 同一キャッシュヒット
12. `llm_cache_returns_different_provider_for_different_model` — 異モデル別インスタンス
13. `llm_cache_returns_different_provider_for_different_base_url` — 異 base_url 別インスタンス
14. `llm_cache_reflects_config_file_change` — 設定変更で自動キャッシュミス
15. `llm_cache_skipped_for_override` — override 時キャッシュ不使用

### Codex 認証キャッシュ (4)
16. `codex_auth_cache_returns_cached_value_within_ttl` — TTL 内キャッシュヒット
17. `codex_auth_cache_refreshes_after_ttl` — TTL 経過後再読込
18. `codex_auth_env_var_bypasses_cache` — env var バイパス
19. `codex_auth_cache_returns_error_when_no_auth` — auth 無しエラー

### 並列ツール実行 (6)
20. `parallel_tools_read_only_all_parallel` — read-only 3 ツール並列 + 順序維持
21. `parallel_tools_single_tool_no_change` — 1 ツール時従来通り
22. `parallel_tools_partial_failure_collects_all` — 部分失敗全収集
23. `parallel_tools_events_emitted_concurrently` — イベント並列発火
24. `parallel_tools_mixed_falls_back_to_sequential` — read-only + 副作用混在 → 逐次
25. `parallel_tools_write_tools_always_sequential` — 副作用ツールのみ → 逐次

### ドキュメント (0 — レビューのみ)

---

## 工数見積もり

| Step | 内容 | 見積もり |
|---|---|---|
| Step 0 | Worktree 作成 | ~5 行 |
| Step 1 | SurfaceContext コンストラクタ | ~80 行 |
| Step 2 | スラッシュコマンド共通化 | ~150 行 |
| Step 3 | PersistedMessage / Message 統合 | ~200 行 |
| Step 4 | LLM プロバイダキャッシュ | ~120 行 |
| Step 5 | Codex 認証キャッシュ | ~80 行 |
| Step 6 | 並列ツール実行 | ~100 行 |
| Step 7 | ドキュメント更新 | ~40 行 |
| **合計** | | **~775 行** |
