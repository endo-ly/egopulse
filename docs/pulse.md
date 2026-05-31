# EgoPulse Pulse 仕様書 v0.2

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

Phase 1 では、signal source を **Temporal Intention** のみに限定する。

---

## 1.2 Sleep / Pulse / Turn の関係

| 概念          | 役割         | 主な入力         | 主な出力                                     |
| ----------- | ---------- | ------------ | ---------------------------------------- |
| 通常 Turn     | ユーザー入力への応答 | 会話メッセージ      | 応答・ツール実行                                 |
| Sleep Batch | 経験を長期記憶へ畳む | 会話履歴・記憶ファイル  | episodic / semantic / prospective memory |
| Pulse       | 注意を活性化する   | 時間・記憶・signal | PULSE_OK / 通知 / 将来の注意候補                  |

整理すると、こうなる。

```text
通常 Turn:
  人間から明示的に呼ばれて応答する

Sleep:
  経験を沈め、記憶へ畳む

Pulse:
  時間・記憶・外界から、意識へ浮上すべきものを選ぶ
```

---

## 1.3 Pulse の第一原則

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

6. **保存は通知本文だけ通常 session に残す**
   `PULSE_OK` や内部 capsule は通常 session に保存しない。ユーザーに見えた通知本文だけを assistant message として保存する。

7. **State は将来構想として残す**
   Trait は静的傾向、State は将来的な動的内的状態。Phase 1 では State に触れない。

---

# 2. 最小実装の設計方針

## 2.1 Phase 1 の目的

Phase 1 では、Pulse の最終思想を維持しつつ、対象を **Temporal Intention** のみに限定する。

```text
Phase 1:
  事前定義された「何時に何へ注意を向けるか」だけを動かす。
```

処理の意味は cron ではなく、次の流れである。

```text
時刻条件を満たす
  ↓
Temporal Intention が due になる
  ↓
PulseSignal を作る
  ↓
Gate を通過したら LLM を短く起こす
  ↓
PULSE_OK なら黙る
  ↓
必要な場合だけ、普段の会話場所へ通知する
```

---

## 2.2 Home Surface

### 定義

**Home Surface** は、その agent が Pulse の結果を出す標準の会話場所である。

```text
Home Surface =
  agent が普段会話している channel / chat / session
```

Phase 1 では、原則として **agent が最後に会話した surface を Home Surface とみなす**。

```text
agent_id
  ↓
その agent の最新 chat を探す
  ↓
そこを Home Surface として使う
```

### 解決順序

Home Surface 解決順序は以下とする。

```text
1. intention.delivery（明示指定）
2. default_delivery（agent レベルのデフォルト）
3. agent が最後に会話した chat（自動解決）
4. 見つからなければ skipped
```

将来的には、以下を追加できる。

```text
- Pulse Inbox
- Lyre Router
```

---

## 2.3 Output / Execution / Storage の分離

Pulse では、以下を明確に分ける。

| 領域 | 方針                                               |
| -- | ------------------------------------------------ |
| 実行 | Pulse Capsule で実行する                              |
| 出力 | 普段の会話場所、つまり Home Surface に出す                     |
| 保存 | 通知した本文だけ、普段の session に assistant message として保存する |

これは本仕様の重要な決定事項である。

```text
Pulse は普段の部屋で声を出す。
ただし、考える時は Pulse Capsule で考える。
```

---

## 2.4 実行モデル：Capsule Execution

Pulse Activation は通常 session の messages をそのまま使わない。
代わりに、Pulse 専用の **Pulse Capsule** を構築する。

```text
Pulse Capsule =
  binary embedded contract
  + due になった Temporal Intention
  + PULSE.md body
  + Home Surface の軽量 recent context
```

通常 session の全文は使わない。
必要に応じて、最新数件の user-visible message だけを軽量 context として入れる。

### Pulse Capsule に入れるもの

| 要素                             | Phase 1 |
| ------------------------------ | ------: |
| binary embedded Pulse Contract |     入れる |
| due intention                  |     入れる |
| `PULSE.md` body                |  あれば入れる |
| Home Surface の直近メッセージ          | 少量だけ入れる |
| 通常 session 全文                  |    入れない |
| tool call 履歴全体                 |    入れない |
| 過去 Pulse の全文                   |    入れない |

