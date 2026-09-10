# EgoPulse Reasoning Effort 設定 詳細設計

## 1. 目的

EgoPulse の Agent ごとに `reasoning_effort` を設定できるようにする。

同じ Provider / Model を利用する Agent 同士でも、それぞれの役割に応じて推論量を変えられる構成とする。

例:

```yaml
agents:
  default:
    label: Default Agent
    provider: openai-codex
    model: gpt-5.3-codex
    reasoning_effort: medium

  reviewer:
    label: Reviewer
    provider: openai-codex
    model: gpt-5.3-codex
    reasoning_effort: high
```

この設定により、`default` Agent は `medium`、`reviewer` Agent は `high` の effort で LLM を呼び出す。

基準リポジトリ:

- Repository: `endo-ly/egopulse`
- 基準 commit: `da2399eb6a63a9dfb00828c5d7642ecf40678a0e`

---

## 2. 設計概要

`reasoning_effort` は `AgentConfig` に追加する。

```text
AgentConfig
  │
  ├ provider
  ├ model
  └ reasoning_effort
        │
        ▼
resolve_llm_for_agent_channel()
        │
        ▼
ResolvedLlmConfig
        │
        ▼
OpenAiProvider
        │
        ├ Chat Completions
        │    └ reasoning_effort
        │
        └ Responses / Codex
             └ reasoning.effort
```

値は文字列として保持する。

```rust
pub reasoning_effort: Option<String>
```

EgoPulse は設定された effort 値を保持し、利用する API の request shape に変換して送信する。

設定が省略されている Agent は Provider 側の既定動作を利用する。

---

## 3. 設定場所

### 3.1 AgentConfig

現在の `AgentConfig` は Agent ごとの Provider / Model 選択を保持している。

現行構造:

```rust
pub(crate) struct AgentConfig {
    pub label: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub discord_bot: Option<BotId>,
    pub telegram_bot: Option<BotId>,
    pub profiles: HashMap<String, AgentProfileConfig>,
}
```

ここへ `reasoning_effort` を追加する。

```rust
pub(crate) struct AgentConfig {
    pub label: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub discord_bot: Option<BotId>,
    pub telegram_bot: Option<BotId>,
    pub profiles: HashMap<String, AgentProfileConfig>,
}
```

これにより Provider / Model / Reasoning Effort が Agent の LLM 実行設定として同じ場所にまとまる。

### 3.2 YAML

```yaml
agents:
  default:
    label: Default Agent
    provider: openai-codex
    model: gpt-5.3-codex
    reasoning_effort: medium

  coder:
    label: Coder
    provider: openai-codex
    model: gpt-5.3-codex
    reasoning_effort: high

  lightweight:
    label: Lightweight Agent
    provider: openai
    model: gpt-5.6-luna
    reasoning_effort: low
```

Provider 定義は現在の役割を維持する。

```yaml
providers:
  openai-codex:
    label: OpenAI Codex
    base_url: https://chatgpt.com/backend-api/codex
    default_model: gpt-5.3-codex
    models:
      gpt-5.3-codex: {}
```

`reasoning_effort` は Provider や Model の定義には置かない。

---

## 4. Agent単位に置く理由

Reasoning Effort は、モデルそのものの固定属性よりも Agent の処理方針として扱う方が実際の利用方法に合う。

例えば同じモデルでも、

```text
General Agent
→ medium

Code Reviewer
→ high

Simple Utility Agent
→ low
```

という使い分けが成立する。

ModelConfig に設定した場合は、

```text
gpt-5.3-codex
→ 常に high
```

となり、同じモデルを利用する Agent 全体へ設定が波及する。

AgentConfig に置くことで、

```text
Agent A
  model = gpt-5.3-codex
  effort = medium

Agent B
  model = gpt-5.3-codex
  effort = high
```

を直接表現できる。

現在の EgoPulse でも Provider / Model は Agent ごとに指定できるため、Reasoning Effort を同じ単位に揃えることで設定構造も理解しやすくなる。

---

## 5. 設定値の型

### 5.1 内部型

```rust
pub reasoning_effort: Option<String>
```

を使用する。

API へ送る値を文字列として保持する。

例:

```yaml
reasoning_effort: low
```

```yaml
reasoning_effort: medium
```

```yaml
reasoning_effort: high
```

```yaml
reasoning_effort: xhigh
```

### 5.2 設定値の正規化

YAML から読み込んだ値には既存の `normalize_string()` を使う。

概念:

```rust
reasoning_effort: normalize_string(file_agent.reasoning_effort),
```

これにより、

```yaml
reasoning_effort: ""
```

