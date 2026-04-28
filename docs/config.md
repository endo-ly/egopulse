# EgoPulse 設定仕様

全設定フィールドの型・制約・デフォルト値の完全リファレンス。

## 目次

1. [Config YAML 設計思想](#1-config-yaml-設計思想)
2. [完全フィールドリファレンス](#2-完全フィールドリファレンス)
3. [モデル解決チェーン](#3-モデル解決チェーン)
4. [SecretRef（シークレット参照）](#4-secretrefシークレット参照)
5. [環境変数オーバーライド](#5-環境変数オーバーライド)
6. [プロバイダープリセット](#6-プロバイダープリセット)
7. [デフォルトパス](#7-デフォルトパス)
8. [セットアップウィザード](#8-セットアップウィザード)
9. [設定の変更インターフェース](#9-設定の変更インターフェース)
10. [再起動要否と秘匿フィールド](#10-再起動要否と秘匿フィールド)

---

## 1. Config YAML 設計思想

### 1.1 基本方針

- **単一ファイル管理**: すべての設定を `~/.egopulse/egopulse.config.yaml` に集約する。環境変数による部分的オーバーライドは可能だが、ファイルが真実の情報源（Single Source of Truth）。
- **OpenAI 互換前提**: すべてのプロバイダーは OpenAI 互換 API エンドポイントとして扱う。ベンダー固有 SDK は使わず、`base_url` の切り替えで対応する。
- **階層的オーバーライド**: グローバル設定 → チャネル設定の順で優先度が高くなり、チャネルごとにプロバイダーやモデルを個別指定できる。
- **エージェントファースト**: API キーなどの秘匿値は、YAML では SecretRef として保持し、実値は `~/.egopulse/.env` に保存する。外部 API レスポンスでは `has_api_key` の真偽値のみ返却し、値そのものはマスクする。

### 1.2 設計上の制約

- **単一プロバイダー生成**: セットアップウィザードは 1 つのプロバイダーのみ生成する。複数プロバイダーの追加は手動で YAML を編集するか WebUI から行う（現在は編集未対応）。
- **チャネル独立**: 各チャネル（Web / Discord / Telegram）は独立して有効化・無効化でき、それぞれにプロバイダーとモデルのオーバーライドが可能。
- **ホットリロード対応と非対応の分離**: 一部フィールドは設定変更後に即座に反映される。サーバーの再起動が必要なフィールドもある（後述）。

---

## 2. 完全フィールドリファレンス

### 2.1 グローバル設定

| フィールド | 型 | 必須 | デフォルト | 説明 |
|---|---|---|---|---|
| `default_provider` | `string` | **必須** | なし | `providers` マップ内のキーを参照。起動時に使用するプロバイダーを決定 |
| `default_model` | `string \| null` | 任意 | `null` | プロバイダーの `default_model` をグローバルに上書き。`null` の場合プロバイダー定義に従う |
| `log_level` | `"info" \| "debug" \| "warn" \| "error"` | 任意 | `"info"` | ログ出力レベル |
| `compaction_timeout_secs` | `u64` | 任意 | `180` | 履歴圧縮（compaction）時の LLM 呼び出しタイムアウト秒数 |
| `max_history_messages` | `usize` | 任意 | `50` | セッション復元時のフォールバックメッセージ取得数 |
| `max_session_messages` | `usize` | 任意 | `40` | 履歴圧縮をトリガーするメッセージ閾値 |
| `compact_keep_recent` | `usize` | 任意 | `20` | 圧縮後に保持する直近メッセージ数 |

### 2.2 プロバイダー定義（`providers.<id>`）

`providers` はキーがプロバイダー ID のマップ。複数定義可能。

| フィールド | 型 | 必須 | 説明 |
|---|---|---|---|
| `label` | `string` | 推奨 | UI 上の表示名 |
| `base_url` | `string` | **必須** | OpenAI 互換 API エンドポイント URL |
| `api_key` | `string \| SecretRef \| null` | 条件付き | API 認証キー。`localhost` 系プロバイダーでは不要。SecretRef 使用可能（後述）。秘匿フィールド |
| `default_model` | `string` | **必須** | このプロバイダーのデフォルトモデル |
| `models` | `[string]` | 任意 | 利用可能なモデル一覧。UI のドロップダウン等で使用 |

### 2.3 Web チャネル（`channels.web`）

| フィールド | 型 | 必須 | デフォルト | 説明 |
|---|---|---|---|---|
| `enabled` | `bool` | 任意 | `false` | Web UI の有効化 |
| `host` | `string` | 任意 | `"127.0.0.1"` | バインドホスト |
| `port` | `u16` | 任意 | `10961` | バインドポート |
| `auth_token` | `string \| SecretRef` | 条件付き | なし | Web 有効時は必須。ブラウザアクセス時の認証トークン。SecretRef 使用可能。秘匿フィールド |
| `allowed_origins` | `[string]` | 任意 | `[]` | WebSocket CORS 許可オリジンリスト |
| `provider` | `string \| null` | 任意 | `null` | このチャネル専用のプロバイダーオーバーライド |
| `model` | `string \| null` | 任意 | `null` | このチャネル専用のモデルオーバーライド |

### 2.4 Discord チャネル（`channels.discord`）

| フィールド | 型 | 必須 | デフォルト | 説明 |
|---|---|---|---|---|
| `enabled` | `bool` | 任意 | `false` | Discord Bot の有効化 |
| `bots` | `map<BotId, DiscordBot>` | 条件付き | なし | Bot 定義。有効時は少なくとも 1 つの Bot が必要 |
| `provider` | `string \| null` | 任意 | `null` | このチャネル専用のプロバイダーオーバーライド |
| `model` | `string \| null` | 任意 | `null` | このチャネル専用のモデルオーバーライド |

#### `bots.<bot_id>` のフィールド

| フィールド | 型 | 必須 | デフォルト | 説明 |
|---|---|---|---|---|
| `token` | `string \| SecretRef` | 必須 | なし | Discord Bot トークン。SecretRef 使用可能。秘匿フィールド |
| `default_agent` | `string` | 必須 | なし | この Bot のデフォルト応答エージェント |
| `allowed_channels` | `[u64]` | 任意 | `[]` | 応答するギルドチャンネル ID。空の場合はギルドメッセージを全拒否（DM は常に許可） |
| `channel_agents` | `map<string → string>` | 任意 | なし | チャンネル ID → エージェント ID のマッピング。該当チャンネルで `default_agent` を上書きする |

### 2.5 Telegram チャネル（`channels.telegram`）

| フィールド | 型 | 必須 | デフォルト | 説明 |
|---|---|---|---|---|
| `enabled` | `bool` | 任意 | `false` | Telegram Bot の有効化 |
| `bot_token` | `string \| SecretRef` | 条件付き | なし | Telegram Bot トークン。有効時は必須。SecretRef 使用可能。環境変数 `TELEGRAM_BOT_TOKEN` でも指定可能。秘匿フィールド |
| `bot_username` | `string` | 条件付き | なし | Bot のユーザー名。グループ内で `@botname` メンション検知に使用。有効時は必須 |
| `provider` | `string \| null` | 任意 | `null` | このチャネル専用のプロバイダーオーバーライド |
| `model` | `string \| null` | 任意 | `null` | このチャネル専用のモデルオーバーライド |
| `allowed_chat_ids` | `[i64]` | 任意 | `[]` | Bot が応答するグループ/スーパーグループの chat ID。空の場合はグループメッセージを全拒否。リスト内のチャットでは @mention なしで即応答する |

### 2.6 完全 YAML 例

```yaml
# グローバル LLM 設定
default_provider: openrouter
default_model: null

# エージェント定義
default_agent: default
agents:
  default:
    label: Default Agent
    model: null
    provider: null
  alice:
    label: Alice
    model: anthropic/claude-sonnet-4
    provider: openrouter

# システム設定
log_level: info
compaction_timeout_secs: 180
max_history_messages: 50
max_session_messages: 40
compact_keep_recent: 20

# プロバイダー定義
providers:
  openrouter:
    label: OpenRouter
    base_url: https://openrouter.ai/api/v1
    api_key: sk-or-v1-xxxxxxxxxxxxx
    default_model: anthropic/claude-sonnet-4
    models:
      - anthropic/claude-sonnet-4
      - google/gemini-2.5-pro
      - openai/gpt-4.1
  ollama:
    label: Ollama (Local)
    base_url: http://localhost:11434/v1
    api_key: null
    default_model: llama3
    models:
      - llama3
      - codellama

# チャネル設定
channels:
  web:
    enabled: true
    host: 127.0.0.1
    port: 10961
    auth_token: my-secret-token-here
    allowed_origins:
      - http://localhost:3000
    provider: null
    model: null
  discord:
    enabled: true
    bots:
      main:
        token:
          source: env
          id: DISCORD_BOT_TOKEN
        default_agent: alice
        allowed_channels:
          - 1234567890123456789
    provider: openrouter
    model: anthropic/claude-sonnet-4
  telegram:
    enabled: false
    bot_token: TELEGRAM_BOT_TOKEN_HERE
    bot_username: my_egopulse_bot
    provider: null
    model: null
    allowed_chat_ids:
      - -1001234567890
```

---

## 3. モデル解決チェーン

メッセージ送信時に使用するモデルは、以下の優先順位で解決される。

```text
agent.model（エージェント固有モデル指定）
    ↓ null の場合
channel.model（チャネル固有モデル指定）
    ↓ null の場合
config.default_model（グローバルモデル上書き）
    ↓ null の場合
provider.default_model（プロバイダーのデフォルトモデル）
```

各ステップの説明:

1. **`agent.model`**: エージェント設定にモデルが指定されていれば、それが最優先。エージェントごとに異なるモデルを使う運用が可能。
2. **`channel.model`**: チャネル設定にモデルが指定されていれば、次に優先。Discord だけ別モデルを使う、といった運用が可能。
3. **`config.default_model`**: エージェント・チャネル指定がない場合のグローバルフォールバック。全チャネルで統一モデルを使いたい場合に設定する。
4. **`provider.default_model`**: 最終フォールバック。プロバイダー定義に記述されたデフォルトモデルが使われる。

> **Note**: `agent.provider` と `agent.model` は独立して解決される。`agent.provider` だけを設定しても、そのプロバイダーの `default_model` は自動適用されない。モデル解決は別途 `agent.model` → `channel.model` → `config.default_model` → `provider.default_model` へフォールバックする。

`/provider` / `/model` のデフォルト更新対象は現在の `agent_id`（`agents.<id>.provider` / `agents.<id>.model`）。チャネル設定を変更したい場合は `--scope discord` のように明示する。

プロバイダー解決も同様のチェーン:

```text
agent.provider（エージェント固有プロバイダー指定）
    ↓ null の場合
channel.provider（チャネル固有プロバイダー指定）
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
| Discord Bot トークン | `DISCORD_BOT_TOKEN` |
| Telegram Bot トークン | `TELEGRAM_BOT_TOKEN` |

---

## 5. 環境変数オーバーライド

Config YAML の値を環境変数で上書き可能。環境変数が設定されている場合、YAML の値より優先される。

| 環境変数 | 対象フィールド | 型 |
|---|---|---|
| `LOG_LEVEL` | `log_level` | `string` |
| `WEB_HOST` | `channels.web.host` | `string` |
| `WEB_PORT` | `channels.web.port` | `u16` |
| `WEB_ENABLED` | `channels.web.enabled` | `bool` |
| `WEB_AUTH_TOKEN` | `channels.web.auth_token` | `string` |
| `WEB_ALLOWED_ORIGINS` | `channels.web.allowed_origins` | `string`（カンマ区切り） |
| `DISCORD_BOT_TOKEN` | `channels.discord.bots.*.token` | `string` |
| `TELEGRAM_BOT_TOKEN` | `channels.telegram.bot_token` | `string` |

---

## 6. プロバイダープリセット

セットアップウィザードで選択可能な 25 のプリセット一覧。各プリセットは `base_url` と推奨 `default_model` を内包しており、選択すると自動入力される。

| ID | サービス | base_url |
|---|---|---|
| `openai` | OpenAI | `https://api.openai.com/v1` |
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

## 7. デフォルトパス

| 用途 | パス | 備考 |
|---|---|---|
| 設定ファイル | `~/.egopulse/egopulse.config.yaml` | 環境変数で変更不可 |
| データディレクトリ | `~/.egopulse/data` | SQLite 等 |
| ワークスペース | `~/.egopulse/workspace` | セッション・履歴データ |

---

## 8. セットアップウィザード

`egopulse setup` で起動する対話型 TUI ウィザード。
プロバイダープリセットから選択し、必要に応じて Discord / Telegram Bot を設定する。

### 設定可能項目

| 項目 | 必須 | 備考 |
|------|:---:|------|
| プロバイダー選択（25 プリセットから選択） | **必須** | `PROVIDER` → `BASE_URL` → `MODEL` の順に連動入力 |
| API キー | 条件付き | localhost 系（Ollama, LMStudio）では不要 |
| Discord Bot トークン | 任意 | Discord 有効時は必須 |
| Telegram Bot トークン・ユーザー名 | 任意 | Telegram 有効時は必須 |

### 動作

- Web チャネルは常に有効化。`auth_token` は自動生成される
- 秘匿値は YAML に SecretRef、実値は `.env` に保存
- 既存設定ファイルは上書き前にバックアップ
- 単一プロバイダーのみ生成（複数追加は手動編集）

### ウィザードで設定できないフィールド

- `log_level`, `compaction_*`, `max_*`
- `channels.web.host`, `channels.web.port`, `channels.web.allowed_origins`
- 各チャネルの `provider` / `model` オーバーライド
- チャネル別アクセス制御（`allowed_channels`, `allowed_chat_ids`）
- 複数プロバイダーの追加

---

## 9. 設定の変更インターフェース

設定の読み取り・書き込みは以下のインターフェースから行える：

| インターフェース | 読み取り | 書き込み | 対象 |
|---------|:---:|:---:|------|
| YAML 手動編集 | 全フィールド | 全フィールド | `~/.egopulse/egopulse.config.yaml` |
| Setup Wizard | — | プロバイダー、Bot トークン | 初回セットアップ |
| WebUI (`/api/config`) | 公開フィールド | 公開フィールド | ランタイム中の設定変更 |
| スラッシュコマンド (`/provider`, `/model`) | ○ | ○ | プロバイダー・モデルの動的切替 |

WebUI の設定 API 仕様は [api.md](./api.md) を参照。
スラッシュコマンドの仕様は [commands.md](./commands.md) を参照。

---

## 10. 再起動要否と秘匿フィールド

### 10.1 再起動が必要なフィールド

以下のフィールドを変更した場合、プロセスの再起動が必要。

| フィールド | 理由 |
|---|---|
| `channels.web.enabled` | Web サーバーの起動/停止が伴う |
| `channels.web.host` | バインドアドレスの変更 |
| `channels.web.port` | バインドポートの変更 |
| `channels.discord.enabled` | Discord Bot の接続/切断 |
| `channels.discord.bots.<bot_id>.token` | Bot 認証の再確立 |
| `channels.telegram.enabled` | Telegram Bot の接続/切断 |
| `channels.telegram.bot_token` | Bot 認証の再確立 |
| `log_level` | ロガーの初期化が伴う |

### 10.2 ホットリロード可能なフィールド

以下のフィールドは変更が即座に反映され、再起動は不要。

| フィールド | 理由 |
|---|---|
| `default_provider` | 次回メッセージ送信時に参照 |
| `default_model` | 次回メッセージ送信時に参照 |
| `default_agent` | 次回メッセージ送信時に参照 |
| `agents` の内容 | エージェント定義は都度読み込み |
| `providers` の内容 | プロバイダー定義は都度読み込み |
| `channels.*.provider` | チャネルオーバーライドは都度参照 |
| `channels.*.model` | チャネルオーバーライドは都度参照 |
| `compaction_*` | 次回圧縮時に参照 |
| `max_*` | 次回セッション操作時に参照 |

### 10.3 秘匿フィールド

以下のフィールドは API レスポンスでマスクされ、`has_*` 真偽値のみ返却される。

| フィールド | API での表現 |
|---|---|
| `providers.<id>.api_key` | `has_api_key: boolean` |
| `channels.web.auth_token` | `web_auth_enabled: boolean` |
| `channels.discord.bots.<bot_id>.token` | 返却なし |
| `channels.telegram.bot_token` | 返却なし |

---
