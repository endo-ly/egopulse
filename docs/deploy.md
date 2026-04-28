# EgoPulse Deploy

Linux サーバー上で EgoPulse を systemd サービスとして常駐化する手順。

## 目次

1. [前提](#1-前提)
2. [インストール](#2-インストール)
3. [設定](#3-設定)
4. [systemd 常駐](#4-systemd-常駐)
5. [リリースプロセス](#5-リリースプロセス)

---

## 1. 前提

- Linux (systemd 搭載ディストリビューション)
- `curl`, `jq`
- Tailscale 公開する場合: Tailscale アカウント

## 2. インストール

ワンライナー、プリビルドバイナリ配置、ソースビルドの 3 通りの導線がある。

### 2.1 ワンライナーインストール（推奨）

スクリプトが OS/アーキテクチャを自動検出し、GitHub Releases から最新バイナリをダウンロード・配置する。

```bash
curl -fsSL https://raw.githubusercontent.com/endo-ly/egopulse/main/scripts/install.sh | bash
```

バイナリは `$HOME/.local/bin/egopulse` に配置する。`egopulse update` も同じパスを更新対象にする。

確認:

```bash
egopulse --version
```

### 2.2 プリビルドバイナリ配置（手動）

GitHub Releases から直接ダウンロードする。

```bash
# バイナリ配置先のディレクトリを作成
mkdir -p "$HOME/.local/bin"

# 最新のリリースバイナリをダウンロード（x86_64 Linux の場合）
# 完全なURLは GitHub Releases で確認してください
curl -fsSL -o egopulse.tar.gz "https://github.com/endo-ly/egopulse/releases/latest/download/egopulse-<version>-x86_64-unknown-linux-gnu.tar.gz"
tar -xzf egopulse.tar.gz
install -m 0755 egopulse "$HOME/.local/bin/egopulse"
```

確認:

```bash
egopulse --version
```

### 2.3 ソースビルド

Rust toolchain が必要。未導入の場合は [rustup](https://rustup.rs/) でインストールする。

> **Rust toolchain / Rustup とは？**
> - **Rust toolchain**: Rust コンパイラ(`rustc`)、ビルドツール(`cargo`)、標準ライブラリなどの一式
> - **Rustup**: toolchain のインストール・バージョン管理を行う公式ツール

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
```

WebUI の埋め込みには Node.js も必要:

```bash
curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.40.3/install.sh | bash
source ~/.nvm/nvm.sh
nvm install --lts
```

ビルド:

```bash
git clone https://github.com/endo-ly/egopulse.git
cd egopulse
cargo build --release
mkdir -p "$HOME/.local/bin"
install -m 0755 target/release/egopulse "$HOME/.local/bin/egopulse"
```

確認:

```bash
egopulse --version
```

更新時も同じ手順で `target/release/egopulse` を `$HOME/.local/bin/egopulse` に上書きする。

## 3. 設定

### 3.1 設定ファイル配置

設定ファイルは `$HOME/.egopulse/egopulse.config.yaml` に固定する。

```bash
mkdir -p "$HOME/.egopulse"
nano "$HOME/.egopulse/egopulse.config.yaml"
```

最小限の設定例:

```yaml
model: "gpt-4o-mini"
api_key: "sk-..."
base_url: "https://api.openai.com/v1"
log_level: "info"

channels:
  web:
    enabled: true
    host: "127.0.0.1"
    port: 10961
    auth_token: "openssl rand -base64 32 の出力"
    allowed_origins:
      - "http://127.0.0.1:10961"
```

`auth_token` は必ず自分で生成したものに書き換えること。

```bash
openssl rand -base64 32
```

サンプルは [`egopulse.config.example.yaml`](../egopulse.config.example.yaml) を参照。

### 3.2 固定ディレクトリ

EgoPulse は以下の固定ディレクトリを使用する。

```bash
mkdir -p "$HOME/.egopulse/data" "$HOME/.egopulse/workspace"
```

## 4. systemd 常駐

systemd で常駐化し、障害時は自動復旧させる。

### 4.1 サービス登録（自動インストール）

systemd unit ファイルの自動生成・配置・有効化まで一括実行する。
すでにインストール済みの場合は unit を更新してサービスを再起動する。

```bash
egopulse run
```

systemd に登録せず、その場で有効チャネルを前景実行する。

```bash
egopulse gateway install
```

`--config` を指定した場合、その絶対パスが unit に埋め込まれる。省略時は `~/.egopulse/egopulse.config.yaml` が使われる。

削除:

```bash
egopulse gateway uninstall
```

状態確認:

```bash
egopulse gateway status
```

起動:

```bash
egopulse gateway start
```

停止:

```bash
egopulse gateway stop
```

再起動:

```bash
egopulse gateway restart
```

### 4.2 手動サービス登録（systemd unit 直書き）

systemd unit を手動で作成する。

> **パスについて**: 以下の例のパスは実際の環境に合わせて書き換えてください。
> systemd の `ExecStart` はシェル展開を行わないため、絶対パスの指定が必要です。

`~/.config/systemd/user/egopulse.service`:

```ini
[Unit]
Description=EgoPulse Agent Runtime
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=%h/.local/bin/egopulse --config "%h/.egopulse/egopulse.config.yaml" run
Restart=always
RestartSec=10
Environment=HOME=%h

# Security hardening
NoNewPrivileges=true
ProtectSystem=strict
ReadWritePaths=%h/.egopulse %h/.egopulse/data %h/.egopulse/workspace
ProtectHome=false

[Install]
WantedBy=default.target
```

> ソースビルド時も `ExecStart` は `%h/.local/bin/egopulse` のままにし、`install -m 0755 target/release/egopulse "$HOME/.local/bin/egopulse"` で配置してください。`~/.cargo/bin` への `cargo install` は配布版と競合しやすいため非推奨です。
> `egopulse update` と `egopulse gateway restart` はユーザーサービスと `$HOME/.local/bin/egopulse` を前提に管理します。

### 4.3 起動・確認

```bash
systemctl --user daemon-reload
systemctl --user enable egopulse
systemctl --user start egopulse
systemctl --user status egopulse
```

### 4.4 再起動

systemd で常駐中のサービスを再起動する。

```bash
egopulse gateway restart
```

内部で `systemctl --user restart egopulse.service` を実行する。

### 4.5 更新

最新リリースに更新し、サービスを再起動する。

```bash
egopulse update
```

内部で最新リリースを検証して `$HOME/.local/bin/egopulse` に配置後、systemd ユーザーサービスを再起動する。

### 4.6 Tailscale Serve（オプション）

WebUI を Tailnet 内に HTTPS 公開する。

```bash
sudo tailscale serve --bg http://127.0.0.1:10961
sudo tailscale serve status
```

接続 URL: `https://<hostname>.<tailnet>.ts.net/`

### 4.7 ログ確認

```bash
# 最新ログ
journalctl --user -u egopulse.service -n 100 --no-pager

# リアルタイム監視
journalctl --user -u egopulse.service -f

# エラーのみ
journalctl --user -u egopulse.service -p err --no-pager
```

## 5. リリースプロセス

main ブランチへのマージ時に自動で GitHub Release が作成される。

### 自動リリースの仕組み

```
main へマージ
         ↓
Cargo.toml のバージョン + 日付 + run番号 でタグを自動生成
  例: v0.1.0-20260404.1
         ↓
Linux/macOS × x86_64/aarch64 の 4 ターゲットを並列ビルド
         ↓
GitHub Release に全バイナリ + SHA256SUMS.txt をアップロード
```

### タグ命名規則

`v{バージョン}-{YYYYMMDD}.{run_number}`

例: `v0.1.0-20260404.1`

バージョンは `Cargo.toml` の `version` フィールドから取得する。
