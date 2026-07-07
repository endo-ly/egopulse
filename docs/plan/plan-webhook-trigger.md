# Plan: Webhook Trigger

Webhook を外部 trigger として受け取り、receiver ごとに設定した target channel / thread / agent 上でエージェントを行動させる。Webhook 専用 channel は作らず、既存 ChannelAdapter と TurnScheduler に橋渡しする。

> **Note**: 振る舞い（What）は決して変えてはいけないが、より美しい設計があれば実装方法（HOW）だけは変えてもよい。

## 設計方針

- 正本仕様は `docs/superpowers/specs/2026-07-05-webhook-trigger-design.md` とする。
- Webhook は会話チャネルではなく trigger として扱う。`channels.webhook`、Webhook 専用 `ChannelAdapter`、`ChannelRegistry` 登録は追加しない。
- HTTP route は既存 Axum Web server に mount するが、Web UI API auth middleware とは分離し、receiver ごとの Bearer token で認証する。
- 受信後は `target.channel / target.thread / target.agent` から `SurfaceContext` を作り、既存 `TurnScheduler` へ enqueue する。`SurfaceContext.channel` は常に target channel を使う。
- `target.channel` は Config ではなく起動中 `ChannelRegistry` に登録済みかで検証する。`voice` は初期 target 対象外とする。
- Discord / Telegram target が既存 channel config で `secret: true` の場合、通常チャネル入力と同じく `SurfaceContext.scope = ConversationScope::Secret` に解決する。Webhook 経由でも storage 境界を緩めない。
- payload format は設定項目化しない。EgoGraph Pipelines payload は自動整形し、それ以外の JSON は generic formatter で扱う。
- 初期実装では HMAC、replay protection、rule engine、複雑な条件分岐 routing は扱わない。payload size limit は固定 64KB とする。
- 関連 docs は `docs/api.md`, `docs/config.md`, `docs/channels.md` を更新対象とする。

## TDD 方針

テストリスト項目と自動テストを区別し、各 Cycle ではテストリストから 1 項目だけを選ぶ。1 回の RED では失敗する自動テストを 1 件だけ追加し、GREEN ではそのテストを通す最小実装だけを行い、全テストが Green の状態で REFACTOR する。1 項目に必要なテスト総数は 1 件とは限らないため、境界や異常系が残る場合は同じ項目を複数 Cycle に分ける。実装中に新しい不安を見つけた場合は、その場で Green に混ぜず、テストリストへ追加して次の Cycle で扱う。

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成 → レビュー待機・レビューバック

## 対象一覧

| 対象 | 種別 | 既存パターン / 参照元 | 備考 |
| -- | -- | -- | -- |
| `docs/superpowers/specs/2026-07-05-webhook-trigger-design.md` | 参照 | 正本仕様 | What の確認元 |
| `src/config/types.rs` | 変更 | `ChannelConfig`, SecretRef, typed config | `webhooks.receivers` 型を追加 |
| `src/config/loader.rs` | 変更 | `FileConfig`, `resolve_string_or_ref`, validation | receiver token / target 正規化 |
| `src/config/resolve.rs` | 変更 | channel / voice accessor | webhook receiver accessor を追加 |
| `src/config/persist.rs` | 変更 | config serialization / dotenv collection | Web UI 等の config 保存で `webhooks.receivers` と SecretRef を保持 |
| `src/webhooks/mod.rs` | **新規** | `channels/voice.rs`, `channels/web/*` | module公開とhandler統合 |
| `src/webhooks/auth.rs` | **新規** | `channels/web/auth.rs` | Bearer token抽出、constant-time比較再利用 |
| `src/webhooks/formatter.rs` | **新規** | voice request validation, JSON整形 | EgoGraph / generic formatter |
| `src/webhooks/handler.rs` | **新規** | Axum handlers, Voice route | HTTP handler, validation, enqueue |
| `src/webhooks/error.rs` | **新規** | JSON error response pattern | WebhookError -> HTTP response |
| `src/channels/web/mod.rs` | 変更 | route mount, voice route分離 | `/api/webhooks/{receiver_id}` を専用routeでmount |
| `src/runtime/channel_input.rs` | 変更候補 | `build_channel_context`, `submit_agent_turn` | 必要なら webhook 用 helper を追加 |
| `src/lib.rs` | 変更 | module公開 | `webhooks` module追加 |
| `docs/api.md` | 変更 | HTTP API仕様 | Webhook APIを追加 |
| `docs/config.md` | 変更 | 設定仕様 | `webhooks` 設定を追加 |
| `docs/channels.md` | 変更 | channel仕様 | Webhookはchannelではない旨を追記 |

## テストリスト / 不安リスト

