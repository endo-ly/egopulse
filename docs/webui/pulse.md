# EgoPulse WebUI — Pulse Tab

Pulse（注意活性化）の実行履歴を監査するためのタブ。

Pulse は agent 単位で動作し、Temporal Intention が due になったときに対象 agent の注意を短く活性化し、必要なときだけ Home Surface へ通知を出す機構（[pulse.md](../pulse.md) 参照）。Pulse Tab は `pulse_runs` テーブルの実行履歴を人間が確認できる形で表示する。

Sleep Tab と設計パターンを共有する（一覧 + 詳細、状態アイコン、before/after 系の表示）が、Pulse 固有の情報（intention / output_kind / Home Surface）を適切に扱う。

## 1. 構成

Pulse Tab は **Run List ビュー** と **Run Detail ビュー** を切り替える。ビュー切替は URL で表現する。

```
┌─ Pulse Tab (List) ──────────────────────────────────┐
│ ┌─ Pulse Header ────────────────────────────────┐   │
│ │  Pulse Activations                              │   │
│ │  lyre · 56 runs · last 2026-06-29 09:00         │   │
│ │                              [agent: lyre ▼] [↻]│   │
│ └────────────────────────────────────────────────┘   │
│                                                       │
│ ┌─ Run List ────────────────────────────────────┐    │
│ │ ◴ 2026-06-29 09:00  morning_review   notify   │    │
│ │    "今日のMTG確認して…"            [Details→]   │    │
│ │ ─────────                                     │    │
│ │ ◴ 2026-06-29 09:00  weekly_review    silent   │    │
│ │    PULSE_OK                        [Details→]  │    │
│ │ ─────────                                     │    │
│ │ ❌ 2026-06-28 21:00  evening_check   failed    │    │
│ │    Error: rate limited             [Details→]  │    │
│ └────────────────────────────────────────────────┘    │
└───────────────────────────────────────────────────────┘

  ↓ [Details →]

┌─ Pulse Tab (Detail) ─────────────────────────────────┐
│ ← Back   Run: c0f3...   ◴ notify                    │
│                                                       │
│ ┌─ Run Meta ─────────────────────────────────────┐   │
│ │ Agent        lyre                               │   │
│ │ Intention    morning_review                     │   │
│ │ Started      2026-06-29 09:00:00                │   │
│ │ Finished     2026-06-29 09:00:08  (8s)          │   │
│ │ Home Surface discord:1234567890                 │   │
│ └─────────────────────────────────────────────────┘   │
│                                                       │
│ Output                                                │
│ ┌─────────────────────────────────────────────────┐  │
│ │ 今日のMTG確認して。あと昨日の設計論点は…         │  │
│ │ (Home Surface へ送信済み)                       │  │
│ └─────────────────────────────────────────────────┘  │
└───────────────────────────────────────────────────────┘
```

---

## 2. Pulse Header

- タイトル（"Pulse Activations"）
- 選択 agent / 総 run 数 / 最終実行時刻の要約
- agent フィルタ（ドロップダウン）：Sleep Header と同様に Sidebar 選択 agent と独立に保持
- Refresh button

---

## 3. Run List ビュー

### 3.1 Pulse Run Card

各 run をカード形式で表示。1行目に PulseIcon・開始時刻・intention_id・output_kind badge、2行目に output_kind 別の summary、右端に "Details →" リンク。

### 3.2 Output Kind Badge

`output_kind` に応じてバッジ色を変える：

| `output_kind` | 色 | label |
|---|---|---|
| `silent` | muted | `SILENT` |
| `notify` | accent | `NOTIFY` |
| `failed` | danger | `FAILED` |

### 3.3 Summary Text

`output_kind` と `status` に応じて2行目を生成：

| output_kind / status | summary 例 |
|---|---|
| `silent` | `PULSE_OK · no notification sent` |
| `notify` | 通知本文の先頭60字（markdown 無視・plain text 抽出） |
| `failed` | `Error: {error_message 先頭60字}` |
| status=`running` | `Running… (started {relativeTime})` |
| status=`skipped` | `Skipped: {reason}`（duplicate / active_turn defer / home surface unresolved 等） |

### 3.4 Status Icons

Sleep と同じアイコンセットを使う：

| status | icon |
|---|---|
| `success`（notify / silent） | ✅ |
| `failed` | ❌ |
| `skipped` | ⏭ |
| `running` | 🔄 |

### 3.5 List Order・Empty States

Sleep に準拠（`started_at` 降順、`limit` デフォルト50、EmptyState パターン共通）。

---

## 4. Run Detail ビュー

### 4.1 Run Meta Card

以下のメタ情報を表示：

| 項目 | ソース | 表示 |
|---|---|---|
| Agent | `agent_id` | agent ラベル（無ければ agent_id） |
| Intention | `intention_id` | 等幅表示 |
| Started | `started_at` | ローカル時刻 |
| Finished | `finished_at` | ローカル時刻 + 所要時間 |
| Home Surface | `chat_id` から lookup | `{channel}:{external_chat_id}` 形式（lookup 不能な場合は `chat #{chat_id}`） |

`chat_id` が null（silent / failed）の場合は Home Surface 行を表示しない。トークン量は `pulse_runs` テーブルに記録されないため表示しない。

