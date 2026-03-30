# EgoPulse

EgoPulse は EgoGraph 向けの Rust runtime foundation です。  
この MVP では、channel-agnostic な agent loop と SQLite ベースの session 永続化を備えた
CLI runtime を提供します。

## Prerequisites

- Rust stable
- `cargo fmt`
- `cargo clippy`

## Config

環境変数または TOML 設定ファイルに対応します。

読み込み順は次の通りです。

1. プロセス環境変数
2. `--config` で指定した TOML

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

サンプルは [`egopulse.example.toml`](./egopulse.example.toml) を参照してください。
実運用では、git 管理しないローカル設定として `egopulse.local.toml` を使う想定です。

```bash
cargo run -p egopulse -- --config egopulse/egopulse.local.toml ask "hello"
```

## Usage

```bash
export EGOPULSE_MODEL="gpt-4o-mini"
export EGOPULSE_API_KEY="sk-..."
export EGOPULSE_BASE_URL="https://api.openai.com/v1"
export EGOPULSE_DATA_DIR=".egopulse"

cargo run -p egopulse -- ask "hello"
```

期待する出力:

```text
assistant: ...
```

永続 session を使う場合:

```bash
cargo run -p egopulse -- chat --session local-dev
```

別プロセスで再開する場合:

```bash
cargo run -p egopulse -- ask --session local-dev "remember my last question?"
```

## Local checks

```bash
cargo fmt --check
cargo check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```
