# Repository Stabilization Phase 3 Plan

## 1. 目的

本 Plan が解決するのは次の4点である。

1. `202 Accepted` を返した Webhook を、Turn 開始前のプロセス停止で失わない
2. Runtime が起動した長寿命 task と Turn task を所有し、順序立てて停止できる
3. Sleep Memory の DB 状態と Markdown ファイルを、クラッシュ後も同じ世代へ復旧できる
4. LLM が実行する Bash Tool を、EgoPulse 本体の host 権限から分離する

本 Phase は、現在存在する信頼性・整合性・権限分離上の問題を解消するためのものである。

全面的な Event Sourcing、全 Channel の Durable Queue 化、Memory 専用の新しい正本モデル、汎用 Workflow Engine は導入しない。

---

## 2. 背景

### 2.1 Webhook は永続化前に受付成功を返し得る

現在の Webhook は、入力を in-memory Turn Scheduler へ投入できた時点で `202 Accepted` を返す。

そのため、次の順序で入力が失われ得る。

```text
Webhook受信
  ↓
TurnSchedulerへ投入
  ↓
202 Accepted
  ↓
Turnがturn_runsへ永続化される前にプロセス停止
  ↓
外部送信元は成功済みと判断し、再送しない
```

Phase 2 により Turn 開始後の状態は永続化されたが、Turn になる前の受付状態はまだ永続化されていない。

### 2.2 Runtime が background task を完全には所有していない

MCP reconnect loop、Agent Turn worker、Channel listener、Scheduler、個々の Turn などが複数箇所で起動されている。

一部は `JoinHandle` を保持せず、Runtime 全体として次を保証できない。

- task panic の検知
- 長寿命 task の異常終了の可視化
- shutdown 中の新規受付停止
- 実行中 Turn の drain
- shutdown deadline 超過時の abort
- MCP や Scheduler の停止順序

### 2.3 Sleep Memory の DB と Markdown は同一 transaction では更新できない

Sleep Memory は DB と filesystem の両方へ状態を保存する。

通常のエラーでは rollback 相当の処理を行えても、Markdown 更新直後にプロセスが停止すると、その補償処理は実行されない。

Phase 3 では DB と filesystem を単一 transaction に入れるのではなく、DB 上に再公開可能な状態を先に残し、クラッシュ後に同じ世代を再公開できる protocol を作る。

### 2.4 Bash Tool は host process と同じ権限で動作する

現在の Bash Tool は EgoPulse process から host 上の `bash` を直接起動する。

command guard、path guard、timeout、output truncation、secret redaction は存在するが、これらは OS sandbox ではない。

LLM が Shell Tool を利用できる以上、Prompt Injection や誤操作により次が起こり得る。

- workspace 外の host filesystem 参照
- workspace 外への書込み
- parent process の環境変数参照
- network access
- CPU / memory の過剰消費
- child process の残留

Phase 3 では Built-in Bash Tool を対象として、host 権限から分離する。

---

## 3. 設計原則

### 3.1 Durable にする対象を限定する

Durable Queue の対象は Webhook に限定する。

Discord、Telegram、Web、CLI の全入力を DB Queue 経由へ変更しない。

Webhook は外部 API として `202 Accepted` を返すため、受付済み入力を失わない契約が必要である。

### 3.2 新規テーブルは1つに限定する

新規テーブルは `ingress_jobs` のみ追加する。

以下の新規テーブルは作らない。

- `memory_generations`
- `memory_generation_files`
- `task_registry`
- `task_runs`
- `job_attempts`
- `dead_letters`
- `agent_locks`
- `capability_grants`
- `supervisor_state`

Memory publication は既存の `sleep_runs`、`sleep_run_steps`、`memory_snapshots` を利用する。

Supervisor、lock、capability policy は process memory または Config で管理する。

### 3.3 不明な状態を推測で再実行しない

既存の fail-stop 方針を維持する。

