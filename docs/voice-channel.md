# EgoPulse Voice Channel Integration Specification

Wake Word / STT / TTS / device playback を持つ外部音声システムを、EgoPulse の会話 runtime に接続するための仕様。

この仕様では StackChan を具体的な接続対象として扱うが、EgoPulse 側の抽象は `stackchan` ではなく `voice` とする。StackChan は voice channel に接続する voice surface の一つであり、別マイク、WebRTC、スマートフォン、別デバイス入力も同じ枠で表現できる。

## 目的

現在の固定音声応答を、EgoPulse の通常会話 turn に置き換える。

変更前:

```text
StackChan wake
  -> voice-gateway /v1/transcribe
  -> stackchan-bridge /stt/events
  -> 固定文を生成
  -> voice-gateway /v1/audio/speech
  -> StackChan 再生
```

変更後:

```text
StackChan wake
  -> voice-gateway /v1/transcribe
  -> stackchan-bridge /stt/events
  -> EgoPulse voice channel に text turn を送信
  -> EgoPulse が会話 runtime として応答テキストを生成
  -> stackchan-bridge が voice-gateway /v1/audio/speech で TTS
  -> StackChan 再生
```

EgoPulse は音声デバイス、STT Provider、TTS Provider、Aivis、StackChan 再生 API を知らない。EgoPulse の責務は、STT 済みテキストを voice channel のユーザー入力として受け取り、会話状態つきで応答テキストを返すことだけである。

## 用語

| 用語 | 意味 |
|---|---|
| voice channel | EgoPulse 内のチャネル種別。音声入出力の会話面を表す。`channel = "voice"` |
| voice surface | voice channel 配下の具体的な音声面。例: `stackchan`, `desk-mic`, `phone`, `webrtc` |
| session key | surface 内の会話セッション名。例: `main`, `kitchen`, `workroom` |
| source | STT event の発生源やトリガー。例: `stackchan-wake`, `manual-record`, `webrtc` |
| device bridge | 音声デバイス固有の I/O を扱う外部プロセス。本仕様の対象は `stackchan-bridge` |

## 責務分離

| 層 | 持つ責務 | 持たない責務 |
|---|---|---|
| EgoPulse | 会話 runtime、session、memory、tools、LLM 応答生成、voice channel API | Wake Word、録音、STT、TTS、音声再生、Aivis/StackChan 固有処理 |
| voice-gateway | STT/TTS Provider 抽象、ReazonSpeech/Aivis などの実行差異吸収、OpenAI 互換 API | 会話状態、device 再生制御、EgoPulse session 管理 |
| stackchan-bridge | StackChan との通信、STT callback 受信、EgoPulse voice API 呼び出し、TTS 呼び出し、WAV 再生 | LLM 会話判断、memory、Provider 固有処理 |
| StackChan Firmware | Wake Word、録音、STT upload、WAV playback、身体 I/O | 会話 runtime、TTS Provider 知識、EgoPulse API 知識 |

## Surface Identity

voice channel は単一のチャネル名を使うが、すべての音声入力を単一セッションに押し込めない。EgoPulse 内部では `voice surface + session key` から安定した `surface_thread` を作る。

正規化ルール:

```text
channel = "voice"
chat_type = "voice"
surface_user = request.user_id
surface_thread = "{surface}:{session_key}"
agent_id = request.agent_id ?? config.default_agent
```

例:

| surface | session_key | source | surface_thread | 意味 |
|---|---|---|---|---|
| `stackchan` | `main` | `stackchan-wake` | `stackchan:main` | StackChan の通常会話 |
| `desk-mic` | `main` | `desk-mic` | `desk-mic:main` | デスクマイク入力 |
| `phone` | `main` | `mobile-voice` | `phone:main` | スマホ音声入力 |
| `webrtc` | `browser-a` | `webrtc` | `webrtc:browser-a` | ブラウザ音声入力 |

複数の入力を同じ会話として扱いたい場合は、bridge 側で同じ `surface` と `session_key` に正規化して送る。別会話として扱いたい場合は、どちらかを分ける。

## EgoPulse Config

`channels.voice` を追加する。

```yaml
channels:
  voice:
    enabled: true
    auth_token:
      source: env
      id: EGOPULSE_VOICE_AUTH_TOKEN
    default_surface: stackchan
    default_session: main
    allowed_surfaces:
      - stackchan
```

