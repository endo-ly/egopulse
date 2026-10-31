# EgoPulse WebUI — Metrics Tab

ランタイム全体を監視するグローバルタブ。「いま健全か」と「agent ごとにどう使われているか（期間推移）」を一画面で把握し、異常の切り分けから各 agent の Agent / Schedule タブへ降りていく起点になる。

## 1. 構成

```
┌ Metrics ──────────────────────────────── [24h | 7d | 30d]  ↻ 10s ┐
│ HEALTH                                                          │
│ [Active 2] [Uptime 3d] [DB ok] [MCP 2/2] [Channels 3/3]         │
│ ● web running · ● discord running · ○ telegram failed: token…   │
│                                                                 │
│ AGENTS                                                          │
│ agent  turns  tokens  errors  last active   sleep   pulse       │
│ lyre    142   1.2M     3      2m            ●       ●           │
│ musa     38   210k     0      1h            ●       ✕           │
│                                                                 │
│ USAGE    tokens / day（agent 別積み上げ）  ▁▃▅▂▇▆▃                 │
│          by model   requests · input · output · cache%           │
│          by kind    agent_loop / sleep / pulse / compaction      │
│                                                                 │
│ TURNS    channel 別件数 · 所要時間 p50 / p95 · 失敗率                 │
│ ERRORS   error_kind 別集計 + 直近のエラー                            │
└─────────────────────────────────────────────────────────────────┘
```

- Header に期間（`24h` / `7d` / `30d`）、最終取得時刻、手動 Refresh を置く。期間は Health 以外の全セクションに効く
- Sidebar の agent 選択の影響を受けない。agent での絞り込みは Metrics 内のフィルタ（query）で行う
- Sidebar の文脈領域にはセクション一覧を表示する

---

## 2. セクション

### 2.1 Health

現在のランタイム状態。期間指定の影響を受けない。

| 要素 | 内容 |
|---|---|
| Stat Cards | Active turns / Uptime / DB / MCP（接続数 / 全体）/ Channels（running / 全体）/ Bash sandbox |
| Channels | 各チャネルの state（`starting` / `running` / `failed` / `stopped`）と、`last_error`（あれば）または `last_activity` |
| MCP | 接続済みサーバー（transport・tool 数）と失敗サーバー（エラー概要） |

### 2.2 Agents

agent ごとの期間集計の表。行クリックでその agent の Agent タブ、Sleep / Pulse 列のクリックで Schedule タブへ遷移する。

| 列 | 内容 |
|---|---|
| turns | 期間内の turn 数 |
| tokens | 期間内の LLM トークン合計 |
| errors | 期間内の失敗 turn 数 |
| last active | 最終 turn 時刻 |
| sleep / pulse | 最新 run の status |

### 2.3 Usage

LLM 利用量の推移と内訳。

- 日（`24h` は時間）単位のトークン推移を agent 別に積み上げ表示
- モデル別: requests / input / output / cached input の割合
- request kind 別: 通常 turn・Sleep・Pulse・compaction などの内訳

### 2.4 Turns

- channel 別の turn 数
- 所要時間の分布（p50 / p95）
- 失敗率と状態別件数（`failed` / `cancelled` / `uncertain`）

### 2.5 Errors

- `error_kind` 別の件数集計
- 直近のエラー一覧（時刻・error_kind・agent・channel・summary）。trace_id はクリックでコピー

---

## 3. データソース

| セクション | ソース |
|---|---|
| Health | `GET /api/status`（既存）を可視時 10 秒、非可視時 30 秒でポーリング |
| Agents / Usage / Turns / Errors | DB 集計 API（新設）。`llm_usage_logs` と `turn_runs` を `chats.agent_id` で agent に結び付けて集計する |
| 直近のエラー | `GET /telemetry`（既存）の `recent_errors` |

`/telemetry` はプロセス内に直近 100 件を保持するだけで再起動で失われるため、期間推移には使わず「いまこの瞬間」の補助に限定する。

### 3.1 API

| API | 用途 |
|---|---|
| `GET /api/status` | Health（既存） |
| `GET /telemetry` | 直近のエラー（既存） |
| `GET /api/metrics/agents?range=` | Agents |
| `GET /api/metrics/usage?range=&agent_id=` | Usage |
| `GET /api/metrics/turns?range=&agent_id=` | Turns / Errors 集計 |

---

## 4. Out of Scope

- ログストリーミング（`tracing` ログの表示）
- アラート設定
- メトリクスの export
- Health 状態の WS プッシュ
