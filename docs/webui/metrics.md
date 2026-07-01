# EgoPulse WebUI — Metrics Tab

ランタイムの健全性・ターン履歴・エラー詳細を可視化するタブ。`/health` と `/telemetry` のデータを消費し、運用時の異常検知と原因切り分けを支援する。

情報密度は **数値カード + エラーリスト** に統一する。

## 1. 構成

```
┌─ Metrics Tab ────────────────────────────────────────┐
│ ┌─ Metrics Header ──────────────────────────────┐    │
│ │  Runtime Health                                 │    │
│ │  Updated 12s ago · auto-refresh 10s             │    │
│ │                                  [↻ Refresh]     │    │
│ └────────────────────────────────────────────────┘    │
│                                                       │
│ ┌─ Stat Cards ─────────────────────────────────┐     │
│ │ ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐  │     │
│ │ │ Active │ │ Total  │ │ Errors │ │ Uptime │  │     │
│ │ │   2    │ │  142   │ │   3    │ │  24h   │  │     │
│ │ │ turns  │ │ turns  │ │ recent │ │        │  │     │
│ │ └────────┘ └────────┘ └────────┘ └────────┘  │     │
│ │ ┌────────┐ ┌────────┐ ┌────────┐             │     │
│ │ │  DB    │ │ MCP    │ │Channels│             │     │
│ │ │  ok    │ │ 2 / 2  │ │ 3 / 3  │             │     │
│ │ └────────┘ └────────┘ └────────┘             │     │
│ └────────────────────────────────────────────────┘   │
│                                                       │
│ Channels                                              │
│ ┌─────────────────────────────────────────────────┐  │
│ │ ● web       running · 14s ago                    │  │
│ │ ● discord   running · 2m ago                     │  │
│ │ ○ telegram  failed  · bot token rejected         │  │
│ └─────────────────────────────────────────────────┘  │
│                                                       │
│ Recent Errors                                         │
│ ┌─────────────────────────────────────────────────┐  │
│ │ 2026-06-29 14:32  llm / lyre / discord           │  │
│ │ rate limited (retry 3/3)                          │  │
│ │ ─────                                            │  │
│ │ 2026-06-29 14:30  channel_send / ace / discord   │  │
│ │ Missing access                                    │  │
│ └─────────────────────────────────────────────────┘  │
│                                                       │
│ Recent Turns                                          │
│ ┌─────────────────────────────────────────────────┐  │
│ │ 14:32:05  lyre / discord / 5.2s / ok            │  │
│ │ 14:31:48  ace  / web     / 1.8s / ok            │  │
│ │ 14:31:20  lyre / discord / 0.3s / error         │  │
│ └─────────────────────────────────────────────────┘  │
└───────────────────────────────────────────────────────┘
```

Metrics Tab は4領域で構成される：

1. Metrics Header
2. Stat Cards（数値カード群）
3. Channels（チャネル別状態）
4. Recent Errors / Recent Turns（運用ログ）

---

## 2. Metrics Header

- タイトル（"Runtime Health"）
- 前回取得時刻と polling 間隔の表示
- 手動 Refresh button

### Polling 間隔

- `/health` と `/telemetry` を **10秒** 毎にポーリング
- タブがバックグラウンドのときは 30秒に緩める（`document.visibilityState` で切替）
- ユーザーが手動 Refresh を押した場合は即時再取得

---

## 3. Stat Cards

2行のグリッド（desktop: 4列 / tablet: 3列 / mobile: 2列）に並べた数値カード。

### 3.1 StatCard の構造

各カードは3要素で構成：

- 値（特大・強調本文、tone 色があれば着色）
- ラベル（小テキスト・muted・uppercase）
- hint（極小テキスト・muted-2）

### 3.2 カード定義