### 4.2 Output Section

`output_kind` に応じて出力を表示：

| output_kind | 表示 |
|---|---|
| `notify` | 通知本文を Markdown で表示。送信先 Home Surface を右上バッジで明示 |
| `silent` | EmptyState: "PULSE_OK · no notification was sent." |
| `failed` | エラーブロック（danger 背景 + error_message） |

### 4.3 Tool 実行の表示

Pulse Activation はツール使用可能（[pulse.md §14](../pulse.md#phase-1-implementation-decisions)）。LLM がツールを呼んだ場合、Chat Tab と同じ Tool Card を Output Section の下に表示する（折りたたみ inline、[chat.md §5](./chat.md#5-tool-実行カード) に従う）。

ただし、Pulse Capsule の内部ログ（system prompt への注入内容・Recent Visible Context・Core Contract 等）は監査対象外。ユーザーに見える通知本文とツール実行結果のみを表示する。

ツール実行履歴を Pulse run 詳細に含めるため、バックエンド側で `pulse_runs` と `tool_calls` テーブルを関連付ける（trace_id または run_id で JOIN）クエリを提供する。Run Detail API レスポンスの `tools` 配列でツール実行履歴を返す。

---

## 5. Attention Section

Pulse の `attention`（intention 定義内のテキスト）は、LLM が何に注意を向けるべきだったかを示す重要な監査情報。Run Detail に表示する。

### 実現方針

`pulse_runs` テーブルに `attention_snapshot` カラムを追加し、run 開始時に当時の `attention` テキストを保存する。Run Detail API レスポンスでこのスナップショットを返す。

これにより、run 完了後に `PULSE.md` が編集されても、当時の attention を正確に再現できる。

### 表示仕様

Run Meta Card の下に独立セクションとして表示：

- セクションタイトル："Attention"
- 本文：`attention_snapshot` を Markdown レンダリング
- panel-2 背景、左ボーダー `3px solid var(--color-border-strong)`
- 本文体は `text-sm`

---

## 6. バックエンド API（新設）

### 6.1 `GET /api/pulse/runs`

Run 一覧。

**Query Parameters:**

| param | required | default | 説明 |
|---|---|---|---|
| `agent_id` | no | (all) | agent 絲り込み |
| `limit` | no | 50 | 最大100 |

**Response 例:**

```json
{
  "ok": true,
  "runs": [
    {
      "id": "uuid",
      "agent_id": "lyre",
      "intention_id": "morning_review",
      "status": "success",
      "output_kind": "notify",
      "started_at": "2026-06-29T09:00:00Z",
      "finished_at": "2026-06-29T09:00:08Z",
      "chat_id": 42,
      "message_id": "msg-uuid",
      "output_text": "今日のMTG確認して。...",
      "error_message": null
    }
  ]
}
```

> **注記**: `pulse_runs` テーブルにトークン量（input/output/total）のカラムは存在しない（[pulse.md §11](../pulse.md#11-db-最小仕様)）。Run 一覧・詳細でもトークン量は返さない。
```

#### フィールド補足

| フィールド | 説明 |
|---|---|
| `output_kind` | `silent` / `notify` / `failed` |
| `chat_id` | 通知先の chat。silent / failed の場合は null |
| `message_id` | 通知本文の message ID。silent / failed の場合は null |
| `output_text` | LLM 出力。`silent` の場合は `"PULSE_OK"`。`failed` の場合は null |
| `error_message` | 失敗時の詳細 |

### 6.2 `GET /api/pulse/runs/:run_id`

単一 run の詳細。list item と同じフィールドに加え、以下を含める：

- `attention_snapshot`: run 開始時点の `attention` テキスト（§5 参照）
- `tools`: ツール実行履歴の配列（§4.3 参照）。各要素はツール名・入力・出力・成否・所要時間を持つ

### 6.3 リアルタイム更新

Sleep と同様、Pulse の開始・終了 WS イベント受信でキャッシュを無効化し再取得。

---

## 7. Pulse 通知の Chat Tab での表示

Pulse が Home Surface へ通知を送った場合、そのメッセージは通常 session（chat_id）へ assistant message として保存される（[pulse.md §10](../pulse.md#10-出力仕様)）。

Chat Tab で当該 session を開いたとき、このメッセージは [chat.md §4.5](./chat.md#45-pulse-通知の識別) に従い、通常 assistant bubble と同じレイアウトで表示される。Pulse 識別の方法は chat.md 側に委ねる。

Pulse Tab の Run Detail から、対応する Chat session へジャンプできる（chat_id を用いて `/agents/:agentId/chat/s/chat:{chat_id}` へリンク）。

---

## 8. Out of Scope

- Pulse Intention の CRUD 編集（`PULSE.md` の編集はファイルシステム経由）
- Pulse の手動発火ボタン（テスト用途でも WebUI からは提供しない）
- 集計ダッシュボード（発火頻度・silent率等の統計）
- Pulse Capsule 内部ログの表示（`PULSE.md` body / Recent Visible Context / Core Contract 等）
- Pulse の強制 skip / 再実行（due_key 消去等の操作は CLI から）