---

## 2.5 出力モデル：Inline Output

Pulse の通知本文は、Home Surface に通常の assistant message として出す。

```text
Pulse Activation
  ↓
PULSE_OK
  → 何も送らない
  → 通常 session に保存しない
  → pulse_runs に silent として記録

通知本文
  → Home Surface へ送信
  → 通常 session に assistant message として保存
  → pulse_runs に chat_id / message_id を記録
```

これにより、ユーザーは Pulse 通知にそのまま返信できる。

```text
リラ:
昨日の Pulse 設計で、まだ Home Surface の扱いが未確定です。
ここを固めると Phase 1 仕様がかなり安定しそうです。

User:
それもう少し深掘りして
```

この返信は通常 Turn として処理される。
直前の Pulse 通知が通常 session に残っているため、自然に文脈がつながる。

---

## 2.6 通常 session に残すもの / 残さないもの

| 内容                       |   通常 session に保存 | 理由            |
| ------------------------ | ---------------: | ------------- |
| `PULSE_OK`               |              しない | ユーザーに見えていないため |
| Pulse 内部 contract        |              しない | 通常会話を汚すため     |
| Pulse Capsule 全文         |              しない | 内部実行文脈のため     |
| due intention の内部 prompt |              しない | 通常会話に不要       |
| 通知本文                     |               する | ユーザーに見えた発言だから |
| run metadata             | `pulse_runs` に保存 | 監査・重複防止用      |
| due_key                  | `pulse_runs` に保存 | 再実行防止用        |

この方針により、**ユーザー体験としては通常会話に見えつつ、内部文脈は通常 session を汚さない**。

---

# 3. ファイル責務

## 3.1 `PULSE.md`

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

`PULSE.md` は二層構造にする。

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

| 領域                       | 役割                                      |
| ------------------------ | --------------------------------------- |
| front matter             | 非LLMで due 判定するための構造化 Temporal Intention |
| Markdown body            | LLM に渡す柔らかい注意メモ                         |
| binary embedded contract | Pulse の内部契約・安全制約・出力契約                   |

`PULSE.md` に内部仕様を詰め込まない。
Pulse の内部契約はバイナリ側に埋め込む。

---

## 3.2 Config

`egopulse.config.yaml` には runtime 設定だけを置く。

```yaml
pulse:
  enabled: true
  tick_interval: "1m"
  timezone: Asia/Tokyo
```

| 設定                         | 役割                               |
| -------------------------- | -------------------------------- |
| `pulse.enabled`            | Pulse 全体の有効化                     |
| `pulse.tick_interval`      | due scan の周期（Duration 形式: `30s`, `1m`, `1h`） |
| `pulse.timezone`           | daily / weekly 判定の timezone      |

Temporal Intention の中身は config に置かない。
それは agent の注意方針なので、`agents/{agent_id}/PULSE.md` に置く。

---

# 4. Temporal Intention 仕様

Phase 1 で対応する schedule は3種類のみ。

## daily

```yaml
schedule:
  kind: daily
  at: "09:00"
```

## weekly

```yaml
schedule:
  kind: weekly
  day: sun
  at: "21:00"
```

## validation

| 項目                          | 仕様                          |
| --------------------------- | --------------------------- |
| `id`                        | agent 内で一意                  |
| `enabled`                   | `true` / `false`。省略時 `true`。`false` のときその intention の due 判定・実行をスキップする |
| `schedule.kind`             | `daily` / `weekly` |
| `daily.at`                  | `HH:MM`                     |
| `weekly.day`                | `mon`〜`sun`                 |
| `weekly.at`                 | `HH:MM`                     |
| `attention`                 | LLM に渡す注意対象。実行命令ではない        |
| `default_delivery`          | 省略可能。agent レベルのデフォルト配送先     |
| `default_delivery.channel`  | `discord` / `telegram` のみ   |
| `default_delivery.external_chat_id` | 空不可                    |
| `delivery`（intention 内）     | 省略可能。`default_delivery` をオーバーライド |
| `delivery.channel`          | `discord` / `telegram` のみ   |
| `delivery.external_chat_id` | 空不可                         |