| ID | 観点 | 期待する振る舞い | 優先 | 対応するCycle | 状態 / 今回対象外理由 |
| -- | -- | -- | -- | -- | -- |
| T1 | 設定正常系 | 複数 receiver の token と target を SecretRef 解決込みで読み込める | High | Step 1 | 未着手 |
| T2 | 設定異常系 | receiver token 未設定、target.channel/thread 不正、agent 不在を config load または handler validation で拒否できる。Web target の空 thread は `main` 正規化対象なので拒否しない | High | Step 2, Step 7 | 未着手 |
| T2A | 設定保存 | config 保存時に `webhooks.receivers` と receiver token の SecretRef / dotenv entry が欠落しない | High | Step 2A | 未着手 |
| T3 | Formatter正常系 | EgoGraph Pipelines payload を調査・原因・ユーザー操作要否・次アクションを促す入力文へ整形する | High | Step 3 | 未着手 |
| T4 | Formatter汎用系 | 未知 JSON payload を pretty JSON 付き generic 入力文へ整形する | High | Step 4 | 未着手 |
| T5 | HTTP認証 | receiver ごとの Bearer token だけを受理し、Web UI token や他receiver tokenを拒否する | High | Step 5 | 未着手 |
| T6 | Payload制限 | 64KB を超える payload を turn enqueue 前に 413 で拒否する | High | Step 6 | 未着手 |
| T7 | Target validation | ChannelRegistry 未登録、voice、非 Web target の空thread、agent不在を `400 invalid_target` で拒否する | High | Step 7, Step 8 | 未着手 |
| T8 | Session identity | target channelを `SurfaceContext.channel` に使い、`surface_user=webhook:{receiver}`、origin_id UUIDを設定する | High | Step 9 | 未着手 |
| T9 | Secret scope | Discord / Telegram target の `secret: true` が `SurfaceContext.scope == Secret` になり、通常DBへ漏れない | High | Step 9A | 未着手 |
| T10 | Turn enqueue | 正常な Webhook は turn 完了を待たず `202 Accepted` を返し、TurnScheduler に投入される | High | Step 10 | 未着手 |
| T11 | Web target正規化 | `target.channel=web` の thread は既存 Web session key と同じ規則で `web:` prefix と空値を正規化する | Medium | Step 11 | 未着手 |
| T12 | Persistence | Webhook payload は Channel Log に保存せず、通常 session user input として保存される | Medium | Step 12 | 未着手 |
| T13 | Route分離 | `/api/webhooks/{receiver_id}` は Web UI auth middleware 配下に入らず receiver tokenだけで処理される | High | Step 13 | 未着手 |
| T14 | Docs | API / config / channels docs が実装仕様と一致する | High | Step 14 | 未着手 |
| T15 | HMAC / replay protection | signature と timestamp でリプレイを防止する | Low | 今回対象外 | 初期ユースケースでは receiver token と payload上限で十分。別仕様で扱う |
| T16 | Rule routing | payload内容で target を条件分岐する | Low | 今回対象外 | 過剰設計を避けるため receiver固定targetに限定する |

---

## Step 0: Worktree 作成

- ブランチ名: `feat/webhook-trigger`
- Worktree: `/root/workspace/egopulse/wt-webhook-trigger`
- 作成コマンド:
  - `git worktree add wt-webhook-trigger -b feat/webhook-trigger`
- 注意:
  - 現在の作業ツリーには既存未コミット差分があるため、実装は worktree 上で行う。
  - 仕様書 `docs/superpowers/specs/2026-07-05-webhook-trigger-design.md` が未コミットの場合は、Plan と一緒に実装ブランチへ持ち込む。

---

## Step 1: Config TDD Cycle - receiver設定の正常読み込み

### この Step の目的

複数 Webhook receiver を設定から読み込み、receiver token と target を解決できる状態にする。

### 今回選ぶ項目

- 対象: `T1`
- 選ぶ理由: HTTP handler より前に設定契約を固定するため
- この時点では扱わないこと: agent 存在検証、ChannelRegistry 検証、HTTP route

### RED: 失敗する自動テストを書く

- 追加するテスト名: `webhook_receivers_config_resolves_declared_targets`
- Given: `webhooks.receivers.egograph` と `github` を持つ YAML
- When: config loader で読み込む
- Then: receiver ごとの token、target.channel、target.thread、target.agent が取得できる
- 失敗理由の想定: `webhooks` config 型と accessor が未実装

### GREEN: 最小実装

`Config` に `webhooks` 設定を追加し、loader で `StringOrRef` token を既存 SecretRef 経路で解決する。receiver id は既存 ID 型のパターンに合わせ、空 id は拒否または無視ではなく設定エラーにする。

### REFACTOR: 設計の整理

- 重複: SecretRef 解決処理を複製していないか
- 命名: `WebhookReceiverConfig`, `WebhookTargetConfig` が責務を表すか
- 責務: loader と runtime validation を混ぜていないか
- テストの構造的結合: YAML入力と公開accessorだけを検証しているか
- 次の項目へ進める身軽さ: validationを追加しやすいか

### テストリスト更新

- 完了: `T1`
- 追加: 実装中に見つかった設定境界があれば追記
- 次候補: `T2`

### コミット

`feat: add webhook receiver configuration`

---

## Step 2: Config TDD Cycle - receiver設定の基本validation

### この Step の目的

明らかに壊れた receiver 設定を起動前に検出する。

### 今回選ぶ項目

- 対象: `T2`
- 選ぶ理由: handler到達前に不変条件を小さく固定するため
- この時点では扱わないこと: ChannelRegistry 登録状態、agent存在の runtime validation

### RED: 失敗する自動テストを書く

- 追加するテスト名: `webhook_receiver_requires_token_channel_and_non_web_thread`
- Given: tokenなし、channel空、または `target.channel != "web"` で thread 空の YAML
- When: config loader で読み込む
- Then: 構造化 ConfigError になる
- 失敗理由の想定: webhook receiver validation が未実装

### GREEN: 最小実装

receiver token、target.channel、非 Web target の target.thread 必須 validation を loader に追加する。`target.channel == "web"` の空 thread は Step 11 の正規化対象として許可する。`target.agent` は省略可能として `default_agent` 解決を handler validation に残す。

