# Runtime Boundary Cleanup Plan

## 1. 目的

Repository Stabilization Phase 3 では、現在存在する信頼性・整合性・権限分離上の問題を解消する。

- Durable Webhook Ingress
- Runtime Supervisor
- Recoverable Memory Publication
- Bash OS Sandbox

本 Plan では、その安全境界を型・モジュール・Build 構造へ反映し、今後の Channel 追加、Tool Policy 拡張、Runtime 改修を行いやすくする。

この Plan の中心は、現在の実害を解消することではなく、Phase 3 で導入した安全な仕組みを重複なく、一貫した境界から利用できるようにすることである。

---

## 2. 背景

### 2.1 Channel 入力の型と処理経路が platform ごとに分散する

Discord、Telegram、Web、Webhook、CLI は、それぞれ異なる platform event を受け取る。

Runtime へ渡す際には共通して次が必要になる。

- actor
- conversation
- agent
- scope
- request key
- input content
- attachment
- received time
- durable / best-effort の受付方針

Phase 3 では Webhook の durability を優先し、全 Channel の全面的な型統一は行わない。

後続では、既存 `channel_input` を発展させ、platform 固有処理と Runtime ingress を明確に分離する。

### 2.2 `AppState` は Composition Root と feature dependency を兼ねやすい

Phase 2 で Turn 実行は `TurnRuntime` に分離された。

Phase 3 では Supervisor、Ingress、Memory publication、Sandbox にも個別の依存境界が生まれる。

それでも各 feature module が `AppState` 全体を参照すると、不要な依存へアクセスでき、テスト構築も重くなる。

ただし、`AppState` の存在自体を問題とはしない。

`AppState` は Composition Root として残し、feature へ必要な依存だけを渡す。

### 2.3 Bash 用 Policy と他 Tool の制御が別々になり得る

Phase 3 では Built-in Bash Tool を優先して sandbox 化する。

その後、File Tool、Web Fetch、MCP Tool、Secret Channel などにも権限制御を広げる場合、個別の boolean や例外分岐を増やすと Policy が分散する。

後続では、Phase 3 の実装を基に Tool Policy を一般化する。

### 2.4 Cargo build が Web と docs の生成責務を抱えている

現在の build では、Web asset の状態に応じて Cargo build 中に npm command が実行され得る。

また、`docs/` 全体が Built-in Skill の references へ取り込まれる。

この構造には次の問題がある。

- Cargo build が Node.js / npm に暗黙依存する
- offline build が不安定になる
- source tree が build 中に変更され得る
- development plan や内部文書まで release binary に取り込まれる
- Web artifact の生成責務が CI / Release と Cargo の間で曖昧になる

本 Plan で Build 境界を整理する。

---

## 3. 設計原則

### 3.1 動作変更を目的にしない

この Plan は原則として Phase 3 で確立した動作を維持する。

- Webhook durability
- Turn / Tool fail-stop
- Memory publication recovery
- Supervisor shutdown
- Bash sandbox

構造整理により、これらの意味論を変えない。

### 3.2 `AppState` を消すことを目的にしない

完了条件は、

> feature module が `AppState` を一切参照しない

ではない。

完了条件は、

> feature module が不要な Scheduler、Channel、DB、Config、Tool へ偶発的に依存できない

である。

### 3.3 全 Channel を Durable Queue へ移行しない

typed ingress を導入しても、すべての入力を `ingress_jobs` に保存する必要はない。

- Webhook: durable
- Discord / Telegram / Web / CLI: 既存方針を維持

durability は Ingress Envelope の属性または呼出経路で明示する。

### 3.4 不要な抽象化を増やさない

interface / trait / wrapper は、複数の実装やテスト置換に意味がある場合だけ追加する。

単に `AppState` の field を1つずつ包むだけの Repository class や Manager class は追加しない。

### 3.5 DB変更を原則行わない

本 Plan では新規テーブルを追加しない。

`ingress_jobs` に追加情報が必要になった場合も、実際の利用要件が確認できた column だけを追加する。

---

## 4. Scope

