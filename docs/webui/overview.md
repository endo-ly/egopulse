# EgoPulse WebUI — Overview

WebUI は EgoPulse ランタイムの運転席である。エージェントの選択・観測・対話・監査を単一のブラウザセッションから行う。

## 1. 設計思想

### 1.1 Agent-First

すべての操作は agent を主体として編成される。チャット・Sleep 監査・Pulse 履歴・運用メトリクスのいずれも、対象 agent を選択してから扱う。チャネル・セッション・ツール実行は agent に従属する概念として扱う。

### 1.2 Observe, then Act

Discord / Telegram / CLI / TUI / Voice など外部チャネルで進行中の会話は、WebUI から **観測専用** として扱う。WebUI から他チャネルへ強制的にメッセージを送ることはせず、各チャネル本来の入力経路を尊重する。WebUI が能動的に書き込めるのは Web 起源のセッションのみ。

### 1.3 URL as Source of Truth

UI の状態は可能な限り URL にエンコードする。これにより、ブラウザの再読込・履歴・共有が自然に扱える。React の in-memory state は入力途中のドラフト・モーダルの開閉・コマンドパレットの照会など、URL に乗せると窮屈な一時的な状態のみに限定する。

### 1.4 Streaming First

LLM の応答・ツール実行・Pulse 発火など、ランタイムで進行するあらゆる事象は、完了後ではなく **進行中に UI へ反映する**。ユーザーは待機中であっても「いま何が起きているか」を必ず視認できる。

---

## 2. 情報設計

### 2.1 3 領域の構成

WebUI は Sidebar / Top Bar / Main の3領域で構成される。

```
┌─ Sidebar ────────┬─ Top Bar ──────────────────────────────────┐
│ EgoPulse         │ [⌘K]  Tabs…                  Health Badge  │
│ ─────────        ├─ Main ─────────────────────────────────────┤
│ AGENTS           │                                            │
│ Sessions         │   選択タブ + 選択 agent のコンテンツ        │
│ + New Session    │                                            │
│ ─────────        │                                            │
│ Runtime Status   │                                            │
└──────────────────┴────────────────────────────────────────────┘
```

| 領域 | 役割 |
|---|---|
| **Sidebar** | agent の選択・セッションの選択・新規セッション作成・ランタイム状態の簡易表示 |
| **Top Bar** | タブ切り替え・コマンドパレット起動・ヘルス状態の簡易表示 |
| **Main** | 選択中タブ・選択中 agent に従属するコンテンツ |

詳細は [layout.md](./layout.md)。

### 2.2 タブ構成

| タブ | 内容 | 詳細 |
|---|---|---|
| **Chat** | 選択 agent との Web セッション、または他チャネルセッションの read-only 監査 | [chat.md](./chat.md) |
| **Sleep** | Sleep Batch の実行履歴とメモリ差分の監査 | [sleep-batch.md](./sleep-batch.md) |
| **Pulse** | Pulse run の実行履歴と結果確認 | [pulse.md](./pulse.md) |
| **Metrics** | ランタイムの健全性・ターン履歴・エラーリスト | [metrics.md](./metrics.md) |
| **Config** | プロバイダー・Web サーバー・チャネル別オーバーライドの設定 | [config.md](./config.md) |

タブは常に5つすべて表示する。空状態（例：Sleep run が一件もない）では Main 領域内で適切な empty state を表示する。

### 2.3 Agent 選択のスコープ

Sidebar で選択した agent は、**Chat / Sleep / Pulse の3タブのコンテキスト**として扱う。Metrics と Config はグローバル扱い（agent 非依存）とし、Sidebar の agent 選択の影響を受けない。

- Chat タブ：選択 agent が関与するセッションのみ Sidebar の SESSIONS リストに表示
- Sleep タブ：選択 agent の Sleep run のみ表示
- Pulse タブ：選択 agent の Pulse run のみ表示
- Metrics タブ：全 agent のランタイム状態を常に表示（Sidebar の agent 選択の影響を受けない）
- Config タブ：グローバル設定と Channel Overrides を表示

---

## 3. トランスポート

### 3.1 WebSocket をストリーミングの主経路に

LLM 応答のストリーミング・ツール実行イベント・Pulse 発火・状態変化は、**WebSocket (`/ws`)** 上で配信する。SSE (`/api/stream`) は廃止し、WS 1本に統一する。

### 3.2 WS メッセージプロトコル

