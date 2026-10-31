# EgoPulse WebUI — Schedule Tab

選択中 agent が自分で動く仕組み、Pulse（決まった時刻に注意を向けて必要なら声をかける）と Sleep（会話を長期記憶へ蒸留する）について、実行の監視と設定の確認を行うタブ。監視（いつ何が起きるか・起きたか）と設定（何をするものか）は見る目的が違うため、別の区画に分ける。設定の区画は、将来 Pulse の attention や Sleep の各ステップのプロンプトを編集する場所になる。

## 1. 構成

Sidebar の文脈領域（[layout.md §2.5](./layout.md#25-context-section)）に、Agent 画面の SECTIONS と同じ形で 3 つの区画を並べる。

| 区画 | 役割 | 内容 |
|---|---|---|
| Activity | 監視 | 問題の要約・Upcoming（これから）・History（これまで）。run を選ぶと右に詳細を開く（§2） |
| Pulse | 設定 | agent 単位の状態と、intention ごとの定義（§3） |
| Sleep | 設定 | 次回チェックと実行の見込み（§4） |

```
┌ Sidebar ──────────────┐┌ Activity ───────────────────────────────┬ Pulse run ─────────────┐
│ SECTIONS              ││ ● Wrap up the day failed on its last …  │ Check the calendar     │
│ ▤ Activity            ││ UPCOMING                                │ Notified · 07:00 · 8s  │
│   1 needs attention   ││  Today 09:00  ⌁ Check the calendar      │ ┌────────────────────┐ │
│ ⌁ Pulse               ││  Tomorrow 03:00 ☾ Sleep  Likely to skip │ │ 通知本文            │ │
│   2 of 3 active       ││ ● NOW ───────────────────────────────   │ └────────────────────┘ │
│ ☾ Sleep               ││ HISTORY               ☐ Show quiet runs │ Sent to Discord · Team │
│   Tomorrow 03:00      ││ Today                                   │ ATTENTION              │
│                       ││  07:00 ⌁ Check the calendar  Notified … │                        │
└───────────────────────┘└─────────────────────────────────────────┴────────────────────────┘
```

| 行 | 2 行目の要約 |
|---|---|
| Activity | 問題があれば `{n} needs attention`、なければ直近の予定（`Next: Today 09:00`）。何もなければ `Nothing scheduled` |
| Pulse | `{有効数} of {総数} active`。agent の Pulse が無効なら `Off`、PULSE.yaml がパースできなければ `PULSE.yaml error` |
| Sleep | 次回チェックの時刻。スケジューラが無効なら `Manual only`、agent が対象外なら `Not scheduled` |

- 区画を移るのは Sidebar で選んだときだけ。Activity の中で run を開いても区画は変わらない
- Pulse と Sleep はそれぞれ固有の色とアイコンを持つ（Pulse: accent-2 / 波形、Sleep: accent / 月）。Sidebar・Timeline の行・run 詳細の見出しで一貫して使う
- 対象 agent は Sidebar の選択 agent に従う。agent 横断の状況は Metrics タブで扱う
- 手動の再読み込みボタンは置かない。表示中のデータは表示時に再取得し、以降は可視時ポーリング（10 秒）で追従する

---

## 2. Activity

Timeline と、選んだ run の詳細を左右に並べる。それぞれ独立してスクロールする。639px 以下では run 詳細を開くと Timeline を隠し、`×` で戻る。

### 2.1 問題の要約

Timeline の上に、直近の run に問題がある intention / Sleep を新しい順に並べる。問題がなければ何も表示しない。

| 対象 | 問題とみなす条件 | 表示 |
|---|---|---|
| Pulse intention | 最新の run が `failed` | `{intention id} failed on its last firing` |
| Pulse intention | 最新の run が `skipped`（配送先の会話が解決できず通知できなかった） | `{intention id} could not deliver its last notification` |
| Sleep | 実際に処理した最新の run（`skipped` を除く）が `failed` / `partial_failure` | `Sleep failed on its last run` / `Sleep partly failed on its last run` |

- 各行は status dot・タイトル・時刻と `View`（その run を右に開く）
- その後に成功した run があれば問題は解消したとみなし、表示しない
- PULSE.yaml がパースできない場合は、その旨（scheduler はその agent の intention を発火しない）とパースエラーを同じ枠に表示する

### 2.2 Upcoming

これから起きることだけを並べる。有効な intention の次回発火と Sleep の次回チェックを時刻順に並べ、発火時刻を過ぎて次のスキャン待ちの intention を先頭にする。行はどれも、それが設定されている区画への入口になる（Pulse 行 → その intention、Sleep 行 → Sleep 区画）。設定を直したいときはここから 1 クリックで届く。

- 各行は時刻（日付ラベルと `09:00`。スキャン待ちは `Due`）・アイコン・タイトルと、2 行目
  - Pulse: `{schedule} → {配送先}`。配送先が解決できなければ `Not deliverable` を warning 色で添える
  - Sleep: 見込み（`Expected to run` / `Likely to skip`）と `41 of 65 new messages`（閾値 64 を超える 65 件目で実行する）
- 次回発火はサーバーが `pulse` ツールの `list` と同じ判定（カレンダー候補 + 実行履歴）で返す。WebUI は独自に計算しない
- agent の Pulse が無効なら `Pulse is off for this agent.` を示す。何もなければ、会話の中で agent に頼めば作られることを示す

Upcoming と History の間に `Now` の区切りを置く。

### 2.3 History

Pulse run と Sleep run を新しい順に 1 本にまとめ、ローカル日付ごとに `Today` / `Yesterday` / `Mon, Nov 3` の見出しで区切る。

| 要素 | 内容 |
|---|---|
| 時刻 | `07:00`（ホバーで日時） |
| タイトル | Pulse は intention の id、Sleep は `Sleep`（trigger が `manual` / `backfill` なら `Manual sleep` / `Backfill sleep`）。attention は行に出さず、Pulse 区画と run 詳細で読む |
| 結果 | Pulse: `Notified` / `Silent` / `Failed` / `Not delivered` / `Running`。Sleep: status。status の色で示す |
| 要約 | Pulse: 通知本文の先頭行（Markdown 記法を除く）/ `Nothing worth reporting` / エラー / skip 理由。Sleep: `Updated episodic, semantic` / `Memory unchanged` / エラー / skip 理由 |

- **Quiet run**: 何も変えず注意も要らない run（報告することがなかった Pulse run と、実行しないと判定した Sleep run）。同じ日に 2 件以上続く quiet run は `{n} quiet runs`（時間帯と内訳 `Silent ×1 · Skipped ×1`）の 1 行にまとめ、選ぶとその場で展開する。`Show quiet runs` で全件を展開する。skip された Pulse run は通知が届かなかったことを意味するので quiet ではない
- 行を選ぶと右に run 詳細を開く（§5）。開いている run の行は active 表示にする
- 最新の 20 件だけを表示時に取得し、以降は可視時ポーリングで追従する。`Load earlier` は各仕組みの最古の run を `{started_at}|{id}` のカーソルにして 20 件ずつ遡り、取得した分を足していく（`id` で決着するので、同じ時刻の run がページ境界で欠けない）。`has_more` が偽になったら履歴の先頭まで到達したので、ボタンを消す（黙って止めない）
- 遡り始めたら一覧はその時点で凍結し、ポーリングも止める。追従を続けると最新ページの範囲がずれ、取得済みのページとの間に抜けができるため。開いている run の詳細は別途追従する
- 一方の仕組みにまだ古い run が残っている間は、その最古の run より前を表示しない（もう一方だけが古い run を出すと、抜けがあるように見えるため）

---

## 3. Pulse

Skills と同じく、intention の一覧と選んだ intention の定義を左右に並べる。

| 要素 | 内容 |
|---|---|
| agent 単位の設定 | Status（`On` / `Off`）・Timezone・Default destination（`default_delivery`。なければ直近の Discord / Telegram 会話） |
| 一覧 | PULSE.yaml の定義順。各行は id と `{schedule} · next {次回発火}`。無効な intention（または agent の Pulse が無効）は薄くして `Off` を添える |
| 定義 | 見出しは intention の id、状態（`Active` / `Off` / `Off (Pulse is off)`）、When（schedule と timezone）、Next（無効なら `Does not fire while off`）、Sends to（§3.1）、Attention（DocumentCard で全文）。タイトルに attention を出さない（同じ文が二度並ぶため） |

- 選択は URL に持つ。指定がない、または PULSE.yaml から消えた intention を指す場合は先頭の intention を選ぶ
- PULSE.yaml がパースできない場合は、agent 単位の設定と一覧の代わりにその旨とパースエラーを表示する
- intention がない場合は、会話の中で agent に頼めば作られることを示す
- 実行の結果はここに出さない（Activity で見る）

### 3.1 配送先の表示

配送先は intention 固有 → `default_delivery` → 直近の Discord / Telegram 会話の順に決まる。サーバーが scheduler と同じ解決を行い、届く会話（`target`）を返す。WebUI はその会話を Sessions と同じ名前（[layout.md §2.6](./layout.md#26-sessions)）で `Discord · Team chat` のように示し、どう決まったか（intention 固有 / agent の既定 / 直近の会話）と `Open in Chat` を添える。解決できない場合は設定された配送先（`Telegram · 987654321`）または `Latest conversation` を示し、発火が skip されることを warning 色で伝える。

---

## 4. Sleep

| 項目 | 内容 |
|---|---|
| Next check | 次にスケジューラが Sleep を試みる時刻。スケジューラが無効なら `Scheduled sleep is off; it runs only when started by hand.`、agent が対象外なら `This agent is not included in scheduled sleep.` |
| Decision | 現時点の見込み（`Expected to run` / `Likely to skip`）、閾値に対する新しいメッセージ数のメーター（閾値を超えると Sleep の色）、`41 new messages so far; it runs when there are more than 64.` |

Sleep は決まった時刻に起動するが、実行するかはその時点の状態で決まる（新しいメッセージが閾値以下ならスキップ、agent が会話中なら見送り）。このため結論を先に示しつつ、「予定」と断定しない言い方にする。

---

## 5. Run detail

見出しは種別（`Pulse run` / `Sleep run`、各色）と閉じるボタン `×`。本文は「結果 → 中身 → 過程」の順に並べる。

### 5.1 Pulse run

| 要素 | 内容 |
|---|---|
| 見出し | intention の id。押すと Pulse 区画のその intention へ移る（`↗` を添える）。メタ行に結果・開始時刻・所要時間（開始時刻の直後に括弧で添える） |
| Outcome | `notify` は通知本文をカードで Markdown 表示。`silent` は報告することがなく送信しなかった旨。`failed` はエラー、`skipped` は通知できなかった旨と理由（warning 色） |
| 配送先 | `Sent to Discord · Team chat` と `Open in Chat`（通知を保存した会話を Chat タブで開く）。通知がなくても `Open intention` を出し、意図の定義へ移れる |
| Attention | run 開始時点の attention。PULSE.yaml がその後編集されても当時の内容を表示する。記録のない run は `Not recorded for this run.` |

Activation 中のツール実行は、通知を保存した会話の Pulse trace として Chat タブで確認する（[chat.md](./chat.md)）。Pulse Capsule の内部（注入されたコンテキスト等）は表示しない。

### 5.2 Sleep run

実行の記録（サマリーとステップ）を上段に左右で並べ、その下に Memory changes を全幅で置く。実行の記録は横に広げず、diff が幅を使う。ペインが狭いときは上段を縦積みにする。

| 要素 | 内容 |
|---|---|
| 見出し | `Sleep` / `Manual sleep` / `Backfill sleep` |
| Run summary | 上段の左。status（`Success` / `Partial failure` / `Failed` / `Skipped` / `Running`）と、Tokens（総トークン数）・Sessions（対象セッション数）・Started（開始時刻）・Took（所要時間）・Run（`#` + 先頭6文字、ホバーで全 ID）。Took は Started の下に置く。値のない項目は出さない。ステップの成否は Pipeline の行が示すので件数は繰り返さない |
| Pipeline | 上段の右。4 ステップ（Event extraction → Episodic update → Semantic update → Prospective update）をパイプライン順に 1 行ずつ並べ、行はアイコン・ステップ名・トークン数（入力 + 出力。0 は非表示）。成功は `✓` と success 色、失敗は danger 色の `✕`、skipped / pending は薄く表示する。全ステップが成功した run でも各ステップを残す。失敗したステップのエラーは全文表示する |
| Memory changes | 公開済みの run（`success` / `partial_failure`）だけに表示する。見出しの右に、変更のあったファイルを増減行数付き（`episodic +4 −0`）のチップで並べ、選んだファイルの diff を表示する。既定は最初のファイル。変更がなければ `Memory was left unchanged.` |

- 実行しないと判定した run は `Did not run: {理由}.` を表示し、Pipeline は出さない
- ステップを1つも実行せず失敗した run は run のエラーを表示する
- `running` の run の snapshot は公開前の候補なので Memory changes に出さない

### 5.3 DiffViewer

行レベル diff（LCS ベース、自前実装）。文字単位の diff は行わない。

- split / unified の2モード。既定値はビューポート幅に追従（desktop は split、1023px 以下は unified）し、明示的な切替を優先する
- 追加行は success 系背景、削除行は danger 系背景（unified では `+` / `-` prefix）
- 最大高さ 60vh でスクロール。500 行超は先頭 500 行のみ表示し `Show all {n} lines` で展開

Agent タブの Memory でも同じコンポーネントを使う。

---

## 6. Intention の編集

intention の追加・変更・削除・有効化 / 無効化は、`pulse` ツールと同じ操作（`add` / `update` / `remove` / `enable` / `disable`）を API として呼び出す。PULSE.yaml を直接書き換える経路は設けない（[pulse.md §3](../pulse.md#3-ファイル責務) の「書き込みはツール経由のみ」に従う）。編集は Pulse 区画の定義（§3）から行う。

---

## 7. URL 構造

| URL | 状態 |
|---|---|
| `/agents/:agentId/schedule` | Activity |
| `/agents/:agentId/schedule/runs/{pulse\|sleep}/:runId` | Activity + run 詳細 |
| `/agents/:agentId/schedule/pulse` | Pulse（先頭の intention を選択） |
| `/agents/:agentId/schedule/pulse/:intentionId` | Pulse + その intention |
| `/agents/:agentId/schedule/sleep` | Sleep |

agent を切り替えると Activity に戻る（run と intention は agent ごとに異なるため）。

---

## 8. バックエンド

### 8.1 API

API の詳細は [api.md §2.9](../api.md#29-sleep-batch) と [§2.13](../api.md#213-pulse)。

| API | 用途 |
|---|---|
| `GET /api/agents/:agentId/pulse` | Pulse 有効状態・intention 一覧・次回発火・配送先の会話（`target`）・直近 run・パースエラー |
| `GET /api/agents/:agentId/sleep/next` | 次回実行の時刻・対象可否・新しいメッセージ数・閾値 |
| `GET /api/pulse/runs` | Pulse run 一覧（`agent_id` / `status` / `limit`） |
| `GET /api/pulse/runs/:runId` | Pulse run 詳細（attention スナップショット・配送先の会話） |
| `GET /api/sleep/runs` | Sleep run 一覧（`agent_id` / `status` / `limit`）。変更のあった記憶ファイル（`changed_memory`）を含む。問題の要約には `status=running,success,partial_failure,failed&limit=1` で実際に処理した最新の run を取る |
| `GET /api/sleep/runs/:runId` | Sleep run + snapshots + steps |
| `POST /api/agents/:agentId/pulse/intentions/...` | intention 操作（§6） |

### 8.2 記録されるデータ

- **Sleep のスキップ**: 新しいメッセージが閾値以下でスキップした回と、agent が会話中で見送った回も、理由付きの `skipped` run として `sleep_runs` に記録する（[sleep.md](../sleep.md)）。History で毎日の結果を監査できる
- **Pulse の attention スナップショット**: `pulse_runs.attention_snapshot` に run 開始時点の attention を保存する（[pulse.md §10](../pulse.md#10-db-仕様)）

---

## 9. Out of Scope

- Pulse の手動発火、Sleep の手動実行
- run の再実行・強制 skip
- 発火頻度・成功率などの集計（Metrics タブで扱う）
- LLM 入力プロンプト・Pulse Capsule 内部の表示
- 月・週のカレンダー表示（intention の大半は毎日・毎週の繰り返しで、グリッドは同じ予定の反復になる。未来の発火を WebUI で展開することにもなり、§2.2 の「サーバー判定に従う」と両立しない）
