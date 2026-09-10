# Plan: TUIをShared RuntimeのClientへ分離する

Howはあくまで参考であり、よりよい設計方針があれば各自で判断し採用する。

## 1. 背景

現在のEgoPulseでは、Gatewayまたは`egopulse run`でRuntimeを起動した状態で、別プロセスからTUIを起動するとRuntime Instance Lockが競合する。

現在の構造は以下。

```text
Process A
egopulse run / gateway
        ↓
build_app_state_with_path()
        ↓
InstanceGuard
        ↓
AppState
        ↓
Web / Discord / Telegram / Voice


Process B
egopulse
        ↓
runtime::run_tui()
        ↓
build_app_state_with_path()
        ↓
InstanceGuard
        ↓
AppState
        ↓
TUI
```

TUIが単なるユーザーインターフェースではなく、独自Runtimeを起動する構造になっていることが原因。

今回の修正ではInstance Lockを回避するのではなく、**TUIからRuntimeの所有責務そのものを除去する。**

---

# 2. 目的

Runtimeを1つだけ存在させ、TUIをそのRuntimeへ接続するClientにする。

完成形:

```text
                    EgoPulse Runtime
               egopulse run / Gateway
        ┌───────────────────────────────┐
        │ InstanceGuard                 │
        │ AppState                      │
        │ ConfigManager                 │
        │ SQLite                        │
        │ Agent Loop                    │
        │ Tools / MCP                   │
        │ RuntimeSupervisor             │
        │ Turn Scheduler                │
        │ Sleep / Pulse                 │
        │                               │
        │ Web                           │
        │ Discord                       │
        │ Telegram                      │
        │ Voice                         │
        │                               │
        │ Local Runtime API             │
        └───────────────┬───────────────┘
                        │
                  Local IPC
                        │
                        ▼
                       TUI
```

TUI / Web / Discord / Telegram / Voiceは引き続きユーザーとの接点となるChannelとして扱う。

TUIだけを別カテゴリへ移動しない。

---

# 3. 完了条件

以下をすべて満たすこと。

## Runtime ownership

TUIは以下を一切所有しない。

* `AppState`
* `InstanceGuard`
* `Database`
* `ConfigManager`
* `ToolRegistry`
* MCP Manager
* Agent Loop dependencies
* RuntimeSupervisor
* Sleep / Pulse
* Turn Scheduler

## TUI起動

Runtime稼働中に、

```bash
egopulse
```

を実行すると正常にTUIが起動する。

Runtime Instance Lockを新たに取得しない。

## 共存

TUI利用中も、

* Web
* Discord
* Telegram
* Voice

は同じRuntime上で継続動作する。

## Lifecycle

```text
TUI終了
  ≠
Runtime終了
```

とする。

TUIを終了してもGateway Runtimeは継続動作する。

## Runtime停止

Runtime停止時はTUIが接続断を認識し、安全に端末状態を復旧して終了する。

## Runtime不在

Runtimeが存在しない場合、TUI自身がRuntimeを構築しない。

明示的にRuntime未起動として終了する。

---

# 4. スコープ

## 対象

* RuntimeへのLocal IPC endpoint追加
* TUIをRuntime Client化
* TUIのSession操作をRuntime経由へ変更
* TUIのAgent Turn実行をRuntime経由へ変更
* TUIのSlash Command実行をRuntime経由へ変更
* TUIのTool-phase follow-upをRuntime経由へ変更
* TUIからRuntime ownership依存を削除
* Runtimeだけを起動してTUIから利用できる構成への対応
* socket lifecycle / permission / error handling
* 関連テスト
* 関連docs

## 対象外

* `channels/tui/`の移動
* Webの実行経路リファクタ
* Discordの実行経路リファクタ
* Telegramの実行経路リファクタ
* Voiceの実行経路リファクタ
* Web API / SSE / WebSocketの変更
* Discord / Telegramの別プロセス化
* DB schema変更
* `ScheduledTurn`永続形式変更
* Agent Loop内部ロジック変更
* `egopulse -p`のRuntime Client化
* Sleep / Events等のCLI commandのRuntime RPC化
* Remote Runtime接続
* TCPによるRuntime API公開
* Runtime自動起動

