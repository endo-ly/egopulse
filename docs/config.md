# EgoPulse 設定仕様書

EgoPulse の設定システム全体を統合した仕様書。Config YAML の全フィールド、セットアップウィザード、WebUI 設定画面、CLI ランタイムコマンドの各インターフェースが対象範囲と機能を網羅する。

## 目次

1. [Config YAML 設計思想](#1-config-yaml-設計思想)
2. [完全フィールドリファレンス](#2-完全フィールドリファレンス)
3. [モデル解決チェーン](#3-モデル解決チェーン)
4. [SecretRef（シークレット参照）](#4-secretrefシークレット参照)
5. [環境変数オーバーライド](#5-環境変数オーバーライド)
6. [プロバイダープリセット](#6-プロバイダープリセット)
7. [デフォルトパス](#7-デフォルトパス)
8. [セットアップウィザード](#8-セットアップウィザード)
9. [WebUI 設定画面](#9-webui-設定画面)
10. [再起動要否と秘匿フィールド](#10-再起動要否と秘匿フィールド)
11. [インターフェース間比較マトリクス](#11-インターフェース間比較マトリクス)
12. [モジュールアーキテクチャ](#12-モジュールアーキテクチャ)

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
| `bot_token` | `string \| SecretRef` | 条件付き | なし | Discord Bot トークン。有効時は必須。SecretRef 使用可能。環境変数 `DISCORD_BOT_TOKEN` でも指定可能。秘匿フィールド |
| `provider` | `string \| null` | 任意 | `null` | このチャネル専用のプロバイダーオーバーライド |
| `model` | `string \| null` | 任意 | `null` | このチャネル専用のモデルオーバーライド |
| `allowed_channels` | `[u64]` | 任意 | `[]` | Bot が応答するギルドチャンネル ID。空の場合はギルドメッセージを全拒否（DM は常に許可）。リスト内のチャンネルでは @mention なしで即応答する |

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
    bot_token: DISCORD_BOT_TOKEN_HERE
    provider: openrouter
    model: anthropic/claude-sonnet-4
    allowed_channels:
      - 1234567890123456789
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

```
channel.model（チャネル固有モデル指定）
    ↓ null の場合
config.default_model（グローバルモデル上書き）
    ↓ null の場合
provider.default_model（プロバイダーのデフォルトモデル）
```

各ステップの説明:

1. **`channel.model`**: チャネル設定にモデルが指定されていれば、それが最優先。Discord だけ別モデルを使う、といった運用が可能。
2. **`config.default_model`**: チャネル指定がない場合のグローバルフォールバック。全チャネルで統一モデルを使いたい場合に設定する。
3. **`provider.default_model`**: 最終フォールバック。プロバイダー定義に記述されたデフォルトモデルが使われる。

プロバイダー解決も同様のチェーン:

```
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
    bot_token:
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
| `DISCORD_BOT_TOKEN` | `channels.discord.bot_token` | `string` |
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

`egopulse setup` で起動する Ratatui ベースの TUI ウィザード。初回設定または再設定に使用する。

### 8.1 ウィザードのフィールド一覧

| キー | ラベル | UI タイプ | 必須 | 秘匿 | 備考 |
|---|---|---|---|---|---|
| `PROVIDER` | Provider profile ID | セレクター（フィルター付きポップアップ） | **必須** | なし | 25 プリセットから選択 |
| `MODEL` | LLM model | セレクター（フィルター付きポップアップ） | 任意 | なし | プリセット選択で自動入力 |
| `BASE_URL` | API base URL | テキスト入力 | **必須** | なし | プリセット選択で自動入力 |
| `API_KEY` | API key | テキスト入力 | 条件付き | **秘匿** | localhost 系プリセットでは不要 |
| `DISCORD_ENABLED` | Enable Discord | トグル | 任意 | なし | デフォルト `false` |
| `DISCORD_BOT_TOKEN` | Discord bot token | テキスト入力 | 条件付き | **秘匿** | Discord 有効時に表示、必須 |
| `TELEGRAM_ENABLED` | Enable Telegram | トグル | 任意 | なし | デフォルト `false` |
| `TELEGRAM_BOT_TOKEN` | Telegram bot token | テキスト入力 | 条件付き | **秘匿** | Telegram 有効時に表示、必須 |
| `TELEGRAM_BOT_USERNAME` | Telegram bot username | テキスト入力 | 条件付き | なし | Telegram 有効時に表示、必須 |

### 8.2 ウィザードの動作仕様

- **プリセット連動**: `PROVIDER` を変更すると、`MODEL` と `BASE_URL` がプリセット値で自動入力される
- **条件付き表示**: `DISCORD_BOT_TOKEN` は `DISCORD_ENABLED=true` の場合のみ表示。`TELEGRAM_BOT_TOKEN` / `TELEGRAM_BOT_USERNAME` も同様に `TELEGRAM_ENABLED=true` の場合のみ表示
- **Web チャネル固定**: Web チャネルは常に有効化される。`auth_token` は自動生成される
- **トークン保護**: 再セットアップ時、既存の `auth_token` は上書きされず保持される
- **SecretRef 保存**: `API_KEY` / `auth_token` / `bot_token` は YAML に SecretRef を保存し、実値は `.env` に出力する
- **バックアップ**: 既存の設定ファイルは上書き前にバックアップが作成される
- **単一プロバイダー**: ウィザードは 1 つのプロバイダー定義のみ生成する

### 8.3 ウィザードが設定しないフィールド

以下のフィールドはウィザードで設定できず、手動で YAML を編集する必要がある。

- `log_level`
- `compaction_timeout_secs`, `max_history_messages`, `max_session_messages`, `compact_keep_recent`
- `channels.web.host`, `channels.web.port`
- `channels.web.allowed_origins`
- 各チャネルの `provider` / `model` オーバーライド
- `channels.discord.allowed_channels`
- `channels.telegram.allowed_chat_ids`
- 複数プロバイダーの追加

---

## 9. WebUI 設定画面

ブラウザ上で設定を確認・変更するモーダル UI。

### 9.1 API エンドポイント

| メソッド | パス | 説明 |
|---|---|---|
| `GET` | `/api/config` | 現在の設定を取得 |
| `PUT` | `/api/config` | 設定を更新 |

`PUT /api/config` で更新されるプロバイダー `api_key` も、YAML へは SecretRef として保存され、実値は `.env` に保存される。

### 9.2 レスポンスペイロード（GET）

`ConfigPayload` 構造体で返却される値。

```typescript
{
  // グローバル LLM
  default_provider: string,
  default_model: string | null,
  effective_model: string,           // 解決後の実際のモデル名

  // パス情報
  data_dir: string,
  workspace_dir: string,
  config_path: string,

  // Web サーバー
  web_enabled: boolean,
  web_host: string,
  web_port: number,
  web_auth_enabled: boolean,         // auth_token の有無

  // API キー状態
  has_api_key: boolean,              // デフォルトプロバイダーの API キー有無

  // プロバイダー一覧
  providers: [
    {
      id: string,
      label: string,
      base_url: string,
      default_model: string,
      models: string[],
      has_api_key: boolean            // 秘匿値は返却しない
    }
  ],

  // チャネルオーバーライド
  channel_overrides: {
    web?:     { provider?: string, model?: string },
    discord?: { provider?: string, model?: string },
    telegram?:{ provider?: string, model?: string }
  }
}
```

### 9.3 更新リクエスト（PUT）

`ConfigUpdateRequest` 構造体で送信する値。

```typescript
{
  // グローバル LLM
  default_provider: string,
  default_model: string | null,

  // プロバイダー（編集用）
  providers: {
    [id: string]: {
      label: string,
      base_url: string,
      api_key?: string,              // 省略時は変更なし
      default_model: string,
      models: string[]
    }
  },

  // Web サーバー
  web_enabled: boolean,
  web_host: string,
  web_port: number,

  // チャネルオーバーライド
  channel_overrides: {
    web?:     { provider?: string, model?: string },
    discord?: { provider?: string, model?: string },
    telegram?:{ provider?: string, model?: string }
  }
}
```

### 9.4 UI セクション構成

#### セクション 1: Default LLM（編集可能）

| 項目 | UI コンポーネント | 備考 |
|---|---|---|
| プロバイダー | `<select>` | `providers` リストから選択 |
| モデル | `<input>` + `<datalist>` | 選択中プロバイダーの `models` を候補表示 |
| API キー | `<password input>` + Clear ボタン | 現在のキー値は非表示。新規入力またはクリア |

#### セクション 2: Web Server（編集可能）

| 項目 | UI コンポーネント |
|---|---|
| 有効化 | `<checkbox>` |
| ホスト | `<text input>` |
| ポート | `<number input>` |

#### セクション 3: Providers（表示のみ）

各プロバイダーのカード表示。内容は読み取り専用。

- ラベル、ID、`base_url`、`default_model`、`models` 一覧
- API キーの有無（バッジ表示）

#### セクション 4: Channel Overrides（編集可能）

| チャネル | 項目 | UI コンポーネント |
|---|---|---|
| Discord | プロバイダー | `<select>` |
| Discord | モデル | `<text input>` |
| Telegram | プロバイダー | `<select>` |
| Telegram | モデル | `<text input>` |

### 9.5 WebUI で非表示のフィールド

以下は API レスポンスや UI に含まれない、または表示のみで編集不可の項目。

| フィールド | 扱われ方 |
|---|---|
| `data_dir`, `workspace_dir` | ヘッダー部にパス表示のみ。編集不可 |
| `config_path` | ヘッダー部にパス表示のみ。編集不可 |
| `channels.web.auth_token` | 「認証あり/なし」の表示のみ。トークン値は非公開 |
| `channels.discord.enabled` | 非表示 |
| `channels.discord.bot_token` | 非表示 |
| `channels.discord.allowed_channels` | 非表示 |
| `channels.telegram.enabled` | 非表示 |
| `channels.telegram.bot_token` | 非表示 |
| `channels.telegram.bot_username` | 非表示 |
| `channels.telegram.allowed_chat_ids` | 非表示 |
| `channels.web.allowed_origins` | 非表示 |
| `log_level` | 非表示 |
| `compaction_*`, `max_*` | 非表示 |
| プロバイダーの追加/削除 | 未対応 |

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
| `channels.discord.bot_token` | Bot 認証の再確立 |
| `channels.telegram.enabled` | Telegram Bot の接続/切断 |
| `channels.telegram.bot_token` | Bot 認証の再確立 |
| `log_level` | ロガーの初期化が伴う |

### 10.2 ホットリロード可能なフィールド

以下のフィールドは変更が即座に反映され、再起動は不要。

| フィールド | 理由 |
|---|---|
| `default_provider` | 次回メッセージ送信時に参照 |
| `default_model` | 次回メッセージ送信時に参照 |
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
| `channels.discord.bot_token` | 返却なし |
| `channels.telegram.bot_token` | 返却なし |

---

## 11. インターフェース間比較マトリクス

### 11.1 フィールド別カバレッジ

各インターフェースがどの設定フィールドに対して読み取り（R）・書き込み（W）・表示のみ（V）の能力を持つかを整理する。

| フィールド | YAML（手動） | Setup Wizard | WebUI | スラッシュコマンド (`/provider` `/model`) |
|---|:---:|:---:|:---:|:---:|
| **グローバル** | | | | |
| `default_provider` | R/W | W（`PROVIDER` → 単一生成） | R/W | W（`/provider global`） |
| `default_model` | R/W | W（`MODEL`） | R/W | W（`/model global`） |
| `log_level` | R/W | なし | なし | なし |
| `compaction_timeout_secs` | R/W | なし | なし | なし |
| `max_history_messages` | R/W | なし | なし | なし |
| `max_session_messages` | R/W | なし | なし | なし |
| `compact_keep_recent` | R/W | なし | なし | なし |
| **プロバイダー定義** | | | | |
| `providers.<id>.label` | R/W | W | V（表示のみ） | なし |
| `providers.<id>.base_url` | R/W | W（`BASE_URL`） | V（表示のみ） | なし |
| `providers.<id>.api_key` | R/W | W（`API_KEY`） | R/W（パスワード入力） | なし |
| `providers.<id>.default_model` | R/W | W | V（表示のみ） | なし |
| `providers.<id>.models` | R/W | W（プリセット由来） | V（表示のみ） | R（`/models`） |
| プロバイダー追加/削除 | R/W | なし（1 つのみ生成） | なし | なし |
| **Web チャネル** | | | | |
| `channels.web.enabled` | R/W | なし（常に true） | R/W | なし |
| `channels.web.host` | R/W | なし | R/W | なし |
| `channels.web.port` | R/W | なし | R/W | なし |
| `channels.web.auth_token` | R/W | W（自動生成、再設定時は保持） | V（有効/無効のみ） | なし |
| `channels.web.allowed_origins` | R/W | なし | なし | なし |
| `channels.web.provider` | R/W | なし | なし | なし（Web チャネル未対応） |
| `channels.web.model` | R/W | なし | なし | なし（Web チャネル未対応） |
| **Discord チャネル** | | | | |
| `channels.discord.enabled` | R/W | W（`DISCORD_ENABLED`） | なし | なし |
| `channels.discord.bot_token` | R/W | W（`DISCORD_BOT_TOKEN`） | なし | なし |
| `channels.discord.provider` | R/W | なし | R/W | W（`/provider discord`） |
| `channels.discord.model` | R/W | なし | R/W | W（`/model discord`） |
| `channels.discord.allowed_channels` | R/W | なし | なし | なし |
| **Telegram チャネル** | | | | |
| `channels.telegram.enabled` | R/W | W（`TELEGRAM_ENABLED`） | なし | なし |
| `channels.telegram.bot_token` | R/W | W（`TELEGRAM_BOT_TOKEN`） | なし | なし |
| `channels.telegram.bot_username` | R/W | W（`TELEGRAM_BOT_USERNAME`） | なし | なし |
| `channels.telegram.provider` | R/W | なし | R/W | W（`/provider telegram`） |
| `channels.telegram.model` | R/W | なし | R/W | W（`/model telegram`） |
| `channels.telegram.allowed_chat_ids` | R/W | なし | なし | なし |

### 11.2 インターフェースの役割まとめ

| インターフェース | 目的 | 強み | 弱み |
|---|---|---|---|
| **YAML（手動編集）** | フルコントロール | 全フィールドにアクセス可能。複数プロバイダー、細かなチューニング | エディタが必要。構文エラーのリスク |
| **Setup Wizard** | 初回セットアップ | TUI で直感的。プリセットで素早く設定。バックアップ自動作成 | 単一プロバイダーのみ。システム設定は対象外 |
| **WebUI** | ランタイム設定変更 | ブラウザから操作。プロバイダー一覧が見やすい。モデル/プロバイダー切替 | Bot 関連フィールドは非表示。プロバイダー追加/削除不可 |

---

## 12. モジュールアーキテクチャ

設定モジュールは設定データのライフサイクルに沿って分割されている。

```text
config/
├── mod.rs         型定義 + 公開ファサード
├── loader.rs      Read + Transform（YAML 読込、正規化、検証、環境変数オーバーライド）
├── persist.rs     Write（YAML 保存、アトミック書込、ファイルロック、.env 出力）
├── resolve.rs     Use（LLM 解決、チャネルアクセサ、パス導出）
└── secret_ref.rs  SecretRef 型、解決ロジック、.env 読み書き
```

### データフロー

```text
YAML ファイル
   ↓ loader.rs: Deserialize
FileConfig / FileProviderConfig（部分型、全フィールド Option）
   ↓ loader.rs: normalize + env overlay + validate
Config / ProviderConfig / ChannelConfig（実行時型、不変条件あり）
   ↓ resolve.rs: LLM 解決・チャネルアクセス
ResolvedLlmConfig
   ↓ persist.rs: Serialize → atomic write
YAML ファイル
```

### 型の設計原則

| 型 | 役割 | Deserialize | Serialize |
|---|---|---|---|
| `FileConfig`, `FileProviderConfig`, `FileChannelConfig` | YAML 入力（loader.rs 内 private） | ✅ | ❌ |
| `StringOrRef`, `SecretSource` | YAML SecretRef 表現（secret_ref.rs） | ✅ | ❌ |
| `ResolvedValue` | 解決済みシークレット値。出処を追跡し保存時に復元 | ❌ | ❌ |
| `Config`, `ProviderConfig`, `ChannelConfig` | 実行時ドメイン型（mod.rs で定義） | ❌ | ❌ |
| `SerializableConfig` 等 | YAML 出力（persist.rs 内 private） | ❌ | ✅ |

実行時型は `Deserialize` を導出せず、loader パイプライン経由でのみ構築する。これにより検証をバイパスする不正な構築を防ぐ。

### Newtypes

| 型 | 用途 | 正規化 |
|---|---|---|
| `ProviderId` | プロバイダー識別子（HashMap のキー） | `trim` + `ascii_lowercase` |
| `ChannelName` | チャネル名（HashMap のキー） | `trim` + `ascii_lowercase` |

文字列の取り違えを型レベルで防止する。
