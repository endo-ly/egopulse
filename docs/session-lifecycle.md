# EgoPulse Session Lifecycle

会話セッションの永続化・復元・圧縮（compaction）のアルゴリズム仕様。

## 目次

1. [Session Identity](#1-session-identity)
2. [保存モデル](#2-保存モデル)
3. [Turn 開始時の復元](#3-turn-開始時の復元)
4. [Turn 中の保存](#4-turn-中の保存)
5. [Compaction](#5-compaction)
6. [Fallback](#6-fallback)
7. [Archive](#7-archive)
8. [Conflict Retry](#8-conflict-retry)
9. [Config](#9-config)

---

## 1. Session Identity

session は `(channel, surface_thread)` から安定的に決まる。

- CLI: `cli:<session_name>`
- Web: `web:<session_key>`
- その他の channel も同じ考え方で surface ごとの thread identity を使う

この surface identity から `chat_id` を解決し、以後の履歴保存・復元は `chat_id` 単位で扱う。

### 1.1 エージェント対応セッションアイデンティティ

`SurfaceContext` は `agent_id`（string）を保持し、各会話サーフェスにエージェントの識別情報を持たせる。

- `session_key()` は `channel:surface_thread` を返す（`agent_id` はキーに含まれない）
- **Discord マルチボット**: `discord_surface_thread(channel_id, bot_id, agent_id)` ヘルパーが `{channel_id}:bot:{bot_id}:agent:{agent_id}` 形式の `surface_thread` を生成する。保存される session key / `external_chat_id` は `discord:{channel_id}:bot:{bot_id}:agent:{agent_id}` になる
- **Web / Telegram / CLI / TUI**: `default_agent` を使用し、従来のアイデンティティ形式を維持する

## 2. 保存モデル

会話永続化は SQLite ベースで、役割は次の 4 つに分かれる。

### `chats`

- 役割: chat の論理 ID と surface との対応付け
- 主な列:
  - `chat_id`
  - `channel`
  - `external_chat_id`
  - `chat_title`
  - `chat_type`
  - `last_message_time`

### `messages`

- 役割: 表示用・一覧用の message レコード
- 主な列:
  - `id`
  - `chat_id`
  - `sender_name`
  - `content`
  - `is_from_bot`
  - `timestamp`

### `sessions`

- 役割: 次ターン再開用の session snapshot
- 主な列:
  - `chat_id`
  - `messages_json`
  - `updated_at`

`messages_json` には LLM 入力に近い `Message` 配列が入る。tool call、tool result、multimodal image ref もここに含まれる。

### `tool_calls`

- 役割: assistant が要求した tool call と output の追跡
- 主な列:
  - `id`
  - `chat_id`
  - `message_id`
  - `tool_name`
  - `tool_input`
  - `tool_output`
  - `timestamp`

## 3. Turn 開始時の復元

turn 開始時は次の順で session を復元する。

1. `sessions.messages_json` があればそれを使う
2. JSON を `Message` 配列へ戻す
3. image は `input_image_ref` から asset store 経由で hydrate する
4. assistant tool call に対応する tool output が欠けている場合は synthetic error tool output を補う
5. snapshot が無い、または壊れている場合だけ `messages` テーブルから recent history を組み立てる

原則:

- 真の次ターン入力は `sessions.messages_json`
- `messages` は fallback 用
- LLM API に再送する履歴では、assistant の tool call と tool output の対応関係を必ず保つ

## 4. Turn 中の保存

turn 中の保存は phase ごとに進む。

1. user message を session 末尾に追加する
2. 必要なら compaction する
3. user-phase snapshot を保存する
4. assistant reply / tool call / tool result を進める
5. 各 phase の結果を通常の persistence に流す

compaction は保存の別系統ではなく、「保存前に session を整形する段」として扱う。

## 5. Compaction

長い会話で古い文脈を要約に畳み、recent なやり取りをそのまま残す機構。

### 目的

1. context window を超えにくくする
2. 古い会話の要点を維持する
3. recent な往復はそのまま残す

### Trigger

最初の LLM 呼び出し前に行う。`messages.len() > max_session_messages` で発火。

### Algorithm

**分割**: message list を `old_messages`（要約対象）と `recent_messages`（保持）に分ける。`recent_messages` の件数は `compact_keep_recent`。

**要約入力**: `old_messages` を `[user]: ...` / `[assistant]: ...` 形式で text 化。画像は `[image]`、tool call は `[tool_use: ...]`、tool result は `[tool_result]: ...` に変換。tool result body は 200 文字でカット。入力が 20,000 文字を超える場合は char-boundary-safe に切り詰め。

**要約呼び出し**: system prompt `You are a helpful summarizer.` + 会話要約要求 + old_messages dump。

**Compact 後の形**:
1. `user`: `[Conversation Summary]\n{summary}`
2. `assistant`: `Understood, I have the conversation context. How can I help?`
3. `recent_messages`

**Role 補正**: 同じ role の plain-text message で `tool_calls` 空かつ `tool_call_id` が `None` の場合のみ merge。末尾が assistant なら除去。

## 6. Fallback

要約は best effort。失敗時は session を壊さないことを優先する。

- **Summarizer Error**: 要約失敗 → `recent_messages` のみ残す
- **Summarizer Timeout**: `compaction_timeout_secs` 超 → `recent_messages` のみ
- **Empty Summary**: 要約結果が空 → `recent_messages` のみ

## 7. Archive

compaction 発火時は、compact 前の全文会話を markdown として archive する。

### 出力先

```text
<data_dir>/groups/<channel>/<chat_id>/conversations/<timestamp>-<unique_suffix>.md
```

### 目的

- compact 前の verbatim context を後から追えるようにする
- デバッグや監査で元の会話を確認できるようにする

### 形式

各 message を次の形式で保存する。

```md
## user

...

---
```

## 8. Conflict Retry

session snapshot 保存には楽観ロックを使う。

### 基本方針

pre-LLM の user-phase save が競合したら、次をやり直す。

1. 最新 snapshot を再ロードする
2. 新しい user message を積み直す
3. compaction 条件を再評価する
4. compacted state を再度 persist する

### 理由

stale snapshot に単純 append だけを再試行すると、compaction 前の shape に戻る可能性があるため。

### 適用範囲

- 最初の user-phase retry
  - compaction-aware
- 以降の assistant / tool phase retry
  - compaction はすでに終わっている前提で通常 persist

## 9. Config

| 設定 | デフォルト | 役割 |
|------|-----------:|------|
| `compaction_timeout_secs` | `180` | 要約 compaction の timeout 秒数 |
| `max_history_messages` | `50` | snapshot が使えない時に `messages` テーブルから復元する件数 |
| `max_session_messages` | `40` | compaction を発火させる message 数の閾値 |
| `compact_keep_recent` | `20` | compact 後にそのまま残す recent message 数 |

### Validation

- `compaction_timeout_secs >= 1`
- `max_history_messages >= 1`
- `max_session_messages >= 1`
- `compact_keep_recent >= 1`
