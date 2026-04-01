# EgoPulse

EgoPulse は EgoGraph 向けの Rust runtime foundation です。  
Issue 2.5 時点では、local TUI と developer 向け CLI に加えて、React/Vite ベースの WebUI と WebSocket gateway を提供します。

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
`EGOPULSE_WEB_ENABLED` / `EGOPULSE_WEB_HOST` / `EGOPULSE_WEB_PORT` を使う場合でも、実行時には `channels.web` 相当の値として扱われます。

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
```

```bash
cargo run -p egopulse -- --config /path/to/egopulse.config.yaml ask "hello"
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

`web` サブコマンドで HTTP サーバーを起動し、React/Vite 製 WebUI と WebSocket gateway を公開します。`--host` / `--port` を省略した場合は `egopulse.config.yaml` の `channels.web.host` / `channels.web.port` を使います。

```bash
cargo run -p egopulse -- web
cargo run -p egopulse -- --config egopulse.config.yaml web --host 0.0.0.0 --port 8080
```

Endpoints:
- `GET /` - WebUI
- `GET /health` - Health check
- `GET /api/health` - Health check
- `GET /api/config` - Runtime config 取得
- `PUT /api/config` - Runtime config 保存
- `GET /api/sessions` - List sessions
- `GET /api/history?session_key=...` - Get message history
- `POST /api/send_stream` - HTTP 経由のチャット送信
- `GET /ws` - WebSocket gateway

現在の WebUI では次ができます。
- セッション一覧の表示
- セッション履歴の表示
- Runtime Config の表示と保存
- WebSocket gateway 経由のチャット

会話履歴と session snapshot は `EGOPULSE_DATA_DIR/egopulse.db` に保存されます。  
Issue 2.5 の local TUI では、session 一覧から再開・新規開始ができます。

## Current scope

- `egopulse` 無引数起動で local TUI
- `egopulse.config.yaml` 自動検出
- `--config` による明示指定
- OpenAI-compatible endpoint に対する `ask`
- SQLite 永続化付きの `chat --session`
- `ask --session` による既存 session の再開
- `web` による HTTP サーバー + React WebUI
- `GET /ws` による WebSocket gateway

次フェーズで追加予定:

- Discord / Telegram channels
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
