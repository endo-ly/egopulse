# Repository Stabilization Phase 3 Plan

## 1. 目的

Phase 3では、EgoPulseを長時間常駐するAgent Runtimeとして安定運用できる状態へ仕上げる。

Phase 2でConversation TurnとTool Callの実行状態は永続化された。一方、Runtime taskの所有、非同期Turnの受付、Sleep Memoryの公開には、プロセス停止時の入力消失や不整合につながる境界が残っている。

本Phaseでは、次の3つを完成させる。

1. Runtimeが起動したtaskを所有し、異常終了とshutdownを一貫して管理する
2. 非同期実行されるConversation Turnを、memory queueへ入れる前に永続化する
3. Sleep Memoryの3ファイルを、一貫したbundleとして公開・復旧する

以下は本PRのスコープ外とし、独立したSecurity Hardening PRへ延期する。

- **Bash Toolの権限分離**: LLMが実行するBash Toolを、EgoPulse本体のhost権限から分離する sandbox は未実施である。現時点ではhost権限で実行される既知リスクが残る。

---

## 2. 完成後の構造

```text
Discord / Telegram / Webhook / Agent Send
                    │
                    ▼
              TurnIntake
        turn_runs: accepted
                    │
                    ▼
            TurnDispatcher
                    │
                    ▼
             TurnScheduler
                    │
                    ▼
       execute accepted Turn
                    │
                    ▼
 input_committed → model → tools → completed
```

```text
Sleep Steps
    │
    ▼
candidate Memory
    │
    ▼
memory_snapshots
    │
    ▼
Memory Publication
    │
    ▼
episodic / semantic / prospective
```

```text
RuntimeSupervisor
├── Channel tasks
├── TurnDispatcher
├── Agent Send worker
├── Conversation Turn tasks
├── MCP reconnect task
├── Sleep scheduler / tasks
├── Pulse scheduler / tasks
└── Backup scheduler
```


---

## 3. Runtime invariants

Phase 3完了後、Runtimeは次を常に満たす。

### 3.1 Task ownership

- 長寿命taskには所有者が存在する
- Conversation Turn taskはSupervisorが追跡する
- taskのpanic、error、unexpected completionを観測できる
- shutdown開始後に新しいTurnを開始しない
- shutdown deadlineを超えたtaskをabortできる

### 3.2 Durable acceptance

- 非同期Turnは`turn_runs`へcommitされた後に受付成功となる
- `accepted` Turnは再起動後に実行要求を復元できる
- Scheduler capacity不足で受付済みTurnを失わない
- 同一request keyは同一Turnへ収束する
- Turn executionの重複競合は、片方のexecutorが安全に終了する

### 3.3 Memory consistency

- 1つのSleep Runが生成した3ファイルは同じcandidate bundleから公開される
- Turnはpublication途中の混在したMemoryを読まない
- publication途中のprocess停止後に同じbundleへ収束する
- Sleep中に発生した手動編集を検知する

### 3.4 Single-instance guarantee

- 同じ state root に対して同時に起動できる Runtime は 1 つだけである
- Runtime 起動時に state root 内の専用ロックファイルに対して OS の排他 advisory lock を取得し、DB オープン前に獲得する
- ロック獲得に失敗した場合（他プロセスが既に保持）、起動を `RuntimeAlreadyRunning` で拒否する
- ロックはプロセス終了（正常・異常問わず）時に OS が自動解放するため、クラッシュ後の次回起動をブロックしない
- 手動 Sleep 実行も同一 state root のロックを共有し、Runtime と同時実行されない


# 4. Package 1 — Runtime Supervisor

## 4.1 目的

Runtimeが起動するtaskを一つのlifecycleへ統合し、起動、異常検知、drain、停止を一貫して管理する。

## 4.2 RuntimeSupervisor

`RuntimeSupervisor`は次を保持する。