### REFACTOR: 設計の整理

- 重複: validation helper が過度に汎用化されていないか
- 命名: error message が lower-case で具体的か
- 責務: 起動時に判断できることだけを loader に置いているか
- テストの構造的結合: error文言に依存しすぎていないか
- 次の項目へ進める身軽さ: handler validationと役割が分かれているか

### テストリスト更新

- 完了: `T2` の設定基本validation
- 追加: なし
- 次候補: `T2A`

### コミット

Step 1 とまとめて `feat: add webhook receiver configuration`

---

## Step 2A: Config TDD Cycle - config保存時のWebhook設定保持

### この Step の目的

Web UI 等から config を保存したときに、`webhooks.receivers` と receiver token の SecretRef が YAML / `.env` から消えないことを保証する。

### 今回選ぶ項目

- 対象: `T2A`
- 選ぶ理由: 既存 `save_config_with_secrets` は `SerializableConfig` を経由するため、persist対応漏れが設定消失に直結するため
- この時点では扱わないこと: Webhook設定のWeb UI編集画面

### RED: 失敗する自動テストを書く

- 追加するテスト名: `webhook_receivers_persist_with_secret_refs_on_config_save`
- Given: EnvRef token を持つ `webhooks.receivers.egograph` を含む Config
- When: `save_config_with_secrets` で YAML と `.env` を保存し、再読み込みする
- Then: receiver、target、SecretRef、dotenv entry が保持される
- 失敗理由の想定: `SerializableConfig` と dotenv collection が Webhook 設定を扱っていない

### GREEN: 最小実装

`src/config/persist.rs` に Webhook 用 Serializable 型を追加し、`SerializableConfig` へ `webhooks` を含める。receiver token が EnvRef の場合は `collect_dotenv_entries` に含める。

### REFACTOR: 設計の整理

- 重複: Channel / provider の secret serialization と同じ考え方で実装できているか
- 命名: serializable 型が config 型と対応しているか
- 責務: persist層がvalidationを重複していないか
- テストの構造的結合: YAML round-trip と `.env` entry という外部結果を見ているか
- 次の項目へ進める身軽さ: formatterへ進めるか

### テストリスト更新

- 完了: `T2A`
- 追加: なし
- 次候補: `T3`

### コミット

Step 1-2 とまとめて `feat: add webhook receiver configuration`

---

## Step 3: Formatter TDD Cycle - EgoGraph payload整形

### この Step の目的

第一ユースケースである EgoGraph Pipelines 失敗通知を、エージェントが行動しやすい入力文にする。

### 今回選ぶ項目

- 対象: `T3`
- 選ぶ理由: Webhook の価値が最初に現れる中核の振る舞いだから
- この時点では扱わないこと: HTTP handler、generic JSON、巨大payload

### RED: 失敗する自動テストを書く

- 追加するテスト名: `formats_egograph_pipeline_failure_for_agent_action`
- Given: `source=urn:egograph:pipelines`, `type=egograph.pipelines.workflow_failed`, `data.workflow_id/run_id/error_message/custom_message`
- When: formatter に渡す
- Then: 主要フィールドと「likely cause / user action required / next action」を促す文が含まれる
- 失敗理由の想定: formatter 未実装

### GREEN: 最小実装

`src/webhooks/formatter.rs` を追加し、EgoGraph 判定と固定テンプレートを実装する。payload の欠損フィールドは `(none)` ではなく存在する値だけを整形し、過度な schema validation はしない。

### REFACTOR: 設計の整理

- 重複: field extraction helper が読みやすいか
- 命名: EgoGraph専用関数と generic formatter の境界が明確か
- 責務: formatter が HTTP や config を知らないか
- テストの構造的結合: 出力全文一致で壊れやすくしていないか
- 次の項目へ進める身軽さ: generic fallbackを追加できるか

### テストリスト更新

- 完了: `T3`
- 追加: 欠損fieldの扱いで不安があれば追記
- 次候補: `T4`

### コミット

`feat: format webhook payloads for agent turns`

---

## Step 4: Formatter TDD Cycle - generic JSON fallback

### この Step の目的

EgoGraph 以外の JSON webhook も受け、最低限エージェントが内容を読める入力文にする。

### 今回選ぶ項目

- 対象: `T4`
- 選ぶ理由: 複数 receiver に対応する上で必要な最小汎用性だから
- この時点では扱わないこと: payload type別設定、rule routing

### RED: 失敗する自動テストを書く

- 追加するテスト名: `formats_unknown_json_payload_as_generic_webhook_event`
- Given: EgoGraph条件に一致しない JSON
- When: formatter に渡す
- Then: receiver id と pretty JSON を含む generic 入力文になる
- 失敗理由の想定: generic fallback 未実装

### GREEN: 最小実装

EgoGraph 判定に一致しない場合の fallback を追加する。JSON pretty print には `serde_json` の標準機能を使い、独自文字列組み立てを増やさない。

### REFACTOR: 設計の整理

- 重複: receiver id の埋め込み箇所が一箇所か
- 命名: fallbackの意図が明確か
- 責務: formatter が tokenやtargetを扱っていないか
- テストの構造的結合: JSON field順序に過度依存していないか
- 次の項目へ進める身軽さ: handlerから呼びやすいAPIか

### テストリスト更新

- 完了: `T4`
- 追加: なし
- 次候補: `T5`

### コミット

Step 3 とまとめて `feat: format webhook payloads for agent turns`

---