shutdown deadline を超えた Turn や、完了を証明できない Tool を「たぶん安全」と判断して再開しない。

再起動時は既存の Turn / Tool recovery により `failed` または `uncertain` へ安全停止する。

### 3.4 OS sandbox 不在時に host 実行へ fallback しない

`sandboxed` が指定されている環境で sandbox backend が利用できない場合、Bash Tool は無効化または明示的エラーとする。

暗黙に host 上で直接実行しない。

---

## 4. Scope

### 4.1 対象

- Runtime Supervisor
- root cancellation
- 長寿命 task ownership
- Turn task ownership
- graceful shutdown
- shutdown deadline
- Durable Webhook Ingress
- `ingress_jobs`
- Webhook Job worker
- Job lease / retry / dead letter
- Secret scope 対応
- Recoverable Memory Publication
- per-agent Memory read/write lock
- 起動時 Memory recovery
- Built-in Bash Tool の OS sandbox
- sandbox availability validation
- Runtime 起動順序の統一

### 4.2 対象外

- 全 Channel の Durable Queue 化
- `turn_runs` を汎用 Job Queue として利用すること
- 汎用 Supervisor framework
- task 状態の DB 永続化
- task 自動 restart policy の一般化
- Memory の Event Sourcing
- DB を日常的な Memory 読込の正本へ変更すること
- Markdown 外部編集の DB 同期
- MCP Server 全体の sandbox 化
- Browser sandbox
- 全 OS で同一の sandbox backend を提供すること
- Channel identity の全面的な型再設計
- `AppState` の全面解体
- Cargo / Web build 境界の整理
- Built-in docs manifest

最後の4項目は後続 Plan で扱う。

---

# 5. Package 1 — Runtime Supervisor

## 5.1 目的

Runtime が起動した長寿命 task と実行中 Turn を所有し、異常終了・shutdown・deadline 超過を管理できるようにする。

## 5.2 Supervisor が所有する対象

- Channel listener
- Agent Turn worker
- Durable Ingress worker
- MCP reconnect loop
- Sleep scheduler
- Pulse scheduler
- Backup scheduler
- 実行中 Turn task

単発の短い補助 Future まで汎用 registry へ登録する必要はない。

Runtime の存続期間に関係する task と、shutdown 時に待つ必要がある task を所有対象とする。

## 5.3 構成

概念的には次の要素を持つ。

```text
RuntimeSupervisor
├── root CancellationToken
├── long_lived_tasks: JoinSet
├── turn_tasks: JoinSet または owned handle registry
├── accepting_inputs: AtomicBool
├── shutdown_started: AtomicBool
└── RuntimeStatus
```

既存の `AppState` は Composition Root として残してよい。

Supervisor の導入を理由に、Phase 3 内で `AppState` を全面的に分解しない。

## 5.4 task 起動

長寿命 task は、直接 `tokio::spawn` して handle を捨てない。

Supervisor を通じて起動し、少なくとも次を記録する。

- task kind
- task name
- critical / non-critical
- 正常終了か
- panic / error の有無

DB へ task 状態は保存しない。

## 5.5 task 異常終了

### Critical task

例:

- Agent Turn worker
- Durable Ingress worker
- 必須 Channel listener

予期せず終了した場合は RuntimeStatus へ記録し、新規受付を停止する。

自動 restart は本 Phase の必須要件にしない。

### Non-critical task

例:

- 補助的な定期処理
- optional Channel
- optional reconnect loop

異常終了を RuntimeStatus と log へ記録する。

Runtime 全体を停止するかは既存機能の重要度に応じて決定する。

## 5.6 shutdown 順序

```text
1. accepting_inputs = false
2. Webhook / Channel の新規入力受付停止
3. 新しい Job claim と Turn start を停止
4. Scheduler の新規 dispatch を停止
5. 実行中 Turn と Memory publication の完了を待つ
6. Durable Ingress worker を停止
7. MCP / Sleep / Pulse / Backup 等を停止
8. deadline 超過 task を abort
9. DB と Runtime を終了
```

