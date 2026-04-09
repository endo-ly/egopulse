# EgoPulse

OpenAI互換のLLMエンドポイント1つで動く、永続化AIエージェントランタイム。

ローカルTUI、WebUI（React + SSE + WebSocket）、Discord、Telegramを単一のバイナリで提供。

## Quick Start

### 1. Install

```bash
# リリースから（推奨）
curl -fsSL https://raw.githubusercontent.com/endo-ava/ego-graph/main/scripts/install-egopulse.sh | bash

# 開発中にソースから起動
cargo run -p egopulse
```

### 2. Setup

```bash
egopulse setup
```

対話型TUIウィザードが `~/.egopulse/egopulse.config.yaml` を作成。LLMプロバイダの選択、API認証情報の入力、チャネルの設定をガイド。

### 3. Run

```bash
egopulse          # ローカルTUIを開く
egopulse run
```

## Configuration

### Config resolution

1. 環境変数（最優先）
2. `--config <PATH>` で指定したYAML
3. `~/.egopulse/egopulse.config.yaml` 自動検出

### YAML config

```yaml
model: gpt-4o-mini
api_key: sk-...
base_url: https://api.openai.com/v1
log_level: info
compaction_timeout_secs: 180
max_history_messages: 50
max_session_messages: 40
compact_keep_recent: 20

channels:
  web:
    enabled: true
    host: 127.0.0.1
    port: 10961
    auth_token: <openssl rand -base64 32 で生成した値>
    allowed_origins:
      - http://127.0.0.1:10961
  discord:
    enabled: false
    bot_token: your-discord-bot-token
  telegram:
    enabled: false
    bot_token: your-telegram-bot-token
    bot_username: your_bot_username
```

完全なテンプレートは [`egopulse.config.example.yaml`](./egopulse.config.example.yaml) を参照。

### Session compaction

EgoPulse は MicroClaw スタイルの自動 compaction を持ち、LLM 実行前に長い session を要約して recent context を残します。compaction が走る直前の全文会話は `data_dir/groups/<channel>/<chat_id>/conversations/<timestamp>.md` に archive されます。

| 設定 | デフォルト | 説明 |
|------|-----------:|------|
| `compaction_timeout_secs` | `180` | 要約 compaction の LLM タイムアウト秒数 |
| `max_history_messages` | `50` | session snapshot が壊れている/未保存時に messages テーブルから復元する件数 |
| `max_session_messages` | `40` | compaction を発火させる message 数の閾値 |
| `compact_keep_recent` | `20` | compaction 後にそのまま残す recent messages 数 |

### Environment variables

| 変数 | 説明 |
|------|------|
| `EGOPULSE_MODEL` | モデル名（必須） |
| `EGOPULSE_API_KEY` | APIキー（`localhost`/`127.0.0.1`/`0.0.0.0`/`::1` のbase_urlなら省略可） |
| `EGOPULSE_BASE_URL` | OpenAI互換APIエンドポイント |
| `EGOPULSE_LOG_LEVEL` | ログレベル（デフォルト: `info`） |
| `EGOPULSE_DISCORD_BOT_TOKEN` | Discordボットトークンの上書き |
| `EGOPULSE_TELEGRAM_BOT_TOKEN` | Telegramボットトークンの上書き |

### Endpoint examples

**OpenAI:**
```bash
export EGOPULSE_MODEL="gpt-4o-mini"
export EGOPULSE_API_KEY="sk-..."
export EGOPULSE_BASE_URL="https://api.openai.com/v1"
```

**OpenRouter:**
```bash
export EGOPULSE_MODEL="openai/gpt-4o-mini"
export EGOPULSE_API_KEY="sk-or-..."
export EGOPULSE_BASE_URL="https://openrouter.ai/api/v1"
```

**ローカル（APIキー不要）:**
```bash
export EGOPULSE_MODEL="local-model"
export EGOPULSE_BASE_URL="http://127.0.0.1:1234/v1"
```

## Commands

### Global options

| オプション | 説明 |
|-----------|------|
| `--config <PATH>` | 設定ファイルのパス（絶対/相対） |
| `--version` | バージョン表示 |
| `--help` | ヘルプ表示 |

### Subcommands

| コマンド | 説明 | 設定必須 |
|---------|------|:-:|
| `egopulse` | ローカルTUI（セッションブラウザ + チャット） | 必須 |
| `egopulse setup` | 対話型設定ウィザード | 不要 |
| `egopulse ask <PROMPT>` | 単発プロンプト、結果をstdoutに出力 | 必須 |
| `egopulse chat` | 永続化CLIチャットセッション | 必須 |
| `egopulse run` | 有効なチャネルを一括起動（前景実行） | 必須（api_keyは省略可） |
| `egopulse gateway <ACTION>` | systemdサービス管理 | — |
| `egopulse update` | 最新リリースに更新 | — |

#### `egopulse ask <PROMPT>`

単発のプロンプト送信。応答を出力して終了。

```bash
egopulse ask "Rustとは？"
egopulse ask --session my-session "前回の続き"
```

| オプション | 説明 |
|-----------|------|
| `--session <SESSION>` | 既存セッションの再開、または新規作成 |

#### `egopulse chat`

セッション履歴付きの永続化CLIチャット。

```bash
egopulse chat
egopulse chat --session my-session
```

| オプション | 説明 |
|-----------|------|
| `--session <SESSION>` | セッション名（省略時は自動生成 `cli-<uuid>`） |

#### `egopulse run`

設定で `enabled: true` のチャネルを並列起動。Web、Discord、Telegramを同時に稼働。Ctrl+C でgraceful shutdown。

