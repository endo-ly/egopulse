# Plan: Agent Interaction Profiles

エージェントに profiles を追加し、channel 名を profile キーとしてモデル解決に組み込む。profile 未設定時は既存の解決チェーンがそのまま動く。

> **Note**: 振る舞い（What）は決して変えてはいけないが、より美しい設計があれば実装方法（HOW）だけは変えてもよい。

## 設計方針

- **エージェントファースト**: モデル選択の主語は常に agent。channel は自分の名前を渡すだけで、どのモデルを使うかは agent が決める。
- **channel 名 = profile キー**: `SurfaceContext.channel`（例: `"voice"`）をそのまま `agent.profiles` の lookup キーに使う。channel 側に profile フィールドは追加しない。`resolve_llm_for_agent_channel` の未使用 `_channel` 引数が意味を持つようになる。
- **既存互換**: profiles 未設定時は既存の model resolution chain がそのまま動く。既存設定ファイルに変更は不要。
- **解決順**: `agent.profiles[channel].provider/model` → `agent.provider/model` → `config.default_provider/default_model` → `provider.default_model`。
- **最小フィールド**: 初期実装では `profiles.<name>.provider` と `profiles.<name>.model` のみ。構造だけ拡張可能にしておき、実装は小さく始める。`max_output_tokens` 等の実行特性は `models` の役割であり profile には含めない。
- **既存パターン参照**:
  - Config 型: `src/config/types.rs`
  - YAML 読込: `src/config/loader.rs`（`FileAgentConfig` / `normalize_agents`）
  - YAML 書出: `src/config/persist.rs`（`SerializableAgent`）
  - Model 解決: `src/config/resolve.rs`（`resolve_llm_for_agent_channel`）

## TDD 方針

テストリスト項目と自動テストを区別する。1 回の Red では自動テスト 1 件だけを追加し、Green で最小実装、Refactor で整理する。実装中に見つかった不安はテストリストへ戻して次サイクルで扱う。config 系テストは `src/config/tests.rs` に集約する。

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 種別 | 既存パターン / 参照元 | 備考 |
| -- | -- | -- | -- |
| `src/config/types.rs` | 変更 | `AgentConfig` | `AgentProfileConfig` 追加、`AgentConfig.profiles` 追加 |
| `src/config/loader.rs` | 変更 | `FileAgentConfig`, `normalize_agents` | YAML パース拡張 |
| `src/config/persist.rs` | 変更 | `SerializableAgent` | YAML 書き出し拡張 |
| `src/config/resolve.rs` | 変更 | `resolve_llm_for_agent_channel` | `_channel` 引数を用いた profile 解決追加 |
| `src/config/tests.rs` | 変更 | 既存テスト群 | profile 解決テスト追加 |

## テストリスト / 不安リスト

| ID | 観点 | 期待する振る舞い | 優先 | 対応するCycle | 状態 / 今回対象外理由 |
| -- | -- | -- | -- | -- | -- |
| T1 | 正常系 | agent に profiles.voice.model がある場合、channel="voice" で resolve するとその model が返る | High | Step 1 | 未着手 |
| T2 | 正常系 | agent に profiles があるが該当 channel 名の profile がない場合、agent.model へフォールバックする | High | Step 2 | 未着手 |
| T3 | 正常系 | agent.profiles.voice.model と agent.model の両方がある場合、channel="voice" では profile 側が優先される | High | Step 3 | 未着手 |
| T4 | 空・ゼロ状態 | agent に profiles がない場合、既存の解決チェーンがそのまま動く | High | Step 4 | 未着手 |
| T5 | 境界値 | channel に対応する profile がなく、agent にも model がない場合、既存の default_model → provider.default_model チェーンが壊れない | Medium | Step 4 | 未着手 |
| T6 | 正常系 | YAML に agents.orphe.profiles.voice.model: gpt-4.1-mini を書いて load すると AgentConfig.profiles["voice"].model == Some("gpt-4.1-mini") になる | High | Step 5 | 未着手 |
| T7 | 統合 | YAML save-load round trip で profiles が保持される | High | Step 6 | 未着手 |
| T8 | 正常系 | 複数 agent が同じ profile 名で異なる model を持てる（lyre.profiles.voice.model != orphe.profiles.voice.model） | High | Step 3 | 未着手 |
| T9 | 正常系 | profiles.voice.provider が設定されている場合、その provider が解決される。省略時は agent.provider を引き継ぐ | High | Step 1 | 未着手 |