shutdown 中、既に queue 済みの次の Turn を新たに開始しない。

## 5.7 Turn の扱い

Turn 実行中に cancellation が通知された場合、可能な安全地点で終了する。

ただし、外部副作用の完了状態を証明できない場合は、再実行可能状態へ戻さない。

deadline 超過により task が abort された場合、次回起動時に 既存の recovery を通じて `failed` または `uncertain` へ移行する。

## 5.8 完了条件

- Runtime が所有していない長寿命 task が残っていない
- shutdown 開始後に新しい Turn が開始されない
- 実行中 Turn を deadline まで待てる
- deadline 超過 taskを abort できる
- task panic が RuntimeStatus へ反映される
- MCP reconnect loop を停止できる
- shutdown が特定 Channel の終了待ちで無期限停止しない

---

# 6. Package 2 — Durable Webhook Ingress

## 6.1 目的

`202 Accepted` を返した Webhook 入力を、Turn 開始前のプロセス停止で失わないようにする。

## 6.2 `turn_runs` と分ける理由

`turn_runs` は、Turn として durable accept された後の実行状態を管理する。

`ingress_jobs` は、Turn になる前の外部入力を管理する。

```text
Webhook delivery
  ↓
ingress_jobs
  ↓
Turn durable accept
  ↓
turn_runs
```

対象とライフサイクルが異なるため、`turn_runs` を Queue として流用しない。

## 6.3 新規テーブル

```sql
CREATE TABLE ingress_jobs (
    job_id          TEXT PRIMARY KEY,
    receiver_id     TEXT NOT NULL,
    request_key     TEXT NOT NULL,
    target_channel  TEXT NOT NULL,
    target_thread   TEXT NOT NULL,
    agent_id        TEXT NOT NULL,
    input_text      TEXT NOT NULL,
    state           TEXT NOT NULL,
    attempt_count   INTEGER NOT NULL DEFAULT 0,
    available_at    TEXT NOT NULL,
    lease_until     TEXT,
    turn_id          TEXT,
    last_error       TEXT,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL,
    CHECK (state IN ('queued', 'running', 'handed_off', 'dead_letter')),
    UNIQUE (receiver_id, request_key)
);
```

必要な index を追加する。

```sql
CREATE INDEX idx_ingress_jobs_claim
    ON ingress_jobs(state, available_at, lease_until);

CREATE INDEX idx_ingress_jobs_receiver_created
    ON ingress_jobs(receiver_id, created_at);

CREATE INDEX idx_ingress_jobs_turn_id
    ON ingress_jobs(turn_id);
```

## 6.4 Normal / Secret DB

Job は解決済み target の Conversation Scope に対応する DB へ保存する。

- Normal target → Normal DB
- Secret target → Secret DB

Secret Webhook の payload、input text、target metadata を Normal DB へ保存しない。

Normal DB と Secret DB の双方へ同じ `ingress_jobs` schema を導入する。

## 6.5 受付フロー

```text
Webhook受信
  ↓
receiver解決
  ↓
token認証
  ↓
payload size / JSON validation
  ↓
target channel / thread / agent / scope解決
  ↓
Agent入力へ正規化
  ↓
request_key決定
  ↓
対象DBのingress_jobsへINSERT
  ↓
COMMIT成功
  ↓
202 Accepted + job_id
```

DB commit に失敗した場合は `202 Accepted` を返さない。

## 6.6 Request Key

優先順位は次とする。

1. `Idempotency-Key` header
2. payload の明示的な `event_id`
3. payload の明示的な `message_id`
4. receiver が対応する外部イベント固有 ID
5. ランダム生成した Job ID

明示的な ID がない場合、payload hash による deduplication は行わない。