`attention` は `task` ではない。
「この時間に、この対象へ注意を向ける」という意味を持つ。

---

# 5. 最小フロー

```text
┌──────────────────────────────┐
│        PulseScheduler         │
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
│ daily / weekly 判定     │
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
  no session save     save as assistant message
```

---

# 6. Due Resolver

Temporal Intention が due かどうかを非LLMで判定する。

```text
daily:
  今日の日付 + at <= now
  かつ due_key 未実行

weekly:
  今日の曜日 == day
  かつ 今日の日付 + at <= now
  かつ due_key 未実行
```

## due_key

重複実行防止のため、各 intention から `due_key` を作る。

| schedule | due_key 例                                    |
| -------- | -------------------------------------------- |
| daily    | `lyre:morning_review:2026-05-10`             |
| weekly   | `kitara:weekly_reflection:2026-W19`          |

---

# 7. Pulse Gate v1

Phase 1 の Gate は賢くしない。
役割は「起こすべきでないものを確実に落とす」こと。

```text
Pulse Gate v1 =
  due である
  かつ due_key が未実行
  かつ agent が active turn 中ではない
```

Phase 1 では以下を入れない。

```text
- salience scoring
- tiny LLM judge
- State 判定
- 内発的 signal
- webhook signal
```

---

# 8. Home Surface Resolver

## 8.1 目的

Pulse は agent 単位で発火するが、結果はユーザーが普段その agent と会話している場所へ出したい。

そのため、Pulse は Home Surface を解決してから通知する。

## 8.2 解決ルール

```text
resolve_home_surface(agent_id, delivery):

1. delivery が明示指定されていれば、その channel:external_chat_id を DB から検索
2. 見つからなければ、default_delivery を同様に検索
3. どちらも指定されていなければ、chats から agent_id に一致する最新 chat を探す
4. channel adapter が存在し、送信可能なら採用
5. すべて見つからなければ skipped
```

## 8.3 注意点

Pulse 通知を通常 session に保存すると、その chat の `last_message_time` は更新される。
そのため、一度 Pulse が発話した chat は Home Surface として維持されやすい。

ユーザーが別の場所で同じ agent と会話すれば、その chat が新しい Home Surface になる。

これは自然な挙動である。

---

# 9. Pulse Capsule

Phase 1 の LLM 呼び出しは以下の2つから構成される。

- **system prompt**: `build_system_prompt()` で構築（SOUL / AGENTS / Memory / Skills）+ Core Contract
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

{Home Surface の直近 user-visible messages を少量だけ入れる}
```

通常 session の内部 snapshot は使わない。
`Recent Visible Context` は、ユーザーに見えている直近文脈を補助的に渡すだけである。

---

# 10. 出力仕様

Phase 1 の出力は2種類だけ。

| 出力         | 意味   | 動作                                                       |
| ---------- | ---- | -------------------------------------------------------- |
| `PULSE_OK` | 通知不要 | silent として記録。通常 session には何も保存しない                        |
| その他の本文     | 通知あり | Home Surface へ送信し、通常 session に assistant message として保存する |

`PULSE_OK` は case-insensitive で前後空白を trim して判定する。
LLM の出力揺れを許容しつつ、実質的な誤検知は避ける。

---

# 11. DB 最小仕様

Phase 1 では `pulse_runs` を追加する。

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

## カラム責務

| カラム             | 役割                                        |
| --------------- | ----------------------------------------- |
| `id`            | pulse run ID                              |
| `agent_id`      | 対象 agent                                  |
| `intention_id`  | due になった intention                        |
| `due_key`       | 重複実行防止                                    |
| `chat_id`       | 通知先の通常 chat。silent の場合 null               |
| `message_id`    | 保存した assistant message ID。silent の場合 null |
| `status`        | running / success / failed / skipped      |
| `output_kind`   | silent / notify / failed                  |
| `output_text`   | LLM 出力。通知した本文または PULSE_OK                 |
| `error_message` | 失敗時の詳細                                    |

---

# 12. 通常 session 保存方針

通知本文が出た場合、通常の assistant message として保存する。

```text
sender_name: agent display name
content: 通知本文
is_from_bot: true
chat_id: Home Surface の chat_id
```

同時に `sessions.messages_json` にも反映する。
これにより、ユーザーが Pulse 通知に返信した時、通常 turn が自然に文脈を引き継げる。

ただし、保存するのは最終通知本文のみ。

```text
保存する:
  - ユーザーに見えた通知本文

