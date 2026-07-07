# EgoPulse 設定仕様

全設定フィールドの型・制約・デフォルト値の完全リファレンス。

## 目次

1. [Config YAML 設計思想](#1-config-yaml-設計思想)
2. [完全 YAML 例](#2-完全-yaml-例)
3. [完全フィールドリファレンス](#3-完全フィールドリファレンス)
   - [3.1 グローバル設定](#31-グローバル設定)
   - [3.2 プロバイダー定義](#32-プロバイダー定義providersid)
   - [3.3 Web チャネル](#33-web-チャネルchannelsweb)
   - [3.4 Discord チャネル](#34-discord-チャネルchannelsdiscord)
   - [3.5 Telegram チャネル](#35-telegram-チャネルchannelstelegram)
   - [3.6 Voice チャネル](#36-voice-チャネルchannelsvoice)
   - [3.7 Sleep Batch 設定](#37-sleep-batch-設定sleep_batch)
   - [3.8 Pulse 設定](#38-pulse-設定pulse)
   - [3.9 DB バックアップ設定](#39-db-バックアップ設定dbbackup)
   - [3.10 Web Fetch 設定](#310-web-fetch-設定web_fetch)
   - [3.11 エージェント定義](#311-エージェント定義agentsid)
4. [モデル解決チェーン](#4-モデル解決チェーン)
5. [SecretRef（シークレット参照）](#5-secretrefシークレット参照)
6. [環境変数オーバーライド](#6-環境変数オーバーライド)
7. [プロバイダープリセット](#7-プロバイダープリセット)
8. [デフォルトパス](#8-デフォルトパス)
9. [セットアップウィザード](#9-セットアップウィザード)
10. [設定の変更インターフェース](#10-設定の変更インターフェース)
11. [再起動要否と秘匿フィールド](#11-再起動要否と秘匿フィールド)

---

## 1. Config YAML 設計思想

### 1.1 基本方針

- **単一ファイル管理**: すべての設定を `~/.egopulse/egopulse.config.yaml` に集約する。環境変数による部分的オーバーライドは可能だが、ファイルが真実の情報源（Single Source of Truth）。
- **OpenAI 互換前提**: すべてのプロバイダーは OpenAI 互換 API エンドポイントとして扱う。ベンダー固有 SDK は使わず、`base_url` の切り替えで対応する。
- **DeepSeek thinking 履歴**: DeepSeek 系プロバイダー（`provider` または `model` に `deepseek` を含む、または `base_url` のホストが `deepseek.com` 配下の場合）は、assistant 応答の `reasoning_content` をセッション内に保持し、次回 Chat Completions 履歴へ戻す。他プロバイダーにはこの追加フィールドを送信しない。
- **エージェント単位のモデル指定**: プロバイダー・モデルはエージェント定義（`agents.<id>`）に設定する。チャネル単位のモデル指定は廃止。
- **エージェントファースト**: API キーなどの秘匿値は、YAML では SecretRef として保持し、実値は `~/.egopulse/.env` に保存する。外部 API レスポンスでは `has_api_key` の真偽値のみ返却し、値そのものはマスクする。

### 1.2 設計上の制約

- **単一プロバイダー生成**: セットアップウィザードは 1 つのプロバイダーのみ生成する。複数プロバイダーの追加は手動で YAML を編集するか WebUI から行う（WebUI によるプロバイダー編集は未対応。制限事項は §9 参照）。
- **チャネル境界**: Web / Discord / Telegram / Voice は独立した入力面として session を分離する。プロバイダーとモデルはチャネルではなく選択された Agent の設定から解決する。
- **ホットリロード対応と非対応の分離**: 一部フィールドは設定変更後に即座に反映される。サーバーの再起動が必要なフィールドもある（§11 参照）。

---

## 2. 完全 YAML 例

すべての設定ブロックを網羅した実例。各ブロックの詳細は §3 を、フィールドごとの再起動要否は §11 を参照。秘匿値は SecretRef（§5）または `.env`（§8）経由で指定すること。

```yaml
# ========================================
# グローバル設定（§3.1）
# ========================================
default_provider: openrouter
default_model: null
default_agent: default
timezone: Asia/Tokyo
log_level: info
compaction_timeout_secs: 180
max_history_messages: 50
default_context_window_tokens: 32768
compaction_threshold_ratio: 0.80
compaction_target_ratio: 0.40
compact_keep_recent: 20

# ========================================
# エージェント定義（§3.11）
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
  lyre:
    label: Lyre
    provider: sakura
    model: preview/Kimi-K2.6
    discord_bot: lyre
    # チャネル別オーバーライド（voice チャネルでは別モデルを使用）
    profiles:
      voice:
        provider: openrouter
        model: gpt-4.1-mini

# ========================================
# プロバイダー定義（§3.2）
# ========================================
providers:
  openrouter:
    label: OpenRouter
    base_url: https://openrouter.ai/api/v1
    api_key:
      source: env
      id: OPENROUTER_API_KEY
    default_model: anthropic/claude-sonnet-4
    models:
      anthropic/claude-sonnet-4:
        context_window_tokens: 200000
        # モデル固有の追加指示（インライン）
        model_instructions: |
          Prefer concise, action-first responses.
          Avoid preamble unless the user asks for reasoning.
      google/gemini-2.5-pro:
        context_window_tokens: 1048576
        # モデル固有の追加指示（ファイル参照。相対パスは設定ファイルと同ディレクトリ基点）
        model_instructions_file: prompts/gemini-instructions.md
      openai/gpt-4.1:
        context_window_tokens: 1048576
  ollama:
    label: Ollama (Local)
    base_url: http://127.0.0.1:11434/v1
    api_key: null
    default_model: llama3.2
    models:
      llama3.2: {}
      codellama: {}

# ========================================
# チャネル設定（§3.3〜§3.6）
# ========================================
channels:
  web:
    enabled: true
    host: 127.0.0.1
    port: 10961
    auth_token:
      source: env
      id: WEB_AUTH_TOKEN
    allowed_origins:
      - http://localhost:3000

  discord:
    enabled: true
    bots:
      main:
        token:
          source: env
          id: DISCORD_BOT_TOKEN
      lyre:
        token:
          source: env
          id: DISCORD_BOT_TOKEN_LYRE
    channels:
      "1234567890123456789": {}
      "9876547890123456789":
        require_mention: true
        agents: [alice, reviewer]
        multi_agent: true
        secret: true
        tool_progress: true

  telegram:
    enabled: false
    telegram_bots:
      default:
        token:
          source: env
          id: TELEGRAM_BOT_TOKEN
    telegram_channels:
      "-1001234567890": {}
      "-1009876543210":
        require_mention: true
        agents: [alice, reviewer]
        multi_agent: true
        secret: true

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
# Sleep Batch 設定（§3.7）
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
# Pulse 設定（§3.8）
# ========================================
pulse:
  enabled: true
  tick_interval: "1h"

# ========================================
# DB バックアップ設定（§3.9）
# ========================================
db:
  backup:
    enabled: true
    interval_days: 7
    time: "03:00"
    max_generations: 12

# ========================================
# Web Fetch 設定（§3.10）
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

---

## 3. 完全フィールドリファレンス

### 3.1 グローバル設定

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

### 3.2 プロバイダー定義（`providers.<id>`）

`providers` はキーがプロバイダー ID のマップ。複数定義可能。

| フィールド | 型 | 必須 | 説明 |
|---|---|---|---|
| `label` | `string` | 推奨 | UI 上の表示名 |
| `base_url` | `string` | **必須** | OpenAI 互換 API エンドポイント URL |
| `api_key` | `string \| SecretRef \| null` | 条件付き | API 認証キー。`localhost` 系および `openai-codex` プリセットでは不要（OAuth セッショントークンを自動利用）。SecretRef 使用可能（§5）。秘匿フィールド |
| `default_model` | `string` | **必須** | このプロバイダーのデフォルトモデル |
| `models` | `map<string, ModelConfig>` | 任意 | 利用可能なモデル一覧。各モデルにメタデータを設定可能 |

#### `ModelConfig` のフィールド

| フィールド | 型 | 必須 | デフォルト | 説明 |
|---|---|---|---|---|
| `context_window_tokens` | `usize` | 任意 | `default_context_window_tokens` に従う | このモデルの context window のトークン数。未設定時はグローバルフォールバックを使用 |
| `model_instructions` | `string` | 任意 | なし | モデル固有の追加指示（インライン）。system prompt の `<soul>` セクションと Core Instructions の間に `<model-instructions>` タグで注入される。`model_instructions_file` と排他（両立時は起動エラー） |
| `model_instructions_file` | `string` | 任意 | なし | モデル固有の追加指示を記述したファイルパス。相対パスは設定ファイルのディレクトリ基点で解決（絶対パスも可）。`model_instructions` と排他 |

### 3.3 Web チャネル（`channels.web`）

| フィールド | 型 | 必須 | デフォルト | 説明 |
|---|---|---|---|---|
| `enabled` | `bool` | 任意 | `false` | Web UI の有効化 |
| `host` | `string` | 任意 | `"127.0.0.1"` | バインドホスト |
| `port` | `u16` | 任意 | `10961` | バインドポート |
| `auth_token` | `string \| SecretRef` | 条件付き | なし | Web 有効時は必須。ブラウザアクセス時の認証トークン。SecretRef 使用可能。秘匿フィールド |
| `allowed_origins` | `[string]` | 任意 | `[]` | WebSocket CORS 許可オリジンリスト |

### 3.4 Discord チャネル（`channels.discord`）

| フィールド | 型 | 必須 | デフォルト | 説明 |
|---|---|---|---|---|
| `enabled` | `bool` | 任意 | `false` | Discord Bot の有効化 |
| `bots` | `map<BotId, DiscordBotConfig>` | 条件付き | なし | Bot 定義。有効時は少なくとも 1 つの Bot が必要 |
| `channels` | `map<u64, DiscordChannelConfig>` | 任意 | なし | 共有チャンネル設定。キーがチャンネル ID。キー存在 = 許可 |

#### `bots.<bot_id>` のフィールド

| フィールド | 型 | 必須 | デフォルト | 説明 |
|---|---|---|---|---|
| `token` | `string \| SecretRef` | 必須 | なし | Discord Bot トークン。SecretRef 使用可能。秘匿フィールド |

#### Discord チャンネル設定（`channels.discord.channels`）

`channels.discord.channels` は Bot ごとではなく Discord チャネル全体で共有される。各エージェントの `discord_bot` が、どの Bot に紐づくかを決める。Single-Agent チャネルでは `agents[0]` に紐づく Bot だけが受信し、`agents[1..]` に紐づく Bot や別 Bot は同じチャンネルに参加していても応答しない。

#### `DiscordChannelConfig` のフィールド

| フィールド | 型 | デフォルト | 説明 |
|---|---|---|---|
| `require_mention` | `bool` | `false` | `true` の場合 @mention なしでは応答しない |
| `agents` | `list<string>` | `[]`（正規化後は `[default_agent]`） | チャンネルで使用するエージェント ID のリスト。空の場合はグローバル `default_agent` が設定される |
| `multi_agent` | `bool` | `false` | `true` の場合複数エージェントで応答。`agents` に 2 つ以上指定が必要 |
| `secret` | `bool` | `false` | `true` の場合、このチャネルの会話を `secret.db` に隔離して保存。Sleep Batch・PULSE はこのチャネルの内容に触れない。Web / TUI では未対応。内部的には `ConversationScope::Secret` にマッピングされる（[architecture.md §7.1](./architecture.md#71-conversationscopeストレージ境界) 参照） |
| `tool_progress` | `bool` | `false` | `true` の場合、5 秒以上かかるターンでツール実行状況を編集式の累積ログとして投稿する（[channels.md §ツール進捗表示](./channels.md#3-discord) 参照） |

### 3.5 Telegram チャネル（`channels.telegram`）

Telegram は Discord と同一の Multi-Agent 仕様をサポートする。
複数 Bot 定義 (`telegram_bots`)、チャットごとのエージェント選択 (`telegram_channels`)、
`@mention` によるルーティングが可能。

> **フィールド名の注意**: Discord が `bots` / `channels` なのに対し、Telegram は `telegram_bots` / `telegram_channels` とプレフィックス付きのキー名になる（Discord と Telegram で非対称）。

| フィールド | 型 | 必須 | デフォルト | 説明 |
|---|---|---|---|---|
| `enabled` | `bool` | 任意 | `false` | Telegram Bot の有効化 |
| `telegram_bots` | `map<BotId, TelegramBotConfig>` | 条件付き | なし | Bot 定義マップ。有効時は必須。キーが Bot ID |
| `telegram_channels` | `map<i64, TelegramChatConfig>` | 任意 | なし | チャットごとの設定。キーが chat ID。キー存在 = 許可 |

#### `TelegramBotConfig` のフィールド

| フィールド | 型 | 必須 | デフォルト | 説明 |
|---|---|---|---|---|
| `token` | `string \| SecretRef` | 必須 | なし | Telegram Bot トークン。SecretRef 使用可能。環境変数 `TELEGRAM_BOT_TOKEN` でも指定可能。秘匿フィールド |

#### `TelegramChatConfig` のフィールド

| フィールド | 型 | デフォルト | 説明 |
|---|---|---|---|
| `require_mention` | `bool` | `false` | `true` の場合 @mention なしでは応答しない |
| `agents` | `[string]` | `["default"]` | チャットにバインドするエージェント ID リスト |
| `multi_agent` | `bool` | `false` | `true` の場合 Multi-Agent ルームとして動作。`@mention` で Bot に紐づくエージェントが応答し、非メンション時は Channel Log のみ記録 |
| `secret` | `bool` | `false` | `true` の場合、このチャットの会話を `secret.db` に隔離して保存。Sleep Batch・PULSE はこのチャットの内容に触れない。Web / TUI では未対応。内部的には `ConversationScope::Secret` にマッピングされる（[architecture.md §7.1](./architecture.md#71-conversationscopeストレージ境界) 参照） |
| `tool_progress` | `bool` | `false` | `true` の場合、5 秒以上かかるターンでツール実行状況を編集式の累積ログとして投稿する（[channels.md §ツール進捗表示](./channels.md#4-telegram) 参照） |

### 3.6 Voice チャネル（`channels.voice`）

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

HTTP 契約は [api.md §2.7](./api.md#27-voice-turn)、責務境界と session identity は [voice-channel.md](./voice-channel.md) を参照。設定例は §2 の完全 YAML 例を参照。

### 3.7 Sleep Batch 設定（`sleep_batch`）

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

### 3.8 Pulse 設定（`pulse`）

Pulse（注意活性化）のスケジューラ設定。

| フィールド | 型 | 必須 | デフォルト | 説明 |
|---|---|---|---|---|
| `pulse.enabled` | `bool` | 任意 | `false` | Pulse 機能の有効/無効 |
| `pulse.tick_interval` | `string` | 任意 | `"1m"` | due scan の周期。Duration 形式（例: `30s`, `5m`, `1h`, `1h30m`） |

### 3.9 DB バックアップ設定（`db.backup`）

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

### 3.10 Web Fetch 設定（`web_fetch`）

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

### 3.11 エージェント定義（`agents.<id>`）

`agents` はキーがエージェント ID のマップ。複数定義可能。

| フィールド | 型 | 必須 | デフォルト | 説明 |
|---|---|---|---|---|
| `label` | `string` | 任意 | エージェント ID | UI 上の表示名 |
| `provider` | `string \| null` | 任意 | `null` | エージェント固有のプロバイダー ID。`null` なら `default_provider` |
| `model` | `string \| null` | 任意 | `null` | エージェント固有のモデル名。`null` ならモデル解決チェーンに従う |
| `discord_bot` | `string \| null` | 任意 | `null` | このエージェントが紐づく Discord Bot ID。`channels.discord.bots` のキーを参照 |
| `telegram_bot` | `string \| null` | 任意 | `null` | このエージェントが紐づく Telegram Bot ID。`channels.telegram.telegram_bots` のキーを参照 |
| `profiles` | `map` | 任意 | `{}` | チャネル別オーバーライド。キーがチャネル名（例: `voice`） |

#### `profiles.<channel_name>` のフィールド

| フィールド | 型 | 必須 | デフォルト | 説明 |
|---|---|---|---|---|
| `provider` | `string \| null` | 任意 | `null` | このチャネルでのプロバイダー ID。省略時は `agent.provider` を引き継ぐ |
| `model` | `string \| null` | 任意 | `null` | このチャネルでのモデル名。省略時はモデル解決チェーンに従う |

`profiles` の利用例（完全な実装例は §2 を参照）:

```yaml
agents:
  lyre:
    label: Lyre
    provider: sakura
    model: preview/Kimi-K2.6
    discord_bot: lyre
    profiles:
      voice:
        provider: openrouter
        model: gpt-4.1-mini
```

---

## 4. モデル解決チェーン

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

## 5. SecretRef（シークレット参照）

### 5.1 概要

秘匿フィールド（`api_key`, `auth_token`, `bot_token`）は、YAML に平文を直接書く代わりに SecretRef オブジェクトで外部ソースを参照できる。

### 5.2 参照ソース

| source | フィールド | 解決順序 |
|---|---|---|
| `env` | `id: VAR_NAME` | プロセス環境変数 → `~/.egopulse/.env` |
| `exec` | `command: "cmd"` | コマンドを実行し stdout を取得（10 秒タイムアウト） |

### 5.3 SecretRef の記述例

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

### 5.4 解決レイヤー

SecretRef 解決は以下の 2 層で構成される。

1. **Layer 1: YAML SecretRef 解決** — `{ source: env, id: X }` をプロセス環境変数 → `.env` ファイルの順で解決
2. **Layer 2: プロセス環境変数オーバーライド** — `WEB_AUTH_TOKEN` 等が Layer 1 の結果を上書き（§6 参照）

### 5.5 環境変数名の規約

| 用途 | 環境変数名 |
|---|---|
| プロバイダー API キー | `{PROVIDER_ID}_API_KEY`（例: `OPENAI_API_KEY`） |
| Web 認証トークン | `WEB_AUTH_TOKEN` |
| Voice 認証トークン | 任意の SecretRef ID（推奨: `EGOPULSE_VOICE_AUTH_TOKEN`） |
| Discord Bot トークン | `DISCORD_BOT_TOKEN` |
| Telegram Bot トークン | `TELEGRAM_BOT_TOKEN` |

---

## 6. 環境変数オーバーライド

Config YAML の値を環境変数で上書き可能。環境変数が設定されている場合、YAML の値より優先される。

| 環境変数 | 対象フィールド | 型 |
|---|---|---|
| `LOG_LEVEL` | `log_level` | `string` |
| `WEB_HOST` | `channels.web.host` | `string` |
| `WEB_PORT` | `channels.web.port` | `u16` |
| `WEB_ENABLED` | `channels.web.enabled` | `bool` |
| `WEB_AUTH_TOKEN` | `channels.web.auth_token` | `string` |
| `WEB_ALLOWED_ORIGINS` | `channels.web.allowed_origins` | `string`（カンマ区切り） |
| `DISCORD_BOT_TOKEN` | `channels.discord.bots.*.token` | `string`（SecretRef 経由） |
| `TELEGRAM_BOT_TOKEN` | `channels.telegram.telegram_bots.*.token` | `string`（SecretRef 経由） |

> ※ Discord / Telegram Bot トークンは SecretRef `{ source: env, id: <VAR_NAME> }` で解決（§5 参照）。Layer 2 の直接オーバーライドは非対応。

---

## 7. プロバイダープリセット

セットアップウィザードで選択可能なプリセット一覧。各プリセットは `base_url` と推奨 `default_model` を内包しており、選択すると自動入力される。

| ID | サービス | base_url |
|---|---|---|
| `openai` | OpenAI | `https://api.openai.com/v1` |
| `openai-codex` | OpenAI Codex (OAuth) | `https://chatgpt.com/backend-api/codex` |
| `openrouter` | OpenRouter | `https://openrouter.ai/api/v1` |
| `ollama` | Ollama (local) | `http://127.0.0.1:11434/v1` |
| `google` | Google AI | `https://generativelanguage.googleapis.com/v1beta/openai` |
| `aliyun-bailian` | Alibaba Cloud Bailian | `https://coding.dashscope.aliyuncs.com/v1` |
| `alibaba` | Alibaba Cloud (Qwen / DashScope) | `https://dashscope.aliyuncs.com/compatible-mode/v1` |
| `qwen-portal` | Qwen Portal (OAuth) | `https://portal.qwen.ai/v1` |
| `deepseek` | DeepSeek | `https://api.deepseek.com/v1` |
| `synthetic` | Synthetic | `https://api.synthetic.new/openai/v1` |
| `chutes` | Chutes | `https://llm.chutes.ai/v1` |
| `moonshot` | Moonshot AI | `https://api.moonshot.cn/v1` |
| `mistral` | Mistral AI | `https://api.mistral.ai/v1` |
| `azure` | Microsoft Azure AI | `https://YOUR-RESOURCE.openai.azure.com/openai/deployments/YOUR-DEPLOYMENT` |
| `bedrock` | Amazon AWS Bedrock | `https://bedrock-runtime.YOUR-REGION.amazonaws.com/openai/v1` |
| `zhipu` | Zhipu AI (GLM / Z.AI) | `https://open.bigmodel.cn/api/paas/v4` |
| `zai` | Z.AI Coding | `https://api.z.ai/api/coding/paas/v4` |
| `minimax` | MiniMax | `https://api.minimax.io/v1` |
| `cohere` | Cohere | `https://api.cohere.ai/compatibility/v1` |
| `tencent` | Tencent AI Lab | `https://api.hunyuan.cloud.tencent.com/v1` |
| `xai` | xAI (Grok) | `https://api.x.ai/v1` |
| `nvidia` | NVIDIA NIM | `https://integrate.api.nvidia.com/v1` |
| `huggingface` | Hugging Face | `https://router.huggingface.co/v1` |
| `together` | Together AI | `https://api.together.xyz/v1` |
| `local` | Local OpenAI-compatible | `http://127.0.0.1:1234/v1` |
| `lmstudio` | LM Studio (local) | `http://127.0.0.1:1234/v1` |
| `custom` | Custom | ユーザー入力 |

---

## 8. デフォルトパス

| 用途 | パス | 備考 |
|---|---|---|
| 設定ファイル | `~/.egopulse/egopulse.config.yaml` | 環境変数で変更不可 |
| シークレットファイル | `~/.egopulse/.env` | `KEY=VALUE`（1 行 1 エントリ、`#` コメント対応）。パーミッション `0600`。`source: env` の SecretRef が参照（§5） |
| データディレクトリ | `~/.egopulse/data` | SQLite 等 |
| ワークスペース | `~/.egopulse/workspace` | セッション・履歴データ |

---

## 9. セットアップウィザード

`egopulse setup` で起動する対話型設定ウィザード（dialoguer ベースの順次プロンプト）。
Agent-First 設計に基づき、エージェント名を最初に問い、LLM と対話するために必要な最小限の項目のみを順次収集する。
通常の text / select / confirm 入力はターミナル上に表示し、API key / Discord Bot Token / Telegram Bot Token のみ hidden 入力にする。

### 9.1 フロー全体像

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
[Q2: Provider]          27 プリセット + Custom (select)
   │                     Custom 選択時のみ base_url 入力を追加
   ▼
[Q3: Model]             preset なら select + Custom model / Custom なら text
   │
   ▼
[Q4: API Key]           password。空欄可、非 localhost 系の空欄は confirm
   │
   ▼
[Q5: Web Channel]       confirm（デフォルト yes）
   │
   ▼
[Q6: Discord]           confirm ── yes のみ Bot Token 入力
   │
   ▼
[Q7: Telegram]          confirm ── yes のみ Bot Token 入力
   │
   ▼
[Review]                生成内容表示 + Save configuration? confirm
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

### 9.2 入力項目仕様（Q1〜Q7）

| Q | 項目 | 入力種別 | 必須 | デフォルト | 備考 |
|---|---|---|:---:|---|---|
| Q1 | Agent Label | text | ○ | なし | エージェントの表示名。空入力時は `"Default"` にフォールバック |
| — | Agent ID | (自動) | — | label を slugify | lowercase + 英数字以外をハイフン化 + 連続ハイフン圧縮。空結果は `"default"` にフォールバック |
| Q2 | Provider | select | ○ | なし | 27 プリセット（§7 プロバイダープリセット）+ `Custom` |
| Q2' | base_url | text | 条件付き | なし | `Custom` 選択時のみ追加質問。URL 検証あり |
| Q3 | Model | select / text | ○ | preset の `default_model` | preset 選択時は select 末尾の `Custom model...` で自由入力可、`Custom` provider 選択時は text |
| Q4 | API Key | password | △ | 空文字 | 常に入力ステップを表示。localhost 系は空欄でそのまま通す |
| Q5 | Web Channel | confirm | — | `yes` | 無効時は `channels.web` エントリ自体を YAML に含めない |
| — | Web auth_token | (自動) | — | `generate_auth_token()` | ユーザー入力なし。実値は Review / Done で非表示 |
| Q6 | Discord | confirm | — | `no` | `yes` のみ Bot Token 入力 (password) へ分岐 |
| Q7 | Telegram | confirm | — | `no` | `yes` のみ Bot Token 入力 (password) へ分岐 |

各質問の分岐仕様:

- **Q2 Provider**: `Custom` 選択時のみ直後に base_url 入力を追加。preset 選択時は base_url を preset のデフォルトで自動補完
- **Q3 Model**: preset 選択時は `models` リストからの select。末尾の `Custom model...` を選ぶとモデル名を自由入力できる。`Custom` provider 選択時は最初から text モードに切替
- **confirm 入力**: `y` / `yes` / `n` / `no` をテキスト入力として受け付ける。空入力はデフォルト値を選択する
- **Q4 API Key**: ステップ自体は常に表示。非 localhost 系プロバイダで空欄入力時は `Proceed with an empty key?` で確認。`no` で再入力、`yes` で警告付きで進行
- **Q6 / Q7**: `yes` の場合のみ Bot Token (password 入力) を追加質問。空トークンは拒否

### 9.3 既存設定の再編集（prefill）

`egopulse setup` を既存設定が存在する状態で実行した場合、各 Q のプロンプトは既存値をデフォルトとして事前入力する。Enter で進めば既存値を維持、入力し直せば上書き。

| Q | 事前入力される値 |
|---|---|
| Q1 Agent Label | 既存 `agents.<default_agent>.label`（無ければ空） |
| Q2 / Q2' | 既存 `default_provider`（preset 非一致なら `Custom` 扱いで base_url も事前入力） |
| Q3 Model | 既存 `providers.<id>.default_model` |
| Q4 API Key | YAML 上の `api_key` が文字列の場合のみ事前入力（SecretRef 形式や解決不能時は空） |
| Q5 Web | 既存 `channels.web.enabled`（無ければ `yes`） |
| Q6 Discord | 既存 `channels.discord.enabled` |
| Q7 Telegram | 既存 `channels.telegram.enabled` |

既存 YAML のパースエラー時は Q1 の前に警告表示 + `Continue with empty defaults?` で確認する（黙殺は廃止）。`WEB_AUTH_TOKEN` と `state_root` は事前入力対象外だが上書きされない。

---

## 10. 設定の変更インターフェース

設定の読み取り・書き込みは以下のインターフェースから行える：

| インターフェース | 読み取り | 書き込み | 対象 |
|---------|:---:|:---:|------|
| YAML 手動編集 | 全フィールド | 全フィールド | `~/.egopulse/egopulse.config.yaml` |
| Setup Wizard | — | Agent / Provider / Model / Web / Discord / Telegram 初期設定 | 初回セットアップ・再セットアップ |
| WebUI (`/api/config`) | 公開フィールド | 公開フィールド | ランタイム中の設定変更 |
| スラッシュコマンド (`/provider`, `/model`) | ○ | ○ | プロバイダー・モデルの動的切替 |

---

## 11. 再起動要否と秘匿フィールド

### 11.1 再起動が必要なフィールド

以下のフィールドは起動時に固定される（プロセススナップショットから参照、またはネットワーク接続の確立が伴う）。変更後はプロセスの再起動が必要。

| フィールド | 理由 |
|---|---|
| `timezone` | 各スケジューラが起動時スナップショットを参照 |
| `log_level` | ロガーの初期化が伴う |
| `compaction_timeout_secs` / `compaction_threshold_ratio` / `compaction_target_ratio` / `compact_keep_recent` | 起動時スナップショットから参照 |
| `max_history_messages` | 起動時スナップショットから参照 |
| `default_context_window_tokens` | 起動時スナップショットから参照 |
| `sleep_batch.*` | sleep scheduler が起動時スナップショットを参照 |
| `pulse.*` | pulse scheduler が起動時スナップショットを参照 |
| `db.backup.*` | backup scheduler が起動時スナップショットを参照 |
| `web_fetch.*` | ツール起動時に `Arc<Config>` スナップショットを保持 |
| `providers.<id>.models.<model>.context_window_tokens` | 起動時スナップショットから参照 |
| `providers.<id>.models.<model>.model_instructions` / `model_instructions_file` | system prompt 構築が起動時スナップショットを参照 |
| `channels.web.enabled` / `host` / `port` | Web サーバーの起動/停止・バインド変更 |
| `channels.web.auth_token` / `allowed_origins` | Web 層が起動時スナップショットを参照 |
| `channels.voice.enabled` | Voice route の mount / unmount |
| `channels.voice.auth_token` | Voice 認証 middleware の credential 更新 |
| `channels.voice.default_surface` / `default_session` / `allowed_surfaces` | request default / access control の更新 |
| `channels.discord.enabled` | Discord Bot の接続/切断 |
| `channels.discord.bots.<bot_id>.token` | Bot 認証の再確立 |
| `channels.discord.channels` | チャンネルアクセス制御・メンション要件・秘密モードの変更 |
| `channels.telegram.enabled` | Telegram Bot の接続/切断 |
| `channels.telegram.telegram_bots` | Bot 定義の更新 |
| `channels.telegram.telegram_channels` | チャットアクセス制御・メンション要件・秘密モードの変更 |

> **`model_instructions_file` の例外**: フィールド自体（参照先パス）の変更は再起動が必要だが、**参照先ファイルの内容**は毎ターン再読込される。したがってファイル内容の追記・編集は再起動なしで反映される。

### 11.2 ホットリロード可能なフィールド

以下のフィールドは、LLM 解決チェーン（`llm_for_context`）が毎ターンディスクから再読込するため、変更が次回メッセージ送信時に即座に反映される。プロセスの再起動は不要。

| フィールド | 理由 |
|---|---|
| `default_provider` | 次回メッセージ送信時に LLM 解決チェーンで再読込 |
| `default_model` | 同上 |
| `default_agent` | 同上 |
| `agents.<id>.provider` / `model` / `profiles.*` / `discord_bot` / `telegram_bot` | エージェント定義は LLM 解決チェーンで都度再読込 |
| `providers.<id>.label` / `base_url` / `api_key` / `default_model` | プロバイダー定義は LLM 解決チェーンで都度再読込。プロバイダークライアントは `base_url` / `api_key` を含む cache key でキャッシュされるため、これらの変更も即座に反映される |

> これらは `/provider` / `/model` スラッシュコマンド経由でも更新可能（§10 参照）。

### 11.3 秘匿フィールド

以下のフィールドは API レスポンスでマスクされ、`has_*` 真偽値のみ返却される。

| フィールド | API での表現 |
|---|---|
| `providers.<id>.api_key` | `has_api_key: boolean` |
| `channels.web.auth_token` | `web_auth_enabled: boolean` |
| `channels.voice.auth_token` | API レスポンスへ返却しない |
| `channels.discord.bots.<bot_id>.token` | 返却なし |
| `channels.telegram.telegram_bots.<bot_id>.token` | 返却なし |