---

## Step 0: Worktree 作成

- ブランチ名: `feat/agent-interaction-profiles`
- 作成コマンド:
  - `git worktree add ../egopulse-profiles -b feat/agent-interaction-profiles`

---

## Step 1: AgentProfileConfig 型と profile 解決 TDD Cycle - channel 名による provider/model 解決

### この Step の目的

`AgentConfig` に `profiles` フィールドを追加し、`resolve_llm_for_agent_channel` が channel 名で profile を lookup して provider と model を返すようにする。

### 今回選ぶ項目

- 対象: `T1`, `T9`
- 選ぶ理由: この機能のコアとなる解決ロジック。最も価値があり、他のすべての Step の基盤になる。provider の解決も含めて最初に確立する。
- この時点では扱わないこと: YAML パース・永続化。

### RED: 失敗する自動テストを書く

- 追加するテスト名 1: `resolve_llm_uses_profile_model_when_channel_matches`
- 追加するテスト名 2: `resolve_llm_uses_profile_provider_when_specified`
- Given (test1): agent に `model: "claude-sonnet-4"` と `profiles: { voice: { model: "gpt-4.1-mini" } }` が設定されている。channel は `"voice"`。
- When (test1): `resolve_llm_for_agent_channel` を agent_id と channel="voice" で呼ぶ。
- Then (test1): 返り値の model が `"gpt-4.1-mini"`、provider は agent と同じ。
- Given (test2): agent の provider が "sakura" だが `profiles: { voice: { provider: "openrouter", model: "gpt-4.1-mini" } }` が設定されている。
- When (test2): channel="voice" で resolve。
- Then (test2): 返り値の provider が "openrouter"、model が `"gpt-4.1-mini"`。
- 失敗理由の想定: `resolve_llm_for_agent_channel` が `_channel` 引数を無視している。

### GREEN: 最小実装

1. `AgentProfileConfig` 型を `src/config/types.rs` に追加: `provider: Option<String>`, `model: Option<String>`。
2. `AgentConfig` に `profiles: HashMap<String, AgentProfileConfig>` を追加。
3. `resolve_llm_for_agent_channel` 内で `_channel` 引数を用いて profile lookup を追加:
   - provider: `profile.provider` → `agent.provider` → `config.default_provider`
   - model: `profile.model` → `agent.model` → `config.default_model` → `provider.default_model`
4. テスト内で Config を手組みして解決を検証。

### REFACTOR: 設計の整理

- 重複: `resolve_llm_for_agent_channel` 内の chain が長くなっていないか。早期 return で平坦に保つ。
- 命名: `AgentProfileConfig` が既存命名規約に沿っているか確認。
- 責務: model 解決が Config のメソッドとして適切か。
- テストの構造的結合: 内部 HashMap 構造に直接依存しすぎていないか。
- 次の項目へ進める身軽さ: profile なしの呼び出しも壊れていないか。

### テストリスト更新

- 完了: `T1`, `T9`
- 追加: なし
- 次候補: `T2`

### コミット

`feat(config): add AgentProfileConfig and channel-based profile model resolution`

---

## Step 2: profile フォールバック TDD Cycle - channel に該当 profile なし

### この Step の目的

channel 名に対応する profile が agent にない場合、既存の agent.model chain にフォールバックすることを確認する。

### 今回選ぶ項目