保存しない:
  - Pulse Capsule
  - Core Contract
  - PULSE.md front matter の内部構造
  - PULSE_OK
```

---

# 13. Sleep Scheduler との分離方針

Sleep を Pulse に吸収しない。
Pulse を Sleep Scheduler に寄せすぎない。

Sleep は記憶変換バッチであり、Pulse は注意活性化である。
両者は同じ scheduler 系の仲間だが、同じドメインではない。

## Phase 1 の構成

```text
src/
├── sleep/scheduler.rs      # Sleep scheduler
├── pulse_scheduler.rs      # 新規
└── scheduler_utils.rs      # 小さな共通関数のみ
```

## 共通化するもの

| 共通化               | 内容                          |
| ----------------- | --------------------------- |
| timezone helper   | now / local date / due 判定補助 |
| active_turn defer | agent active 中は defer       |
| shutdown 連動       | runtime 停止時に scheduler も止まる |
| retry utility     | 必要になったら共通化                  |

## 分けるもの

| 分離               | 理由                                         |
| ---------------- | ------------------------------------------ |
| Sleep 本体         | memory 書き換え処理だから                           |
| Pulse 本体         | attention activation だから                   |
| DB schema        | `sleep_runs` と `pulse_runs` は責務が違う         |
| prompt / capsule | LLM に求める役割が違う                              |
| 出力処理             | Sleep は基本 silent、Pulse は Home Surface 出力あり |

---

# 14. Phase 1 のスコープ

## やる

| 項目                             | 内容                                                    |
| ------------------------------ | ----------------------------------------------------- |
| `pulse` config                 | enabled / tick_interval / timezone |
| `PULSE.md` front matter parser | Temporal Intention を読む                                |
| due 判定                         | daily / weekly                                 |
| duplicate 判定                   | `pulse_runs` の `due_key`                              |
| active turn defer              | agent active 中は起こさない                                  |
| Home Surface 解決                | agent の最後の会話場所を使う                                     |
| Pulse Capsule                  | LLM 入力を専用構築                                           |
| `PULSE_OK` 抑制                  | silent として記録                                          |
| Inline Output                  | Home Surface へ通知                                      |
| session 保存                     | 通知本文だけ assistant message として保存                        |
| `pulse_runs`                   | 監査・重複防止・message 紐づけ                                   |

## やらない

| 項目                    | 理由                            |
| --------------------- | ----------------------------- |
| State                 | 将来の内的状態。Phase 1 では触れない        |
| 内発的 signal            | State と一緒に後続                  |
| Webhook ingress       | Phase 2 以降                    |
| Salience score        | Phase 1 では due / duplicate のみ |
| tiny LLM judge        | Gate 強化フェーズ                   |
| Pulse Inbox           | 通知が増えてから                      |
| Lyre Router           | multi-agent 通知設計の後            |
| 自律探索                  | 最終段階                          |
| 複雑な delivery override | —                             |

## Phase 1 Implementation Decisions

実装判断による元仕様との差分。実装者が迷わないよう明示する。

### Home Surface の解決順序
- `intention.delivery` → `default_delivery` → 自動解決（最新 chat）→ skipped
- 明示指定時は `get_chat_by_channel_external()` で DB ルックアップ
- channel adapter が存在するか `state.channels` で確認
- 指定先が見つからない場合はその intention のみ skipped + 警告ログ

### Home Surface の送信可能 chat 探索
- `resolve_home_surface()` は Discord/Telegram チャネルのみ対応
- 自動解決時は agent_id に紐づく最新の chat を `get_agent_chats_by_recent()` で取得
- channel adapter が存在するか `state.channels` で確認
- Web/CLI は対象外

### Tools 使用可能
- Pulse Activation は通常 turn と同じく built-in tools + MCP tools を使用可能
- `run_activation()` は tool loop を回す（max_turns 制限あり）
- 破壊的操作は Pulse Core Contract で禁止

### 通知本文ありの通常会話同等保存
- 通知テキストは構造化 synthetic user message として通常 session に保存
- synthetic message のフォーマット:
  ```
  [Pulse: {intention_id}]
  Schedule: {schedule}
  Attention:
  {attention}
  ```
  - `Schedule` は `daily 08:00`, `weekly sun 21:00` の形式
  - `Attention` は PULSE.md の intention 定義そのまま
- sender_name は `Pulse`、message_kind は `SystemEvent`
- ユーザーが Pulse 通知に返信すると、通常 turn が文脈（intention の目的・スケジュール・指示内容）を引き継げる

### `PULSE_OK` 非保存
- `PULSE_OK` 判定時は通常 session に何も保存しない
- `pulse_runs` にのみ `output_kind = "silent"` として記録

### Gate v1 の実装詳細
- `DeferActive` は due_key を消費しない（次 tick で再評価可能）
- `Duplicate` は due_key が既存 run と重複している場合
- `Allow` のみ run 作成→activation へ進む

### Capsule 構成
- Core Contract: `include_str!` で `pulse_core_contract.md` を埋め込み
- 入力: intention, body, recent messages (直近5件)
- Home Surface 情報をメタデータとして含む

### LLM 呼び出しの System Prompt 構成

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

---

# 15. 将来の展望

## 15.1 Phase 展開

```text
Phase 1: Temporal Pulse
  PULSE.md front matter の Temporal Intention を due 判定して起動する。
  結果は Home Surface に出し、通知本文だけ通常 session に保存する。

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

