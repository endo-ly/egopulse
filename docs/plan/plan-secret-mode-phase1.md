# Plan: Secret Mode Phase 1

秘匿性の高い会話を通常長期記憶から物理的に隔離する「秘密モード」の Phase 1 を実装する。`secret.db` 別ファイル化、`SurfaceContext.is_secret` フラグ、`SECRET.md` ユーザー編集可能プロンプトを中核とする。設計詳細は [docs/plan/secret-mode-design.md](./secret-mode-design.md) を参照。

> **Note**: 振る舞い（What）は決して変えてはいけないが、より美しい設計があれば実装方法（HOW）だけは変えてもよい。

## 設計方針

- **別 DB ファイル分離**: `~/.egopulse/runtime/secret.db` を新設。`egopulse.db` 側は一切改修しない。`Database` 構造体は `pool: Pool` を持つ既存実装を流用し、`new_secret()` コンストラクタで秘密用インスタンスを生成する
- **`db_for(is_secret)` 戦略注入**: 全クエリ関数は `&Database` を取る現状を変更せず、呼出側が `state.db_for(ctx.is_secret)` で参照を切替える。`secret_*` 系の並列クエリ関数は作らない
- **構造的隔離優先**: PULSE / Sleep Batch は `state.db` のみを知る。`secret.db` には接続しない。明示的な分岐判定ではなく「接続先が違うので絶対に見えない」ことで漏洩を防ぐ
- **`SECRET.md` はユーザー制御**: バイナリ埋め込みなし。`SoulAgentsLoader::load_secret()` で `agents/<agent_id>/SECRET.md` を読み込む。ファイル不在時は `None` で振る舞いは通常同等
- **ログは内容フィールドを span に含めない**: `tracing` で `is_secret == true` の span には `user_msg` / `tool_input` / `tool_output` 等を載せない。`is_secret` フィールド自体は載せる（監査性確保）
- **Phase 1 は最小スコープ**: `secret_episodic.md`、WebUI/TUI 表示、SOUL/Skills/Tools 上書き、path guard 等は入れない

### 参照元

- 設計仕様: [docs/plan/secret-mode-design.md](./secret-mode-design.md)
- 既存スキーマ・マイグレーション: `src/storage/migration.rs`, `docs/db.md`
- 既存プロンプト構築: `src/agent_loop/prompt_builder.rs`, `docs/system-prompt.md`
- 既存 `SoulAgentsLoader`: `src/agent_loop/soul_agents.rs`
- 既存 `Database`: `src/storage/mod.rs`, `src/storage/backup.rs`
- 既存 `SurfaceContext`: `src/agent_loop/mod.rs`
- 既存 backup scheduler: `src/runtime/backup_scheduler.rs`

## TDD 方針

テストリスト項目（T1, T2…）と自動テスト（`test_name`）を明確に区別する。各 TDD Cycle では**テストリスト項目を1つだけ**選び、Red で自動テスト1件を書き、Green・Refactor を経てから次へ進む。1項目に境界値・異常系等の複数ケースがある場合は同一項目で Cycle を複数回回す。

Green 中に別ケースや気になるリファクタを混ぜない。実装中に新たな不安が出たら、その場で対処せずテストリストへ追加して次 Cycle で扱う。

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 種別 | 既存パターン / 参照元 | 備考 |
| -- | -- | -- | -- |
| `src/config/types.rs` | 変更 | `DiscordChannelConfig`, `TelegramChatConfig` | `secret: bool` フィールド追加 |
| `src/config/loader.rs` | 変更 | 既存の channel config parse | `secret` フラグ読込 |
| `src/storage/migration.rs` | 変更 | `run_migrations()`, `SCHEMA_VERSION` | `run_secret_migrations()`, `SECRET_SCHEMA_VERSION` 新設 |
| `src/storage/mod.rs` | 変更 | `Database::new()`, `Pool` | `Database::new_secret()` 新設 |
| `src/runtime/mod.rs` | 変更 | `AppState`, `AppStateParts` | `secret_db: Option<Arc<Database>>`, `db_for()` |
| `src/runtime/backup_scheduler.rs` | 変更 | periodic backup 機構 | `secret.db` の並列バックアップ |
| `src/storage/backup.rs` | 変更 | `run_backup()` | `secret.db` 向け VACUUM INTO |
| `src/agent_loop/mod.rs` | 変更 | `SurfaceContext` | `is_secret: bool` フィールド追加 |
| `src/agent_loop/turn.rs` | 変更 | `process_turn_inner()` | `db_for(ctx.is_secret)` 経由に切替 |
| `src/agent_loop/session.rs` | 変更 | `load_session`, `save_session` | DB 参照の動的切替 |
| `src/agent_loop/soul_agents.rs` | 変更 | `SoulAgentsLoader` | `load_secret()`, キャッシュ拡張 |
| `src/agent_loop/prompt_builder.rs` | 変更 | `build_system_prompt()` | `build_secret_prompt_section()` 新設 |
| `src/agent_loop/compaction.rs` | 変更 | archive path 構築 | `secret_groups/` への振分 |
| `src/channels/discord.rs` | 変更 | message receive flow | `is_secret` を SurfaceContext に設定 |
| `src/channels/telegram.rs` | 変更 | message receive flow | 同上 |
| `src/agent_loop/mod.rs` (`process_turn` 周辺) | 変更 | tracing span 構築 | 秘密ターンは content field を span に含めない |
| `docs/db.md` | 変更 | 既存スキーマ説明 | `secret.db` 追加 |
| `docs/config.md` | 変更 | 既存コンフィグ説明 | `secret: true` フィールド追加 |
| `docs/architecture.md` | 変更 | 既存アーキテクチャ | secret mode 追加 |
| `docs/channels.md` | 変更 | チャネル説明 | secret フラグ |
| `docs/session-lifecycle.md` | 変更 | session 永続化 | secret.db のルーティング |
| `docs/system-prompt.md` | 変更 | system prompt 構築 | SECRET.md 注入順序 |

## テストリスト / 不安リスト

| ID | 観点 | 期待する振る舞い | 優先 | 対応するCycle | 状態 / 今回対象外理由 |
| -- | -- | -- | -- | -- | -- |
| T1 | 正常系 | `DiscordChannelConfig` が YAML の `secret: true` を読み込める | High | Step 1 | 未着手 |
| T2 | 正常系 | `TelegramChatConfig` が YAML の `secret: true` を読み込める | High | Step 1 | 未着手 |
| T3 | デフォルト | `DiscordChannelConfig.secret`, `TelegramChatConfig.secret` のデフォルトは `false` | High | Step 1 | 未着手 |
| T4 | 正常系 | `Database::new_secret(path)` が `secret.db` を開き WAL モードにする | High | Step 2 | 未着手 |
| T5 | 正常系 | `run_secret_migrations()` が `chats`, `messages`, `sessions`, `llm_usage_logs`, `db_meta`, `schema_migrations` の6テーブルを作る | High | Step 2 | 未着手 |
| T6 | 境界値 | `run_secret_migrations()` は2回目の呼出で冪等（エラーなく同一スキーマ） | High | Step 2 | 未着手 |
| T7 | 正常系 | `AppState::db_for(false)` は通常 `db` を返す | High | Step 3 | 未着手 |
| T8 | 正常系 | `AppState::db_for(true)` は `secret_db` を返す（初期化済み前提） | High | Step 3 | 未着手 |
| T9 | 異常系 | `AppState::db_for(true)` は `secret_db == None` のとき panic する | Medium | Step 3 | 未着手 |
| T10 | 正常系 | `AppState::secret_enabled()` は `secret_db.is_some()` と一致 | Medium | Step 3 | 未着手 |
| T11 | 統合 | config に secret エントリ1件でもあれば `secret_db` が初期化される | High | Step 3 | 未着手 |
| T12 | 統合 | config に secret エントリ0件なら `secret_db == None` | High | Step 3 | 未着手 |
| T13 | 正常系 | `SurfaceContext` に `is_secret: bool` を追加できる（既存テストのビルドが通る） | High | Step 4 | 未着手 |
| T14 | 正常系 | `SoulAgentsLoader::load_secret(agent_id)` は `agents/<id>/SECRET.md` が存在する時に内容を返す | High | Step 5 | 未着手 |
| T15 | 空状態 | `SoulAgentsLoader::load_secret(agent_id)` は `SECRET.md` 不在時に `None` を返す（エラーにしない） | High | Step 5 | 未着手 |
| T16 | 異常系 | `load_secret` は path traversal を含む `agent_id` を拒否する（`safe_agent_id` と整合） | Medium | Step 5 | 未着手 |
| T17 | 統合 | system prompt に `is_secret == true` のとき `SECRET.md` 内容が含まれる | High | Step 6 | 未着手 |
| T18 | 統合 | system prompt に `is_secret == false` のとき `SECRET.md` は含まれない | High | Step 6 | 未着手 |
| T19 | 統合 | `SECRET.md` は AGENTS section より後、Memory section より前に出現する | Medium | Step 6 | 未着手 |
| T20 | 正常系 | `process_turn` は `is_secret == true` 時に `secret_db` 側に chat を作る | High | Step 7 | 未着手 |
| T21 | 正常系 | `process_turn` は `is_secret == false` 時に `egopulse.db` 側に chat を作る（既存挙動維持） | High | Step 7 | 未着手 |
| T22 | 統合 | 秘密ターンで store_message が `secret.db.messages` に INSERT される | High | Step 7 | 未着手 |
| T23 | 統合 | 秘密ターンで `egopulse.db` 側は一切変更されない（chat/message/session 0件） | High | Step 7 | 未着手 |
| T24 | 正常系 | Discord メッセージ受信時、`secret: true` チャネルなら `SurfaceContext.is_secret == true` | High | Step 8 | 未着手 |
| T25 | 正常系 | Discord メッセージ受信時、`secret: false`（デフォルト含む）なら `is_secret == false` | High | Step 8 | 未着手 |
| T26 | 正常系 | Telegram メッセージ受信時、`secret: true` chat なら `is_secret == true` | High | Step 9 | 未着手 |
| T27 | 正常系 | Telegram メッセージ受信時、`secret: false`（デフォルト含む）なら `is_secret == false` | High | Step 9 | 未着手 |
| T28 | 正常系 | Compaction archive が `is_secret == true` 時に `runtime/secret_groups/...` に出力される | Medium | Step 10 | 未着手 |
| T29 | 正常系 | Compaction archive が `is_secret == false` 時に `runtime/groups/...` に出力される（既存挙動） | Medium | Step 10 | 未着手 |
| T30 | 正常系 | バックアップ機構が `secret.db` 存在時に `secret-YYYYMMDD-HHMMSS.db` を生成する | Medium | Step 11 | 未着手 |
| T31 | 空状態 | バックアップ機構が `secret_db == None` 時は `secret-*.db` を生成しない | Medium | Step 11 | 未着手 |
| T32 | 統合 | 秘密ターンの `tracing` span に `user_msg` フィールドが含まれない | Medium | Step 12 | 未着手 |
| T33 | 統合 | 通常ターンの `tracing` span には `user_msg` フィールドが含まれる（regression） | Medium | Step 12 | 未着手 |
| T34 | 統合 | agent_send で発生する相手 turn が送信元の `is_secret` を継承する | Medium | Step 13 | 未着手 |
| T35 | 異常系 | `safe_agent_id` が `..` 等を含む `agent_id` を拒否する（既存の回帰確認） | Low | 今回対象外 | 既存 `safe_agent_id` ロジックを流用するため再テスト不要 |
| T36 | 統合 | 秘密ターンの LLM usage log が `secret.db.llm_usage_logs` に格納され、`egopulse.db.llm_usage_logs` に増えない | High | Step 7 | 未着手 |
| T37 | 統合 | 秘密ターンで `tool_calls` INSERT がスキップされ、エラーにならない（`secret.db` に `tool_calls` テーブル無し） | High | Step 7 | 未着手 |
| T38 | 統合 | secret Discord チャネルの Channel Log 保存（`store_human_channel_log_message`）が `secret.db` 側に書き込まれる | High | Step 8 | 未着手 |
| T39 | 統合 | secret Telegram chat の Channel Log 保存が `secret.db` 側に書き込まれる | High | Step 9 | 未着手 |
| T40 | 統合 | agent_send 実行時の Channel Log 保存（`store_message_only`）が `context.is_secret == true` 時に `secret.db` に書き込まれる | High | Step 13 | 未着手 |
| T41 | 統合 | `AgentSendTool` / `SendMessageTool` が `context.is_secret` に基づき `db` / `secret_db` を切替える | High | Step 13 | 未着手 |
| T42 | 統合 | 秘密ターン終了後、`egopulse.db` の全テーブル（`chats` / `messages` / `sessions` / `tool_calls` / `llm_usage_logs`）が1件も増えていない | High | Step 15 | 未着手 |
| T43 | 統合 | `ToolExecutionContext` が `is_secret: bool` フィールドを持ち、`process_turn` が `SurfaceContext.is_secret` をこれへ伝播する | High | Step 7 | 未着手 |
| T44 | 統合 | secret チャネルで `/new` を実行したとき、`secret.db.sessions` のクリアのみ行われ `egopulse.db.sessions` は触られない | High | Step 14 | 未着手 |
| T45 | 統合 | secret チャネルで `/compact` / `/status` を実行したとき、読込・書込が `secret.db` 側で完結する | High | Step 14 | 未着手 |
| T46 | 統合 | Discord / Telegram の `make_context()` がチャネルの `secret` フラグを参照し `SurfaceContext.is_secret` を設定する。これにより slash command 経路（`process_text_slash_command` / interaction）も自動的に正しい `is_secret` を持つ | High | Step 8 / 9 | 未着手 |
| T47 | 統合 | Multi-Agent Room の停止条件発火時、`runtime/mod.rs` の `store_system_event` が `state.db_for(turn.context.is_secret)` 経由で DB 切替される | High | Step 13 | 未着手 |
| T48 | 統合 | Compaction 実行時の LLM usage log 挿入（`compaction.rs` 内 `state.db` 参照）が `db_for(is_secret)` 経由で DB 切替される | High | Step 10 | 未着手 |