## Step 5: Handler TDD Cycle - receiver token認証

### この Step の目的

receiver ごとの Bearer token だけを受理し、Web UI token や別receiver tokenとの混同を防ぐ。

### 今回選ぶ項目

- 対象: `T5`
- 選ぶ理由: 公開HTTP endpointの最小セキュリティ境界だから
- この時点では扱わないこと: target validation、turn enqueue、payload size

### RED: 失敗する自動テストを書く

- 追加するテスト名: `webhook_route_accepts_only_matching_receiver_token`
- Given: receiver `egograph` と `github` が別tokenで設定された test router
- When: 正しいtoken、別receiver token、Web token、tokenなしでPOSTする
- Then: 正しいtokenだけが認証を通過し、他は `401 unauthorized`
- 失敗理由の想定: handler/auth 未実装

### GREEN: 最小実装

`src/webhooks/auth.rs` と handler skeleton を追加し、receiver lookup と Bearer token比較だけを実装する。constant-time comparison は既存 `web::auth::constant_time_eq` を再利用するか、公開範囲を最小に調整する。

### REFACTOR: 設計の整理

- 重複: Bearer抽出処理を複製しすぎていないか
- 命名: auth failure と receiver not found の責務が分かれているか
- 責務: handler が formatter/enqueue へ進む前に認証を完了しているか
- テストの構造的結合: route response の外部契約を検証しているか
- 次の項目へ進める身軽さ: payload limitを挟み込めるか

### テストリスト更新

- 完了: `T5`
- 追加: なし
- 次候補: `T6`

### コミット

`feat: add authenticated webhook receiver route`

---

## Step 6: Handler TDD Cycle - payload size limit

### この Step の目的

巨大 JSON が memory / token / LLM 入力を圧迫する前に HTTP boundary で拒否する。

### 今回選ぶ項目

- 対象: `T6`
- 選ぶ理由: 実装コストが低く、公開endpointの安全性に直結するため
- この時点では扱わないこと: 設定化、streaming body処理の高度化

### RED: 失敗する自動テストを書く

- 追加するテスト名: `webhook_route_rejects_payload_over_fixed_limit`
- Given: 64KBを超える request body
- When: 正しい receiver token でPOSTする
- Then: `413 payload_too_large` を返し、turn はenqueueされない
- 失敗理由の想定: body limit 未実装

### GREEN: 最小実装

Webhook route に固定 64KB の body limit を適用する。Axum の body limit layer か handler内 size check のうち、既存routerと衝突しにくい方法を選ぶ。

### REFACTOR: 設計の整理

- 重複: size定数が一箇所にあるか
- 命名: `MAX_WEBHOOK_PAYLOAD_BYTES` が明確か
- 責務: payload parse前に拒否できているか
- テストの構造的結合: 実装方法ではなくHTTP結果を見ているか
- 次の項目へ進める身軽さ: target validationを追加できるか

### テストリスト更新

- 完了: `T6`
- 追加: なし
- 次候補: `T7`

### コミット

Step 5 とまとめるか、差分が独立する場合は `feat: limit webhook payload size`

---

## Step 7: Handler TDD Cycle - target channel validation

### この Step の目的

応答配送できない target channel へ turn を投入しない。

### 今回選ぶ項目

- 対象: `T7`
- 選ぶ理由: `202 Accepted` 後に配送不能になる中途半端な状態を防ぐため
- この時点では扱わないこと: agent不在、web thread正規化

### RED: 失敗する自動テストを書く

- 追加するテスト名: `webhook_route_rejects_unregistered_or_voice_target_channel`
- Given: target.channel が `missing` または `voice` の receiver
- When: 正しいtokenでPOSTする
- Then: `400 invalid_target` を返し、turn はenqueueされない
- 失敗理由の想定: ChannelRegistry validation 未実装

### GREEN: 最小実装

handler で `state.channels.get(target.channel)` を確認し、未登録または `voice` の場合は `invalid_target` を返す。Config上の enabled では判定しない。

### REFACTOR: 設計の整理

- 重複: target validationが分散していないか
- 命名: validation errorが外部に分かりやすいか
- 責務: handlerがruntime状態を使う理由が明確か
- テストの構造的結合: registry登録有無という外部結果を検証しているか
- 次の項目へ進める身軽さ: agent validationを追加できるか

### テストリスト更新

- 完了: `T7` のchannel検証
- 追加: なし
- 次候補: `T7` のagent/thread検証

### コミット

`feat: validate webhook targets before enqueue`

---

## Step 8: Handler TDD Cycle - target agent / thread validation

### この Step の目的

壊れた receiver target を `202 Accepted` 後の stop condition に流さず、受信時点で拒否する。

### 今回選ぶ項目

- 対象: `T7`
- 選ぶ理由: senderへ受信可否を正しく返すため
- この時点では扱わないこと: turn実行中のLLM/tool error

### RED: 失敗する自動テストを書く

- 追加するテスト名: `webhook_route_rejects_missing_agent_and_blank_thread`
- Given: target.agent が存在しない receiver と、Discord / Telegram target で thread が空白の receiver
- When: 正しいtokenでPOSTする
- Then: `400 invalid_target` を返し、turn はenqueueされない
- 失敗理由の想定: agent/thread validation 未実装

### GREEN: 最小実装

省略 agent は `default_agent` に解決し、解決後 agent が `config.agents` に存在することを handler で検証する。thread は Discord / Telegram など非 Web target では trim 後の空を拒否し、Web target では Step 11 の正規化に委ねる。

