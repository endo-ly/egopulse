# EgoPulse

EgoPulse は EgoGraph 向けの Rust runtime foundation です。  
Issue 2.5 時点では、local TUI と developer 向け CLI を備えた persistent runtime を提供します。

## Prerequisites

- Rust stable
- `cargo fmt`
- `cargo clippy`

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

会話履歴と session snapshot は `EGOPULSE_DATA_DIR/egopulse.db` に保存されます。  
Issue 2.5 の local TUI では、session 一覧から再開・新規開始ができます。

### Local TUI controls

Browser:

- `j/k` or arrows
- `Ctrl-N/P`
- `g/G`
- `PageUp/PageDown`
- `Enter` open
- `n` new session
- `r` refresh sessions
- `q` quit

Chat:

- `Enter` send
- `Esc` back
- `← / →` move cursor
- `↑` move to input start, then walk input history
- `↓` move forward through input history

## Current scope

- `egopulse` 無引数起動で local TUI
- `egopulse.config.yaml` 自動検出
- `--config` による明示指定
- OpenAI-compatible endpoint に対する `ask`
- SQLite 永続化付きの `chat --session`
- `ask --session` による既存 session の再開

次フェーズで追加予定:

- Discord / Telegram / WebUI
- tools / skills
- MCP integration

## Local checks

```bash
cargo fmt --check
cargo check -p egopulse
cargo clippy --all-targets --all-features -- -D warnings
cargo test -p egopulse
```
