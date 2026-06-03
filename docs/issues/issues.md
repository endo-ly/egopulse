# Issue 一覧

Issue 一覧を記載する。完了したら削除する。

---

## 既存 Issue

### 13. Config ホットリロードの仕組みが不明

「ホットリロード可能」と分類されているフィールドがあるが、**どのタイミングで再読込されるのか**が未文書化。inotify watch？ API 呼び出し時の都度読み込み？ YAML を直接編集した場合いつ反映されるか不明確。

### 16. Pulse 通知が `require_mention` をバイパス

Pulse が Discord チャネルに通知を送る際、`require_mention` 設定を確認せず直接送信。`require_mention: true` のチャネルに突然 Bot から通知が飛んでくることをユーザーが予期していない可能性。

### 20. `context_window_tokens` の手動設定

各モデルの token 数は手動設定。プロバイダーの models API から自動取得する仕組みなし。OpenRouter 等は `/models` で取得可能なので将来自動解決を検討すべき。

---

### P0. Built-in Tools ドキュメントの実装参照が古い

分類: 変更・運用の持続性

根拠:

- `docs/tools.md` は実装本体を `src/tools.rs` としているが、現行は `src/tools/` 配下に分割されている。
- `docs/tools.md` は `src/tools/mcp_adapter.rs` を参照しているが、現行ツリーに該当ファイルはなく MCP 実装は `src/tools/mcp.rs` にある。
- `edit` の節には top-level `oldText` / `newText` 受け入れが記載されていた。

問題:

- 新規変更者が誤ったファイルを追い、調査コストが増える。
- ツール仕様は LLM に読ませる可能性が高く、古い参照は実装ミスを誘発しやすい。

対応候補:

- `docs/tools.md` の実装リンクを現行ファイルへ更新する。
- 各 tool の実装所在地を `read/write/edit: src/tools/files.rs`, `bash: src/tools/shell.rs`, `grep/find/ls: src/tools/search.rs`, `web_fetch: src/tools/web_fetch/` のように明示する。
- 正規 schema から外れる入力は受け付けない。

検証:

- `rg "src/tools.rs|mcp_adapter" docs` が archived plan 以外でヒットしないこと。

### P1. Storage query が巨大化し、DB 契約の変更影響が読みにくい

分類: 境界・依存の健全性 / 変更・運用の持続性

根拠:

- `src/storage/queries.rs` は 4,500 行超。
- chats / messages / sessions / sleep_runs / episode_events / rollups / pulse_runs など複数の集約が同一ファイルに集中している。
- テストも同一ファイル後半にまとまっており、対象領域ごとの差分レビューが重くなっている。

問題:

- DB スキーマ変更時の影響範囲がファイル単位で大きく見え、レビューしづらい。
- `Database` の impl が大きくなり、クエリ契約のまとまりが見えにくい。

対応候補:

- `storage/queries/` 配下に `chats.rs`, `messages.rs`, `sessions.rs`, `sleep_runs.rs`, `episode_events.rs`, `pulse_runs.rs` などへ段階分割する。
- まずテストだけを領域別ファイルへ移し、次に impl を移すと安全。
- DB public facade は `Database` のまま維持し、呼び出し側の変更を最小にする。

検証:

- 分割前後で `cargo test storage::queries storage::migration` が通ること。
- `cargo clippy --all-targets --all-features -- -D warnings` で dead code が増えないこと。

### P1. Session / messages / episode_events の正本関係をコード境界で明示する余地がある

分類: 目的・概念の妥当性

根拠:

- `sessions.messages_json` は turn context のスナップショットとして扱われ、sleep 後に `clear_session_messages` で空配列になる。
- `messages` テーブルは永続履歴として残り、Event Extraction の backfill は `messages` から実行されている。
- `episode_events` は episodic memory の正本として扱われる。

問題:

- 3つのデータソースがそれぞれ「正本」「作業用 snapshot」「生成物」のどれなのか、コード上の型境界だけでは追いにくい。
- 今後の memory / pulse / search 機能追加で、誤って `sessions.messages_json` を履歴正本として使うリスクがある。

対応候補:

- `SessionSnapshot`、`StoredMessage`、`EpisodeEvent` 周辺に「用途の違い」を doc comment と module 境界で明示する。
- sleep の chunk builder 名を `build_memory_update_chunks_from_session_snapshot` のように用途寄りへ寄せる。
- `docs/db.md` と `docs/sleep.md` に「正本関係」セクションを追加する。

検証:

- Event Extraction / Memory Update / archive clear のテスト名が、どのデータソースを使うかを明示していること。

### P2. 公開モジュール境界が広く、private-first 方針とズレがある

分類: 境界・依存の健全性

根拠:

