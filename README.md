# EgoPulse

EgoPulse は EgoGraph 向けの Rust runtime foundation です。
local TUI と developer 向け CLI に加えて、React/Vite ベースの WebUI、SSE によるチャット stream、WebSocket gateway、そして Discord / Telegram チャネルアダプターを提供します。

## Prerequisites

- Rust stable
- `cargo fmt`
- `cargo clippy`
- Node.js / npm

## Config

環境変数または `egopulse.config.yaml` に対応します。

読み込み順は次の通りです。

1. プロセス環境変数
2. `--config` で指定した YAML
3. current directory の `./egopulse.config.yaml` 自動検出

同じキーが複数箇所にある場合は、上の項目が優先されます。

### OpenAI-compatible environment variables

```bash
export EGOPULSE_MODEL="gpt-4o-mini"
export EGOPULSE_API_KEY="sk-..."
export EGOPULSE_BASE_URL="https://api.openai.com/v1"
export EGOPULSE_DATA_DIR=".egopulse"
export EGOPULSE_LOG_LEVEL="info"
```

ローカルの OpenAI-compatible server を使う場合は、`localhost` / `127.0.0.1` / `0.0.0.0` / `::1` の base URL に限り `EGOPULSE_API_KEY` を省略できます。

Web の有効化や bind 設定は YAML の `channels.web` で管理します。
`EGOPULSE_WEB_ENABLED` / `EGOPULSE_WEB_HOST` / `EGOPULSE_WEB_PORT` / `EGOPULSE_WEB_AUTH_TOKEN` / `EGOPULSE_WEB_ALLOWED_ORIGINS` を使う場合でも、実行時には `channels.web` 相当の値として扱われます。

Discord / Telegram の bot token は環境変数で上書きできます。

```bash
export EGOPULSE_DISCORD_BOT_TOKEN="your-discord-bot-token"
export EGOPULSE_TELEGRAM_BOT_TOKEN="your-telegram-bot-token"
```

### Endpoint examples

Default OpenAI-compatible endpoint:

```bash
export EGOPULSE_MODEL="gpt-4o-mini"
export EGOPULSE_API_KEY="sk-..."
export EGOPULSE_BASE_URL="https://api.openai.com/v1"
```

OpenAI-compatible router endpoint example:

```bash
export EGOPULSE_MODEL="openai/gpt-4o-mini"
export EGOPULSE_API_KEY="sk-or-..."
export EGOPULSE_BASE_URL="https://openrouter.ai/api/v1"
```

Local OpenAI-compatible endpoint example:

```bash
export EGOPULSE_MODEL="local-model"
export EGOPULSE_BASE_URL="http://127.0.0.1:1234/v1"
```

### Config file

サンプルは [`egopulse.config.example.yaml`](./egopulse.config.example.yaml) を参照してください。  
`egopulse.config.yaml` は current directory から自動検出されます。明示的に指定したい場合は `--config` を使ってください。

Web の設定は次の形です。

```yaml
channels:
  web:
    enabled: true
    host: 127.0.0.1
    port: 10961
    auth_token: "change-this-token"
    allowed_origins:
      - "https://egopulse.<tailnet>.ts.net"
      - "http://127.0.0.1:10961"
```

`channels.web.enabled: true` のとき `auth_token` は必須です。`/api/*` は `Authorization: Bearer <token>`、`/ws` は最初の `connect` frame に `authToken` が必須になります。WebUI は未認証時に token 入力モーダルを表示し、入力値をブラウザの localStorage に保存します。

`auth_token` は利用者が自分で生成して `egopulse.config.yaml` に設定してください。

```bash
openssl rand -base64 32
```

`allowed_origins` を設定すると `/ws` の `Origin` を allowlist 照合します。未設定の場合は `Origin` と `Host` の host:port が一致する同一ホスト接続だけを許可します。

```bash
cargo run -p egopulse -- --config /path/to/egopulse.config.yaml ask "hello"
```

Discord / Telegram の設定例:

```yaml
channels:
  discord:
    enabled: true
    bot_token: your-discord-bot-token
  telegram:
    enabled: true
    bot_token: your-telegram-bot-token
    bot_username: your_bot_username
```

## Install

この repo では専用の installer は用意していません。  
サポートする導線は cargo ベースのみです。

```bash
cargo install --path egopulse --locked
egopulse
```

`cargo install --path egopulse --locked` を再実行すると、インストール済みバイナリは更新されます。

## Usage

開発中は source checkout からそのまま起動できます。

```bash
cargo run -p egopulse
```

`egopulse` を無引数で起動すると local TUI が開きます。

developer 向けの entrypoint はそのまま残っています。

```bash
cargo run -p egopulse -- ask "hello"
cargo run -p egopulse -- chat --session local-dev
cargo run -p egopulse -- ask --session local-dev "remember my last question?"
```

`ask` は OpenAI-compatible endpoint に対する単発問い合わせです。  
`chat --session ...` は persistent SQLite session を使った継続会話です。

### HTTP Server with WebUI

`web` サブコマンドで HTTP サーバーを起動し、React/Vite 製 WebUI、SSE chat stream、WebSocket gateway を公開します。`--host` / `--port` を省略した場合は `egopulse.config.yaml` の `channels.web.host` / `channels.web.port` を使います。

```bash
cargo run -p egopulse -- web
cargo run -p egopulse -- --config egopulse.config.yaml web --host 0.0.0.0 --port 8080
```