同じ内容のイベントが複数回実際に起こり得るため、同一 payload を同一イベントとみなさない。

## 6.7 Worker claim

単一 process 内で複数 worker が存在しても、同じ Job を同時実行しないよう transaction 内で claim する。

```text
queued
  ↓ claim
running + lease_until
```

claim 対象は次を満たす Job とする。

- `state = queued`
- `available_at <= now`

または、

- `state = running`
- `lease_until < now`

後者は前回 worker の停止による lease 失効として扱う。

## 6.8 Turn への handoff

Job は Scheduler へ投入しただけでは `handed_off` にしない。

```text
Job claim
  ↓
ScheduledTurn生成
  ↓
Turn durable accept
  ↓
turn_runs.turn_id確定
  ↓
ingress_jobs.turn_id保存
  ↓
state = handed_off
```

TurnScheduler の in-memory queue に入っただけの状態は、handoff 完了ではない。

## 6.9 重複 handoff

worker 再実行時、同じ `receiver_id + request_key` に対応する Turn が既に存在する場合は、新しい Turn を作らない。

既存 Turn の `turn_id` を Job へ関連付け、`handed_off` にする。

既存の Turn request key deduplication と組み合わせ、Job worker の再実行でも重複 Turn を防止する。

## 6.10 Retry

一時的な内部エラーに限り retry する。

retry 時は次を更新する。

- `attempt_count += 1`
- `state = queued`
- `available_at = backoff後`
- `lease_until = NULL`
- `last_error`

retry 回数を超えた Job は `dead_letter` とする。

専用の attempt table と dead letter table は作らない。

## 6.11 Queue capacity と retention

Config で少なくとも次を設定可能にする。

- 最大未処理 Job 件数
- 最大 retry 回数
- lease duration
- retry backoff 上限
- handed_off retention
- dead_letter retention

capacity 超過時は DB に保存せず、明示的な 429 または 503 を返す。

既に `202 Accepted` を返した Job を capacity 理由で後から捨てない。

## 6.12 Job Status API

```text
GET /api/webhooks/{receiver_id}/jobs/{job_id}
```

receiver token で認証する。

`input_text`、payload、Secret 情報、内部 error detail をそのまま返さない。

## 6.13 完了条件

- DB commit 成功後にのみ 202 を返す
- 202 後、worker 開始前に停止しても Job が残る
- worker claim 後の停止で lease recovery できる
- Turn durable accept 前の停止で再処理できる
- Turn durable accept 後は重複 Turn を作らない
- Secret Job が Normal DB に保存されない
- queue capacity が有界
- dead letter が確認可能
- retention で完了済み Job を削除できる

---

# 7. Package 3 — Recoverable Memory Publication

## 7.1 目的

Sleep Memory の3つの Markdown ファイルを、1つの Sleep Run に対応する同じ世代として公開し、公開途中のクラッシュから復旧できるようにする。

## 7.2 新規テーブルを作らない理由

既存の `memory_snapshots` は次を保持している。

- `run_id`
- `agent_id`
- `file`
- `content_before`
- `content_after`

`sleep_run_id` を Memory generation ID として利用できる。

そのため、同等の情報を持つ `memory_generations` や `memory_generation_files` は追加しない。

## 7.3 正本の扱い

Markdown ファイルは引き続き通常の Memory 読込元とする。

DB は、Sleep が生成した Memory を安全に再公開するための durable publication record とする。

Phase 3 では「DB を日常的な Memory 正本へ変更する」仕様変更は行わない。

## 7.4 Step 内でファイルを更新しない

各 Memory Step は、LLM 処理と DB commit までを行う。

```text
Step開始
  ↓
LLM処理
  ↓
候補Memory生成
  ↓
memory_snapshotsへcontent_before / content_after保存
  ↓
sleep_run_stepsをsuccessへ更新
```

各 Step の途中では、公開中の Markdown を変更しない。