- `src/lib.rs` で `agent_loop`, `channels`, `config`, `runtime`, `setup`, `sleep`, `storage` が `pub mod` になっている。
- 実体は単一バイナリ中心で、外部ライブラリ API としての安定公開が必要な範囲は限定的に見える。

問題:

- `pub` が広いほど public item doc / API 互換の責任が増える。
- 内部都合の型が crate 外から見えると、将来のリファクタに制約がかかる。

対応候補:

- binary entrypoint から必要な API だけを `pub(crate)` または小さな facade に寄せる。
- `main.rs` から必要なものだけを公開する `app` / `cli` facade を検討する。
- まず `cargo clippy --all-targets --all-features -- -D warnings` で公開範囲起因の dead code を確認する。

検証:

- 公開範囲縮小後も `cargo doc --no-deps` が通ること。

### P2. Runtime 起動・監視の責務が `runtime/mod.rs` に集中している

分類: 変更・運用の持続性 / 境界・依存の健全性

根拠:

- `runtime/mod.rs` は AppState 構築、agent turn worker、MCP reconnect loop、Web / Discord / Telegram / sleep scheduler / pulse scheduler 起動、監視ループを持つ。
- `start_channels` 周辺にチャネルごとの起動詳細がまとまっている。

問題:

- 新しい常駐タスク追加時に `runtime/mod.rs` の変更が増え、既存チャネル起動に影響しやすい。
- 起動失敗、再接続、graceful shutdown の契約がタスク種別ごとに散らばりやすい。

対応候補:

- `runtime/tasks.rs` または `runtime/supervisor.rs` を作り、常駐タスクの登録・監視・停止契約を切り出す。
- Web / Discord / Telegram / sleep / pulse を同じ `RuntimeTaskSpec` のような形で登録する。
- まず挙動を変えずに、起動処理の切り出しだけを行う。

検証:

- チャネル無効時の `NoActiveChannels`、チャネル起動失敗時の shutdown、scheduler 起動有無のテストを追加する。

---

## 集約・共通化リファクタ Issue

> コードベース全体の横断調査で発見した「分割より集約」方向のリファクタ候補。
> 影響範囲・削減見込み LOC・リスクを併記。

---

### R0. Discord / Telegram ボット実装の共通化

分類: 集約・共通化 / 変更・運用の持続性

根拠:

- `discord.rs`（2000+ 行）と `telegram.rs`（1500+ 行）に、構造がほぼ同一のコードが大量に存在する。
- チャネル固有の差分は「キー型（`u64` / `i64`）」「フレームワーク（serenity / teloxide）」「メッセージ形式」のみで、ルーティング・チェーン管理・設定構造はロジックごと重複している。
- 新しいボットチャネルを追加する際、どちらかをベースにコピペする構造になっている。

具体的な重複:

| 重複箇所 | discord.rs 行 | telegram.rs 行 | 同一度 |
|---|---|---|---|
| `BotChainState` + `ChainEntry` 構造体とメソッド群 | L57-117 | L48-105 | 100%（キー型のみ差） |
| `ReceiveDecision` / `RouteDecision` enum | L130-159 | L122-151 | 100% |
| `route_message()` / `resolve_single_agent_channel()` / `resolve_multi_agent_room()` | L440-491 | L200-243 | 95% |
| API retry ロジック（429 rate limit 処理） | L365-399 | L574-680 | 85% |
| channel log storage 処理 | L587-644 | L332-390 | 構造同一 |
| `agent_uses_this_bot()` / `first_agent_for_this_bot()` / `primary_agent_for_this_bot()` | Handler 内 | Handler 内 | 同一 |
| `should_process_message()` | Handler 内 | Handler 内 | 同一 |
| `make_context()` | Handler 内 | Handler 内 | 同一 |

設定構造体の重複（`config/types.rs`）:

| 構造体 | 行 | 備考 |
|---|---|---|
| `DiscordChannelConfig` / `TelegramChatConfig` | L77-110 | フィールド 100% 同一（`require_mention`, `agents`, `multi_agent`） |
| `DiscordBotConfig` / `TelegramBotConfig` | L100-122 | フィールド 100% 同一（`token`, `file_token`） |

共通定数の重複:

```
BOT_CHAIN_MAX_DEPTH: u32 = 5   // 両ファイルで同一値
BOT_CHAIN_TTL_SECS: u64 = 300  // 両ファイルで同一値
MAX_RETRIES: u32 = 3           // 両ファイルで実質同一
```

対応候補:

- `src/channels/bot_common.rs`（新規）を作成し、以下を集約:
  - `BotChainState<K: Copy + Eq + Hash>` ジェネリック構造体
  - `ReceiveDecision` / `RouteDecision` enum
  - ルーティングロジックのトレイト化（`BotRouter<K>`）
  - 共通定数
- `src/config/types.rs` で `ChannelChatConfig` / `BotTokenConfig` に統一:
  - `DiscordChannelConfig` と `TelegramChatConfig` → `ChannelChatConfig`
  - `DiscordBotConfig` と `TelegramBotConfig` → `BotTokenConfig`