---

# 5. InstanceGuardは維持する

今回、`InstanceGuard`の基本構造は変更しない。

現在のLockには、

```text
同じstate_rootを複数プロセスが
同時にRuntime/maintenance writerとして扱わない
```

という役割がある。

通常Runtimeだけでなく、manual Sleep等のState構築経路でも同じ排他を利用している。

したがって、

```text
AppState構築
=
必ず悪い
```

のではない。

問題は、

```text
TUIまでAppStateを構築している
```

こと。

今回の修正では、

```text
Before

TUI
 ↓
build_app_state_with_path()
 ↓
InstanceGuard


After

TUI
 ↓
Runtime Client
```

とすることで解決する。

Instance Lockを弱めたり削除したりしない。

---

# 6. Web / Discord / Telegram / Voiceは現状維持する

これらは現在すでにGateway Runtimeが所有する同じ`AppState`上で動作している。

```text
Runtime
 ├─ Web
 ├─ Discord
 ├─ Telegram
 └─ Voice
```

つまり今回問題になっている「2個目のRuntime」を生成していない。

このため、TUIのRuntime ownership問題を解決するためだけにこれらの内部実行方式まで変更しない。

---

# 7. TUIの配置

TUIはそのまま、

```text
src/channels/tui/
```

に置く。

完成後も、

```text
channels/
├── tui/
├── web/
├── discord.rs
├── telegram.rs
└── voice.rs
```

という分類を維持する。

---

# 8. Local Runtime API

RuntimeにTUIから利用するLocal APIを追加する。

推奨構成:

```text
src/runtime/
├── local_api/
│   ├── mod.rs
│   ├── protocol.rs
│   ├── server.rs
│   └── service.rs
├── turn/
├── channel_input.rs
├── supervisor.rs
└── mod.rs
```

ファイル分割は実装量に応じて調整してよい。

## `protocol`

プロセス間で交換するDTOだけを定義する。

## `server`

Local IPCのlisten / connection lifecycleを担当する。

Runtimeのビジネスロジックを持たせない。

## `service`

受信したLocal API requestを既存Runtime処理へ橋渡しする。

---

# 9. Transport

同一ホスト上のTUIとRuntimeを接続するため、Unix Domain Socketを基本とする。

候補:

```text
<state_root>/runtime/egopulse.sock
```

実際のpath生成は既存のstate/runtime directory解決へ合わせる。

TUIをWeb Channelへ依存させない。

したがって、

* Web port
* Web Bearer Token
* HTTP
* WebSocket

をTUI Runtime接続のために使用しない。

---

# 10. IPC connection方式

以下の意味要件だけを固定する。

* RequestとResponseの対応が曖昧にならない
* Agent TurnのEventを逐次配信できる
* Agent Turn実行中でもTool follow-upを受け付けられる
* Runtime停止をClientが検知できる
* connection切断でRuntimeを停止しない
* 不要に複雑なRPC frameworkを導入しない

以下は実装方法として固定しない。

* connectionを常時1本にするか
* operationごとにconnectionを分けるか
* request IDによるmultiplexingを行うか
* framingをJSON Lines等のどの方式にするか

上記要件を満たす中で最も単純な実装を採用する。

---

# 11. Protocol version

Local APIにはProtocol versionを持たせる。

例:

```text
protocol_version = 1
```

RuntimeとTUIのProtocol versionが一致しない場合は明示的に接続を拒否する。

旧Protocolとの互換分岐は作らない。

---

# 12. Local API操作

最低限、以下の操作を提供する。

```text
RuntimeInfo
ListSessions
OpenSession
ExecuteTurn
ExecuteCommand
StageFollowup
```

命名は実装時により自然なものへ変更してよい。

---

# 13. RuntimeInfo

TUI起動時の接続確認に利用する。