---

## Step 0: Worktree 作成

- ブランチ名: `feature/secret-mode-phase1`
- 作成コマンド:
  - `git worktree add ../egopulse-secret-phase1 -b feature/secret-mode-phase1`
- 作業前に `git status --short` で既存差分を確認する

---

## Step 1: Config 型拡張 TDD Cycle - `secret` フラグ追加

### この Step の目的

Discord / Telegram の per-channel config に `secret: bool` フィールドを追加し、YAML からの読込とデフォルト値を担保する。

### 今回選ぶ項目

- 対象: `T1`, `T2`, `T3`
- 選ぶ理由: 全体の入口となる設定。これが無いと下流のルーティングが決まらない
- この時点では扱わないこと: AppState の `secret_db` 初期化、Discord/Telegram への伝播

### RED: 失敗する自動テストを書く

- 追加するテスト名（`src/config/tests.rs` に追加）:
  - `discord_channel_config_parses_secret_flag`
  - `telegram_chat_config_parses_secret_flag`
  - `channel_config_secret_defaults_to_false`
- Given: YAML 文字列（`secret: true` を含む / 含まない）
- When: `serde_yaml::from_str::<DiscordChannelConfig>(yaml)`
- Then:
  - `secret: true` YAML → `config.secret == true`
  - デフォルト → `config.secret == false`
- 失敗理由の想定: `DiscordChannelConfig`, `TelegramChatConfig` に `secret` フィールドが無いため、デシリアライズでエラーまたは無視される

### GREEN: 最小実装

`src/config/types.rs` の `DiscordChannelConfig`, `TelegramChatConfig` に `pub secret: bool` を追加。`#[derive(Default)]` に頼り `false` をデフォルトとする。`serde` 属性は既存フィールドと同じ（`#[serde(default)]` は必須）。

### REFACTOR: 設計の整理

- 重複: 2つの struct に同じフィールドが並ぶが、本質的に別物（Discord はチャネル、Telegram は chat）なのでマージしない
- 命名: `secret` で一貫
- 責務: config は読込のみ。AppState の初期化判定は Step 3 で扱う
- テストの構造的結合: 内部表現に依存しない

### テストリスト更新

- 完了: `T1`, `T2`, `T3`
- 追加: なし
- 次候補: `T4`, `T5`, `T6`（Storage）

### コミット

`feat: add secret flag to discord and telegram channel config`

---

## Step 2: Storage TDD Cycle - `Database::new_secret` と `run_secret_migrations`

### この Step の目的

`secret.db` を開く `Database::new_secret()` と、6テーブルを作る `run_secret_migrations()` を実装する。

### 今回選ぶ項目

- 対象: `T4`, `T5`, `T6`
- 選ぶ理由: 全コンポーネントが依存する基盤。先に確定させないとルーティングが書けない
- この時点では扱わないこと: AppState への統合、既存 `egopulse.db` のマイグレーション改修

### RED: 失敗する自動テストを書く

- 追加するテスト名（`src/storage/migration.rs` 末尾の `#[cfg(test)]` または新規テストモジュールに追加）:
  - `new_secret_opens_wal_database`
  - `run_secret_migrations_creates_expected_tables`
  - `run_secret_migrations_is_idempotent`
- Given: tmp ディレクトリに `secret.db` パス
- When: `Database::new_secret(&path)` → migration 実行 → テーブル一覧取得
- Then:
  - `PRAGMA journal_mode` が `wal`
  - `sqlite_master` に `chats`, `messages`, `sessions`, `llm_usage_logs`, `db_meta`, `schema_migrations` が含まれる
  - 2回続けて `new_secret()` してもエラーにならず、スキーマが同一
- 失敗理由の想定: `new_secret` 未実装、`run_secret_migrations` 未実装

### GREEN: 最小実装

`src/storage/migration.rs` に以下を追加:

```rust
pub(super) const SECRET_SCHEMA_VERSION: i64 = 1;

pub(super) fn run_secret_migrations(conn: &Connection) -> Result<(), StorageError> {
    let mut version = schema_version(conn)?;  // 既存関数を流用
    if version < 1 {
        // chats, messages, sessions, llm_usage_logs, db_meta, schema_migrations を作成
        // db_meta, schema_migrations は schema_version() 内で自動作成される
        conn.execute_batch(/* DDL: 既存スキーマから必要6テーブルを抽出 */)?;
        set_schema_version(conn, 1, "initial secret schema")?;
        version = 1;
    }
    debug_assert_eq!(version, SECRET_SCHEMA_VERSION);
    Ok(())
}
```

`src/storage/mod.rs` の `Database` に `new_secret()` を追加:

```rust
impl Database {
    pub(crate) fn new_secret(path: &Path) -> Result<Self, StorageError> {
        // new() と同様に Pool を初期化し、migration だけ run_secret_migrations に切替
    }
}
```

### REFACTOR: 設計の整理

- 重複: 既存 `run_migrations()` の DDL と重複する部分は許容（スキーマが独立進化する前提）
- 命名: `run_secret_migrations`, `SECRET_SCHEMA_VERSION`, `new_secret` で一貫
- 責務: `Database` は「どの DB ファイルか」を知らない。new_secret はあくまでエントリポイントの1つ
- テストの構造的結合: テーブル名リストをハードコードせず、定数 `SECRET_TABLES` 等を定義して参照

### テストリスト更新

- 完了: `T4`, `T5`, `T6`
- 追加: なし
- 次候補: `T7`, `T8`, `T9`, `T10`, `T11`, `T12`（AppState）

### コミット

`feat: add secret.db migrations and Database::new_secret`

---

## Step 3: AppState TDD Cycle - `secret_db` と `db_for`

### この Step の目的

`AppState` に `secret_db: Option<Arc<Database>>` を持たせ、`db_for(is_secret)` で動的に参照を返す。

### 今回選ぶ項目

- 対象: `T7`, `T8`, `T9`, `T10`, `T11`, `T12`
- 選ぶ理由: 以降の Step 全てが依存する中核
- この時点では扱わないこと: `SurfaceContext.is_secret` の伝播（Step 4 以降）