## 7.5 3ファイルすべての snapshot

最終 publication 前に、1つの Sleep Run について次の3行が存在することを保証する。

- episodic
- semantic
- prospective

変更されなかった Memory file についても、現在内容を `content_before` と `content_after` に保存する。

これにより、1つの run_id だけで完全な Memory 状態を再構築できる。

## 7.6 Publication protocol

```text
1. 全Memory Step完了
2. 3ファイルの最終content_afterを確定
3. DB transaction:
   - 3つのmemory_snapshotsを確認
   - sleep_runs.status = publishing
4. per-agent Memory write lock取得
5. agent専用staging directoryへ3ファイル生成
6. fsync可能な範囲で内容を確定
7. staging generationをcurrentへ切替
8. 3ファイルの公開状態を確認
9. DBでsleep_runs.status = success
10. write lock解放
```

## 7.7 ファイル切替方式

単純に3つの既存ファイルへ順番に上書きすると、途中状態を Turn が読み得る。

そのため、3ファイルを一つの generation directory として生成し、current generation の参照を atomic に切り替える方式を優先する。

```text
agents/{agent_id}/memory/
├── generations/
│   ├── {run_id}/
│   │   ├── episodic.md
│   │   ├── semantic.md
│   │   └── prospective.md
│   └── ...
└── current -> generations/{run_id}
```

platform 上で directory symlink / rename の扱いが不適切な場合は、同等の atomic manifest 切替を使う。

重要なのは、Turn が3ファイルを別世代から読む期間を作らないことである。

## 7.8 per-agent Memory lock

Agent ごとに read/write lock を持つ。

Sleep の LLM 処理中ずっと Turn を停止しない。

### Turn

```text
read lock取得
  ↓
current generationから3ファイル読込
  ↓
Turn用Memory snapshot生成
  ↓
read lock解放
```

Turn は読み込んだ Memory snapshot を Turn 完了まで使用する。

### Sleep publication

```text
write lock取得
  ↓
generation切替
  ↓
write lock解放
```

lock は process memory で管理し、DB table は追加しない。

## 7.9 起動時 recovery

Channel と Scheduler の開始前に、`sleep_runs.status = publishing` を検索する。

```text
memory_snapshotsを取得
  ↓
episodic / semantic / prospectiveの3行を確認
  ↓
同じrun_idのgenerationを再生成
  ↓
currentをそのgenerationへ切替
  ↓
sleep_runs.status = success
```

snapshot が不足している場合は、その run を成功扱いにしない。

暗黙に不整合な Markdown を使用し続けない。

## 7.10 手動編集

Phase 3 では、公開済み Markdown の手動編集を DB へ逆同期する仕組みは導入しない。

本 Phase の中心は Sleep publication のクラッシュ整合性であり、双方向同期ではない。

## 7.11 完了条件

- Step 成功前に公開中 Markdown を変更しない
- 1 run_id で3ファイルを完全再構築できる
- Turn が異なる generation の3ファイルを混在して読まない
- Sleep LLM処理中はTurnを長時間blockしない
- 1ファイル目更新後のクラッシュから復旧できる
- 2ファイル目更新後のクラッシュから復旧できる
- current切替後・DB success前のクラッシュから復旧できる
- publishing run の自動 recovery が Channel 起動前に完了する
- snapshot 不足を安全側へ倒す

---

# 8. Package 4 — Bash OS Sandbox

## 8.1 目的

Built-in Bash Tool を EgoPulse 本体の host 権限から分離する。

対象はまず Bash Tool に限定する。

Tool Policy の全面的一般化や MCP process 全体の sandbox 化は後続 Plan で扱う。

## 8.2 Policy

権限を単一 enum にまとめず、直交する項目として扱う。

```yaml
tools:
  bash:
    process: sandboxed
    filesystem: workspace_write
    network: disabled
    timeout_secs: 30
    memory_mb: 512
    cpu_seconds: 30
    env:
      allowed:
        - SOME_SKILL_TOKEN
```

