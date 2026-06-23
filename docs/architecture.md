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
8. [オブザーバビリティレイヤー](#8-オブザーバビリティレイヤー)

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
| **ドメインサービス** | LLM 抽象化、ツール、セッション管理、SOUL/AGENTS 読み込み | `llm/`, `tools/`, `agent_loop/session.rs`, `agent_loop/soul_agents.rs` |
| **インフラ** | 永続化、設定、セキュリティ | `storage/`, `config/`, `tools/mcp.rs`, `skills.rs` |

---

## 3. モジュール構成

```
src/
├── main.rs              # CLI エントリポイント
├── lib.rs               # 全モジュールの公開インターフェース
├── assets.rs            # 埋め込みアセット（Web UI 用静的ファイル）
│
├── runtime/             # AppState 構築、チャネル起動・監視
│   ├── mod.rs           # AppState, build_app_state(), start_channels()
│   ├── turn_scheduler.rs # TurnScheduler, TurnTracker, StopReason, evaluate_stop_conditions
│   ├── gateway.rs       # systemd サービス管理
│   ├── logging.rs       # ログ初期化
│   ├── metrics.rs       # メトリクス初期化・ヘルパー（内部 Prometheus レコーダー）
│   ├── runtime_status.rs # RuntimeStatus (インメモリヘルスサマリー)
│   └── status.rs        # MCP ステータス型
│
├── agent_loop/          # エージェントループ
│   ├── mod.rs           # SurfaceContext, process_turn()
│   ├── turn.rs          # LLM 呼び出し、ツール実行、compaction
│   ├── session.rs       # セッションロード・保存、競合解決
│   ├── prompt_builder.rs # システムプロンプト構築
│   ├── compaction.rs    # コンテキスト圧縮 + アーカイブ
│   ├── formatting.rs    # 出力フォーマット
│   ├── guards.rs        # 各種チェック
│   └── soul_agents.rs   # SOUL.md / AGENTS.md 読み込み
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
├── setup/               # 初回セットアップウィザード
│
├── memory.rs            # 長期記憶ファイルの読み込み（episodic/semantic/prospective）
├── skills.rs            # スキル管理 (発見・読み込み・カタログ生成)
├── slash_commands.rs    # slash command dispatcher、LLM プロファイル管理
├── sleep/               # sleep batch 実行・scheduler
│   ├── batch.rs         # 手動 sleep batch の排他実行と監査記録
│   ├── scheduler.rs     # 自動 scheduler による定期 sleep batch 実行
│   └── prompt.md        # sleep batch 用プロンプト本文
├── error.rs             # エラー型
├── test_env.rs          # テスト用 EnvVarGuard、ENV_MUTEX
└── test_util.rs         # テストユーティリティ
```

---

## 4. コア型

### AppState

すべてのチャネルとエージェントループが共有する状態。

```rust
pub struct AppState {
    pub(crate) db: Arc<Database>,
    pub(crate) secret_db: Option<Arc<Database>>,  // None = 秘密モード無効
    pub(crate) config: Config,
    pub(crate) config_path: Option<PathBuf>,
    pub(crate) llm_override: Option<Arc<dyn LlmProvider>>,
    pub(crate) channels: Arc<ChannelRegistry>,
    pub(crate) skills: Arc<SkillManager>,
    pub(crate) tools: Arc<ToolRegistry>,
    pub(crate) mcp_manager: Option<Arc<RwLock<McpManager>>>,
    pub(crate) assets: Arc<AssetStore>,
    pub(crate) soul_agents: Arc<SoulAgentsLoader>,
    pub(crate) memory_loader: Arc<MemoryLoader>,
    pub(crate) llm_cache: Mutex<HashMap<u64, Arc<dyn LlmProvider>>>,
    pub(crate) active_turns: Arc<ActiveTurnTracker>,
    pub(crate) turn_sender: mpsc::Sender<PendingAgentTurn>,
    pub(crate) turn_scheduler: Arc<TurnScheduler>,
    pub(crate) turn_tracker: Arc<TurnTracker>,
    pub(crate) runtime_status: Arc<RuntimeStatus>,  // インメモリヘルスサマリー
}

impl AppState {
    /// 秘密モードが有効（設定に secret チャネルが1件でもある）
    pub(crate) fn secret_enabled(&self) -> bool {
        self.secret_db.is_some()
    }

    /// 文脈に応じた DB 参照を返す
    pub(crate) fn db_for(&self, is_secret: bool) -> &Database {
        if is_secret {
            self.secret_db.as_ref().expect("secret db required but not initialized")
        } else {
            &self.db
        }
    }
}
```

### SurfaceContext

メッセージの送信元を識別する型。チャネル・ユーザー・スレッドの組み合わせで一意になる。

```rust
pub(crate) struct SurfaceContext {
    pub channel: String,         // "discord" | "telegram" | "web" | "tui" | "cli"
    pub surface_user: String,    // プラットフォーム固有のユーザー ID
    pub surface_thread: String,  // プラットフォームの会話スレッド ID
    pub chat_type: String,       // 永続化用チャット種別
    pub agent_id: String,        // 使用するエージェント定義のキー
    pub channel_log_chat_id: Option<i64>, // Multi-Agent Room の Channel Log
    pub chain_depth: usize,      // agent_send のチェーン深度 (0 = ユーザー発信)
    pub origin_id: String,       // ヒューマン入力起点の UUID (暴走防止用)
    pub trace_id: String,        // オブザーバビリティ用トレース ID (ターン相関)
    pub is_secret: bool,         // 秘密モードフラグ。true のとき全 DB 操作が secret.db にルーティングされる
}
```

`channel` フィールドはモデル解決の profile lookup キーとしても機能する。`resolve_llm_for_agent_channel` は `agent.profiles[channel]` を参照し、チャネル別のプロバイダー/モデルオーバーライドを解決する（詳細は [config.md §3](./config.md#3-モデル解決チェーン)）。

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
       ├─ secret_db 初期化 (Config::needs_secret_db() が true の場合のみ)
       ├─ SkillManager 構築
      ├─ McpManager 初期化 (MCP server 接続)
      ├─ ToolRegistry 構築 (built-in + MCP adapters)
      ├─ ChannelAdapter 登録
      └─ SOUL.md プロビジョニング
      │
4. start_channels()
       │
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
| **Sleep Batch** | `sleep/batch.rs` | 手動 sleep batch の排他実行と長期記憶昇格 |
| **Sleep Scheduler** | `sleep/scheduler.rs` | 自動 scheduler による定期 sleep batch 実行 |
| **Active Turn Tracker** | `runtime/mod.rs` | agent ごとのアクティブ turn 追跡（scheduler defer 用） |
| **Turn Scheduler** | `runtime/turn_scheduler.rs` | per-session busy flag + input queue による同時実行制御 |
| **Stop Condition Evaluator** | `runtime/turn_scheduler.rs` | chain depth / turn count / agent 存在確認による暴走防止 |
| **Turn Tracker** | `runtime/turn_scheduler.rs` | origin_id 単位の turn 数カウント |
| **Secret DB Routing** | `runtime/` AppState | `db_for(is_secret)` で通常 DB と秘密 DB を切替。`SurfaceContext.is_secret` でルーティング判定 |

---

## 8. オブザーバビリティレイヤー

3 層構造で運用時の可観測性を提供する。

### 8.1 3 層モデル

| 層 | 形式 | 用途 |
|---|---|---|
| **構造化ログ** | `tracing` スパン + `trace_id` | リクエスト単位のログ追跡、`journalctl` / Loki での検索 |
| **Live Health API** | `/health` | ヘルスプローブ、オペレーション確認 |
| **テレメトリー API** | `/telemetry` | JSON メトリクス・ターン履歴・エラー詳細（AI エージェント向け） |

### 8.2 RuntimeStatus

`AppState` 上に保持されるインメモリのヘルスサマリー。各チャネル・MCP・DB の状態を集約し、`/health` エンドポイントの応答に使用される。プロセス起動時に初期化され、チャネルの起動・停止・MCP 接続状態の変化に応じてリアルタイムに更新される。

### 8.3 trace_id 伝播

エージェントターンのライフサイクル全体で `trace_id` が伝播する。

1. `execute_scheduled_turn` で UUID v4 を生成し `SurfaceContext.trace_id` に設定
2. `process_turn_inner` は空 `trace_id` を自動補完（UUID v4 を再生成）
3. `tracing::info_span!` に `trace_id` フィールドとして注入
4. `journalctl` などで `trace_id=<value>` を grep することで、特定ターンの全ログを抽出できる

### 8.4 エラーリングバッファ

直近のエラーをインメモリのリングバッファ（容量 100 件）に保持する。`/telemetry` エンドポイントの `recent_errors` フィールドから `trace_id` 付きで参照可能。プロセス再起動で消失するため、永続的なエラー追跡には外部ログ収集基盤（Loki 等）と組み合わせる必要がある。

### 8.5 ターン履歴リングバッファ

直近のターン実行結果をインメモリのリングバッファ（容量 100 件）に保持する。`/telemetry` エンドポイントの `recent_turns` フィールドから参照可能。各レコードには `trace_id`、`agent_id`、`channel`、`started_at`、`duration_secs`、`ok` が含まれる。

### 8.6 メトリクス

`/telemetry` エンドポイントは JSON 形式でメトリクスを出力する。`egopulse_` プレフィックスのカウンター・ゲージをラベル付きで返す。

主要メトリクス:

| メトリクス | 型 | 説明 |
|---|---|---|
| `egopulse_turns_total` | counter | 処理済みターン総数（ラベル: `agent`, `channel`） |
| `egopulse_turn_errors_total` | counter | ターンエラー総数（ラベル: `kind`, `agent`） |
| `egopulse_llm_tokens_total` | counter | LLM トークン消費量（ラベル: `direction`, `provider`） |
| `egopulse_tool_calls_total` | counter | ツール呼び出し総数（ラベル: `tool`, `status`） |
| `egopulse_active_turns` | gauge | 実行中のエージェントターン数 |