### 4.1 対象

- typed Ingress boundary
- `IngressEnvelope`
- typed actor / conversation / request key
- `channel_input` の整理
- durable / best-effort ingress の共通 handoff
- narrow runtime dependencies
- `SleepRuntime`
- `IngressRuntime`
- `ToolRuntime`
- `SupervisorRuntime`
- AppState composition の整理
- Tool Policy generalization
- File / Web Fetch / MCP visibility policy
- Secret scope Tool policy
- Cargo / Web build boundary
- Web artifact generation command
- Built-in docs manifest
- Runtime docs と development docs の分離

### 4.2 対象外

- Webhook durability の再設計
- 全 Channel の durable queue 化
- Runtime task の DB 永続化
- Memory正本の変更
- Memory generation table
- OS sandbox backend の全面作り直し
- MCP Server process の必須 sandbox 化
- DB permission table
- plugin architecture の全面刷新
- monorepo 分割
- frontend framework の変更

---

# 5. Package 1 — Typed Ingress Boundary

## 5.1 目的

platform event を Runtime の入力へ変換する境界を一つにまとめ、Channel ごとの差異が Turn 実行や persistence へ漏れないようにする。

## 5.2 命名

既存の systemd 管理用 `runtime::gateway` と混同しないよう、`ChannelGateway` という名前は使用しない。

候補:

- `IngressRouter`
- `ChannelIngress`
- `ConversationIngress`

本 Plan では概念名を `IngressRouter` とする。

## 5.3 IngressEnvelope

概念モデル:

```rust
pub struct IngressEnvelope {
    pub source: IngressSource,
    pub actor: ActorId,
    pub conversation: ConversationAddress,
    pub agent_id: AgentId,
    pub scope: ConversationScope,
    pub request_key: RequestKey,
    pub content: IngressContent,
    pub attachments: Vec<IngressAttachment>,
    pub received_at: DateTime<Utc>,
    pub durability: IngressDurability,
}
```

### IngressSource

```text
Web
Discord
Telegram
Webhook(receiver_id)
Cli
AgentSend
```

### IngressDurability

```text
BestEffort
Durable
```

Phase 3 時点では Webhook が `Durable`、その他は原則 `BestEffort` となる。

## 5.4 Typed Identity

最低限、次を newtype または enum として定義する。

```text
ActorId
ConversationAddress
RequestKey
IngressJobId
ExternalEventId
```

### ConversationAddress

単なる文字列の連結規則を各 Channel に持たせない。

概念的には次を保持する。

```text
channel
surface_thread
surface_user / actor
agent_id
chat_type
```

ただし DB schema まで全面変更しない。

既存の session key や external chat id への変換は boundary 内へ閉じ込める。

## 5.5 Channel の責務

各 Channel adapter の責務は次までとする。

- platform event を受信する
- platform authentication / filtering
- platform 固有 ID を読み取る
- attachment metadata を正規化する
- `IngressEnvelope` を構築する
- `IngressRouter` へ渡す
- platform 向け受付結果を返す

次を Channel adapter へ残さない。

- TurnScheduler の直接操作
- DB の直接選択
- request key の独自 fallback
- agent session key の文字列生成
- scope routing の重複実装
- queue capacity metric の個別記録

## 5.6 IngressRouter の責務

```text
IngressEnvelope受領
  ↓
identity / scope validation
  ↓
durability判定
  ├── Durable → ingress_jobsへ保存
  └── BestEffort → Turn submit
  ↓
共通SubmitOutcome
```

Phase 3 の Durable Webhook 実装を再利用する。

Webhook 専用 worker と BestEffort submit が、最終的に同じ Turn durable accept 境界へ到達するようにする。

## 5.7 Request Key

platform ごとの request key 生成を明示する。

- Discord: message / interaction ID
- Telegram: update ID または message ID
- Web: client request ID
- Webhook: Idempotency-Key / event ID / random
- CLI: command invocation UUID
- AgentSend: parent Turn + Tool Call ID + child target

fallback を各 adapter に散らさない。

## 5.8 Attachments