## 8.3 Default

default は `sandboxed` とする。

sandbox backend が利用できない場合、Bash Tool は fail-closed でエラーを返す。

host 実行を許可するのは、明示的に `host_trusted` が設定された場合だけとする。

## 8.4 Sandbox要件

最低限、次を保証する。

- workspace 外 filesystem を原則参照できない
- filesystem policy に応じて read-only / read-write を切り替える
- network は default disabled
- parent process の環境変数を継承しない
- allowlist された Skill env だけを注入する
- process group 単位で timeout kill できる
- child process が残留しない
- CPU limit
- memory limit
- stdout / stderr の既存上限を維持する
- Secret scope は Normal workspace を mount しない
- temporary output も scope 外へ漏らさない

## 8.5 Platform

最初の backend は、現在 EgoPulse を実運用する主要 platform 向けに実装する。

未対応 platform では `sandboxed` を host 実行へ fallback させない。

## 8.6 Environment

Bash process には `env_clear()` 相当を適用する。

最低限必要な実行環境だけを明示的に構築する。

- `PATH`
- locale
- sandbox 内 HOME
- sandbox 内 TMPDIR
- allowlist Skill env

EgoPulse process が持つ provider token、Webhook token、DB path、Secret 設定値などを継承しない。

## 8.7 Defense in Depth

既存の次の防御は維持する。

- command guard
- path guard
- timeout
- process group kill
- output truncation
- secret redaction
- Tool execution ledger

ただし、これらを sandbox の代替とは扱わない。

## 8.8 Host Trusted

`host_trusted` は明示的な危険設定とする。

- default ではない
- configuration documentation で警告する
- RuntimeStatus または startup log へ明示する
- Secret Channel では原則拒否する
- unrestricted parent env 継承は行わない

## 8.9 完了条件

- workspace 外 read を拒否する
- workspace 外 write を拒否する
- symlink escape を拒否する
- network access を default で拒否する
- parent env を継承しない
- allowlist env のみ注入する
- timeout 時に process tree を終了する
- CPU / memory limit が機能する
- backend 不在時に fail-closed になる
- explicit `host_trusted` のみ host 実行する
- Secret scope と Normal workspace を分離する

---

# 9. Package 5 — Startup and Cutover

## 9.1 起動順序

```text
Config読込
  ↓
DB open
  ↓
migration / backup
  ↓
Turn / Tool recovery
  ↓
Memory publication recovery
  ↓
Durable Ingress lease recovery
  ↓
Sandbox capability validation
  ↓
Runtime Supervisor開始
  ↓
Workers開始
  ↓
Schedulers開始
  ↓
Channels開始
  ↓
accepting_inputs = true
```

Memory や DB の recovery が完了する前に外部入力を受け付けない。

## 9.2 Migration

新規 DB migration は `ingress_jobs` のみとする。

Normal / Secret schema version を更新する。

Memory publication 用の新規 table は追加しない。

---

# 10. Observability

## 10.1 Metrics

少なくとも次を追加する。

- `ingress_jobs_queued`
- `ingress_jobs_running`
- `ingress_jobs_dead_letter`
- `ingress_job_retries_total`
- `ingress_job_handoff_total`
- `runtime_owned_tasks`
- `runtime_task_failures_total`
- `runtime_shutdown_aborts_total`
- `memory_publication_recoveries_total`
- `memory_publication_failures_total`
- `sandbox_executions_total`
- `sandbox_failures_total`
- `sandbox_host_trusted_executions_total`

## 10.2 RuntimeStatus

次を確認可能にする。

- accepting inputs
- shutdown started
- critical task failure
- Durable Ingress worker state
- queued / dead-letter Job count
- Memory recovery failure
- sandbox availability
- host_trusted有効化

payload、secret、Tool env の実値は出さない。