や空白だけの値は `None` として扱う。

内部状態は、

```text
Some("high")
None
```

の2種類になる。

### 5.3 AgentConfig の Debug

`AgentConfig` は手書きの `Debug` 実装を持っているため、`reasoning_effort` も表示対象へ追加する。

```rust
.field("reasoning_effort", &self.reasoning_effort)
```

設定値は秘密情報ではなく、実効設定の確認に利用できる。

### 5.4 未設定時

```yaml
agents:
  default:
    label: Default Agent
```

の場合:

```text
AgentConfig.reasoning_effort
= None
```

最終 request には reasoning effort 関連フィールドを追加せず、Provider の既定動作を利用する。

---

## 6. Config Loader

### 6.1 FileAgentConfig

現在の `FileAgentConfig`:

```rust
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileAgentConfig {
    label: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    discord_bot: Option<String>,
    telegram_bot: Option<String>,
    profiles: Option<HashMap<String, FileAgentProfileConfig>>,
}
```

ここへ追加する。

```rust
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileAgentConfig {
    label: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    reasoning_effort: Option<String>,
    discord_bot: Option<String>,
    telegram_bot: Option<String>,
    profiles: Option<HashMap<String, FileAgentProfileConfig>>,
}
```

### 6.2 normalize_agents

`normalize_agents()` で内部 `AgentConfig` へ変換する。

概念:

```rust
AgentConfig {
    label,
    provider,
    model,
    reasoning_effort: normalize_string(fa.reasoning_effort),
    discord_bot,
    telegram_bot,
    profiles,
}
```

既存の Agent ID、Provider reference、Bot reference の validation はそのまま利用する。

---

## 7. Config Persist

現在の保存処理では `AgentConfig` から `SerializableAgent` を生成している。

現行:

```rust
struct SerializableAgent {
    label: String,
    provider: Option<String>,
    model: Option<String>,
    discord_bot: Option<String>,
    telegram_bot: Option<String>,
    profiles: HashMap<String, SerializableAgentProfile>,
}
```

`reasoning_effort` を追加する。

```rust
struct SerializableAgent {
    label: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    discord_bot: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    telegram_bot: Option<String>,

    #[serde(skip_serializing_if = "HashMap::is_empty")]
    profiles: HashMap<String, SerializableAgentProfile>,
}
```

`From<&Config> for SerializableConfig` の Agent 変換でも値をコピーする。

```rust
reasoning_effort: a.reasoning_effort.clone(),
```

これにより config の更新・保存後も effort が保持される。

---

## 8. LLM設定解決

### 8.1 現行経路

通常の Agent turn は現在、

```text
Agent + Channel
    │
    ▼
resolve_llm_for_agent_channel()
    │
    ▼
ResolvedLlmConfig
```

という流れで Provider / Model を解決している。

Provider / Model の現在の優先順位は、

```text
AgentProfile
    ↓
Agent
    ↓
Global
    ↓
Provider default
```

となっている。

### 8.2 Reasoning Effort

Reasoning Effort は Agent から取得する。

```rust
let reasoning_effort = agent.reasoning_effort.clone();
```

結果:

```rust
ResolvedLlmConfig {
    provider,
    label,
    base_url,
    api_key,
    model,
    reasoning_effort,
}
```

AgentProfile が Provider / Model を切り替えた場合も、Reasoning Effort は Agent 自身の設定を使用する。

```text
Agent
  reasoning_effort = high

  profiles:
    discord:
      model = model-a

    telegram:
      model = model-b
```

この場合:

```text
Discord
→ model-a + high

Telegram
→ model-b + high
```

Reasoning Effort を Agent の実行方針として一貫させる。

---

## 9. ResolvedLlmConfig

### 9.1 フィールド

現在:

```rust
pub(crate) struct ResolvedLlmConfig {
    pub provider: String,
    pub label: String,
    pub base_url: String,
    pub api_key: Option<SecretString>,
    pub model: String,
}
```

変更後:

```rust
pub(crate) struct ResolvedLlmConfig {
    pub provider: String,
    pub label: String,
    pub base_url: String,
    pub api_key: Option<SecretString>,
    pub model: String,
    pub reasoning_effort: Option<String>,
}
```

### 9.2 Debug

`Debug` に effort を追加する。

```rust
.field("reasoning_effort", &self.reasoning_effort)
```

### 9.3 PartialEq

`PartialEq` に effort を含める。

```rust
self.reasoning_effort == other.reasoning_effort
```

### 9.4 Cache Key

現在の Provider cache key は、

```text
revision
provider
label
base_url
model
api_key
```

を利用している。

