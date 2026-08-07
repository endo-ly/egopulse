# EgoPulse Pulse 仕様

Pulse（注意活性化機構）の仕様。時間・外界・記憶からの signal を「いま意識へ上げるべきもの」として選び、必要な agent を短く活性化する。

## 目次

1. [全体思想](#1-全体思想)
2. [最小フロー](#2-最小フロー)
3. [ファイル責務](#3-ファイル責務)
4. [Temporal Intention 仕様](#4-temporal-intention-仕様)
5. [Due Resolver](#5-due-resolver)
6. [Pulse Gate v1](#6-pulse-gate-v1)
7. [Home Surface Resolver](#7-home-surface-resolver)
8. [Pulse Capsule](#8-pulse-capsule)
9. [出力・保存仕様](#9-出力保存仕様)
10. [DB 仕様](#10-db-仕様)
11. [Sleep Scheduler との分離方針](#11-sleep-scheduler-との分離方針)
12. [将来の展望](#12-将来の展望)
13. [関連ドキュメント](#13-関連ドキュメント)

---

## 1. 全体思想

### 1.1 Pulse の定義

**Pulse は、EgoPulse における Attention Activation Layer である。**

Pulse は、単なる cron、通知機能、LLM 定期呼び出しではない。
時間・外界・記憶・将来的な State から発生する signal を受け取り、**いま意識へ上げるべきもの**を選び、必要な agent を短く活性化する仕組みである。

```text
Pulse =
  Signal
  → Attention Gate
  → Activation
  → Output
  → Runtime Record
```

現在の signal source は **Temporal Intention** のみである。

Pulse の本質は次の一文に集約される。

```text
時間条件を持つ intention が due になったとき、
その agent の注意を Pulse Capsule で短く活性化し、
必要がなければ黙り、
必要なときだけ普段の会話場所で声を出す。
```

その声（と実行文脈）だけが通常 session に残る。

### 1.2 Sleep / Pulse / Turn の関係

| 概念 | 役割 | 主な入力 | 主な出力 |
|---|---|---|---|
| 通常 Turn | ユーザー入力への応答 | 会話メッセージ | 応答・ツール実行 |
| Sleep Batch | 経験を長期記憶へ畳む | 会話履歴・記憶ファイル | episodic / semantic / prospective memory |
| Pulse | 注意を活性化する | 時間・記憶・signal | PULSE_OK / 通知 |

```text
通常 Turn:
  人間から明示的に呼ばれて応答する

Sleep:
  経験を沈め、記憶へ畳む

Pulse:
  時間・記憶・外界から、意識へ浮上すべきものを選ぶ
```

### 1.3 Pulse の第一原則

1. **LLM 定期呼び出しにしない**
   due でない場合、重複済みの場合、active turn 中の場合、LLM は呼ばない。

2. **Cron ではなく Temporal Intention として扱う**
   「09:00 に X を実行する」ではなく、「09:00 に X へ注意を向ける」。

3. **Pulse は agent 単位で動く**
   Pulse は channel 単位ではなく agent 単位の機構である。

4. **出力は普段の会話場所に出す**
   Pulse の結果は、その agent が普段会話している surface に出す。

5. **実行は Pulse Capsule で行う**
   Pulse の内部 prompt / contract / capsule は通常 session に混ぜない。

6. **保存は通知経路の activation 会話だけ通常 session に残す**
   `PULSE_OK` や内部 capsule は通常 session に保存しない。ユーザーに見えた通知本文とその実行文脈（synthetic input・tool phase）だけを残す。

7. **State は将来構想として残す**
   Trait は静的傾向、State は将来的な動的内的状態。現行では State に触れない。

---

## 2. 最小フロー

```text
┌──────────────────────────────┐
│         Pulse Scheduler       │
│  tick_interval ごとに起動     │
└──────────────┬───────────────┘
               │
               ▼
┌──────────────────────────────┐
│       Load agents/*/PULSE.md  │
│  front matter + body を読む    │
└──────────────┬───────────────┘
               │
               ▼
┌──────────────────────────────┐
│        Temporal Due Resolver  │
│ daily / weekly / interval 判定     │
└──────────────┬───────────────┘
               │ due
               ▼
┌──────────────────────────────┐
│          Pulse Gate v1        │
│ duplicate / active_turn 判定   │
└──────────────┬───────────────┘
               │ pass
               ▼
┌──────────────────────────────┐
│        Home Surface Resolver  │
│ agent の最後の会話場所を探す    │
└──────────────┬───────────────┘
               │
               ▼
┌──────────────────────────────┐
│        Pulse Capsule Builder  │
│ contract + intention + notes   │
└──────────────┬───────────────┘
               │
               ▼
┌──────────────────────────────┐
│        Pulse Activation       │
│ LLM を短く起こす               │
└──────────────┬───────────────┘
               │
      ┌────────┴─────────┐
      ▼                  ▼
  PULSE_OK            message
  silent              notify to Home Surface
  no session save     save activation conversation
```

---

## 3. ファイル責務

### 3.1 `PULSE.md`

Pulse は agent 配下の `PULSE.md` を使う。

```text
~/.egopulse/
└── agents/
    └── {agent_id}/
        ├── SOUL.md
        ├── AGENTS.md
        ├── PULSE.md
        └── memory/
            ├── episodic.md
            ├── semantic.md
            └── prospective.md
```

`PULSE.md` は二層構造にする。ファイルに内容を書く場合は YAML front matter を必須とし、
front matter なしの Markdown 本文だけの定義はパースエラーとして扱う。

```md
---
version: 1
default_delivery:
  channel: discord
  external_chat_id: "1234567890123456789"
intentions:
  - id: morning_review
    schedule:
      kind: daily
      at: "09:00"
    attention: |
      今日の予定、未解決事項、昨日から持ち越している設計論点を確認する。
    delivery:
      channel: telegram
      external_chat_id: "987654321"
  - id: weekly_reflection
    schedule:
      kind: weekly
      day: sun
      at: "21:00"
    attention: |
      週の振り返り。
---

# PULSE

## Notes

- trivial な変化では通知しない。
- 大きな作業は開始しない。
- 通知する価値がなければ PULSE_OK。
```

| 領域 | 役割 |
|---|---|
| front matter | 非LLMで due 判定するための構造化 Temporal Intention |
| Markdown body | LLM に渡す柔らかい注意メモ |
| binary embedded contract | Pulse の内部契約・安全制約・出力契約 |

#### version 検証

front matter の `version` は Pulse 定義フォーマットの版数である。runtime は `SUPPORTED_PULSE_VERSION`（現行 `1`）との一致を検証し、不一致の定義は `pulse_unsupported_version` エラーで読込を拒否する。未対応版を実行せず、読み取りだけで無視しない。

これは DB schema version（[db.md](./db.md#db_meta)）や Config revision（[config.md §12.6](./config.md#126-3つのバージョン概念の区別)）とは独立した概念である。

`PULSE.md` に内部仕様を詰め込まない。
Pulse の内部契約はバイナリ側に埋め込む。

### 3.2 Config

`egopulse.config.yaml` には runtime 設定だけを置く。

```yaml
pulse:
  enabled: true
  tick_interval: "1m"
```

| 設定 | 役割 |
|---|---|
| `pulse.enabled` | Pulse 全体の有効化（デフォルト `false`） |
| `pulse.tick_interval` | due scan の周期（Duration 形式: `30s`, `1m`, `1h`。デフォルト `1m`） |

due 判定の timezone は `pulse` 配下には設定しない。トップレベルの `timezone`（[config.md](./config.md)）を使用する。

Temporal Intention の中身は config に置かない。
それは agent の注意方針なので、`agents/{agent_id}/PULSE.md` に置く。

---

## 4. Temporal Intention 仕様

対応する schedule は以下の 3 種類。

### 4.1 daily

```yaml
schedule:
  kind: daily
  at: "09:00"
```

### 4.2 weekly

```yaml
schedule:
  kind: weekly
  day: sun
  at: "21:00"
```

### 4.3 interval

`interval_days` 日ごとに発火する。起点は**前回成功した発火日**（`pulse_runs.status = success` の最新 `started_at`）。

```yaml
schedule:
  kind: interval
  interval_days: 3
  at: "09:00"
```

`daily` / `weekly` がカレンダー期間固定の絶対基準であるのに対し、`interval` は**意図の達成（=成功）を起点とする相対基準**である。この違いにより、失敗時の扱いが異なる。

#### 失敗時の挙動（意図駆動リトライ）

前回成功日を起点にするため、失敗しても起点は更新されず、`interval_days` 経過後は**成功するまで毎日再評価される**。ただし `due_key` が評価日単位で進むため、1 日 1 回まで（翌日リトライ）。

| 日付 | 状態 | due | 備考 |
|---|---|---|---|
| 07/03 | 成功 | — | `last_success = 07/03` に更新 |
| 07/04〜05 | skip | ✗ | `07/03 + 3日 = 07/06` 未到 |
| 07/06 | 発火→**失敗** | ✓ | `last_success` は更新されず `07/03` のまま |
| 07/07 | 発火→成功 | ✓ | `due_key` が 07/07 に進むため再試行可能 |
| 07/08〜09 | skip | ✗ | `07/07 + 3日 = 07/10` 未到 |
| 07/10 | 発火 | ✓ | — |

初回（`last_success = None`）は時刻判定のみで due になる。

### 4.4 validation

| 項目 | 仕様 |
|---|---|
| `id` | agent 内で一意 |
| `enabled` | `true` / `false`。省略時 `true`。`false` のときその intention の due 判定・実行をスキップする |
| `schedule.kind` | `daily` / `weekly` / `interval` |
| `daily.at` | `HH:MM` |
| `weekly.day` | `mon`〜`sun` |
| `weekly.at` | `HH:MM` |
| `interval.interval_days` | `1` 以上の整数 |
| `interval.at` | `HH:MM` |
| `attention` | LLM に渡す注意対象。実行命令ではない |
| `default_delivery` | 省略可能。agent レベルのデフォルト配送先 |
| `default_delivery.channel` | `discord` / `telegram` のみ |
| `default_delivery.external_chat_id` | 空不可 |
| `delivery`（intention 内） | 省略可能。`default_delivery` をオーバーライド |
| `delivery.channel` | `discord` / `telegram` のみ |
| `delivery.external_chat_id` | 空不可 |

`attention` は `task` ではない。
「この時間に、この対象へ注意を向ける」という意味を持つ。

---

## 5. Due Resolver

Temporal Intention が due かどうかを非LLMで判定する。

### 5.1 判定ロジック

```text
daily:
  今日の日付 + at <= now
  かつ due_key 未実行

weekly:
  今日の曜日 == day
  かつ 今日の日付 + at <= now
  かつ due_key 未実行

interval:
  今日のローカル日付 >= 前回成功日 + interval_days
  かつ 今日の日付 + at <= now
  かつ due_key 未実行
  ※ 前回成功日 = pulse_runs.status='success' の最新 started_at
  ※ 前回成功日が無い（初回）場合は日付条件をスキップ（時刻判定のみ）
```

### 5.2 due_key

重複実行防止のため、各 intention から `due_key` を作る。形式は `{agent_id}:{intention_id}:{期間キー}`。

| schedule | due_key 例 |
|---|---|
| daily | `lyre:morning_review:2026-05-10` |
| weekly | `kitara:weekly_reflection:2026-W19` |
| interval | `lyre:periodic_report:2026-05-10` |

`interval` の `due_key` は評価日（今日のローカル日付）。前回成功日ではない。これにより、失敗した日は `due_key` が消費され、翌日は別の `due_key` で再評価される（意図駆動リトライ）。

---

## 6. Pulse Gate v1

Gate の役割は「起こすべきでないものを確実に落とす」こと。

```text
Pulse Gate v1 =
  due である
  かつ due_key が未実行
  かつ agent が active turn 中ではない
```

判定結果は以下の 3 つ。

| 判定 | 意味 | 挙動 |
|---|---|---|
| `Allow` | 実行してよい | run 作成 → activation へ進む |
| `Duplicate` | 同一 `due_key` の run が既にある | 何もしない |
| `DeferActive` | agent が active turn 中 | 何もしない。`due_key` は消費しないため次 tick で再評価される |

---

## 7. Home Surface Resolver

### 7.1 目的

Pulse は agent 単位で発火するが、結果はユーザーが普段その agent と会話している場所へ出したい。

そのため、Pulse は Home Surface を解決してから通知する。

**Home Surface** は、その agent が Pulse の結果を出す標準の会話場所である。

```text
Home Surface =
  agent が普段会話している channel / chat / session
```

### 7.2 解決ルール

```text
resolve_home_surface(agent_id, delivery):

1. delivery が明示指定されていれば（intention.delivery → default_delivery）、
   channel:external_chat_id を DB から検索
   （channel が送信可能な discord/telegram の場合のみ）
2. 見つからなければ警告ログを出し、自動解決へフォールバック
3. 自動解決: chats から agent_id に一致する最新 chat を探す
4. channel adapter が存在し、送信可能なら採用
5. どれも見つからなければ skipped（"no sendable home surface"）
```

- 明示指定時の DB ルックアップは `get_chat_by_channel_external_and_agent()` を使用する（ユーザー指定の生の external chat ID を session-key 形式 `{channel}:{external_chat_id}:agent:{agent_id}` に変換して照合）
- 送信可能 channel は Discord / Telegram のみ。Web / CLI は対象外
- Home Surface が解決できない場合は、その run を `skipped` として記録する（LLM は呼ばない）

### 7.3 注意点

Pulse 通知を通常 session に保存すると、その chat の `last_message_time` は更新される。
そのため、一度 Pulse が発話した chat は Home Surface として維持されやすい。

ユーザーが別の場所で同じ agent と会話すれば、その chat が新しい Home Surface になる。

これは自然な挙動である。

---

## 8. Pulse Capsule

Pulse Activation は通常 session の messages をそのまま使わない。
代わりに、Pulse 専用の **Pulse Capsule** を構築する。

### 8.1 構成

```text
Pulse Capsule =
  binary embedded contract
  + due になった Temporal Intention
  + PULSE.md body
  + Home Surface の軽量 recent context
```

LLM 呼び出しは以下の 2 つから構成される。

- **system prompt**: `build_system_prompt()` で構築（SOUL / AGENTS / Memory / Skills）
- **user message**: 以下の Pulse Capsule

```text
# Pulse Activation

agent_id: lyre
intention_id: morning_review
trigger: temporal_due
home_surface:
  channel: discord
  external_chat_id: "1234567890123456789"
now: 2026-05-10T09:00:00+09:00

## Core Contract

{binary embedded Pulse Core Contract}

## Temporal Intention

{front matter の attention}

## Pulse Notes

{PULSE.md body}

## Recent Visible Context

{Home Surface の直近 user-visible messages（最大10件）}
```

| 要素 | 入れる |
|---|---|
| binary embedded Pulse Contract | 入れる |
| due intention | 入れる |
| `PULSE.md` body | あれば入れる |
| Home Surface の直近メッセージ | 最大10件入れる |
| 通常 session 全文 | 入れない |
| tool call 履歴全体 | 入れない |
| 過去 Pulse の全文 | 入れない |

Core Contract は `include_str!` で `pulse_core_contract.md` をバイナリに埋め込む。内容は「注意活性化モードであること」「intention を目的として評価・充足すること」「通知する価値がなければ `PULSE_OK` を返すこと」「出力に Pulse や activation の話を書かないこと」で、作業範囲は bounded scope に限定される。

通常 session の内部 snapshot は使わない。
`Recent Visible Context` は、ユーザーに見えている直近文脈を補助的に渡すだけである。

### 8.2 LLM 呼び出し構成

Pulse Activation の LLM 呼び出しは、通常セッションと**同じ `build_system_prompt()` をそのまま** system prompt として使用する。
Pulse 固有の指示（Core Contract を含む）はすべて user message（Capsule）側に含まれる。

```text
system prompt = build_system_prompt() の出力
  SOUL.md
  + Core Instructions
  + AGENTS.md
  + Long-term Memory (episodic / semantic / prospective)
  + Skills catalog
```

Capsule (user message) には prospective memory を含めない。
理由: system prompt 経由で既に注入されているため、2重注入を避ける。

この構成により:

- 通常セッションと system prompt が完全一致 → prompt cache が最大効率で hit する
- agent の人格・記憶・スキルが Pulse でも一貫して利用可能

### 8.3 Tools

Pulse Activation は通常 turn と同じく built-in tools + MCP tools を使用可能。tool loop を最大 50 イテレーション回し、LLM が完了を出すまで続ける。activation 全体は 30 分のタイムアウトで保護され、タイムアウト・パニック時は run を `failed` として記録する。

activation 中も `active_turn` を保持するため、その間の通常 turn / 他 Pulse は defer される。

---

## 9. 出力・保存仕様

### 9.1 出力種別

Pulse の出力は 2 種類だけ。

| 出力 | 意味 | 動作 | `output_kind` |
|---|---|---|---|
| `PULSE_OK` | 通知不要 | silent として記録。通常 session には何も保存しない | `silent` |
| その他の本文 | 通知あり | Home Surface へ送信し、通常 session に保存する | `notify` |

`PULSE_OK` は case-insensitive で前後空白を trim して判定する（空文字も silent 扱い）。
LLM の出力揺れを許容しつつ、実質的な誤検知は避ける。

### 9.2 通知本文の保存

通知本文が出た場合、Pulse Activation の会話を通常 session に保存する。

```text
1. synthetic user message（intention の構造化文脈）
   [Pulse: {intention_id}]
   Schedule: {schedule}
   Attention:
   {attention}
   - Schedule は `daily 08:00`, `weekly sun 21:00`, `every 3 days 09:00` の形式
   - Attention は PULSE.md の intention 定義そのまま
   - sender は `pulse`、message_kind は `SystemEvent`

2. tool phase（あった場合）
   assistant message + tool messages を順に保存

3. 最終 assistant message
   通知本文を保存
```

これにより、ユーザーは Pulse 通知にそのまま返信できる。

```text
リラ:
昨日の Pulse 設計で、まだ Home Surface の扱いが未確定です。
ここを固めると仕様がかなり安定しそうです。

User:
それもう少し深掘りして
```

この返信は通常 Turn として処理される。
直前の Pulse 通知が通常 session に残っているため、自然に文脈がつながる。

### 9.3 通常 session に残すもの / 残さないもの

| 内容 | 通常 session に保存 | 理由 |
|---|---|---|
| `PULSE_OK` | しない | ユーザーに見えていないため |
| Pulse 内部 contract | しない | 通常会話を汚すため |
| Pulse Capsule 全文 | しない | 内部実行文脈のため |
| due intention の内部 prompt | しない | 通常会話に不要 |
| synthetic input（通知本文の文脈） | する | 返信時に文脈を引き継ぐため |
| tool phase（通知までの実行文脈） | する | 返信時に文脈を引き継ぐため |
| 通知本文 | する | ユーザーに見えた発言だから |
| run metadata | `pulse_runs` に保存 | 監査・重複防止用 |
| due_key | `pulse_runs` に保存 | 再実行防止用 |

この方針により、**ユーザー体験としては通常会話に見えつつ、内部文脈は通常 session を汚さない**。

### 9.4 Output / Execution / Storage の分離

| 領域 | 方針 |
|---|---|
| 実行 | Pulse Capsule で実行する |
| 出力 | 普段の会話場所、つまり Home Surface に出す |
| 保存 | 通知経路の activation 会話だけ、通常 session に保存する |

```text
Pulse は普段の部屋で声を出す。
ただし、考える時は Pulse Capsule で考える。
```

---

## 10. DB 仕様

### 10.1 `pulse_runs` テーブル

```sql
CREATE TABLE pulse_runs (
    id            TEXT PRIMARY KEY,
    agent_id      TEXT NOT NULL,
    intention_id  TEXT NOT NULL,
    due_key       TEXT NOT NULL,

    chat_id       INTEGER,
    message_id    TEXT,

    status        TEXT NOT NULL,
    started_at    TEXT NOT NULL,
    finished_at   TEXT,
    output_kind   TEXT,
    output_text   TEXT,
    error_message TEXT
);

CREATE UNIQUE INDEX idx_pulse_runs_due
    ON pulse_runs(agent_id, intention_id, due_key);

CREATE INDEX idx_pulse_runs_agent_started
    ON pulse_runs(agent_id, started_at);

CREATE INDEX idx_pulse_runs_chat_id
    ON pulse_runs(chat_id);
```

### 10.2 カラム責務

| カラム | 役割 |
|---|---|
| `id` | pulse run ID |
| `agent_id` | 対象 agent |
| `intention_id` | due になった intention |
| `due_key` | 重複実行防止 |
| `chat_id` | 通知先の通常 chat。silent の場合 null |
| `message_id` | 保存した assistant message ID。silent の場合 null |
| `status` | running / success / failed / skipped |
| `output_kind` | silent / notify |
| `output_text` | LLM 出力。通知した本文または PULSE_OK |
| `error_message` | 失敗時の詳細 |

`status = skipped` になるのは、送信可能な Home Surface が解決できなかった場合（LLM は呼ばれない）。

---

## 11. Sleep Scheduler との分離方針

Sleep を Pulse に吸収しない。
Pulse を Sleep Scheduler に寄せすぎない。

Sleep は記憶変換バッチであり、Pulse は注意活性化である。
両者は同じ scheduler 系の仲間だが、同じドメインではない。

### 11.1 実装構成

```text
src/
├── sleep/                   # Sleep scheduler と処理本体
│   ├── scheduler.rs         # Sleep scheduler
│   ├── orchestrator.rs      # Sleep Batch 実行
│   └── ...                  # episodic_renderer / event_rollup / memory_update
└── pulse/                   # Pulse scheduler と処理本体
    ├── scheduler.rs         # Pulse scheduler（tick / due / gate / home surface）
    ├── capsule.rs           # Gate 判定・Capsule 構築・Home Surface 解決
    ├── definition.rs        # PULSE.md パース・due 判定・due_key 生成
    ├── runner.rs            # LLM activation（tool loop）
    ├── output.rs            # 出力処理（silent / notify・session 保存）
    ├── pulse_core_contract.md
    └── mod.rs
```

### 11.2 共通化するもの

| 共通化 | 内容 |
|---|---|
| timezone helper | now / local date / due 判定補助 |
| active_turn defer | agent active 中は defer |
| shutdown 連動 | runtime 停止時に scheduler も止まる |
| retry utility | Turn と Pulse の tool phase で共通利用 |

### 11.3 分けるもの

| 分離 | 理由 |
|---|---|
| Sleep 本体 | memory 書き換え処理だから |
| Pulse 本体 | attention activation だから |
| DB schema | `sleep_runs` と `pulse_runs` は責務が違う |
| prompt / capsule | LLM に求める役割が違う |
| 出力処理 | Sleep は基本 silent、Pulse は Home Surface 出力あり |

---

## 12. 将来の展望

### 12.1 Phase 展開

```text
Phase 1: Temporal Pulse
  PULSE.md front matter の Temporal Intention を due 判定して起動する。
  結果は Home Surface に出し、通知経路の会話を通常 session に保存する。

Phase 2: Pulse Inbox / Notification Router
  すべてをチャットへ流さず、内部 inbox / notification level を導入する。

Phase 3: Signal Ingress
  Webhook / GitHub / Proxmox / SwitchBot / EgoGraph などを PulseSignal として受け取る。

Phase 4: Attention Gate
  salience score / cooldown / duplicate suppression / tiny LLM judge を導入する。

Phase 5: State
  Trait とは別に、agent の runtime state を導入する。

Phase 6: Autonomous Pulse
  内発的 signal から、自ら探索対象・改善候補・創作種を見つける。
```

### 12.2 将来の内部モデル

現行では `TemporalDue` しか使わないが、内部型は拡張を塞がない。

```rust
enum PulseSignalKind {
    TemporalDue,

    // Future:
    ExternalEvent,
    ProspectiveDue,
    MemoryResurfaced,
    StateShift,
    AutonomousCuriosity,
}
```

Pulse pipeline は最初から次の形を維持する。

```text
PulseSignal
  ↓
AttentionGate
  ↓
HomeSurfaceResolver
  ↓
PulseCapsule
  ↓
Activation
  ↓
InlineOutput
  ↓
PulseRecord
```

### 12.3 State との接続

将来の State は、Pulse の signal source になる。

| 概念 | 役割 |
|---|---|
| Trait | 静的な性格傾向 |
| State | その時点の内的状態 |
| Pulse | State や外界から attention を活性化する機構 |

現行では State は未実装。
ただし、`PulseSignalKind::StateShift` のように将来の接続点だけを思想として残す。

### 12.4 Notification の発展

現行は Home Surface への Inline Output のみ。

将来は以下へ拡張する。

```text
PulseOutput
  ↓
Notification Router
  ├─ silent
  ├─ log_only
  ├─ pulse_inbox
  ├─ notify
  ├─ urgent
  └─ approval_required
```

Multi-agent 構成では、sub-agent が直接ユーザーへ通知するのではなく、最終的には Lyre が調律する。

```text
Sub-agent Pulse
  ↓
Pulse Inbox
  ↓
Lyre
  ↓
User
```

---

## 13. 関連ドキュメント

| 項目 | 正本 |
|---|---|
| DB スキーマ（`pulse_runs` テーブル） | [db.md](./db.md) |
| 設定（`pulse` セクション・`timezone`） | [config.md §3.8](./config.md#38-pulse-設定pulse) |
| Sleep Batch（Scheduler 分離の相手） | [sleep.md](./sleep.md) |
| WebUI の Pulse 画面 | [webui/pulse.md](./webui/pulse.md) |
