# Plan: Voice Channel と StackChan Agent Runtime 統合

EgoPulse に `voice` channel の同期 turn API を追加し、StackChan bridge の固定返答を `AgentClient` 経由の agent runtime 応答へ置き換える。実装対象は独立した2リポジトリだが、HTTP契約を境界として一つのEnd-to-End機能を完成させる。

> **Note**: 振る舞い（What）は決して変えてはいけないが、より美しい設計があれば実装方法（HOW）だけは変えてもよい。
>
> Howはあくまで参考であり、よりよい設計方針があれば各自で判断し採用する。

## 設計方針

- このPlanは以下の2リポジトリを対象とする。
  - EgoPulse: `/root/workspace/egopulse`
  - StackChan: `/root/workspace/stackchan-lab`
- PlanファイルとVoice Channelの正本仕様はEgoPulseリポジトリに置く。
  - 仕様: `/root/workspace/egopulse/docs/voice-channel.md`
  - Plan: `/root/workspace/egopulse/docs/plan/plan-voice-channel-agent-runtime-integration.md`
- EgoPulseから見た抽象は `voice` channel とする。EgoPulseはStackChan、Wake Word、STT/TTS Provider、音声再生を知らない。
- StackChan bridgeから見た抽象はagent runtimeとする。コード名は `AgentClient`、設定名は `agent_runtime` とし、EgoPulse固有名を含めない。
- リポジトリ間の境界は `POST /api/voice/turn` のHTTP契約だけにする。共有ライブラリ、Git submodule、EgoPulse固有Adapterは追加しない。
- EgoPulseのVoice APIは既存Axum HTTPサーバーへ追加する。専用listenerの追加、共通server設定への再編、`channels.web.host/port` の移動は行わない。
- 初期実装では既存HTTPサーバーを利用するため、EgoPulseの `channels.web.enabled: true` を運用上の前提とする。Voice単独でHTTPサーバーを起動するためのライフサイクル再設計は今回対象外とする。
- `channels.voice.enabled: true` かつ `channels.web.enabled: false` は、Voice APIが実際にはlistenされない壊れた設定なので起動前のconfig validationで拒否する。
- `/api/voice/turn` は既存Web APIと同じlistenerに同居するが、`channels.web.auth_token` は使用せず、`channels.voice.auth_token` 専用middlewareで認証する。
- EgoPulseの `voice` channelでは `surface + session_key` を `surface_thread = "{surface}:{session_key}"` へ正規化し、異なる音声面の履歴を分離する。
- `surface_thread` の連結衝突を防ぐため、trim後の `surface` と `session_key` は空文字および `:` を含む値を拒否する。初期実装ではエスケープ・可逆エンコードを導入しない。
- `source` と `metadata` は観測情報であり、session identityや認可判定には使わず、初期実装ではLLM入力にも混ぜない。
- EgoPulseの応答生成には既存 `process_turn()` と既存session永続化を使う。Voice専用の会話runtimeを作らない。
- 初期実装のVoice APIは同期request/responseとする。音声ストリーミング、partial response、outbound自発発話は扱わない。
- StackChan bridgeでは現在の `SpokenReplyPipeline` が持つsource filter、busy guard、cooldown、TTS、device playbackを維持し、固定文生成部分だけを `AgentClient.createTurn()` に置き換える。
- `AgentClient` はSTT済みテキストと会話面識別情報を送り、応答テキストを返す責務だけを持つ。LLM、memory、tools、EgoPulse内部型を公開しない。
- StackChan bridgeのローカル `config.yaml` は既にGit管理外であるため、`agent_runtime.auth_token` は既存の `wifi.token` と同様に文字列設定として扱う。`config.example.yaml` には実値を置かない。
- 後方互換分岐は作らない。`spoken_reply.enabled: true` で固定返答へ戻るfallbackは廃止し、agent runtimeエラー時は音声生成せず状態へ失敗を記録する。
- 両リポジトリの既存未コミット差分は変更・取り込み・巻き戻しをしない。EgoPulseは `main` からworktreeを作成し、StackChanは既存作業ツリーでfeature branchへ切り替え、対象変更だけをコミットする。

## TDD 方針

テストリスト項目と実際の自動テストを区別し、各Cycleではテストリストから1項目だけを選ぶ。1回のREDでは失敗する自動テストを1件だけ追加し、GREENではそのテストを通す最小実装だけを行い、全テストがGreenの状態でREFACTORする。1つのテストリスト項目に必要なテスト総数は1件とは限らないため、境界や失敗条件が残る場合は同じ項目を複数Cycleに分ける。実装中に新しい不安を発見した場合は、その場でGreenへ混ぜずテストリストへ追加して次のCycleで扱う。

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | Repo / 種別 | 既存パターン / 参照元 | 備考 |
| -- | -- | -- | -- |
| `/root/workspace/egopulse/src/config/` | EgoPulse / 変更 | `ChannelConfig`, loader validation, SecretRef | `channels.voice` の設定・検証・accessor |
| `/root/workspace/egopulse/src/channels/adapter.rs` | EgoPulse / 変更 | `ChannelAdapter`, `ChannelRegistry` | `voice` chat typeの登録 |
| `/root/workspace/egopulse/src/channels/voice.rs` | EgoPulse / **新規** | Web/Discord/Telegram channel実装 | VoiceのHTTP handler、identity正規化、同期turn |
| `/root/workspace/egopulse/src/channels/web/auth.rs` | EgoPulse / 変更候補 | Bearer tokenのconstant-time比較 | 比較処理を再利用可能な最小範囲へ整理 |
| `/root/workspace/egopulse/src/channels/web/mod.rs` | EgoPulse / 変更 | 既存Axum router | `/api/voice/turn` を専用認証route groupとして追加 |
| `/root/workspace/egopulse/src/runtime/mod.rs` | EgoPulse / 変更 | `build_app_state()` のadapter登録 | VoiceAdapter登録のみ。listener起動条件は変更しない |
| `/root/workspace/egopulse/src/error.rs` | EgoPulse / 変更候補 | `thiserror` 構造化エラー | 既存型で表現不能な場合だけ追加 |
| `/root/workspace/egopulse/docs/voice-channel.md` | EgoPulse / 変更 | 本機能の正本仕様 | 実装結果、Web有効前提、最終config/APIへ同期 |
| `/root/workspace/egopulse/docs/config.md` | EgoPulse / 変更 | 設定リファレンス | `channels.voice` を追加 |
| `/root/workspace/egopulse/docs/channels.md` | EgoPulse / 変更 | channel仕様 | Voice channelの責務と制約を追加 |
| `/root/workspace/egopulse/docs/api.md` | EgoPulse / 変更 | HTTP API仕様 | `/api/voice/turn` と専用認証を追加 |
| `/root/workspace/egopulse/egopulse.config.example.yaml` | EgoPulse / 変更 | config例 | secret実値なしのVoice設定例 |
| `/root/workspace/stackchan-lab/bridge/src/config/` | StackChan / 変更 | `BridgeConfig`, `loadConfig()` | `agent_runtime` を追加し入力検証 |
| `/root/workspace/stackchan-lab/bridge/src/agent/AgentClient.ts` | StackChan / **新規** | `VoiceGatewayClient` のfetch/timeout/error変換 | Agent turn HTTP client |
| `/root/workspace/stackchan-lab/bridge/src/spokenReply/SpokenReplyPipeline.ts` | StackChan / 変更 | 現行固定返答pipeline | `AgentClient.createTurn()` をTTS前に呼ぶ |
| `/root/workspace/stackchan-lab/bridge/src/bridge/BridgeError.ts` | StackChan / 変更 | 構造化error code | agent runtimeのHTTP/timeout/unreachable |
| `/root/workspace/stackchan-lab/bridge/src/main.ts` | StackChan / 変更 | dependency composition root | `AgentClient` を生成・注入 |
| `/root/workspace/stackchan-lab/bridge/package.json` | StackChan / 変更 | TypeScript build | Node test runner + `tsx` のtest command |
| `/root/workspace/stackchan-lab/bridge/config.example.yaml` | StackChan / 変更 | bridge設定例 | `agent_runtime` を追加、token実値は置かない |
| `/root/workspace/stackchan-lab/docs/audio-pipeline.md` | StackChan / 変更 | 音声E2E仕様 | 固定返答からagent runtime応答へ更新 |
| `/root/workspace/stackchan-lab/docs/architecture.md` | StackChan / 変更 | 責務境界 | AgentClientとEgoPulse非依存境界を反映 |
| `/root/workspace/stackchan-lab/docs/bridge-api.md` | StackChan / 変更 | bridge config/status仕様 | `agent_runtime` と状態・errorを追加 |
| `/root/workspace/stackchan-lab/docs/conversation-pipeline-plan.md` | StackChan / 変更またはarchived | 旧固定返答Plan | 実装済み旧Planとして扱いを明確化 |