Phase 3 で attachment durability を実装していない場合、typed model だけ先に導入し、既存挙動を維持する。

attachment byte の新しい DB table は追加しない。

必要に応じて既存 AssetStore への参照を持つ。

## 5.9 完了条件

- 全 Channel が共通 IngressEnvelope を構築する
- TurnScheduler の直接操作が Channel adapter から消える
- scope routing が一箇所に集約される
- request key 生成規則が明示される
- Webhook の durable semantics が変わらない
- BestEffort Channel が不要に DB Queue 化されない
- Session identity の文字列生成が platform code に散らばらない

---

# 6. Package 2 — Narrow Runtime Dependencies

## 6.1 目的

各 feature が必要な依存だけを受け取り、`AppState` 全体への偶発的依存を防ぐ。

## 6.2 Runtime boundaries

Phase 3 後の実際の利用箇所に合わせ、次を導入または整理する。

```text
TurnRuntime
SleepRuntime
IngressRuntime
ToolRuntime
SupervisorRuntime
```

すべてを trait にする必要はない。

clone 可能な dependency bundle struct で十分な箇所は struct とする。

## 6.3 TurnRuntime

既存の境界を維持する。

Turn 実行に不要な以下を持たせない。

- Channel listener ownership
- Runtime shutdown coordinator
- Job retention scheduler
- Build artifact state

Phase 3 で追加された Memory snapshot 読込境界と sandboxed Tool execution を適切に参照する。

## 6.4 SleepRuntime

Sleep に必要な依存をまとめる。

候補:

- normal DB
- Config snapshot / Sleep config
- LLM provider resolution
- Memory publication service
- Memory lock registry
- MemoryLoader
- timezone

次を持たせない。

- ChannelRegistry
- TurnScheduler
- Webhook config
- Runtime-wide queue tracker

Sleep が Turn の内部実装へ直接依存しないようにする。

## 6.5 IngressRuntime

候補:

- Config snapshot
- DB routing
- Durable Ingress storage
- Turn submit boundary
- queue capacity
- Runtime acceptance state
- metrics / RuntimeStatus

Channel adapter は `IngressRuntime` または `IngressRouter` だけを参照する。

## 6.6 ToolRuntime

候補:

- ToolRegistry
- Sandbox backend
- Tool policy resolver
- Secret redaction
- MCP manager
- scope-aware workspace resolver

Bash policy の一般化に合わせて整理する。

## 6.7 SupervisorRuntime

Supervisor 自身が Runtime 全体の feature internals を持たないようにする。

保持するのは主に次とする。

- cancellation
- JoinSet / handle registry
- acceptance state
- shutdown deadline
- RuntimeStatus

## 6.8 AppState

`AppState` は構築と wiring を担う Composition Root として残す。

主な責務:

- Config から各 dependency を構築
- Normal / Secret DB を構築
- Runtime boundary を組み立てる
- Supervisor へ worker を登録
- Channel adapter を登録
- 起動順序を制御

feature 処理そのものを `AppState` method に増やさない。

## 6.9 過剰な抽象化を避ける

以下を作らない。

- fieldごとの単純なRepository wrapper
- 実装が1つしかなく置換予定もないtrait
- methodをそのまま転送するManager
- 型を隠すだけのService Locator
- `dyn Trait` 化による不要なallocation

## 6.10 完了条件

- Channel feature が DB / Scheduler / Config 全体を直接参照しない
- Sleep feature が ChannelRegistry を参照しない
- Tool feature が TurnScheduler を参照しない
- Supervisor が feature domain logic を持たない
- AppState は wiring 中心になる
- test が全Runtime起動なしで各featureを構築できる
- 依存整理だけを理由に動作意味論を変更しない

---

# 7. Package 3 — Tool Policy Generalization

## 7.1 目的

Phase 3 で Bash Tool に導入した Policy を、必要な Tool と scope へ一貫して適用できるようにする。

## 7.2 Policy model

概念モデル:

```rust
pub struct ToolPolicy {
    pub enabled: bool,
    pub filesystem: FilesystemPolicy,
    pub process: ProcessPolicy,
    pub network: NetworkPolicy,
    pub environment: EnvironmentPolicy,
    pub timeout: Duration,
    pub output_limit: OutputLimit,
}
```

すべての Tool が全項目を利用する必要はない。

Tool kind ごとに意味のある policy だけを評価する。

## 7.3 Tool class

最低限、次を区別する。

```text
Read-only filesystem
Write filesystem
Process execution
Network access
Message / external side effect
MCP delegated tool
Internal control tool
```

既存の `is_read_only` と idempotency classification は維持する。

OS capability と retry safety を同じ分類に統合しない。

## 7.4 File Tool

対象:

- read
- write
- edit
- grep
- find
- ls

検討項目:

- workspace read / write
- Secret workspace
- symlink policy
- generated temp file
- max file size
- binary file
- hidden file
- protected path

Phase 3 の Bash sandbox と同じ workspace resolver を利用し、Bash と File Tool で異なる workspace 判定を持たない。

## 7.5 Web Fetch

network policy を適用する。

候補:

- disabled
- unrestricted
- allowlist domain
- private / loopback address拒否
- redirect先再検証

既存 security policy と統合し、別の URL guard を増やさない。

## 7.6 MCP Tool

MCP Tool は外部 server が持つ capability を完全には判断できない。

まず次を実装対象とする。

- MCP server 単位の有効 / 無効
- Agent 単位 visibility
- Channel scope 単位 visibility
- Secret scope での明示許可
- Tool name allow / deny

MCP process 自体の OS sandbox は本 Plan の必須要件にしない。

## 7.7 Agent / Channel policy

Config で global default と override を表現する。

```text
global default
  ↓
agent override
  ↓
channel / scope override
  ↓
tool-specific override
```

override の優先順位を明示し、複数箇所で独自 merge しない。

## 7.8 Secret scope

Secret scope では少なくとも次を厳しくできるようにする。

- host_trusted process拒否
- Normal workspace拒否
- unrestricted network拒否
- Secret未対応MCP Tool拒否
- external send Tool制限

Secret scopeだからすべて無効、という固定仕様にはしない。

明示された capability だけ許可する。

## 7.9 Policy snapshot

Turn 開始時の Config snapshot に基づき、Tool definitions と Tool Policy を固定する。

Turn途中の Config reload により、表示された Tool definitions と実行時 policy が食い違わないようにする。

## 7.10 DB

permission / capability の新規 table は作らない。

Policy は Config の責務とする。

Tool execution の実績は既存 `tool_calls` ledger を利用する。

## 7.11 完了条件

- Bash と File Tool が同じ workspace resolver を使う
- network policy が Web Fetch に一貫して適用される
- MCP visibility がAgent / scope単位で制御できる
- Tool definition と実行 policy が同じ Config snapshot に基づく
- Secret scope で危険 capability を明示的に制限できる
- retry safety と OS capability が混同されない
- permission tableを追加しない

---

# 8. Package 4 — Cargo / Web Build Boundary

## 8.1 目的

Cargo build と Web artifact 生成の責務を分離し、offline・再現可能・source tree 非変更の build を実現する。

## 8.2 現在の問題

Cargo build 中に Web asset が不足または古いと判断された場合、npm command が起動され得る。

その結果、Cargo build が次へ暗黙依存する。

- Node.js
- npm
- package registry
- `web/node_modules`
- source tree 上の `web/dist`

Rust code の確認だけをしたい場合でも、Web build failure が影響する。

## 8.3 新しい責務分離

```text
Web build command
  ↓
npm ci
  ↓
typecheck
  ↓
test
  ↓
vite build
  ↓
versioned Web artifact生成

Rust build
  ↓
生成済みWeb artifactを検証
  ↓
OUT_DIRへコピー
  ↓
binaryへembed
```

Cargo build は npm を起動しない。

## 8.4 Artifact location

source tree の `web/dist` を通常 build の生成先として使わない方針を優先する。

候補:

```text
target/egopulse-web/{profile}/
.artifacts/web/
CI artifact download location
```

local development と Release が同じ artifact contract を使う。

ただし repository へ巨大な build asset を常時 commit することは前提にしない。

## 8.5 Explicit command

local developer 向けに明示的な command を用意する。

例:

```text
cargo xtask web-build
just web-build
scripts/build-web
```

どの方式を採用しても、Cargo build script から package manager を起動しない。

## 8.6 Build.rs

`build.rs` の責務を次に限定する。

- artifact存在確認
- artifact manifest確認
- OUT_DIRへのコピー
- include source生成
- Built-in docs manifestに基づくコピー

次を行わない。

- npm install
- npm ci
- npm run build
- network access
- source treeへの生成物書込み

## 8.7 Artifact freshness

mtime 比較だけに依存しない。

Web artifact manifest に最低限次を持たせる。

```text
source hash
package-lock hash
build command version
artifact version
generated_at
```

Rust build は manifest と期待値を比較し、古い artifact の場合は明確なエラーを返す。

自動再生成はしない。

## 8.8 CI

CI の順序を明示する。

```text
Web:
  npm ci
  typecheck
  test
  build
  artifact upload

Rust:
  Web artifact取得または同一job内生成
  cargo fmt
  cargo check
  cargo clippy
  cargo test
  cargo doc
```

Release Workflow でも Web artifact 生成成功後に Rust binary を build する。

## 8.9 Source tree

build / test により tracked file が変更されないことを検証する。

必要に応じて CI で次を確認する。

```text
git diff --exit-code
```

## 8.10 完了条件

- Cargo buildがnpmを起動しない
- Cargo buildがnetworkへ接続しない
- Cargo buildがsource treeを書き換えない
- Web artifact不足時に明確に失敗する
- local用の明示的Web build commandがある
- CI / Release がartifactを必ず生成する
- artifact freshnessがmtimeだけに依存しない
- Rust-only変更の開発体験がWeb環境に過剰依存しない

---

# 9. Package 5 — Built-in Documentation Manifest

## 9.1 目的

Runtime binary に組み込む文書を明示し、development plan、内部設計資料、過去文書が自動的にBuilt-in Skillへ含まれないようにする。

## 9.2 Manifest

runtime へ組み込む文書を allowlist で定義する。

候補:

```text
src/assets/builtin-skills/egopulse/manifest.toml
builtin-docs.toml
```

例:

```toml
[[document]]
source = "docs/architecture.md"
target = "references/architecture.md"

[[document]]
source = "docs/config.md"
target = "references/config.md"

[[document]]
source = "docs/tools.md"
target = "references/tools.md"
```

## 9.3 文書分類

### Runtime user documentation

- architecture
- config
- channels
- tools
- memory
- API
- deployment
- security

必要なものだけmanifestへ登録する。

### Built-in Skill specific references

Skill が回答に利用するために必要な要約・参照資料。

Runtime user docs と同じ内容を無条件にすべてコピーしない。

### Development plans

- `docs/plan/*`
- 実装計画
- migration plan
- temporary review plan

binaryへ組み込まない。

### Historical / internal documents

- 過去設計
- review記録
- internal investigation
- deprecated specs

binaryへ組み込まない。

## 9.4 Validation

build 時または専用 test で次を確認する。

- manifest sourceが存在する
- target pathが重複しない
- path traversalがない
- directory全体の暗黙コピーがない
- manifest未登録文書がbinaryへ入らない

## 9.5 Built-in Skill

Built-in Skill の `SKILL.md` から参照される文書が manifest に存在することを検証する。

文書削除やrenameでbroken referenceを作らない。

## 9.6 完了条件

- `docs/`全体を再帰コピーしない
- runtime組み込み文書がmanifestで明示される
- `docs/plan`がbinaryへ含まれない
- manifest source / targetが検証される
- Built-in Skill reference切れをtestで検出する
- Build中にsource treeを書き換えない

---

# 10. Migration Strategy

## 10.1 進め方

推奨順序:

```text
1. IngressEnvelopeとtyped identity
2. 既存channel_inputをIngressRouterへ移行
3. Runtime dependency bundle整理
4. Tool Policy一般化
5. Web build明示コマンド
6. build.rsからnpm削除
7. Built-in docs manifest
8. 旧経路削除
```

1つのPR内で実施してよいが、各Packageの途中で旧経路と新経路を長期間併存させない。

## 10.2 DB

原則 DB migration なし。

`ingress_jobs` に column が必要になった場合、利用箇所とqueryを明示した上で最小限追加する。

新規 identity table、permission table、build metadata table は作らない。

## 10.3 Config

Tool Policy の一般化に伴い Config schema は変更され得る。

互換用の二重モデルを長期間維持しない。

migration / validation / persist の3経路を同時に更新する。

未対応の矛盾した設定は fail-closed で拒否する。

---

# 11. Test Plan

## 11.1 Typed Ingress

- Discord eventからIngressEnvelope
- Telegram eventからIngressEnvelope
- Web inputからIngressEnvelope
- Webhook durable envelope
- CLI input envelope
- AgentSend child request key
- scope routing
- session identity
- request key stability
- IDなしWebhookのrandom key
- adapterがSchedulerを直接操作しないこと
- Durable / BestEffort semantics維持

## 11.2 Runtime Dependencies

- TurnRuntime単体構築
- SleepRuntime単体構築
- IngressRuntime単体構築
- ToolRuntime単体構築
- feature testがChannel全起動を要求しない
- featureが不要dependencyを持たない
- AppState wiring test
- startup order regression

## 11.3 Tool Policy

- global default
- agent override
- scope override
- tool-specific override
- precedence
- Config snapshot固定
- File read/write policy
- Secret workspace
- Web Fetch network disable
- Web Fetch allowlist
- MCP visibility
- Secret MCP deny
- Bash Phase 3 semantics維持

## 11.4 Build Boundary

- Cargo buildがnpmを起動しない
- artifactなしで明確な失敗
- stale artifactで明確な失敗
- valid artifactで成功
- Web build command成功
- artifact manifest生成
- package-lock変更でstale判定
- source変更でstale判定
- Cargo build後にgit diffがない
- offline Rust build

## 11.5 Documentation Manifest

- source存在
- target重複拒否
- path traversal拒否
- docs/plan非同梱
- manifest登録文書のみ同梱
- Built-in Skill reference検証
- rename regression

## 11.6 Integration

- Discord / Telegram / Web / Webhook が同じ Turn boundaryへ到達する
- Webhook durabilityが維持される
- Secret scope Tool policy
- Config reload後の次TurnでPolicy更新
- 実行中Turnは旧snapshot維持
- Release WorkflowがWeb artifactを含むbinaryを生成する
- Built-in Skillがmanifest文書を参照できる

---

# 12. Definition of Done

1. 全 Channel 入力が共通 IngressEnvelope へ正規化される
2. Webhook の Durable semantics が維持される
3. BestEffort Channel が不要に DB Queue 化されない
4. Channel adapter がTurnSchedulerやDB routingを直接扱わない
5. feature moduleへ必要なRuntime依存だけが渡される
6. AppStateがComposition Root中心になる
7. Tool PolicyがBash以外にも一貫して適用できる
8. Secret scopeで危険Toolを明示的に制限できる
9. Cargo buildがnpm、network、source tree変更を行わない
10. Web artifact生成が明示的な工程になる
11. runtime組み込み文書がmanifestで管理される
12. `docs/plan`がrelease binaryへ含まれない
13. 新規テーブルを追加しない
14. Phase 3で確立したdurability、recovery、sandbox semanticsを変えない

---

# 13. このPlan後に残し得る課題

以下は必要性が確認された場合に別途扱う。

- Discord / Telegram / Web の Durable Queue 化
- attachment byte の durable persistence
- MCP Server process の OS sandbox
- 複数 EgoPulse process による Job worker
- distributed lease
- runtime plugin architecture
- Memory手動編集の正式な双方向同期
- remote build cache
- frontend / backend repository 分割