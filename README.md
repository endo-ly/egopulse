# EgoPulse

OpenAI互換の provider / model を切り替えながら動かせる、永続化AIエージェントランタイム。
TUI / Web UI / Discord / Telegram を単一バイナリで提供。Rust (Tokio) 製。

## Quick Start

```bash
# インストール（リリースバイナリ）
curl -fsSL https://raw.githubusercontent.com/endo-ava/ego-graph/main/scripts/install-egopulse.sh | bash

# 初期セットアップ（対話型TUIウィザード → ~/.egopulse/egopulse.config.yaml を生成）
egopulse setup

# systemd サービスとして起動（本番推奨）
egopulse gateway install    # ユニット作成 + 有効化 + 起動

# 前景実行（開発・確認用）
egopulse run
```

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

# WebUI ビルド（build.rs が web/src の mtime を監視し変更時自動ビルドするが、手動も可能）
npm install --prefix egopulse/web
npm run build --prefix egopulse/web

# 開発時・WebUIのみ確認
cd egopulse/web && npm run dev
```

## バイナリ配置

インストール方法によってバイナリの配置場所が異なる。`gateway install` は **起動中のバイナリパス（`current_exe`）** を systemd ユニットの `ExecStart` に埋め込むため、どのバイナリから実行したかで登録内容が変わる。

| インストール方法 | バイナリパス | 備考 |
|---|---|---|
| `install-egopulse.sh` | `/usr/local/bin/egopulse` | 書き込み権限がない場合は `~/.local/bin/egopulse` |
| `cargo build --release` | `{project}/target/release/egopulse` | 手動 cp するまで systemd には反映されない |
| `cargo run` で `gateway install` | `{project}/target/debug/egopulse` | ⚠️ デバッグバイナリが登録される。リリース運用には不適 |

### systemd 運用中の差し替え

バイナリに WebUI 資産が埋め込まれているため、リビルド → stop → cp → start の手順が必要。`cargo run` で起動中のバイナリを上書きできない点にも注意。

```bash
cargo build --release -p egopulse
sudo systemctl stop egopulse
sudo cp target/release/egopulse /usr/local/bin/egopulse
sudo systemctl start egopulse
```

リリースバイナリへ戻す場合は `egopulse update` で再ダウンロード → 自動再起動。

## Documentation

| トピック | ドキュメント |
|---|---|
| コマンド仕様 | [commands.md](../docs/30.egopulse/commands.md) |
| 設定仕様 | [config.md](../docs/30.egopulse/config.md) |
| セッションライフサイクル | [session-lifecycle.md](../docs/30.egopulse/session-lifecycle.md) |
| MCP 統合 | [mcp.md](../docs/30.egopulse/mcp.md) |
| Built-in Tools | [tools.md](../docs/30.egopulse/tools.md) |
| DB Schema | [db.md](../docs/30.egopulse/db.md) |
| デプロイ手順 | [docs/50.deploy/](../docs/50.deploy/) |
