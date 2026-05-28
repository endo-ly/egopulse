# Development

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