### RED: 失敗する自動テストを書く

- 追加するテスト名（`src/runtime/mod.rs` または `src/runtime/tests.rs` 等に追加）:
  - `db_for_returns_normal_db_when_not_secret`
  - `db_for_returns_secret_db_when_secret`
  - `db_for_panics_when_secret_db_uninitialized`
  - `secret_enabled_reflects_secret_db_presence`
  - `app_state_initializes_secret_db_when_config_has_secret_entry`
  - `app_state_does_not_initialize_secret_db_when_no_secret_entry`
- Given: テスト用 Config（`secret: true` エントリあり / なし）、tmp `secret.db`
- When: `AppState::build_app_state(parts)` または同等のビルダ
- Then:
  - secret あり → `state.secret_db.is_some()`, `db_for(true)` がそれを返す
  - secret なし → `state.secret_db.is_none()`, `db_for(true)` が panic
  - `db_for(false)` は常に通常 db
- 失敗理由の想定: `AppState` に `secret_db` フィールド無し、ビルダが初期化ロジックを持たない

### GREEN: 最小実装

`src/runtime/mod.rs` の変更:

```rust
pub struct AppState {
    pub(crate) db: Arc<Database>,
    pub(crate) secret_db: Option<Arc<Database>>,  // 新設
    // ... 既存フィールド ...
}

impl AppState {
    pub(crate) fn secret_enabled(&self) -> bool {
        self.secret_db.is_some()
    }

    pub(crate) fn db_for(&self, is_secret: bool) -> &Database {
        if is_secret {
            self.secret_db.as_ref().expect("secret db required but not initialized")
        } else {
            &self.db
        }
    }
}
```

`build_app_state()` で config から `secret: true` エントリを走査し、1件でもあれば `Database::new_secret()` を呼んで `secret_db` を初期化:

```rust
let secret_needed = config.discord_channels().any(|(_, c)| c.secret)
    || config.telegram_chats().any(|(_, c)| c.secret);

let secret_db = if secret_needed {
    Some(Arc::new(Database::new_secret(&secret_db_path)?))
} else {
    None
};
```

### REFACTOR: 設計の整理

- 重複: `secret: true` エントリ走査ロジックは `Config` 側にメソッドとして切り出す（例: `Config::needs_secret_db() -> bool`）
- 命名: `secret_db`, `db_for`, `secret_enabled` で一貫
- 責務: AppState は「初期化済み DB を保持」だけ。Config 解析は Config 側でやる
- テストの構造的結合: AppState の内部構造ではなく、`db_for()` 振る舞いで検証

### テストリスト更新

- 完了: `T7`〜`T12`
- 追加: なし
- 次候補: `T13`（SurfaceContext）

### コミット

`feat: wire secret_db into AppState with db_for router`

---

## Step 4: SurfaceContext TDD Cycle - `is_secret` フィールド追加

### この Step の目的

`SurfaceContext` に `is_secret: bool` を追加する。全呼出箇所（channel adapter, agent_send 等）への影響を最小化するため、`SurfaceContext::new()` は `is_secret: false` をデフォルトとする。

### 今回選ぶ項目

- 対象: `T13`
- 選ぶ理由: 以降の Step でチャネルから `is_secret` を立てるには型が先
- この時点では扱わないこと: Discord/Telegram での実設定

### RED: 失敗する自動テストを書く

- 追加するテスト名（`src/agent_loop/mod.rs` のテストモジュール）:
  - `surface_context_supports_is_secret_field`
- Given: 既存の `SurfaceContext::new()` 呼出
- When: フィールドアクセス
- Then: `ctx.is_secret` にアクセス可能、デフォルト `false`
- 失敗理由の想定: フィールド無し

### GREEN: 最小実装

`src/agent_loop/mod.rs` の `SurfaceContext` に `pub is_secret: bool` を追加。`SurfaceContext::new()` シグネチャを変えず、内部で `is_secret: false` を設定。後続 Step でチャネル側から上書きする。

### REFACTOR: 設計の整理

- 重複: なし
- 命名: `is_secret` で一貫
- 責務: SurfaceContext は「発生源の属性」を運ぶだけ。判定ロジックは持たない
- テストの構造的結合: 既存テストのビルドが通ることを確認

### テストリスト更新

- 完了: `T13`
- 追加: なし
- 次候補: `T14`, `T15`, `T16`（SoulAgentsLoader）

### コミット

`refactor: add is_secret field to SurfaceContext with false default`

---

## Step 5: SoulAgentsLoader TDD Cycle - `load_secret`

### この Step の目的

`SoulAgentsLoader` に `load_secret(agent_id) -> Option<String>` を追加し、`agents/<agent_id>/SECRET.md` を読み込む。既存の `safe_agent_id` ガードと mtime cache を流用。

### 今回選ぶ項目

- 対象: `T14`, `T15`, `T16`
- 選ぶ理由: system prompt への注入（次 Step）の前提
- この時点では扱わないこと: prompt builder への統合

### RED: 失敗する自動テストを書く

- 追加するテスト名（`src/agent_loop/soul_agents.rs` のテストモジュール）:
  - `load_secret_returns_content_when_file_exists`
  - `load_secret_returns_none_when_file_missing`
  - `load_secret_rejects_unsafe_agent_id`
- Given: tmp `agents/<id>/SECRET.md` を作成 / 作成しない
- When: `loader.load_secret("agent_id")`
- Then:
  - ファイル存在 → `Some(content)`
  - ファイル不在 → `None`
  - `agent_id = "../etc"` 等 → `None`（safe_agent_id と同じ挙動）
- 失敗理由の想定: `load_secret` メソッド無し

### GREEN: 最小実装

`SoulAgentsLoader` に以下を追加:

```rust
pub(crate) fn load_secret(&self, agent_id: &str) -> Option<String> {
    self.read_agent_file(agent_id, "SECRET.md")
}
```

`read_agent_file` は既存の `safe_agent_id` ガードを使うため、path traversal 保護は自動的に効く。

### REFACTOR: 設計の整理

- 重複: `read_agent_file` を流用するので新規 I/O ロジックなし
- 命名: `load_secret` で SOUL/AGENTS と一貫
- 責務: ファイル読込のみ。キャッシュ済み内容の invalidate 等は既存機構に任せる
- テストの構造的結合: 内部キャッシュに依存しない

### テストリスト更新

- 完了: `T14`, `T15`, `T16`
- 追加: なし
- 次候補: `T17`, `T18`, `T19`（Prompt Builder）

### コミット

`feat: load SECRET.md via SoulAgentsLoader`

---

## Step 6: System Prompt Builder TDD Cycle - SECRET.md 注入

### この Step の目的

`build_system_prompt()` で `is_secret == true` のとき `SECRET.md` を AGENTS section と Memory section の間に注入する。

### 今回選ぶ項目

- 対象: `T17`, `T18`, `T19`
- 選ぶ理由: 秘密モード時の LLM 挙動を決める中核
- この時点では扱わないこと: agent_loop の DB routing

### RED: 失敗する自動テストを書く

- 追加するテスト名（`src/agent_loop/prompt_builder.rs` のテストモジュール）:
  - `system_prompt_includes_secret_md_when_is_secret`
  - `system_prompt_excludes_secret_md_when_not_secret`
  - `secret_md_appears_between_agents_and_memory`
- Given: テスト用 AppState (SECRET.md を含む agents dir)、`SurfaceContext { is_secret: true/false, ... }`
- When: `build_system_prompt(&state, &ctx)`
- Then:
  - is_secret=true → 出力に `<secret>` block または SECRET.md 内容が含まれる
  - is_secret=false → 含まれない
  - 順序: AGENTS section 終了 > SECRET section 開始 > Memory section 開始
- 失敗理由の想定: `build_secret_prompt_section` 未実装、または順序分岐なし

### GREEN: 最小実装

`src/agent_loop/prompt_builder.rs` に追加:

```rust
fn build_secret_prompt_section(state: &AppState, context: &SurfaceContext) -> Option<String> {
    if !context.is_secret {
        return None;
    }
    let content = state.soul_agents.load_secret(&context.agent_id)?;
    Some(format!("<secret>\n{content}\n</secret>"))
}
```

`build_system_prompt()` の順序を、AGENTS section と Memory section の間に SECRET section 挿入に変更:

```rust
// 4. AGENTS section
if let Some(s) = build_agents_prompt_section(state, context) { push(&mut prompt, s); }
// 5. SECRET section
if let Some(s) = build_secret_prompt_section(state, context) { push(&mut prompt, s); }
// 6. Memory section
if let Some(s) = build_memory_prompt_section(state, context) { push(&mut prompt, s); }
```

### REFACTOR: 設計の整理

- 重複: なし
- 命名: `build_secret_prompt_section` で `build_agents_prompt_section` と一貫
- 責務: prompt builder は順序組み立てのみ。`is_secret` 判定は SECRET section 内部
- テストの構造的結合: 順序検証は文字列検索（`find()`）で抽象度を保つ

### テストリスト更新

- 完了: `T17`, `T18`, `T19`
- 追加: なし
- 次候補: `T20`, `T21`, `T22`, `T23`（Agent Loop DB routing）

### コミット

`feat: inject SECRET.md into system prompt between AGENTS and Memory`

---

## Step 7: Agent Loop TDD Cycle - `db_for(ctx.is_secret)` 経由 DB 切替

### この Step の目的

`process_turn_inner()` 内の **全 DB アクセス**（chat 解決、session 読込・保存、message 保存、llm_usage_log、tool_call 永続化）を `state.db_for(ctx.is_secret)` 経由に切替える。**tool_call 永続化は秘密モード時スキップ**する。**加えて `ToolExecutionContext` に `is_secret` フィールドを追加し、`process_turn` からツール群へ伝播する**（これがないと `AgentSendTool` / `SendMessageTool` が `context.is_secret` を読めない）。

### 今回選ぶ項目