### REFACTOR: 設計の整理

- 重複: loader validationとhandler validationの境界が明確か
- 命名: resolved agent helper が読みやすいか
- 責務: stop condition頼みになっていないか
- テストの構造的結合: config内部構造に寄りすぎていないか
- 次の項目へ進める身軽さ: SurfaceContext生成へ進めるか

### テストリスト更新

- 完了: `T7`
- 追加: なし
- 次候補: `T8`

### コミット

Step 7 とまとめて `feat: validate webhook targets before enqueue`

---

## Step 9: Runtime TDD Cycle - SurfaceContext identity

### この Step の目的

Webhook を会話チャネルにせず、target channel 上の turn として正しい identity を作る。

### 今回選ぶ項目

- 対象: `T8`
- 選ぶ理由: 本設計の最重要 invariant だから
- この時点では扱わないこと: TurnScheduler統合、Channel Log保存

### RED: 失敗する自動テストを書く

- 追加するテスト名: `webhook_context_uses_target_channel_and_receiver_surface_user`
- Given: receiver `egograph`, target.channel `discord`, thread `123`, agent `default`
- When: webhook handler が context を構築する
- Then: `channel=discord`, `surface_user=webhook:egograph`, `surface_thread=123`, `chat_type=discord`, `origin_id` が非空UUIDになる
- 失敗理由の想定: context builder 未実装

### GREEN: 最小実装

handler か `src/webhooks` 内 helper で `SurfaceContext` を作成する。`build_channel_context` を再利用できる場合は使い、Webhook 固有の `surface_user` と `origin_id` だけ補う。この時点では scope は Normal のままでよく、secret scope は次 Cycle で追加する。

### REFACTOR: 設計の整理

- 重複: context生成が handler内に肥大化していないか
- 命名: webhookがtriggerであることがコードから読めるか
- 責務: ChannelLog処理を混ぜていないか
- テストの構造的結合: helper private APIに依存しすぎていないか
- 次の項目へ進める身軽さ: enqueueへ渡せるか

### テストリスト更新

- 完了: `T8`
- 追加: なし
- 次候補: `T9`

### コミット

`feat: enqueue webhook events as target channel turns`

---

## Step 9A: Runtime TDD Cycle - target secret scope 解決

### この Step の目的

Webhook が secret 設定の Discord / Telegram target に入った場合でも、通常チャネル入力と同じ storage 境界を維持する。

### 今回選ぶ項目

- 対象: `T9`
- 選ぶ理由: secret channel の内容を通常DBへ保存する実装を防ぐため
- この時点では扱わないこと: Channel Log保存、turn enqueue完了後の配送確認

### RED: 失敗する自動テストを書く

- 追加するテスト名: `webhook_context_uses_secret_scope_for_secret_discord_or_telegram_target`
- Given: `secret: true` の Discord channel または Telegram chat を target にする receiver
- When: webhook context を構築する
- Then: `SurfaceContext.scope == ConversationScope::Secret` になり、`secret: false` または未設定 target は `ConversationScope::Normal` になる
- 失敗理由の想定: Webhook context builder が target channel config の secret flag を見ていない

### GREEN: 最小実装

Discord / Telegram target では、既存 channel 実装の `scope_for_thread` と同じ規則で target thread から channel config を引き、`secret: true` を `ConversationScope::Secret` に変換する。Web target は初期実装では `ConversationScope::Normal` のままとする。

### REFACTOR: 設計の整理

- 重複: Discord / Telegram の secret scope 解決を不必要に複製していないか
- 命名: target scope helper が channel identity と混ざっていないか
- 責務: Webhook handler が storage 境界の意図を失っていないか
- テストの構造的結合: DB実装ではなく `SurfaceContext.scope` の外部契約を検証しているか
- 次の項目へ進める身軽さ: enqueueへ渡せるか

### テストリスト更新

- 完了: `T9`
- 追加: secret scope の永続化先を統合で確認する不安があれば `T11` に追記
- 次候補: `T10`

### コミット

Step 9 とまとめて `feat: enqueue webhook events as target channel turns`

---

## Step 10: Handler TDD Cycle - 202 Accepted と TurnScheduler enqueue

### この Step の目的

正常な Webhook を turn として投入し、HTTP sender には turn 完了を待たず受信成功を返す。

### 今回選ぶ項目

- 対象: `T10`
- 選ぶ理由: Webhook trigger のE2E中核だから
- この時点では扱わないこと: Web target正規化、Channel Log非保存の確認

### RED: 失敗する自動テストを書く

- 追加するテスト名: `webhook_route_accepts_and_enqueues_turn_without_waiting_for_completion`
- Given: FakeProvider と valid receiver/target を持つ router
- When: 正しいtokenでPOSTする
- Then: `202 Accepted` を返し、target session に user input と assistant response が保存される
- 失敗理由の想定: TurnScheduler enqueue 未接続

### GREEN: 最小実装

formatter 出力を input として `ScheduledTurn` を TurnScheduler へ投入する。既存 `submit_agent_turn` が使えるなら使い、HTTP handler は response生成を待たない。

### REFACTOR: 設計の整理

- 重複: enqueue処理が runtime helper と重複していないか
- 命名: accepted response が turn完了を意味しないことが明確か
- 責務: HTTP response と agent executionが分離しているか
- テストの構造的結合: 非同期完了待ちが flaky でないか
- 次の項目へ進める身軽さ: web正規化を差し込めるか