- Handler の共通メソッドを `BotHandler` トレイトに抽出
- API retry ロジックを `channels/utils/` 配下にジェネリック関数として抽出

推定削減 LOC: ~500 行

リスク: 低（既存の振る舞いを変更せず、構造抽出のみ）

検証:

- 抽出前後で Discord / Telegram チャネルの E2E テストが通ること
- `cargo clippy --all-targets --all-features -- -D warnings` で dead code が増えないこと
- 新しいボットチャネル追加時に `bot_common.rs` のトレイト実装だけで完結できること

---

### R1. 文字列トランケート関数の統合

分類: 集約・共通化

根拠:

- 「文字列を切り詰める」目的の関数が、実質的に同じ処理をモジュールごとに別名で定義している。
- 全部で 10+ 種類の切り詰め関数が存在し、「文字数ベース」系だけでも 7箇所に分散している。

| 関数 | ファイル | 方式 |
|---|---|---|
| `truncate_by_chars()` | `channels/utils/text.rs` | 文字数 |
| `preview_text()` | `agent_loop/formatting.rs` | 文字数 |
| `truncate_summary_text()` | `agent_loop/formatting.rs` | 文字数 |
| `truncate_summary_for_target()` | `agent_loop/compaction.rs` | 文字数 |
| `truncate_string_to_bytes_from_end()` | `tools/text.rs` | バイト数 |
| `truncate_head()` / `truncate_tail()` | `tools/text.rs` | 行数 + バイト数 |
| `truncate_output()` | `tools/web_fetch/mod.rs` | バイト数 |
| `truncate_grep_line()` | `tools/search.rs` | 固定長 |
| `truncate_chars()` | `skills.rs` | 文字数 |
| `truncate_preview()` | `channels/tui.rs` | 固定長 |
| `preview_body()` | `llm/responses.rs` | 文字数 |

対応候補:

- 3系統に整理して `src/text_utils.rs`（新規）に集約:
  - **文字数ベース**: `truncate_by_chars(s, max) -> String`（省略記号付き）
  - **バイト数ベース**: `truncate_by_bytes(s, max) -> String`（UTF-8 境界安全）
  - **行数 + バイト数ベース**: `TruncationResult` 付きの既存 `truncate_head` / `truncate_tail` をそのまま移管
- 各モジュールの個別関数は `text_utils` の呼び出しに差し替え
- モジュール固有のフォーマット（`shell_quote`, `normalize_newlines` 等）は `tools/text.rs` に残す

推定削減 LOC: ~100 行

リスク: 低（各関数の入出力仕様は同じなので、呼び出し先を差し替えるだけ）

検証:

- 差し替え前後でテストが通ること
- 既存のテストがない切り詰め関数には、抽出時にユニットテストを追加すること

---

### R1. エラー型のバリアント重複の整理

分類: 集約・共通化 / 境界・依存の健全性

根拠:

- 15個のエラー型（`error.rs` に 8個、モジュールローカルに 7個）があるが、似たバリアントが横断的に存在し、形状が不統一。

重複バリアント:

| バリアントパターン | 出現箇所 | 備考 |
|---|---|---|
| `NotFound` 系 | `StorageError(String)`, `ChannelError(String)`, `ConfigError` × 5 variant | `String` と名前付きフィールドが混在 |
| `InitFailed(String)` | `TuiError`, `LlmError`, `LoggingError`, `StorageError` | 4箇所で同一形状 |
| `ParseFailed` 系 | `ConfigError::ConfigParseFailed`, `SleepBatchError::ParseFailed`, `PulseParseError::ParseFailed` | 3箇所、形状がバラバラ |
| `UnsafeAgentId` | `SleepBatchError(String)`, `PulseParseError { agent_id }` | 形状不一致 |
| `Io` | `StorageError(#[from])`, `MediaError(#[from])`, `SleepBatchError(String)` | `#[from]` と手動 `String` が混在 |

即時改善点（小粒）:

- `SleepBatchError::Io(String)` → `Io(#[from] std::io::Error)` に変更（`map_err` ボイラープレート削減）
- `UnsafeAgentId` の形状統一（名前付きフィールド `{ agent_id: String }` に統一）
- `SummarizeError`（`agent_loop/compaction.rs`、private）に `#[derive(Debug, Error)]` を追加
- `ValidationFailure`（`tools/web_fetch/content_validation.rs`）の manual `Display` impl を thiserror に置換

中長期改善点:

- `InitFailed(String)` パターンが 4つのエラー型に出現 → ドメイン固有として許容するか、共通化を検討
- `ParseFailed` 系の形状統一（`{ source, detail }` または `(String)` に統一）

