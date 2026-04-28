# EgoPulse

OpenAI 互換の provider / model を切り替えながら動かせる、永続化 AI エージェントランタイム。
TUI / Web UI / Discord / Telegram を単一バイナリで提供。Rust (Tokio) 製。

## Getting Started

```bash
# 1. インストール
curl -fsSL https://raw.githubusercontent.com/endo-ly/egopulse/main/scripts/install.sh | bash

# 2. 初期セットアップ（対話型 TUI ウィザード）
#    プロバイダー選択 → API キー入力 → Discord/Telegram の有効化（任意）
egopulse setup

# 3. 起動
egopulse run                     # 全チャネル前景実行（動作確認向け）
egopulse gateway install         # systemd サービス登録 + 起動（本番向け）
```

起動後、ブラウザで `http://127.0.0.1:10961` にアクセスすると WebUI が利用できる。

CLI で直接チャットする場合：

```bash
egopulse chat                     # CLI チャットセッション
egopulse chat --session mybot     # セッション名を指定
```

Discord / Telegram を使う場合は [channels.md](./docs/channels.md) を参照。

## Development

```bash
# 初回セットアップ
cargo run -p egopulse -- setup

# 各種起動
cargo run -p egopulse                          # TUI（セッションブラウザ + チャット）
cargo run -p egopulse -- run                   # 全チャネル前景起動
cargo run -p egopulse -- chat                  # CLI チャットセッション
cargo run -p egopulse -- chat --session myses  # セッション名指定

# チェック
cargo fmt --check
cargo check -p egopulse
cargo clippy --all-targets --all-features -- -D warnings
cargo test -p egopulse

# WebUI ビルド
npm install --prefix web
npm run build --prefix web
cd web && npm run dev              # 開発時・WebUI のみ確認
```

## バイナリ配置

インストール方法によってバイナリの配置場所が異なる。`gateway install` は **起動中のバイナリパス（`current_exe`）** を systemd ユニットの `ExecStart` に埋め込む。

| インストール方法 | バイナリパス | 備考 |
|---|---|---|
| `install-egopulse.sh` | `~/.local/bin/egopulse` | `egopulse update` の更新対象 |
| `cargo build --release` | `{project}/target/release/egopulse` | 手動 cp するまで systemd には反映されない |
| `cargo run` で `gateway install` | `{project}/target/debug/egopulse` | ⚠️ デバッグバイナリが登録される |

### systemd 運用中の差し替え

```bash
cargo build --release -p egopulse
systemctl --user stop egopulse
install -m 0755 target/release/egopulse "$HOME/.local/bin/egopulse"
systemctl --user start egopulse
```

リリースバイナリへ戻す場合は `egopulse update` で再ダウンロード → 自動再起動。

## Documentation

| トピック | ドキュメント |
|---|---|
| アーキテクチャ概要 | [architecture.md](./docs/architecture.md) |
| コマンド仕様 | [commands.md](./docs/commands.md) |
| 設定仕様 | [config.md](./docs/config.md) |
| チャネル仕様 (Web/Discord/Telegram/TUI/CLI) | [channels.md](./docs/channels.md) |
| セッションライフサイクル | [session-lifecycle.md](./docs/session-lifecycle.md) |
| Built-in Tools | [tools.md](./docs/tools.md) |
| MCP 統合 | [mcp.md](./docs/mcp.md) |
| System Prompt 構築 | [system-prompt.md](./docs/system-prompt.md) |
| セキュリティ | [security.md](./docs/security.md) |
| デプロイ手順 | [deploy.md](./docs/deploy.md) |
| ディレクトリ構成 | [directory.md](./docs/directory.md) |
| DB スキーマ | [db.md](./docs/db.md) |
| WebUI API | [api.md](./docs/api.md) |