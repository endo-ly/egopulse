# EgoPulse WebUI — Sleep Tab

Sleep batch（記憶整理処理）の実行履歴・ステップ結果・メモリ差分・現在の長期記憶を監査するためのタブ。

主用途は 2 つ:

1. **Runs ビュー**: 個別 run の before/after diff 確認（記憶がどう書き換わったかの監査）
2. **Memory ビュー**: agent の現在の公開済み長期記憶 3 ファイルの閲覧

Sleep batch 自体はスケジュール実行または手動トリガー（CLI）で動作し、`sleep_runs` / `sleep_run_steps` / `memory_snapshots` テーブルに監査データが格納される。Sleep Tab はこのデータを人間が目視確認できる形で表示する。

## 1. 構成

```
┌ Sleep ─────────────────────────────────────────────────────────┐
│  [ Runs | Memory ]                [All agents ▾]  [↻]          │
│ ┌──────────────────────────┬─────────────────────────────────┐ │
│ │ ● main     sched    3h   │  ● Partial failure      #a1b2c3 │ │
│ │ ● work     manual   1d   │  Lyre · Scheduled · 3 sessions  │ │
│ │ ○ side     sched    2d   │  Jul 4 03:00 → 03:04 (4m) 1.2k  │ │
│ │ ...                      │  STEPS / MEMORY CHANGES ...     │ │
│ └──────────────────────────┴─────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────────┘
```

- デスクトップ（≥ 1024px）: 左に run 一覧（スマートな縦リスト）、右に選択 run の詳細を表示する 2 ペイン構成
- 1023px 以下: 一覧が全幅で表示され、run 選択で詳細が全画面に切り替わる（`←` で戻る）。767px 以下は詳細の meta×steps も縦積みになる
- ビュー切替・run 選択はすべて URL で表現する（§4）

## 2. Sleep Header

- ビュー切替（下線タブ）: `Runs` / `Memory`。ヘッダー全体の下罫線がタブの下線を兼ねる。タブ幅はグループ内で等幅（中央寄せ）
- Refresh button（icon-only、右端）: Sleep 関連の全クエリを無効化して再取得

## 3. Runs ビュー

### 3.1 Run 一覧（左ペイン）

ペイン上部に agent フィルタ（`All agents` または特定 agent）。フィルタはパネルローカルな状態で、Sidebar の選択 agent とは独立。その下に区切り線ベースの 1 行リスト（カードは使わない）。1 行の構成:

```
● {agent label} · {trigger}          {relative time}
```

| 要素 | 内容 |
|---|---|
| Status dot | §3.2 のトーン色のドット |
| agent label | `/api/agents` の `label`（見つからない場合は `agent_id`） |
| trigger | `Scheduled` / `Manual` / `Backfill` |
| relative time | `now` / `5m` / `3h` / `2d`、30 日以上前は短い日付 |

- hover で薄いハイライト、選択行は左端に accent バー + 薄いハイライト
- `started_at` 降順。20 件ずつ取得し、ページが満杯のとき `Load more` ボタンで追加取得
- 10 秒間隔のポーリングで自動更新（タブ可視中のみ）
- Empty state: `No sleep batch runs yet`

### 3.2 Status

| status | dot 色 |
|---|---|
| `success` | success |
| `partial_failure` | warning |
| `failed` | danger |
| `skipped` | muted |
| `running` | accent（点滅） |

### 3.3 Run 詳細（右ペイン）

未選択時は `No run selected` の EmptyState。選択時は縦積みスクロールで表示:

1. **ヘッダー**: status dot + status 名（`Partial failure` 等の humanize 表記）+ run ID 短縮形（先頭 6 文字、`#` 付き）。モバイルでは `←` back ボタンを併設
2. **メタ行**: agent label · trigger · session 数
3. **サブ行**: 開始 → 終了時刻（短いローカル時刻）+ 所要時間 + 総トークン数
4. **Error**: run 直下のエラーブロックは持たない。エラーは失敗ステップの行内に等幅で常時表示する。ステップが 1 つも実行されなかった run が `error_message` のみを持つ場合は、Steps セクションと同じスタイルで Error セクションを表示する
5. **Steps**: §3.4
6. **Memory Changes**: §3.5

### 3.4 Steps

4 ステップをパイプライン順（event_extraction → episodic_update → semantic_update → prospective_update）で表示。

| step status | icon |
|---|---|
| `success` | `✓`（success 色） |
| `failed` | `✗`（danger 色、行背景も danger soft） |
| `skipped` | `–`（muted） |
| `running` / `pending` | spinner |

- 各行の右にステップのトークン合計（0 の場合は非表示）
- `error_message` があるステップは折りたたみなしでエラー詳細（等幅）を常時表示
- ステップデータは run detail API の `steps` から取得（§5.2）

### 3.5 Memory Changes

- file tabs: `episodic` / `semantic` / `prospective`（スナップショットに存在するファイルのみ）。ヘッダーのビュー切替と共通の下線タブスタイルを使い、変更のあるファイルのタブには増減行数（`+12 −3`）を表示
- 既定の選択: 最初に変更のあるファイル（変更がなければ最初のファイル）
- `skipped` run、またはスナップショットが存在しない run（`running` 中など）ではセクションごと非表示
- diff 表示は §3.6

### 3.6 DiffViewer

行レベルの diff を算出（LCS ベース）して表示する。**文字単位の diff は行わない**。外部 diff ライブラリは導入せず自前実装する。

- split（side-by-side）と unified の 2 モード。ビューポート幅に追従して既定値が決まる（desktop は split、1023px 以下は unified）。ユーザーが明示的に切り替えた場合はその選択を優先
- 追加行: success 系背景 + `+` prefix（unified のみ）/ 削除行: danger 系背景 + `-` prefix（unified のみ）
- diff 領域の最大高さは 60vh、`overflow-y: auto`
- 500 行を超える差分は最初の 500 行のみ表示し、`Show all {n} lines` ボタンで全行展開

## 4. URL 構造

| URL | 状態 |
|---|---|
| `/agents/:agentId/sleep` | Runs ビュー（未選択） |
| `/agents/:agentId/sleep/runs/:runId` | Runs ビュー（run 選択済み） |
| `/agents/:agentId/sleep/memory` | Memory ビュー |

- ブラウザの戻る/進むで状態を復元する
- モバイルでは run 選択 URL が詳細全画面に対応する
- agent を切り替えると run 選択は解除される（ビューは維持）

## 5. バックエンド API

API 詳細は [api.md](../api.md) §2.9。

### 5.1 `GET /api/sleep/runs`

`agent_id`（任意）、`limit`（default 20）、`offset`（default 0）をサポート。`started_at` 降順で返す。

### 5.2 `GET /api/sleep/runs/:run_id`

`run` + `snapshots`（確定済み run は 3 ファイルフルセット）+ `steps`（パイプライン順）を返す。

### 5.3 `GET /api/agents/:agent_id/memory`

現在の公開済み長期記憶 3 ファイルの内容を返す。

### 5.4 リアルタイム更新

WS による sleep イベントは未実装。代わりに `useServerState` の可視時ポーリング（run 一覧・詳細とも 10 秒間隔）で running run の進行を追従する。

## 6. Out of Scope

- Sleep batch の再実行ボタン（WebUI からの実行トリガーは提供しない）
- 統計・集計ダッシュボード（成功率・トークン推移等の集計）
- LLM 入力プロンプトの表示（system prompt の中身は表示しない）
- 文字単位の diff（行レベルで十分）
- Memory ビューの Markdown レンダリング（プレーンテキスト表示のまま）