最低限返すもの:

```text
protocol_version
egopulse_version
```

必要以上のRuntime内部状態は公開しない。

---

# 14. Session識別

TUI側で`SurfaceContext`を構築しない。

Session identityの解決はRuntime側の責務とする。

## 保存済みSession

保存済みSessionはDB上の安定した識別子を利用する。

基本候補:

```text
chat_id
```

## 未作成Session

新規TUI Sessionは、

```text
session name
```

をRuntimeへ渡し、最初のTurnで既存Session resolutionに従って作成する。

---

# 15. ListSessions

現在TUIが直接、

```text
agent_loop::list_sessions()
```

を呼んでいる処理をRuntimeへ移す。

TUIには表示に必要な情報だけを返す。

例:

```text
session_id
channel
surface_thread
agent_id
last_message_time
last_message_preview
```

`storage::SessionSummary`そのものをIPC contractにはしない。

---

# 16. OpenSession

現在TUI側で実施している、

```text
get_chat_by_id()
SurfaceContext構築
load_transcript_history()
model解決
```

をRuntime側へ移す。

RuntimeからTUIへ、

```text
SessionView
```

を返す。

最低限:

```text
session identifier
channel
surface_thread
agent_id
effective model
transcript
```

を含める。

---

# 17. Transcript DTO

IPC越しに`llm::Message`をそのまま送らない。

またTUIからAgent Loop内部のmessage formatting helperへの依存を残さない。

Runtime側で永続履歴を、

```text
User
Assistant
ToolStarted
ToolFinished
System
```

等のFrontend向け履歴DTOへ変換する。

必要な情報:

### User

```text
text
```

### Assistant

```text
text
```

### Tool

```text
call_id
name
input
status
result preview
error flag
```

TUIはそのDTOを既存の`Transcript` / `Block`へ変換して描画する。

---

# 18. Agent Turn

現在TUIは、

```text
process_turn_with_events()
```

を直接実行している。

完成後は、

```text
TUI
 ↓
ExecuteTurn
 ↓
Local Runtime API
 ↓
Runtime
 ↓
process_turn_with_events()
```

とする。

Agent Loop本体には変更を加えない。

---

# 19. Turn Event

Agent Loop内部の`AgentEvent`をそのままIPC公開契約にしない。

Local API用のTurn Event DTOを定義する。

最低限、

```text
Iteration
Delta
ToolStart
ToolResult
UserInputInjected
FinalResponse
Error
```

を表現できること。

Runtime側で、

```text
AgentEvent
 ↓
Local Turn Event
```

へ変換する。

TUIはLocal Turn Eventだけを見る。

---

# 20. Turn実行の所有者

Local API経由のTurnもRuntimeが所有する。

TUI processでAgent Loop taskをspawnしない。

Runtime側で既存`RuntimeSupervisor`の管理下に置き、

* Runtime shutdown
* in-flight Turn drain
* panic handling

という既存Lifecycleへ統合する。

---

# 21. Cross-channel Session

現在のTUIはDiscordやWeb等の既存Sessionを開ける。

この挙動を維持する。

例えば、

```text
Discord Session
      ↓
TUIでOpen
      ↓
TUIからExecuteTurn
```

した場合、

* 同じSession履歴を利用する
* 同じagent identityを利用する
* 元Sessionのchannel-specific model resolutionを維持する
* 応答はTUIへstreamする
* Discordへ自動送信しない

こと。

Local API Turnを既存のChannelAdapter配送経路へ安易に流さない。

---

# 22. Slash Command

TUI専用コマンド、

```text
/sessions
```

はTUI内部に残す。

その他の共通Slash CommandはRuntimeへ送る。

```text
TUI
 ↓
ExecuteCommand
 ↓
Runtime
 ↓
process_slash_command()
```

とする。

TUIから`process_slash_command()`への直接依存を削除する。

---

# 23. Slash Command後の状態同期

以下のようなCommandはRuntime状態を変更する。

```text
/model
/provider
/new
```