```rust
struct RuntimeSupervisor {
    cancellation: CancellationToken,
    accepting_inputs: AtomicBool,
    long_lived_tasks: TaskSet,
    turn_tasks: TaskSet,
    maintenance_tasks: TaskSet,
    shutdown_state: ShutdownState,
}
```

内部実装は`JoinSet`または所有された`JoinHandle`集合を使用する。

各taskには次のmetadataを付与する。

```rust
struct TaskMetadata {
    name: &'static str,
    kind: TaskKind,
    criticality: TaskCriticality,
}
```

```rust
enum TaskKind {
    Channel,
    Dispatcher,
    Turn,
    AgentWorker,
    Mcp,
    Sleep,
    Pulse,
    Backup,
}
```

```rust
enum TaskCriticality {
    Critical,
    Supporting,
}
```

## 4.3 Supervisor経由で起動するtask

- Web server
- Discord listener
- Telegram listener
- Voice listener
- TurnDispatcher
- Agent Send receiver
- MCP reconnect loop
- Sleep scheduler
- Pulse scheduler
- Backup scheduler
- Schedulerから開始されるConversation Turn
- Sleep batch
- Pulse Activation

親Futureが必ず完了をawaitする短時間の補助taskは、親taskのlifecycleに含める。

## 4.4 Critical task failure

Critical taskがpanic、error、unexpected completionした場合、次を実行する。

1. RuntimeStatusへtask名と終了理由を記録
2. `accepting_inputs`をfalseへ変更
3. root cancellationを発火
4. graceful shutdownへ遷移

Critical task:

- Web server
- 有効化されたChannel listener
- TurnDispatcher
- Agent Send receiver

## 4.5 Turn task ownership

TurnSchedulerが実行開始を決定した後、Conversation TurnはSupervisor経由でspawnする。

Supervisorは次を追跡する。

- turn_id
- agent_id
- channel
- session key
- started_at
- completion
- panic
- forced abort

Turn taskの正常終了時、Schedulerへsession完了を通知し、同じsessionの次Turnを開始する。

Turn taskのpanic時もsession slotを解放し、該当Turnをrecovery対象として残す。

## 4.6 Shutdown sequence

```text
1. accepting_inputs = false
2. HTTP / Channelの新規入力受付を停止
3. TurnIntakeの新規acceptを停止
4. TurnDispatcherの新規dispatchを停止
5. TurnSchedulerの新規Turn開始を停止
6. 実行中Conversation Turnをdrain
7. 実行中Sleep publicationをdrain
8. 実行中Pulse Activationをdrain
9. Sleep / Pulse / Backup schedulerを停止
10. MCP reconnect loopを停止
11. deadline超過taskをabort
12. DB poolとRuntime resourceを解放
```

`accepted`状態で待機しているTurnはDBに保持する。

## 4.7 RuntimeStatus / Metrics

記録する項目:

- task count by kind
- critical task failures
- task panics
- active Turn tasks
- shutdown started
- shutdown duration
- forced abort count
- accepting inputs

## 4.8 完了条件

- 長寿命taskのhandleが破棄されていない
- Scheduler起点のConversation TurnがSupervisorに所有される
- critical task終了をRuntime全体が検知する
- shutdown開始後に新しい入力をacceptしない
- shutdown開始後に新しいTurnを開始しない
- active Turnをdeadlineまでdrainする
- deadline超過taskをabortする
- task panic後もsession scheduler slotが解放される

## 4.9 単一インスタンス保証（RuntimeSupervisor の所有）

単一インスタンス保証（§3.4）は RuntimeSupervisor が所有する。

- `InstanceGuard` を supervisor が保持し、プロセス生存期間中ロックを維持する
- ロック取得は `build_app_state` において DB オープン前に行われ、取得失敗時は起動を拒否する
- ロック状態は health エンドポイントと metrics ゲージ（`egopulse_runtime_instance_lock`）から観測できる

---

# 5. Package 2 — Durable Scheduled Turn

## 5.1 目的

`ScheduledTurn`として非同期実行される入力を、TurnSchedulerへ投入する前に`turn_runs`へ永続化する。