### テストリスト更新

- 完了: `T10`
- 追加: 非同期テストが不安定なら待機helper改善を追記
- 次候補: `T11`

### コミット

Step 9 とまとめて `feat: enqueue webhook events as target channel turns`

---

## Step 11: Web target TDD Cycle - Web session key正規化

### この Step の目的

Webhook target が Web の場合、既存 Web UI と同じ session identity に入るようにする。

### 今回選ぶ項目

- 対象: `T11`
- 選ぶ理由: `web:main` と `main` が別履歴になる事故を防ぐため
- この時点では扱わないこと: WebSocket/SSE通知

### RED: 失敗する自動テストを書く

- 追加するテスト名: `webhook_web_target_normalizes_thread_like_web_session_key`
- Given: target.channel `web`, thread `web:main`, `web:   `, `custom`
- When: webhook context を構築する
- Then: 既存 `web_session_key` と同じ規則で surface_thread が正規化される
- 失敗理由の想定: Web target専用正規化未実装

### GREEN: 最小実装

`target.channel == "web"` の場合だけ `channels::web::web_session_key` を使う。Discord / Telegram は trim のみに留め、過剰なID検証は行わない。

### REFACTOR: 設計の整理

- 重複: Web正規化ロジックをコピーしていないか
- 命名: channel固有正規化の分岐が読みやすいか
- 責務: handlerから独立した小さな関数か
- テストの構造的結合: 既存Web仕様との整合を見ているか
- 次の項目へ進める身軽さ: persistence検証へ進めるか

### テストリスト更新

- 完了: `T11`
- 追加: なし
- 次候補: `T12`

### コミット

`fix: normalize webhook web targets`

---

## Step 12: Persistence TDD Cycle - Channel Logに保存しない

### この Step の目的

Webhook payload が Discord / Telegram の Multi-Agent Channel Log 上で人間発話に見えないことを保証する。

### 今回選ぶ項目

- 対象: `T12`
- 選ぶ理由: Webhook trigger と人間会話を混同しないため
- この時点では扱わないこと: 将来の監査用Webhook受信ログ

### RED: 失敗する自動テストを書く

- 追加するテスト名: `webhook_turn_persists_to_target_session_without_channel_log_message`
- Given: Discord target の webhook receiver
- When: Webhook を受信して turn が完了する
- Then: target agent session に user/assistant message が保存され、Channel Log には webhook user message が保存されない
- 失敗理由の想定: もし channel_input の Channel Log 経路を誤用していると失敗する

### GREEN: 最小実装

Webhook handler は `store_human_channel_log_message` を呼ばず、通常の scheduled turn だけを投入する。必要ならこの挙動をテストしやすい helper に分ける。

### REFACTOR: 設計の整理

- 重複: Channel Log 非対象の意図がコメントなしでも読めるか
- 命名: `submit_webhook_turn` などの責務が狭いか
- 責務: runtime/channel_input の人間入力境界を乱していないか
- テストの構造的結合: DBの外部観測で確認しているか
- 次の項目へ進める身軽さ: route mount検証へ進めるか

### テストリスト更新

- 完了: `T12`
- 追加: なし
- 次候補: `T13`

### コミット

Step 10 または Step 11 とまとめるか、独立する場合は `test: cover webhook persistence boundaries`

---

## Step 13: Router TDD Cycle - Web UI auth middlewareから分離

### この Step の目的

Webhook route が Web UI auth token と衝突せず、receiver token だけで認証されることを保証する。

### 今回選ぶ項目

- 対象: `T13`
- 選ぶ理由: route mount時の事故が起きやすい箇所だから
- この時点では扱わないこと: Web UI API既存routeの追加変更

### RED: 失敗する自動テストを書く

- 追加するテスト名: `webhook_route_is_not_protected_by_web_api_auth_middleware`
- Given: Web auth token と Webhook receiver token が別の router
- When: `/api/webhooks/egograph` に receiver token でPOSTする
- Then: Web auth middleware に拒否されず `202` または後続validation結果になる
- 失敗理由の想定: `api_routes.route_layer(require_http_auth)` 配下に誤ってmountしている

### GREEN: 最小実装

`channels/web/mod.rs` で Webhook route を `api_routes` とは別 router として merge する。auth は handler内または webhook専用 layer で行う。

### REFACTOR: 設計の整理

- 重複: route構築が読みやすいか
- 命名: `webhook_routes` が voice_routes と並んで自然か
- 責務: Web moduleはmountだけ、webhooks moduleが処理本体を持つか
- テストの構造的結合: middleware実装ではなく外部HTTP挙動を見ているか
- 次の項目へ進める身軽さ: docs更新へ進めるか

### テストリスト更新

- 完了: `T13`
- 追加: なし
- 次候補: `T14`

### コミット

`feat: mount webhook routes separately from web auth`

---

## Step 14: Docs TDD Cycle - 仕様と実装docsの同期

### この Step の目的

実装された Webhook Trigger の API、設定、チャネル境界を docs に反映する。

### 今回選ぶ項目

- 対象: `T14`
- 選ぶ理由: 公開設定とHTTP契約を使える状態にするため
- この時点では扱わないこと: HMACやrule routingの将来仕様詳細

### RED: 失敗する自動テストを書く

- 追加するテスト名: `docs_are_updated_for_webhook_trigger`
- Given: docs更新前の状態
- When: 手動チェックリストで `docs/api.md`, `docs/config.md`, `docs/channels.md`, 正本仕様を確認する
- Then: endpoint、設定例、target validation、非ChannelAdapter方針、payload制限が記載されている
- 失敗理由の想定: docs未更新