## テストリスト / 不安リスト

| ID | 観点 | 期待する振る舞い | 優先 | 対応するCycle | 状態 / 今回対象外理由 |
| -- | -- | -- | -- | -- | -- |
| T1 | EgoPulse設定正常系 | `channels.voice` の有効化、token、default surface/session、allowlistを正規化して取得できる | High | Step 1 | 未着手 |
| T2 | EgoPulse設定異常系 | Voice有効時にauth tokenが未設定、またはWeb channelが無効なら起動前に設定エラーになる | High | Step 2, Step 2A | 未着手 |
| T3 | EgoPulse認証 | Voice APIはVoice tokenだけを受理し、Web tokenやtokenなしを拒否する | High | Step 3, Step 4 | 未着手 |
| T4 | Voice identity | requestのsurface/session/user/agentを安定した `SurfaceContext` へ正規化し、区切り文字衝突を拒否する | High | Step 5, Step 5A | 未着手 |
| T5 | Voice入力検証 | 空textと許可されていないsurfaceをagent loop実行前に拒否する | High | Step 6, Step 7 | 未着手 |
| T6 | EgoPulse turn正常系 | Voice APIが既存agent loopを実行し、応答・trace・正規化identityを返し、voice sessionへ履歴を保存する | High | Step 8 | 未着手 |
| T7 | Voice adapter | `voice` chat typeがPrivate channelとしてregistryに登録される | Medium | Step 9 | 未着手 |
| T8 | StackChan設定正常系 | `agent_runtime` のendpoint/token/identity/timeoutをbridge configとして読み込める | High | Step 10 | 未着手 |
| T9 | StackChan設定異常系 | spoken reply有効時に必須agent runtime設定が欠ける、またはendpoint/timeoutが不正なら起動前に失敗する | High | Step 11, Step 12 | 未着手 |
| T10 | AgentClient正常系 | 正しいBearer tokenとturn payloadを送信し、agent runtime応答を返す | High | Step 13 | 未着手 |
| T11 | AgentClient異常系 | 非2xx、timeout、到達不能、壊れた/空responseを区別可能なBridgeErrorへ変換する | High | Step 14, Step 15, Step 16 | 未着手 |
| T12 | Pipeline正常系 | STT textをagent runtimeへ渡し、その応答だけをTTS化してdeviceで再生する | High | Step 17 | 未着手 |
| T13 | Pipeline失敗系 | agent runtime失敗・空応答時はTTS/再生せず、busyを解除してstatusへerrorを残す | High | Step 18, Step 19 | 未着手 |
| T14 | Pipeline既存制御 | disabled/source filter/busy/cooldownの既存挙動がAgentClient導入後も維持される | Medium | Step 20 | 未着手 |
| T15 | E2E契約 | 実サービス間でWake由来STTがVoice sessionへ入り、agent応答がAivis TTS経由でStackChan再生される | High | Step 22 | 手動・実機確認 |
| T16 | Voice単独起動 | `channels.web.enabled=false` でもVoice APIだけでHTTP serverが起動する | Low | 今回対象外 | listener lifecycleと共通server設定の再設計になるため。今回の実装ではVoice有効かつWeb無効をconfig errorとして拒否する |
| T17 | Outbound voice | Pulseやagent_sendからbridgeへ自発的に発話配送できる | Low | 今回対象外 | 同期inbound turn完成後の別仕様 |
| T18 | Streaming/barge-in | partial response、音声streaming、割り込み発話に対応する | Low | 今回対象外 | transport・再生状態設計を別途要する |

---

## Step 0: EgoPulse Worktree・StackChan Branch 作成

2リポジトリは独立している。EgoPulseではworktreeとbranchを作成し、StackChanではworktreeを作らず既存作業ツリーでbranchだけを作成する。

- EgoPulse
  - 元Repo: `/root/workspace/egopulse`
  - Worktree: `/root/workspace/egopulse/wt-voice-channel`
  - ブランチ名: `feat/voice-channel`
  - 作成コマンド:
    - `git -C /root/workspace/egopulse worktree add /root/workspace/egopulse/wt-voice-channel -b feat/voice-channel main`
- StackChan
  - 作業ディレクトリ: `/root/workspace/stackchan-lab`
  - ブランチ名: `feat/agent-runtime`
  - 切替前確認:
    - `git -C /root/workspace/stackchan-lab status --short`
    - `git -C /root/workspace/stackchan-lab branch --show-current`
  - 作成・切替コマンド:
    - `git -C /root/workspace/stackchan-lab switch -c feat/agent-runtime`

EgoPulseの実装コマンドは必ず `/root/workspace/egopulse/wt-voice-channel` で実行する。StackChanは `/root/workspace/stackchan-lab` で作業し、branch切替前から存在した変更・未追跡ファイルを保持したまま進める。StackChanのコミットでは `git add <今回変更したpath>` のように対象を明示し、既存差分を一括stageしない。

---

## Step 1: EgoPulse Config TDD Cycle - Voice設定の正常読み込み

### この Step の目的

`channels.voice` を設定モデルへ追加し、Voice APIが必要とする値を一貫して解決できるようにする。

### 今回選ぶ項目

- 対象: `T1`
- 選ぶ理由: API実装前に設定契約を確定する最小の入口だから
- この時点では扱わないこと: token欠落、HTTP route、turn実行

### RED: 失敗する自動テストを書く

- 追加するテスト名: `voice_channel_config_resolves_declared_values`
- Given: enabled/token/default_surface/default_session/allowed_surfacesを持つYAML
- When: 既存config loaderで読み込む
- Then: Voice用accessorが正規化済み値を返す
- 失敗理由の想定: `ChannelConfig` とVoice accessorが未実装

### GREEN: 最小実装

`ChannelConfig`、file config正規化、Voice用accessorへ必要フィールドだけを追加する。汎用channel設定の意味を壊す場合はVoice固有structへ寄せる。

### REFACTOR: 設計の整理

- 重複: Web auth token解決とSecretRef処理の既存経路を再利用できているか
- 命名: `voice_*` accessorが既存 `web_*` と整合するか
- 責務: loaderとruntime accessorが混ざっていないか
- テストの構造的結合: YAML入力と公開される解決結果だけを検証しているか
- 次の項目へ進める身軽さ: token必須validationを独立追加できるか

### テストリスト更新

- 完了: `T1` の正常系
- 追加: 実装中に発見した境界があれば追記
- 次候補: `T2`

### コミット

`feat: add voice channel configuration`

---

## Step 2: EgoPulse Config TDD Cycle - Voice token必須化

### この Step の目的

Voice channelを有効化した状態で認証なしに起動できないことを保証する。

### 今回選ぶ項目

- 対象: `T2`
- 選ぶ理由: route公開前にsecurity invariantを設定層で固定するため
- この時点では扱わないこと: Bearer header検証、Web tokenとの比較

### RED: 失敗する自動テストを書く

- 追加するテスト名: `voice_channel_enabled_requires_auth_token`
- Given: `channels.voice.enabled=true` かつtokenなしのYAML
- When: configをloadする
- Then: Voice auth token不足を示す構造化ConfigErrorになる
- 失敗理由の想定: Voice用validationが未実装

### GREEN: 最小実装

Web token必須validationと同じload時点で、Voice有効時のtoken必須条件を追加する。Web tokenへのfallbackは作らない。

### REFACTOR: 設計の整理

