# EgoPulse Session Lifecycle

`egopulse` の会話 session がどのように解決され、保存され、次ターンで復元され、長い会話で compaction されるかをまとめた仕様。

この文書は runtime core の会話履歴管理だけを対象にする。

## 1. スコープ

### 含むもの

- session identity の決まり方
- SQLite 上の保存モデル
- turn 開始時の復元手順
- turn 中の保存手順
- compaction の発火条件と変換結果
- conflict retry の扱い
- compaction 関連 config

### 含まないもの

- built-in tool の input / output schema
- Web UI / TUI の画面仕様
- Discord / Telegram 固有の UX
- structured memory / long-term memory

## 2. Session Identity

session は `(channel, surface_thread)` から安定的に決まる。

- CLI: `cli:<session_name>`
- Web: `web:<session_key>`
- その他の channel も同じ考え方で surface ごとの thread identity を使う

この surface identity から `chat_id` を解決し、以後の履歴保存・復元は `chat_id` 単位で扱う。

## 3. 保存モデル

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

## 4. Turn 開始時の復元

turn 開始時は次の順で session を復元する。

1. `sessions.messages_json` があればそれを使う
2. JSON を `Message` 配列へ戻す
3. image は `input_image_ref` から asset store 経由で hydrate する
4. snapshot が無い、または壊れている場合だけ `messages` テーブルから recent history を組み立てる

原則:

- 真の次ターン入力は `sessions.messages_json`
- `messages` は fallback 用

## 5. Turn 中の保存

turn 中の保存は phase ごとに進む。

1. user message を session 末尾に追加する
2. 必要なら compaction する
3. user-phase snapshot を保存する
4. assistant reply / tool call / tool result を進める
5. 各 phase の結果を通常の persistence に流す

compaction は保存の別系統ではなく、「保存前に session を整形する段」として扱う。

## 6. Compaction の目的

長い会話で古い文脈を無言で捨てず、古い部分を summary block に畳み、recent なやり取りを verbatim で残すために compaction を行う。

目的は次の 3 つ。

1. context window を超えにくくする
2. 古い会話の要点を維持する
3. recent な往復はそのまま残す

## 7. Compaction Trigger

compaction は最初の LLM 呼び出し前に行う。

流れ:

1. 既存 session をロードする
2. 新しい user message を追加する
3. `messages.len() > max_session_messages` なら compaction を実行する
4. compacted message list を通常の user-phase persist に渡す

判定:

- `messages.len() <= max_session_messages`
  - compaction しない
- `messages.len() > max_session_messages`
  - compaction する

## 8. Compaction Algorithm

### 8.1 分割

message list を 2 つに分ける。

- `old_messages`
- `recent_messages`

`recent_messages` の件数は `compact_keep_recent`。

### 8.2 要約入力の構築

`old_messages` を次の形式で text 化する。

```text
[user]: ...

[assistant]: ...
```

text 化ルールは次の通り。

- plain text はそのまま使う
- image は `[image]` に変換する
- tool call は `[tool_use: ...]` として埋め込む
- tool result は `[tool_result]: ...` として埋め込む
- tool error は `[tool_error]: ...` として埋め込む
- tool result body は 200 文字で切り、末尾に `...` を付ける

要約入力が長すぎる場合は、20,000 文字の上限を char-boundary-safe に適用し、末尾に `\n... (truncated)` を付ける。

### 8.3 要約呼び出し

要約には専用 prompt を使う。

- system:
  - `You are a helpful summarizer.`
- user:
  - 会話要約要求 + `old_messages` の dump

### 8.4 Compact 後の形

要約成功時、message list は次の形へ変わる。

1. `user`: `[Conversation Summary]\n{summary}`
2. `assistant`: `Understood, I have the conversation context. How can I help?`
3. `recent_messages`

### 8.5 Role 補正

recent message を append する際、同じ role の plain-text message どうしで、`tool_calls`
が空かつ `tool_call_id` が `None` の場合のみ merge する。

目的:

- compact 後の role 列を安定させる
- 次ターン resume 時の文脈崩れを避ける

### 8.6 末尾 assistant の除去

compacted 結果の末尾が assistant なら落とす。

目的:

- 次の user message を自然につなげる
- session の末尾を assistant で閉じにくくする

## 9. Fallback

要約は best effort とし、失敗時は session を壊さないことを優先する。

### Summarizer Error

要約呼び出しが失敗したら summary block は作らず、`recent_messages` のみ残す。

### Summarizer Timeout

`compaction_timeout_secs` を超えたら timeout とみなし、`recent_messages` のみ残す。

### Empty Summary

要約結果が空なら異常とみなし、`recent_messages` のみ残す。

fallback は一貫して「summary block なし、recent messages のみ」になる。

## 10. Archive

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

## 11. Conflict Retry

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

## 12. Multimodal / Tool Result

compaction があっても、session snapshot の保存形式そのものは変えない。

### Image

- image data URL を session に直接ベタ保存しない
- asset store に保存し、`input_image_ref` で参照する
- 復元時に再 hydrate する

### Tool Result

- tool result message 自体は session snapshot に含まれる
- compaction の要約入力では text 化される
- render 時は tool result body を 200 文字に収め、error なら `[tool_error]: ...`、それ以外は `[tool_result]: ...` になる
- compact 後に古い tool result の verbatim block は summary に吸収されうる

## 13. Config

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

## 14. 非目標

この仕様は次を扱わない。

- structured memory の抽出
- semantic retrieval
- archive の自動削除
- UI 上での compaction 状態表示
- session branching / fork
