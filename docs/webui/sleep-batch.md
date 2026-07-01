# EgoPulse WebUI — Sleep Tab

Sleep batch（記憶整理処理）の実行履歴とメモリ差分を監査するためのタブ。

主用途は、個別 run の before/after diff 確認（記憶がどう書き換わったかの監査）。Sleep batch 自体はスケジュール実行または手動トリガーで動作し、`sleep_runs` と `memory_snapshots` テーブルに監査データが格納される。Sleep Tab はこのデータを人間が目視確認できる形で表示する。

## 1. 構成

Sleep Tab は **Run List ビュー** と **Run Detail ビュー** を切り替える。ビュー切替は URL で表現する。

```
┌─ Sleep Tab (List) ───────────────────────────────────┐
│ ┌─ Sleep Header ────────────────────────────────┐    │
│ │  Sleep Batch Audit                              │    │
│ │  lyre · 24 runs · last 2026-06-29 03:00         │    │
│ │                              [agent: lyre ▼] [↻] │    │
│ └────────────────────────────────────────────────┘    │
│                                                       │
│ ┌─ Run List ────────────────────────────────────┐     │
│ │ ✅ 2026-06-29 03:00  scheduled  1.2k tok      │     │
│ │    3 sessions processed              [Details→]│     │
│ │ ─────────                                     │     │
│ │ ❌ 2026-06-28 15:22  manual     0 tok         │     │
│ │    Error: parse failed         [Details→]     │     │
│ │ ─────────                                     │     │
│ │ ⏭ 2026-06-28 03:00  scheduled  skipped        │     │
│ │    No new messages               [Details→]    │     │
│ └────────────────────────────────────────────────┘     │
└───────────────────────────────────────────────────────┘

  ↓ [Details →]

┌─ Sleep Tab (Detail) ─────────────────────────────────┐
│ ← Back   Run: a1b2c3d4   ✅ success                 │
│                                                       │
│ ┌─ Run Meta ─────────────────────────────────────┐   │
│ │ Started    2026-06-29 03:00:12                  │   │
│ │ Finished   2026-06-29 03:00:45  (33s)           │   │
│ │ Trigger    scheduled                            │   │
│ │ Tokens     1,247 in / 420 out                   │   │
│ │ Sessions   3 chats processed                    │   │
│ └─────────────────────────────────────────────────┘   │
│                                                       │
│ Memory Changes                       [Split ▼]        │
│                                                       │
│ ▼ episodic.md                                         │
│ ┌─ Diff ──────────────────────────────────────────┐  │
│ │ (side-by-side or unified diff)                  │  │
│ └─────────────────────────────────────────────────┘  │
└───────────────────────────────────────────────────────┘
```

---

## 2. Sleep Header

- タイトル（"Sleep Batch Audit"）
- 選択 agent / 総 run 数 / 最終実行時刻の要約
- agent フィルタ（ドロップダウン）：Sidebar で選択中 agent をデフォルトとするが、"All agents" を選ぶと全 agent の run を混在表示
- Refresh button

agent フィルタの変更は URL query で表現し、Sidebar の選択 agent とは独立に保持できる。

---

## 3. Run List ビュー

### 3.1 Run Card

各 run をカード形式で表示。1行目にステータスアイコン・開始時刻・trigger badge・トークン総量、2行目に status 別の summary、右端に "Details →" リンク。

### 3.2 Status Icon

| status | icon | 色 |
|---|---|---|
| `success` | ✅ | success |
| `partial_failure` | ⚠️ | warning |
| `failed` | ❌ | danger |
| `skipped` | ⏭ | muted |
| `running` | 🔄 | accent |

### 3.3 Trigger Badge

`trigger_type` に応じてバッジを表示：

- `scheduled`：muted バッジ、label `SCHEDULED`
- `manual`：アクセント2色の半透明バッジ、label `MANUAL`
- `backfill`：warning 系バッジ、label `BACKFILL`

### 3.4 Summary Text

status に応じて2行目のテキストを生成：

| status | summary 例 |
|---|---|
| `success` | `{session_count} sessions processed` |
| `partial_failure` | `{session_count} sessions processed · {failed_count} failed` |
| `failed` | `Error: {error_message 先頭60字}` |
| `skipped` | `Skipped: {理由}`（理由は `error_message` または status から推定） |
| `running` | `Running… (started {relativeTime})` |

### 3.5 List Order・件数制限

- `started_at` 降順（最新が上）
- `limit` query で取得件数を制御（デフォルト 50）
- 無限スクロール（IntersectionObserver）または "Load more" ボタンで追加取得

### 3.6 Empty States

| 状態 | 表示 |
|---|---|
| run 0件 | EmptyState: "No sleep batch runs yet. The scheduler will create runs automatically." |
| agent 0件（fresh DB） | EmptyState: "No agents with sleep history yet." |
| ロード中 | 大 spinner 中央 |
| ロード失敗 | EmptyState: error + retry |