- 重複: channel auth必須validationを過剰に一般化せず読みやすく保てているか
- 命名: error variantがVoice設定不足を明示するか
- 責務: auth tokenの値をerror/logへ出していないか
- テストの構造的結合: errorの外部契約を検証しているか
- 次の項目へ進める身軽さ: middlewareへtoken accessorを渡せるか

### テストリスト更新

- 完了: `T2` のtoken必須条件
- 追加: なし
- 次候補: `T2` のWeb有効条件

### コミット

Step 1のコミットへ含めるか、独立性が高ければ `feat: validate voice channel authentication`

---

## Step 2A: EgoPulse Config TDD Cycle - 既存Web listener依存の検証

### この Step の目的

Voice channelを有効と設定したのに既存HTTP serverが起動せず、APIが存在しない状態を起動前に検出する。

### 今回選ぶ項目

- 対象: `T2`
- 選ぶ理由: 既存listenerへ同居する方針を、黙って動かない設定にしないため
- この時点では扱わないこと: Voice単独listener、共通server設定への移行

### RED: 失敗する自動テストを書く

- 追加するテスト名: `voice_channel_enabled_requires_web_channel`
- Given: Voice tokenあり、`channels.voice.enabled=true`、`channels.web.enabled=false`
- When: configをloadする
- Then: Voice APIには既存Web channelが必要だと分かる構造化ConfigErrorになる
- 失敗理由の想定: listener起動条件とVoice設定の整合validationが未実装

### GREEN: 最小実装

config load時にVoice有効かつWeb無効の組み合わせを拒否する。`start_channels()` の起動条件変更や別listener追加は行わない。

### REFACTOR: 設計の整理

- 重複: channel間依存validationがloader内で読みやすくまとまっているか
- 命名: errorがWeb tokenではなくWeb channel有効化を要求していると分かるか
- 責務: runtimeで同じ条件を重複判定していないか
- テストの構造的結合: config入力と起動前errorだけを検証しているか
- 次の項目へ進める身軽さ: auth middleware実装へ進めるか

### テストリスト更新

- 完了: `T2`
- 追加: なし
- 次候補: `T3`

### コミット

Step 1のコミットへ含める。

---

## Step 3: EgoPulse Auth TDD Cycle - 正しいVoice tokenを受理

### この Step の目的

既存HTTP listener上でVoice route専用Bearer認証を成立させる。

### 今回選ぶ項目

- 対象: `T3`
- 選ぶ理由: handler実装より先にroute境界の認証を固定するため
- この時点では扱わないこと: 不正token、turn payload、agent loop

### RED: 失敗する自動テストを書く

- 追加するテスト名: `voice_auth_accepts_configured_voice_token`
- Given: Web tokenと異なるVoice tokenを持つAppStateと保護されたtest route
- When: Voice tokenをBearer headerで送る
- Then: downstream handlerへ到達する
- 失敗理由の想定: Voice専用middlewareが未実装

### GREEN: 最小実装

既存のconstant-time Bearer比較を再利用し、Voice token accessorだけを見るmiddlewareを追加する。比較helperの可視性変更は必要最小限にする。

### REFACTOR: 設計の整理

- 重複: Web/Voice middlewareでtoken抽出と比較が重複しすぎていないか
- 命名: WebとVoiceの認証責務が混同されないか
- 責務: middlewareがroute business logicを持っていないか
- テストの構造的結合: middlewareのHTTP結果を検証しているか
- 次の項目へ進める身軽さ: reject caseを独立テストできるか

### テストリスト更新

- 完了: `T3` の受理側
- 追加: なし
- 次候補: `T3` の拒否側

### コミット

`feat: add voice API authentication`

---

## Step 4: EgoPulse Auth TDD Cycle - Web tokenと未認証を拒否

### この Step の目的

同一listenerでもWeb APIとVoice APIのcredential境界が分離されることを保証する。

### 今回選ぶ項目

- 対象: `T3`
- 選ぶ理由: 同居routeで最も重要なsecurity回帰だから
- この時点では扱わないこと: allowlist、agent turn

### RED: 失敗する自動テストを書く

- 追加するテスト名: `voice_auth_rejects_web_token`
- Given: 異なるWeb tokenとVoice token
- When: Voice routeへWeb tokenを送る
- Then: `401 unauthorized` となりdownstreamへ到達しない
- 失敗理由の想定: middlewareがWeb tokenを参照またはfallbackしている

### GREEN: 最小実装

Voice middlewareがVoice tokenのみを見るよう修正する。tokenなしも同じ401契約になることを既存helperで確認し、別の分岐を増やさない。

### REFACTOR: 設計の整理

- 重複: unauthorized response生成の共有範囲が適切か
- 命名: error messageがsecretやtoken値を含まないか
- 責務: Web middlewareの既存挙動を変えていないか
- テストの構造的結合: HTTP status/error codeを検証しているか
- 次の項目へ進める身軽さ: Voice handlerをroute groupへ追加できるか

### テストリスト更新

- 完了: `T3`
- 追加: tokenなしの専用テストが必要と判明した場合は追加
- 次候補: `T4`

### コミット

Step 3のコミットへ含める。

---

## Step 5: EgoPulse Voice TDD Cycle - Surface identity正規化

### この Step の目的

複数の音声入力面を衝突させず、既存session modelへ渡せる `SurfaceContext` を生成する。

### 今回選ぶ項目

- 対象: `T4`
- 選ぶ理由: Voice APIの中心的domain ruleでありHTTPやLLMから独立して検証できるため
- この時点では扱わないこと: request rejection、process_turn、DB保存

### RED: 失敗する自動テストを書く

- 追加するテスト名: `voice_request_builds_stable_surface_context`
- Given: surface=`stackchan`, session=`main`, user=`local-speaker`, agent=`default`
- When: Voice requestをcontextへ変換する
- Then: channel/chat_type=`voice`、surface_thread=`stackchan:main`、指定user/agentになる
- 失敗理由の想定: Voice request/context変換が未実装

### GREEN: 最小実装

Voice request DTOと、trim・default適用・`surface_thread` 組み立てを行う小さな正規化処理を追加する。

### REFACTOR: 設計の整理

- 重複: `SurfaceContext::new()` を利用できているか
- 命名: `surface`, `session_key`, `source` の意味が混ざっていないか
- 責務: HTTP response生成とidentity生成が分離されているか
- テストの構造的結合: private関数ではなく入力からcontext結果を検証しているか
- 次の項目へ進める身軽さ: validationを前段へ追加できるか

### テストリスト更新

- 完了: `T4` の正常なidentity
- 追加: なし
- 次候補: `T4` の区切り文字拒否

### コミット

`feat: add voice turn request model`

---

## Step 5A: EgoPulse Voice TDD Cycle - Identity区切り文字の拒否

### この Step の目的

異なる `surface` / `session_key` の組み合わせが同じ `surface_thread` に潰れ、会話履歴が混線することを防ぐ。

### 今回選ぶ項目

- 対象: `T4`
- 選ぶ理由: `"{surface}:{session_key}"` を永続identityとして使うため、連結前に曖昧性を排除する必要がある
- この時点では扱わないこと: エスケープ、URL encoding、可逆な複合キー形式

### RED: 失敗する自動テストを書く

- 追加するテスト名: `voice_turn_rejects_identity_components_with_delimiter`
- Given: `surface="stack:chan"` または `session_key="room:main"` の認証済みrequest
- When: Voice handlerへ送る
- Then: `400 invalid_params` となりagent loopとDBへ到達しない
- 失敗理由の想定: identity成分を無検証で `:` 連結している

### GREEN: 最小実装

trim後のsurface/session keyについて、空文字と `:` をrequest validationで拒否する。default値にも同じvalidationを適用する。

### REFACTOR: 設計の整理

- 重複: surface/sessionのvalidationを一箇所で共有できているか
- 命名: machine-readable identity componentの制約だと分かるか
- 責務: `SurfaceContext::session_key()` の全チャネル仕様を変更していないか
- テストの構造的結合: collisionを生む外部入力が拒否されることを検証しているか
- 次の項目へ進める身軽さ: text/allowlist validationへ進めるか