フィールド:

| フィールド | 型 | 必須 | デフォルト | 説明 |
|---|---|---:|---|---|
| `enabled` | `bool` | 任意 | `false` | voice channel API を有効化する |
| `auth_token` | `string \| SecretRef` | 有効時必須 | なし | bridge からの API 呼び出しを認証する Bearer token |
| `default_surface` | `string` | 任意 | `voice` | request で surface 未指定時の surface |
| `default_session` | `string` | 任意 | `main` | request で session_key 未指定時の session |
| `allowed_surfaces` | `list<string>` | 任意 | `[]` | 空なら全 surface 許可。非空なら列挙された surface のみ許可 |

`auth_token` は `channels.web.auth_token` とは分ける。Web UI 用 token と device bridge 用 token を共有しない。

## EgoPulse API

### POST /api/voice/turn

STT 済みテキストを voice channel のユーザー入力として送信し、EgoPulse の応答テキストを返す。

```text
Authorization: Bearer <channels.voice.auth_token>
Content-Type: application/json
```

Request:

```json
{
  "surface": "stackchan",
  "session_key": "main",
  "user_id": "local-speaker",
  "text": "聞こえてますか",
  "source": "stackchan-wake",
  "agent_id": "default"
}
```

Required fields:

| フィールド | 型 | 説明 |
|---|---|---|
| `text` | `string` | ユーザー発話の STT 結果。空文字は拒否する |

Optional fields:

| フィールド | 型 | デフォルト | 説明 |
|---|---|---|---|
| `surface` | `string` | `channels.voice.default_surface` | 音声面の名前 |
| `session_key` | `string` | `channels.voice.default_session` | surface 内の会話セッション |
| `user_id` | `string` | `voice-user` | 発話者識別子。個人識別より surface 内の安定名を優先する |
| `source` | `string` | `unknown` | 発生源やトリガー。session identity には使わない |
| `agent_id` | `string` | `config.default_agent` | 応答する agent |

Response:

```json
{
  "ok": true,
  "response": "はい、聞こえています。今日はどうしましたか？",
  "session_key": "main",
  "surface": "stackchan",
  "surface_thread": "stackchan:main",
  "agent_id": "default",
  "trace_id": "550e8400-e29b-41d4-a716-446655440000"
}
```

Error response:

```json
{
  "ok": false,
  "error": "invalid_params",
  "message": "text is required"
}
```

主なエラー:

| error | HTTP | 説明 |
|---|---:|---|
| `unauthorized` | 401 | token がない、または一致しない |
| `voice_channel_disabled` | 404 | `channels.voice.enabled` が false の場合は route を公開しない |
| `invalid_params` | 400 | text が空、surface/session_key が不正 |
| `surface_not_allowed` | 403 | `allowed_surfaces` に含まれない |
| `turn_failed` | 500 | EgoPulse turn 処理に失敗 |

## Turn Processing

`/api/voice/turn` は既存の agent loop を使う。

```text
POST /api/voice/turn
  -> auth
  -> request validation
  -> voice surface identity normalization
  -> SurfaceContext(channel="voice", surface_thread="{surface}:{session_key}", chat_type="voice")
  -> process_turn()
  -> response text
```

LLM にユーザー入力として渡す本文は `text` のみとする。`surface`、`session_key`、`user_id`、`source`、`agent_id` は routing、session identity、認可後の処理、observability に使用し、system prompt や user message の本文には注入しない。

EgoPulse の履歴には、通常の user/assistant message として保存する。

## Channel Adapter

voice turn は `POST /api/voice/turn` の同期 request/response で完結する。EgoPulse は HTTP response で応答テキストを返し、TTS と playback は request 元の device bridge が担当する。

`voice` の `ChannelAdapter::send_text()` は outbound delivery を行わない。`agent_send` などから呼び出された場合は、配送成功を装う no-op ではなく、`outbound voice delivery is not supported` と明示的に失敗を返す。

## stackchan-bridge Integration

bridge 側は固定文生成をやめ、agent runtime に 1 turn の処理を依頼する。
本仕様の接続先は EgoPulse だが、bridge の型名と設定名には EgoPulse 固有名を含めない。

