# EgoPulse WebUI — Overview

WebUI は EgoPulse ランタイムの運転席である。agent との対話、agent の中身の管理、自律動作の監査、ランタイム全体の監視と設定を、単一のブラウザセッションから行う。

## 1. 設計思想

### 1.1 Agent-First

画面は agent を主体として編成する。対話・人格・記憶・自律動作はすべて agent に従属し、Sidebar で選んだ agent がそれらの画面の文脈になる。agent に従属しないランタイム全体の関心事（監視・設定）だけをグローバルな画面として分ける。

### 1.2 Observe, then Act

Discord / Telegram / CLI / TUI / Voice で進行する会話は観測専用として扱い、WebUI から書き込めるのは Web 起源のセッションのみとする。ファイル・設定の編集は、必ず楽観的同時実行制御と差分確認を経由する。

### 1.3 URL as Source of Truth

選択 agent・画面・セクション・選択中の run などの UI 状態は URL にエンコードする。React の in-memory state は入力ドラフト・モーダル開閉などの一時的な状態に限る。

### 1.4 Streaming First

LLM 応答・ツール実行・Pulse / Sleep の進行は、完了後ではなく進行中に UI へ反映する。

---

## 2. 画面構成

### 2.1 画面一覧

| 区分 | 画面 | 役割 | 詳細 |
|---|---|---|---|
| agent | **Chat** | Web セッションでの対話と、他チャネルセッションの観測 | [chat.md](./chat.md) |
| agent | **Agent** | 人格・規約・長期記憶・スキル・実効モデルの確認と編集 | [agent.md](./agent.md) |
| agent | **Schedule** | Pulse と Sleep の次回実行と実行履歴の監査、Pulse intention の管理 | [schedule.md](./schedule.md) |
| global | **Metrics** | ランタイムの健全性と、agent 別の利用状況の推移 | [metrics.md](./metrics.md) |
| global | **Config** | 設定 YAML の確認と編集 | [config.md](./config.md) |

### 2.2 配置

```
┌ Sidebar ─────────────────────┬ Main ───────────────────────────┐
│ EgoPulse              🔍  ‹  │                                 │
│ [💬Chat][👤Agent][📅Schedule] │  選択中の画面                     │
│ AGENTS                       │  （agent 画面は選択 agent の内容）  │
│ ──────────────────────────── │                                 │
│ 画面ごとの文脈領域               │                                 │
│ ──────────────────────────── │                                 │
│ ● ok          [📈] [⚙]       │                                 │
└──────────────────────────────┴─────────────────────────────────┘
```

- agent 画面（Chat / Agent / Schedule）は Sidebar 上部の Nav に置く
- グローバル画面（Metrics / Config）は Sidebar フッターに置く
- Sidebar 中段の文脈領域は画面ごとに内容が変わる

詳細は [layout.md](./layout.md)。

### 2.3 Agent 選択のスコープ

- agent 画面は Sidebar で選択した agent を対象にする。agent を切り替えても画面は維持する
- グローバル画面は Sidebar の agent 選択の影響を受けない。グローバル画面で agent を選ぶと、その agent の Agent 画面へ遷移する

---

## 3. トランスポート

| 経路 | 役割 |
|---|---|
| WebSocket (`/ws`) | チャット送信・ストリーミング応答・ツール実行・セッション更新・Pulse / Sleep の開始終了 |
| REST (`/api/*`) | 一覧・詳細・集計の取得と、ストリーミングを伴わない更新（設定・ファイル編集） |

- すべての `/api/*` と `/ws` は Bearer token 認証（`channels.web.auth_token`）を要求する。token はブラウザの localStorage に保存し、401 / WS `unauthorized` で AuthModal を表示する
- API の詳細は [api.md](../api.md)

---

## 4. 状態管理

| 分類 | 例 | 保持場所 |
|---|---|---|
| URL State | 選択 agent / 画面 / セクション / セッション / run | URL |
| Server State | セッション・履歴・run・記憶・設定・メトリクス | キャッシュ層（無効化・リトライ・ポーリング） |
| Streaming State | 進行中の応答・ツール実行 | WS から派生する in-memory store |
| Ephemeral UI State | 入力ドラフト・編集中フォーム・モーダル・パレット | React のローカル state |

キャッシュの無効化は操作とイベントで明示的に行う。

| トリガ | 無効化対象 |
|---|---|
| チャット送信完了 / `session_updated` | セッション一覧・当該セッション履歴 |
| Pulse / Sleep の開始・終了 | Schedule 関連・Agent の Memory |
| ファイル・設定の保存成功 | 当該ファイル / 設定 |

楽観的更新はチャット送信時のユーザーメッセージ挿入のみとし、それ以外の更新はサーバー応答を待って反映する。

---

## 5. 関連ドキュメント

| トピック | ファイル |
|---|---|
| デザイントークン・共通コンポーネント | [design-system.md](./design-system.md) |
| レイアウト（Sidebar / Mobile / レスポンシブ） | [layout.md](./layout.md) |
| PWA | [pwa.md](./pwa.md) |
| Command Palette | [command-palette.md](./command-palette.md) |
| 各画面 | [chat.md](./chat.md) / [agent.md](./agent.md) / [schedule.md](./schedule.md) / [metrics.md](./metrics.md) / [config.md](./config.md) |
| HTTP / WS API | [../api.md](../api.md) |
