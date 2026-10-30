# EgoPulse WebUI — Schedule Tab

選択中 agent の自律動作（Pulse / Sleep）について、「これから何が起きるか」と「何が起きたか」を一本の時間軸で確認・監査するタブ。

- **Upcoming**: Pulse の次回発火と、Sleep の次回判定
- **History**: Pulse run と Sleep run（判定結果を含む）の時系列。各 run の詳細監査
- **Intentions**: Pulse intention の定義と実績

## 1. 構成

```
┌ Schedule · lyre ─────────────── [All | Pulse | Sleep] [Intentions] ┐
│ UPCOMING                                                          │
│  今日 21:00  ◴ evening_check     → telegram:9876…                 │
│  明日 09:00  ◴ morning_review    → discord:#general               │
│ ┌ 🌙 Sleep ──────────────────────────────────────────────────┐    │
│ │ 次回判定  明日 03:00                                         │    │
│ │ pending messages  41 / 65 で実行  ▓▓▓▓▓▓░░░░                 │    │
│ │ 現時点ではスキップ見込み                                       │    │
│ └────────────────────────────────────────────────────────────┘    │
├─────────────────────────────┬─────────────────────────────────────┤
│ HISTORY        [status ▾]   │ ◴ morning_review       ● notify     │
│ ◴ 09:00 morning   notify    │ 2026-10-30 09:00 → 09:00:08 (8s)    │
│   "今日のMTG確認…"            │ Delivery  discord:#general          │
│ 🌙 03:00 Sleep    success   │ ── Attention ────────────────────    │
│   3 sessions · 1.2k tokens  │ 今日の予定、未解決事項…                 │
│ 🌙 昨 03:00 Sleep skipped   │ ── Output ───────────────────────    │
│   pending 41 ≤ 64           │ 今日のMTG確認して。…                   │
│ ◴ 昨 21:00 evening failed   │              [Chat でこの発言を開く]   │
└─────────────────────────────┴─────────────────────────────────────┘
```