対象経路:

- Discord
- Telegram
- Webhook
- Agent Send
- `submit_scheduled_turn`を使用するChannel adapter

## 5.2 turn_runs schema

Normal DBとSecret DBの`turn_runs`へ次を追加する。

```sql
ALTER TABLE turn_runs ADD COLUMN scheduled_request_json TEXT;
ALTER TABLE turn_runs ADD COLUMN origin_id TEXT;
ALTER TABLE turn_runs ADD COLUMN origin_stop_reason TEXT;
```

検索index:

```sql
CREATE INDEX idx_turn_runs_dispatch
ON turn_runs(state, accepted_at, turn_id)
WHERE scheduled_request_json IS NOT NULL;

CREATE INDEX idx_turn_runs_origin
ON turn_runs(origin_id, accepted_at)
WHERE origin_id IS NOT NULL;
```

各fieldの責務:

| field | 責務 |
|---|---|
| `scheduled_request_json` | accepted Turnの実行要求を復元する |
| `origin_id` | Agent Send chainのidentityを永続化する |
| `origin_stop_reason` | chainを停止させた理由を永続化する |

既存の次のfieldをそのまま使用する。

- `turn_id`
- `chat_id`
- `request_key`
- `request_payload_hash`
- `state`
- `config_revision`
- `config_fingerprint`
- `input_message_id`
- `output_published`
- `accepted_at`
- `updated_at`

## 5.3 PersistedScheduledTurn

`scheduled_request_json`はversioned schemaとする。

```rust
#[derive(Serialize, Deserialize)]
struct PersistedScheduledTurnV1 {
    version: u32,
    context: PersistedSurfaceContextV1,
    input: String,
}

#[derive(Serialize, Deserialize)]
struct PersistedSurfaceContextV1 {
    channel: String,
    surface_user: String,
    surface_thread: String,
    chat_type: String,
    agent_id: String,
    channel_log_chat_id: Option<i64>,
    chain_depth: usize,
}
```

次は独立columnまたは実行時生成値として扱う。

- `request_key`: `turn_runs.request_key`
- `origin_id`: `turn_runs.origin_id`
- `scope`: 保存先DB
- `trace_id`: execution開始時に生成
- `turn_id`: `turn_runs.turn_id`

serialization対象は専用型とし、`SurfaceContext`自体へ永続schemaの責務を持たせない。

## 5.4 Canonical request hash

`request_payload_hash`は次のcanonical inputから生成する。

```text
version
channel
surface_user
surface_thread
chat_type
agent_id
channel_log_chat_id
chain_depth
input
```

JSON objectのfield順序やwhitespaceに依存しないcanonical serializationを使用する。

同じ`chat_id + request_key`で既存Turnが存在する場合:

- hash一致: 既存Turnを返す
- hash不一致: conflictを返す

`origin_id`と`trace_id`はhashへ含めない。

## 5.5 Root originとAgent Send origin

Root Turnでは、新規作成する`turn_id`を`origin_id`として使用する。

```text
root turn:
  origin_id = turn_id
  chain_depth = 0
```

Agent Sendが生成する子Turnでは、親Turnの`origin_id`を継承する。

```text
child turn:
  origin_id = parent.origin_id
  chain_depth = parent.chain_depth + 1
```

重複受付で既存Turnが返る場合は、既存rowの`origin_id`を使用する。

## 5.6 TurnIntake

新しい受付境界を`TurnIntake`へ集約する。

```rust
struct NewScheduledTurn {
    context: SurfaceContext,
    input: String,
}
```

```rust
enum TurnIntakeOutcome {
    Created(AcceptedTurnRef),
    Existing(AcceptedTurnRef, TurnRunState),
    Rejected(TurnRejectReason),
}
```

```rust
struct AcceptedTurnRef {
    turn_id: String,
    scope: ConversationScope,
    session_key: String,
    origin_id: String,
}
```

受付処理:

```text
1. Runtimeがinput受付中であることを確認
2. request keyを確定
3. chatをresolve / create
4. canonical requestを生成
5. request hashを生成
6. duplicateを確認
7. origin reservationを取得
8. accepted backlog capacityを確認
9. turn_runsへaccepted rowをinsert
10. transaction commit
11. TurnDispatcherをwake
12. AcceptedTurnRefを返す
```

DB write失敗時はorigin reservationをreleaseする。

duplicateで既存Turnを返す場合も、新規に取得したreservationをreleaseする。

## 5.7 Backlog capacity

次の既存上限をdurable accepted backlogへ適用する。

- global accepted Turn: `MAX_GLOBAL_QUEUED_TURNS`
- chat単位accepted Turn: `MAX_QUEUED_TURNS_PER_SESSION`
- tracked origin: `MAX_TRACKED_ORIGINS`
- origin単位execution Turn: `MAX_AGENT_TURNS_PER_INPUT`
- chain depth: `MAX_AGENT_CHAIN_DEPTH`

capacity判定は新規row insertと同じSQLite transaction内で行う。

duplicate rowの取得はcapacityに関係なく成功させる。

## 5.8 Webhook acceptance

Webhook handlerは次の順序で処理する。

```text
receiver resolve
  ↓
Bearer token validation
  ↓
payload size / JSON validation
  ↓
target channel / thread / agent / scope resolve
  ↓
Agent input formatting
  ↓
TurnIntake::accept
  ↓
DB commit
  ↓
202 Accepted
```

request keyの優先順位:

1. `Idempotency-Key` header
2. payloadのstable event identifier
3. requestごとのUUID

response:

```json
{
  "ok": true,
  "status": "accepted",
  "turn_id": "..."
}
```

hash conflictはHTTP 409とする。

capacity rejectionはHTTP 429とする。

shutdown中の受付はHTTP 503とする。

## 5.9 TurnDispatcher

TurnDispatcherはRuntimeSupervisorが所有する単一のlong-lived taskとする。

内部状態:

```rust
struct TurnDispatcher {
    queued_turn_ids: HashSet<String>,
    wake_rx: Receiver<()>,
}
```

dispatch loop:

```text
1. wakeまたはperiodic intervalを待つ
2. Normal DBからdispatchable Turnを取得
3. Secret DBからdispatchable Turnを取得
4. accepted_at, turn_id順でmerge
5. queued_turn_idsに存在しないTurnを選ぶ
6. TurnSchedulerへAcceptedTurnRefをsubmit
7. Started / Queuedならqueued_turn_idsへ登録
8. Scheduler capacity時はDB rowをacceptedのまま保持
```

periodic scanにより、wake notificationの消失後もDBへ収束する。

## 5.10 TurnScheduler

TurnSchedulerが保持するqueue itemを、入力本文を持つ`ScheduledTurn`からaccepted済みTurn参照へ変更する。

```rust
struct RunnableTurn {
    turn_id: String,
    scope: ConversationScope,
    session_key: String,
    origin_id: String,
    resume_point: TurnResumePoint,
}
```

```rust
enum TurnResumePoint {
    Accepted,
    InputCommitted,
}
```

session orderingは`session_key`で維持する。

Schedulerが実行開始を決めた後、RunnableTurnをRuntimeSupervisorへ渡す。

## 5.11 Turn execution split

現在のTurn処理を次の境界へ分離する。

```text
accept direct Turn
execute accepted Turn
resume input_committed Turn
run model / tool loop
```

概念API:

```rust
process_direct_turn(...)
execute_accepted_turn(turn_id, scope)
resume_input_committed_turn(turn_id, scope)
```

`process_direct_turn`は、直接応答を返す既存経路のwrapperとして使用する。

非同期Scheduled Turnは`execute_accepted_turn`を使用する。

## 5.12 Config pinning

Scheduled Turnの受付時点では`config_revision = 0`、`config_fingerprint = NULL`とする。

実行開始時にConfig snapshotを1回取得し、`turn_runs`へ固定する。