- 対象: `T20`, `T21`, `T22`, `T23`, `T36`, `T37`, `T43`
- 選ぶ理由: 隔離保証の核。これが無いと `secret.db` に書き込まれない、または逆に `egopulse.db` に漏れる。`ToolExecutionContext.is_secret` は Step 13 の前提
- この時点では扱わないこと: Discord/Telegram からの `is_secret` 伝播（Step 8, 9）、Channel Log 保存のルーティング（Step 8, 9）、ツール内での DB 切替実装（Step 13）

### RED: 失敗する自動テストを書く

- 追加するテスト名（`src/agent_loop/turn.rs` のテストモジュール、または統合テスト）:
  - `process_turn_secret_creates_chat_in_secret_db`（T20）
  - `process_turn_normal_creates_chat_in_normal_db`（T21）
  - `process_turn_secret_stores_message_in_secret_db`（T22）
  - `process_turn_secret_does_not_touch_normal_db`（T23）
  - `process_turn_secret_logs_llm_usage_to_secret_db`（T36）
  - `process_turn_secret_skips_tool_call_persistence`（T37）
  - `tool_execution_context_propagates_is_secret_from_surface_context`（T43）
- Given: テスト用 AppState（両 DB 初期化済み）、`SurfaceContext { is_secret: true/false, ... }`
- When: `process_turn(state, ctx, user_msg)` を mock LLM で実行（tool 実行を含むケースも）
- Then:
  - is_secret=true → `secret_db` 側に chat/message/session/llm_usage_log レコード
  - is_secret=true → `egopulse.db` 側は 0 件（chat/message/session/tool_calls/llm_usage_logs すべて）
  - is_secret=true → tool 実行を含むターンでもエラーにならない（`tool_calls` INSERT スキップ）
  - is_secret=false → `egopulse.db` 側にレコード（既存挙動）
  - tool 実行時に渡される `ToolExecutionContext.is_secret` が `SurfaceContext.is_secret` と一致（T43）
- 失敗理由の想定: `process_turn` が `state.db` を直接参照、`tool_calls` テーブルが `secret.db` に無いため INSERT でエラー、`ToolExecutionContext` に `is_secret` フィールド無し

### GREEN: 最小実装

`src/agent_loop/turn.rs` の `process_turn_inner()` で DB 参照を全て `state.db_for(ctx.is_secret)` 経由に:

```rust
let db = state.db_for(ctx.is_secret);
let chat_id = db.resolve_or_create_chat_id(...)?;
let snapshot = db.load_session_snapshot(chat_id)?;
// ...
db.store_message_with_session(...)?;
```

`agent_loop/tool_phase.rs` の `log_llm_usage()` は `state.db_for(is_secret)` を使うよう引数か内部で切替:

```rust
pub(crate) fn log_llm_usage(state: &AppState, is_secret: bool, ...) {
    let db = Arc::clone(state.db_for(is_secret));
    // ...
}
```

`store_pending_tool_call` / `update_tool_call_output` は `is_secret == true` のとき早期 return:

```rust
async fn store_pending_tool_call(state: &AppState, is_secret: bool, ...) -> Result<()> {
    if is_secret {
        return Ok(());  // secret.db には tool_calls テーブルが無いためスキップ
    }
    // 既存の INSERT 処理
}
```

`src/tools/mod.rs` の `ToolExecutionContext` に `pub is_secret: bool` を追加し、`agent_loop/turn.rs` / `agent_loop/tool_phase.rs` で `ToolExecutionContext` を構築する箇所で `SurfaceContext.is_secret` を渡す:

```rust
// src/tools/mod.rs
pub(crate) struct ToolExecutionContext {
    pub chat_id: i64,
    pub channel: String,
    // ... 既存フィールド ...
    pub is_secret: bool,  // 新設
}

// agent_loop 側で構築:
let tool_ctx = ToolExecutionContext {
    chat_id,
    channel: ctx.channel.clone(),
    // ... 既存 ...
    is_secret: ctx.is_secret,  // SurfaceContext から伝播
};
```

`agent_loop/session.rs`, `agent_loop/compaction.rs` も同様に `&Database` 引数を受け取る形へ調整。

### REFACTOR: 設計の整理

- 重複: `db` 変数を関数先頭で捕まえ、以降はこれを参照。重複アクセス回避
- 命名: `db` で十分。`db_for` は helper 名
- 責務: agent_loop は DB 選択を `is_secret` に委任。DB 内部は知らない
- テストの構造的結合: mock LLM で実行し、両 DB のレコード有無を検証

### テストリスト更新

- 完了: `T20`, `T21`, `T22`, `T23`, `T36`, `T37`, `T43`
- 追加: なし
- 次候補: `T24`, `T25`, `T38`（Discord）

### コミット

`feat: route agent_loop DB access via db_for(is_secret)`

---

## Step 8: Discord Channel TDD Cycle - `is_secret` 伝播

### この Step の目的

Discord メッセージ受信時に、そのチャネル ID の config が `secret: true` なら `SurfaceContext.is_secret = true` を立てる。**`make_context()` ヘルパーを修正することで、slash command 処理経路（`process_text_slash_command` / interaction ハンドラ）にも自動的に伝播**させる。

### 今回選ぶ項目

- 対象: `T24`, `T25`, `T38`, `T46`
- 選ぶ理由: Discord は Phase 1 の主要侵入経路。`make_context` 修正により slash command 経路も一括でカバー（Step 14 の前提）
- この時点では扱わないこと: Multi-Agent Room 以外の Channel Log 処理（Step 9 以降で対応）

### RED: 失敗する自動テストを書く

- 追加するテスト名（`src/channels/discord.rs` のテストモジュール）:
  - `discord_sets_is_secret_when_channel_marked_secret`（T24）
  - `discord_sets_is_secret_false_for_normal_channel`（T25）
  - `discord_secret_channel_log_saved_to_secret_db`（T38）
  - `make_context_propagates_secret_flag_for_slash_command_paths`（T46）
- Given: テスト用 Config（特定 channel ID に `secret: true`）、受信メッセージイベント
- When: メッセージ受信 → `make_context()` 経由で SurfaceContext 構築、Multi-Agent Room では `store_human_channel_log_message` 実行
- Then:
  - `ctx.is_secret == config.channels[<id>].secret`（T24, T25）
  - secret チャネルの Channel Log 保存が `secret.db.messages` に書き込まれ、`egopulse.db.messages` には増えない（T38）
  - slash command 処理で呼ばれる `make_context` も同じく `is_secret` を設定する（T46）
- 失敗理由の想定: `make_context` が `secret` フラグを見ていない、`store_human_channel_log_message` が常に `app_state.db` を使う

### GREEN: 最小実装

`src/channels/discord.rs` の `make_context()` ヘルパーを修正。`thread`（channel_id 文字列）からチャネルを lookup し、`DiscordChannelConfig.secret` を `SurfaceContext.is_secret` に設定:

```rust
fn make_context(&self, user: &str, thread: &str, agent_id: &str) -> SurfaceContext {
    let is_secret = thread.parse::<u64>()
        .ok()
        .and_then(|cid| self.channels.get(&cid))
        .is_some_and(|c| c.secret);
    let mut ctx = SurfaceContext::new(...);
    ctx.is_secret = is_secret;
    ctx
}
```

この修正により、`make_context` を経由するすべての SurfaceContext 構築（メインメッセージハンドラ、`process_text_slash_command`、interaction handler 等）が自動的に正しい `is_secret` を持つ。Step 14 の slash command handler 修正が機能する前提となる。

`store_human_channel_log_message` の `Arc::clone(&self.app_state.db)` 部分も、チャネルの `secret` フラグに基づき `state.db_for(is_secret)` 経由で参照を取得するよう書換:

```rust
let is_secret = self.channels.get(&channel_id).is_some_and(|c| c.secret);
let db = self.app_state.db_for(is_secret).clone();
// resolve_channel_log_chat_id, INSERT INTO messages もこの db を使用
```

### REFACTOR: 設計の整理

- 重複: channel lookup ロジックは既存。`secret` フラグ読込を追加するだけ
- 命名: 一貫
- 責務: Discord channel は受信とルーティングのみ。DB 選択もここで完結
- テストの構造的結合: 実際のメッセージ処理より `SurfaceContext` 構築部分を抽出して検証

### テストリスト更新

- 完了: `T24`, `T25`, `T38`, `T46`
- 追加: なし
- 次候補: `T26`, `T27`, `T39`, `T46`（Telegram）

### コミット

`feat: propagate secret flag in Discord including Channel Log routing`

---

## Step 9: Telegram Channel TDD Cycle - `is_secret` 伝播

### この Step の目的

Telegram メッセージ受信時に、その chat ID の config が `secret: true` なら `SurfaceContext.is_secret = true` を立てる。**`make_context()` ヘルパーを修正**することで slash command 経路にも自動伝播。Channel Log 保存も Discord と同様に DB 切替。

### 今回選ぶ項目

- 対象: `T26`, `T27`, `T39`, `T46`
- 選ぶ理由: Discord と対称性確保。`make_context` 修正が Step 14 の前提
- この時点では扱わないこと: その他チャネル（CLI/Web/TUI は Phase 1 対象外）

### RED: 失敗する自動テストを書く

- 追加するテスト名（`src/channels/telegram.rs` のテストモジュール）:
  - `telegram_sets_is_secret_when_chat_marked_secret`（T26）
  - `telegram_sets_is_secret_false_for_normal_chat`（T27）
  - `telegram_secret_channel_log_saved_to_secret_db`（T39）
  - `telegram_make_context_propagates_secret_flag`（T46）
- Given: テスト用 Config（特定 chat ID に `secret: true`）、受信メッセージイベント
- When: メッセージ受信 → `make_context()` 経由で SurfaceContext 構築、Channel Log 保存
- Then:
  - `ctx.is_secret == config.telegram_channels[<id>].secret`（T26, T27）
  - secret chat の Channel Log 保存が `secret.db` 側（T39）
  - slash command 経由の `make_context` 呼出も同じく `is_secret` を設定（T46）

### GREEN: 最小実装

`src/channels/telegram.rs` の `make_context()` を Discord と同様に修正し、チャネル lookup から `secret` フラグを取得して `SurfaceContext.is_secret` に設定。`store_human_channel_log_message` の `Arc::clone(&self.app_state.db)` 部分も `db_for(is_secret)` 経由に切替。