WS 上のメッセージは既存の JSON-RPC 風形式（[api.md §3](../api.md#3-websocket)）を踏襲する。チャット関連のイベントは現在 `chat` イベント1種で `state` フィールド (`delta` / `done` / `error`) により判別する形式だが、本仕様では以下を追加する：

- ツール実行を伝える `state` の新値（`tool_start` / `tool_result`）。現行の `chat` イベントペイロードを拡張し、ツール名・入力・出力・成否・所要時間を含める
- Pulse / Sleep の開始・終了を伝える新イベント（`pulse` / `sleep`）。`state` に `started` / `finished` を追加
- read-only セッションのリアルタイム更新のため、`chat` イベントペイロードに sessionKey（または chat_id）を含める

WS 上でこれらの事象が配信されることを要件とする。

### 3.3 REST API の役割

REST (`/api/*`) は **状態の取得と、ストリーミングを伴わない操作**のみを担う。

- `GET`: 一覧・詳細・履歴の取得
- `POST`: 即時応答を返す操作（設定の保存等）
- `PUT`: 設定の更新

チャット送信のキックは WS の `chat.send` メソッドで行う。外部 voice client 等のために REST の `/api/send_stream` は残すが、WebUI は WS 経由を本命とする。

### 3.4 認証

- すべての `/api/*` と `/ws` は Bearer token 認証（`channels.web.auth_token`）を要求
- token はブラウザの localStorage に保存
- 401 / WS `unauthorized` → AuthModal を表示し token 再入力を促す

---

## 4. 状態管理

### 4.1 状態の分類

| 分類 | 例 | 保持場所 |
|---|---|---|
| **URL State** | 選択 agent / タブ / セッション / Sleep run / Pulse run | URL（router 経由） |
| **Server State** | セッション一覧・履歴・Sleep runs・Pulse runs・Config・Health | キャッシュ層（無効化・リトライ・ポーリングを抽象化） |
| **Streaming State** | 進行中のチャット応答・ツール実行・Pulse 発火 | WS メッセージから派生する in-memory store |
| **Ephemeral UI State** | 入力ドラフト・モーダル開閉・palette 照会・トースト | React のローカル state |

### 4.2 Server State のキャッシュ戦略

各サーバー状態は agent 選択やフィルタ条件を含むキーでキャッシュする。無効化は操作トリガで明示的に行う：

- チャット送信完了 → セッション一覧と当該セッションの履歴を無効化
- Sleep 完了イベント受信 → Sleep run 関連のキャッシュを無効化
- Pulse 完了イベント受信 → Pulse run 関連のキャッシュを無効化
- Config 保存成功 → Config キャッシュを無効化

### 4.3 楽観的更新の扱い

**限定的に楽観的更新を採用する**。チャット送信時のユーザーメッセージ挿入のみ即座にタイムラインへ反映し、サーバー応答後に最終的な一覧と差し替える。これにより体感レイテンシを下げつつ、サーバー側の履歴を正とする整合性を保つ。

それ以外（設定保存・セッション新規作成・Sleep 実行トリガ等）は楽観的更新を行わず、サーバー応答を待って UI に反映する。失敗時はトーストで通知する。

---

## 5. ランタイムイベントと UI の対応

WS 上で配信される事象と UI 側の関心事の対応を以下に示す。

| WS 上の事象 | UI 利用 |
|---|---|
| チャットのトークン刻み応答 | Chat timeline のドラフトメッセージへ文字追加 |
| チャットの完了 | ドラフトの確定・最終メッセージへの差し替え |
| ツール実行の開始 | Tool Card の開始表示 |
| ツール実行の完了 | Tool Card の成否・所要時間の反映 |
| チャットの進行状態変化 | Composer 上部の状態表示（"iteration 2" など） |
| チャットのエラー | トースト + timeline 上のエラーバブル |
| Pulse の開始・終了 | Pulse タブのリスト更新・Sidebar の agent live indicator |
| Sleep の開始・終了 | Sleep タブのリスト更新・Sidebar の agent live indicator |

Health 状態（`/health` 相当）は REST の polling で取得する（[metrics.md](./metrics.md) 参照）。

---

## 6. 主要ドキュメント

| トピック | ファイル |
|---|---|
| デザイントークン・共通コンポーネント | [design-system.md](./design-system.md) |
| 全体レイアウト（Sidebar / Top Bar / レスポンシブ） | [layout.md](./layout.md) |
| Chat タブ | [chat.md](./chat.md) |
| Sleep Batch タブ | [sleep-batch.md](./sleep-batch.md) |
| Pulse タブ | [pulse.md](./pulse.md) |
| Metrics タブ | [metrics.md](./metrics.md) |
| Config タブ | [config.md](./config.md) |
| Command Palette | [command-palette.md](./command-palette.md) |
| HTTP / WS API リファレンス | [../api.md](../api.md) |