- デスクトップ（≥ 1024px）: Upcoming の下に、左 History・右 run 詳細の2ペイン
- 1023px 以下: History が全幅で表示され、run 選択で詳細が全画面に切り替わる（`←` で戻る）
- 対象 agent は Sidebar の選択 agent に従う。agent 横断の状況は Metrics タブの Agents セクションで扱う
- Sidebar の文脈領域には Upcoming の先頭5件を表示する（[layout.md §2.5](./layout.md#25-context-section)）

---

## 2. Header

- 種別フィルタ（下線タブ）: `All` / `Pulse` / `Sleep`。History と Upcoming の両方に効く
- `Intentions` ビューへの切替
- 手動の再読み込みボタンは置かない。一覧と詳細は可視時ポーリングと WS イベントで更新する（§7.3）

---

## 3. Upcoming

Pulse と Sleep は予測可能性が異なるため、表現を分ける。

### 3.1 Pulse

- 有効な intention の次回発火を時刻順に並べる。各行は時刻・intention id・配送先
- 次回発火は `pulse` ツールの `list` と同じ判定（カレンダー候補 + 実行履歴）をサーバーが返す。WebUI は独自に計算しない
- agent の Pulse が無効の場合は「Pulse disabled」と表示する

### 3.2 Sleep

Sleep は毎日の判定時刻（`sleep_batch.schedule`）に起動するが、実行可否はその時点の状態で決まる（pending messages が閾値以下ならスキップ、agent の turn 実行中なら見送り）。このため「予定」ではなく **次回判定とその時点の条件** を表示する。

| 項目 | 内容 |
|---|---|
| 次回判定 | 次の判定時刻。対象 agent でない（`sleep_batch.agents` 外）・Sleep 無効の場合はその旨を表示 |
| pending messages | 現在の pending 件数と実行閾値。進捗バーで示す |
| 見込み | 現時点の条件による実行 / スキップの見込み。判定時刻までに変わり得ることを明示する |

---

## 4. History

### 4.1 一覧

Pulse run と Sleep run を `started_at` 降順で混在表示する。区切り線ベースの2行リスト（カードは使わない）。

| 種別 | 1行目 | 2行目 |
|---|---|---|
| Pulse | ◴ 時刻・intention id・output kind | 通知本文の先頭 / `PULSE_OK` / エラー / skip 理由 |
| Sleep | 🌙 時刻・trigger（Scheduled / Manual / Backfill）・status | session 数・トークン数 / skip 理由 / エラー |

- status フィルタ（`All` / `Failed` / `Skipped` など）を持つ
- 選択行は左端に accent バー + 薄いハイライト
- 20 件ずつ取得し、`Load more` で追加取得。Pulse / Sleep それぞれの API に `before` カーソルで問い合わせ、クライアントでマージする
- running の run がある間は可視時ポーリングで追従する

### 4.2 Status

| status | dot 色 |
|---|---|
| `success` | success |
| `partial_failure` | warning（Sleep のみ） |
| `failed` | danger |
| `skipped` | muted |
| `running` | accent（点滅） |

Pulse の output kind は `notify`（accent）/ `silent`（muted）のバッジで併記する。

### 4.3 Run 詳細

#### Pulse run

1. **ヘッダー**: status・output kind・intention id。モバイルでは `←` back
2. **メタ**: 開始 → 終了（所要時間）、配送先（`{channel}:{external_chat_id}`）
3. **Attention**: run 開始時点の attention テキスト（`attention_snapshot`）。PULSE.yaml がその後編集されても当時の内容を表示する
4. **Output**: `notify` は本文を Markdown 表示し、`chat_id` / `message_id` から Chat タブの該当発言へジャンプできる。`silent` は `PULSE_OK · no notification was sent.`。`failed` はエラーブロック、`skipped` は理由
5. **Tools**: Pulse 中のツール実行を Chat タブと同じ Tool Card で表示（[chat.md §5](./chat.md#5-tool-実行カード)）

Pulse Capsule の内部（注入されたコンテキスト等）は表示しない。

#### Sleep run

1. **ヘッダー**: status（humanize 表記）+ run ID 短縮形（`#` + 先頭6文字）
2. **メタ**: trigger · session 数、開始 → 終了（所要時間）+ 総トークン数
3. **Steps**: 4 ステップをパイプライン順（event_extraction → episodic_update → semantic_update → prospective_update）で表示

   | step status | icon |
   |---|---|
   | `success` | `✓`（success） |
   | `failed` | `✗`（danger、行背景も danger soft） |
   | `skipped` | `–`（muted） |
   | `running` / `pending` | spinner |

   各行右にステップのトークン合計（0 は非表示）。`error_message` を持つステップはエラー詳細（等幅）を常時表示する。ステップを1つも実行せず `error_message` のみを持つ run は、同じスタイルの Error セクションを表示する
4. **Memory Changes**: file tabs（`episodic` / `semantic` / `prospective`、スナップショットが存在するもののみ）。変更のあるタブには増減行数（`+12 −3`）を表示し、既定では最初に変更のあるファイルを選ぶ。スナップショットのない run では非表示
5. **Skip 判定**: スキップされた判定は理由（`pending 41 ≤ 64` / `agent active`）のみを表示する

### 4.4 DiffViewer

行レベル diff（LCS ベース、自前実装）。文字単位の diff は行わない。

- split / unified の2モード。既定値はビューポート幅に追従（desktop は split、1023px 以下は unified）し、明示的な切替を優先する
- 追加行は success 系背景、削除行は danger 系背景（unified では `+` / `-` prefix）
- 最大高さ 60vh でスクロール。500 行超は先頭 500 行のみ表示し `Show all {n} lines` で展開

Agent タブの Memory History でも同じコンポーネントを使う。

---

## 5. Intentions

PULSE.yaml の intention をカードで表示する。生の YAML は表示しない。

```
┌ morning_review ───────────────────────── ● enabled ┐
│ 毎日 09:00            → discord:#general            │
│ 今日の予定、未解決事項、昨日から持ち越している…            │
│ 次回 明日 09:00        直近  ● ● ● ○ ● ● ✕ ● ● ●      │
└────────────────────────────────────────────────────┘
```

| 要素 | 内容 |
|---|---|
| schedule | 人が読める形（毎日 09:00 / 毎週日曜 21:00 / 一度だけ 11/3 10:00） |
| attention | 本文（長い場合は折りたたみ） |
| delivery | 配送先。`default_delivery` を使う場合はその旨を示す |
| enabled | intention 単位の有効状態 |
| 次回 | §3.1 と同じ判定 |
| 直近 | 直近 10 run の結果ドット。クリックで該当 run の詳細へ |

ビュー上部に agent 単位の Pulse 有効状態と、PULSE.yaml のパースエラー（あれば）を表示する。

### 5.1 編集

intention の追加・変更・削除・有効化 / 無効化は、`pulse` ツールと同じ操作（`add` / `update` / `remove` / `enable` / `disable`）を API として呼び出す。PULSE.yaml を直接書き換える経路は設けない（[pulse.md §3](../pulse.md#3-ファイル責務) の「書き込みはツール経由のみ」に従う）。

---

## 6. URL 構造

| URL | 状態 |
|---|---|
| `/agents/:agentId/schedule` | Upcoming + History（未選択） |
| `/agents/:agentId/schedule?kind=pulse\|sleep` | 種別フィルタ適用 |
| `/agents/:agentId/schedule/pulse/:runId` | Pulse run 選択 |
| `/agents/:agentId/schedule/sleep/:runId` | Sleep run 選択 |
| `/agents/:agentId/schedule/intentions` | Intentions |

agent を切り替えると run 選択は解除される（フィルタ・ビューは維持）。

---

## 7. バックエンド

### 7.1 API

| API | 用途 |
|---|---|
| `GET /api/sleep/runs` | Sleep run 一覧（既存）。`before` カーソルを追加 |
| `GET /api/sleep/runs/:runId` | run + snapshots + steps（既存） |
| `GET /api/agents/:agentId/sleep/next` | 次回判定時刻・対象可否・pending 件数・閾値 |
| `GET /api/pulse/runs` | Pulse run 一覧（`agent_id` / `before` / `limit`） |
| `GET /api/pulse/runs/:runId` | run 詳細（`attention_snapshot`、`tools` を含む） |
| `GET /api/agents/:agentId/pulse` | agent の Pulse 有効状態・intention 一覧・次回発火・パースエラー |
| `POST /api/agents/:agentId/pulse/intentions/...` | intention 操作（§5.1） |

### 7.2 データの拡張

- **Sleep の判定結果の記録**: スケジューラ判定でスキップ（pending 件数が閾値以下）・見送り（agent の turn 実行中）となった場合も `sleep_runs` に `skipped` として理由とともに記録する。これにより History で毎日の判定を監査できる
- **Pulse の attention スナップショット**: `pulse_runs` に run 開始時点の attention を保存する
- **Pulse のツール実行**: Pulse run とツール実行記録を関連付けて run 詳細で返す

### 7.3 リアルタイム更新

WS の `pulse` / `sleep` イベント（`started` / `finished`）でキャッシュを無効化し再取得する。イベント未対応の間は可視時ポーリング（10 秒）で running run を追従する。

---

## 8. Out of Scope

- Pulse の手動発火、Sleep の手動実行
- run の再実行・強制 skip
- 発火頻度・成功率などの集計（Metrics タブで扱う）
- LLM 入力プロンプト・Pulse Capsule 内部の表示