---

# 11. Test Plan

## 11.1 Durable Ingress

- DB commit失敗時に202を返さない
- 202後、worker開始前に停止してもJobが残る
- claim直後の停止でlease recoveryされる
- Scheduler投入後、Turn accept前の停止で再処理される
- Turn accept後はJobがhanded_offになる
- Job再実行で重複Turnを作らない
- 同一Idempotency-Keyが同一Jobに収束する
- IDなしで同じpayloadを2回送ると別Jobになる
- retry backoff
- retry上限超過でdead_letter
- queue capacity
- handed_off retention
- dead_letter retention
- Secret JobがNormal DBへ保存されない
- Status APIの認証と情報制限

## 11.2 Runtime Supervisor

- Channel受付停止後に新規Turnが開始されない
- shutdown中に次のqueued Turnを開始しない
- 実行中Turnがdeadline内でdrainされる
- deadline超過Turnがabortされる
- task panicがRuntimeStatusへ記録される
- critical task終了で受付停止する
- MCP reconnect loopが停止する
- optional task失敗が無視されず観測される
- shutdownが無期限にhangしない

## 11.3 Memory Publication

- Step中にMarkdownが変更されない
- 3種類のsnapshotが1 run_idに揃う
- 変更なしのfileもsnapshotされる
- 1ファイル目生成後のクラッシュ復旧
- 2ファイル目生成後のクラッシュ復旧
- current切替後・DB success前のクラッシュ復旧
- Turnが3ファイルの異なるgenerationを読まない
- Sleep LLM処理中はTurnを不必要にblockしない
- publishing runが起動時に再公開される
- snapshot不足時に安全側へ停止する
- 旧generationがrecovery失敗時も維持される

## 11.4 Sandbox

- workspace外read拒否
- workspace外write拒否
- absolute path escape拒否
- `..` escape拒否
- symlink escape拒否
- network拒否
- parent env非継承
- allowlist envのみ注入
- timeout時process tree終了
- background child残留防止
- memory limit
- CPU limit
- output truncation維持
- backendなしでfail-closed
- explicit host_trustedのみhost実行
- Secret workspace分離

## 11.5 Integration

- Webhook受信からJob永続化、Turn durable accept、最終応答まで
- Webhook受付後のプロセス再起動
- shutdown中のWebhook拒否
- Sleep publication中のshutdown
- Memory recovery完了前にChannelが開始されない
- sandboxed Bashを含むTurnの正常完了
- sandboxed Bash中のshutdown

---

# 12. Definition of Done

1. `202 Accepted` を返した Webhook が Turn 開始前の停止で消失しない
2. Webhook Job と Turn の二重実行を防止できる
3. Secret Webhook Job が Normal DB に保存されない
4. Runtime がすべての長寿命 task を所有する
5. shutdown時に受付停止、drain、abortが順序立てて行われる
6. shutdown開始後に新しいTurnが開始されない
7. Sleep Memoryが3ファイルの混在状態で読まれない
8. Memory公開途中のクラッシュから同じrun_idを再公開できる
9. Bash Toolがdefaultでhost権限を使用しない
10. sandboxが利用不能な環境で暗黙のhost fallbackをしない
11. 新規テーブルは`ingress_jobs`の1つだけである
12. Normal / Secret DB の migration と recovery が検証されている
13. Runtime、Webhook、Sleep、Tool のdocumentationが実装と一致している

---

# 13. Phase 3 後に残す課題

以下はこのPRの完了を妨げない。

- Discord / Telegram / Web の Durable Queue 化
- typed Ingress Envelope の全面導入
- Channel入力経路の統一
- `AppState` の追加分解
- Tool Policy の他Toolへの展開
- MCP process sandbox
- Cargo build と Web build の分離
- Built-in docs manifest
- Memory手動編集の正式な同期仕様

これらは `plan-runtime-boundary-cleanup.md` で扱う。