Command実行後にTUI側が古い状態を表示し続けないようにする。

ExecuteCommandのResponseには、必要に応じて、

```text
response
effective model
effective provider
```

など、TUI表示更新に必要な最新状態を含める。

`/new`の既存Transcript clear挙動も維持する。

---

# 24. Slash completion

Slash Commandの名前一覧など、Runtime状態を必要としない純粋な補完処理はTUIから共有利用してよい。

Runtime実行と補完catalogを混同しない。

---

# 25. Tool-phase Follow-up

現在TUIが直接、

```text
runtime::try_stage_tool_followup()
```

を呼んでいる処理をLocal API経由へ変更する。

```text
TUI
 ↓
StageFollowup
 ↓
Runtime
 ↓
try_stage_tool_followup()
```

既存のdurable follow-up実装を正本とし、Local API内に同じqueue処理を再実装しない。

既存の、

```text
Accepted
NoToolPhase
Rejected
```

という意味をTUIへ返せるようにする。

---

# 26. TUI local pending prompt

現在TUIにはTurn実行中に1件だけ次のpromptを保持するUI側queueがある。

これはTUI固有のUI挙動なので維持する。

```text
Tool phase
    ↓
StageFollowupを試行
    ↓
NoToolPhase
    ↓
TUI local pending prompt
```

という現在の意味を維持する。

RuntimeへUI用queueを移さない。

---

# 27. Runtime Endpoint Resolution

TUIがLocal socketを見つけるためだけに通常の`Config::load()`を実行しない。

現在の通常Config loaderは、

* Provider
* API key
* SecretRef
* `.env`
* Bot
* Runtime validation

まで扱うため、Runtime Clientには重すぎる。

Config moduleに、

```text
config path
 ↓
state_root
 ↓
runtime socket path
```

だけを解決する軽量read pathを追加する。

要件:

* `--config`対応
* YAMLの`state_root`対応
* `state_root`未指定時は既存defaultと完全一致
* Providerを解決しない
* SecretRefを解決しない
* `.env`を読み込まない
* API keyを要求しない
* Bot Tokenを読み込まない

通常Runtime用Config loaderの仕様は変更しない。

---

# 28. Runtime不在時

Local socketへ接続できなかった場合は明示的なErrorを返す。

例:

```text
error: EgoPulse runtime is not running

Start it with:
  egopulse gateway start
or:
  egopulse run
```

以下は禁止。

* TUIから`build_app_state*()`を呼ぶ
* TUIがRuntimeを自動起動
* 別state rootへのfallback
* Web APIへのfallback

---

# 29. Local API lifecycle

Local APIはRuntimeの長寿命Serviceとして扱う。

```text
Runtime startup
 ↓
InstanceGuard
 ↓
AppState build / recovery
 ↓
Local API bind
 ↓
Channels / schedulers
 ↓
Runtime supervision
```

Runtime終了時はLocal APIもSupervisorのshutdownへ従う。

---

# 30. RuntimeSupervisor

Local API listenerをRuntimeSupervisor管理下へ置く。

必要であれば、

```text
TaskKind::LocalApi
```

を追加する。

Local API listenerの予期しない終了は、TUIを受け付けられなくなるためRuntime failureとして扱う。

単なるTUI connection切断はRuntime failureではない。

---

# 31. Runtimeだけを起動できるようにする

現在はWeb / Discord / Telegramが1つも起動しない場合、

```text
NoActiveChannels
```

でRuntimeが終了する。

TUIがRuntime Clientになると、

```text
Runtime
+
TUI
```

だけの利用形態も成立する必要がある。

Local APIがRuntimeの入力面として常時存在するため、外部Channelが0でもRuntimeを起動可能にする。

`NoActiveChannels`が完全に不要になる場合は、

* Error variant
* tests
* docs

まで含めて削除する。

不要コードとして残さない。

---

# 32. Socket lifecycle

## 起動

Instance Lock取得後にLocal socketをbindする。