```text
SpokenReplyPipeline
  input: TranscriptionResult
  deps:
    - AgentClient
    - VoiceGatewayClient
    - DeviceTransport
  behavior:
    - source filter
    - busy guard
    - AgentClient.createTurn(result.text)
    - VoiceGatewayClient.synthesizeSpeech(response)
    - DeviceTransport.playWav(wav)
```

bridge config 例:

```yaml
agent_runtime:
  endpoint: "http://127.0.0.1:10961/api/voice/turn"
  auth_token:
    source: env
    id: AGENT_RUNTIME_AUTH_TOKEN
  agent_id: "default"
  surface: "stackchan"
  session_key: "main"
  user_id: "local-speaker"
  timeout_ms: 120000

spoken_reply:
  enabled: true
  listen_sources:
    - "stackchan-wake"
  cooldown_ms: 800
  busy_policy: "ignore"
```

`AgentClient` の契約は、STT 済みテキストと会話面の識別情報を送り、agent runtime が生成した応答テキストを受け取ることに限定する。LLM Provider、memory、tools、EgoPulse の内部構造は公開しない。

bridge は agent runtime の応答が空の場合、TTS を実行せず正常に turn を終了する。呼び出しに失敗した場合は TTS を実行せず、`/spoken-reply/status` の `lastError` に記録する。エラー時のフォールバック音声は再帰的な失敗や障害の隠蔽を避けるため設けない。

## Security

- `/api/voice/turn` は `channels.voice.auth_token` で認証する。
- `channels.web.auth_token` とは共有しない。
- token は SecretRef で管理できるようにする。
- `source` は認証や権限判定の根拠にしない。
- `allowed_surfaces` は surface 名の allowlist であり、token の代替ではない。

## Observability

voice turn では以下を構造化ログに含める。

| field | 値 |
|---|---|
| `channel` | `voice` |
| `surface` | request surface |
| `session_key` | request session_key |
| `surface_thread` | normalized surface_thread |
| `source` | request source |
| `agent_id` | resolved agent |
| `trace_id` | turn trace |

既存の `/telemetry` の `recent_turns` には `channel: "voice"` として出る。

bridge 側の `/spoken-reply/status` には、agent runtime 呼び出し時間、TTS 生成時間、再生時間、turn 全体の所要時間を記録する。

## 実装 Plan

### Phase 1: EgoPulse voice channel API

1. `Config` に `channels.voice` の型と loader validation を追加する。
2. `VoiceAdapter` を追加し、`chat_type_routes()` で `("voice", Private)` を返す。
3. `build_app_state` で voice adapter を登録する。
4. Web server に `/api/voice/turn` を追加する。
5. `channels.voice.auth_token` 用の認証 middleware を追加する。
6. request から `SurfaceContext(channel="voice")` を作り、`process_turn()` を呼ぶ。
7. unit test で surface/session 正規化、auth、empty text、allowed_surfaces を確認する。

### Phase 2: stackchan-bridge を agent runtime に接続

1. bridge config に `agent_runtime` を追加する。
2. `AgentClient` を追加し、設定された `endpoint` を呼ぶ。本仕様では EgoPulse の `POST /api/voice/turn` に接続する。
3. `SpokenReplyPipeline` の `buildReplyText()` を `AgentClient.createTurn()` に置き換える。
4. `/spoken-reply/status` に agent runtime/TTS/playback timing を追加する。
5. 固定文依存の docs を更新する。

### Phase 3: End-to-end verification

1. EgoPulse を `channels.voice.enabled=true` で起動する。
2. voice-gateway を Reazon STT + Aivis TTS で起動する。
3. stackchan-bridge を Wi-Fi transport で起動する。
4. Wake Word -> STT -> EgoPulse response -> Aivis TTS -> StackChan playback を確認する。
5. `channel=voice`, `surface_thread=stackchan:main` として EgoPulse に履歴が保存されることを確認する。

## 非目標

本仕様は以下を含まない。

- EgoPulse から自発的に StackChan に話しかける outbound voice delivery
- 音声ストリーミング応答
- partial STT / barge-in / interruption
- speaker diarization
- VAD による録音終了制御
- 複数 voice surface の同時 mix
- device 固有の表情・姿勢制御を EgoPulse channel API に含めること

必要になった場合は、それぞれを独立した要件と責務境界を持つ別仕様として定義する。