推定削減 LOC: ~30 行（ボイラープレート削減）、保守性向上の効果が大きい

リスク: 低

検証:

- 各変更後に `cargo test` が通ること
- `SleepBatchError::Io` 変更後に `#[from]` で自動導出される `From<std::io::Error>` がコンパイル通ること

---

### R1. Config Secret 系フィールドの共通型化

分類: 集約・共通化

根拠:

- `token: Option<ResolvedValue>` + `file_token: Option<yaml_serde::Value>` のペアが 4箇所に同じ形で出現している。

| 構造体 | ファイル | 行 |
|---|---|---|
| `TelegramBotConfig` | `config/types.rs` | L100-103 |
| `DiscordBotConfig` | `config/types.rs` | L119-122 |
| `ProviderConfig` | `config/types.rs` | ~L180 |
| `ChannelConfig`（`auth_token` 系） | `config/types.rs` | ~L137 |

対応候補:

- 共通型を定義:
  ```rust
  pub(crate) struct SecretConfig {
      pub resolved: Option<ResolvedValue>,
      pub file_ref: Option<yaml_serde::Value>,
  }
  ```
- 4箇所のフィールドペアを `SecretConfig` に置き換え
- 直列化/逆直列化の互換性に注意（既存 YAML との後方互換）

推定削減 LOC: ~40 行

リスク: 低〜中（YAML 直列化形式の互換性確認が必要）

検証:

- 既存の設定ファイルが `SecretConfig` 形式でも正常に読み込めること
- `cargo test config` が通ること

---

### R2. LLM メッセージシリアライズの内部重複の解消

分類: 集約・共通化

根拠:

- `llm/messages.rs` で Chat Completions API と Responses API の 2形式をサポートするため、同一プロバイダ内で 90〜95% 同一のコードが存在する。
- ただし現状の分離は「どの API 形式の処理か」をファイル内で明確に分けており、可読性の観点では悪くない。

重複箇所:

| 関数ペア | 行 | 同一度 | 差分 |
|---|---|---|---|
| `append_chat_tools()` / `append_responses_tools()` | L238-279 | 95% | `tool_choice: "auto"` の有無のみ |
| `chat_content_part()` / `responses_content_part()` | L285-321 | 90% | JSON フィールド名（`"text"` vs `"input_text"` 等） |
| `translate_content_to_chat_completions()` / `translate_content_to_responses_message()` | L155-218 | 構造類似 | 戻り型が `Value` vs `Vec<Value>` |

対応候補:

- `ApiFormat` enum を導入し、差分（フィールド名・`tool_choice` の有無）をメソッドで抽象化
- 共通ロジックを 1つの関数に統合し、`ApiFormat` で分岐
- **または**: 現状の分離を維持する（可読性重視の判断もあり得る）

推定削減 LOC: ~50 行

リスク: 中（OpenAI API 仕様変更時に影響箇所が集約される反面、抽象化レイヤーのメンテが必要）

検証:

- 統合前後で Chat Completions API / Responses API 両方のテストが通ること

---

### R2. スラッシュコマンド処理ボイラープレートの共通化

分類: 集約・共通化

根拠:

- CLI / TUI / Discord / Telegram / Web の全チャネルで、ほぼ同一のスラッシュコマンド分岐パターンが重複している。

```rust
// 5箇所（cli.rs, tui.rs, discord.rs, telegram.rs, web/stream.rs）で反復
match process_slash_command(state, &context, trimmed, None).await {
    SlashCommandOutcome::Respond(response) => { /* チャネルに送信 */ }
    SlashCommandOutcome::Error(response) => { /* チャネルに送信 */ }
    SlashCommandOutcome::NotHandled => { /* agent loop へ */ }
}
```

対応候補:

- `handle_slash_command_or_continue()` ヘルパー関数を `slash_commands.rs` または `channels/adapter.rs` に定義
- 戻り値を `Result<String, NotHandled>` のような形にして、各チャネルの送信ロジックに委譲

推定削減 LOC: ~50 行

リスク: 低

検証:

- 各チャネルでスラッシュコマンドの動作に退行がないこと

---

## 優先度マトリクス

| ID | 対象 | 推定削減 LOC | リスク | 優先度 |
|---|---|---|---|---|
| R0 | Discord/Telegram ボット共通化 | ~500 | 低 | **P0** |
| R0 | ユーティリティ関数のインライン反復共通化 | ~40（34+箇所の呼び出し簡略化） | 極低 | **P0** |
| R1 | トランケート関数統合 | ~100 | 低 | P1 |
| R1 | エラーバリアント整理 | ~30 | 低 | P1 |
| R1 | SecretConfig 集約 | ~40 | 低〜中 | P1 |
| R2 | LLM シリアライズ抽象化 | ~50 | 中 | P2 |
| R2 | スラッシュコマンド共通化 | ~50 | 低 | P2 |