---

## 15.2 将来の内部モデル

Phase 1 では `TemporalDue` しか使わないが、内部型は拡張を塞がない。

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

---

## 15.3 State との接続

将来の State は、Pulse の signal source になる。

| 概念    | 役割                             |
| ----- | ------------------------------ |
| Trait | 静的な性格傾向                        |
| State | その時点の内的状態                      |
| Pulse | State や外界から attention を活性化する機構 |

Phase 1 では State は未実装。
ただし、`PulseSignalKind::StateShift` のように将来の接続点だけを思想として残す。

---

## 15.4 Notification の発展

Phase 1 は Home Surface への Inline Output のみ。

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

# 16. 最終固定事項

| 項目            | 決定                             |
| ------------- | ------------------------------ |
| 機能名           | `Pulse`                        |
| 本質            | Attention Activation Layer     |
| Phase 1 対象    | Temporal Intention のみ          |
| 構造化定義         | `PULSE.md` front matter        |
| ユーザー自由記述      | `PULSE.md` body                |
| 内部契約          | バイナリ埋め込み                       |
| Config        | runtime 設定のみ                   |
| 出力先           | delivery → default_delivery → 最新 Home Surface → skipped |
| 実行文脈          | Pulse Capsule                  |
| 通常 session 保存 | 通知本文だけ assistant message として保存 |
| `PULSE_OK`    | 通知せず、通常 session にも保存しない        |
| Sleep との関係    | 責務分離。共通 utility のみ共有           |
| Gate v1       | due / duplicate / active_turn  |
| LLM 呼び出し      | Gate 通過時のみ                     |
| 記録            | `pulse_runs`                   |
| State         | 将来構想。Phase 1 では触れない            |

---

# 17. 仕様の核

Phase 1 の Pulse は、次のための最小実装である。

```text
時間条件を持つ intention が due になったとき、
その agent の注意を Pulse Capsule で短く活性化し、
必要がなければ黙り、
必要なときだけ普段の会話場所で声を出す。
```

そして、その声だけが通常 session に残る。

```text
実行:
  Pulse Capsule

出力:
  Home Surface

保存:
  通知本文だけ通常 session
```