ここへ `reasoning_effort` を追加する。

```rust
self.reasoning_effort.hash(&mut hasher);
```

理由は、同じ Provider / Model を使う Agent でも effort が異なれば異なる Provider instance が必要になるため。

例:

```text
Agent A
provider = openai-codex
model = gpt-5.3-codex
effort = medium

Agent B
provider = openai-codex
model = gpt-5.3-codex
effort = high
```

cache key を分けることで、

```text
Provider Instance A
→ medium

Provider Instance B
→ high
```

として保持される。

---

## 10. OpenAiProvider

### 10.1 フィールド

現在:

```rust
pub(crate) struct OpenAiProvider {
    http: reqwest::Client,
    api_key: Option<String>,
    model: String,
    base_url: String,
    provider: String,
    account_id: Option<String>,
    is_codex: bool,
}
```

変更後:

```rust
pub(crate) struct OpenAiProvider {
    http: reqwest::Client,
    api_key: Option<String>,
    model: String,
    reasoning_effort: Option<String>,
    base_url: String,
    provider: String,
    account_id: Option<String>,
    is_codex: bool,
}
```

### 10.2 初期化

`OpenAiProvider::new()` で `ResolvedLlmConfig` からコピーする。

```rust
reasoning_effort: config.reasoning_effort.clone(),
```

Provider instance が request を送る際は、この値を request builder へ渡す。

---

## 11. Chat Completions Request

現在の `build_request_body()`:

```rust
pub(crate) fn build_request_body(
    model: &str,
    system: &str,
    messages: &[Message],
    tools: Option<&[ToolDefinition]>,
    stream: Option<bool>,
    include_reasoning_content: bool,
) -> serde_json::Value
```

`reasoning_effort` を追加する。

```rust
pub(crate) fn build_request_body(
    model: &str,
    system: &str,
    messages: &[Message],
    tools: Option<&[ToolDefinition]>,
    stream: Option<bool>,
    reasoning_effort: Option<&str>,
    include_reasoning_content: bool,
) -> serde_json::Value
```

request body 生成時:

```rust
if let Some(effort) = reasoning_effort {
    body["reasoning_effort"] =
        serde_json::Value::String(effort.to_string());
}
```

例:

```json
{
  "model": "example-model",
  "messages": [
    {
      "role": "user",
      "content": "..."
    }
  ],
  "reasoning_effort": "high"
}
```

設定が `None` の場合は既存 request shape のままになる。

---

## 12. Responses / Codex Request

現在の `build_responses_request_body()`:

```rust
pub(crate) fn build_responses_request_body(
    model: &str,
    system: &str,
    messages: &[Message],
    tools: Option<&[ToolDefinition]>,
) -> serde_json::Value
```

変更後:

```rust
pub(crate) fn build_responses_request_body(
    model: &str,
    system: &str,
    messages: &[Message],
    tools: Option<&[ToolDefinition]>,
    reasoning_effort: Option<&str>,
) -> serde_json::Value
```

設定値が存在する場合:

```rust
body["reasoning"] = serde_json::json!({
    "effort": effort,
});
```

生成例:

```json
{
  "model": "example-model",
  "input": [
    {
      "type": "message",
      "role": "user",
      "content": "..."
    }
  ],
  "reasoning": {
    "effort": "high"
  }
}
```

Codex 経路では既存処理によって、

```json
{
  "stream": true,
  "store": false
}
```

も追加される。

完成形:

```json
{
  "model": "example-model",
  "input": [...],
  "reasoning": {
    "effort": "high"
  },
  "stream": true,
  "store": false
}
```

---

## 13. API形式の責務

Reasoning Effort の request 表現は API 方式によって決める。

```text
Chat Completions
    │
    ▼
"reasoning_effort": "high"

Responses
    │
    ▼
"reasoning": {
    "effort": "high"
}
```

変換責務は `src/llm/messages.rs` に置く。

`OpenAiProvider` は現在どちらの API を利用するかを決定し、Builder はその API の request shape を生成する。

この既存境界をそのまま利用する。

---

## 14. 通常Agent Turnの最終フロー

```text
egopulse.config.yaml

agents:
  reviewer:
    provider: openai-codex
    model: gpt-5.3-codex
    reasoning_effort: high

        │
        ▼

FileAgentConfig
reasoning_effort = Some("high")

        │
        ▼

AgentConfig
reasoning_effort = Some("high")

        │
        ▼

resolve_llm_for_agent_channel()

        │
        ▼

ResolvedLlmConfig
provider = "openai-codex"
model = "gpt-5.3-codex"
reasoning_effort = Some("high")

        │
        ▼

Provider cache

        │
        ▼

OpenAiProvider
reasoning_effort = Some("high")

        │
        ▼

Responses Request

{
  "model": "gpt-5.3-codex",
  "reasoning": {
    "effort": "high"
  },
  "input": [...],
  "stream": true,
  "store": false
}
```

