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
│ tick_interval ごと / once 時刻 │
└──────────────┬───────────────┘
               │
               ▼
┌──────────────────────────────┐
│      Load agents/*/PULSE.yaml │
│   document 全体を読む          │
│  （enabled: false は skip）    │
└──────────────┬───────────────┘
               │
               ▼
┌──────────────────────────────┐
│        Temporal Due Resolver  │
│ daily / weekly / interval /   │
│ once 判定                     │
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
│  contract + intention + ctx    │
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

### 3.1 `PULSE.yaml`

Pulse は agent 配下の `PULSE.yaml` を使う。

```text
~/.egopulse/
└── agents/
    └── {agent_id}/
        ├── SOUL.md
        ├── AGENTS.md
        ├── PULSE.yaml
        └── memory/
            ├── episodic.md
            ├── semantic.md
            └── prospective.md
```

ファイルは純粋な YAML ドキュメントである（front matter は使わない）。

```yaml
enabled: true
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
```

| 領域 | 役割 |
|---|---|
| `enabled` | agent 単位の Pulse 有効化スイッチ（省略時 `true`）。`false` のとき scheduler はその agent を skip し、`pulse` ツールは `list` / `enable` のみ応答する |
| `default_delivery` / `intentions` | 非LLMで due 判定するための構造化 Temporal Intention |
| binary embedded contract | Pulse の内部契約・安全制約・出力契約 |

Pulse 定義に version フィールドは存在しない。形式の正本はこのドキュメントと runtime の実装であり、runtime が書き込むファイルは常に自力でパースできることが保証される（[§3.3](#33-agent-による-pulse-操作pulse-ツール)）。

`PULSE.yaml` に内部仕様を詰め込まない。
Pulse の内部契約はバイナリ側に埋め込む。

### 3.2 Config

`egopulse.config.yaml` には runtime 設定だけを置く。Pulse の有効 / 無効は agent 単位（`PULSE.yaml` の `enabled`）でのみ制御され、グローバルスイッチは存在しない。Pulse は常設の runtime 機能であり、scheduler の tick ループは常に回る。

```yaml
pulse:
  tick_interval: "1m"
```

| 設定 | 役割 |
|---|---|
| `pulse.tick_interval` | due scan の基本周期（Duration 形式: `30s`, `1m`, `1h`。デフォルト `1m`）。`once` の次回発火がこの周期内にある場合、scheduler はその時刻に起床する（発火は tick に依存しない） |

due 判定の timezone は `pulse` 配下には設定しない。トップレベルの `timezone`（[config.md](./config.md)）を使用する。

Temporal Intention の中身は config に置かない。
それは agent の注意方針なので、`agents/{agent_id}/PULSE.yaml` に置く。

### 3.3 Agent による Pulse 操作（`pulse` ツール）

Pulse の設定は built-in ツール `pulse` を通じてのみ行う。エージェントは会話の中で「毎日 9時にこれを確認して」「明日の 10時にリマインドして」と言われると、このツールで intention を設定する。ファイルへの書き込みはすべてツール経由とし、エージェントが PULSE.yaml を直接 write/edit することはない。

**実装**: [`src/tools/pulse.rs`](../src/tools/pulse.rs)。ツール description 自体がエージェント向けの Pulse 知識の正本である（system prompt や skill への Pulse 専門セクションは存在しない）。

| op | 引数 | 動作 |
|---|---|---|
| `list` | — | 現在の設定を表示。各 intention の schedule・enabled・次回発火見積もり、agent の enabled 状態。パースエラー（手編集による破損）もここで報告される |
| `add` | `schedule` + `attention`、`id` / `delivery` は省略可 | intention を作成。`id` 省略時は schedule から自動採番（重複時は接尾辞）。`delivery` 省略時は**現在の会話**が使われる（チャネルが Discord/Telegram の場合）。`once` は未来日のみ可 |
| `update` | `id` + `schedule` / `attention` / `delivery` | intention を変更 |
| `remove` | `id` | intention を削除 |
| `enable` / `disable` | `id` 省略可 | `id` あり: その intention のON/OFF。`id` なし: agent レベルの `enabled` 切替 |

挙動の固定ルール:

- **バリデーションエラーはツールエラー文字列として返る**（`HH:MM` 不正、id 重複、`interval_days < 1`、曜日不正、非対応チャネル等）。エージェントは会話の中で自己修正できる
- **Secret 会話からは利用できない**: Pulse の実行・配送は Normal スコープで行われるため、Secret 会話からの呼び出しはエラーになる（秘匿内容の意図しない漏洩を防ぐ）。Pulse の設定は通常の会話から行う
- **書き込みは serialize → parse round-trip → atomic rename**。ツールが書いたファイルがパースできない状態は原理的に発生しない
- **並行書き込みは直列化される**: load → 変更 → save をプロセス共通の排他ゲート内で行うため、複数 turn や activation 後のアーカイブが同時に走っても更新消失・破損はしない
- **`add` の delivery デフォルトは現在の会話**。TUI / Web など送信可能チャネル以外で頼まれた場合は intention を作らず、Discord / Telegram の配送先を問い返す
- **同日の再発火はしない**: daily の `due_key` は日付単位のため、09:00 発火済みの日に 10:00 に変更しても当日は再発火しない。再発火させたい場合は id を変える
- **発火見込みの表示は scheduler と同じ判定**: `list` / `add` の次回発火テキストは、カレンダー候補（`estimate_next_due`）に実行履歴（`pulse_runs` / due_key）を加えた scheduler と同一の判定で作る。時刻到達かつ未実行は `due now (pending next scan)`、候補が実行済み期間に属する場合は `already attempted for current period`、`once` 実行済みは `already attempted (no automatic retry)` と表示する。無効な agent・intention には発火見込みを表示しない。実行履歴の参照に失敗した場合は該当行に `error` を表示し、`list` 全体をツールエラーとして返す
- **`once` は成功時に自動アーカイブされる**: run 成功後、実行時点の内容が現在の PULSE.yaml と一致する場合にのみ PULSE.yaml から除去される。activation 中にユーザーが再スケジュールした場合はそちらを優先して残す。失敗・skipped の場合はファイルに残り、`list` で可視化されるため、エージェントが原因を説明して `remove` → `add` で再設定する。自動リトライはしない
- **v1 は自 agent のみ操作可能**（他 agent の PULSE.yaml は操作しない）。将来的な委譲拡張の余地は `agent_id` 引数の追加として残す
- 編集は次の tick で反映される。再起動は不要。保存が成功すると scheduler へ起床信号が送られるため、`once` を追加した場合も次の tick を待たずに指定時刻へ起床する
- Pulse Activation 中の自己編集も許可される（activation が active turn を保持するため、通常 turn との書き込み競合は発生しない）

---

## 4. Temporal Intention 仕様

対応する schedule は以下の 4 種類。

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

### 4.4 once

指定した日付の `at` に 1 回だけ発火する単発リマインダ。

```yaml
schedule:
  kind: once
  date: "2026-05-12"
  at: "18:00"
```

- `add` 時に `date + at` が過去のものは拒否される（ツール経由の場合）。タイムゾーンの DST 移行で存在しないローカル時刻も同様に拒否される
- 発火は対象日の `at` **ちょうど**（秒精度）に行われる。`once` は予約型リマインダであり、時刻そのものが意味を持つため、scheduler は `tick_interval` を待たずに次回 due 時刻へ起床する（[§3.2](#32-config)）
- **成功した run の後に intention は自動アーカイブされる**（PULSE.yaml から除去される）。単発リマインドが設定の残余を残さないため。activation 中に内容が変更されていた場合はアーカイブせず変更後の定義を残す
- 失敗・skipped の場合は intention はファイルに残る。`due_key` は対象日を消耗するため自動リトライはせず、エージェントが `list` で検知して会話的に再設定する

### 4.5 validation

| 項目 | 仕様 |
|---|---|
| `enabled`（document） | `true` / `false`。省略時 `true`。`false` のとき scheduler はその agent を skip する |
| `id` | agent 内で一意 |
| `enabled`（intention） | `true` / `false`。省略時 `true`。`false` のときその intention の due 判定・実行をスキップする |
| `schedule.kind` | `daily` / `weekly` / `interval` / `once` |
| `daily.at` | `HH:MM` |
| `weekly.day` | `mon`〜`sun` |
| `weekly.at` | `HH:MM` |
| `interval.interval_days` | `1` 以上の整数 |
| `interval.at` | `HH:MM` |
| `once.date` | `YYYY-MM-DD` |
| `once.at` | `HH:MM` |
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

once:
  今日のローカル日付 == date
  かつ 今日の日付 + at <= now
  かつ due_key 未実行
```

### 5.2 due_key

重複実行防止のため、各 intention から `due_key` を作る。形式は `{agent_id}:{intention_id}:{期間キー}`。

| schedule | due_key 例 |
|---|---|
| daily | `lyre:morning_review:2026-05-10` |
| weekly | `kitara:weekly_reflection:2026-W19` |
| interval | `lyre:periodic_report:2026-05-10` |
| once | `lyre:dentist_reminder:2026-05-12`（設定日固定） |

`interval` の `due_key` は評価日（今日のローカル日付）。前回成功日ではない。これにより、失敗した日は `due_key` が消費され、翌日は別の `due_key` で再評価される（意図駆動リトライ）。

`once` の `due_key` は設定日固定。最初の run（status によらず）が消費するため、失敗時に同一日の再発火はしない。再設定は `remove` → `add` で行う（[§3.3](#33-agent-による-pulse-操作pulse-ツール)）。

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
   - 見つからなければ自動解決へフォールバックしない。
     その run は skipped（"explicit delivery could not be resolved"）。
     ユーザーが指定した配送先を暗黙に変更しないため
2. delivery が省略された場合のみ自動解決:
   chats から agent_id に一致する最新 chat を探す
3. channel adapter が存在し、送信可能なら採用
4. どれも見つからなければ skipped（"no sendable home surface"）
```

- 明示指定時の DB ルックアップは `get_chat_by_channel_external_and_agent()` を使用する（ユーザー指定の生の external chat ID を session-key 形式 `{channel}:{external_chat_id}:agent:{agent_id}` に変換して照合）
- 送信可能 channel は Discord / Telegram のみ。Web / CLI は対象外
- Activation の `SurfaceContext` / Tool 実行コンテキストは、Home Surface の `external_chat_id`（session key 形式）から生の `surface_thread` を復元して構築する。これにより Activation 中の `agent_send` は宛先エージェントの通常 session key（`{channel}:{surface_thread}:agent:{to}`）を解決する
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

{intention の attention}

## Recent Visible Context

{Home Surface の直近 user-visible messages（最大10件）}
```

| 要素 | 入れる |
|---|---|
| binary embedded Pulse Contract | 入れる |
| due intention | 入れる |
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

各 LLM 呼び出しは、通常 Turn と同じリトライ判定（`ModelRunner` の共通リトライループ）で一過性エラー（送信失敗・429・5xx）を最大 3 回まで試行する。Pulse は全エージェントを逐次スキャンするため、待機時間（サーバーの `Retry-After` を含む）が 60 秒を超えるリトライは行わず、その Activation を即座に `failed` とする。これによりプロバイダのクォータ枯渇時に後続の Pulse が止まらない。

Pulse の生成途中の delta は破棄し、外部公開済み出力として扱わない。途中でストリームが中断しても同じ LLM 呼び出しを再試行でき、最終応答が空の場合も共通の runtime guard による回復を行う。通常 Turn は delta を外部公開した時点で再試行を抑止する。いずれも実行済み Tool や Activation 全体を再実行しない。

### 8.3 Tools

Home Surface が Multi-Agent Room の場合、共有 Channel Log を解決または作成し、その ID を Tool 実行と `agent_send` の宛先 Turn へ引き継ぐ。`agent_send` の公開メッセージは共有 Log、宛先の直接入力と内部イベントは宛先 Agent Session に保存する。Single-Agent Channel では共有 Log を作成せず、Home の Agent Session を保存先とする。

Pulse Activation は通常 turn と同じく built-in tools + MCP tools を使用可能。tool loop を最大 50 イテレーション回し、48回目に終了警告を付け、49〜50回目は最終回答を優先する指示を追加する。activation 全体は 30 分のタイムアウトで保護され、タイムアウト・パニック時は run を `failed` として記録する。

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
   - Attention は PULSE.yaml の intention 定義そのまま
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
    error_message TEXT,
    attention_snapshot TEXT
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
| `error_message` | 失敗時の詳細、または skip 理由 |
| `attention_snapshot` | run 開始時点の attention。PULSE.yaml がその後変わっても、その run が何に注意を向けたかを示す |

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
├── tools/pulse.rs           # `pulse` ツール（agent による PULSE.yaml 操作・§3.3）
└── pulse/                   # Pulse scheduler と処理本体
    ├── scheduler.rs         # Pulse scheduler（tick / due / gate / home surface / once archive）
    ├── capsule.rs           # Gate 判定・Capsule 構築・Home Surface 解決
    ├── definition.rs        # PULSE.yaml パース・検証・保存・due 判定・due_key 生成
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
| retry utility | Turn と Pulse の LLM 呼び出しで共通のリトライループを利用（Pulse は待機時間に上限あり） |

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
  PULSE.yaml の Temporal Intention を due 判定して起動する。
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
| WebUI の Schedule 画面 | [webui/schedule.md](./webui/schedule.md) |