### テストリスト更新

- 完了: `T4`
- 追加: control character制約が必要と判明した場合は別項目へ追加
- 次候補: `T5`

### コミット

Step 5のコミットへ含める。

---

## Step 6: EgoPulse Voice TDD Cycle - 空text拒否

### この Step の目的

意味のないturnをagent loopやDBへ流さない。

### 今回選ぶ項目

- 対象: `T5`
- 選ぶ理由: 最小のrequest validationであり副作用防止に直結するため
- この時点では扱わないこと: surface allowlist、正常turn

### RED: 失敗する自動テストを書く

- 追加するテスト名: `voice_turn_rejects_blank_text`
- Given: whitespaceだけのtextを持つ認証済みrequest
- When: Voice handlerへ送る
- Then: `400 invalid_params` となりagent loopを実行しない
- 失敗理由の想定: handler/validation未実装

### GREEN: 最小実装

trim後の空textをhandler入口で拒否し、仕様どおりのJSON errorを返す。

### REFACTOR: 設計の整理

- 重複: Web API error形式との整合を保てているか
- 命名: clientが原因を判別できるerror codeか
- 責務: validationがagent loop内部へ漏れていないか
- テストの構造的結合: HTTP契約を検証しているか
- 次の項目へ進める身軽さ: allowlist判定を追加できるか

### テストリスト更新

- 完了: `T5` の空text
- 追加: なし
- 次候補: `T5` のsurface拒否

### コミット

Step 5のコミットへ含める。

---

## Step 7: EgoPulse Voice TDD Cycle - Surface allowlist拒否

### この Step の目的

設定されたvoice surface以外からのturnをagent loop実行前に拒否する。

### 今回選ぶ項目

- 対象: `T5`
- 選ぶ理由: 認証済みclient内の誤設定・想定外surfaceを明確に検出するため
- この時点では扱わないこと: 正常turn、metadata利用

### RED: 失敗する自動テストを書く

- 追加するテスト名: `voice_turn_rejects_surface_outside_allowlist`
- Given: allowed=`stackchan`、request surface=`desk-mic`
- When: Voice handlerへ送る
- Then: `403 surface_not_allowed` となる
- 失敗理由の想定: allowlist判定未実装

### GREEN: 最小実装

空allowlistは全許可、非空時は正規化surfaceの完全一致だけを許可する。

### REFACTOR: 設計の整理

- 重複: config accessor側で不要な再正規化をしていないか
- 命名: allowlistが認証の代替に見えないか
- 責務: sourceを認可に使っていないか
- テストの構造的結合: request/resultだけを検証しているか
- 次の項目へ進める身軽さ: process_turnを接続できるか

### テストリスト更新

- 完了: `T5`
- 追加: なし
- 次候補: `T6`

### コミット

Step 5のコミットへ含める。

---

## Step 8: EgoPulse Voice TDD Cycle - Agent turnと履歴永続化

### この Step の目的

Voice requestを既存agent loopへ接続し、通常のEgoPulse sessionとして応答と履歴を得る。

### 今回選ぶ項目

- 対象: `T6`
- 選ぶ理由: EgoPulse側機能の主価値を完成させる統合Cycleだから
- この時点では扱わないこと: StackChan client、実LLM、音声処理

### RED: 失敗する自動テストを書く

- 追加するテスト名: `voice_turn_returns_agent_response_and_persists_voice_session`
- Given: deterministic fake LLM、temporary DB、認証済みVoice request
- When: `/api/voice/turn` を呼ぶ
- Then: response text、surface/session/surface_thread/agent_id/trace_idが返り、`channel=voice` のuser/assistant履歴が保存される
- 失敗理由の想定: handlerが `process_turn()` と接続されていない

### GREEN: 最小実装

正規化した `SurfaceContext` とtextを既存 `process_turn()` へ渡し、結果を同期JSON responseへ変換する。trace_idはturnとresponseで同じ値を使う。

### REFACTOR: 設計の整理

- 重複: Web stream handlerのturn準備と無理に共通化していないか
- 命名: Voice API responseがagent内部型を漏らしていないか
- 責務: session永続化をVoice handlerが再実装していないか
- テストの構造的結合: fake LLMとDBを使い外部挙動を検証しているか
- 次の項目へ進める身軽さ: adapter登録を独立確認できるか

### テストリスト更新

- 完了: `T6`
- 追加: turn failureのHTTP mappingに新たな不安があれば追加
- 次候補: `T7`

### コミット

`feat: expose authenticated voice turn API`

---

## Step 9: EgoPulse Channel TDD Cycle - VoiceAdapter登録

### この Step の目的

Voice sessionをEgoPulseの既存channel modelへ正式に登録する。

### 今回選ぶ項目

- 対象: `T7`
- 選ぶ理由: channel identityと将来のoutbound拡張点を既存抽象へ揃えるため
- この時点では扱わないこと: outbound HTTP delivery、Pulse発話

### RED: 失敗する自動テストを書く

- 追加するテスト名: `voice_adapter_registers_private_voice_route`
- Given: `VoiceAdapter` を登録した `ChannelRegistry`
- When: adapter名とrouteを参照する
- Then: name=`voice`、route=`("voice", Private)` である
- 失敗理由の想定: VoiceAdapter未実装・未登録

### GREEN: 最小実装

初期 `send_text()` はno-opの `VoiceAdapter` を追加し、`build_app_state()` で登録する。outbound設定やcallbackは追加しない。

### REFACTOR: 設計の整理

- 重複: WebAdapterと同程度の最小実装になっているか
- 命名: API handlerとChannelAdapterの責務が分かれているか
- 責務: no-op送信が将来機能を先回りしていないか
- テストの構造的結合: trait契約だけを検証しているか
- 次の項目へ進める身軽さ: EgoPulse docsと全体検証へ進めるか

### テストリスト更新

- 完了: `T7`
- 追加: なし
- 次候補: `T8`

### コミット

Step 8のコミットへ含める。

---

## Step 10: StackChan Config TDD Cycle - Agent runtime設定の正常読み込み

### この Step の目的

StackChan bridgeが接続先agent runtimeとvoice surface identityを設定から取得できるようにする。

### 今回選ぶ項目

- 対象: `T8`
- 選ぶ理由: AgentClient実装前にbridge側の接続契約を固定するため
- この時点では扱わないこと: 必須validation、HTTP呼び出し、pipeline

### RED: 失敗する自動テストを書く

- 追加するテスト名: `loadConfig_reads_agent_runtime_settings`
- Given: endpoint/token/agent_id/surface/session_key/user_id/timeout_msを持つtemporary YAML
- When: `loadConfig()` を呼ぶ
- Then: camelCaseの `BridgeConfig.agentRuntime` として値を取得できる
- 失敗理由の想定: test commandとagent runtime configが未実装

### GREEN: 最小実装

`tsx --test` を使うtest scriptを追加し、`RawConfig`、`BridgeConfig`、loaderへ `agent_runtime` を追加する。endpointは完全URLとして正規化し、base URLとの二重組み立てを避ける。

### REFACTOR: 設計の整理

- 重複: string/number validation helperを再利用できているか
- 命名: YAML=`agent_runtime`、TypeScript=`agentRuntime` が一貫するか
- 責務: EgoPulse固有フィールドを型へ入れていないか
- テストの構造的結合: YAMLから解決configまでを検証しているか
- 次の項目へ進める身軽さ: 必須validationを独立追加できるか

### テストリスト更新

- 完了: `T8`
- 追加: なし
- 次候補: `T9`

### コミット

`feat: add agent runtime configuration`

---

## Step 11: StackChan Config TDD Cycle - Spoken reply有効時の必須設定

### この Step の目的

固定返答fallbackなしで、起動後に初めて接続設定不足へ気づく状態を防ぐ。

### 今回選ぶ項目

- 対象: `T9`
- 選ぶ理由: 本番起動時のfail-fast条件だから
- この時点では扱わないこと: URL形式、timeout境界、HTTP errors

### RED: 失敗する自動テストを書く