---

## 15. 複数Agentでの動作

設定:

```yaml
agents:
  assistant:
    provider: openai-codex
    model: gpt-5.3-codex
    reasoning_effort: medium

  reviewer:
    provider: openai-codex
    model: gpt-5.3-codex
    reasoning_effort: high
```

解決結果:

```text
assistant
  ↓
ResolvedLlmConfig
  model = gpt-5.3-codex
  reasoning_effort = medium

reviewer
  ↓
ResolvedLlmConfig
  model = gpt-5.3-codex
  reasoning_effort = high
```

Provider / Model が同じでも cache key が異なるため、それぞれの effort を保持した Provider instance が利用される。

Multi-Agent room でも各 Agent の turn ごとに現在の Agent ID から LLM 設定が解決されるため、Agent 単位の effort がそのまま適用される。

---

## 16. `/model` との関係

現在 `/model` は Agent または global scope の model を変更する。

Agent scope でモデルを変更した場合、Agent の `reasoning_effort` はそのまま保持する。

例:

初期状態:

```yaml
agents:
  reviewer:
    model: model-a
    reasoning_effort: high
```

`/model model-b` 実行後:

```yaml
agents:
  reviewer:
    model: model-b
    reasoning_effort: high
```

モデル切替後も、Agent に設定された Reasoning Effort を継続して使用する。

---

## 17. 適用経路

`reasoning_effort` は `SurfaceContext.agent_id` を持つ通常の Agent Turn に適用する。

| 実行経路 | LLM解決 | Reasoning Effort |
|---|---|---|
| TUI | `resolve_llm_for_agent_channel()` | 対象 Agent の設定 |
| Discord | `resolve_llm_for_agent_channel()` | 対象 Agent の設定 |
| Telegram | `resolve_llm_for_agent_channel()` | 対象 Agent の設定 |
| Web / Voice | `resolve_llm_for_agent_channel()` | 対象 Agent の設定 |
| `egopulse -p` one-shot | `resolve_global_llm()` | Provider の既定動作 |
| Sleep Batch / Events Extract | `resolve_sleep_batch_llm()` | Provider の既定動作 |

TUI は Local Runtime API 側で session から Agent ID を解決し、通常の Agent Turn へ投入しているため、Agent ごとの effort がそのまま適用される。

`ResolvedLlmConfig` には全経路で同じフィールドを持たせるため、`resolve_global_llm()` と `resolve_sleep_batch_llm()` では `reasoning_effort: None` を設定する。


---

## 18. エラー処理

EgoPulse は `reasoning_effort` の文字列を request へ渡す。

Provider がその値を受け付けなかった場合は、現在の API error 処理を利用する。

既存経路:

```text
HTTP non-success
    │
    ▼
LlmError::ApiError
```

エラーは既存の `LlmError::ApiError` で扱う。

YAML の空文字や空白だけの値は loader で `None` に正規化する。

---

## 19. 変更ファイル

| ファイル | 変更内容 |
|---|---|
| `src/config/types.rs` | `AgentConfig.reasoning_effort`、`ResolvedLlmConfig.reasoning_effort` 追加。両ConfigのDebug、ResolvedLlmConfigのEq / cache key 更新 |
| `src/config/loader.rs` | `FileAgentConfig.reasoning_effort` 追加、`normalize_agents()` で正規化 |
| `src/config/persist.rs` | `SerializableAgent.reasoning_effort` 追加 |
| `src/config/resolve.rs` | Agent の effort を `ResolvedLlmConfig` へ解決 |
| `src/llm/openai.rs` | Provider instance に effort を保持して request builder へ渡す |
| `src/llm/messages.rs` | Chat Completions / Responses の request body へ effort を反映 |
| `docs/config.md` | Agent 設定項目と YAML 例を追加 |
| `docs/openai-codex.md` | Codex request に `reasoning.effort` が反映されることを追記 |

既存テストの fixture で `AgentConfig` / `ResolvedLlmConfig` を直接生成している箇所は、新フィールド追加に合わせて更新する。

---

## 20. テスト

実装完了後に今回の変更点を確認する回帰テストを追加する。

### 20.1 Config Load

```yaml
agents:
  reviewer:
    reasoning_effort: high
```

を読み込んだ結果:

```rust
config.agents["reviewer"].reasoning_effort
== Some("high")
```