### REFACTOR: 設計の整理

- Discord と対称。特記事項なし

### テストリスト更新

- 完了: `T26`, `T27`, `T39`, `T46`
- 追加: なし
- 次候補: `T28`, `T29`, `T48`（Compaction archive）

### コミット

`feat: propagate secret flag from Telegram chat config to SurfaceContext`

---

## Step 10: Compaction Archive TDD Cycle - `secret_groups/` 出力

### この Step の目的

Compaction 時の archive 出力先を `is_secret == true` なら `runtime/secret_groups/...` に切替える。加えて、**Compaction 中の LLM 呼び出しに対する usage log 挿入（`compaction.rs:353` 周辺）も `db_for(is_secret)` 経由に切替**（`tool_phase.rs::log_llm_usage` とは別経路のため Step 7 ではカバーされていない）。

### 今回選ぶ項目

- 対象: `T28`, `T29`, `T48`
- 選ぶ理由: audit artifact の漏洩防止に加え、compaction 中の LLM usage log も `egopulse.db` へ漏れないようにする
- この時点では扱わないこと: archive の暗号化、有期限保持

### RED: 失敗する自動テストを書く

- 追加するテスト名（`src/agent_loop/compaction.rs` のテストモジュール）:
  - `archive_path_uses_secret_groups_when_is_secret`（T28）
  - `archive_path_uses_normal_groups_when_not_secret`（T29）
  - `compaction_llm_usage_log_routes_to_secret_db_in_secret_mode`（T48）
- Given: tmp state_root、`SurfaceContext { is_secret: true/false, ... }`、compaction 発火条件
- When: compaction 実行 → archive ファイル出力、LLM 呼び出し発生
- Then:
  - is_secret=true → `runtime/secret_groups/<channel>/<chat_id>/conversations/*.md` が作られる（T28）
  - is_secret=false → `runtime/groups/<channel>/<chat_id>/conversations/*.md`（既存）（T29）
  - is_secret=true → compaction の LLM usage log が `secret.db.llm_usage_logs` に格納され、`egopulse.db.llm_usage_logs` は増えない（T48）
- 失敗理由の想定: archive path が `groups/` 固定、`compaction.rs:353` の `state.db` が直参照

### GREEN: 最小実装

archive path 構築関数（`archive_conversation_blocking` 等の内部）で、`is_secret` 引数を追加し、`groups` or `secret_groups` を切替える。

加えて `compaction.rs:353` 周辺の LLM usage log 挿入処理も `state.db_for(context.is_secret)` 経由に切替:

```rust
let db = Arc::clone(state.db_for(context.is_secret));
// 以降の call_blocking(db, ...) で llm_usage_logs へ INSERT
```

### REFACTOR: 設計の整理

- 重複: パス構築ロジックは1関数に集約
- 命名: `secret_groups` で `groups` と対称
- 責務: compaction モジュールは archive 出力の振分だけ。内容は変わらない
- テストの構造的結合: 実際のファイル存在で検証

### テストリスト更新

- 完了: `T28`, `T29`, `T48`
- 追加: なし
- 次候補: `T30`, `T31`（Backup）

### コミット

`feat: route secret chat compaction archives to secret_groups`

---

## Step 11: Backup TDD Cycle - `secret.db` バックアップ

### この Step の目的

起動時バックアップ・定期バックアップが `secret.db` 存在時に `secret-YYYYMMDD-HHMMSS.db` を生成するようにする。

### 今回選ぶ項目

- 対象: `T30`, `T31`
- 選ぶ理由: 障害復旧可能性の確保
- この時点では扱わないこと: バックアップ暗号化、世代管理の個別設定

### RED: 失敗する自動テストを書く

- 追加するテスト名（`src/storage/backup.rs` のテストモジュール）:
  - `backup_creates_secret_db_snapshot_when_present`
  - `backup_skips_secret_db_when_not_present`
- Given: tmp ディレクトリ、`AppState` with `secret_db: Some/None`
- When: バックアップ実行
- Then:
  - secret_db あり → `secret-YYYYMMDD-HHMMSS.db` ファイル生成
  - secret_db なし → ファイル生成されない
- 失敗理由の想定: backup 関数が `egopulse.db` だけを対象にしている

### GREEN: 最小実装

`src/storage/backup.rs` の `run_backup()` または同等関数に `secret_db: Option<&Database>` 引数を追加。存在時のみ VACUUM INTO を実行。世代管理（max_generations 適用）も `secret-*.db` 系で独立カウント。

`src/runtime/backup_scheduler.rs` から `AppState.secret_db` を渡すよう調整。

### REFACTOR: 設定の整理

- 重複: VACUUM INTO と世代管理ロジックは既存。DB 参照を引数で取るように共通化
- 命名: `secret-YYYYMMDD-HHMMSS.db`（`egopulse-*.db` と prefix 以外は同形式）
- 責務: backup モジュールは「渡された DB をバックアップする」。AppState の状態は知らなくて良い
- テストの構造的結合: ファイル存在で検証

### テストリスト更新

- 完了: `T30`, `T31`
- 追加: なし
- 次候補: `T32`, `T33`（Logging）

### コミット

`feat: backup secret.db alongside egopulse.db`

---

## Step 12: Logging TDD Cycle - secret ターンの content field マスク

### この Step の目的

`is_secret == true` の span では `user_msg` 等の content フィールドを span に含めない。

### 今回選ぶ項目

- 対象: `T32`, `T33`
- 選ぶ理由: ログ経由の漏洩防止
- この時点では扱わないこと: より細かなフィールド単位制御（LLM request body 等）は必要に応じて後で追加

### RED: 失敗する自動テストを書く

- 追加するテスト名（`src/agent_loop/mod.rs` または `src/agent_loop/turn.rs` のテストモジュール）:
  - `secret_turn_span_omits_user_msg_field`
  - `normal_turn_span_includes_user_msg_field`
- Given: テスト用 tracing subscriber（span fields を capture）、`SurfaceContext { is_secret: true/false }`
- When: `process_turn` 実行
- Then:
  - is_secret=true → span に `user_msg` field なし、`is_secret = true` はあり
  - is_secret=false → `user_msg` field あり（regression）
- 失敗理由の想定: span 構築が常に `user_msg` を含める

### GREEN: 最小実装

`info_span!` 等のマクロ呼出箇所で条件分岐:

```rust
let span = if ctx.is_secret {
    info_span!("turn", agent_id = %ctx.agent_id, is_secret = true)
} else {
    info_span!("turn", agent_id = %ctx.agent_id, is_secret = false, user_msg = %msg)
};
```

tool 実行ログ・LLM request ログも同様に `is_secret` を見て content field を省略。

### REFACTOR: 設計の整理

- 重複: 条件分岐を関数に切り出しても良い（`make_turn_span(ctx, msg)`）
- 命名: `is_secret` field 名で統一
- 責務: span 構築は agent_loop 内。subscriber 側での加工はしない
- テストの構造的結合: 実際の tracing 出力を検証

### テストリスト更新

- 完了: `T32`, `T33`
- 追加: なし
- 次候補: `T34`（agent_send propagation）

### コミット

`feat: redact content fields in tracing spans for secret turns`

---

## Step 13: agent_send / SendMessage ツール TDD Cycle - is_secret 継承と DB ルーティング

### この Step の目的

`agent_send` で発生する宛先 agent の turn が、送信元の `is_secret` を継承することを保証する。加えて、`AgentSendTool` / `SendMessageTool` 内部の DB アクセス（Channel Log 保存、chat info 参照）が `context.is_secret` に基づき適切に `secret_db` に切替わることを保証する。**さらに `runtime/mod.rs` の `execute_scheduled_turn` 内 `store_system_event`（停止条件発火時のシステムイベント記録）も `db_for(turn.context.is_secret)` 経由に切替る**。

### 今回選ぶ項目

- 対象: `T34`, `T40`, `T41`, `T47`
- 選ぶ理由: Multi-Agent Room での秘密モード一貫性、ツール経由の漏洩防止、停止条件発火時のシステムイベント漏洩防止
- この時点では扱わないこと: クロスモード send の明示的拒否（構造的に発生しないので不要）

### RED: 失敗する自動テストを書く

- 追加するテスト名:
  - `agent_send_inherits_is_secret_to_recipient_turn`（T34, `src/runtime/turn_scheduler.rs` または `src/tools/agent_send.rs`）
  - `agent_send_writes_channel_log_to_secret_db_in_secret_mode`（T40, `src/tools/agent_send.rs`）
  - `agent_send_tool_selects_db_by_context_is_secret`（T41, `src/tools/agent_send.rs`）
  - `stop_condition_store_system_event_routes_to_secret_db`（T47, `src/runtime/mod.rs`）
- Given: 秘密 Discord チャネル（`secret: true`）、alice と bob 両 agent 配置、alice が `agent_send(to=bob)` 実行。停止条件発火（chain depth 超過等）をトリガするケースも
- When: agent_send 実行 → Channel Log 保存 → bob の turn schedule、停止条件発火 → `store_system_event` 実行
- Then:
  - bob の `SurfaceContext.is_secret == true`（T34）
  - Channel Log 保存（`store_message_only`）が `secret.db.messages` に書き込まれ、`egopulse.db.messages` は増えない（T40）
  - `lookup_chat_info` 等の読込も含め、`AgentSendTool` の全 DB アクセスが `context.is_secret` で切替わる（T41）
  - 停止条件発火時の `store_system_event` が `secret.db.messages` に書き込まれ、`egopulse.db.messages` は増えない（T47）
- 失敗理由の想定: `PendingAgentTurn` が `is_secret` を保持していない、`AgentSendTool` が `secret_db` を持たない、`store_system_event` が `state.db` を直参照

### GREEN: 最小実装

1. **`PendingAgentTurn` 拡張**: `is_secret: bool` フィールド追加。`execute_scheduled_turn()` で `SurfaceContext` 構築時に設定。

2. **`AgentSendTool` / `SendMessageTool` のコンストラクタ拡張**:

```rust
// src/tools/agent_send.rs
pub(crate) struct AgentSendTool {
    agents: Arc<HashMap<AgentId, AgentConfig>>,
    db: Arc<Database>,
    secret_db: Option<Arc<Database>>,  // 新設
    channels: Arc<ChannelRegistry>,
}

fn db_for(&self, is_secret: bool) -> &Database {
    if is_secret {
        self.secret_db.as_ref().expect("secret db required for secret mode turn")
    } else {
        &self.db
    }
}
```

3. **`runtime/mod.rs` の ToolRegistry 構築を更新**:

```rust
tools.register_tool(Box::new(crate::tools::AgentSendTool::new(
    config.agents.clone(),
    Arc::clone(&deps.db),
    deps.secret_db.clone(),  // 新設
    Arc::clone(&channels),
)));
// SendMessageTool も同様
```

4. **`agent_send.rs` の実行部**: `Arc::clone(&self.db)` を `Arc::clone(self.db_for(context.is_secret))` に切替。

5. **`runtime/mod.rs::execute_scheduled_turn` の `store_system_event`**:

```rust
// 変更前
if let Err(error) = state.db.store_system_event(log_chat_id, &reason) { ... }

// 変更後
let db = state.db_for(turn.context.is_secret);
if let Err(error) = db.store_system_event(log_chat_id, &reason) { ... }
```

`store_system_event` は `&self` on Database を取る既存メソッドなので、参照元を切替えるだけで対応可能。

### REFACTOR: 設計の整理

- 重複: `db_for` ヘルパーは AppState のものと同じシグネチャでツール内にも持つ
- 命名: `is_secret` で一貫
- 責務: scheduler はプロパゲーション、ツールは実行時 DB 選択
- テストの構造的結合: 実際の agent_send 実行で `is_secret` と DB bookkeeping を検証

### テストリスト更新

- 完了: `T34`, `T40`, `T41`, `T47`
- 追加: なし
- 次候補: `T44`, `T45`（Slash commands）

### コミット

`feat: route agent_send and SendMessage tools through db_for(is_secret)`

---

## Step 14: Slash Commands TDD Cycle - `db_for(ctx.is_secret)` 経由 DB 切替

### この Step の目的

Discord / Telegram の secret チャネルで `/new` / `/compact` / `/status` 等の slash command が実行されたとき、`process_slash_command` から呼ばれる各 handler（`handle_new`, `handle_compact`, `handle_status` 等）が `state.db_for(ctx.is_secret)` 経由で DB を切替えるようにする。現状 `src/slash_commands.rs` は `state.db` を直接参照しており、secret チャネルで実行すると `egopulse.db` に漏れる。

### 今回選ぶ項目

- 対象: `T44`, `T45`
- 選ぶ理由: Phase 1 サポートする Discord / Telegram のユーザがごく自然に使い得る経路。放置すると `/new` 等の routine 操作で隔離が破れる
- この時点では扱わないこと: slash command 以外の経路（前 Step までで完備）

### RED: 失敗する自動テストを書く

- 追加するテスト名（`src/slash_commands.rs` のテストモジュール）:
  - `slash_new_in_secret_channel_clears_secret_db_session_only`（T44）
  - `slash_compact_and_status_in_secret_channel_use_secret_db`（T45）
- Given: テスト用 AppState（両 DB 初期化済み）、`SurfaceContext { is_secret: true, ... }`、`chat_id` は `secret.db` 側に存在
- When: `process_slash_command(state, ctx, "/new", None)` 等を実行
- Then:
  - `/new` → `secret.db.sessions` の該当行がクリアされ、`egopulse.db.sessions` は無変更
  - `/compact` → `secret.db` の message 群から compaction され、`secret.db.sessions` が更新。`egopulse.db` 側は無変更
  - `/status` → `secret.db.messages` を読み込み、`egopulse.db.messages` を読みにいかない
- 失敗理由の想定: 各 handler が `Arc::clone(&state.db)` を直接使っており、`is_secret` を見ない

### GREEN: 最小実装

`src/slash_commands.rs` の各 handler で `state.db_for(context.is_secret)` 経由に切替:

```rust
async fn handle_new(state: &AppState, is_secret: bool, chat_id: i64) -> Option<String> {
    let db = Arc::clone(state.db_for(is_secret));
    match call_blocking(db, move |db| { /* ... */ }).await { /* ... */ }
}

async fn handle_compact(
    state: &AppState,
    context: &SurfaceContext,
    chat_id: i64,
) -> Option<String> {
    let db = Arc::clone(state.db_for(context.is_secret));
    // ... load_messages_for_turn も db_for 経由に
}

async fn handle_status(
    state: &AppState,
    context: &SurfaceContext,
    chat_id: i64,
    sender_id: Option<&str>,
) -> Option<String> {
    let db = Arc::clone(state.db_for(context.is_secret));
    // ...
}
```

`handle_slash_command` の dispatcher も `context.is_secret` を各 handler へ伝播するようシグネチャ調整。`process_slash_command` は `SurfaceContext` を受け取る現状を活かし、内部で `context.is_secret` を参照。

### REFACTOR: 設計の整理

- 重複: `db_for(is_secret)` の呼び出しは各 handler の先頭で1回に統一
- 命名: 既存 handler 名を維持
- 責務: `slash_commands.rs` はコマンド振り分けとDB選択。各 handler は DB 内部を知らない
- テストの構造的結合: 各 handler の戻り値と両 DB の row count で検証

### テストリスト更新

- 完了: `T44`, `T45`
- 追加: なし
- 次候補: `T42`（包括検証）

### コミット

`feat: route slash command DB access via db_for(is_secret)`

---

## Step 15: 包括的 DB 隔離検証 TDD Cycle - egopulse.db 不変保証

### この Step の目的

秘密ターンを一通り実行した後、`egopulse.db` の**全テーブル**（`chats` / `messages` / `sessions` / `tool_calls` / `llm_usage_logs`）が1件も増えていないことを検証する統合テストを追加する。Codex review #2 で指摘された「ルーティング漏れの最終防衛」。

### 今回選ぶ項目

- 対象: `T42`
- 選ぶ理由: 個別ルーティングテスト（T20, T22, T23, T36-T45）は各経路を検証するが、新規経路の追加や見落としを最終的に検出するための包括的テスト
- この時点では扱わないこと: 各経路の詳細（前 Step までで完備）

### RED: 失敗する自動テストを書く

- 追加するテスト名（統合テストモジュール、`tests/` または `src/integration/` 等に新設）:
  - `secret_turn_leaves_egopulse_db_untouched`
- Given: 空の `egopulse.db` と `secret.db`、秘密モード設定の AppState、tool 実行を含む user message、加えて `/new` / `/compact` / `/status` 等の slash command 実行
- When: 1ターン実行（user 発言 → tool 実行 → assistant 応答）、必要に応じて slash command も実行
- Then:
  - `egopulse.db.chats.count() == 0`
  - `egopulse.db.messages.count() == 0`
  - `egopulse.db.sessions.count() == 0`
  - `egopulse.db.tool_calls.count() == 0`
  - `egopulse.db.llm_usage_logs.count() == 0`
  - `secret.db` 側には各テーブルにレコードが増えている
- 失敗理由の想定: 何らかの経路で `state.db` への書込が残っている

### GREEN: 最小実装

特になし（前 Step まででルーティングが完了している前提）。テストが失敗した場合は該当経路を特定して Step 7-14 へ戻る。

### REFACTOR: 設計の整理

- 重複: なし
- 命名: `secret_turn_leaves_egopulse_db_untouched` で意図明示
- 責務: 統合テスト。内部構造に依存しない
- テストの構造的結合: テーブル row count のみで検証

### テストリスト更新

- 完了: `T42`
- 追加: なし
- 次候補: なし（テストリスト完）

### コミット

`test: add comprehensive DB isolation assertion for secret turns`

---

## Step 16: 動作確認