```text
accepted
  ↓
current ConfigSnapshot取得
  ↓
config revision / fingerprintをCASで固定
  ↓
Provider / Prompt / Tool definition解決
```

既にfingerprintが固定されているTurnでは、current fingerprintとの一致を確認する。

Config identityの固定後は、Turn完了まで同じsnapshotを使用する。

## 5.13 accepted execution

`execute_accepted_turn`は次を実行する。

```text
1. turn_runsを取得
2. state = acceptedを確認
3. scheduled_request_jsonをdeserialize
4. SurfaceContextを再構築
5. Config snapshotを固定
6. Provider / Prompt / Toolを準備
7. sessionをload
8. compactionを実行
9. user messageとsession snapshotをcommit
10. accepted → input_committed
11. model loopを開始
```

user message、session snapshot、chat revision、input message ID、Turn state transitionは同一transactionでcommitする。

input message ID:

```text
turn:{turn_id}:input
```

## 5.14 input_committed resume

`resume_input_committed_turn`は次を検証する。

- scheduled requestを復元できる
- input message IDが存在する
- input messageが該当Turnに属する
- session snapshotが該当inputを含む
- Config fingerprintが一致する
- `output_published = false`
- Tool execution rowが存在しない
- stateが`input_committed`

検証後、保存済みsession snapshotからmodel loopを開始する。

input messageのinsert、compaction、`accepted → input_committed`処理は再実行しない。

## 5.15 Executor競合

同じTurnを複数executorが開始した場合、state transitionのCASを実行権境界として扱う。

- `accepted → input_committed`を先にcommitしたexecutorが継続する
- `input_committed → model_pending`を先にcommitしたexecutorが継続する
- CAS conflictとなったexecutorは最新stateを読み、通常終了する

CAS conflictをTurn failureへ変換しない。

## 5.16 Recovery

Runtime起動時、Normal DBとSecret DBを対象にTurnを分類する。

| persisted state | recovery |
|---|---|
| `accepted` + valid scheduled request | Dispatcherへ登録 |
| `input_committed` + resume検証成功 | `InputCommitted`としてDispatcherへ登録 |
| `accepted` + request復元error | `failed` |
| `input_committed` + resume検証error | `failed` |
| `model_pending` | `uncertain` |
| `model_completed` | `uncertain` |
| `tools_pending` | `uncertain` |
| `tools_completed` | `uncertain` |

`output_published = true`の非terminal Turnは`uncertain`とする。

## 5.17 Origin tracker recovery

Runtime起動時、`origin_id`が存在し、`accepted_at`がorigin TTL内のTurnを集計する。

復元する値:

- accepted rows: pending reservation
- input_committed以降のrows: executed count
- `origin_stop_reason`: terminal reason
- latest updated_at: TTL基準

復元後にTurnDispatcherを開始する。

chain stopを発生させたTurnでは、該当reasonを`origin_stop_reason`へ保存する。

## 5.18 Completion handling

Turnが`input_committed`へ進んだ時点で、Dispatcherの`queued_turn_ids`から削除する。

Turn taskが完了・失敗・panicした場合、Scheduler slotを解放する。

同じsessionの次TurnはSupervisor経由で開始する。

## 5.19 Metrics

- accepted Turn count
- oldest accepted age
- intake created / existing / rejected
- dispatcher scans
- dispatcher started / queued / deferred
- accepted recovery
- input_committed recovery
- request decode failure
- request hash conflict
- origin recovery count
- executor CAS conflict

## 5.20 完了条件

- `submit_scheduled_turn`経路がSchedulerより先に`turn_runs`へcommitする
- WebhookがDB commit後に202を返す
- accepted Turnがrestart後に実行される
- input_committed Turnがrestart後にmodel loopを開始する
- same request key + same requestが同一Turnへ収束する
- same request key + different requestがconflictになる
- Scheduler capacity時にaccepted TurnがDBへ残る
- Normal / Secret DBのroutingが維持される
- Agent Sendのoriginとchain depthがrestart後も復元される
- duplicate executorがTurnをfailedへ変更しない
- Config snapshotがexecution開始時に固定される