- 追加するテスト名: `loadConfig_requires_agent_runtime_when_spoken_reply_enabled`
- Given: `spoken_reply.enabled=true` かつagent runtime endpoint/tokenなし
- When: configをloadする
- Then: `CONFIG_ERROR` となり不足fieldを示す
- 失敗理由の想定: conditional validation未実装

### GREEN: 最小実装

spoken reply有効時だけendpoint/tokenを必須化する。無効時はbridgeの他機能をagent runtimeなしで起動可能に保つ。

### REFACTOR: 設計の整理

- 重複: conditional required validationが読みやすいか
- 命名: errorが `agent_runtime.*` を正確に指すか
- 責務: AgentClient constructorで設定不足を再検証していないか
- テストの構造的結合: 起動前config contractを検証しているか
- 次の項目へ進める身軽さ: URL/timeout検証を追加できるか

### テストリスト更新

- 完了: `T9` の必須条件
- 追加: なし
- 次候補: `T9` の値境界

### コミット

Step 10のコミットへ含める。

---

## Step 12: StackChan Config TDD Cycle - Endpointとtimeout境界

### この Step の目的

不正なagent runtime接続設定をHTTP実行前に拒否する。

### 今回選ぶ項目

- 対象: `T9`
- 選ぶ理由: AgentClientのerrorとconfig errorを混同しないため
- この時点では扱わないこと: 実際のfetch、response parsing

### RED: 失敗する自動テストを書く

- 追加するテスト名: `loadConfig_rejects_invalid_agent_runtime_endpoint`
- Given: HTTP/HTTPS absolute URLではないendpoint
- When: configをloadする
- Then: `CONFIG_ERROR` となる
- 失敗理由の想定: URL validation未実装

### GREEN: 最小実装

標準 `URL` parserでabsolute HTTP/HTTPS endpointを検証し、timeoutは正の有限値に制限する。

### REFACTOR: 設計の整理

- 重複: `normalizeBaseUrl` と目的が異なることが明確か
- 命名: endpointが `/api/voice/turn` を含む完全URLであると読めるか
- 責務: provider固有pathをclientでハードコードしていないか
- テストの構造的結合: 入力値とConfigErrorだけを検証しているか
- 次の項目へ進める身軽さ: AgentClientを設定値だけで構築できるか

### テストリスト更新

- 完了: `T9`
- 追加: timeout=0の専用testが必要なら同項目の追加Cycle
- 次候補: `T10`

### コミット

Step 10のコミットへ含める。

---

## Step 13: StackChan AgentClient TDD Cycle - 正常なturn呼び出し

### この Step の目的

bridgeからagent runtimeへ仕様どおりの認証済みturn requestを送り、応答を取得する。

### 今回選ぶ項目

- 対象: `T10`
- 選ぶ理由: AgentClientの最小価値を外部HTTP契約で固定するため
- この時点では扱わないこと: timeout、非2xx、pipeline

### RED: 失敗する自動テストを書く

- 追加するテスト名: `AgentClient_sends_turn_and_returns_response`
- Given: requestを記録して成功JSONを返すlocal test server
- When: `createTurn()` にSTT text/sourceを渡す
- Then: configured endpointへBearer token、agent/surface/session/user/source/textを送り、response/traceIdを返す
- 失敗理由の想定: `AgentClient` 未実装

### GREEN: 最小実装

AbortController付きfetch clientを追加し、成功responseを `AgentTurnResult` へ変換する。EgoPulseという名前や固定pathをclientへ入れない。

### REFACTOR: 設計の整理

- 重複: `VoiceGatewayClient` のtimeout/error patternと形を揃えられるか
- 命名: `AgentTurnInput` / `AgentTurnResult` がHTTP DTOと責務を混同していないか
- 責務: pipelineやTTSをclientへ入れていないか
- テストの構造的結合: local HTTP serverでwire contractを検証しているか
- 次の項目へ進める身軽さ: errorsを1種類ずつ追加できるか

### テストリスト更新

- 完了: `T10`
- 追加: なし
- 次候補: `T11` 非2xx

### コミット

`feat: add agent runtime client`

---

## Step 14: StackChan AgentClient TDD Cycle - 非2xx応答

### この Step の目的

agent runtimeが返す認証・validation・内部失敗をbridgeの構造化errorへ変換する。

### 今回選ぶ項目

- 対象: `T11`
- 選ぶ理由: HTTP接続時に最頻出の失敗境界だから
- この時点では扱わないこと: timeout、network failure、壊れた成功JSON

### RED: 失敗する自動テストを書く

- 追加するテスト名: `AgentClient_maps_non_success_response_to_bridge_error`
- Given: 401または500を返すlocal test server
- When: `createTurn()` を呼ぶ
- Then: statusと安全なresponse detailを持つ `AGENT_RUNTIME_ERROR` になる
- 失敗理由の想定: 非2xx mapping未実装

### GREEN: 最小実装

非2xx bodyを安全に読み、tokenを含めずBridgeErrorへ変換する。HTTP statusごとの過剰なerror code分割はしない。

### REFACTOR: 設計の整理

- 重複: error body読取がVoiceGatewayClientと共有すべき実質的重複か
- 命名: provider名ではなくagent runtime境界を表しているか
- 責務: retryを勝手に追加していないか
- テストの構造的結合: error code/statusだけを検証しているか
- 次の項目へ進める身軽さ: timeoutを独立追加できるか

### テストリスト更新

- 完了: `T11` の非2xx
- 追加: なし
- 次候補: `T11` timeout

### コミット

Step 13のコミットへ含める。

---

## Step 15: StackChan AgentClient TDD Cycle - Timeout

### この Step の目的

長時間応答しないagent runtimeでpipelineが永久にbusyにならないようにする。

### 今回選ぶ項目

- 対象: `T11`
- 選ぶ理由: LLM/tool実行を含む接続では明示的timeoutが必須だから
- この時点では扱わないこと: network refused、invalid JSON

### RED: 失敗する自動テストを書く

- 追加するテスト名: `AgentClient_aborts_after_configured_timeout`
- Given: timeoutより長く応答しないlocal test server
- When: `createTurn()` を呼ぶ
- Then: `AGENT_RUNTIME_TIMEOUT` になる
- 失敗理由の想定: AbortController処理未実装またはerror mapping不足

### GREEN: 最小実装

設定timeoutでfetchをabortし、timerを必ずclearして専用BridgeErrorへ変換する。

### REFACTOR: 設計の整理

- 重複: timeout lifecycleがVoiceGatewayClientと一貫しているか
- 命名: timeoutとunreachableを区別しているか
- 責務: pipeline側で二重timeoutを持っていないか
- テストの構造的結合: 経過時間ではなくerror contractを主に検証しているか
- 次の項目へ進める身軽さ: malformed responseを追加できるか

### テストリスト更新

- 完了: `T11` のtimeout
- 追加: なし
- 次候補: `T11` response validation

### コミット

Step 13のコミットへ含める。

---

## Step 16: StackChan AgentClient TDD Cycle - Response validation

### この Step の目的

HTTP 200でも応答契約を満たさないagent runtimeから不正なtextをTTSへ流さない。

### 今回選ぶ項目

- 対象: `T11`
- 選ぶ理由: 外部service境界では成功statusだけを信頼できないため
- この時点では扱わないこと: schema library導入、fallback返答

### RED: 失敗する自動テストを書く

- 追加するテスト名: `AgentClient_rejects_success_response_without_non_empty_response`
- Given: `200 {"ok":true,"response":"   "}` を返すserver
- When: `createTurn()` を呼ぶ
- Then: `AGENT_RUNTIME_INVALID_RESPONSE` になる
- 失敗理由の想定: response shape validation未実装

### GREEN: 最小実装

標準JSON parseと明示的型guardで `ok`, non-empty `response`, optional `trace_id` を検証する。新規schema libraryは追加しない。

### REFACTOR: 設計の整理

- 重複: HTTP DTO validationがclient内に閉じているか
- 命名: AgentTurnResultが検証済み値だけを表すか
- 責務: 空応答をpipeline fallbackへ変換していないか
- テストの構造的結合: public `createTurn()` のerrorだけを検証しているか
- 次の項目へ進める身軽さ: pipelineへ安全なresultを渡せるか