になることを確認する。

### 20.2 Config Save / Reload

`AgentConfig.reasoning_effort = Some("high")` の config を保存し、再読込後も `high` が保持されることを確認する。

### 20.3 Resolve

同じ Provider / Model を使用する2 Agent:

```text
agent-a → medium
agent-b → high
```

について、

```text
resolve_llm_for_agent_channel(agent-a)
→ medium

resolve_llm_for_agent_channel(agent-b)
→ high
```

になることを確認する。

### 20.4 Cache Key

以下の2設定:

```text
same provider
same model
effort = medium
```

```text
same provider
same model
effort = high
```

から生成される cache key が異なることを確認する。

### 20.5 Chat Completions

`reasoning_effort = Some("high")` の場合:

```json
{
  "reasoning_effort": "high"
}
```

が request body に入ることを確認する。

`None` の場合は既存 request body と同じになることを確認する。

### 20.6 Responses

`reasoning_effort = Some("high")` の場合:

```json
{
  "reasoning": {
    "effort": "high"
  }
}
```

が入ることを確認する。

### 20.7 Codex

Reasoning Effort を追加した状態でも既存の、

```json
{
  "stream": true,
  "store": false
}
```

が維持されることを確認する。

---

## 21. 実装順序

### Step 1: Config型

`src/config/types.rs`

- `AgentConfig.reasoning_effort`
- `AgentConfig::Debug`
- `ResolvedLlmConfig.reasoning_effort`
- `ResolvedLlmConfig::Debug`
- `PartialEq`
- cache key

を更新する。

### Step 2: Loader / Persist

`src/config/loader.rs`

```text
YAML
→ FileAgentConfig
→ AgentConfig
```

へ effort を追加する。

`src/config/persist.rs`

```text
AgentConfig
→ SerializableAgent
→ YAML
```

へ effort を追加する。

### Step 3: Resolve

`resolve_llm_for_agent_channel()` で Agent の effort を `ResolvedLlmConfig` へ渡す。

`resolve_global_llm()` と `resolve_sleep_batch_llm()` では `None` を設定する。

### Step 4: Provider

`OpenAiProvider::new()` で effort を保持する。

全 Chat Completions / Responses request builder call に effort を渡す。

### Step 5: Request Builder

`src/llm/messages.rs` で API ごとの request shape へ変換する。

### Step 6: 回帰テスト

Config、Resolve、Cache、Request body の変更点を確認する。

### Step 7: Documentation

`docs/config.md` と `docs/openai-codex.md` を更新する。

### Step 8: Repository Check

```bash
cargo fmt --check
cargo test
cargo check
cargo clippy --all-targets --all-features -- -D warnings
```

を実行する。

---

## 22. Acceptance Criteria

1. `agents.<agent-id>.reasoning_effort` を YAML で設定できる
2. TUI / Discord / Telegram / Web / Voice の Agent Turn で対象 Agent の effort が利用される
3. 同じ Provider / Model を利用する Agent ごとに異なる effort を設定できる
4. Agent の effort が `ResolvedLlmConfig` へ伝播する
5. effort が Provider cache key に含まれる
6. Chat Completions request へ `reasoning_effort` が反映される
7. Responses / Codex request へ `reasoning.effort` が反映される
8. effort 未設定時は Provider 既定動作を利用する
9. `/model` によるモデル変更後も Agent の effort が保持される
10. config save / reload 後も Agent の effort が保持される
11. 既存 Codex request の `stream: true` / `store: false` が維持される
12. `cargo test` が成功する
13. `cargo check` が成功する
14. `cargo clippy --all-targets --all-features -- -D warnings` が成功する
15. `docs/config.md` と `docs/openai-codex.md` が実装内容と一致する

---

## 23. 最終設定例

```yaml
default_provider: openai-codex
default_agent: general

agents:
  general:
    label: General
    provider: openai-codex
    model: gpt-5.3-codex
    reasoning_effort: medium

  coder:
    label: Coder
    provider: openai-codex
    model: gpt-5.3-codex
    reasoning_effort: high

  reviewer:
    label: Reviewer
    provider: openai-codex
    model: gpt-5.3-codex
    reasoning_effort: xhigh

providers:
  openai-codex:
    label: OpenAI Codex
    base_url: https://chatgpt.com/backend-api/codex
    default_model: gpt-5.3-codex
    models:
      gpt-5.3-codex: {}
```

最終的な設定責務は次の形になる。

```text
Provider
→ 接続先・認証・利用可能モデル

Agent
→ 使用 Provider
→ 使用 Model
→ Reasoning Effort

Request Builder
→ API形式への変換
```