---

# 6. Package 3 — Recoverable Memory Publication

## 6.1 目的

Sleep Runが生成する`episodic.md`、`semantic.md`、`prospective.md`を一つのMemory bundleとして公開し、process停止後も同じbundleへ復旧する。

## 6.2 BatchContext

BatchContextはrun開始時のMemoryと生成中のcandidateを分けて保持する。

```rust
struct BatchContext {
    run_id: String,
    agent_id: String,
    base_memory: MemoryBundle,
    candidate_memory: MemoryBundle,
    ...
}
```

```rust
struct MemoryBundle {
    episodic: String,
    semantic: String,
    prospective: String,
}
```

Sleep Stepは`candidate_memory`を更新する。

## 6.3 Step commit

各Memory Stepは次の順序で完了する。

```text
1. LLM / rendererでcandidateを生成
2. candidate_memoryを更新
3. memory_snapshotsへcontent_before / content_afterを保存
4. checkpointとStep resultをcommit
5. Stepをsuccessまたはskippedへ遷移
```

Step処理中は公開中Markdownを変更しない。

`content_before`は`base_memory`の値を使用する。

`content_after`はStep完了時点のcandidateを使用する。

## 6.4 Complete snapshot set

finalize前に、同じrun_idについて3種類のsnapshotを揃える。

- episodic
- semantic
- prospective

更新が発生しなかったfileでは次を保存する。

```text
content_before = base content
content_after  = base content
```

`memory_snapshots`の`UNIQUE(run_id, file)`をpublication bundleの整合性条件として使用する。

## 6.5 Per-agent Memory lock

MemoryLoaderへagent単位の`RwLock` registryを追加する。

```rust
struct AgentMemoryLockRegistry {
    locks: DashMap<AgentId, Arc<RwLock<()>>>,
}
```

Turn側:

```text
read lock取得
  ↓
3ファイルをMemoryBundleとしてload
  ↓
bundleをclone
  ↓
read lock解放
```

Sleep publication側:

```text
write lock取得
  ↓
precondition検証
  ↓
3ファイルをpublish
  ↓
cache更新
  ↓
write lock解放
```

LLM生成中はwrite lockを保持しない。

## 6.6 MemoryLoader bundle load

MemoryLoaderにbundle単位のAPIを追加する。

```rust
fn load_bundle(&self, agent_id: &str) -> Result<Arc<MemoryBundle>, MemoryError>
```

3ファイルを同じread lock内で読み込む。

cacheもagent単位のMemoryBundleとして更新する。

Turn prompt buildingはbundle APIを使用する。

## 6.7 Publication precondition

通常publicationでは、write lock取得後に現在の3ファイルを読み、各snapshotの`content_before`と一致することを確認する。

一致後にpublicationを開始する。

一致しない場合:

- current filesを維持
- Sleep Runをfailedへ遷移
- conflict fileをerrorへ記録
- publication conflict metricを増加

## 6.8 Atomic file replacement

各fileについて同じdirectoryにtemp fileを作成する。

```text
episodic.md.<run_id>.tmp
semantic.md.<run_id>.tmp
prospective.md.<run_id>.tmp
```

publication:

```text
1. 3つのtemp fileへcontent_afterを書込
2. 各temp fileをflush / sync_all
3. episodic tempをrename
4. semantic tempをrename
5. prospective tempをrename
6. directoryをsync
7. MemoryLoader cacheをcandidate bundleへ更新
8. sleep_runsをsuccessへ遷移
```

rename中はagent write lockを保持する。

## 6.9 Startup recovery

Runtime起動時、`status = running`のSleep Runを検査する。

3種類のsnapshotが揃い、Memory Stepがterminalであるrunをpublication recovery対象とする。

各fileのcurrent contentを次で検証する。

```text
current == content_before
または
current == content_after
```

