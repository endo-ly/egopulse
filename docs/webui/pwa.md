# EgoPulse WebUI — PWA（ホーム画面アプリ化）

WebUI を Android / iOS のホーム画面からアプリのように使うための PWA 構成を定義する。

## 1. 構成

Service Worker は使わない（オフライン動作が不要なため）。ランタイムへの常時接続（SSE / WebSocket）が前提のアプリであり、PWA に必要なのは manifest とアイコンのみ。

| ファイル | 役割 |
|---|---|
| `web/public/manifest.webmanifest` | アプリ名・`display: standalone`・テーマ色・アイコン定義 |
| `web/public/pwa-192.png` / `pwa-512.png` | インストール時のアプリアイコン（`purpose: any`） |
| `web/public/pwa-maskable-512.png` | Android のアダプティブアイコン用（`purpose: maskable`、安全領域 80% に収める） |
| `web/public/apple-touch-icon.png` | iOS ホーム画面用アイコン（180px） |
| `web/index.html` | manifest リンク・`theme-color`・iOS 用メタタグ |

- `start_url` / `scope` は `/`。ルーティングは履歴 API のため standalone 起動でもディープリンクが機能する
- `background_color` / `theme_color` は `#141416`（アプリの `--color-bg` と同一）

## 2. インストール手順

### Android（Chrome）

HTTPS 配信が必須。開発・検証用には Vite の自己署名 HTTPS を使う:

```bash
npm run dev:mock:https -- --host 0.0.0.0 --port 5173
```

1. スマホで `https://<host>:5173/` を開く
2. 証明書警告が出るので「詳細 → 続行」で通す（自己署名のため）
3. Chrome メニュー →「アプリをインストール」（またはアドレスバーのインストール案内）

本番運用ではリバースプロキシ等で正規 TLS を終端すること（[deploy.md](../deploy.md)）。HTTP のままだとインストールは出ず、「ショートカットを追加」（ブラウザタブ表示）止まりになる。

### iOS（Safari）

共有メニュー →「ホーム画面に追加」。HTTPS 不要で、`apple-touch-icon` と iOS 用メタタグから standalone 表示になる。

## 3. 動作

- standalone 表示ではアドレスバー・ブラウザ UI が消え、ステータスバーは `theme_color` に寄せる
- スプラッシュは `background_color` + アイコンから自動生成される
- オフライン時は接続できないため、そのままエラーになる（Service Worker によるキャッシュは意図的に提供しない）