### テストリスト更新

- 完了: `T11`
- 追加: network refusedを専用testにする必要があれば同項目へ追加
- 次候補: `T12`

### コミット

Step 13のコミットへ含める。

---

## Step 17: StackChan Pipeline TDD Cycle - Agent応答をTTS再生

### この Step の目的

固定文を廃止し、STT入力に対するagent runtimeの応答を音声化して再生する。

### 今回選ぶ項目

- 対象: `T12`
- 選ぶ理由: StackChan側の中心的な振る舞いだから
- この時点では扱わないこと: AgentClient失敗、空応答、既存guard回帰

### RED: 失敗する自動テストを書く

- 追加するテスト名: `SpokenReplyPipeline_speaks_agent_response`
- Given: STT textを記録するfake AgentClient、応答を記録するfake TTS、WAVを記録するfake transport
- When: listen sourceのtranscriptionをhandleする
- Then: STT textがagentへ渡り、agent応答だけがTTSへ渡り、返されたWAVが1回再生される
- 失敗理由の想定: pipelineが固定 `buildReplyText()` を使用している

### GREEN: 最小実装

`AgentClient` をcomposition rootで生成してpipelineへ注入し、固定文関数を削除する。依存型は必要なpublic methodだけを表す最小contractにし、provider registryは作らない。

### REFACTOR: 設計の整理

- 重複: Pipeline内の順序がagent -> TTS -> playbackとして読みやすいか
- 命名: `lastInput` と `lastReply` がSTT/agent応答を正しく表すか
- 責務: AgentClientがpipeline stateを知らないか
- テストの構造的結合: fake依存による外部呼出順と値を検証しているか
- 次の項目へ進める身軽さ: failureを1地点ずつ追加できるか

### テストリスト更新

- 完了: `T12`
- 追加: なし
- 次候補: `T13` agent failure

### コミット

`feat: generate spoken replies with agent runtime`

---

## Step 18: StackChan Pipeline TDD Cycle - Agent runtime失敗

### この Step の目的

agent runtime失敗時に誤った音声を出さず、pipelineを次の発話へ復帰させる。

### 今回選ぶ項目

- 対象: `T13`
- 選ぶ理由: 外部agent runtime導入で新しく増える主要障害点だから
- この時点では扱わないこと: 空response、TTS/playback既存error

### RED: 失敗する自動テストを書く

- 追加するテスト名: `SpokenReplyPipeline_records_agent_error_without_speaking`
- Given: rejectするfake AgentClient
- When: transcriptionをhandleする
- Then: TTS/playbackは呼ばれず、busy=false、lastErrorにagent failureが残る
- 失敗理由の想定: pipeline error handlingがagent call前提になっていない

### GREEN: 最小実装

既存try/finally境界内にagent callを置き、失敗時は現在のstatus error契約へ記録する。固定fallback文は追加しない。

### REFACTOR: 設計の整理

- 重複: error記録を障害地点ごとに重複させていないか
- 命名: error messageがtokenやresponse bodyを過剰露出しないか
- 責務: retry policyをpipelineへ先回り追加していないか
- テストの構造的結合: observable statusと依存未呼出を検証しているか
- 次の項目へ進める身軽さ: empty responseを独立確認できるか

### テストリスト更新

- 完了: `T13` のagent failure
- 追加: なし
- 次候補: `T13` empty response

### コミット

Step 17のコミットへ含める。

---

## Step 19: StackChan Pipeline TDD Cycle - 空応答を再生しない

### この Step の目的

防御的に、AgentClient contract外の空応答がpipelineへ入ってもTTSしない。

### 今回選ぶ項目

- 対象: `T13`
- 選ぶ理由: 外部境界とpipeline境界の両方で無音・不正入力を止めるため
- この時点では扱わないこと: fallback文、再試行

### RED: 失敗する自動テストを書く

- 追加するテスト名: `SpokenReplyPipeline_rejects_blank_agent_response`
- Given: blank responseを返すtest double
- When: transcriptionをhandleする
- Then: TTS/playbackせず、busyを解除してlastErrorへ残す
- 失敗理由の想定: pipelineがresponseを無条件にTTSへ渡す

### GREEN: 最小実装

pipeline境界でnon-empty応答をassertし、構造化BridgeErrorまたは明確なErrorとして既存catchへ流す。

### REFACTOR: 設計の整理

- 重複: AgentClient validationとの二重防御が簡潔に保たれているか
- 命名: 空応答を「成功」と記録していないか
- 責務: fallback policyを混ぜていないか
- テストの構造的結合: downstream未呼出とstatusを検証しているか
- 次の項目へ進める身軽さ: 既存guard regressionへ進めるか

### テストリスト更新

- 完了: `T13`
- 追加: なし
- 次候補: `T14`

### コミット

Step 17のコミットへ含める。

---

## Step 20: StackChan Pipeline TDD Cycle - 既存guard回帰防止

### この Step の目的

AgentClient導入によってdisabled/source filter/busy/cooldownが迂回されないことを保証する。

### 今回選ぶ項目

- 対象: `T14`
- 選ぶ理由: 既存の本番制御を維持する回帰確認だから
- この時点では扱わないこと: queue policy追加、並列turn

### RED: 失敗する自動テストを書く

- 追加するテスト名: `SpokenReplyPipeline_does_not_call_agent_for_ignored_source`
- Given: listenSources外のtranscriptionと呼出回数を記録するfake AgentClient
- When: handleする
- Then: AgentClient/TTS/playbackは呼ばれず、lastIgnored=`source_not_listened`
- 失敗理由の想定: refactorでagent callがguardより前へ移動している

### GREEN: 最小実装

既存guard順序をAgentClient呼出前に維持する。busy/cooldownの追加不安は同じT14の追加Cycleへ分ける。

### REFACTOR: 設計の整理

- 重複: guardが一箇所にまとまっているか
- 命名: ignored reasonが既存status契約と一致するか
- 責務: AgentClientがsource filterを持っていないか
- テストの構造的結合: 呼び出されないという外部挙動を検証しているか
- 次の項目へ進める身軽さ: 全StackChan testsへ進めるか

### テストリスト更新

- 完了: `T14` のsource filter
- 追加: busy/cooldownの既存自動testがなければ同項目のCycleを追加
- 次候補: `T15` の前にdocsと全体検証

### コミット

Step 17のコミットへ含める。

---

## Step 21: ドキュメントと設定例の同期

### この Step の目的

2リポジトリの運用者が、抽象境界と実際の起動設定を同じ理解で再現できるようにする。

### 今回選ぶ項目

- 対象: TDD項目の横断反映
- 選ぶ理由: 実装契約確定後にdocsとの差分を解消するため
- この時点では扱わないこと: Voice単独listener、outbound voice、streaming

### RED: 失敗する自動テストを書く

- 追加するテスト名: なし。このStepは実装済み契約の文書同期であり、直前までの自動テストをGreenに保つ。
- Given: 両Repoで確定したconfig/API/status/error契約
- When: 関連docsとexample configを照合する
- Then: 固定返答、`EgopulseClient`、`egopulse:` など旧表現が残らない
- 失敗理由の想定: docsと実装の命名・必須条件の不一致

### GREEN: 最小実装

EgoPulseでは `voice-channel.md`, `config.md`, `channels.md`, `api.md`, example configを更新し、Web channel有効前提とidentity成分の `:` 禁止を明記する。StackChanでは音声pipeline、architecture、bridge API、example configを更新し、`AgentClient` / `agent_runtime` に統一する。

### REFACTOR: 設計の整理

- 重複: 詳細仕様は `voice-channel.md` を正本とし、他docsは必要な参照に留める
- 命名: EgoPulse側=`voice`、StackChan側=`AgentClient`/`agent_runtime`
- 責務: voice-gatewayがagent runtimeを知らない記述になっているか
- テストの構造的結合: 該当なし
- 次の項目へ進める身軽さ: E2E手順がそのまま実行可能か

### テストリスト更新

- 完了: docs同期
- 追加: なし
- 次候補: `T15`

### コミット

- EgoPulse: `docs: document voice channel integration`
- StackChan: `docs: document agent runtime integration`

