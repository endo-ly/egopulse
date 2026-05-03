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
             │    │              runtime.rs                   │
             │    │          (AppState 構築・管理)              │
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
| **ランタイム** | 依存注入、チャネル起動、ライフサイクル管理 | `runtime.rs` |
| **チャネル** | 外部プラットフォームとの通信 | `channels/`, `web/`, `channel_adapter.rs` |
| **エージェントループ** | 会話ターン処理、LLM 呼び出し、ツール実行 | `agent_loop/` |
| **ドメインサービス** | LLM 抽象化、ツール、セッション管理 | `llm/`, `tools/`, `agent_loop/session.rs` |
| **インフラ** | 永続化、設定、セキュリティ | `storage.rs`, `config/`, `mcp.rs`, `skills.rs` |

---

## 3. モジュール構成

```
src/
├── main.rs              # CLI エントリポイント
├── lib.rs               # 全モジュールの公開インターフェース
├── runtime.rs           # AppState 構築、チャネル起動・監視
│
├── agent_loop/          # エージェントループ
│   ├── mod.rs           # SurfaceContext, process_turn()
│   ├── turn.rs          # LLM 呼び出し、ツール実行、compaction
│   └── session.rs       # セッションロード・保存、競合解決
│
├── channels/            # チャネル実装
│   └── ...              # discord.rs, telegram.rs, tui.rs, cli.rs
│
├── web/                 # Web サーバー
│   ├── mod.rs           # Axum ルーター、WebAdapter
│   ├── auth.rs          # Bearer 認証
│   ├── stream.rs        # HTTP API + SSE ストリーミング
│   ├── ws.rs            # WebSocket ゲートウェイ
│   ├── sse.rs           # SSE イベント型
│   ├── sessions.rs      # セッション管理 API
│   ├── config.rs        # 設定取得・更新 API
│   └── health.rs        # ヘルスチェック
│
├── llm/                 # LLM プロバイダー抽象化
│   └── mod.rs           # LlmProvider trait, OpenAI 互換クライアント
│
├── config/              # 設定管理
│   ├── mod.rs           # 型定義、公開ファサード
│   ├── loader.rs        # YAML 読み込み、正規化、検証
│   ├── persist.rs       # YAML 書き出し、アトミック書込
│   ├── resolve.rs       # モデル解決、チャネルアクセサ
│   └── secret_ref.rs    # SecretRef 型、.env 読み書き
│
├── tools/               # ツールシステム
│   ├── mod.rs           # ToolRegistry, Tool trait, is_read_only()
│   ├── mcp_adapter.rs   # MCP tool → Tool trait アダプター
│   ├── command_guard.rs # bash コマンド検閲
│   ├── path_guard.rs    # 機密パスブロック
│   └── sanitizer.rs     # 出力リダクション
│
├── storage.rs           # SQLite 永続化
├── mcp.rs               # MCP クライアント
├── codex_auth.rs        # Codex auth 解決、AUTH_CACHE
├── channel_adapter.rs   # ChannelAdapter trait, ChannelRegistry
├── skills.rs            # スキル管理
├── slash_commands.rs    # slash command dispatcher、SlashCommandOutcome
├── soul_agents.rs       # SOUL.md / AGENTS.md 読み込み
├── error.rs             # エラー型
├── gateway.rs           # systemd サービス管理
├── status.rs            # ランタイムステータス
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
      ├─ 3c. compaction 判定 (messages.len > max_session_messages)
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
| **Channel Adapter** | `channel_adapter.rs` | 全チャネルを統一インターフェースで扱う |
| **Dependency Injection** | `runtime.rs` AppState | 全コンポーネントの依存を明示的に注入 |
| **Optimistic Concurrency** | `storage.rs` sessions | セッション書き込みの競合を `updated_at` で解決 |
| **Tool Registry** | `tools/mod.rs` | built-in / MCP の区別なくツールを動的登録 |
| **Feature Flag** | `Cargo.toml` | Discord / Telegram をオプショナルに |
| **Graceful Shutdown** | `runtime.rs` | 10 秒タイムアウト付きで全チャネルを安全停止 |
| **LLM Provider Cache** | `runtime.rs` AppState | 同一 ResolvedLlmConfig の LLM クライアントを再利用 |
| **Codex Auth Cache** | `codex_auth.rs` | 5 分 TTL で codex auth 解決結果をキャッシュ |
| **Read-only Parallel** | `agent_loop/turn.rs` | `is_read_only()` が真のツールは並列実行 |