#### `egopulse gateway <ACTION>`

| アクション | 説明 |
|-----------|------|
| `install` | systemdユニットを作成・有効化・起動。既存なら更新して再起動。 |
| `start` | インストール済みのsystemdサービスを起動。 |
| `stop` | systemdサービスを停止。 |
| `uninstall` | サービスの無効化・停止・削除。 |
| `status` | `systemctl status` の出力を表示。未起動時はexit 1。 |
| `restart` | systemdサービスを再起動。 |

設定ファイルが必須。先に `egopulse setup` を実行。

#### `egopulse update`

最新リリースのバイナリをダウンロードし、systemdサービスがあれば再起動。

## Channels

### Web

`channels.web.enabled: true` で有効化。React WebUI、SSEライブチャット、WebSocketゲートウェイを提供。

| エンドポイント | 認証 | 説明 |
|--------------|:----:|------|
| `GET /` | 不要 | WebUI |
| `GET /health` | 不要 | ヘルスチェック |
| `GET /api/health` | Bearer | ヘルスチェック |
| `GET /api/config` | Bearer | ランタイム設定の取得 |
| `PUT /api/config` | Bearer | ランタイム設定の保存 |
| `GET /api/sessions` | Bearer | セッション一覧 |
| `GET /api/history?session_key=...` | Bearer | メッセージ履歴の取得 |
| `POST /api/send_stream` | Bearer | チャットrun開始、`run_id` を返す |
| `GET /api/stream?run_id=...` | Bearer | SSEでrunイベントを購読 |
| `GET /ws` | トークン | WebSocketゲートウェイ |

認証: `/api/*` は `Authorization: Bearer <auth_token>`、`/ws` は初回frameの `connect.authToken`。WebUIは初回訪問時にトークン入力モーダルを表示し、localStorageに保存。

auth_tokenの生成:
```bash
openssl rand -base64 32
```

`allowed_origins` を設定すると `/ws` の `Origin` ヘッダーをallowlist照合。未設定の場合は `Host` ヘッダーと一致する同一ホスト接続のみ許可。

### Discord

1. [Discord Developer Portal](https://discord.com/developers/applications) でボットアプリケーション作成
2. Bot → Privileged Gateway Intents で **Message Content Intent** と **Server Members Intent** をON
3. OAuth2 URL Generator でボットをサーバーに招待（Scopes: `bot`、Permissions: `Send Messages`, `Read Message History`）
4. 設定に `channels.discord.bot_token` または `EGOPULSE_DISCORD_BOT_TOKEN` を設定

文字数制限: 2000文字（送信時に自動分割）。受信はGateway（WebSocket）。

### Telegram

1. [@BotFather](https://t.me/BotFather) に `/newbot` を送信
2. 設定に `channels.telegram.bot_token` と `bot_username`、または `EGOPULSE_TELEGRAM_BOT_TOKEN` を設定

文字数制限: 4096文字（送信時に自動分割）。受信はLong Polling。

`bot_username` はグループでの `@メンション` 検出に使用。DMのみの場合は省略可。

## Deployment

### systemd service

```bash
egopulse run                # 前景実行
egopulse gateway install    # インストール・起動
egopulse gateway start      # サービス起動
egopulse gateway stop       # サービス停止
egopulse gateway status     # 状態確認
egopulse gateway restart    # 再起動
egopulse gateway uninstall  # 削除
```

ユニットファイルは `/etc/systemd/system/egopulse.service` に配置。`--config` を指定するとその絶対パスがユニットに埋め込まれる。省略時は `~/.egopulse/egopulse.config.yaml` を自動検出する。

### Update

```bash
egopulse update
```

`install-egopulse.sh --skip-run` で最新バイナリを配置します。`--skip-run` はインストール後の自動実行（`--version` チェック）をスキップするのみで、systemdサービスの再起動は `egopulse update` 側で別途実行されます。

### Manual systemd unit

```ini
[Unit]
Description=EgoPulse Agent Runtime
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/egopulse --config "%h/.egopulse/egopulse.config.yaml" run
Restart=always
RestartSec=10
Environment=HOME=%h

# Security hardening
NoNewPrivileges=true
ProtectSystem=strict
ReadWritePaths=%h/.egopulse %h/.egopulse/data %h/.egopulse/workspace
ProtectHome=read-only

[Install]
WantedBy=multi-user.target
```

`User=root` 相当で動かす場合の設定ファイル例は `/root/.egopulse/egopulse.config.yaml`。固定ディレクトリは `/root/.egopulse/data` と `/root/.egopulse/workspace`。

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now egopulse
journalctl -u egopulse -f
```

## Development

### Local checks

```bash
npm install --prefix egopulse/web
npm run build --prefix egopulse/web
cargo fmt --check
cargo check -p egopulse
cargo clippy --all-targets --all-features -- -D warnings
cargo test -p egopulse
```

### Run from source

```bash
cargo run -p egopulse                    # TUI
cargo run -p egopulse -- setup           # 初回セットアップ
cargo run -p egopulse -- ask "hello"     # 単発
cargo run -p egopulse -- run             # 全チャネル
```

### Install script options

```bash
curl -fsSL https://raw.githubusercontent.com/endo-ava/ego-graph/main/scripts/install-egopulse.sh | bash -s -- --setup
```

| オプション | 説明 |
|-----------|------|
| `--skip-run` | インストール後の `--version` 確認をスキップ |
| `--setup` | インストール直後に `egopulse setup` ウィザードを起動 |