- `cargo fmt --check`
- `cargo test`
- `cargo check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
- 失敗時に戻る Step: 該当ステップへ戻り修正

### 手動確認（オプション）

- `cargo run -- setup` で通常セットアップ後、config に `secret: true` Discord チャネルを追加して起動
- `secret.db` が `~/.egopulse/runtime/` に作られることを確認
- 該当チャネルで発言 → `secret.db` 側に message レコードが増えることを SQL で確認
- 別チャネル（通常）で発言 → `egopulse.db` 側のみ増えることを確認
- **追加**: 該当チャネルで tool 実行を伴う発言（例: `bash echo` 等）をした後、`egopulse.db.tool_calls` と `egopulse.db.llm_usage_logs` を SQL で確認し、**secret チャネルの分が混入していないこと**を確認

---

## Step 17: Plan・仕様書との自己チェック

実装完了後にこの Plan と [docs/plan/secret-mode-design.md](./secret-mode-design.md) を最初から読み直し、実装・自動テスト・文書が要求した振る舞いと一致しているかを照合する。不整合があれば該当 Step へ戻る。

チェック項目:

- Plan のテストリスト（T1〜T42）が全て対応 Cycle を持つか
- design doc §3〜§7 のアーキテクチャ・コンポーネント改修が漏れなく実装されているか
- design doc §6.3 の DB アクセス表（chat / session / message / llm_usage / tool_call skip）が全て実装されているか
- design doc §6.4 の Channel Log ルーティング（Discord / Telegram）が実装されているか
- design doc §6.9 の `SendMessageTool` / `AgentSendTool` の `secret_db` 注入・切替が実装されているか
- design doc §9 隔離保証が全てテストで担保されているか
- design doc §6.6 system prompt の SECRET.md 注入順序が正しいか
- 変更ファイル一覧・コミット分割・自動テスト一覧が実際の変更と一致しているか
- 関連 docs（db.md / config.md / architecture.md / channels.md / session-lifecycle.md / system-prompt.md）が更新対象に含まれているか

---

## Step 18: ドキュメント更新

以下の docs を更新する:

- `docs/db.md`: `secret.db` のスキーマ・6テーブル構成・`SECRET_SCHEMA_VERSION` を追記
- `docs/config.md`: `channels.discord.channels.<id>.secret`, `channels.telegram.telegram_channels.<id>.secret` フィールド説明
- `docs/architecture.md`: AppState 構造変更（`secret_db`）、`db_for()` router、secret mode のライフサイクル追記
- `docs/channels.md`: Discord / Telegram での `secret: true` 挙動説明
- `docs/session-lifecycle.md`: `secret.db` 側の session 永続化、`is_secret` ルーティング追記
- `docs/system-prompt.md`: SECRET.md 注入順序（AGENTS と Memory の間）、`<secret>` block 形式
- `docs/security.md`: 秘密モードの脅威モデル・隔離戦略追記（必要に応じて）

各 doc は既存スタイル（説明 + テーブル + コード例）に合わせて記載する。

### コミット

`docs: update database, config, architecture, channels, session, system-prompt docs for secret mode`

---

## Step 19: PR 作成

- PR タイトル: `feat: add secret mode (Phase 1)`
- PR description:
  - 概要: 秘匿会話を `secret.db` 別ファイルに隔離する秘密モードの Phase 1 実装
  - 設計 doc: `docs/plan/secret-mode-design.md` へのリンク
  - 実装 Plan: `docs/plan/plan-secret-mode-phase1.md` へのリンク
  - テスト: T1〜T42 の自動テスト一覧
  - Close #<issue-number>（該当する場合）
  - スクリーンショット・動作確認結果（オプション）

PR description は日本語で記載（プロジェクト規約に従う）。

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
| ---- | ---- | -- |
| `src/config/types.rs` | 変更 | `DiscordChannelConfig.secret`, `TelegramChatConfig.secret` フィールド追加 |
| `src/config/loader.rs` | 変更 | `secret` フラグ読込・バリデーション |
| `src/config/tests.rs` | 変更 | Step 1 のテスト追加 |
| `src/storage/migration.rs` | 変更 | `run_secret_migrations()`, `SECRET_SCHEMA_VERSION` 新設、対応テスト追加 |
| `src/storage/mod.rs` | 変更 | `Database::new_secret()` 新設 |
| `src/storage/backup.rs` | 変更 | `secret.db` バックアップ対応、テスト追加 |
| `src/runtime/mod.rs` | 変更 | `AppState.secret_db`, `db_for()`, `secret_enabled()`, ビルダ拡張、`store_system_event` の DB ルーティング、テスト追加 |
| `src/runtime/backup_scheduler.rs` | 変更 | `secret_db` を backup へ渡す |
| `src/agent_loop/mod.rs` | 変更 | `SurfaceContext.is_secret` 追加、span 構築の条件分岐、テスト追加 |
| `src/agent_loop/soul_agents.rs` | 変更 | `load_secret()` 新設、テスト追加 |
| `src/agent_loop/prompt_builder.rs` | 変更 | `build_secret_prompt_section()` 新設、順序調整、テスト追加 |
| `src/agent_loop/turn.rs` | 変更 | `db_for(ctx.is_secret)` 経由 DB 切替、`ToolExecutionContext` 構築時に `is_secret` を伝播、テスト追加 |
| `src/agent_loop/session.rs` | 変更 | `&Database` 引数を取るように調整（既存シグネチャ依存） |
| `src/agent_loop/compaction.rs` | 変更 | archive path の `secret_groups/` 振分、LLM usage log 挿入の DB ルーティング、テスト追加 |
| `src/tools/mod.rs` | 変更 | `ToolExecutionContext.is_secret` フィールド追加 |
| `src/channels/discord.rs` | 変更 | `make_context()` がチャネルの `secret` フラグを参照して `is_secret` 設定、Channel Log の DB ルーティング、テスト追加 |
| `src/channels/telegram.rs` | 変更 | `make_context()` 同上、Channel Log の DB ルーティング、テスト追加 |
| `src/runtime/turn_scheduler.rs` | 変更 | `PendingAgentTurn.is_secret` 追加、テスト追加 |
| `src/tools/agent_send.rs` | 変更 | `secret_db` フィールド追加、`context.is_secret` による DB 切替、テスト追加 |
| `src/tools/send_message.rs` | 変更 | `secret_db` フィールド追加、`context.is_secret` による DB 切替（DB アクセスがある場合） |
| `src/agent_loop/tool_phase.rs` | 変更 | `log_llm_usage` が `db_for(is_secret)` 経由で DB 切替 |
| `src/slash_commands.rs` | 変更 | `handle_new` / `handle_compact` / `handle_status` 等が `db_for(context.is_secret)` 経由で DB 切替、テスト追加 |
| `tests/secret_db_isolation.rs` | **新規** | Step 15 の包括的 DB 隔離テスト |
| `docs/db.md` | 変更 | `secret.db` スキーマ説明追記 |
| `docs/config.md` | 変更 | `secret` フラグ説明追記 |
| `docs/architecture.md` | 変更 | AppState・secret mode アーキテクチャ追記 |
| `docs/channels.md` | 変更 | Discord / Telegram secret 挙動追記 |
| `docs/session-lifecycle.md` | 変更 | `secret.db` ルーティング追記 |
| `docs/system-prompt.md` | 変更 | SECRET.md 注入順序追記 |
| `docs/security.md` | 変更 | 脅威モデル・隔離戦略追記（必要に応じて） |

---

## コミット分割

1. `feat: add secret flag to discord and telegram channel config` - `src/config/types.rs`, `src/config/loader.rs`, `src/config/tests.rs`
2. `feat: add secret.db migrations and Database::new_secret` - `src/storage/migration.rs`, `src/storage/mod.rs`
3. `feat: wire secret_db into AppState with db_for router` - `src/runtime/mod.rs`
4. `refactor: add is_secret field to SurfaceContext with false default` - `src/agent_loop/mod.rs`
5. `feat: load SECRET.md via SoulAgentsLoader` - `src/agent_loop/soul_agents.rs`
6. `feat: inject SECRET.md into system prompt between AGENTS and Memory` - `src/agent_loop/prompt_builder.rs`
7. `feat: route agent_loop DB access via db_for(is_secret) including llm_usage, tool_call skip and ToolExecutionContext propagation` - `src/agent_loop/turn.rs`, `src/agent_loop/session.rs`, `src/agent_loop/tool_phase.rs`, `src/tools/mod.rs`
8. `feat: propagate secret flag in Discord make_context including Channel Log routing` - `src/channels/discord.rs`
9. `feat: propagate secret flag in Telegram make_context including Channel Log routing` - `src/channels/telegram.rs`
10. `feat: route secret chat compaction archives to secret_groups and compaction LLM usage to db_for(is_secret)` - `src/agent_loop/compaction.rs`
11. `feat: backup secret.db alongside egopulse.db` - `src/storage/backup.rs`, `src/runtime/backup_scheduler.rs`
12. `feat: redact content fields in tracing spans for secret turns` - `src/agent_loop/mod.rs`, `src/agent_loop/turn.rs`
13. `feat: route agent_send, SendMessage tools and stop-condition store_system_event through db_for(is_secret)` - `src/runtime/turn_scheduler.rs`, `src/tools/agent_send.rs`, `src/tools/send_message.rs`, `src/runtime/mod.rs`
14. `feat: route slash command DB access via db_for(is_secret)` - `src/slash_commands.rs`
15. `test: add comprehensive DB isolation assertion for secret turns` - `tests/secret_db_isolation.rs`
16. `docs: update database, config, architecture, channels, session, system-prompt docs for secret mode` - 各 docs/

---

## 自動テスト一覧（全 48 件）

この一覧は Plan 作成時点で必要と判断した最低限の予定であり、最終テスト件数の上限ではない。実装中に追加された不安には、テストリストと TDD Cycle を追加して対応する。

### Config（全 3 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T1 | `discord_channel_config_parses_secret_flag` | Step 1 | `cargo test -p egopulse config::tests::discord_channel_config_parses_secret_flag` |
| T2 | `telegram_chat_config_parses_secret_flag` | Step 1 | `cargo test -p egopulse config::tests::telegram_chat_config_parses_secret_flag` |
| T3 | `channel_config_secret_defaults_to_false` | Step 1 | `cargo test -p egopulse config::tests::channel_config_secret_defaults_to_false` |

### Storage（全 3 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T4 | `new_secret_opens_wal_database` | Step 2 | `cargo test -p egopulse storage::migration::new_secret_opens_wal_database` |
| T5 | `run_secret_migrations_creates_expected_tables` | Step 2 | `cargo test -p egopulse storage::migration::run_secret_migrations_creates_expected_tables` |
| T6 | `run_secret_migrations_is_idempotent` | Step 2 | `cargo test -p egopulse storage::migration::run_secret_migrations_is_idempotent` |

### AppState / Runtime（全 6 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T7 | `db_for_returns_normal_db_when_not_secret` | Step 3 | `cargo test -p egopulse runtime::db_for_returns_normal_db_when_not_secret` |
| T8 | `db_for_returns_secret_db_when_secret` | Step 3 | `cargo test -p egopulse runtime::db_for_returns_secret_db_when_secret` |
| T9 | `db_for_panics_when_secret_db_uninitialized` | Step 3 | `cargo test -p egopulse runtime::db_for_panics_when_secret_db_uninitialized` |
| T10 | `secret_enabled_reflects_secret_db_presence` | Step 3 | `cargo test -p egopulse runtime::secret_enabled_reflects_secret_db_presence` |
| T11 | `app_state_initializes_secret_db_when_config_has_secret_entry` | Step 3 | `cargo test -p egopulse runtime::app_state_initializes_secret_db_when_config_has_secret_entry` |
| T12 | `app_state_does_not_initialize_secret_db_when_no_secret_entry` | Step 3 | `cargo test -p egopulse runtime::app_state_does_not_initialize_secret_db_when_no_secret_entry` |

### SurfaceContext（全 1 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T13 | `surface_context_supports_is_secret_field` | Step 4 | `cargo test -p egopulse agent_loop::surface_context_supports_is_secret_field` |

### SoulAgentsLoader（全 3 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T14 | `load_secret_returns_content_when_file_exists` | Step 5 | `cargo test -p egopulse agent_loop::soul_agents::load_secret_returns_content_when_file_exists` |
| T15 | `load_secret_returns_none_when_file_missing` | Step 5 | `cargo test -p egopulse agent_loop::soul_agents::load_secret_returns_none_when_file_missing` |
| T16 | `load_secret_rejects_unsafe_agent_id` | Step 5 | `cargo test -p egopulse agent_loop::soul_agents::load_secret_rejects_unsafe_agent_id` |

### System Prompt（全 3 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T17 | `system_prompt_includes_secret_md_when_is_secret` | Step 6 | `cargo test -p egopulse agent_loop::prompt_builder::system_prompt_includes_secret_md_when_is_secret` |
| T18 | `system_prompt_excludes_secret_md_when_not_secret` | Step 6 | `cargo test -p egopulse agent_loop::prompt_builder::system_prompt_excludes_secret_md_when_not_secret` |
| T19 | `secret_md_appears_between_agents_and_memory` | Step 6 | `cargo test -p egopulse agent_loop::prompt_builder::secret_md_appears_between_agents_and_memory` |

### Agent Loop（全 4 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T20 | `process_turn_secret_creates_chat_in_secret_db` | Step 7 | `cargo test -p egopulse agent_loop::process_turn_secret_creates_chat_in_secret_db` |
| T21 | `process_turn_normal_creates_chat_in_normal_db` | Step 7 | `cargo test -p egopulse agent_loop::process_turn_normal_creates_chat_in_normal_db` |
| T22 | `process_turn_secret_stores_message_in_secret_db` | Step 7 | `cargo test -p egopulse agent_loop::process_turn_secret_stores_message_in_secret_db` |
| T23 | `process_turn_secret_does_not_touch_normal_db` | Step 7 | `cargo test -p egopulse agent_loop::process_turn_secret_does_not_touch_normal_db` |

### Discord Channel（全 3 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T24 | `discord_sets_is_secret_when_channel_marked_secret` | Step 8 | `cargo test -p egopulse channels::discord::discord_sets_is_secret_when_channel_marked_secret` |
| T25 | `discord_sets_is_secret_false_for_normal_channel` | Step 8 | `cargo test -p egopulse channels::discord::discord_sets_is_secret_false_for_normal_channel` |
| T46 | `make_context_propagates_secret_flag_for_slash_command_paths` | Step 8 | `cargo test -p egopulse channels::discord::make_context_propagates_secret_flag_for_slash_command_paths` |

### Telegram Channel（全 3 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T26 | `telegram_sets_is_secret_when_chat_marked_secret` | Step 9 | `cargo test -p egopulse channels::telegram::telegram_sets_is_secret_when_chat_marked_secret` |
| T27 | `telegram_sets_is_secret_false_for_normal_chat` | Step 9 | `cargo test -p egopulse channels::telegram::telegram_sets_is_secret_false_for_normal_chat` |
| T46 | `telegram_make_context_propagates_secret_flag` | Step 9 | `cargo test -p egopulse channels::telegram::telegram_make_context_propagates_secret_flag` |

### Compaction Archive（全 3 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T28 | `archive_path_uses_secret_groups_when_is_secret` | Step 10 | `cargo test -p egopulse agent_loop::compaction::archive_path_uses_secret_groups_when_is_secret` |
| T29 | `archive_path_uses_normal_groups_when_not_secret` | Step 10 | `cargo test -p egopulse agent_loop::compaction::archive_path_uses_normal_groups_when_not_secret` |
| T48 | `compaction_llm_usage_log_routes_to_secret_db_in_secret_mode` | Step 10 | `cargo test -p egopulse agent_loop::compaction::compaction_llm_usage_log_routes_to_secret_db_in_secret_mode` |

### Backup（全 2 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T30 | `backup_creates_secret_db_snapshot_when_present` | Step 11 | `cargo test -p egopulse storage::backup::backup_creates_secret_db_snapshot_when_present` |
| T31 | `backup_skips_secret_db_when_not_present` | Step 11 | `cargo test -p egopulse storage::backup::backup_skips_secret_db_when_not_present` |

### Logging（全 2 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T32 | `secret_turn_span_omits_user_msg_field` | Step 12 | `cargo test -p egopulse agent_loop::secret_turn_span_omits_user_msg_field` |
| T33 | `normal_turn_span_includes_user_msg_field` | Step 12 | `cargo test -p egopulse agent_loop::normal_turn_span_includes_user_msg_field` |

### agent_send / SendMessage propagation（全 4 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T34 | `agent_send_inherits_is_secret_to_recipient_turn` | Step 13 | `cargo test -p egopulse runtime::turn_scheduler::agent_send_inherits_is_secret_to_recipient_turn` |
| T40 | `agent_send_writes_channel_log_to_secret_db_in_secret_mode` | Step 13 | `cargo test -p egopulse tools::agent_send::agent_send_writes_channel_log_to_secret_db_in_secret_mode` |
| T41 | `agent_send_tool_selects_db_by_context_is_secret` | Step 13 | `cargo test -p egopulse tools::agent_send::agent_send_tool_selects_db_by_context_is_secret` |
| T47 | `stop_condition_store_system_event_routes_to_secret_db` | Step 13 | `cargo test -p egopulse runtime::stop_condition_store_system_event_routes_to_secret_db` |

### Agent Loop 追加ルーティング（全 3 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T36 | `process_turn_secret_logs_llm_usage_to_secret_db` | Step 7 | `cargo test -p egopulse agent_loop::process_turn_secret_logs_llm_usage_to_secret_db` |
| T37 | `process_turn_secret_skips_tool_call_persistence` | Step 7 | `cargo test -p egopulse agent_loop::process_turn_secret_skips_tool_call_persistence` |
| T43 | `tool_execution_context_propagates_is_secret_from_surface_context` | Step 7 | `cargo test -p egopulse agent_loop::tool_execution_context_propagates_is_secret_from_surface_context` |

### Channel Log ルーティング（全 2 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T38 | `discord_secret_channel_log_saved_to_secret_db` | Step 8 | `cargo test -p egopulse channels::discord::discord_secret_channel_log_saved_to_secret_db` |
| T39 | `telegram_secret_channel_log_saved_to_secret_db` | Step 9 | `cargo test -p egopulse channels::telegram::telegram_secret_channel_log_saved_to_secret_db` |

### 包括的 DB 隔離検証（全 1 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T42 | `secret_turn_leaves_egopulse_db_untouched` | Step 15 | `cargo test -p egopulse --test secret_db_isolation` |

### Slash Commands ルーティング（全 2 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T44 | `slash_new_in_secret_channel_clears_secret_db_session_only` | Step 14 | `cargo test -p egopulse slash_commands::slash_new_in_secret_channel_clears_secret_db_session_only` |
| T45 | `slash_compact_and_status_in_secret_channel_use_secret_db` | Step 14 | `cargo test -p egopulse slash_commands::slash_compact_and_status_in_secret_channel_use_secret_db` |

---

## 工数見積もり

| Step | 内容 | 見積もり |
| -- | -- | -- |
| Step 1 | Config TDD Cycle | ~50 行（実装 10 + テスト 40） |
| Step 2 | Storage TDD Cycle | ~200 行（実装 100 + テスト 100） |
| Step 3 | AppState TDD Cycle | ~150 行（実装 60 + テスト 90） |
| Step 4 | SurfaceContext TDD Cycle | ~30 行（実装 5 + テスト 25） |
| Step 5 | SoulAgentsLoader TDD Cycle | ~80 行（実装 20 + テスト 60） |
| Step 6 | System Prompt TDD Cycle | ~120 行（実装 30 + テスト 90） |
| Step 7 | Agent Loop DB routing TDD Cycle（llm_usage・tool_call skip・ToolExecutionContext 拡張） | ~370 行（実装 130 + テスト 240） |
| Step 8 | Discord TDD Cycle（make_context 拡張・Channel Log routing 拡張） | ~180 行（実装 50 + テスト 130） |
| Step 9 | Telegram TDD Cycle（make_context 拡張・Channel Log routing 拡張） | ~160 行（実装 45 + テスト 115） |
| Step 10 | Compaction Archive TDD Cycle（archive path + LLM usage log） | ~120 行（実装 30 + テスト 90） |
| Step 11 | Backup TDD Cycle | ~150 行（実装 50 + テスト 100） |
| Step 12 | Logging TDD Cycle | ~100 行（実装 30 + テスト 70） |
| Step 13 | agent_send / SendMessage TDD Cycle（DB routing 拡張 + stop_condition store_system_event） | ~240 行（実装 90 + テスト 150） |
| Step 14 | Slash Commands TDD Cycle | ~130 行（実装 30 + テスト 100） |
| Step 15 | 包括的 DB 隔離検証 TDD Cycle | ~100 行（テスト 100） |
| Step 16 | 動作確認 | コマンド実行のみ |
| Step 17 | Plan・仕様書との自己チェック | レビュー作業 |
| Step 18 | ドキュメント更新 | ~400 行（docs 7ファイル） |
| Step 19 | PR 作成 | PR description 作成 |
| **合計** | | **~2620 行**（実装 ~970 + テスト ~1250 + docs ~400） |

### リスク要因

- `process_turn_inner()` の DB 参照切替は影響範囲が大きく、既存テストの修正が必要になる可能性あり
- `agent_loop/session.rs`, `agent_loop/compaction.rs`, `agent_loop/tool_phase.rs` のシグネチャ変更が波及する場合、Step 7 のみならず前後 Step に影響する可能性
- Discord / Telegram の受信ハンドラ（`store_human_channel_log_message` 含む）は mock しづらい部分があり、テストのためにリファクタリングが必要になる可能性
- `AgentSendTool` / `SendMessageTool` への `secret_db` 注入は `ToolRegistry` 構築順序に依存するため、`runtime/mod.rs` の構築ロジック順序整理が必要な可能性
- backup scheduler の非同期処理と `secret_db` 参照のライフタイム管理に注意
- 包括的 DB 隔離テスト（T42）で新たな漏れ経路が発見された場合、Step 14 から該当 Step へ戻り追加対応が必要