- 対象: `T2`
- 選ぶ理由: フォールバック動作が既存互換の要。早期に確認する。
- この時点では扱わないこと: YAML パース。

### RED: 失敗する自動テストを書く

- 追加するテスト名: `resolve_llm_falls_back_when_profile_not_found_for_channel`
- Given: agent に `model: "claude-sonnet-4"`。profiles は空か存在しない。channel は "voice"。
- When: `resolve_llm_for_agent_channel` を呼ぶ。
- Then: model が `"claude-sonnet-4"` になる（agent.model へフォールバック）。
- 失敗理由の想定: なし（Step 1 の Green で既にフォールバック実装済みなら即 Green）。

### GREEN: 最小実装

Step 1 の実装で既に対応済みの場合は追加実装なし。テストが通ることを確認。

### REFACTOR: 設計の整理

- profile 解決と通常解決の境界が明確か確認。

### テストリスト更新

- 完了: `T2`
- 追加: なし
- 次候補: `T3`

### コミット

`test(config): verify profile fallback to agent model`

---

## Step 3: profile 優先度確認 TDD Cycle - profile vs agent default と multi-agent

### この Step の目的

`agent.profiles.voice.model` と `agent.model` の両方がある場合、channel="voice" では profile 側が優先されることを確認する。また、異なる agent が同じ profile 名で異なる model を持てることを確認する。

### 今回選ぶ項目

- 対象: `T3`, `T8`
- 選ぶ理由: 優先度の正確性が「エージェントファースト」設計の核心。複数 agent で profile を変えられることはユーザーの主要ユースケース。
- この時点では扱わないこと: YAML。

### RED: 失敗する自動テストを書く

- 追加するテスト名 1: `resolve_llm_profile_takes_priority_over_agent_default_model`
- 追加するテスト名 2: `different_agents_can_have_different_models_for_same_profile`
- Given (test1): agent に `model: "claude-sonnet-4"` と `profiles.voice.model: "gpt-4.1-mini"`。
- When (test1): channel="voice" で resolve。
- Then (test1): model が `"gpt-4.1-mini"`（profile 側が優先）。
- Given (test2): agent lyre に `profiles.voice.model: "gpt-4.1-mini"`、agent orphe に `profiles.voice.model: "claude-3.5-haiku"`。
- When (test2): 両方を channel="voice" で resolve。
- Then (test2): それぞれ異なる model が返る。

### GREEN: 最小実装

Step 1 で profile 優先を実装済みなら追加実装なし。テストが通ることを確認。

### REFACTOR: 設計の整理

- テストの意図が明確か確認。

### テストリスト更新

- 完了: `T3`, `T8`
- 追加: なし
- 次候補: `T4`

### コミット

`test(config): verify profile priority and multi-agent profile independence`

---

## Step 4: profiles なしでの既存互換 TDD Cycle

### この Step の目的

agent に profiles がない場合、既存の解決チェーンが全く変わらず動くことを確認する。

### 今回選ぶ項目

- 対象: `T4`, `T5`
- 選ぶ理由: 既存互換の回帰テスト。これが壊れると全チャネルに影響する。
- この時点では扱わないこと: YAML。

### RED: 失敗する自動テストを書く

- 追加するテスト名: `resolve_llm_without_profiles_unchanged_behavior`
- Given: agent に model 指定、profiles なし。channel は "discord"。
- When: resolve。
- Then: 既存の agent.model → default_model → provider.default_model チェーン通り。

### GREEN: 最小実装

Step 1 で profile なし時は既存 chain を通るよう実装済み。テストが通ることを確認。

### REFACTOR: 設計の整理

- 全チャネル（discord, web, telegram, tui, cli）で profiles がない場合に回帰していないか。

### テストリスト更新

- 完了: `T4`, `T5`
- 追加: なし
- 次候補: `T6`

### コミット

`test(config): verify backward compat when no profiles configured`

---

## Step 5: YAML 読込 - agent profiles TDD Cycle