これにより、同じstate rootの別Runtimeが生存していないことを確認した状態でsocketを管理できる。

## 正常終了

Runtime shutdownでsocketをcleanupする。

## Crash後

Crash等でsocket entryだけ残った場合は、次回Runtime起動時にstale endpointとして処理する。

ただし無条件unlinkは禁止。

既存pathが、

```text
Unix socket
```

の場合だけstale candidateとして処理する。

以下の場合は削除せず起動エラーとする。

```text
regular file
directory
symlink
other file type
```

---

# 33. Local API security

Local APIはlocalhost network serviceとして公開しない。

Filesystem permissionをアクセス境界とする。

socketはRuntime userだけが利用できるpermissionにする。

Protocolやログへ以下を出さない。

* API key
* Bot token
* SecretRef value
* `.env` content

既存`state_root`全体のpermissionを勝手に変更しない。

---

# 34. TUIの依存関係

完成後の`channels/tui`は概ね以下へ依存する。

```text
channels/tui
├─ terminal / crossterm
├─ ratatui
├─ composer
├─ drawing
├─ markdown
├─ transcript
├─ sessions UI
├─ slash completion catalog
└─ Local Runtime Client / protocol
```

以下へ直接依存しない。

```text
runtime::AppState
runtime::RuntimeSupervisor
agent_loop execution
storage::Database
storage::call_blocking
ConfigManager
ToolRegistry
MCP
```

---

# 35. `main.rs`

現在のTUI起動経路、

```text
resolve config
 ↓
Config::load()
 ↓
runtime::run_tui()
 ↓
build_app_state_with_path()
```

を削除する。

完成後:

```text
resolve config path
 ↓
resolve runtime endpoint
 ↓
connect Runtime
 ↓
channels::tui::run(...)
```

とする。

不要になる、

```text
runtime::run_tui()
```

は削除する。

互換wrapperとして残さない。

---

# 36. Runtime entrypoint

Local API、Channels、Schedulersを起動してRuntime全体をsuperviseする処理が現在の`start_channels()`の責務を超える場合は、実態に合ったRuntime entrypointへ整理する。

例えば、

```text
run_runtime()
```

など。

ただしrename自体を目的にはしない。

現名称のまま責務が十分明確なら不要。

---

# 37. 実装Step

## Step 0: Worktree作成

```bash
git worktree add ../egopulse-single-runtime-tui-client \
  -b refactor/single-runtime-tui-client
```

作業は新しいWorktree内で行う。

---

## Step 1: Runtime endpoint解決を分離

実装:

* config path resolutionを既存処理と共有
* state_rootだけを読む軽量resolver追加
* runtime socket path生成追加
* TUI起動前のfull Config loadを不要にする

確認:

* default config
* custom `--config`
* custom `state_root`
* Provider設定とは独立して解決可能
* Secret resolutionなし
* config不存在時のsetup案内維持

---

## Step 2: Local API protocol / transport追加

実装:

* Protocol envelope
* Protocol version
* RuntimeInfo
* Error response
* Local socket server
* Local Runtime Client
* RuntimeSupervisorへのlistener登録
* socket cleanup / stale handling
* permission

この段階ではRuntimeInfoだけ通せればよい。

確認:

```text
Runtime
 ↓
Local socket
 ↓
Client
 ↓
RuntimeInfo
```

---

## Step 3: Session API追加

実装:

* ListSessions
* OpenSession
* new Session reference
* SessionView
* history DTO変換
* effective model取得

TUI側の、

* `agent_loop::list_sessions`
* `get_chat_by_id`
* `call_blocking`
* `load_transcript_history`
* `model_for(AppState, ...)`

をLocal APIへ置換する。

---

## Step 4: TUI startupをClient化

実装:

* `TuiApp`から`Arc<AppState>`削除
* Local Runtime Client保持
* startup Session取得をRuntime経由へ変更
* `--session`挙動維持
* Runtime不在時error
* TUI終了時にRuntimeを触らない

この時点で、

```bash
egopulse gateway start
egopulse
```