### GREEN: 最小実装

関連 docs を最小範囲で更新する。既存 `docs/config.md` に未コミット差分がある場合は内容を確認し、ユーザー変更を上書きしない。

### REFACTOR: 設計の整理

- 重複: 正本仕様とリファレンスdocsが矛盾していないか
- 命名: `Webhook Trigger` と `webhooks.receivers` が統一されているか
- 責務: docsに実装理由のメタ記述を書いていないか
- テストの構造的結合: 手動チェック項目が具体的か
- 次の項目へ進める身軽さ: 動作確認へ進めるか

### テストリスト更新

- 完了: `T14`
- 追加: なし
- 次候補: 動作確認

### コミット

`docs: document webhook trigger configuration`

---

## Step 15: 動作確認

- Rust formatting:
  - `cargo fmt --check`
- Rust tests:
  - `cargo test`
- Rust compile check:
  - `cargo check`
- Rust lint:
  - `cargo clippy --all-targets --all-features -- -D warnings`
- Focused tests during TDD:
  - `cargo test webhook`
  - `cargo test webhooks`
  - `cargo test config`
- Manual HTTP smoke test:
  - test config に `webhooks.receivers.egograph` を追加する
  - `POST /api/webhooks/egograph` に正しい receiver token と EgoGraph payload を送る
  - `202 Accepted` が返ること
  - target channel に agent 応答が配送されること
  - 間違った token、未定義 receiver、64KB超過 payload がそれぞれ期待エラーになること
- 失敗時:
  - 該当する TDD Cycle に戻り、テストリストへ不安を追加してから修正する

---

## Step 16: Plan・仕様書との自己チェック

実装完了後にこの Plan と `docs/superpowers/specs/2026-07-05-webhook-trigger-design.md` を最初から読み直し、実装・自動テスト・文書が要求した振る舞いと一致しているかを照合する。未実装、過剰実装、テスト不足、仕様書との齟齬を見つけた場合は、該当する TDD Cycle へ戻って修正し、動作確認を再実行してからこの Step を完了する。

- Plan のテストリストと各 Cycle が完了条件を満たしている
- `SurfaceContext.channel` が target channel になっており、`webhook` channel が追加されていない
- Discord / Telegram target の `secret: true` が `ConversationScope::Secret` として維持されている
- Webhook route が Web UI auth middleware と分離している
- receiver token、ChannelRegistry、agent、thread、payload size の validation が揃っている
- EgoGraph payload と generic JSON formatter が仕様どおりである
- Channel Log に Webhook payload を保存していない
- 関連 docs が実装結果と一致している
- 変更ファイル一覧、コミット分割、自動テスト一覧が実際の変更と一致している

---

## Step 17: PR 作成

- PR タイトル: `feat: add webhook trigger receivers`
- PR description:
  - 概要
    - Webhook receiver を追加
    - receiver target channel へ agent turn を enqueue
    - EgoGraph Pipelines payload と generic JSON payload を整形
    - Webhook 設定・API docs を追加
  - テスト
    - `cargo fmt --check`
    - `cargo test`
    - `cargo check`
    - `cargo clippy --all-targets --all-features -- -D warnings`
  - 関連 Issue がある場合は `Close #...` を記載する

---

## Step 18: 初回レビューバック

PR 作成後、レビュー生成を待ってから `pr-review-back-workflow` Skill を実行し、未対応のレビューコメントがあれば修正・検証・コミット・push まで完了する。

- 初回待機: `sleep 15m`
- レビュー対応: `pr-review-back-workflow` Skill を実行する
- まだレビューが無い場合:
  - `sleep 5m` して `pr-review-back-workflow` Skill を再実行する
  - 追加待機と再実行は最大 2 回まで
- レビューコメントが無い、または最大待機後もレビューが無い場合は、その結果を PR に記録して完了扱いにする
- レビュー対応で変更した場合は、必要な動作確認を再実行してからコミット・push する

---

## Step 19: レビュー対応後の再レビューバック

レビュー対応を push した後、追加レビュー生成を待ってから `pr-review-back-workflow` Skill を再実行し、残った指摘や新規指摘があれば同じ品質基準で対応する。

- 対象: Step 18 でレビュー対応の変更を push した場合
- 初回待機: `sleep 15m`
- 再レビュー対応: `pr-review-back-workflow` Skill を実行する
- まだ追加レビューが無い場合:
  - `sleep 5m` して `pr-review-back-workflow` Skill を再実行する
  - 追加待機と再実行は最大 2 回まで
- 追加レビューコメントが無い、または最大待機後も追加レビューが無い場合は、その結果を PR に記録して完了扱いにする
- 再レビュー対応で変更した場合は、必要な動作確認を再実行してからコミット・push する

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
| ---- | ---- | -- |
| `docs/plan/plan-webhook-trigger.md` | **新規** | 本 Plan |
| `docs/superpowers/specs/2026-07-05-webhook-trigger-design.md` | 変更 | 実装中に仕様差分が出た場合のみ同期 |
| `src/config/types.rs` | 変更 | Webhook config 型 |
| `src/config/loader.rs` | 変更 | Webhook config parse / validation |
| `src/config/resolve.rs` | 変更 | Webhook receiver accessor |
| `src/config/persist.rs` | 変更 | Webhook config serialization と dotenv collection |
| `src/webhooks/mod.rs` | **新規** | Webhook module |
| `src/webhooks/auth.rs` | **新規** | receiver auth |
| `src/webhooks/formatter.rs` | **新規** | EgoGraph / generic formatter |
| `src/webhooks/handler.rs` | **新規** | HTTP handler / validation / enqueue |
| `src/webhooks/error.rs` | **新規** | error response |
| `src/channels/web/mod.rs` | 変更 | route mount |
| `src/runtime/channel_input.rs` | 変更候補 | context/enqueue helperが必要な場合のみ |
| `src/lib.rs` | 変更 | module追加 |
| `docs/api.md` | 変更 | Webhook API |
| `docs/config.md` | 変更 | Webhook config |
| `docs/channels.md` | 変更 | Webhook trigger と channel 境界 |