3ファイルすべてが検証を通った場合:

```text
content_afterからtemp fileを再作成
  ↓
3ファイルをrename
  ↓
directory sync
  ↓
cache更新
  ↓
sleep_runsをsuccessへ遷移
```

snapshotが揃っていないrunはfailedへ遷移する。

current contentがbefore / afterのどちらにも一致しない場合、startupをerrorで停止し、agent_id、run_id、fileを表示する。

Memory recovery完了後にTurnDispatcherとChannelを開始する。

## 6.10 Shutdown

Memory publication taskはRuntimeSupervisorのmaintenance taskとして所有する。

shutdown開始時:

- 新しいSleep Runを開始しない
- candidate生成中のtaskをdeadlineまでdrain
- publication開始済みtaskを優先してdrain
- deadline超過時はabortし、次回startup recoveryへ委ねる

## 6.11 Metrics

- publication started
- publication success
- publication conflict
- publication recovery
- snapshot incomplete
- recovery validation error
- Memory read lock wait
- Memory write lock wait

## 6.12 完了条件

- Sleep Stepが公開Markdownを直接変更しない
- 1つのrun_idに3種類のsnapshotが存在する
- 更新なしfileにもsnapshotが存在する
- Turnが3ファイルをbundleとして読む
- publication中にTurn readerが入らない
- 手動編集conflictを検出する
- 1ファイル目rename後のrestartで復旧する
- 2ファイル目rename後のrestartで復旧する
- 全rename後・run success前のrestartで復旧する
- recovery完了前にChannelを開始しない
- MemoryLoader cacheがpublication後のbundleと一致する

---


# 7. Startup integration

Runtime startupを次の順序へ統一する。

```text
1. Config load
2. DB backup
3. Normal DB migration
4. Secret DB migration
5. Tool Call recovery
6. Turn recovery classification
7. Origin tracker recovery
8. Memory publication recovery
9. AppState construction
10. RuntimeSupervisor start
11. TurnDispatcher start
12. recovery TurnをDispatcherへwake
13. Agent Send receiver start
14. MCP reconnect loop start
15. Sleep scheduler start
16. Pulse scheduler start
17. Backup scheduler start
18. Channel / Web server start
19. accepting_inputs = true
```

startup errorはRuntimeStatusとlogへ構造化して記録する。

---

# 8. 実装順序

## Package 1

1. RuntimeSupervisor core
2. long-lived task registration
3. Turn task ownership
4. critical task handling
5. shutdown sequence
6. metrics / tests

## Package 2

1. Normal / Secret migration
2. PersistedScheduledTurn型
3. storage API
4. TurnIntake
5. request key / origin rule
6. TurnDispatcher
7. TurnScheduler item変更
8. execute accepted Turn
9. input_committed resume
10. origin tracker recovery
11. Webhook cutover
12. Discord / Telegram / Agent Send cutover
13. recovery / integration tests

## Package 3

1. MemoryBundle
2. MemoryLoader bundle API
3. agent Memory lock
4. Sleep candidate generation
5. full snapshot set
6. publication protocol
7. startup recovery
8. crash-point tests


## Integration

1. startup order
2. shutdown order
3. RuntimeStatus
4. metrics
5. architecture documentation
6. session lifecycle documentation
7. DB documentation
8. Memory documentation

---

# 9. End-to-end test scenarios

## 9.1 Durable Turn

1. Webhook受信
2. accepted commit
3. 202 response
4. process停止
5. Runtime再起動
6. DispatcherがTurnを取得
7. Turn完了
8. target channelへresponse送信

## 9.2 Accepted before execution

1. session AでTurn実行中
2. session Aへ2件目をaccept
3. 2件目がDB accepted
4. process停止
5. Runtime再起動
6. 2件目を1回だけ実行

## 9.3 Input committed recovery

1. user inputとsessionをcommit
2. stateがinput_committed
3. model_pending前にprocess停止
4. Runtime再起動
5. inputを再insertせずmodel loop開始
6. Turn完了