| Label | Source | 表示形式 | tone 条件 |
|---|---|---|---|
| Active turns | `health.active_turns` | 数値 | `> 0` で live（pulse アニメ） |
| Total turns | `telemetry.metrics["egopulse_turns_total"]` の総和 | 数値 | 常に default |
| Recent errors | `health.recent_errors_count` | 数値 | `> 0` で danger、`0` で success |
| Uptime | `health.uptime_secs` | `Xd Yh Zm` 形式 | 常に default |
| DB | `health.db.ok` | `true` → `"ok"` / `false` → `"down"` | `true` で success、`false` で danger |
| MCP | `health.mcp.healthy / failed` | `"{healthy} / {total}"` | `failed > 0` で warning、他 success |
| Channels | `health.channels` | running / 全チャネル 数 | `failed > 0` で warning、他 success |

---

## 4. Channels セクション

`/health` の `channels` オブジェクトを元に、各チャネルの状態をリスト表示。

### 4.1 Item 構成

1行に以下を並べる：

- StatusDot（state に応じた色）
- channel name
- state badge
- detail（`last_error` があれば優先、なければ `last_activity` の相対時刻）

### 4.2 Channel State Mapping

| state | tone | badge | 意味 |
|---|---|---|---|
| `starting` | live | info | 起動中 |
| `running` | live | success | 正常稼働 |
| `failed` | error | danger | 異常停止 |
| `stopped` | idle | muted | 停止中 |

### 4.3 失敗 channel の表示

失敗 channel は `last_error` の先頭部分を1行で表示する（ellipsis 付き）。全文展開は行わない。

---

## 5. Recent Errors セクション

`/telemetry` の `recent_errors` 配列を表示。

### 5.1 Item 構成

1行目：時刻 / error_kind badge / agent_id（あれば）/ channel badge（あれば）/ trace_id（短縮）
2行目：summary（1行、ellipsis 付き）

- 失敗が新しいほど上（`at` 降順）
- 最大100件表示、それ以上は "Showing latest 100" 通知

### 5.2 Empty State

recent_errors が0件の場合、success トーンの EmptyState（"No recent errors" "All good in the last 100 events."）を表示。

---

## 6. Recent Turns セクション

`/telemetry` の `recent_turns` 配列を table 形式で表示。

### 6.1 Column 構成

| Column | 内容 |
|---|---|
| Started | `started_at` を時刻表示 |
| Agent | `agent_id` |
| Channel | channel badge |
| Duration | `duration_secs` を秒表示 |
| Status | `ok` なら success badge、さもなくば danger badge |
| Trace | `trace_id` の短縮形（先頭8字） |

### 6.2 振る舞い

- 1行 = 1 turn
- 新しい順（`started_at` 降順）
- 最大100件表示（`/telemetry` の返却上限に同じ）
- error 行は行全体に薄い danger 系背景
- Metrics タブ内で **agent / channel の独立フィルタ** を提供（Sidebar の agent 選択とは連動させない。Sidebar は Chat/Sleep/Pulse のコンテキストに専念）
- trace_id はクリックでクリップボードへコピー、トーストで "Copied trace_id"

### 6.3 Empty State

recent_turns が0件（ランタイム起動直後等）の場合、EmptyState（"No turns yet" "Turns will appear here once the agent processes a message."）を表示。

---

## 7. バックエンド API

### 7.1 `GET /health`

変更なし（[api.md §2.1](../api.md#21-ヘルスチェック)）。チャネル状態・DB・MCP・active_turns・recent_errors_count を返す。

### 7.2 `GET /telemetry`

変更なし（[api.md §2.2](../api.md#22-テレメトリー)）。metrics map・recent_turns・recent_errors を返す。

---

## 8. アクセシビリティ

- StatCard の値は `aria-label="{label}: {value}"` で読み上げ
- error log リストは `role="log"` `aria-live="polite"`（新規エラー追加時に読み上げ）
- turns-table は標準的な `<table>` マークアップ、`scope="col"` 必須
- polling 中は Metrics Header に `aria-busy="true"` を設定

---

## 9. Out of Scope

- 時系列グラフ（tokens / turns / errors の推移）
- MCP サーバー個別詳細表示（`health.mcp.servers[]` の個別展開）
- ログストリーミング（`tracing` のログを WebUI で見せる機能）
- アラート設定（特定条件で通知）
- メトリクスの export / CSV ダウンロード
- Health 状態の WS プッシュ（REST polling のみ提供）
