# EgoPulse 設定仕様

全設定フィールドの型・制約・デフォルト値の完全リファレンス。

## 目次

1. [Config YAML 設計思想](#1-config-yaml-設計思想)
2. [完全フィールドリファレンス](#2-完全フィールドリファレンス)
   - [2.1 グローバル設定](#21-グローバル設定)
   - [2.2 プロバイダー定義](#22-プロバイダー定義providersid)
   - [2.3 Web チャネル](#23-web-チャネルchannelsweb)
   - [2.4 Discord チャネル](#24-discord-チャネルchannelsdiscord)
   - [2.5 Telegram チャネル](#25-telegram-チャネルchannelstelegram)
   - [2.6 Voice チャネル](#26-voice-チャネルchannelsvoice)
   - [2.7 Sleep Batch 設定](#27-sleep-batch-設定sleep_batch)
   - [2.8 Pulse 設定](#28-pulse-設定pulse)
   - [2.9 Web Fetch 設定](#29-web-fetch-設定web_fetch)
   - [2.10 エージェント定義](#210-エージェント定義agentsid)
   - [2.11 完全 YAML 例](#211-完全-yaml-例)
   - [2.12 環境変数オーバーライド](#212-環境変数オーバーライド)
3. [モデル解決チェーン](#3-モデル解決チェーン)
4. [SecretRef（シークレット参照）](#4-secretrefシークレット参照)
5. [プロバイダープリセット](#5-プロバイダープリセット)
6. [デフォルトパス](#6-デフォルトパス)
7. [セットアップウィザード](#7-セットアップウィザード)
8. [設定の変更インターフェース](#8-設定の変更インターフェース)
9. [再起動要否と秘匿フィールド](#9-再起動要否と秘匿フィールド)

---

## 1. Config YAML 設計思想

### 1.1 基本方針

- **単一ファイル管理**: すべての設定を `~/.egopulse/egopulse.config.yaml` に集約する。環境変数による部分的オーバーライドは可能だが、ファイルが真実の情報源（Single Source of Truth）。
- **OpenAI 互換前提**: すべてのプロバイダーは OpenAI 互換 API エンドポイントとして扱う。ベンダー固有 SDK は使わず、`base_url` の切り替えで対応する。
- **DeepSeek thinking 履歴**: DeepSeek 系プロバイダー（`provider` または `model` に `deepseek` を含む、または `base_url` のホストが `deepseek.com` 配下の場合）は、assistant 応答の `reasoning_content` をセッション内に保持し、次回 Chat Completions 履歴へ戻す。他プロバイダーにはこの追加フィールドを送信しない。
- **エージェント単位のモデル指定**: プロバイダー・モデルはエージェント定義（`agents.<id>`）に設定する。チャネル単位のモデル指定は廃止。
- **エージェントファースト**: API キーなどの秘匿値は、YAML では SecretRef として保持し、実値は `~/.egopulse/.env` に保存する。外部 API レスポンスでは `has_api_key` の真偽値のみ返却し、値そのものはマスクする。

### 1.2 設計上の制約

- **単一プロバイダー生成**: セットアップウィザードは 1 つのプロバイダーのみ生成する。複数プロバイダーの追加は手動で YAML を編集するか WebUI から行う（現在は編集未対応）。
- **チャネル境界**: Web / Discord / Telegram / Voice は独立した入力面として session を分離する。プロバイダーとモデルはチャネルではなく選択された Agent の設定から解決する。
- **ホットリロード対応と非対応の分離**: 一部フィールドは設定変更後に即座に反映される。サーバーの再起動が必要なフィールドもある（後述）。

---

## 2. 完全フィールドリファレンス

### 2.1 グローバル設定

| フィールド | 型 | 必須 | デフォルト | 説明 |
|---|---|---|---|---|
| `default_provider` | `string` | **必須** | なし | `providers` マップ内のキーを参照。起動時に使用するプロバイダーを決定 |
| `default_model` | `string \| null` | 任意 | `null` | プロバイダーの `default_model` をグローバルに上書き。`null` の場合プロバイダー定義に従う |
| `default_agent` | `string` | 任意 | `"default"` | 使用するエージェントの ID。`agents` マップ内のキーを参照 |
| `timezone` | `string` | 任意 | `"UTC"` | IANA タイムゾーン（例: `Asia/Tokyo`）。全サブシステム（sleep batch、pulse、会話タイムスタンプ）で使用 |
| `log_level` | `"info" \| "debug" \| "warn" \| "error"` | 任意 | `"info"` | ログ出力レベル |
| `compaction_timeout_secs` | `u64` | 任意 | `180` | 履歴圧縮（compaction）時の LLM 呼び出しタイムアウト秒数 |
| `max_history_messages` | `usize` | 任意 | `50` | セッション復元時のフォールバックメッセージ取得数 |
| `default_context_window_tokens` | `usize` | 任意 | `32768` | context window のトークン数フォールバック値。model 固有の設定がない場合に使用。安全上限 `1,000,000` |
| `compaction_threshold_ratio` | `f64` | 任意 | `0.80` | 推定 prompt tokens が usable context のこの割合に達したら compaction を発火。`(0, 1]` |
| `compaction_target_ratio` | `f64` | 任意 | `0.40` | compaction 後の目標 token 量を usable context に対する割合で指定。threshold 未満 `(0, threshold)` |
| `compact_keep_recent` | `usize` | 任意 | `20` | compaction 時に Tail としてそのまま保持する直近メッセージ数の下限 |

### 2.2 プロバイダー定義（`providers.<id>`）

`providers` はキーがプロバイダー ID のマップ。複数定義可能。

| フィールド | 型 | 必須 | 説明 |
|---|---|---|---|
| `label` | `string` | 推奨 | UI 上の表示名 |
| `base_url` | `string` | **必須** | OpenAI 互換 API エンドポイント URL |
| `api_key` | `string \| SecretRef \| null` | 条件付き | API 認証キー。`localhost` 系および `openai-codex` プリセットでは不要（OAuth セッショントークンを自動利用）。SecretRef 使用可能（後述）。秘匿フィールド |
| `default_model` | `string` | **必須** | このプロバイダーのデフォルトモデル |
| `models` | `map<string, ModelConfig>` | 任意 | 利用可能なモデル一覧。各モデルにメタデータを設定可能 |

#### `ModelConfig` のフィールド

| フィールド | 型 | 必須 | デフォルト | 説明 |
|---|---|---|---|---|
| `context_window_tokens` | `usize` | 任意 | `default_context_window_tokens` に従う | このモデルの context window のトークン数。未設定時はグローバルフォールバックを使用 |
| `model_instructions` | `string` | 任意 | なし | モデル固有の追加指示(インライン)。system prompt の `<soul>` セクションと Core Instructions の間に `<model-instructions>` タグで注入される。`model_instructions_file` と排他(両立時は起動エラー) |
| `model_instructions_file` | `string` | 任意 | なし | モデル固有の追加指示を記述したファイルパス。相対パスは設定ファイルのディレクトリ基点で解決(絶対パスも可)。`model_instructions` と排他 |

### 2.3 Web チャネル（`channels.web`）

| フィールド | 型 | 必須 | デフォルト | 説明 |
|---|---|---|---|---|
| `enabled` | `bool` | 任意 | `false` | Web UI の有効化 |
| `host` | `string` | 任意 | `"127.0.0.1"` | バインドホスト |
| `port` | `u16` | 任意 | `10961` | バインドポート |
| `auth_token` | `string \| SecretRef` | 条件付き | なし | Web 有効時は必須。ブラウザアクセス時の認証トークン。SecretRef 使用可能。秘匿フィールド |
| `allowed_origins` | `[string]` | 任意 | `[]` | WebSocket CORS 許可オリジンリスト |

### 2.4 Discord チャネル（`channels.discord`）

| フィールド | 型 | 必須 | デフォルト | 説明 |
|---|---|---|---|---|
| `enabled` | `bool` | 任意 | `false` | Discord Bot の有効化 |
| `bots` | `map<BotId, DiscordBot>` | 条件付き | なし | Bot 定義。有効時は少なくとも 1 つの Bot が必要 |
| `channels` | `map<u64, DiscordChannelConfig>` | 任意 | なし | 共有チャンネル設定。キーがチャンネル ID。キー存在 = 許可 |

#### `bots.<bot_id>` のフィールド

| フィールド | 型 | 必須 | デフォルト | 説明 |
|---|---|---|---|---|
| `token` | `string \| SecretRef` | 必須 | なし | Discord Bot トークン。SecretRef 使用可能。秘匿フィールド |

#### `channels.discord.channels.<channel_id>` のフィールド

`channels.discord.channels` は Bot ごとではなく Discord チャネル全体で共有される。各エージェントの `discord_bot` が、どの Bot に紐づくかを決める。Single-Agent チャネルでは `agents[0]` に紐づく Bot だけが受信し、`agents[1..]` に紐づく Bot や別 Bot は同じチャンネルに参加していても応答しない。

#### `DiscordChannelConfig` のフィールド

| フィールド | 型 | デフォルト | 説明 |
|---|---|---|---|
| `require_mention` | `bool` | `false` | `true` の場合 @mention なしでは応答しない |
| `agents` | `list<string>` | `[]`（正規化後は `[default_agent]`） | チャンネルで使用するエージェント ID のリスト。空の場合はグローバル `default_agent` が設定される |
| `multi_agent` | `bool` | `false` | `true` の場合複数エージェントで応答。`agents` に 2 つ以上指定が必要 |
| `secret` | `bool` | `false` | `true` の場合、このチャネルの会話を `secret.db` に隔離して保存。Sleep Batch・PULSE はこのチャネルの内容に触れない。Web / TUI では未対応。内部的には `ConversationScope::Secret` にマッピングされる（[architecture.md §7.1](./architecture.md#71-conversationscopeストレージ境界) 参照） |

### 2.5 Telegram チャネル（`channels.telegram`）

Telegram は Discord と同一の Multi-Agent 仕様をサポートする。
複数 Bot 定義 (`bots`)、チャットごとのエージェント選択 (`channels`)、
`@mention` によるルーティングが可能。

| フィールド | 型 | 必須 | デフォルト | 説明 |
|---|---|---|---|---|
| `enabled` | `bool` | 任意 | `false` | Telegram Bot の有効化 |
| `bots` | `map<string, TelegramBotConfig>` | 条件付き | なし | Bot 定義マップ。有効時は必須。キーが Bot ID |
| `channels` | `map<i64, TelegramChatConfig>` | 任意 | なし | チャットごとの設定。キーが chat ID。キー存在 = 許可 |

#### `TelegramBotConfig` のフィールド

| フィールド | 型 | 必須 | デフォルト | 説明 |
|---|---|---|---|---|
| `token` | `string \| SecretRef` | 必須 | なし | Telegram Bot トークン。SecretRef 使用可能。環境変数 `TELEGRAM_BOT_TOKEN` でも指定可能。秘匿フィールド |

#### `TelegramChatConfig` のフィールド

| フィールド | 型 | デフォルト | 説明 |
|---|---|---|---|
| `require_mention` | `bool` | `false` | `true` の場合 @mention なしでは応答しない |
| `agents` | `[string]` | `["default"]` | チャットにバインドするエージェント ID リスト |
| `multi_agent` | `bool` | `false` | `true` の場合 Multi-Agent ルームとして動作。`@mention` でBot に紐づくエージェントが応答し、非メンション時は Channel Log のみ記録 |
| `secret` | `bool` | `false` | `true` の場合、このチャットの会話を `secret.db` に隔離して保存。Sleep Batch・PULSE はこのチャットの内容に触れない。Web / TUI では未対応。内部的には `ConversationScope::Secret` にマッピングされる（[architecture.md §7.1](./architecture.md#71-conversationscopeストレージ境界) 参照） |

### 2.6 Voice チャネル（`channels.voice`）

STT 済みテキストを同期 HTTP API で agent runtime へ接続する設定。Voice 専用 listener は持たず、既存 Web listener 上に `POST /api/voice/turn` を公開する。

| フィールド | 型 | 必須 | デフォルト | 説明 |
|---|---|---|---|---|
| `enabled` | `bool` | 任意 | `false` | Voice API を有効化。有効時は `channels.web.enabled: true` が必須 |
| `auth_token` | `string \| SecretRef` | 有効時必須 | なし | voice client 専用 Bearer token。Web token と共有しない。秘匿フィールド |
| `default_surface` | `string` | 任意 | `"voice"` | request で `surface` を省略した場合の値 |
| `default_session` | `string` | 任意 | `"main"` | request で `session_key` を省略した場合の値 |
| `allowed_surfaces` | `[string]` | 任意 | `[]` | 空なら全 surface を許可。非空なら列挙された surface のみ許可 |

#### 起動時 validation

- `enabled: true` かつ `auth_token` が未設定または空の場合は起動エラー
- `enabled: true` かつ `channels.web.enabled: false` の場合は起動エラー
- `auth_token` は `channels.web.auth_token` へ fallback しない

#### 設定例

```yaml
channels:
  web:
    enabled: true
    host: 127.0.0.1
    port: 10961
    auth_token:
      source: env
      id: WEB_AUTH_TOKEN

  voice:
    enabled: true
    auth_token:
      source: env
      id: EGOPULSE_VOICE_AUTH_TOKEN
    default_surface: stackchan
    default_session: main
    allowed_surfaces:
      - stackchan
```

HTTP 契約は [api.md §2.7](./api.md#27-voice-turn)、責務境界と session identity は [voice-channel.md](./voice-channel.md) を参照。

### 2.7 Sleep Batch 設定（`sleep_batch`）

Sleep Batch（長期記憶のバッチ処理）で使用する LLM のプロバイダーとモデルを、デフォルト設定から独立して指定できる。
また、自動スケジューラにより指定時刻に定期実行できる。

| フィールド | 型 | 必須 | デフォルト | 説明 |
|---|---|---|---|---|
| `sleep_batch.enabled` | `bool` | 任意 | `false` | Sleep Batch 機能の有効/無効 |
| `sleep_batch.provider` | `string \| null` | 任意 | `null` | Sleep Batch 用プロバイダー ID。`providers` マップ内のキーを参照。`null` の場合は `default_provider` にフォールバック |
| `sleep_batch.model` | `string \| null` | 任意 | `null` | Sleep Batch 用モデル名。`null` の場合は `default_model`、さらにプロバイダーの `default_model` にフォールバック |
| `sleep_batch.schedule` | `string \| null` | 任意 | `null` | 自動実行時刻（`HH:MM` 形式、例: `04:00`）。`enabled: true` 時は必須 |
| `sleep_batch.agents` | `list \| null` | 任意 | `null` | 実行対象 agent ID のリスト。`null` は全 agent（default_agent 優先）。空リストは実行なし |
| `sleep_batch.retry.max_attempts` | `uint` | 任意 | `3` | 失敗時の最大再試行回数 |
| `sleep_batch.retry.interval_minutes` | `uint` | 任意 | `5` | 再試行間隔（分） |

### 2.8 Pulse 設定（`pulse`）

Pulse（注意活性化）のスケジューラ設定。

| フィールド | 型 | 必須 | デフォルト | 説明 |
|---|---|---|---|---|
| `pulse.enabled` | `bool` | 任意 | `false` | Pulse 機能の有効/無効 |
| `pulse.tick_interval` | `string` | 任意 | `"1m"` | due scan の周期。Duration 形式（例: `30s`, `5m`, `1h`, `1h30m`） |

### 2.9 DB バックアップ設定（`db.backup`）

SQLite DB のバックアップ・世代管理設定。詳細は [db.md](./db.md#5-バックアップ復元) も参照。

| フィールド | 型 | 必須 | デフォルト | 説明 |
|---|---|---|---|---|
| `db.backup.enabled` | `bool` | 任意 | `true` | バックアップ機能の有効/無効。`false` で起動時・定期ともに停止 |
| `db.backup.interval_days` | `u32` | 任意 | `7` | 定期バックアップの間隔（日）。`1` 以上 |
| `db.backup.time` | `string` | 任意 | `"03:00"` | 定期バックアップの実行時刻（HH:MM）。`Config.timezone` で解釈 |
| `db.backup.max_generations` | `u32` | 任意 | `12` | 保持する世代数。超過分は古い順に削除。`1` 以上 |

#### バックアップのタイミング

- **起動時バックアップ**: マイグレーション前に1回だけ実行（既存 DB が存在する場合）
- **定期バックアップ**: `interval_days` 間隔で `time` 時刻に実行。直近 `max_generations` 件を保持

#### 設定例

```yaml
db:
  backup:
    enabled: true
    interval_days: 7
    time: "03:00"
    max_generations: 12
```

### 2.10 Web Fetch 設定（`web_fetch`）

`web_fetch` built-in tool の挙動を制御する設定。URL scheme、タイムアウト、SSRF 対策、コンテンツバリデーションの各項目を設定する。

| フィールド | 型 | 必須 | デフォルト | 説明 |
|---|---|---|---|---|
| `web_fetch.allowed_schemes` | `[string]` | 任意 | `["https"]` | 許可する URL scheme |
| `web_fetch.timeout_secs` | `u64` | 任意 | `15` | リクエストタイムアウト秒 |
| `web_fetch.max_fetch_bytes` | `usize` | 任意 | `524288` | 最大フェッチサイズ（バイト）。この上限までストリーム読み込み、超過時は取得済み部分を返す |
| `web_fetch.max_output_bytes` | `usize` | 任意 | `65536` | 本文の最大バイト数。HTML処理後の本文をこの上限で切り詰める（warning は上限外） |
| `web_fetch.allow_private_ips` | `bool` | 任意 | `false` | プライベート/ループバック IP へのアクセスを許可 |
| `web_fetch.denylist` | `[string]` | 任意 | `[]` | ブロックするホストのリスト（サブドメインワイルドカード `*.prefix` 対応） |
| `web_fetch.allowlist` | `[string]` | 任意 | `[]` | 許可するホストのリスト（空の場合全許可） |
| `web_fetch.content_validation.enabled` | `bool` | 任意 | `true` | コンテンツバリデーションの有効/無効 |
| `web_fetch.content_validation.strict_mode` | `bool` | 任意 | `false` | 厳格モード: 低信頼度ヒットでもブロック |
| `web_fetch.content_validation.max_scan_bytes` | `usize` | 任意 | `65536` | インジェクションスキャンの最大バイト数 |

### 2.11 エージェント定義（`agents.<id>`）

`agents` はキーがエージェント ID のマップ。複数定義可能。

| フィールド | 型 | デフォルト | 説明 |
|---|---|---|---|
| `label` | `string` | エージェント ID | UI 上の表示名 |
| `provider` | `string \| null` | `null` | エージェント固有のプロバイダー ID。`null` なら `default_provider` |
| `model` | `string \| null` | `null` | エージェント固有のモデル名。`null` ならモデル解決チェーンに従う |
| `discord_bot` | `string \| null` | `null` | このエージェントが紐づく Discord Bot ID。`discord.bots` のキーを参照 |
| `telegram_bot` | `string \| null` | `null` | このエージェントが紐づく Telegram Bot ID。`telegram.bots` のキーを参照 |
| `profiles` | `map` | `{}` | チャネル別オーバーライド。キーがチャネル名（例: `voice`） |

`profiles` の各エントリ（`profiles.<channel_name>`）:

| フィールド | 型 | デフォルト | 説明 |
|---|---|---|---|
| `provider` | `string \| null` | `null` | このチャネルでのプロバイダー ID。省略時は `agent.provider` を引き継ぐ |
| `model` | `string \| null` | `null` | このチャネルでのモデル名。省略時はモデル解決チェーンに従う |

設定例:

```yaml
agents:
  lyre:
    label: lyre
    provider: sakura
    model: preview/Kimi-K2.6
    discord_bot: lyre
    profiles:
      voice:
        provider: openrouter
        model: gpt-4.1-mini
```

### 2.12 完全 YAML 例

```yaml
# ========================================
# グローバル LLM 設定
# ========================================
default_provider: openrouter
default_model: null
default_agent: default
timezone: Asia/Tokyo

# ========================================
# エージェント定義
# ========================================
agents:
  default:
    label: Default Agent
    provider: null
    model: null
  alice:
    label: Alice
    provider: openrouter
    model: anthropic/claude-sonnet-4
    discord_bot: main
  reviewer:
    label: Reviewer

# ========================================
# システム設定
# ========================================
log_level: info
compaction_timeout_secs: 180
max_history_messages: 50
default_context_window_tokens: 32768
compaction_threshold_ratio: 0.80
compaction_target_ratio: 0.40
compact_keep_recent: 20

# ========================================
# プロバイダー定義
# ========================================
providers:
  openrouter:
    label: OpenRouter
    base_url: https://openrouter.ai/api/v1
    api_key: sk-or-v1-xxxxxxxxxxxxx
    default_model: anthropic/claude-sonnet-4
    models:
      anthropic/claude-sonnet-4:
        context_window_tokens: 200000
        # モデル固有の追加指示(インライン)
        model_instructions: |
          Prefer concise, action-first responses.
          Avoid preamble unless the user asks for reasoning.
      google/gemini-2.5-pro:
        context_window_tokens: 1048576
        # モデル固有の追加指示(ファイル参照・相対パスは設定ファイルと同ディレクトリ基点)
        model_instructions_file: prompts/gemini-instructions.md
      openai/gpt-4.1:
        context_window_tokens: 1048576
  ollama:
    label: Ollama (Local)
    base_url: http://localhost:11434/v1
    api_key: null
    default_model: llama3
    models:
      llama3: {}
      codellama: {}

# ========================================
# チャネル設定
# ========================================
channels:
  web:
    enabled: true
    host: 127.0.0.1
    port: 10961
    auth_token: my-secret-token-here
    allowed_origins:
      - http://localhost:3000

  discord:
    enabled: true
    bots:
      main:
        token:
          source: env
          id: DISCORD_BOT_TOKEN
    channels:
      "1234567890123456789":
      "9876547890123456789":
        require_mention: true
        agents: [alice, reviewer]
        multi_agent: true
        secret: true

  telegram:
    enabled: false
    telegram_bots:
      default:
        token:
          source: env
          id: TELEGRAM_BOT_TOKEN
    telegram_channels:
      "-1001234567890":
      "-1009876543210":
        require_mention: true
        agents: [alice, reviewer]
        multi_agent: true

  voice:
    enabled: true
    auth_token:
      source: env
      id: EGOPULSE_VOICE_AUTH_TOKEN
    default_surface: stackchan
    default_session: main
    allowed_surfaces:
      - stackchan

# ========================================
# Sleep Batch 設定（任意）
# ========================================
sleep_batch:
  enabled: true
  provider: openrouter
  model: openai/gpt-4o-mini
  schedule: "04:00"
  agents:
    - default
    - sub-agent
  retry:
    max_attempts: 3
    interval_minutes: 5

# ========================================
# Pulse 設定（任意）
# ========================================
pulse:
  enabled: true
  tick_interval: "1h"


# ========================================
# Web Fetch 設定（任意）
# ========================================
web_fetch:
  allowed_schemes:
    - https
  timeout_secs: 15
  max_fetch_bytes: 524288
  max_output_bytes: 65536
  allow_private_ips: false
  denylist: []
  allowlist: []
  content_validation:
    enabled: true
    strict_mode: false
    max_scan_bytes: 65536
```

### 2.13 環境変数オーバーライド

Config YAML の値を環境変数で上書き可能。環境変数が設定されている場合、YAML の値より優先される。

| 環境変数 | 対象フィールド | 型 |
|---|---|---|
| `LOG_LEVEL` | `log_level` | `string` |
| `WEB_HOST` | `channels.web.host` | `string` |
| `WEB_PORT` | `channels.web.port` | `u16` |
| `WEB_ENABLED` | `channels.web.enabled` | `bool` |
| `WEB_AUTH_TOKEN` | `channels.web.auth_token` | `string` |
| `WEB_ALLOWED_ORIGINS` | `channels.web.allowed_origins` | `string`（カンマ区切り） |
| `DISCORD_BOT_TOKEN` | `channels.discord.bots.*.token` | `string` (SecretRef 経由) |
| `TELEGRAM_BOT_TOKEN` | `channels.telegram.telegram_bots.*.token` | `string` (SecretRef 経由) |

> ※ Discord / Telegram Bot トークンは SecretRef `{ source: env, id: <VAR_NAME> }` で解決（§4 参照）。Layer 2 の直接オーバーライドは非対応。

---

## 3. モデル解決チェーン

メッセージ送信時に使用するモデルは、以下の優先順位で解決される。

```text
agent.profiles[channel].model（チャネル別プロファイル指定）
    ↓ null または該当プロファイルなしの場合
agent.model（エージェント固有モデル指定）
    ↓ null の場合
config.default_model（グローバルモデル上書き）
    ↓ null の場合
provider.default_model（プロバイダーのデフォルトモデル）
```

各ステップの説明:

1. **`agent.profiles[channel].model`**: チャネル名（例: `voice`）に対応するプロファイルが存在し、そこにモデル指定があれば最優先。同じエージェントでもチャネルごとに異なるモデルを使える。
2. **`agent.model`**: エージェント設定にモデルが指定されていれば次優先。エージェントごとに異なるモデルを使う運用が可能。
3. **`config.default_model`**: エージェント指定がない場合のグローバルフォールバック。全チャネルで統一モデルを使いたい場合に設定する。
4. **`provider.default_model`**: 最終フォールバック。プロバイダー定義に記述されたデフォルトモデルが使われる。

> **Note**: `agent.provider` と `agent.model` は独立して解決される。`agent.provider` だけを設定しても、そのプロバイダーの `default_model` は自動適用されない。モデル解決は上記チェーンに従い、プロバイダーが解決された後にモデル解決が独立して走る。プロファイルの `provider` / `model` も同様に独立して解決される。

`/provider` / `/model` のデフォルト更新対象は現在の `agent_id`（`agents.<id>.provider` / `agents.<id>.model`）。チャネル設定を変更したい場合は `--scope discord` のように明示する。

プロバイダー解決も同様のチェーン:

```text
agent.profiles[channel].provider（チャネル別プロファイル指定）
    ↓ null または該当プロファイルなしの場合
agent.provider（エージェント固有プロバイダー指定）
    ↓ null の場合
config.default_provider（グローバルプロバイダー）
```

---

## 4. SecretRef（シークレット参照）

### 4.1 概要

秘匿フィールド（`api_key`, `auth_token`, `bot_token`）は、YAML に平文を直接書く代わりに SecretRef オブジェクトで外部ソースを参照できる。

### 4.2 参照ソース

| source | フィールド | 解決順序 |
|---|---|---|
| `env` | `id: VAR_NAME` | プロセス環境変数 → `~/.egopulse/.env` |
| `exec` | `command: "cmd"` | コマンドを実行し stdout を取得（10 秒タイムアウト） |

### 4.3 SecretRef の記述例

```yaml
providers:
  openai:
    api_key:
      source: env
      id: OPENAI_API_KEY

channels:
  discord:
    bots:
      main:
        token:
          source: exec
          command: "pass show discord/bot_token"
```

### 4.4 解決レイヤー

SecretRef 解決は以下の 2 層で構成される。

1. **Layer 1: YAML SecretRef 解決** — `{ source: env, id: X }` をプロセス環境変数 → `.env` ファイルの順で解決
2. **Layer 2: プロセス環境変数オーバーライド** — `WEB_AUTH_TOKEN` 等が Layer 1 の結果を上書き

### 4.5 .env ファイル

| 項目 | 値 |
|---|---|
| パス | `~/.egopulse/.env` |
| フォーマット | `KEY=VALUE`（1 行 1 エントリ、`#` コメント対応） |
| パーミッション | `0600` |
| 対象 | `source: env` で参照される値のみ |

### 4.6 環境変数名の規約

| 用途 | 環境変数名 |
|---|---|
| プロバイダー API キー | `{PROVIDER_ID}_API_KEY`（例: `OPENAI_API_KEY`） |
| Web 認証トークン | `WEB_AUTH_TOKEN` |
| Voice 認証トークン | 任意の SecretRef ID（推奨: `EGOPULSE_VOICE_AUTH_TOKEN`） |
| Discord Bot トークン | `DISCORD_BOT_TOKEN` |
| Telegram Bot トークン | `TELEGRAM_BOT_TOKEN` |

---

## 5. プロバイダープリセット

セットアップウィザードで選択可能なプリセット一覧。各プリセットは `base_url` と推奨 `default_model` を内包しており、選択すると自動入力される。

| ID | サービス | base_url |
|---|---|---|
| `openai` | OpenAI | `https://api.openai.com/v1` |
| `openai-codex` | OpenAI Codex (OAuth) | `https://chatgpt.com/backend-api/codex` |
| `openrouter` | OpenRouter | `https://openrouter.ai/api/v1` |
| `ollama` | Ollama | `http://localhost:11434/v1` |
| `google` | Google AI | `https://generativelanguage.googleapis.com/v1beta/openai` |
| `aliyun-bailian` | 阿里雲百炼 | `https://dashscope.aliyuncs.com/compatible-mode/v1` |
| `alibaba` | Alibaba Cloud | `https://dashscope.aliyuncs.com/compatible-mode/v1` |
| `qwen-portal` | Qwen Portal | `https://dashscope.aliyuncs.com/compatible-mode/v1` |
| `deepseek` | DeepSeek | `https://api.deepseek.com` |
| `synthetic` | Synthetic | `https://api.synthetic.dev/v1` |
| `chutes` | Chutes | `https://chutes.ai/app/v1` |
| `moonshot` | Moonshot AI | `https://api.moonshot.cn/v1` |
| `mistral` | Mistral AI | `https://api.mistral.ai/v1` |
| `azure` | Azure OpenAI | `https://{resource}.openai.azure.com/openai/deployments/{deployment}` |
| `bedrock` | AWS Bedrock | `https://bedrock-runtime.{region}.amazonaws.com` |
| `zhipu` | 智譜 AI (GLM) | `https://open.bigmodel.cn/api/paas/v4` |
| `zai` | ZAI | `https://api.z.ai/api/coding/paas/v4` |
| `minimax` | MiniMax | `https://api.minimax.chat/v1` |
| `cohere` | Cohere | `https://api.cohere.com/v2` |
| `tencent` | Tencent Cloud | `https://hunyuan.tencentcloudapi.com/openai/v1` |
| `xai` | xAI (Grok) | `https://api.x.ai/v1` |
| `nvidia` | NVIDIA NIM | `https://integrate.api.nvidia.com/v1` |
| `huggingface` | Hugging Face | `https://api-inference.huggingface.co/v1` |
| `together` | Together AI | `https://api.together.xyz/v1` |
| `lmstudio` | LMStudio | `http://localhost:1234/v1` |
| `custom` | Custom | ユーザー入力 |

---

## 6. デフォルトパス

| 用途 | パス | 備考 |
|---|---|---|
| 設定ファイル | `~/.egopulse/egopulse.config.yaml` | 環境変数で変更不可 |
| データディレクトリ | `~/.egopulse/data` | SQLite 等 |
| ワークスペース | `~/.egopulse/workspace` | セッション・履歴データ |

---

## 7. セットアップウィザード

`egopulse setup` で起動する対話型設定プロンプト（dialoguer ベースのチャットライク順次プロンプト）。
Agent-First 設計に基づき、エージェント名を最初に問い、LLM と対話するために必要な最小限の項目のみを順次収集する。

> 設計の背景・方針・経緯は [setup-redesign.md](./setup-redesign.md) を参照。コマンド仕様は [commands.md §1.1](./commands.md#1-cli-サブコマンド) を参照。

### 7.1 フロー全体像

```text
[Welcome]
   │
   ▼
[既存設定の読み込み]  ── パースエラー時は警告 + Y/N 確認
   │
   ▼
[Q1: Agent Label]       エージェント名 (text)
   │
   ▼
[Q2: Provider]          26 プリセット + Custom (select)
   │                     Custom 選択時のみ base_url 入力を追加
   ▼
[Q3: Model]             preset なら select / Custom なら text
   │
   ▼
[Q4: API Key]           password。空欄可、非 localhost 系の空欄は Y/N 確認
   │
   ▼
[Q5: Web Channel]       Y/n（デフォルト yes）
   │
   ▼
[Q6: Discord]           y/N ── yes のみ Bot Token 入力
   │
   ▼
[Q7: Telegram]          y/N ── yes のみ Bot Token 入力
   │
   ▼
[Review]                生成内容表示 + Save? (Y/n)
   │                     no の場合は StartOver / Abort / SaveAnyway の 3 択
   ▼
[Save]                  YAML + .env 永続化（上書き前にバックアップ）
   │
   ▼
[Additional Options]    設定対象外項目の案内（情報表示のみ）
   │
   ▼
[Done]                  保存先・次ステップ・チャネル案内
```

### 7.2 入力項目仕様（Q1〜Q7）

| Q | 項目 | 入力種別 | 必須 | デフォルト | 備考 |
|---|---|---|:---:|---|---|
| Q1 | Agent Label | text | ○ | なし | エージェントの表示名。空入力時は `"Default"` にフォールバック |
| — | Agent ID | (自動) | — | label を slugify | lowercase + 英数字以外をハイフン化 + 連続ハイフン圧縮。空結果は `"default"` にフォールバック |
| Q2 | Provider | select | ○ | なし | 26 プリセット（§5 プロバイダープリセット）+ `Custom` |
| Q2' | base_url | text | 条件付き | なし | `Custom` 選択時のみ追加質問。URL 検証あり |
| Q3 | Model | select / text | ○ | preset の `default_model` | preset 選択時は select、`Custom` 選択時は text |
| Q4 | API Key | password | △ | 空文字 | 常に入力ステップを表示。localhost 系は空欄でそのまま通す |
| Q5 | Web Channel | confirm | — | `yes` | 無効時は `channels.web` エントリ自体を YAML に含めない |
| — | Web auth_token | (自動) | — | `generate_auth_token()` | ユーザー入力なし。実値は Review / Done で非表示 |
| Q6 | Discord | confirm | — | `no` | `yes` のみ Bot Token 入力 (password) へ分岐 |
| Q7 | Telegram | confirm | — | `no` | `yes` のみ Bot Token 入力 (password) へ分岐 |

各質問の分岐仕様:

- **Q2 Provider**: `Custom` 選択時のみ直後に base_url 入力を追加。preset 選択時は base_url を preset のデフォルトで自動補完
- **Q3 Model**: preset 選択時は `models` リストからの select（リスト外は選択不可）。`Custom` 選択時は手入力 text モードに切替
- **Q4 API Key**: ステップ自体は常に表示。非 localhost 系プロバイダで空欄入力時は `Proceed with an empty key? (y/N)` で確認。`no` で再入力、`yes` で警告付きで進行
- **Q6 / Q7**: `yes` の場合のみ Bot Token (password 入力) を追加質問。空トークンは拒否

### 7.3 Review（保存確認）

収集した入力から生成される設定内容を表示し、保存確認を行う。

```text
About to save the configuration file with the following values:

  Agent:    Partner (id: partner)
  Provider: openai (https://api.openai.com/v1)
  Model:    gpt-5.2
  API Key:  sk-...xxxx
  Web:      enabled (auth_token: auto-generated, saved to .env)
  Discord:  disabled
  Telegram: disabled

Save? (Y/n)
```

- API Key は先頭 3 文字 + `...` + 末尾 4 文字でマスク。空欄時は `(empty)`
- `Save?` で `no` の場合は 3 択を提示:

| 選択肢 | 動作 |
|---|---|
| Start over (back to Agent Label) | Q1 に戻り入力をやり直す |
| Abort (exit without saving) | 保存せずに終了（exit code 1） |
| Save anyway | 警告を了承の上、保存へ進む |

### 7.4 Additional Options（設定対象外項目の案内）

保存完了後、セットアップで設定しなかったが YAML 編集で設定可能な項目をカテゴリ別に案内する。入力は受け付けず、Enter で次へ進む情報表示のみ。

| カテゴリ | 案内項目 |
|---|---|
| System | `timezone` / `log_level` / `default_context_window_tokens` / `compaction_*` / `max_history_messages` |
| Web UI | `channels.web.host` / `channels.web.port` / `channels.web.allowed_origins` |
| Channels | 追加プロバイダー・エージェント / Discord・Telegram チャネルアクセス制御 / Voice チャネル / エージェント別人格 (`SOUL.md`) |
| Subsystems | `sleep_batch` / `pulse` / `db.backup` / `web_fetch` |

詳細は `docs/config.md`（本ドキュメント）および [channels.md](./channels.md) へ誘導。

### 7.5 Done（完了メッセージ）

保存完了時に以下を出力する。

- **保存先**: `~/.egopulse/egopulse.config.yaml`
- **バックアップ**: 既存設定があった場合のみバックアップファイルパスを表示
- **次ステップ**:

| アクション | コマンド |
|---|---|
| すぐチャット開始 | `egopulse chat` |
| systemd サービス登録 | `egopulse gateway install` |
| 設定編集 | `~/.egopulse/egopulse.config.yaml` |
| エージェント追加 | YAML の `agents` セクション編集 |

- **Web UI 有効時**: アクセス URL (`http://127.0.0.1:10961`) とトークン参照先 (`~/.egopulse/.env` の `WEB_AUTH_TOKEN`)
- **Discord / Telegram 有効時**: DM は即利用可能、サーバー / グループ応答には YAML へのチャネル ID 追加が必要（[channels.md](./channels.md) 参照）

API Key / トークン類の**実値は表示しない**（セキュリティ）。

### 7.6 既存設定の再編集（prefill）

`egopulse setup` を既存設定が存在する状態で実行した場合、各 Q のプロンプトは既存値をデフォルトとして事前入力する。Enter で進めば既存値を維持、入力し直せば上書き。

| Q | 事前入力される値 |
|---|---|
| Q1 Agent Label | 既存 `agents.<default_agent>.label`（無ければ空） |
| Q2 / Q2' | 既存 `default_provider`（preset 非一致なら `Custom` 扱いで base_url も事前入力） |
| Q3 Model | 既存 `providers.<id>.default_model` |
| Q4 API Key | 既存 `.env` から解決した `api_key`（解決不能なら空） |
| Q5 Web | 既存 `channels.web.enabled`（無ければ `yes`） |
| Q6 Discord | 既存 `channels.discord.enabled` |
| Q7 Telegram | 既存 `channels.telegram.enabled` |

既存 YAML のパースエラー時は Q1 の前に警告表示 + `Continue with empty defaults? (y/N)` で確認する（黙殺は廃止）。`WEB_AUTH_TOKEN` と `state_root` は事前入力対象外だが上書きされない。

### 7.7 生成される YAML 構造

- `default_agent`: Q1 で生成した agent id
- `default_provider`: Q2 で選んだ provider id
- `agents.<id>.label`: Q1 の入力値
- `providers.<id>`: Q2 / Q3 / Q4 の値（label, base_url, api_key, default_model, models）
- `channels.web`: Q5 の結果。`yes` の場合は `enabled / host=127.0.0.1 / port=10961 / auth_token` を保存。`no` の場合はエントリ自体を含めない
- `channels.discord`: Q6 の結果（enabled, `bots.default.token`）
- `channels.telegram`: Q7 の結果（enabled, `bots.default.token`）
- 秘匿値は `~/.egopulse/.env` に書き出し、YAML には SecretRef で参照（[§4](#4-secretrefシークレット参照) 参照）
- 単一プロバイダー・単一エージェントのみ生成（複数追加は手動 YAML 編集）

---

## 8. 設定の変更インターフェース

設定の読み取り・書き込みは以下のインターフェースから行える：

| インターフェース | 読み取り | 書き込み | 対象 |
|---------|:---:|:---:|------|
| YAML 手動編集 | 全フィールド | 全フィールド | `~/.egopulse/egopulse.config.yaml` |
| Setup Prompt (`egopulse setup`) | — | エージェント・プロバイダー・モデル・チャネル | 初回セットアップ・再設定 ([§7](#7-セットアップウィザード)) |
| WebUI (`/api/config`) | 公開フィールド | 公開フィールド | ランタイム中の設定変更 |
| スラッシュコマンド (`/provider`, `/model`) | ○ | ○ | プロバイダー・モデルの動的切替 |

WebUI の設定 API 仕様は [api.md](./api.md) を参照。
スラッシュコマンドの仕様は [commands.md](./commands.md) を参照。

---

## 9. 再起動要否と秘匿フィールド

### 9.1 再起動が必要なフィールド

以下のフィールドを変更した場合、プロセスの再起動が必要。

| フィールド | 理由 |
|---|---|
| `channels.web.enabled` | Web サーバーの起動/停止が伴う |
| `channels.web.host` | バインドアドレスの変更 |
| `channels.web.port` | バインドポートの変更 |
| `channels.voice.enabled` | Voice route の mount / unmount が伴う |
| `channels.voice.auth_token` | Voice 認証 middleware の credential 更新 |
| `channels.voice.default_surface` | request default の更新 |
| `channels.voice.default_session` | request default の更新 |
| `channels.voice.allowed_surfaces` | Voice API の access control 更新 |
| `channels.discord.enabled` | Discord Bot の接続/切断 |
| `channels.discord.bots.<bot_id>.token` | Bot 認証の再確立 |
| `channels.discord.channels` | チャンネルアクセス制御・メンション要件・秘密モードの変更 |
| `channels.telegram.enabled` | Telegram Bot の接続/切断 |
| `channels.telegram.telegram_bots` | Bot 定義の更新 |
| `channels.telegram.telegram_channels` | チャットアクセス制御・メンション要件・秘密モードの変更 |
| `log_level` | ロガーの初期化が伴う |

### 9.2 ホットリロード可能なフィールド

以下のフィールドは変更が即座に反映され、再起動は不要。

| フィールド | 理由 |
|---|---|
| `default_provider` | 次回メッセージ送信時に参照 |
| `default_model` | 次回メッセージ送信時に参照 |
| `default_agent` | 次回メッセージ送信時に参照 |
| `agents` の内容 | エージェント定義は都度読み込み |
| `providers` の内容 | プロバイダー定義は都度読み込み |
| `compaction_*` | 次回圧縮時に参照 |
| `max_*` | 次回セッション操作時に参照 |
| `sleep_batch.*` | Sleep Batch 実行時に参照 |
| `pulse.*` | Pulse 実行時に参照 |
| `web_fetch.*` | 次回 web_fetch 実行時に参照 |
| `providers.<id>.models.<model>.model_instructions` | 次回メッセージ送信(または Pulse 起火)時に毎ターン再読み込み |
| `providers.<id>.models.<model>.model_instructions_file` | 同上(参照先ファイルの内容も毎ターン再読み込み) |

### 9.3 秘匿フィールド

以下のフィールドは API レスポンスでマスクされ、`has_*` 真偽値のみ返却される。

| フィールド | API での表現 |
|---|---|
| `providers.<id>.api_key` | `has_api_key: boolean` |
| `channels.web.auth_token` | `web_auth_enabled: boolean` |
| `channels.voice.auth_token` | API レスポンスへ返却しない |
| `channels.discord.bots.<bot_id>.token` | 返却なし |
| `channels.telegram.telegram_bots.<bot_id>.token` | 返却なし |

---