## コミット分割

1. `feat: add webhook receiver configuration` - config型、loader、accessor、設定テスト
2. `feat: format webhook payloads for agent turns` - EgoGraph / generic formatter と単体テスト
3. `feat: add authenticated webhook receiver route` - auth、payload limit、HTTP handler skeleton
4. `feat: validate webhook targets before enqueue` - ChannelRegistry / agent / thread validation
5. `feat: enqueue webhook events as target channel turns` - SurfaceContext生成、TurnScheduler enqueue、persistence境界
6. `fix: normalize webhook web targets` - Web target thread正規化
7. `feat: mount webhook routes separately from web auth` - router mount と統合テスト
8. `docs: document webhook trigger configuration` - docs更新

## 自動テスト一覧（全 15 件）

この一覧はPlan作成時点で必要と判断した最低限の予定であり、最終テスト件数の上限ではない。実装中に追加された不安には、テストリストとTDD Cycleを追加して対応する。

### Config（全 3 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T1 | `webhook_receivers_config_resolves_declared_targets` | Step 1 | `cargo test webhook_receivers_config_resolves_declared_targets` |
| T2 | `webhook_receiver_requires_token_channel_and_non_web_thread` | Step 2 | `cargo test webhook_receiver_requires_token_channel_and_non_web_thread` |
| T2A | `webhook_receivers_persist_with_secret_refs_on_config_save` | Step 2A | `cargo test webhook_receivers_persist_with_secret_refs_on_config_save` |

### Formatter（全 2 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T3 | `formats_egograph_pipeline_failure_for_agent_action` | Step 3 | `cargo test formats_egograph_pipeline_failure_for_agent_action` |
| T4 | `formats_unknown_json_payload_as_generic_webhook_event` | Step 4 | `cargo test formats_unknown_json_payload_as_generic_webhook_event` |

### Handler / Router / Runtime（全 9 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T5 | `webhook_route_accepts_only_matching_receiver_token` | Step 5 | `cargo test webhook_route_accepts_only_matching_receiver_token` |
| T6 | `webhook_route_rejects_payload_over_fixed_limit` | Step 6 | `cargo test webhook_route_rejects_payload_over_fixed_limit` |
| T7 | `webhook_route_rejects_unregistered_or_voice_target_channel` | Step 7 | `cargo test webhook_route_rejects_unregistered_or_voice_target_channel` |
| T7 | `webhook_route_rejects_missing_agent_and_blank_thread` | Step 8 | `cargo test webhook_route_rejects_missing_agent_and_blank_thread` |
| T8 | `webhook_context_uses_target_channel_and_receiver_surface_user` | Step 9 | `cargo test webhook_context_uses_target_channel_and_receiver_surface_user` |
| T9 | `webhook_context_uses_secret_scope_for_secret_discord_or_telegram_target` | Step 9A | `cargo test webhook_context_uses_secret_scope_for_secret_discord_or_telegram_target` |
| T10 | `webhook_route_accepts_and_enqueues_turn_without_waiting_for_completion` | Step 10 | `cargo test webhook_route_accepts_and_enqueues_turn_without_waiting_for_completion` |
| T11 | `webhook_web_target_normalizes_thread_like_web_session_key` | Step 11 | `cargo test webhook_web_target_normalizes_thread_like_web_session_key` |
| T12 | `webhook_turn_persists_to_target_session_without_channel_log_message` | Step 12 | `cargo test webhook_turn_persists_to_target_session_without_channel_log_message` |

### Router Integration（全 1 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T13 | `webhook_route_is_not_protected_by_web_api_auth_middleware` | Step 13 | `cargo test webhook_route_is_not_protected_by_web_api_auth_middleware` |

### Docs 手動チェック

| テストリストID | 確認内容 | 追加Step | 実行方法 |
| -- | -- | -- | -- |
| T14 | `docs/api.md`, `docs/config.md`, `docs/channels.md`, 正本仕様が実装仕様と一致する | Step 14 | 手動確認 |

## 工数見積もり

| Step | 内容 | 見積もり |
| -- | -- | -- |
| Step 1-2A | Config型・loader・validation・persist | ~260 行 |
| Step 3-4 | Formatter | ~140 行 |
| Step 5-6 | Auth・payload limit・handler skeleton | ~180 行 |
| Step 7-8 | Target validation | ~160 行 |
| Step 9-12 | SurfaceContext・enqueue・Web正規化・persistence境界 | ~260 行 |
| Step 13 | Router mount分離 | ~80 行 |
| Step 14 | Docs更新 | ~180 行 |
| Step 15-19 | 動作確認 / 自己チェック / PR / レビューバック | ~80 行 |
| **合計** |  | **~1,340 行** |