Endpoints:
- `GET /` - WebUI
- `GET /health` - Health check
- `GET /api/health` - Health check (Bearer 必須)
- `GET /api/config` - Runtime config 取得 (Bearer 必須)
- `PUT /api/config` - Runtime config 保存 (Bearer 必須)
- `GET /api/sessions` - List sessions (Bearer 必須)
- `GET /api/history?session_key=...` - Get message history (Bearer 必須)
- `POST /api/send_stream` - chat run を開始して `run_id` を返す (Bearer 必須)
- `GET /api/stream?run_id=...` - SSE で run event を購読 (Bearer 必須)
- `GET /ws` - WebSocket gateway (`Origin` 検証 + `connect.authToken` 認証)

現在の WebUI では次ができます。
- セッション一覧の表示
- セッション履歴の表示
- Runtime Config の表示と保存
- SSE 経由の live chat
- WebSocket gateway への接続確認
- `channels.web.auth_token` 入力による Web API / WebSocket 認証

会話履歴と session snapshot は `EGOPULSE_DATA_DIR/egopulse.db` に保存されます。
Issue 2.5 の local TUI では、session 一覧から再開・新規開始ができます。

### Channel Server (Discord / Telegram / Web)

`start` サブコマンドで設定ファイルに基づいて有効なチャネルを一括起動します。microclaw 互換の起動パターンです。

```bash
cargo run -p egopulse -- start
cargo run -p egopulse -- --config egopulse.config.yaml start
```

Web は `channels.web.enabled: true` で起動し、Discord / Telegram は `channels.<name>.enabled: true` かつ `bot_token` が設定されていると起動します。Web、Discord、Telegram を同時に稼働でき、Ctrl-C で全チャネルを graceful shutdown します。

各チャネルのメッセージは `SurfaceContext` に正規化され、agent runtime で処理された結果が各プラットフォームの文字数制限に合わせて自動分割されて返信されます。

| チャネル | 文字数制限 | 受信方式 |
|----------|-----------|---------|
| Discord  | 2000文字  | Gateway (WebSocket) |
| Telegram | 4096文字  | Long Polling |

## Discord セットアップガイド

### 前提条件

- [Discord Developer Portal](https://discord.com/developers/applications) にアクセス可能な Discord アカウント

### 手順

#### 1. Bot Application の作成

1. [Discord Developer Portal](https://discord.com/developers/applications) → **New Application**
2. Application 名を入力 (例: `EgoPulse`) → **Create**
3. 左メニュー **Bot** → **Reset Token** → Token をコピー
4. **Privileged Gateway Intents** を設定:
   - **Message Content Intent**: ON (メッセージ本文を読むために必要)
   - **Server Members Intent**: ON (推奨)
5. **Save Changes**

#### 2. Bot をサーバーに招待

**OAuth2** → **URL Generator**:
- Scopes: `bot`
- Bot Permissions: `Send Messages`, `Read Message History`, `Use Slash Commands`
- 生成された URL で対象サーバーに招待

#### 3. EgoPulse の設定

`egopulse.config.yaml` に Discord の設定を追加:

```yaml
channels:
  discord:
    enabled: true
    bot_token: <手順1でコピーしたToken>
```

または環境変数:

```bash
export EGOPULSE_DISCORD_BOT_TOKEN="<Token>"
```

#### 4. 起動と確認

```bash
cargo run -p egopulse -- start
```

ログに `Starting Discord bot...` が表示されたら接続成功。
Discord サーバーで `@EgoPulse hello` または DM でメッセージを送信して動作確認。

## Telegram セットアップガイド

### 前提条件

- Telegram アカウント

### 手順

#### 1. Bot の作成

1. Telegram で [@BotFather](https://t.me/BotFather) を開く
2. `/newbot` を送信
3. Bot の表示名を入力 (例: `EgoPulse Bot`)
4. Bot の username を入力 (例: `my_egopulse_bot`) — `bot` で終わる必要あり
5. 発行された **HTTP API Token** をコピー (`123456789:ABCdefGHIjklMNOpqrSTUvwxYZ` 形式)

#### 2. EgoPulse の設定

`egopulse.config.yaml` に Telegram の設定を追加:

```yaml
channels:
  telegram:
    enabled: true
    bot_token: "123456789:ABCdefGHIjklMNOpqrSTUvwxYZ"
    bot_username: "my_egopulse_bot"
```

または環境変数:

```bash
export EGOPULSE_TELEGRAM_BOT_TOKEN="123456789:ABCdefGHIjklMNOpqrSTUvwxYZ"
```

> **注意**: `bot_username` はグループでの @メンション検出に使用します。DM のみの場合は省略可能です。

#### 3. 起動と確認

```bash
cargo run -p egopulse -- start
```

ログに `Starting Telegram bot as @my_egopulse_bot...` が表示されたら接続成功。
Telegram で Bot に DM を送信して動作確認。グループの場合は `@my_egopulse_bot hello` でメンション。

## Current scope

- `egopulse` 無引数起動で local TUI
- `egopulse.config.yaml` 自動検出
- `--config` による明示指定
- OpenAI-compatible endpoint に対する `ask`
- SQLite 永続化付きの `chat --session`
- `ask --session` による既存 session の再開
- `web` による HTTP サーバー + React WebUI
- `POST /api/send_stream` + `GET /api/stream` による SSE chat
- `GET /ws` による WebSocket gateway
- `start` によるチャネル一括起動 (Discord / Telegram / Web)
- Discord adapter (serenity 0.12, メッセージ分割 2000文字, DM/Guild 対応)
- Telegram adapter (teloxide 0.17, メッセージ分割 4096文字, DM/Group 対応)

次フェーズで追加予定:

- tools / skills
- MCP integration

## Local checks

```bash
npm install --prefix egopulse/web
npm run build --prefix egopulse/web
cargo fmt --check
cargo check -p egopulse
cargo clippy --all-targets --all-features -- -D warnings
cargo test -p egopulse
```
