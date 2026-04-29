# OpenAI Codex Provider

`openai-codex` provider は OpenAI 公式 `/v1/responses` ではなく、
`https://chatgpt.com/backend-api/codex/responses` を OAuth token で呼び出す。
Responses API に似ているが完全互換ではないため、通常の OpenAI provider と同じ扱いにしない。

## 注意点と対策

| 注意点 | 今の実装での対策 |
|---|---|
| Codex backend は streaming 必須 | Codex provider では常に `stream: true` を付ける |
| Codex backend は保存前提の Responses API ではない | Codex provider では常に `store: false` を付ける |
| 公式 `/v1/responses` の全パラメータは使えない | `max_output_tokens` は送らない。送ると `Unsupported parameter: max_output_tokens` |
| 最終 event が `response.done` ではなく `response.completed` のことがある | `response.done` と `response.completed` の両方を終端 event として扱う |
| `response.completed.response.output` が空でも本文が生成されていることがある | 中間 SSE event の `response.output_text.delta` / `response.output_text.done` / `response.output_item.done` から本文や item を復元する |
| text-only message を parts 配列にすると挙動差が出やすい | text-only は `content: "..."`、画像などがある場合だけ parts 配列にする |
| tools 指定時の選択挙動を backend 任せにすると不安定になり得る | tools がある場合は `tool_choice: "auto"` を明示する |
| 空応答の原因が見えにくい | `status`, `output_items`, `input_tokens`, `output_tokens`, `reasoning_tokens`, `incomplete_reason` をエラー文に含める |

## Request Shape

Codex provider の基本形:

```json
{
  "model": "gpt-5.3-codex",
  "instructions": "...",
  "input": [
    {
      "type": "message",
      "role": "user",
      "content": "hello"
    }
  ],
  "stream": true,
  "store": false
}
```

tools がある場合:

```json
{
  "tools": [
    {
      "type": "function",
      "name": "read",
      "description": "...",
      "parameters": { "type": "object" }
    }
  ],
  "tool_choice": "auto"
}
```

画像などの multimodal content がある場合だけ parts 配列を使う。

```json
{
  "type": "message",
  "role": "user",
  "content": [
    { "type": "input_text", "text": "describe" },
    { "type": "input_image", "image_url": "data:image/png;base64,...", "detail": "auto" }
  ]
}
```

## SSE Parsing

最終 `response.completed` だけを本文ソースにしない。

今の実装では、SSE 全体を走査して以下を保持する。

| Event | 扱い |
|---|---|
| `response.output_text.delta` | text 差分として連結 |
| `response.output_text.done` | 完成 text として採用 |
| `response.output_item.done` | `item` を `ResponsesOutputItem` として parse |
| `response.completed` / `response.done` | final response として保持 |

final response の `output` が空の場合のみ、中間 event で拾った text / item から `output` を補完する。
final response に `output` がある場合は、それを優先するため二重出力しない。

## Empty Response Diagnosis

空応答時は以下のような診断を出す。

```text
llm_invalid_response: assistant content was empty
(status=completed, output_items=0, input_tokens=5827, output_tokens=20, reasoning_tokens=0, incomplete_reason=none)
```

見方:

| フィールド | 意味 |
|---|---|
| `status=completed` | API 呼び出し自体は成功 |
| `output_items=0` | final response に message / function_call がない |
| `output_tokens>0` | backend 側では何らかの出力処理が発生 |
| `reasoning_tokens=0` | 非表示 reasoning に消えたわけではない |
| `incomplete_reason=none` | token 上限や中断ではない |

`output_tokens>0` かつ `reasoning_tokens=0` かつ `output_items=0` の場合は、
中間 SSE event の取り逃がしを最優先で疑う。

## Verification

Codex provider 周辺を変更したら最低限これを実行する。

```bash
cargo fmt --check
cargo test llm::
cargo check
cargo clippy --all-targets --all-features -- -D warnings
```

サービスへ反映する場合:

```bash
cargo build --release -p egopulse
systemctl --user stop egopulse
install -m 0755 target/release/egopulse ~/.local/bin/egopulse
systemctl --user start egopulse
systemctl --user --no-pager --full status egopulse
```
