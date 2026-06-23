# Secret Mode 設計仕様書（Phase 1）

秘匿性の高い会話を agent の通常長期記憶から物理的に隔離する「秘密モード」の Phase 1 設計仕様。

- **Date**: 2026-06-22
- **Status**: Design（実装計画前）
- **Scope**: Phase 1 のみ。Phase 2 以降は別途計画

---

## 目次

1. [背景・目的](#1-背景目的)
2. [設計方針](#2-設計方針)
3. [アーキテクチャ概観](#3-アーキテクチャ概観)
4. [データモデル](#4-データモデル)
5. [ファイル・ディレクトリ構成](#5-ファイルディレクトリ構成)
6. [コンポーネント改修](#6-コンポーネント改修)
7. [ライフサイクル](#7-ライフサイクル)
8. [設定リファレンス](#8-設定リファレンス)
9. [隔離保証一覧](#9-隔離保証一覧)
10. [Phase 1 スコープ外](#10-phase-1-スコープ外)
11. [テスト戦略](#11-テスト戦略)
12. [今後の拡張（Phase 2 以降）](#12-今後の拡張phase-2-以降)

---

## 1. 背景・目的

### 1.1 動機

EgoPulse は会話履歴を `messages` テーブルに格納し、Sleep Batch がこれを `episode_events` / `episodic.md` / `semantic.md` / `prospective.md` へ蒸留する。PULSE はこれら長期記憶を参照して Discord 等のチャネルへ発言する。

この仕組みでは、一度通常チャットで発言された内容は**自動的に長期記憶へ昇格し、エージェントの人格の一部となる**。秘匿性の高い会話がこの経路に乗ると:

- Sleep Batch が秘匿内容を `episodic.md` 等へ書き込み、通常チャットでの回想に混入
- PULSE が秘匿内容由来の記憶を元に Discord 等の公開チャネルへ発言
- WebUI のセッション一覧・メッセージ履歴に秘匿内容が表示される
- バックアップファイルに秘匿内容が残り、クラウド同期等で意図せず流出

### 1.2 目的

秘匿会話を通常長期記憶経路から**物理的に隔離**する。一方で、エージェントは通常の人格・記憶を保ったまま「秘密モード」に入り、ユーザーと同じ空間で振る舞えるようにする。

### 1.3 Phase 1 の目標

「**機能する秘密モード**」を最小スコープでリリースする。具体的には:

- 永続化層の完全隔離（別 DB ファイル）
- 通常記憶の汚染防止（Sleep / PULSE は秘密を認識しない）
- ユーザー編集可能な `SECRET.md` による挙動制御
- Discord / Telegram からの秘密モード侵入
- Phase 2 以降の拡張（`secret_episodic.md`、Web/TUI 表示等）を阻害しない構造

---

## 2. 設計方針

### 2.1 基本方針

| 方針 | 概要 |
|---|---|
| **Approach C（ハイブリッド）** | コンテンツは別テーブルではなく別 DB ファイル、秘密メモリ層は Phase 2 で追加 |
| **非対称アクセス** | 通常メモリ → 通常モード＋秘密モード両方で注入／秘密由来データ → 秘密モード専用 |
| **Defense in Depth** | 単一の隔離機構に頼らず、データ・LLM Context・PULSE・Sleep・Tools・ログの各層で独立に排除 |
| **ユーザー制御** | 秘密モード時の挙動はバイナリ埋め込みではなく `SECRET.md` で完全制御 |

### 2.2 隔離戦略

秘匿内容の流出経路と対策を多層化する:

| 経路 | リスク | 対策 |
|---|---|---|
| DB クエリ | 通常クエリが秘密テーブルを参照 | **物理的に別 DB ファイル**（`secret.db`） |
| Sleep Batch 入力 | 秘密メッセージが `episode_events` へ昇格 | Sleep Batch は `secret.db` にアクセスしない（構造的保証） |
| PULSE 発火 | 秘密記憶が公開チャネルへ投稿 | PULSE は秘密チャットで発火しない |
| LLM Context | 通常 session に秘密内容が混入 | `db_for(is_secret)` で DB を切替 |
| Tool 実行 | `write`/`edit`/`bash` で `secret.db` 外に秘匿内容が書き出される | **Phase 1 では対処なし**（§6.9 参照）。Phase 2 でワークスペース概念導入時に再検討 |
| Multi-Agent 通信 | クロスモード `agent_send` | secret state はチャット属性のため構造的に発生しない |
| バックアップ | バックアップファイル経由の漏洩 | ファイルパーミッション `0600`、ユーザー運用（バックアップファイルの取扱い注意） |
| ログ | `tracing` に秘匿内容が出力 | 秘密ターンは内容フィールドを span に含めない |

---

## 3. アーキテクチャ概観

### 3.1 全体像

```text
                          ┌─────────────────────────────────┐
                          │           AppState              │
                          │  ┌────────────┐ ┌────────────┐ │
   チャネル群 ───────────▶│  │ db (通常)  │ │ secret_db  │ │
   (Discord/Telegram)     │  │ egopulse.db│ │ secret.db  │ │
                          │  └────────────┘ └────────────┘ │
                          │  db_for(is_secret) で切替       │
                          └─────────────────────────────────┘
                                          │
                          ┌───────────────┴───────────────┐
                          │                               │
                          ▼                               ▼
              ┌────────────────────┐         ┌────────────────────┐
              │  通常フロー         │         │  秘密フロー         │
              │  - chats           │         │  - secret_chats    │
              │  - messages        │         │  - secret_messages │
              │  - sessions        │         │  - secret_sessions │
              │  - Sleep Batch     │         │  - (Sleep なし)    │
              │  - PULSE ◯        │         │  - PULSE ✗        │
              └────────────────────┘         └────────────────────┘
                          │                               │
                          └───────────────┬───────────────┘
                                          │
                                          ▼
                              ┌────────────────────────┐
                              │   agent_loop            │
                              │   process_turn()        │
                              │   - 通常 memory 3層注入 │
                              │   - SECRET.md 追加注入  │
                              │     (秘密時のみ)        │
                              └────────────────────────┘
```

### 3.2 AppState 構造

```rust
pub struct AppState {
    pub(crate) db: Arc<Database>,
    pub(crate) secret_db: Option<Arc<Database>>,  // None で秘密機能無効
    // ... 既存フィールド ...
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

`secret_db` は起動時に以下のいずれかが真なら初期化:

- `channels.discord.channels.*` に `secret: true` エントリが1件でもある
- `channels.telegram.telegram_channels.*` に `secret: true` エントリが1件でもある

### 3.3 SurfaceContext 拡張

```rust
pub(crate) struct SurfaceContext {
    // ... 既存フィールド ...
    pub is_secret: bool,  // 新設
}
```

`is_secret` は各チャネルのエントリポイントで決定され、全下流のルーティングに使われる:

| チャネル | `is_secret` の決定方法 |
|---|---|
| Discord | メッセージ受信チャネルの config `secret` フラグ値 |
| Telegram | メッセージ受信チャットの config `secret` フラグ値 |
| Web / TUI | Phase 1 では秘密モード未対応（常に `false`） |
| `agent_send` 起点の turn | 送信元 turn の `is_secret` を継承 |

---

## 4. データモデル

### 4.1 ファイル構成

```text
~/.egopulse/runtime/
├── egopulse.db           # 既存・通常専用。一切変更なし
├── egopulse.db-wal
├── egopulse.db-shm
├── secret.db             # 新設・秘密専用。パーミッション 0600
├── secret.db-wal
└── secret.db-shm
```

両者は独立の `Database` インスタンスとして `Mutex<Connection>` を個別に持つ。クロスデータベーストランザクションは不要（1 turn は通常/秘密いずれか一方で完結）。

### 4.2 `secret.db` テーブル定義

Phase 1 では以下の 6 テーブルのみ:

#### `chats`

`egopulse.db.chats` と同 schema。`chat_type` は通常チャットと同一（チャネル種別 `discord` / `telegram` 等をそのまま格納）で、**`secret` のような特殊値は使わない**。秘密状態の区別は「どちらの DB ファイルに格納されているか」で表現される。

```sql
CREATE TABLE IF NOT EXISTS chats (
    chat_id INTEGER PRIMARY KEY,
    chat_title TEXT,
    chat_type TEXT NOT NULL DEFAULT 'private',
    last_message_time TEXT NOT NULL,
    channel TEXT,
    external_chat_id TEXT,
    agent_id TEXT NOT NULL DEFAULT 'default'
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_chats_channel_external_chat_id
    ON chats(channel, external_chat_id);
```

#### `messages`

`egopulse.db.messages` と同 schema。

```sql
CREATE TABLE IF NOT EXISTS messages (
    id TEXT NOT NULL,
    chat_id INTEGER NOT NULL,
    sender_id TEXT NOT NULL,
    content TEXT NOT NULL,
    sender_kind TEXT NOT NULL DEFAULT 'user',
    timestamp TEXT NOT NULL,
    message_kind TEXT NOT NULL DEFAULT 'message',
    recipient_agent_id TEXT,
    PRIMARY KEY (id, chat_id)
);

CREATE INDEX IF NOT EXISTS idx_messages_chat_timestamp
    ON messages(chat_id, timestamp);
```

#### `sessions`

`egopulse.db.sessions` と同 schema。LLM context snapshot を保持。tool call block も `messages_json` 内に包含される。

```sql
CREATE TABLE IF NOT EXISTS sessions (
    chat_id INTEGER PRIMARY KEY,
    messages_json TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
```

**`tool_calls` テーブルは `secret.db` には作らない**。現行ランタイムは `tool_calls` テーブルへ別途永続化しているが（`src/agent_loop/turn.rs` の `store_pending_tool_call` / `update_tool_call_output`）、秘密モードではこの永続化をスキップする。tool call block は `sessions.messages_json` にも包含されているため LLM context 復元には影響しない。デバッグ用途でも `messages_json` を parse すれば復元可能。保管データ量（＝漏洩面積）を減らすために永続化を止める。

#### `llm_usage_logs`

`egopulse.db.llm_usage_logs` と同 schema。`is_secret` カラムは持たない（当 DB 内はすべて秘密扱い）。

```sql
CREATE TABLE IF NOT EXISTS llm_usage_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    chat_id INTEGER NOT NULL,
    caller_channel TEXT NOT NULL,
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    input_tokens INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    total_tokens INTEGER NOT NULL,
    request_kind TEXT NOT NULL DEFAULT 'agent_loop',
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_llm_usage_chat_created
    ON llm_usage_logs(chat_id, created_at);

CREATE INDEX IF NOT EXISTS idx_llm_usage_created
    ON llm_usage_logs(created_at);
```

#### `db_meta` / `schema_migrations`

`egopulse.db` と同 schema。`secret.db` は独自のスキーマバージョン管理を持つ（独立した `SECRET_SCHEMA_VERSION` 定数、Phase 1 では `1`）。

### 4.3 マイグレーション

既存の `run_migrations()` とは別に `run_secret_migrations()` を新設。`Database::new_secret()` 経由で起動時に呼ばれる。

```rust
pub(super) const SECRET_SCHEMA_VERSION: i64 = 1;

pub(super) fn run_secret_migrations(conn: &Connection) -> Result<(), StorageError> {
    let mut version = schema_version(conn)?;
    if version < 1 {
        // chats, messages, sessions, llm_usage_logs, db_meta, schema_migrations を作成
        conn.execute_batch(/* DDL */)?;
        set_schema_version(conn, 1, "initial secret schema")?;
        version = 1;
    }
    debug_assert_eq!(version, SECRET_SCHEMA_VERSION);
    Ok(())
}
```

`egopulse.db` 側の `SCHEMA_VERSION` と衝突しないよう、別定数・別関数で管理する。

---

## 5. ファイル・ディレクトリ構成

### 5.1 Agent ディレクトリ

```text
~/.egopulse/agents/<agent_id>/
├── SOUL.md               # 既存・両モードで注入
├── AGENTS.md             # 既存・両モードで注入
├── SECRET.md             # 新設・秘密モード時のみ注入
├── memory/               # 既存・両モードで注入
│   ├── episodic.md
│   ├── semantic.md
│   └── prospective.md
└── (secret_memory/       # Phase 2 で新設)
```

### 5.2 `SECRET.md`

**ユーザー編集可能な Markdown ファイル**。秘密モード時のみ system prompt へ追加注入される。バイナリ埋め込みの固定文字列は持たない。

- **フォーマット**: Markdown 自由形式。AGENTS.md / SOUL.md と同形式
- **ロード**: `SoulAgentsLoader` の拡張。`SurfaceContext.is_secret == true` 時に `build_system_prompt()` で AGENTS.md の後に追加
- **内容**: ユーザー完全自由。例:
  - 秘密モード用ペルソナ指示（「より親密なトーンで」等）
  - 秘密モード認識の指示（「ここは秘密の空間、通常話題への言及は避ける」等）
  - ロールプレイ設定、シナリオ、キャラクター設定 等
- **ファイル不存在時**: 空文字列として扱う（エラーにしない）。つまり `SECRET.md` 無しでも秘密モードは動く

---

## 6. コンポーネント改修

### 6.1 Storage / Database

- `Database::new_secret(path)` コンストラクタ新設。`run_secret_migrations()` を呼ぶ
- 既存クエリ関数（`store_message` / `load_session` / `save_session` / `log_llm_usage` 等）は **`&self` on Database** のまま変更なし。どの DB に対して実行されるかは呼出側が `db_for(is_secret)` で決める
- `secret.db` に Channel Log 用の chat 解決関数（`resolve_channel_log_chat_id` 相当）も必要。Multi-Agent Room の secret チャネル向け

### 6.2 Config

#### Discord

`channels.discord.channels.<channel_id>` の値オブジェクトに `secret: bool` フィールドを追加。デフォルト `false`。

```yaml
channels:
  discord:
    channels:
      "1234567890123456789":
        require_mention: true
        secret: true              # 新設
      "9876547890123456789":
        multi_agent: true
```

#### Telegram

`channels.telegram.telegram_channels.<chat_id>` の値オブジェクトに `secret: bool` フィールドを追加。デフォルト `false`。

```yaml
channels:
  telegram:
    telegram_channels:
      "-1001234567890":
        secret: true              # 新設
      "-1009876543210":
        require_mention: true
```

#### CLI

Phase 1 では対象外。

#### DB Backup

`secret.db` は `egopulse.db` と同一スケジュールでバックアップされる。ファイル名は `secret-YYYYMMDD-HHMMSS.db`。世代管理も `egopulse.db` 系と独立して `max_generations` を適用。

### 6.3 Agent Loop

`process_turn()` 内の **全ての DB アクセス** を `state.db_for(ctx.is_secret)` 経由に切替。漏れがあると秘密情報が `egopulse.db` に残留する:

| DB アクセス箇所 | ファイル | ルーティング方針 |
|---|---|---|
| chat_id 解決（`resolve_or_create_chat_id`） | `agent_loop/turn.rs` | `db_for(is_secret)` |
| session snapshot 読込（`load_session` / `load_session_snapshot`） | `agent_loop/session.rs` | `db_for(is_secret)` |
| message 保存（`store_message` / `store_message_with_session`） | `agent_loop/turn.rs` | `db_for(is_secret)` |
| session snapshot 保存（`save_session`） | `agent_loop/session.rs` | `db_for(is_secret)` |
| LLM usage log（`log_llm_usage`） | `agent_loop/tool_phase.rs` | `db_for(is_secret)` |
| **Compaction 中の LLM usage log 挿入** | `agent_loop/compaction.rs` | `db_for(is_secret)`（`tool_phase.rs` とは別経路のため独立対応） |
| **tool_call 永続化**（`store_pending_tool_call` / `update_tool_call_output`） | `agent_loop/turn.rs` | **`is_secret == true` のときスキップ**（`secret.db` に `tool_calls` テーブル無し） |
| **slash command handlers**（`handle_new` / `handle_compact` / `handle_status` 等） | `slash_commands.rs` | `db_for(context.is_secret)` |
| **Multi-Agent Room 停止条件時の `store_system_event`** | `runtime/mod.rs::execute_scheduled_turn` | `db_for(turn.context.is_secret)` |

**tool_call 永続化スキップ**: `store_pending_tool_call` / `update_tool_call_output` は `if !ctx.is_secret { ... }` で囲み、秘密モード時は INSERT を呼ばない。`tool_calls` テーブルは `secret.db` に存在しないため、呼ぶとエラーになる。LLM context には `sessions.messages_json` 内の tool call block で十分。

**slash command handlers**: Discord / Telegram で `/new` / `/compact` / `/status` 等が実行されたとき、`process_slash_command` から呼ばれる各 handler は現状 `state.db` を直接参照する。これを `state.db_for(context.is_secret)` 経由に切替える。`process_slash_command` は `SurfaceContext` を受け取る現状を活かし、内部で `context.is_secret` を参照して各 handler へ伝播。

compaction アーカイブファイルの出力先も分離:

- 通常: `<state_root>/runtime/groups/<channel>/<chat_id>/conversations/`
- 秘密: `<state_root>/runtime/secret_groups/<channel>/<chat_id>/conversations/`

**理由**: `runtime/groups/` 配下はデバッグ・監査用の artifact で、トラブルシュート時に開発者と共有されることが想定される。このディレクトリに秘匿内容のアーカイブが混入していると、誤送信リスクが高まる。`secret_groups/` とディレクトリを分けることで、「`runtime/groups/` は安全に共有できる」「`secret_groups/` はユーザー運用責任」という境界を明確にする。

**却下した代替案**:
- *同一ディレクトリ・ファイル名 prefix*（`secret-<timestamp>.md`）: `runtime/groups/` を一括 tar 圧縮して送るような運用で漏洩するため却下
- *アーカイブを暗号化*: Phase 1 ではオーバースペック

### 6.4 Channels

#### Discord

メッセージ受信時に、そのチャネル ID が `secret: true` なら `SurfaceContext.is_secret = true` を立てる。**`make_context()` ヘルパーを修正**することで、メインメッセージハンドラ・slash command 処理（`process_text_slash_command`）・Discord interaction ハンドラなど、`make_context` 経由で `SurfaceContext` を構築する全経路に自動伝播。**Channel Log 保存（`store_human_channel_log_message`）も同一チャネルの `secret` フラグに従い、`state.db_for(is_secret)` 経由で DB を切替**:

- `secret: true` チャネルの Channel Log は `secret.db` 側の `chats`（`channel_log` type）+ `messages` へ保存
- `resolve_channel_log_chat_id` の呼出も `db_for(is_secret)` 経由に切替

#### Telegram

メッセージ受信時に、その chat ID が `secret: true` なら `SurfaceContext.is_secret = true` を立てる。**`make_context()` を Discord と同様に修正**し slash command 経路にも自動伝播。**Channel Log 保存も `db_for(is_secret)` 経由で DB を切替**。

#### Web / TUI

Phase 1 では対応しない。「New Secret Chat」ボタン等の UI は追加しない。secret チャットが存在しても一覧に表示しない。

### 6.5 Memory Loader

`MemoryLoader` は変更なし。既存の3層（`episodic.md` / `semantic.md` / `prospective.md`）を両モードで読み込む。

Phase 1 では `secret_memory/` ディレクトリや `secret_episodic.md` は存在しない。これらは Phase 2 で追加。

### 6.6 System Prompt Builder

`build_system_prompt()` に `SECRET.md` のロード処理を追加:

```rust
pub(crate) fn build_system_prompt(state: &AppState, context: &SurfaceContext) -> String {
    let mut prompt = String::new();

    // 1. SOUL section
    if let Some(s) = build_soul_prompt_section(state, context) { push(&mut prompt, s); }
    // 2. Model instructions
    if let Some(s) = build_model_instructions_section(state, context) { push(&mut prompt, s); }
    // 3. Base prompt (CORE_INSTRUCTIONS + channel/thread)
    prompt.push_str(&build_base_prompt(context));
    // 4. AGENTS section
    if let Some(s) = build_agents_prompt_section(state, context) { push(&mut prompt, s); }
    // 5. SECRET section ← 新設・秘密モード時のみ
    if context.is_secret {
        if let Some(s) = build_secret_prompt_section(state, context) { push(&mut prompt, s); }
    }
    // 6. Memory section (長期記憶 3層)
    if let Some(s) = build_memory_prompt_section(state, context) { push(&mut prompt, s); }
    // 7. Skills section
    if let Some(s) = build_skills_prompt_section(state) { push(&mut prompt, s); }

    prompt
}
```

注入順序の rationale:

- **SOUL → Model instructions → Base → AGENTS** で「自分が誰で何ができるか」を確定
- **SECRET.md** を直後に置くことで、後続の Memory 解釈に「いまは秘密モード」というフレームが効く
- Memory・Skills は動的情報なので、静的なモード指示の後に配置

`SoulAgentsLoader::load_secret(agent_id) -> Option<String>` を新設。`SECRET.md` が存在しない場合は `None` を返し、`build_secret_prompt_section` も `None` を返す（= プロンプトに影響なし）。

### 6.7 PULSE

PULSE の発火判定・実行は**秘密チャットでは行わない**:

- `pulse/` 系は `state.db`（通常）のみ参照。`secret.db` の chat や session にはアクセスしない
- 秘密チャットに向けた PULSE 通知生成はスキップ
- これは構造的保証: PULSE が `secret.db` を知らなければ漏洩しえない

### 6.8 Sleep Batch

Phase 1 では**秘密側の Sleep 機構は未実装**:

- 通常 Sleep Batch は `state.db`（通常）のみ参照。`secret.db` の message は処理しない（構造的保証）
- `secret.db` に Sleep 起点となるテーブル（`sleep_runs` 等）は作らない
- 結果: 秘密チャットの内容は `episode_events` 等へ昇格しない

### 6.9 Tools

Phase 1 では**ツール実行の特別な制限は設けない**。秘密モード時も `write` / `edit` / `bash` 等は通常通り動作する。

理由: エージェントごとのワークスペース分離を想定しないため、「書き込み先を `secret_memory/` 配下に限定」といったパスガードは有用でない。秘匿会話のユースケースは会話系が主で、ファイル書き出しを伴うツール利用は想定外。

**残存リスク**: 秘密モード中に agent が `write`/`edit` で `~/.egopulse/` 配下等に秘匿内容を書き出した場合、それは `secret.db` の外に残留する。Phase 2 でワークスペース概念を導入する際に再検討。

#### DB 参照を持つツール類のルーティング

`SendMessageTool` / `AgentSendTool` 等の DB 参照を持つツールは、`runtime/mod.rs` の構築時に `Arc::clone(&deps.db)` を注入されている。これらを秘密モードで動かすには**起動時の注入を `db` / `secret_db` の両方に拡張**し、実行時に `context.is_secret` で参照を切替える。

**前提**: `ToolExecutionContext`（`src/tools/mod.rs`）に `is_secret: bool` フィールドを追加する必要がある。現状この構造体は `SurfaceContext` のサブセット的な情報を持つが、`is_secret` を持たない。`process_turn` で `SurfaceContext` から `ToolExecutionContext` を構築する際に `is_secret` を伝播させる。

```rust
// src/tools/mod.rs
pub(crate) struct ToolExecutionContext {
    pub chat_id: i64,
    // ... 既存フィールド ...
    pub is_secret: bool,  // 新設
}
```

```rust
// runtime/mod.rs の ToolRegistry 構築
tools.register_tool(Box::new(crate::tools::SendMessageTool::new(
    workspace_dir.clone(),
    Arc::clone(&channels),
    Arc::clone(&deps.db),
    deps.secret_db.clone(),  // 新設
)));

tools.register_tool(Box::new(crate::tools::AgentSendTool::new(
    config.agents.clone(),
    Arc::clone(&deps.db),
    deps.secret_db.clone(),  // 新設
    Arc::clone(&channels),
)));
```

各ツールの実行時には:

```rust
// agent_send.rs 等
let db = if context.is_secret {
    self.secret_db.as_ref().expect("secret db required for secret mode turn")
} else {
    &self.db
};
```

`AgentSendTool` 内の Channel Log 保存（`store_message_only`）や chat info ルックアップ（`lookup_chat_info`）もすべてこの `db` を経由する。**Channel Log も secret.db 側に保存**されることで、Multi-Agent Room の secret チャネル内の agent_send 通信が `egopulse.db` に漏れない。

### 6.10 Backup

`runtime/mod.rs` の定期バックアップ・起動時バックアップ機構を拡張:

- `egopulse.db` は従来通り `VACUUM INTO`
- `secret.db` も同一スケジュールで `VACUUM INTO`（ファイル名 `secret-YYYYMMDD-HHMMSS.db`）
- バックアップ世代管理も `egopulse.db` 系と `secret.db` 系で独立してカウント

### 6.11 Logging

`tracing` で秘匿内容がログに残らないよう、秘密ターンでは**内容フィールドを span に含めない**:

- `info_span!("turn", agent_id, is_secret = true)` — `user_msg` 等の content フィールドを含めない
- `tool.execute` ログは input/output フィールドを省略。`name` と status のみ
- `llm.request` / `llm.response` ログは内容を含めない。token 数やエラーのメタ情報のみ
- `error!` ログでも内容は含めない。エラー種別と `trace_id` のみ

実装は `ctx.is_secret` フラグを見て field を条件付きで span に挿入する分岐を各所に追加。

---

## 7. ライフサイクル

### 7.1 初期化（プロセス起動時）

```text
1. Config ロード
2. AppState 構築
   ├─ Database::new(egopulse.db) → run_migrations()
   ├─ secret_needed = config に secret: true エントリがある
   ├─ if secret_needed {
   │     Database::new_secret(secret.db) → run_secret_migrations()
   │     state.secret_db = Some(Arc::new(secret_db))
   │  }
   └─ 既存の SkillManager / ToolRegistry / ChannelAdapter 構築
3. start_channels() で Discord / Telegram / Web 起動
```

`secret.db` は不要な環境（設定に secret エントリが1件もない）では作成されない。ディスクスペース・アクセス権の無駄を避ける。

### 7.2 通常メッセージ処理（秘密モード）

```text
1. チャネルがメッセージ受信
2. SurfaceContext 生成（この時点で is_secret 確定）
   ├─ Discord: 受信チャネル ID の config.secret フラグを参照
   └─ Telegram: 受信 chat ID の config.secret フラグを参照
3. agent_loop::process_turn(state, ctx, message)
   ├─ db = state.db_for(ctx.is_secret)
   ├─ chat_id = db.resolve_or_create_chat_id(channel, external_chat_id, ...)
   ├─ session_snapshot = db.load_session_snapshot(chat_id)
   ├─ safety_compaction 判定（必要なら実行）
   ├─ system_prompt 構築（§6.6 の順序で）
   │   ├─ SOUL.md
   │   ├─ Model instructions
   │   ├─ Base prompt
   │   ├─ AGENTS.md
   │   ├─ if ctx.is_secret { SECRET.md }  ← 秘密時のみ
   │   ├─ Memory (通常 3層)
   │   └─ Skills catalog
   ├─ memory ロード
   │   └─ episodic.md / semantic.md / prospective.md（通常・秘密両方で注入）
   ├─ LLM 呼び出し + tool 実行ループ
   │   └─ tool 実行時: 特別な制限なし（§6.9 参照）
   ├─ message / session 永続化（すべて db、つまり secret.db 側）
   ├─ llm_usage_log 永続化（db、つまり secret.db 側）
   └─ 応答送信
```

### 7.3 Multi-Agent Room での `agent_send`

secret チャネル内での Multi-Agent Room 挙動:

```text
#secret-room (config: secret=true)
├─ alice (responds)
├─ bob (responds)
└─ user ping → alice
   │
   └ alice の turn (is_secret=true)
      ├─ Channel Log の保存先: secret.db 内の channel_log chat
      ├─ agent_send(to=bob) 実行
      │   ├─ 送信メッセージ: secret.db の Channel Log に保存 (MessageKind::AgentSend)
      │   └─ PendingAgentTurn が queue に入る
      └─ 応答送信（secret チャネルへ）

   ↓ bob の turn が schedule される
   ├─ bob の turn (is_secret=true)  ← チャネル属性を継承
   └─ 同様に secret.db 側で処理
```

**クロスモード `agent_send` は構造的に発生しない**: secret state はチャット（チャネル）属性であり、`agent_send` は同一チャネル内でしか発生しないため、送信元と受信元で必ず同一の `is_secret` になる。

### 7.4 バックアップ

```text
起動時バックアップ:
├─ if egopulse.db が存在: VACUUM INTO egopulse-YYYYMMDD-HHMMSS.db
└─ if secret.db が存在: VACUUM INTO secret-YYYYMMDD-HHMMSS.db

定期バックアップ（interval_days 毎）:
├─ VACUUM INTO egopulse-YYYYMMDD-HHMMSS.db
└─ if secret.db が存在: VACUUM INTO secret-YYYYMMDD-HHMMSS.db

世代管理:
├─ egopulse-*.db は max_generations を超えた古い順に削除
└─ secret-*.db も独立して max_generations 適用
```

---

## 8. 設定リファレンス

### 8.1 新設フィールド一覧

| フィールド | 型 | デフォルト | 説明 |
|---|---|---|---|
| `channels.discord.channels.<id>.secret` | bool | `false` | この Discord チャネルを秘密モードにする |
| `channels.telegram.telegram_channels.<id>.secret` | bool | `false` | この Telegram chat を秘密モードにする |

### 8.2 完全 YAML 例（抜粋）

```yaml
channels:
  discord:
    enabled: true
    bots:
      main:
        token: { source: env, id: DISCORD_BOT_TOKEN }
    channels:
      "1234567890123456789":
        require_mention: true
      "9876547890123456789":
        require_mention: true
        multi_agent: true
        secret: true                    # このチャネルは秘密モード

  telegram:
    enabled: true
    telegram_bots:
      default:
        token: { source: env, id: TELEGRAM_BOT_TOKEN }
    telegram_channels:
      "-1001234567890":
        secret: true                    # この chat は秘密モード
      "-1009876543210":
        require_mention: true

db_backup:
  interval_days: 7
  time_of_day: "03:00"
  max_generations: 12
```

### 8.3 再起動要否

| フィールド | 変更時の再起動 |
|---|---|
| `channels.discord.channels.<id>.secret` | 必要 |
| `channels.telegram.telegram_channels.<id>.secret` | 必要 |

**理由**: `secret_db` の初期化は起動時の1回限りで、`AppState` が `Arc<Database>` を保持する構造。ホットリロードでこれを動的に追加・破棄するには、稼働中の秘密ターンのキューイング・`Mutex<Connection>` の所有権移譲・`db_for()` 参照のライフタイム管理等の整合性を保つ必要があり、複雑かつ安全面のメリットが薄い。**Phase 1 は再起動必須で安全側に倒す**。

**却下した代替案**:
- *ホットリロード（lazy init）*: config リロード検出時に `secret_db` を生成。ただし削除時（`secret: true` → `false`）の挙動が曖昧（稼働中の secret session は継続するか等）で、競合が増える
- *明示的 reload コマンド（`egopulse reload`）*: 単に再起動のステップを増やすだけでメリット薄

Phase 2 以降で「Channel 追加・agent 変更等も含めたホットリロード全体」を見直す際に、一緒に検討する。

---

## 9. 隔離保証一覧

### 9.1 機密性保証（秘匿内容が通常経路に漏れない）

| 保証 | 実装機構 |
|---|---|
| 通常 Sleep Batch が秘密メッセージを処理しない | Sleep Batch は `state.db` のみ参照。`secret.db` には接続しない |
| PULSE が秘密内容で発火・投稿しない | PULSE は `state.db` のみ参照。秘密チャットでは発火しない |
| 通常 LLM Context に秘密 session が混入しない | `db_for(is_secret)` で参照 DB を分離 |
| 通常チャット session 一覧に秘密チャットが混入しない | 一覧クエリは `state.db` のみ参照 |
| 秘密ターンが `egopulse.db` に一切書き込まない | `process_turn` 内の chat / message / session / `llm_usage_logs` すべて `db_for(is_secret)` 経由。`tool_calls` への INSERT は `is_secret == true` 時にスキップ |
| 秘密チャネルの Channel Log が `egopulse.db` に書き込まれない | Discord / Telegram の `store_human_channel_log_message` が `db_for(is_secret)` 経由で DB 切替 |
| `agent_send` 等のツール経由 DB アクセスも `egopulse.db` に書き込まない | `SendMessageTool` / `AgentSendTool` が `secret_db` を持ち、`context.is_secret` で参照切替 |
| 秘密ターン内容が `tracing` ログに出力されない | 秘密ターンは内容フィールドを span に含めない |
| Multi-Agent Room でクロスモード通信が発生しない | secret state はチャネル属性。同一チャネル内で `agent_send` が完結 |

**注**: Tool 実行経由での書き出し（`write`/`edit`/`bash`）は Phase 1 では対処しない（§6.9 参照）。

### 9.2 可用性保証（秘密モードが機能する）

| 保証 | 実装機構 |
|---|---|
| 秘密チャットの session が永続化・再開可能 | `secret.db.sessions` テーブル |
| 秘密モードでエージェントが通常記憶を参照できる | 通常 memory 3層は両モードで注入 |
| 秘密モードでエージェントが `SECRET.md` 指示を受け取る | `is_secret == true` 時に system prompt へ追加注入 |
| 秘密 Multi-Agent Room が機能する | Channel Log 含め `secret.db` 側で完結 |
| バックアップから秘密 DB を復元できる | `secret.db` は `egopulse.db` と同一スケジュールでバックアップ |

---

## 10. Phase 1 スコープ外

以下は Phase 1 では実装せず、Phase 2 以降で検討:

### 10.1 拡張系

- **`secret_episodic.md` + Secret Sleep Batch**: 秘密側でのイベント記憶蓄積。`episode_events` / `episode_rollups` テーブル追加、`sleep/secret_batch.rs` 新設、`secret_memory/secret_episodic.md` 自動更新
- **`secret_semantic.md` / `secret_prospective.md`**: 秘密側での3層記憶の完全導入
- **WebUI / TUI での秘密チャット表示**: 一覧表示・「New Secret Chat」ボタン等
- **SOUL.md / Skills / Tools 許可リストの上書き**: 秘密モード専用設定
- **per-agent の秘密機能 opt-out**: `agents.<id>.enable_secret: false`
- **通常メモリのレイヤー単位ブロック**: 例: prospective は秘密に持ち込まない

### 10.2 運用系

- **`secret.db` の暗号化（SQLCipher）**: 鍵管理問題を解決してから導入検討
- **`secret.db` の個別削除ツール**: `egopulse secret purge` 等のコマンド

### 10.3 UX 系

- **秘密チャットのロック・パスワード保護**: Web/TUI 表示対応時の Phase 2 以降
- **`/secret-sleep` 手動トリガ**: Secret Sleep Batch 導入時に合わせて

---

## 11. テスト戦略

### 11.1 ユニットテスト

- **Storage**: `secret.db` のマイグレーション・CRUD が正常動作すること
- **Config**: `secret: true` フラグのパース・バリデーション
- **Path Guard**: 秘密モードでの書き込み許可・拒否の境界チェック
- **System Prompt Builder**: `SECRET.md` が `is_secret == true` 時のみ注入されること

### 11.2 統合テスト

- **隔離保証**:
  - 秘密チャットで発言 → 通常 `episode_events` にレコードが増えない
  - 秘密チャットで発言 → 通常 session 一覧に含まれない
  - PULSE が秘密チャットで発火しない
  - Sleep Batch（通常）実行後、`secret.db` の内容が変わらない
- **ラウンドトリップ**:
  - 秘密チャットで user 発言 → assistant 応答 → 再起動 → session 復元が正常
  - `agent_send` が同一 `secret: true` チャネル内で正常動作
- **バックアップ**:
  - `secret.db` が存在する場合、`egopulse.db` と同一スケジュールで `secret-*.db` が生成される
  - 世代管理が `egopulse-*.db` 系と `secret-*.db` 系で独立して機能する

### 11.3 ログ Redaction テスト

- 秘密ターン実行後、ログ出力に user message content / tool input / tool output が含まれないことを検証
- 通常ターンでは内容ログが出力されることを検証（regression 防止）

---

## 12. 今後の拡張（Phase 2 以降）

### 12.1 `secret_episodic.md` + Secret Sleep Batch

- **テーブル**: `secret.db` に `episode_events` / `episode_rollups` を新設
- **ファイル**: `~/.egopulse/agents/<agent_id>/secret_memory/secret_episodic.md`
- **Sleep Batch**: `sleep/secret_batch.rs` 新設。ステップは `event_extraction` + `episodic_update` の2つ。通常 Sleep と同一スケジュール（04:00 等）で発火、通常 batch → 秘密 batch の順で実行
- **Memory Loader**: `load_secret_episodic()` 新設。秘密モード時に追加注入

### 12.2 WebUI / TUI 表示

- セッション一覧に「🔒 Secret Sessions」セクションを新設
- 「New Secret Chat」ボタン / 「New Secret Session」メニュー
- 視覚的マーカー（ロックアイコン等）で誤画面共有時の目印提供
- 非表示トグル・パスワードロック等の UX 系は更に後の Phase で

### 12.3 SOUL.md / Skills / Tools 許可リスト上書き

- `agents.<id>.secret.soul_path`: 秘密モード専用 SOUL.md
- `agents.<id>.secret.skills`: 秘密モード時のスキル許可リスト
- `agents.<id>.secret.tools`: 秘密モード時のツール許可リスト
- per-agent opt-out（`agents.<id>.enable_secret: false`）

### 12.4 SQLCipher 暗号化

- 鍵管理方式の検討（OS keyring 統合等）
- `secret.db` を暗号化、パスフレーズなしでは開けない
- 鍵導出に失敗した場合は秘密モード無効化

---

## 改訂履歴

| 日付 | 内容 |
|---|---|
| 2026-06-22 | 初版（Phase 1 スコープ確定） |