### この Step の目的

YAML に `agents.orphe.profiles.voice.model: gpt-4.1-mini` を書いて load すると、`AgentConfig.profiles["voice"].model` が正しく読み込まれる。

### 今回選ぶ項目

- 対象: `T6`
- 選ぶ理由: YAML 読込が実運用の入口。設定が反映されないと意味がない。
- この時点では扱わないこと: なし。

### RED: 失敗する自動テストを書く

- 追加するテスト名: `loader_parses_agent_profiles`
- Given: YAML に agent 定義があり `profiles: { voice: { model: "gpt-4.1-mini" } }` を含む。
- When: `build_config` で読込。
- Then: `config.agents["orphe"].profiles["voice"].model == Some("gpt-4.1-mini")`。

### GREEN: 最小実装

1. `FileAgentProfileConfig` を `loader.rs` に追加: `provider: Option<String>`, `model: Option<String>`。
2. `FileAgentConfig` に `profiles: Option<HashMap<String, FileAgentProfileConfig>>` を追加。
3. `normalize_agents` で `profiles` を `AgentProfileConfig` へマッピング。

### REFACTOR: 設計の整理

- FileAgentProfileConfig のフィールド名が AgentProfileConfig と整合しているか。

### テストリスト更新

- 完了: `T6`
- 追加: なし
- 次候補: `T7`

### コミット

`feat(config): parse agent profiles from YAML`

---

## Step 6: YAML 永続化 round trip TDD Cycle

### この Step の目的

YAML save-load round trip で profiles が保持される。

### 今回選ぶ項目

- 対象: `T7`
- 選ぶ理由: WebUI や手動編集後の設定保存が壊れていないことの確認。
- この時点では扱わないこと: なし。

### RED: 失敗する自動テストを書く

- 追加するテスト名: `save_load_round_trip_preserves_agent_profiles`
- Given: agent に profiles.voice を含む Config。
- When: save_config_with_secrets → load_allow_missing_api_key。
- Then: profiles の内容が復元される。

### GREEN: 最小実装

1. `SerializableAgentProfile` を追加: `provider: Option<String>`, `model: Option<String>`。
2. `SerializableAgent` に `profiles` フィールドを追加（`HashMap<String, SerializableAgentProfile>`）。
3. `SerializableConfig::from_config` でマッピング。

### REFACTOR: 設計の整理

- SerializableAgent の profiles マッピングが他フィールドと整合しているか。

### テストリスト更新

- 完了: `T7`
- 追加: なし
- 次候補: すべて完了

### コミット

`feat(config): persist agent profiles in YAML round trip`

---

## Step 7: 動作確認

```bash
cargo fmt --check
cargo test
cargo check
cargo clippy --all-targets --all-features -- -D warnings
```

- 失敗時は該当 Step へ戻る

---

## Step 8: Plan・仕様書との自己チェック

実装完了後にこの Plan と関連仕様書を最初から読み直し、実装・自動テスト・文書が要求した振る舞いと一致しているかを照合する。

- Plan のテストリストと各 Cycle が完了条件を満たしている
- 関連仕様書の What と実装結果が一致している
- 実装中に変更した設計判断が関連 docs へ反映されている
- 変更ファイル一覧、コミット分割、自動テスト一覧が実際の変更と一致している

---

## Step 9: docs 更新

以下の docs を実装に合わせて更新:

- `docs/config.md` §2.10: agent 定義に `profiles` フィールドを追記
- `docs/config.md` §3: モデル解決チェーンに profile 段を追加
- `docs/architecture.md` §4: SurfaceContext の channel フィールド解説に profile lookup の記述を追記

---

## Step 10: PR 作成