---

## 4. Run Detail ビュー

### 4.1 戻る操作

- "← Back" ボタンで Run List へ戻る
- ブラウザの戻るボタンでも同様

### 4.2 Run Meta Card

以下のメタ情報を縦並びで表示：

| 項目 | ソース | 表示 |
|---|---|---|
| Status | `status` | アイコン + status 名 |
| Agent | `agent_id` | agent ラベル（無ければ agent_id） |
| Trigger | `trigger_type` | そのまま表示 |
| Started | `started_at` | ローカル時刻 `YYYY-MM-DD HH:mm:ss` |
| Finished | `finished_at` | ローカル時刻 + 所要時間（`finished_at - started_at`） |
| Tokens | `input_tokens`, `output_tokens` | カンマ区切り + `in / out` |
| Sessions | `session_count` | `N chats processed` |

### 4.3 Error Block

`error_message` がある場合、Run Meta の下にエラーブロックを表示。

- danger 系背景、左に3pxの danger 色ボーダー
- エラーメッセージ全文を等幅 `pre` で表示（折り返し有効）
- `role="alert"`

### 4.4 Memory Changes Section

#### Status 別の表示ルール

| Status | Memory Changes 表示 |
|---|---|
| `success` | snapshots の before/after を diff 表示 |
| `partial_failure` | 同上。成功した処理の snapshot を表示 |
| `failed` | snapshots があれば diff 表示、なければ `"No snapshots available"` を表示 |
| `skipped` | Memory Changes セクション全体を非表示 |
| `running` | Memory Changes セクション全体を非表示。"Still running… progress: n/N" を表示 |

#### diff モードの切り替え

- split（side-by-side）と unified の2モード
- デフォルト：desktop は split、mobile（< 768px）は unified
- ユーザーが変更したモードは Run Detail 内の全ファイルで共有、同一セッション表示中は維持

### 4.5 Snapshot Section

3ファイル（`episodic` / `semantic` / `prospective`）をそれぞれ展開可能なセクションで表示。

- ヘッダー：折りたたみアイコン + `{file}.md` + 変更行数 badge（`+12 -3` 等）または `no changes` badge
- デフォルト：変更のあるファイルは展開、変更のないファイルは折りたたみ
- 変更がないファイルは展開しても "No changes in {file}.md" を表示

### 4.6 DiffViewer

行レベルの diff を算出（LCS ベース）して表示する。**文字単位の diff は行わない**。外部 diff ライブラリは導入せず自前実装する。

#### Split Mode

- 2カラム（Before / After）
- 各行の左端に行番号
- 追加行：success 系背景 + 行頭 `+`
- 削除行：danger 系背景 + 行頭 `-`
- 変更なし行：背景なし

#### Unified Mode

- 1カラム
- 追加行：success 系背景 + `+` prefix
- 削除行：danger 系背景 + `-` prefix
- 行番号は Before / After の2列

#### Long Content

- 各 diff カラムは最大高さを画面高の 60% 程度に制限、`overflow-y: auto`
- 1ファイルの差分が500行を超える場合は最初の500行のみ表示し、"Show all (1234 lines)" ボタンで全行展開

---

## 5. バックエンド API

### 5.1 `GET /api/sleep/runs`

Run 一覧を返す。

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
      "status": "success",
      "trigger_type": "scheduled",
      "started_at": "2026-06-29T03:00:12Z",
      "finished_at": "2026-06-29T03:00:45Z",
      "input_tokens": 1247,
      "output_tokens": 420,
      "total_tokens": 1667,
      "error_message": null,
      "session_count": 3
    }
  ]
}
```

`session_count` は `source_chats_json`（JSON配列）の length から算出。

### 5.2 `GET /api/sleep/runs/:run_id`

単一 run の詳細と memory snapshots。

**Response 例:**

```json
{
  "ok": true,
  "run": { /* same as list item */ },
  "snapshots": [
    {
      "file": "episodic",
      "content_before": "# Episodic Memory\n...",
      "content_after": "# Episodic Memory\n..."
    },
    { "file": "semantic", "content_before": "...", "content_after": "..." },
    { "file": "prospective", "content_before": "...", "content_after": "..." }
  ]
}
```

`snapshots` は変更のあったファイルのみ（`content_before != content_after`）。変更のないファイルは含めない。

### 5.3 リアルタイム更新

Sidebar の AGENTS section で Sleep の開始・終了 WS イベントを受信した場合、選択 agent の Sleep run list を自動的に無効化して再取得する。Run Detail 表示中の run が完了した場合も同様。

---

## 6. Out of Scope

- Sleep batch の再実行ボタン（WebUI からの実行トリガーは提供しない）
- 統計・集計ダッシュボード（成功率・トークン推移等の集計）
- LLM 入力プロンプトの表示（system prompt の中身は表示しない）
- 文字単位の diff（行レベルで十分）
