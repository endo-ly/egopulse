# インストール

EgoPulse は単一バイナリとして配布されます。Linux（x86_64 / arm64）と macOS（Apple Silicon）に対応しています。

## ワンライナーでインストール

```sh
curl -fsSL https://raw.githubusercontent.com/endo-ly/egopulse/main/scripts/install.sh | bash
```

スクリプトが OS とアーキテクチャを自動判別し、GitHub Releases の最新バイナリをダウンロードして `$HOME/.local/bin` に配置します。

## 手動でインストール

[GitHub Releases](https://github.com/endo-ly/egopulse/releases/latest) から環境に合った tar.gz をダウンロードし、展開してパスの通った場所に置きます。

```sh
curl -fsSL -o egopulse.tar.gz \
  "https://github.com/endo-ly/egopulse/releases/latest/download/egopulse-<version>-x86_64-unknown-linux-gnu.tar.gz"
tar -xzf egopulse.tar.gz
install -m 0755 egopulse "$HOME/.local/bin/egopulse"
```

`<version>` はリリースのバージョン（例: `2026.8.23`）に置き換えてください。改ざん検証用に各リリースには `SHA256SUMS.txt` が添付されています。

## 動作確認

```sh
egopulse --version
```

## 更新

```sh
egopulse update
```

最新リリースを確認してバイナリを差し替えます。

## 次のステップ

1. `egopulse setup` で初期セットアップを行う
2. [コマンド](./commands.md) と [設定仕様](./config.md) を参照する
3. systemd での常駐化は [デプロイ運用](./deploy.md) へ