---

## Step 22: 動作確認

### EgoPulse worktree

Working directory: `/root/workspace/egopulse/wt-voice-channel`

- `cargo fmt --check`
- `cargo test`
- `cargo check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
- Voice APIのHTTP確認:
  - Voice tokenで成功する
  - tokenなし、Web tokenで401になる
  - allowlist外surfaceで403になる
  - `:` を含むsurface/session keyで400になる
  - 空textで400になる
  - responseに `surface_thread=stackchan:main` と `trace_id` が含まれる
- Config確認:
  - `channels.voice.enabled=true` かつ `channels.web.enabled=false` が起動前の設定エラーになる
- DB/履歴確認:
  - session一覧またはDB queryで `channel=voice` のsessionを確認
  - user発話とassistant応答が同じsessionへ保存される

失敗時は、設定ならStep 1-2A、認証ならStep 3-4、validationならStep 5-7、turn/DBならStep 8-9へ戻る。

### StackChan branch

Working directory: `/root/workspace/stackchan-lab/bridge`

- `npm ci`
- `npm test`
- `npm run build`
- `npm run smoke` は既存smoke testが実device状態を破壊しないことを確認してから実行

失敗時は、configならStep 10-12、HTTP clientならStep 13-16、pipelineならStep 17-20へ戻る。

### End-to-End / 実機確認

対象:

- EgoPulse: `/root/workspace/egopulse/wt-voice-channel`
- StackChan bridge: `/root/workspace/stackchan-lab/bridge`
- voice-gateway: `/root/workspace/voice-gateway`
- StackChan device: Wi-Fi経由

前提:

- EgoPulse既存HTTPサーバーが起動するよう `channels.web.enabled: true`
- EgoPulse `channels.voice.enabled: true`
- EgoPulse Voice tokenとStackChan `agent_runtime.auth_token` が一致
- StackChan `agent_runtime.endpoint` がEgoPulseの `/api/voice/turn`
- voice-gatewayでReazon STTとAivis TTSが正常
- stackchan-bridgeの `spoken_reply.enabled: true`

確認フロー:

1. EgoPulseを通常の `cargo run -- run` またはgateway相当で起動する。
2. voice-gatewayを通常設定で起動する。
3. stackchan-bridgeを通常の `npm run dev` またはbuild後 `npm start` で起動する。
4. StackChanのWake Wordから発話する。
5. Reazon STT結果がbridge `/stt/events` へ届く。
6. bridgeが `AgentClient` 経由でEgoPulse `/api/voice/turn` を呼ぶ。
7. EgoPulseが `channel=voice`, `surface_thread=stackchan:main` のturnを処理する。
8. agent応答textがAivis TTSへ渡る。
9. StackChanで応答音声が聞こえる。
10. bridge statusにagent runtime/TTS/playbackの成功または少なくとも各段階の最終状態が残る。
11. EgoPulseの履歴にSTT textとagent responseが保存されている。

受け入れ条件:

- 固定文 `はい、聞こえています。{STT text}` は使用されない。
- 実際のEgoPulse agent応答が音声として再生される。
- 別surface/sessionを指定したrequestは別Voice sessionとして保存される。
- agent runtime失敗時にTTS・再生せず、bridgeがbusyから復帰する。
- Voice APIはWeb tokenでは認証できない。

---

## Step 23: Plan・仕様書との自己チェック

実装完了後にこのPlanと関連仕様書を最初から読み直し、EgoPulseとStackChanの実装・自動テスト・文書が要求した振る舞いと一致しているかを照合する。未実装、過剰実装、テスト不足、リポジトリ間契約の不一致、仕様書との齟齬を見つけた場合は、該当するTDD Cycleへ戻って修正し、Step 22の動作確認を再実行してからこのStepを完了する。

- Planのテストリストと各Cycleが完了条件を満たしている。
- `/root/workspace/egopulse/docs/voice-channel.md` のWhatと両リポジトリの実装結果が一致している。
- EgoPulseの `/api/voice/turn` とStackChanの `AgentClient` で、request/response、認証、errorの契約が一致している。
- EgoPulseがStackChan固有知識を持たず、StackChanがEgoPulse固有実装へ依存していない。
- `voice`、`AgentClient`、`agent_runtime` の命名がコード、設定、テスト、docsで統一されている。
- 固定返答や `EgopulseClient` など、廃止した実装・命名・説明が残っていない。
- 実装中に変更した設計判断が両リポジトリの関連docsへ反映されている。
- 変更ファイル一覧、コミット分割、自動テスト一覧が実際の変更と一致している。
- EgoPulse worktreeとStackChan feature branchの差分に、元から存在する無関係な未コミット変更が混入していない。

---

## Step 24: PR 作成

2リポジトリで別々のPRを作る。EgoPulse PRを先に作成し、その契約を参照してStackChan PRを作成する。

### EgoPulse PR

- Repo: `endo-ly/egopulse`
- Branch: `feat/voice-channel`
- PRタイトル: `feat: add authenticated voice channel turns`
- PR description:
  - 概要: `voice` channel、専用認証、同期turn API、session identity
  - 設計境界: StackChanや音声Providerを知らない
  - 既存HTTP serverへroute追加し、listener構成は変更していない
  - テスト: fmt/test/check/clippy/doc、Voice API integration tests
  - 関連仕様: `docs/voice-channel.md`

### StackChan PR

- Repo: `endo-ly/stackchan-lab`
- Branch: `feat/agent-runtime`
- PRタイトル: `feat: connect spoken replies to an agent runtime`
- PR description:
  - 概要: `AgentClient`, `agent_runtime`, fixed reply removal
  - 設計境界: EgoPulse固有名・内部型へ依存しない
  - テスト: `npm test`, `npm run build`, E2E実機確認
  - 依存: EgoPulse側 `/api/voice/turn` 契約
  - EgoPulse PRへのリンク

PR作成後、各RepoでCI結果を確認し、EgoPulse側はCoderabbitレビューの致命的指摘を確認する。

---

## 変更ファイル一覧

実装中の責務整理で同等ファイルへ移動してよいが、変更意図は以下を維持する。

| ファイル | 変更種別 | 内容 |
| ---- | ---- | -- |
| `/root/workspace/egopulse/src/config/types.rs` | 変更 | Voice channel設定型 |
| `/root/workspace/egopulse/src/config/loader.rs` | 変更 | Voice設定正規化・必須validation |
| `/root/workspace/egopulse/src/config/resolve.rs` | 変更 | Voice設定accessor |
| `/root/workspace/egopulse/src/config/tests.rs` | 変更 | Voice config tests |
| `/root/workspace/egopulse/src/error.rs` | 変更候補 | Voice auth設定error |
| `/root/workspace/egopulse/src/channels/mod.rs` | 変更 | voice module登録 |
| `/root/workspace/egopulse/src/channels/voice.rs` | **新規** | Voice request/response、identity、handler、adapter |
| `/root/workspace/egopulse/src/channels/adapter.rs` | 変更候補 | testからroute確認に必要な最小API |
| `/root/workspace/egopulse/src/channels/web/auth.rs` | 変更 | Voice authで再利用するBearer比較 |
| `/root/workspace/egopulse/src/channels/web/mod.rs` | 変更 | Voice route group追加 |
| `/root/workspace/egopulse/src/runtime/mod.rs` | 変更 | VoiceAdapter登録 |
| `/root/workspace/egopulse/src/test_util.rs` | 変更 | Voice handler integration test用state |
| `/root/workspace/egopulse/egopulse.config.example.yaml` | 変更 | Voice設定例 |
| `/root/workspace/egopulse/docs/voice-channel.md` | 変更 | 実装結果との同期 |
| `/root/workspace/egopulse/docs/config.md` | 変更 | Voice設定仕様 |
| `/root/workspace/egopulse/docs/channels.md` | 変更 | Voice channel仕様 |
| `/root/workspace/egopulse/docs/api.md` | 変更 | Voice API仕様 |
| `/root/workspace/stackchan-lab/bridge/package.json` | 変更 | test command |
| `/root/workspace/stackchan-lab/bridge/src/config/types.ts` | 変更 | AgentRuntimeConfig |
| `/root/workspace/stackchan-lab/bridge/src/config/loadConfig.ts` | 変更 | `agent_runtime` load/validation |
| `/root/workspace/stackchan-lab/bridge/src/config/loadConfig.test.ts` | **新規** | config tests |
| `/root/workspace/stackchan-lab/bridge/src/agent/AgentClient.ts` | **新規** | agent runtime HTTP client |
| `/root/workspace/stackchan-lab/bridge/src/agent/AgentClient.test.ts` | **新規** | HTTP contract/error tests |
| `/root/workspace/stackchan-lab/bridge/src/spokenReply/SpokenReplyPipeline.ts` | 変更 | agent turn -> TTS -> playback |
| `/root/workspace/stackchan-lab/bridge/src/spokenReply/SpokenReplyPipeline.test.ts` | **新規** | pipeline tests |
| `/root/workspace/stackchan-lab/bridge/src/bridge/BridgeError.ts` | 変更 | agent runtime error codes |
| `/root/workspace/stackchan-lab/bridge/src/http/routes.ts` | 変更候補 | status mappingが必要な場合 |
| `/root/workspace/stackchan-lab/bridge/src/main.ts` | 変更 | AgentClient生成・注入 |
| `/root/workspace/stackchan-lab/bridge/config.example.yaml` | 変更 | `agent_runtime` 設定例 |
| `/root/workspace/stackchan-lab/bridge/README.md` | 変更 | 起動・設定 |
| `/root/workspace/stackchan-lab/docs/audio-pipeline.md` | 変更 | E2E flow |
| `/root/workspace/stackchan-lab/docs/architecture.md` | 変更 | 責務境界 |
| `/root/workspace/stackchan-lab/docs/bridge-api.md` | 変更 | config/status/error |
| `/root/workspace/stackchan-lab/docs/conversation-pipeline-plan.md` | 変更 | 旧固定返答Planの扱い |

---

## コミット分割

### EgoPulse

1. `feat: add voice channel configuration` - config型、loader、accessor、validation、tests
2. `feat: add voice API authentication` - Voice専用Bearer middleware、tests
3. `feat: expose authenticated voice turn API` - request validation、identity、process_turn、adapter、integration tests
4. `docs: document voice channel integration` - config/channel/API/spec/example

### StackChan

1. `test: add bridge test runner` - `tsx --test` scriptと最初のconfig test
2. `feat: add agent runtime configuration` - config型、loader、validation、example
3. `feat: add agent runtime client` - AgentClient、error mapping、tests
4. `feat: generate spoken replies with agent runtime` - pipeline差し替え、fixed reply削除、tests、composition root
5. `docs: document agent runtime integration` - bridge/docs更新

コミット時は各Repoの差分を別々に確認する。EgoPulseでは元worktreeの未コミットファイルを含めず、StackChanではbranch切替前から存在した変更・未追跡ファイルをstageしない。

---

## 自動テスト一覧（全 22 件予定）

この一覧はPlan作成時点で必要と判断した最低限の予定であり、最終テスト件数の上限ではない。実装中に追加された不安には、テストリストとTDD Cycleを追加して対応する。

### EgoPulse（全 11 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T1 | `voice_channel_config_resolves_declared_values` | Step 1 | `cargo test voice_channel_config_resolves_declared_values` |
| T2 | `voice_channel_enabled_requires_auth_token` | Step 2 | `cargo test voice_channel_enabled_requires_auth_token` |
| T2 | `voice_channel_enabled_requires_web_channel` | Step 2A | `cargo test voice_channel_enabled_requires_web_channel` |
| T3 | `voice_auth_accepts_configured_voice_token` | Step 3 | `cargo test voice_auth_accepts_configured_voice_token` |
| T3 | `voice_auth_rejects_web_token` | Step 4 | `cargo test voice_auth_rejects_web_token` |
| T4 | `voice_request_builds_stable_surface_context` | Step 5 | `cargo test voice_request_builds_stable_surface_context` |
| T4 | `voice_turn_rejects_identity_components_with_delimiter` | Step 5A | `cargo test voice_turn_rejects_identity_components_with_delimiter` |
| T5 | `voice_turn_rejects_blank_text` | Step 6 | `cargo test voice_turn_rejects_blank_text` |
| T5 | `voice_turn_rejects_surface_outside_allowlist` | Step 7 | `cargo test voice_turn_rejects_surface_outside_allowlist` |
| T6 | `voice_turn_returns_agent_response_and_persists_voice_session` | Step 8 | `cargo test voice_turn_returns_agent_response_and_persists_voice_session` |
| T7 | `voice_adapter_registers_private_voice_route` | Step 9 | `cargo test voice_adapter_registers_private_voice_route` |

### StackChan bridge（全 11 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T8 | `loadConfig_reads_agent_runtime_settings` | Step 10 | `npm test -- --test-name-pattern=loadConfig_reads_agent_runtime_settings` |
| T9 | `loadConfig_requires_agent_runtime_when_spoken_reply_enabled` | Step 11 | `npm test -- --test-name-pattern=loadConfig_requires_agent_runtime_when_spoken_reply_enabled` |
| T9 | `loadConfig_rejects_invalid_agent_runtime_endpoint` | Step 12 | `npm test -- --test-name-pattern=loadConfig_rejects_invalid_agent_runtime_endpoint` |
| T10 | `AgentClient_sends_turn_and_returns_response` | Step 13 | `npm test -- --test-name-pattern=AgentClient_sends_turn_and_returns_response` |
| T11 | `AgentClient_maps_non_success_response_to_bridge_error` | Step 14 | `npm test -- --test-name-pattern=AgentClient_maps_non_success_response_to_bridge_error` |
| T11 | `AgentClient_aborts_after_configured_timeout` | Step 15 | `npm test -- --test-name-pattern=AgentClient_aborts_after_configured_timeout` |
| T11 | `AgentClient_rejects_success_response_without_non_empty_response` | Step 16 | `npm test -- --test-name-pattern=AgentClient_rejects_success_response_without_non_empty_response` |
| T12 | `SpokenReplyPipeline_speaks_agent_response` | Step 17 | `npm test -- --test-name-pattern=SpokenReplyPipeline_speaks_agent_response` |
| T13 | `SpokenReplyPipeline_records_agent_error_without_speaking` | Step 18 | `npm test -- --test-name-pattern=SpokenReplyPipeline_records_agent_error_without_speaking` |
| T13 | `SpokenReplyPipeline_rejects_blank_agent_response` | Step 19 | `npm test -- --test-name-pattern=SpokenReplyPipeline_rejects_blank_agent_response` |
| T14 | `SpokenReplyPipeline_does_not_call_agent_for_ignored_source` | Step 20 | `npm test -- --test-name-pattern=SpokenReplyPipeline_does_not_call_agent_for_ignored_source` |

---

## 工数見積もり

| Step | 内容 | 見積もり |
| -- | -- | -- |
| Step 0 | EgoPulse worktree・StackChan branch作成 | ~10行相当 |
| Step 1-2A | EgoPulse Voice config/validation | ~150-220行 |
| Step 3-4 | EgoPulse Voice専用認証 | ~100-150行 |
| Step 5-9 | Voice handler、identity、turn、adapter、tests | ~340-500行 |
| Step 10-12 | StackChan test基盤とagent runtime config | ~180-260行 |
| Step 13-16 | AgentClientとHTTP/error tests | ~220-320行 |
| Step 17-20 | SpokenReplyPipeline統合とtests | ~180-280行 |
| Step 21 | 両Repoのdocs/example同期 | ~180-300行 |
| Step 22 | 自動検証・E2E実機確認 | ~30-60行相当 |
| Step 23 | Plan・仕様書との自己チェック | ~20-40行相当 |
| Step 24 | 2 RepoのPR作成 | ~20-40行相当 |
| **合計** |  | **~1,410-2,140行** |

見積もりにはテストとdocsを含む。実装中に既存config/auth/routerの責務整理が必要になった場合でも、Voice単独listener、共通server設定移行、outbound voiceは本Planへ追加しない。
