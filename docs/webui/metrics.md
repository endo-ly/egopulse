# EgoPulse WebUI — Metrics Tab

ランタイム全体を監視するグローバルタブ。次の 3 つの問いに一画面で答え、原因の調査は各 agent の Chat / Agent / Schedule へ降りて行う。

| 問い | セクション |
|---|---|
| いま壊れているものはあるか | Health |
| トークンを何に使っているか（agent・用途・モデル・Pulse intention） | Usage |
| 何が失敗しているか、どこを見ればよいか | Reliability |

agent ごとの比較は Agents セクションの表で行う。

## 1. 構成

```
Metrics                                         [All agents ▾]  [24h | 7d | 30d]
HEALTH                                                                     Live
 ● 2 issues need attention          Version 2026.10.31  Uptime 3d 4h  Running turns 2
   telegram channel failed          Unauthorized: bot token was revoked
   Channels           MCP servers            Runtime
USAGE                                                         The last 7 days
 [Tokens 142M ▲+14%] [LLM calls 2,207] [Cache hit 46%] [Per call 61k]
 Tokens per day                                            [By agent | By use]
 積み上げ棒グラフ + 凡例
 By use（用途別の割合）              By model（calls / tokens / cache hit）
 Pulse intentions（runs / per run / tokens / share of Pulse）
RELIABILITY
 [Turns] [Turn failures] [Turn duration] [Tool failures] [Pulse and Sleep]
 Turn failures by kind              Tool failures by kind
 Tools with failures
 Recent failures（行から Chat / Schedule の run へ移動）
AGENTS
 agent / turns / tokens（割合バー）/ failures / Pulse / last sleep / last active
```

- 期間（`24h` / `7d` / `30d`、既定 `7d`）と agent フィルタは Usage と Reliability に効く。Health は現在値、Agents は比較のため常に全 agent を表示し、フィルタ中の agent を強調する
- 期間と agent は URL の query（`/metrics?range=30d&agent=lyre`）で保持する。既定値は省略する
- Sidebar の文脈領域には 4 セクションを並べ、各行の 2 行目に要約（`All normal` / `2 issues`、`142M tokens in 7d`、`3 failures`、`5 agents`）を出す。クリックでそのセクションへスクロールする

## 2. セクション

### 2.1 Health

`GET /api/status` を可視時 10 秒間隔でポーリングする。

- 判定行：問題がなければ `All systems normal`、あれば件数と一覧。問題の定義は下表
- 判定行の右に Version / Uptime / Running turns
- 下段に Channels（state と、`last_error` または最終アクティビティ）、MCP servers（失敗サーバーを先頭に、接続済みは tool 数）、Runtime（Database / Bash sandbox / Input）

| 問題 | 重さ |
|---|---|
| critical task failure / DB 不良 / チャネル `failed` / MCP 接続失敗 | danger |
| shutdown 中 / 入力受付停止 / bash が隔離なしで実行 | warning |

### 2.2 Usage

| 要素 | 内容 |
|---|---|
| KPI | Tokens（前期間比）/ LLM calls / Cache hit（cached tokens を報告した呼び出しの入力に対する割合。報告がなければ `—`）/ Per call（呼び出しあたりの平均入力） |
| 推移 | `24h` は UTC の時単位、それ以外は `Config.timezone` のローカル日単位の積み上げ棒。ラベルは `Config.timezone` で表示する。DST のフォールバックで同じローカル時刻が2回現れても別バケットになる。`By agent` と `By use` を切り替える。hover で内訳を表示 |
| By use | Conversation（`agent_loop`）/ Pulse / Sleep / Compaction の割合 |
| By model | provider・model ごとの calls / tokens / cache hit |
| Pulse intentions | intention ごとの runs / run あたりトークン / 合計 / Pulse 全体に占める割合。どの run にも入らない Pulse 利用は `Unattributed` |

色は agent が Sidebar の並び順、用途が固定順に割り当てる（フィルタで系列が減っても色は変わらない）。9 番目以降の agent は `Other` 色になる。

### 2.3 Reliability

| 要素 | 内容 |
|---|---|
| KPI | Turns（前期間比）/ Turn failures（`failed` + `uncertain` の割合）/ Turn duration（完了 turn の中央値と 95 パーセンタイル）/ Tool failures / Pulse and Sleep の失敗数 |
| Failure kinds | turn の `error_kind` 別、tool の `error_kind` 別の件数 |
| Tools with failures | 失敗のある tool を失敗数の多い順に最大 10 件 |
| Recent failures | 失敗 turn・失敗 Pulse run・失敗 / 部分失敗 Sleep run を新しい順に最大 20 件。turn は Chat のセッション、run は Schedule の run 詳細を開く |

### 2.4 Agents

設定済みの全 agent の表。行クリックでその agent の Agent 画面を開く。

| 列 | 内容 |
|---|---|
| Turns / Tokens / Failures | 期間内の件数。Tokens は全 agent に占める割合のバーを添える |
| Pulse | 期間内の run 数と失敗数 |
| Last sleep | 最新の（`skipped` を除く）Sleep run の status |
| Last active | 最終 turn の時刻（期間に依存しない） |

---

## 3. データソース

| セクション | API | ポーリング |
|---|---|---|
| Health | `GET /api/status` | 10 秒 |
| Usage / Reliability / Agents | `GET /api/metrics?range=&agent=` | 60 秒 |

集計は既存テーブルを読むだけで行う（[api.md §2.14](../api.md#214-metrics)）。

| 値 | ソース |
|---|---|
| LLM 利用量 | `llm_usage_logs`（`chats.agent_id` で agent に帰属） |
| Sleep のトークン | `sleep_runs`（Sleep は `llm_usage_logs` に記録しない） |
| Pulse intention 別トークン | `llm_usage_logs(request_kind = 'pulse')` を、同じ agent の `pulse_runs` のうち `[started_at, finished_at]` に入る run へ帰属。Pulse は agent を逐次スキャンするため run の時間窓は重ならない |
| Turn | `turn_runs` |
| Tool | `tool_calls`（`succeeded` / `failed` / `uncertain`） |
| 失敗一覧 | `turn_runs` / `pulse_runs` / `sleep_runs` |

Secret スコープ（`secret.db`）は集計しない。WebUI は Secret 会話を表出しない。

---

## 4. Out of Scope

- ログストリーミング（`tracing` ログの表示）
- アラート設定
- メトリクスの export
- Health 状態の WS プッシュ