- PR タイトル: `feat: agent interaction profiles`
- PR description:
  - 概要: エージェントに profiles を追加し、channel 名をキーにモデル解決を行う機能。voice channel が軽量モデルを使うユースケースに対応。
  - テスト: config 解決、YAML 読込、YAML 永続化の各単位でテスト追加
  - 設計意図: エージェントファースト。モデル選択の主語は常に agent。channel 名をそのまま profile キーとして使う。

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
| ---- | ---- | -- |
| `src/config/types.rs` | 変更 | `AgentProfileConfig` 追加、`AgentConfig.profiles` 追加 |
| `src/config/loader.rs` | 変更 | `FileAgentProfileConfig` 追加、`FileAgentConfig.profiles` 追加、`normalize_agents` 拡張 |
| `src/config/persist.rs` | 変更 | `SerializableAgentProfile` 追加、`SerializableAgent.profiles` 追加 |
| `src/config/resolve.rs` | 変更 | `resolve_llm_for_agent_channel` で `_channel` を用いた profile lookup 追加 |
| `src/config/tests.rs` | 変更 | profile 関連テスト追加 |
| `docs/config.md` | 変更 | §2.10, §3 に profiles 関連追記 |
| `docs/architecture.md` | 変更 | §4 に profile lookup 記述追記 |

---

## コミット分割

1. `feat(config): add AgentProfileConfig and channel-based profile model resolution` - types.rs, resolve.rs, tests.rs
2. `test(config): verify profile fallback to agent model` - tests.rs
3. `test(config): verify profile priority and multi-agent profile independence` - tests.rs
4. `test(config): verify backward compat when no profiles configured` - tests.rs
5. `feat(config): parse agent profiles from YAML` - loader.rs
6. `feat(config): persist agent profiles in YAML round trip` - persist.rs
7. `docs: update config.md, architecture.md for interaction profiles` - docs/

---

## 自動テスト一覧（全 8 件）

この一覧はPlan作成時点で必要と判断した最低限の予定であり、最終テスト件数の上限ではない。実装中に追加された不安には、テストリストとTDD Cycleを追加して対応する。

### config（全 8 件）

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T1 | `resolve_llm_uses_profile_model_when_channel_matches` | Step 1 | `cargo test -- config::tests::resolve_llm_uses_profile_model_when_channel_matches` |
| T9 | `resolve_llm_uses_profile_provider_when_specified` | Step 1 | `cargo test -- config::tests::resolve_llm_uses_profile_provider_when_specified` |
| T2 | `resolve_llm_falls_back_when_profile_not_found_for_channel` | Step 2 | `cargo test -- config::tests::resolve_llm_falls_back_when_profile_not_found_for_channel` |
| T3 | `resolve_llm_profile_takes_priority_over_agent_default_model` | Step 3 | `cargo test -- config::tests::resolve_llm_profile_takes_priority` |
| T8 | `different_agents_can_have_different_models_for_same_profile` | Step 3 | `cargo test -- config::tests::different_agents_different_profile_models` |
| T4 | `resolve_llm_without_profiles_unchanged_behavior` | Step 4 | `cargo test -- config::tests::resolve_llm_without_profiles_unchanged` |
| T6 | `loader_parses_agent_profiles` | Step 5 | `cargo test -- config::tests::loader_parses_agent_profiles` |
| T7 | `save_load_round_trip_preserves_agent_profiles` | Step 6 | `cargo test -- config::tests::save_load_round_trip_preserves_agent_profiles` |

---

## 工数見積もり

| Step | 内容 | 見積もり |
| -- | -- | -- |
| Step 1 | AgentProfileConfig 型と profile 解決 | ~60 行 |
| Step 2 | profile フォールバック確認 | ~15 行 |
| Step 3 | profile 優先度と multi-agent 確認 | ~35 行 |
| Step 4 | profiles なしでの既存互換確認 | ~15 行 |
| Step 5 | YAML agent profiles 読込 | ~40 行 |
| Step 6 | YAML 永続化 round trip | ~40 行 |
| Step 7-9 | 動作確認 / docs / PR | ~80 行 |
| **合計** |  | **~285 行** |
