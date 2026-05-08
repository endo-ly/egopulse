# EgoPulse アーキテクチャ

システム全体のコンポーネント構成、データフロー、レイヤー構造を記述する。

## 目次

1. [全体像](#1-全体像)
2. [レイヤー構造](#2-レイヤー構造)
3. [モジュール構成](#3-モジュール構成)
4. [コア型](#4-コア型)
5. [リクエストフロー](#5-リクエストフロー)
6. [起動・停止シーケンス](#6-起動停止シーケンス)
7. [設計パターン](#7-設計パターン)

---

## 1. 全体像

EgoPulse は単一バイナリの Rust (Tokio) 製 AI エージェントランタイム。全コンポーネントが単一プロセス内で動作する。

```text
┌──────────────────────────────────────────────────────────┐
│                        main.rs                           │
│   CLI エントリポイント (chat / run / ask / setup / gateway)  │
└────────────┬──────────────────────────┬──────────────────┘
             │                          │
    ┌────────▼────────┐        ┌────────▼────────┐
    │  ローカルモード  │        │  サーバーモード   │
    │  (TUI / CLI)    │        │  (egopulse run)  │
    └────────┬────────┘        └────────┬────────┘
             │                          │
             │    ┌─────────────────────┼─────────────────────┐
             │    │                     │                     │
             │    │              runtime/                     │
             │    │          (AppState 構築・管理)             │
             │    └─────────────────────┬─────────────────────┘
             │                          │
             │    ┌─────────────────────┼─────────────────────┐
             │    │         channel 群 (tokio task)            │
             │    │   Web Server  │  Discord  │  Telegram     │
             │    └─────────────────────┬─────────────────────┘
             │                          │
             └──────────┬───────────────┘
                        │
             ┌──────────▼──────────┐
             │    agent_loop       │
             │  (process_turn)    │
             └──────────┬──────────┘
                        │
        ┌───────────────┼───────────────┐
        │               │               │
  ┌─────▼─────┐  ┌──────▼──────┐  ┌─────▼─────┐
  │  storage  │  │     llm     │  │   tools   │
  │ (SQLite)  │  │  (Provider) │  │ (built-in │
  │           │  │             │  │  + MCP)   │
  └───────────┘  └─────────────┘  └───────────┘
```

---

## 2. レイヤー構造

| レイヤー | 責務 | 主要モジュール |
|---------|------|--------------|
| **エントリポイント** | CLI 引数解析、サブコマンドの振り分け | `main.rs` |
| **ランタイム** | 依存注入、チャネル起動、ライフサイクル管理 | `runtime/` |
| **チャネル** | 外部プラットフォームとの通信 | `channels/` |
| **エージェントループ** | 会話ターン処理、LLM 呼び出し、ツール実行 | `agent_loop/` |
| **ドメインサービス** | LLM 抽象化、ツール、セッション管理 | `llm/`, `tools/`, `agent_loop/session.rs` |
| **インフラ** | 永続化、設定、セキュリティ | `storage/`, `config/`, `tools/mcp.rs`, `skills.rs` |

---

## 3. モジュール構成

```
src/
├── main.rs              # CLI エントリポイント
├── lib.rs               # 全モジュールの公開インターフェース
├── runtime/             # AppState 構築、チャネル起動・監視
│   ├── mod.rs           # AppState, build_app_state(), start_channels()
│   ├── gateway.rs       # systemd サービス管理
│   ├── logging.rs       # ログ初期化
│   └── status.rs        # ランタイムステータス
│
├── agent_loop/          # エージェントループ
│   ├── mod.rs           # SurfaceContext, process_turn()
│   ├── turn.rs          # LLM 呼び出し、ツール実行、compaction
│   ├── session.rs       # セッションロード・保存、競合解決
│   ├── prompt_builder.rs # システムプロンプト構築
│   ├── compaction.rs    # コンテキスト圧縮
│   ├── formatting.rs    # 出力フォーマット
│   └── guards.rs        # 各種チェック
│
├── channels/            # チャネル実装
│   ├── mod.rs           # ChannelAdapter trait, ChannelRegistry
│   ├── adapter.rs       # チャネルアダプター
│   ├── cli.rs           # CLI チャネル
│   ├── discord.rs       # Discord ボット
│   ├── telegram.rs      # Telegram ボット
│   ├── tui.rs           # TUI チャネル
│   ├── web/             # Web サーバー (Axum, SSE, WebSocket)
│   └── utils/           # チャネル共通ユーティリティ
│
├── llm/                 # LLM プロバイダー抽象化
│   ├── mod.rs           # LlmProvider trait, OpenAI 互換クライアント
│   └── codex_auth.rs    # Codex auth 解決、AUTH_CACHE
│
├── config/              # 設定管理
│   ├── mod.rs           # 型定義、公開ファサード
│   ├── loader.rs        # YAML 読み込み、正規化、検証
│   ├── persist.rs       # YAML 書き出し、アトミック書込
│   ├── resolve.rs       # モデル解決、チャネルアクセサ
│   └── secret_ref.rs    # SecretRef 型、.env 読み書き
│
├── storage/             # SQLite 永続化
│   ├── mod.rs           # Database struct, 型定義, new(), call_blocking()
│   ├── migration.rs     # スキーマ DDL, バージョン管理マイグレーション
│   └── queries.rs       # 全 CRUD クエリ (chats, messages, sessions, tool_calls, LLM usage)
│
├── tools/               # ツールシステム
│   ├── mod.rs           # ToolRegistry, Tool trait, is_read_only()
│   ├── mcp.rs           # MCP クライアント (外部ツールサーバー接続)
│   ├── activate_skill.rs # スキル遅延読み込み
│   ├── command_guard.rs # bash コマンド検閲
│   ├── path_guard.rs    # 機密パスブロック
│   ├── sanitizer.rs     # 出力リダクション
│   ├── search.rs        # grep / find / ls ツール
│   ├── send_message.rs  # メッセージ送信ツール
│   ├── shell.rs         # bash 実行ツール
│   ├── files.rs         # read / write / edit ツール
│   └── text.rs          # テキスト処理ツール
│
├── skills.rs            # スキル管理 (発見・読み込み・カタログ生成)
├── slash_commands.rs    # slash command dispatcher、LLM プロファイル管理
├── soul_agents.rs       # SOUL.md / AGENTS.md 読み込み
├── error.rs             # エラー型
└── test_env.rs          # テスト用 EnvVarGuard、ENV_MUTEX
```

---

## 4. コア型

### AppState

すべてのチャネルとエージェントループが共有する状態。

```rust
pub struct AppState {
    pub db: Arc<Database>,                     // SQLite
    pub config: Config,                        // 設定
    pub channels: Arc<ChannelRegistry>,         // 送信用チャネルアダプター
    pub skills: Arc<SkillManager>,             // スキルカタログ
    pub tools: Arc<ToolRegistry>,              // ツールレジストリ
    pub mcp_manager: Option<Arc<RwLock<McpManager>>>,
    pub assets: Arc<AssetStore>,               // 埋め込みアセット
    pub soul_agents: Arc<SoulAgentsLoader>,    // SOUL/AGENTS ローダー
    pub llm_cache: Mutex<HashMap<u64, Arc<dyn LlmProvider>>>,  // LLM provider cache
}
```

### SurfaceContext

メッセージの送信元を識別する型。チャネル・ユーザー・スレッドの組み合わせで一意になる。
`SurfaceContext::new()` が全チャネルで使用される正規コンストラクタ。

```rust
pub struct SurfaceContext {
    pub channel: String,         // "discord" | "telegram" | "web" | "tui" | "cli"
    pub surface_user: String,    // プラットフォーム固有のユーザー ID
    pub surface_thread: String,  // プラットフォームの会話スレッド ID
    pub chat_type: String,       // 永続化用チャット種別
    pub agent_id: String,        // 使用するエージェント定義のキー
}
```

### ChannelAdapter

全チャネルが実装する送信用 trait。

```rust
pub trait ChannelAdapter: Send + Sync {
    fn name(&self) -> &str;
    fn chat_type_routes(&self) -> Vec<(&str, ConversationKind)>;
    async fn send_text(&self, external_chat_id: &str, text: &str) -> Result<(), String>;
    async fn send_attachment(&self, external_chat_id: &str, text: Option<&str>, file_path: &Path, caption: Option<&str>) -> Result<(), String>;
}
```

---

## 5. リクエストフロー

```text
1. チャネルがメッセージを受信
      │
2. SurfaceContext を生成
      │  (channel, surface_user, surface_thread, agent_id)
      │
3. agent_loop::process_turn(state, ctx, message, attachments)
      │
      ├─ 3a. chat_id を解決 (chats テーブルを upsert)
      ├─ 3b. session snapshot をロード (sessions テーブル)
      ├─ 3c. Safety Compaction 判定 (token estimate >= threshold)
      │
      ├─ 3d. system prompt を構築
      │      ├ SOUL.md (agent → channel → global の順に解決)
      │      ├ AGENTS.md (global + agent の累積)
      │      └ skills catalog
      │
      ├─ 3e. LLM に messages + tools を送信
      │      │
      │      ├─ tool_call があれば:
      │      │  ├ ツール実行 (read-only は join_all で並列、それ以外は逐次)
      │      │  ├ 結果を messages に追加
      │      │  └ 3e に戻る (最大 50 イテレーション)
      │      │
      │      └─ tool_call がなければ → 最終応答
      │
      ├─ 3f. メッセージを永続化
      │      ├ messages テーブルに INSERT
      │      ├ tool_calls テーブルに INSERT
      │      ├ sessions テーブルを UPDATE (楽観ロック)
      │      └ llm_usage_logs テーブルに INSERT
      │
      └─ 3g. 応答を channel adapter 経由で返送
```

---

## 6. 起動・停止シーケンス

### 起動

```text
1. CLI 引数解析 (clap)
      │
2. Config YAML をロード (~/.egopulse/egopulse.config.yaml)
      │
3. build_app_state()
      │
      ├─ Database 初期化 (SQLite WAL, マイグレーション)
      ├─ SkillManager 構築
      ├─ McpManager 初期化 (MCP server 接続)
      ├─ ToolRegistry 構築 (built-in + MCP adapters)
      ├─ ChannelAdapter 登録
      └─ SOUL.md プロビジョニング
      │
4. start_channels()
      │
      ├─ status.json を書き出し
      ├─ Web server 起動 (tokio::spawn)
      ├─ Discord bot 起動 (tokio::spawn × bot 数)
      ├─ Telegram bot 起動 (tokio::spawn)
      │
      └─ 監視ループ (2 秒間隔でタスク状態をチェック)
         └─ いずれかのチャネルが異常終了 → 全チャネルを停止
```

### 停止

```text
1. Ctrl-C シグナル受信 / チャネル異常終了
      │
2. 全チャネルタスクに中止シグナル送信
      │
3. 各チャネルの graceful shutdown (最大 10 秒)
   ├─ Discord:  shard_manager.shutdown_all()
   ├─ Telegram: dispatcher 停止
   └─ Web:      axum graceful shutdown
      │
4. タイムアウト時はタスクを abort
      │
5. プロセス終了
```

---

## 7. 設計パターン

| パターン | 適用箇所 | 目的 |
|---------|---------|------|
| **Channel Adapter** | `channels/adapter.rs` | 全チャネルを統一インターフェースで扱う |
| **Dependency Injection** | `runtime/` AppState | 全コンポーネントの依存を明示的に注入 |
| **Optimistic Concurrency** | `storage/` sessions | セッション書き込みの競合を `updated_at` で解決 |
| **Tool Registry** | `tools/mod.rs` | built-in / MCP の区別なくツールを動的登録 |
| **Feature Flag** | `Cargo.toml` | Discord / Telegram をオプショナルに |
| **Graceful Shutdown** | `runtime/` | 10 秒タイムアウト付きで全チャネルを安全停止 |
| **LLM Provider Cache** | `runtime/` AppState | 同一 ResolvedLlmConfig の LLM クライアントを再利用 |
| **Codex Auth Cache** | `llm/codex_auth.rs` | 5 分 TTL で codex auth 解決結果をキャッシュ |
| **Read-only Parallel** | `agent_loop/turn.rs` | `is_read_only()` が真のツールは並列実行 |
| **Sleep Batch** | `sleep_batch.rs` | 手動 sleep batch の排他実行と監査記録 |
| **Sleep Scheduler** | `sleep_scheduler.rs` | 自動 scheduler による定期 sleep batch 実行 |
| **Active Turn Tracker** | `runtime/mod.rs` | agent ごとのアクティブ turn 追跡（scheduler defer 用） |

### Sleep Batch（手動長期記憶処理）

`egopulse sleep --agent <AGENT>` で手動実行する長期記憶のバッチ処理。

#### アーキテクチャ

Sleep Batch は **1 回の LLM 呼び出し** で Pruning・Consolidation・Compression を一括実行する。複数回の LLM 呼び出しや段階的パイプラインは使用しない。

```text
LLM への入力
    ├ 現在の記憶ファイル（episodic / semantic / prospective）
    └ ソースセッションのメッセージ履歴
         │
    ┌────▼────┐
    │ 1-call  │  Pruning + Consolidation + Compression を1回で実行
    │   LLM   │
    └────┬────┘
         │
    JSON 出力（3キー固定）
    ├ episodic:     更新後のエピソード記憶（Markdown）
    ├ semantic:     更新後の意味記憶（Markdown）
    └ prospective: 更新後の展望記憶（Markdown）
```

LLM は厳密に `episodic`・`semantic`・`prospective` の 3 キーのみを持つ JSON オブジェクトを返す必要がある。`summary_md`・`phases`・`summary` などの追加キーはパーサーで拒否される。

#### 記憶ファイルの原子的書き込み

記憶ファイルの書き込みは backup-and-rename 戦略で原子性を保証する:

1. 一時ディレクトリ `memory.tmp-{uuid}` に全ファイルを書き出し
2. 既存 `memory` ディレクトリを `memory.backup-{uuid}` にリネーム
3. `memory.tmp-{uuid}` を `memory` にリネーム
4. 成功時、`memory.backup-{uuid}` を削除
5. ステップ 3 で失敗した場合、バックアップから復元

前回失敗時の残存ディレクトリは次回実行時に `recover_memory_write()` が自動クリーンアップする。

#### Sleep Batch 固有の LLM 設定

Sleep Batch のプロバイダーとモデルは、デフォルト設定から独立して設定可能。詳細は [config.md](./config.md) の `sleep_batch` セクションを参照。

```text
sleep_batch.provider → 指定時はそのプロバイダー、未指定時は default_provider
sleep_batch.model    → 指定時はそのモデル、未指定時は default_model → provider.default_model
```

#### 実行フロー

```text
1. agent_id 解決（--agent 省略時は default_agent）
       │
2. collect_sleep_input()
       │
       ├─ Skip: 新規メッセージ ≤ 4 → ログ出力して終了（run レコードなし）
       │
       └─ Proceed: ソースセッション一覧を取得
              │
       3. try_create_sleep_run() で排他チェック + run 作成
              │
              ├─ 既に running → AlreadyRunning エラー
              │
              └─ 未実行 → running run を作成
                     │
              4. build_sleep_input() でメモリ + セッションデータを構築
                     │
              5. aggregate snapshot（before）を保存
                     │
              6. build_sleep_system_prompt() でシステムプロンプト構築
                     │
              7. LLM 呼び出し → JSON パース（失敗時 1回リトライ）
                     │
              8. write_memory_files() でメモリファイル書き込み
                     │
              9. 対象セッションのアーカイブ + messages_json クリア
                     │
             10. aggregate snapshot（after）を保存
                     │
             11. update_sleep_run_success() で run を完了
```

ステップ 9 では、処理対象セッションの `messages_json` を Markdown としてアーカイブ（Compaction と同じ形式）した後、`"[]"` に更新する。これにより次ターン開始時に LLM コンテキストが空（= 長期記憶のみ）でスタートする。`messages` レコードと `tool_calls` レコードは保持される。

監査スキーマは1回 LLM 呼び出し前提に整理されており、`phases_json` / `summary_md` / `memory_snapshots.phase` は持たない。

### Sleep Scheduler（自動定期実行）

`sleep_batch.enabled: true` 時に、設定時刻に自動で sleep batch を実行する scheduler。

#### 動作概要

1. `start_channels` 起動時、scheduler enabled なら scheduler task を spawn する
2. scheduler は `next_scheduled_run()` で次回実行時刻を計算し、`tokio::time::sleep` で待機
3. 時刻到達時に `run_scheduled_cycle()` を実行
4. 各 agent について `active_turns.is_active()` を確認し、アクティブなら defer
5. `run_agent_with_retry()` でリトライ設定に基づき再試行

#### Active Turn Tracking

`ActiveTurnTracker` は agent ごとに現在の対話 turn 数を管理する。scheduler は active な agent の sleep batch を defer し、ユーザーとの対話が終了してから実行する。

#### Scheduler と channel の関係

- scheduler 単独では runtime active condition を満たさない（channel が0個なら `NoActiveChannels` エラー）
- Ctrl-C / channel failure 時に scheduler も既存 task shutdown 経路で停止する