がInstance Lock競合なしでTUI表示まで進むこと。

---

## Step 5: Turn streaming移行

実装:

* ExecuteTurn
* AgentEvent → Local Turn Event変換
* Turn Event streaming
* TUI Transcriptへの反映
* Runtime側Supervisor ownership
* Turn failure伝搬

TUIから、

```text
process_turn_with_events()
supervisor.spawn_turn()
```

を削除する。

---

## Step 6: Slash Command / Follow-up移行

実装:

* ExecuteCommand
* updated model/provider metadata
* StageFollowup
* existing `try_stage_tool_followup()`への委譲
* `/sessions`はTUIローカル維持
* TUIのlocal pending prompt維持

TUIから、

```text
process_slash_command()
try_stage_tool_followup()
```

への直接依存を削除する。

---

## Step 7: Runtime-only起動とLifecycle整理

実装:

* Local APIを正式なRuntime input surfaceとして扱う
* external channel 0でもRuntime起動可能
* 必要なら`start_channels()`責務/名称整理
* Runtime停止時のClient切断
* stale socket cleanup
* 不要な`NoActiveChannels`関連処理削除

---

## Step 8: 旧Runtime ownership削除

最終構造完成後に、旧コードをまとめて残さず削除する。

対象:

* `runtime::run_tui()`
* TUI `Arc<AppState>`
* TUI direct DB access
* TUI direct Agent Loop execution
* TUI direct Slash Command execution
* TUI direct follow-up runtime call
* TUI Supervisor操作
* TUI RuntimeStatus監視
* 不要helper
* 不要imports
* 旧構造だけのtests
* 不正確になったcomments

---

## Step 9: Docs更新

コード完成後の実際の構造に合わせて更新する。

---

# 38. テスト方針

既存テストを削除してテスト数だけ減らすのではなく、責務移動に合わせて移動する。

例えば、

```text
TUI内部でSessionをDBからロードできる
```

という旧実装テストは、

```text
Runtime Local APIがSessionViewを返す
```

という新境界のテストへ置き換える。

一方、

* Composer
* Draw
* Markdown
* Transcript
* Session overlay

の純粋UIテストはTUI側に残す。

---

# 39. 必須自動テスト

## Endpoint resolution

* default state_root
* custom state_root
* custom `--config`
* provider/API key無しでもendpoint resolution可能

## Local API

* RuntimeInfo取得
* protocol version mismatch
* malformed request
* Runtime shutdown時connection終了

## Socket

* bind / connect
* stale socket recovery
* regular fileを削除しない
* symlinkを削除しない
* graceful shutdown cleanup

## Session

* Session list
* Existing Session open
* Unknown named Session
* non-default agent保持
* channel identity保持
* effective model
* transcript restoration
* Tool history restoration

## Turn

* Delta stream
* ToolStart
* ToolResult
* UserInputInjected
* FinalResponse
* Error propagation

## Slash Command

* normal shared command
* `/model`
* `/provider`
* `/new`

## Follow-up

* Tool phase accepted
* NoToolPhase
* rejected follow-up

## Ownership

Gateway RuntimeがInstance Lockを保持した状態でTUI Clientを接続できる。

## Lifecycle

TUI Client disconnect後もRuntimeが動作する。

## Local-only

external channelsなしでRuntimeがLocal APIを提供できる。

---

# 40. 手動動作確認

## Runtime + TUI

```bash
egopulse gateway start
egopulse
```

Instance Lock errorなしでTUIが開く。

---

## 通常会話

確認:

* streaming
* Markdown
* Tool Start
* Tool Result
* Final Response

---

## Slash Command

最低限:

```text
/status
/model
/provider
/new
/sessions
```

を確認。

---

## Session切替

`/sessions`から、

* TUI Session
* Web Session
* Discord Session
* Telegram Session

を開く。

履歴とagent/modelが正しく復元される。

---

## Cross-channel Session

Discord SessionをTUIで開き、TUIから会話する。

確認:

* 同じSessionへ履歴が追加される
* Agentの応答がTUIへ表示される
* Discordへ勝手に応答を投稿しない

---

## Follow-up

Tool実行中に追加メッセージを送る。

既存durable follow-upとして受付される。

---

## TUI終了

TUIを終了。

その後Gateway Runtimeが引き続き動作していることを確認する。

Web / Discord / Telegramも引き続き利用可能であること。

---

## Runtime停止

TUI起動中にRuntimeを停止する。

TUIが安全に接続断を処理し、

* raw mode
* bracketed paste
* cursor

を復旧して終了する。

---

## Runtime不在

Runtimeを停止した状態で、

```bash
egopulse
```

を実行。

明示的なRuntime unavailable errorとなる。

新しい`AppState`やRuntimeを作らない。

---

## Local-only

Web / Discord / Telegramを無効化したテスト用Configで、

```bash
egopulse run
```

を起動。

別terminalから、

```bash
egopulse
```

でTUIを利用できること。

---

# 41. 変更ファイル候補

## 新規候補

```text
src/runtime/local_api/mod.rs
src/runtime/local_api/protocol.rs
src/runtime/local_api/server.rs
src/runtime/local_api/service.rs
```

必要以上に細かくなる場合は統合する。

## 主な変更対象

```text
src/main.rs
src/runtime/mod.rs
src/runtime/supervisor.rs
src/config/...
src/channels/tui/mod.rs
src/channels/tui/sessions.rs
src/channels/tui/transcript.rs
src/error.rs
```

必要な場合のみ:

```text
Cargo.toml
Cargo.lock
```

---

# 42. Docs対象

最低限確認する。

```text
README.md
docs/architecture.md
docs/channels.md
docs/commands.md
docs/directory.md
docs/deploy.md
docs/security.md
docs/session-lifecycle.md
```

変更が不要な文書は無理に触らない。

主に以下を修正する。

```text
TUI = local Runtime
```

という旧説明を、

```text
TUI = shared Runtimeへ接続するlocal channel client
```

へ変更する。

Local socketの、

* path
* lifecycle
* permission
* Runtime不在時挙動

も関連文書へ反映する。

---

# 43. 削除確認

実装完了時に以下を検索する。

```bash
rg "run_tui" src
rg "AppState" src/channels/tui
rg "process_turn_with_events" src/channels/tui
rg "call_blocking|Database" src/channels/tui
rg "process_slash_command" src/channels/tui
rg "shutdown_token|poll_long_lived|supervisor.shutdown" src/channels/tui
```

完成状態ではRuntime ownershipに関係する直接依存が残っていないこと。

補完catalogなど純粋な共有処理は対象外。

---

# 44. 全体検証

```bash
cargo fmt --check
cargo test
cargo check
cargo clippy --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
git diff --check
```

Local API追加で依存関係を変更した場合は必要に応じて、

```bash
cargo audit
cargo deny check
```

も確認する。

Web実装を変更していない限り、Webリファクタをこの作業へ追加しない。

---

# 45. コミット分割

意味単位で分割する。

例:

```text
refactor(config): add runtime endpoint resolution
feat(runtime): add local runtime api
feat(runtime): expose local session operations
refactor(tui): connect to shared runtime
refactor(tui): route turns through runtime
refactor(tui): route commands and followups through runtime
refactor(runtime): support local-only runtime
docs: document shared runtime tui architecture
```

実際の変更境界が異なる場合は、ファイル単位ではなく責務単位を優先して調整する。

---

# 46. PR

1つのPRで完結させる。

PRタイトル候補:

```text
refactor(runtime): make TUI a client of the shared runtime
```

PRでは少なくとも以下を説明する。

* 問題のRoot Cause
* TUIが独自Runtimeを所有していたこと
* Single Runtime化後の構造
* Local Runtime API
* TUIから削除したRuntime ownership
* InstanceGuard自体は維持していること
* Web / Discord / Telegram / Voiceの実行構造を変更していないこと
* テスト結果
* 手動確認結果
