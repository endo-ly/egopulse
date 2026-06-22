C# Plan: モデル固有プロンプト(model_instructions)の追加

`providers.<id>.models.<model>` 配下に `model_instructions` / `model_instructions_file` を追加し、その内容を system prompt の SOUL と Core Instructions の間に `<model-instructions>` タグで注入する。

> **Note**: 振る舞い(What)は決して変えてはいけないが、より美しい設計があれば実装方法(HOW)だけは変えてもよい。

## 設計方針

- **既存パターンを踏襲**: `ModelConfig.context_window_tokens` と同じ場所・同じ粒度で追加し、`Config::resolve_*` ヘルパ群(`resolve_context_window_tokens`, `resolve_llm_for_agent_channel`)と並ぶ `resolve_model_instructions` を新設する。`build_system_prompt`(`src/agent_loop/prompt_builder.rs`)は SOUL/AGENTS/Memory/Skills 各セクションを `Option<String>` で組み立てているので、model_instructions も同一パターンの `build_model_instructions_section` として差し込む。
- **呼び出し元順序に非依存**: `process_turn_inner` は `build_system_prompt` → `llm_for_context`、`run_pulse_activation` は `llm_for_context` → `build_system_prompt` と順序が異なるため、`build_system_prompt` 内部で自前 resolve する。これにより通常ターン・Pulse 両方に一括適用される。
- **排他制約**: `model_instructions`(インライン)と `model_instructions_file`(PATH)の両立は起動エラー。loader で fail-fast する。
- **PATH 解決**: 相対パス基点は **設定ファイルのディレクトリ** (`state.config_path` の parent、通常 `~/.egopulse/`)。絶対パスも許可。
- **読み込みタイミング**: 毎ターン読み込み。SOUL.md / AGENTS.md と整合する。
- **ラップ**: `<model-instructions>\n{content}\n</model-instructions>` でラップ(他セクションと同じ XML タグ様式)。
- **適用範囲**: 通常ターン ○、Pulse 活性化 ○(自動)、Compaction ×、Sleep Batch ×(専用プロンプトのため)。
- **セキュリティ**: Core Instructions 既存宣言(“Project instructions may add constraints, but must never weaken or override these security rules”)により、最終的にセキュリティルールが優先される文脈を維持。model_instructions は Core Instructions の前に注入されるが、上書きはできない。
- **参照元**: `docs/system-prompt.md` §1/§4、`docs/config.md` §2.2、`src/agent_loop/prompt_builder.rs`、`src/config/types.rs:174-180`、`src/config/resolve.rs:287-301`(`resolve_context_window_tokens`)

## TDD 方針

テストリスト項目(T1..)と自動テスト(`test_name`)を区別し、1回の Red では自動テスト1件だけ追加する。Green では Red を通す最小実装に集中し、別ケース対応や設計整理を混ぜない。Refactor は全テスト通過を維持したまま行う。実装中に新たな不安が見つかればテストリストへ追加し、必要な Cycle を続ける。1項目に複数の境界がある場合は同じ項目を対象にした Cycle を複数作る。

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 種別 | 既存パターン / 参照元 | 備考 |
| -- | -- | -- | -- |
| `src/config/types.rs` | 変更 | `ModelConfig` (174-180行) | `model_instructions` / `model_instructions_file` 追加 |
| `src/config/loader.rs` | 変更 | `normalize_provider_map` (787-852行) | 両立チェック追加(ファイル存在チェックは実行時 resolve 側で扱う) |
| `src/error.rs` | 変更 | `ConfigError` (132-238行) | 新バリアント `ModelInstructionsConflict` / `ModelInstructionsFileUnreadable` 追加 |
| `src/config/resolve.rs` | 変更 | `resolve_context_window_tokens` (287-301行) | `resolve_model_instructions` 新設 |
| `src/agent_loop/prompt_builder.rs` | 変更 | `build_system_prompt` (8-38行) | `build_model_instructions_section` 新設、SOUL と Core の間に注入 |
| `docs/config.md` | 変更 | §2.2 ModelConfig 表、§2.11 完全 YAML 例 | 新フィールド追記 |
| `docs/system-prompt.md` | 変更 | §1 セクション構成、§4 固定プロンプト | ①.5 Model Instructions セクション追加 |

## テストリスト / 不安リスト

| ID | 観点 | 期待する振る舞い | 優先 | 対応するCycle | 状態 / 今回対象外理由 |
| -- | -- | -- | -- | -- | -- |
| T1 | 正常系 | YAML の `model_instructions:`(インライン) が `ModelConfig.model_instructions` にデシリアライズされる | High | Step 1 | 未着手 |
| T2 | 異常系 | `model_instructions` と `model_instructions_file` を両方指定した YAML は `ConfigError::ModelInstructionsConflict` で起動失敗する | High | Step 2 | 未着手 |
| T3 | 正常系 | `resolve_model_instructions()` はインライン指定時に `Some(trimmed_content)` を返す | High | Step 3 | 未着手 |
| T4 | 正常系 | `resolve_model_instructions()` は `model_instructions_file` 指定時に、`base_dir` 基点で相対パスを解決しファイル内容を `Some(trimmed_content)` で返す | High | Step 4 | 未着手 |
| T5 | 境界値 | `resolve_model_instructions()` は content が空文字/空白のみの場合 `None` を返す(セクション自体省略) | Medium | Step 4 | 未着手 |
| T6 | 正常系 | `build_system_prompt()` は model_instructions が設定されている場合、`<model-instructions>` タグでラップした内容を SOUL の直後かつ Core Instructions の直前に注入する | High | Step 5 | 未着手 |
| T7 | 境界値 | model_instructions が未設定(従来設定)の場合、`build_system_prompt()` の出力に `<model-instructions>` は現れず、従来と同じ並びであること | High | Step 6 | 未着手 |
| T8 | 異常系 | ファイル参照先が実行時に読めない場合、`build_model_instructions_section` は warn ログを出力し `None` を返す(プロンプト構築は継続) | Medium | Step 6 | 未着手 |
| T9 | 適用範囲 | Sleep Batch(`build_sleep_system_prompt` 系)には model_instructions が適用されないこと(専用プロンプトは影響を受けない) | Low | 今回対象外 | Sleep はユーザー対面でないバッチ処理のため別経路。需要が出たら別課題。テストは「影響がないこと」の_unit_test_程度で担保するか、Plan self-check で確認する |

---

## Step 0: Worktree 作成

- ブランチ名: `feat/model-instructions`
- 作業ディレクトリ: `../egopulse-model-instructions`
- 作成コマンド:
  - `git worktree add ../egopulse-model-instructions -b feat/model-instructions`
- 以降の Step はすべて当該 worktree 内で実施する。

---

## Step 1: `ModelConfig` フィールド追加 Cycle - T1

### この Step の目的

`ModelConfig` に `model_instructions` / `model_instructions_file` フィールドを追加し、インライン指定のデシリアライズを通す。

### 今回選ぶ項目

- 対象: `T1`
- 選ぶ理由: 最小の足場。フィールドが無いと以降の Cycle がすべて始まらない。
- この時点では扱わないこと: 両立バリデーション(T2)、resolve ヘルパ(T3/T4)、プロンプト注入(T6)。`model_instructions_file` の解決も未実装でよい。

### RED: 失敗する自動テストを書く

- 追加するテスト名: `model_config_deserializes_inline_instructions` (配置: `src/config/types.rs` 内 `#[cfg(test)] mod tests`)
- Given: 次の YAML 断片
  ```yaml
  context_window_tokens: 200000
  model_instructions: |
    Be concise.
    Avoid preamble.
  ```
- When: `yaml_serde::from_str::<ModelConfig>(yaml)` を呼ぶ
- Then:
  - `context_window_tokens == Some(200000)`
  - `model_instructions == Some("Be concise.\nAvoid preamble.\n".to_string())` (block scalar 末尾改行を含む)
- 失敗理由の想定: `ModelConfig` にフィールドが無いためコンパイルエラー。

### GREEN: 最小実装

`src/config/types.rs` の `ModelConfig` に `model_instructions` フィールドのみ追加する(※ `model_instructions_file` は Step 2 の両立バリデーションで必要になるタイミングで追加し、clippy の dead_code 警告を回避する)。serde 属性は既存 `context_window_tokens` と同じ(`#[serde(default, skip_serializing_if = "Option::is_none")]`)。

```rust
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct ModelConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window_tokens: Option<usize>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_instructions: Option<String>,
}
```

### REFACTOR: 設計の整理

- 重複: なし
- 命名: `model_instructions` / `model_instructions_file` は `context_window_tokens` との並びで自然
- 責務: `ModelConfig` はメタデータ保持のみ。読み込みは resolve 側。
- テストの構造的結合: `ModelConfig` の公開フィールド直接比較で OK。内部構造に寄りすぎていない。
- 次の項目へ進める身軽さ: フィールド追加のみで T2 に進める。

### テストリスト更新

- 完了: `T1`
- 追加: なし
- 次候補: `T2`

### コミット

`feat(config): add model_instructions/model_instructions_file fields to ModelConfig`

---

## Step 2: 両立バリデーション Cycle - T2

### この Step の目的

インラインとファイルPATHの両立を起動時に検出し、`ConfigError::ModelInstructionsConflict` で fail-fast する。あわせて `model_instructions_file` フィールドを追加し、Step 1 で保留していた第2フィールドを完成させる。

### 今回選ぶ項目

- 対象: `T2`
- 選ぶ理由: 設定ミスの早期発見が無いと、実行時の挙動が曖昧になる。resolve 実装前に排他制約を確定させる。また `model_instructions_file` フィールドの追加タイミングとして、両立チェックを実装する本 Step が dead_code 警告を出さない最初のタイミングである。
- この時点では扱わないこと: ファイル存在チェック・読み込み(T4/T8)、resolve ヘルパ(T3)、プロンプト注入(T6)。両立チェックのみ。

### RED: 失敗する自動テストを書く

- 追加するテスト名: `model_instructions_conflict_when_both_specified` (配置: `src/config/tests.rs` に既存の loader 系テストパターンに倣う)
- Given: 単一 provider・単一 model の YAML で、`model_instructions` と `model_instructions_file` を両方指定した `egopulse.config.yaml` を tempdir に配置
- When: `Config::load(Some(&path))` を呼ぶ
- Then: `Err(ConfigError::ModelInstructionsConflict)` (variant 照合、provider / model 情報を含む)
- 失敗理由の想定: バリデーション未実装のため `Ok` で通ってしまう。

### GREEN: 最小実装

1. `src/config/types.rs` の `ModelConfig` に `model_instructions_file` フィールドを追加:
   ```rust
   #[serde(default, skip_serializing_if = "Option::is_none")]
   pub model_instructions_file: Option<String>,
   ```
2. `src/error.rs` の `ConfigError` に新バリアント追加:
   ```rust
   #[error("model_instructions_conflict: provider={provider} model={model}: \
           specify either 'model_instructions' or 'model_instructions_file', not both")]
   ModelInstructionsConflict { provider: String, model: String },
   ```
3. `src/config/loader.rs` の `normalize_provider_map` 内、`for (name, file_provider) in providers` ループにて `models` 構築後に両立チェックを追加:
   ```rust
   for (model_name, model_config) in &models {
       if model_config.model_instructions.is_some()
           && model_config.model_instructions_file.is_some()
       {
           return Err(ConfigError::ModelInstructionsConflict {
               provider: key.to_string(),
               model: model_name.clone(),
           });
       }
   }
   ```
   ※ `key` は `ProviderId`、`model_name` は `String`。

### REFACTOR: 設計の整理

- 重複: なし
- 命名: `ModelInstructionsConflict` は `ConfigError` 既存の `SleepBatchEnabledRequiresSchedule` 等と同じ叙述形式
- 責務: バリデーションは loader に置く(既存の `validate_compaction_config` 等と同じ場所)
- テストの構造的結合: tempdir + Config::load の黒盒テストで、内部構造に依存しない
- 次の項目へ進める身軽さ: バリデーションが確定し、resolve 実装に進める

### テストリスト更新

- 完了: `T2`
- 追加: なし
- 次候補: `T3`

### コミット

`feat(config): reject model_instructions and model_instructions_file both set`

---

## Step 3: `resolve_model_instructions` ヘルパ(インライン) Cycle - T3

### この Step の目的

`Config::resolve_model_instructions(provider_id, model, base_dir)` を新設し、インライン指定時に `Some(trimmed_content)` を返す。

### 今回選ぶ項目

- 対象: `T3`
- 選ぶ理由: resolve の最初の足場。インラインケースだけでシグネチャを確定させる。
- この時点では扱わないこと: ファイルPATH解決(T4/T5)、空文字ケース(T5 で追加)、prompt への注入(T6)。

### RED: 失敗する自動テストを書く

- 追加するテスト名: `resolve_model_instructions_returns_inline` (配置: `src/config/resolve.rs` または `src/config/tests.rs` の resolve 系テスト群)
- Given: 単一 provider / model の `Config` を構築し、`model.model_instructions = Some("  Be concise.  ".to_string())`
- When: `config.resolve_model_instructions(&provider_id, model_name, &base_dir)` を呼ぶ
- Then: `Ok(Some("Be concise.".to_string()))` (前後の空白/改行は trim される)
- 失敗理由の想定: メソッド未実装でコンパイルエラー。

### GREEN: 最小実装

`src/config/resolve.rs` の `impl Config` にメソッド追加。インラインケースのみ。

```rust
/// Resolves the `model_instructions` content for a provider+model pair.
///
/// - Returns inline `model_instructions` content when set.
/// - Falls back to `model_instructions_file` resolution (implemented later).
/// - Trims surrounding whitespace; returns `None` for empty/whitespace-only content.
///
/// # Errors
///
/// Returns [`ConfigError::ModelInstructionsFileUnreadable`] when the referenced
/// file cannot be read.
pub(crate) fn resolve_model_instructions(
    &self,
    provider_id: &ProviderId,
    model: &str,
    base_dir: &std::path::Path,
) -> Result<Option<String>, ConfigError> {
    let _ = base_dir; // used in T4
    let Some(provider) = self.providers.get(provider_id) else {
        return Ok(None);
    };
    let Some(model_config) = provider.models.get(model) else {
        return Ok(None);
    };
    if let Some(inline) = &model_config.model_instructions {
        let trimmed = inline.trim();
        return Ok(if trimmed.is_empty() { None } else { Some(trimmed.to_string()) });
    }
    Ok(None) // T4 で file ケースを追加
}
```

※ `error.rs` に `ModelInstructionsFileUnreadable` バリアントをこのタイミングで追加(後の Step で使用)。
```rust
#[error("model_instructions_file_unreadable: provider={provider} model={model} path={path}: {detail}")]
ModelInstructionsFileUnreadable { provider: String, model: String, path: String, detail: String },
```

### REFACTOR: 設計の整理

- 重複: なし
- 命名: `resolve_model_instructions` は `resolve_context_window_tokens` と並ぶ名前
- 責務: lookup + trim。IO は T4 で追加。
- テストの構造的結合: 戻り値の `Option<String>` のみで検証。内部 HashMap の構造に依存しない。
- 次の項目へ進める身軽さ: インラインケースが通り、ファイルケースに進める

### テストリスト更新

- 完了: `T3`
- 追加: なし
- 次候補: `T4`

### コミット

`feat(config): add resolve_model_instructions helper (inline)`

---

## Step 4: `resolve_model_instructions` ヘルパ(ファイル) Cycle - T4, T5

### この Step の目的

`model_instructions_file` を `base_dir` 基点で解決し、ファイル内容を返す。空文字/空白のみは `None`。

### 今回選ぶ項目

- 対象: `T4`(ファイル読み込み) および `T5`(空文字フォールバック)
- 選ぶ理由: T4 と T5 は同じコードパス(`resolve_model_instructions` の file ブランチ)に触るため、同一項目の複数境界として扱う。ただし Red は1回1件。
- この時点では扱わないこと: prompt への注入(T6)、実行時 IO エラーのフォールバック(T8 はプロンプト側で扱う)。

### 4-A. RED: ファイル読み込みテスト(T4)

- 追加するテスト名: `resolve_model_instructions_reads_file_relative_to_base_dir`
- Given:
  - tempdir に `instructions.txt` を作成(内容: `"Be concise.\n"`)
  - 単一 provider / model の `Config` で `model.model_instructions_file = Some("instructions.txt".to_string())`
- When: `config.resolve_model_instructions(&provider_id, model_name, tempdir.path())` を呼ぶ
- Then: `Ok(Some("Be concise.".to_string()))` (trim 済み)
- 失敗理由の想定: ファイルブランチ未実装で `Ok(None)` が返る。

### 4-A. GREEN: ファイル読み込み実装

`resolve_model_instructions` の末尾(`Ok(None)` の直前)にファイルブランチを追加:

```rust
if let Some(rel) = &model_config.model_instructions_file {
    let path = if std::path::Path::new(rel).is_absolute() {
        std::path::PathBuf::from(rel)
    } else {
        base_dir.join(rel)
    };
    let content = std::fs::read_to_string(&path).map_err(|e| {
        ConfigError::ModelInstructionsFileUnreadable {
            provider: provider_id.to_string(),
            model: model.to_string(),
            path: path.to_string_lossy().into_owned(),
            detail: e.to_string(),
        }
    })?;
    let trimmed = content.trim();
    return Ok(if trimmed.is_empty() { None } else { Some(trimmed.to_string()) });
}
```

### 4-B. RED: 空文字フォールバックテスト(T5)

- 追加するテスト名: `resolve_model_instructions_returns_none_for_blank_content`
- Given:
  - tempdir に `blank.txt` を作成(内容: `"   \n\t\n  "`)
  - 単一 provider / model で `model.model_instructions_file = Some("blank.txt".to_string())`
- When: `config.resolve_model_instructions(&provider_id, model_name, tempdir.path())`
- Then: `Ok(None)`
- 失敗理由の想定: GREEN 実装に空文字チェックが含まれているため、本来は成功するはず。もし失敗したら実装漏れ。

### 4-B. GREEN

追加実装不要(trim チェック済み)。失敗した場合は `trim().is_empty()` チェックを見直す。

### REFACTOR: 設計の整理

- 重複: インラインとファイル両方で `trim -> is_empty -> None` が同じ。ヘルパ関数に切り出すか検討。KISS 的にはこのサイズなら重複許容。
- 命名: `base_dir` は `resolve_*` の他の関数には無い引数だが、ファイル参照のために必要。`state.config_path.parent()` が呼び出し元で取られる。
- 責務: `resolve_model_instructions` は lookup + IO + trim の3役。IO を分離するか迷うが、`resolve_context_window_tokens` も lookup のみで十分小さいので、本関数も1メソッドでよい。
- テストの構造的結合: tempdir を使った IO テスト。環境非依存。
- 次の項目へ進める身軽さ: resolve が完成し、prompt 注入に進める。

### テストリスト更新

- 完了: `T4`, `T5`
- 追加: なし
- 次候補: `T6`

### コミット

`feat(config): resolve model_instructions_file relative to config dir`

---

## Step 5: `build_system_prompt` 注入 Cycle - T6

### この Step の目的

`build_system_prompt` が model_instructions を `<model-instructions>` タグでラップし、SOUL の直後・Core Instructions の直前に注入する。

### 今回選ぶ項目

- 対象: `T6`
- 選ぶ理由: ユーザーから要求された最終的な振る舞い。これが通れば機能完成。
- この時点では扱わないこと: 未設定時の回帰(T7)、実行時 IO エラー(T8)。

### RED: 失敗する自動テストを書く

- 追加するテスト名: `system_prompt_injects_model_instructions_between_soul_and_core` (配置: `src/agent_loop/prompt_builder.rs` 内 `#[cfg(test)] mod tests`)
- Given:
  - tempdir に SOUL.md(内容: `"global soul content"`)を配置
  - `test_util::build_state_with_config` 等で `Config` を構築し、`providers.<id>.models.<model>.model_instructions = Some("You prefer terse output.".to_string())` を設定
  - 既存テスト(`system_prompt_order_soul_before_identity` 等)の構築パターンを踏襲
  - `SurfaceContext` は当該 provider / model を resolve できるよう `agent_id`, `channel` を設定
- When: `build_system_prompt(&state, &context)` を呼ぶ
- Then:
  - `<model-instructions>` と `</model-instructions>` が両方出現
  - `You prefer terse output.` を含む
  - 順序: `find("<soul>") < find("<model-instructions>") < find("Built-in execution playbook")` (Core Instructions の安定識別子)
- 失敗理由の想定: `build_model_instructions_section` 未実装で `<model-instructions>` が出現しない。

### GREEN: 最小実装

`src/agent_loop/prompt_builder.rs` に新関数と注入を追加:

```rust
fn build_model_instructions_section(
    state: &AppState,
    context: &SurfaceContext,
) -> Option<String> {
    let config = &state.config;
    let agent_id = crate::config::AgentId::new(&context.agent_id);
    let resolved = match config.resolve_llm_for_agent_channel(&agent_id, &context.channel) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "model_instructions: llm resolution failed");
            return None;
        }
    };
    let provider_id = crate::config::ProviderId::new(&resolved.provider);
    let base_dir = state
        .config_path
        .as_ref()
        .and_then(|p| p.parent())
        .unwrap_or_else(|| std::path::Path::new("."));

    let content = match config.resolve_model_instructions(
        &provider_id,
        &resolved.model,
        base_dir,
    ) {
        Ok(Some(c)) => c,
        Ok(None) => return None,
        Err(e) => {
            tracing::warn!(error = %e, "model_instructions: resolution failed");
            return None;
        }
    };

    Some(format!("<model-instructions>\n{content}\n</model-instructions>"))
}
```

`build_system_prompt` に注入(SOUL と `build_base_prompt` の間):

```rust
pub(crate) fn build_system_prompt(state: &AppState, context: &SurfaceContext) -> String {
    let channel = &context.channel;
    let thread = &context.surface_thread;

    let mut prompt = String::new();
    if let Some(soul_section) = build_soul_prompt_section(state, context) {
        prompt.push_str(&soul_section);
        prompt.push_str("\n\n");
    }

    if let Some(instr) = build_model_instructions_section(state, context) {
        prompt.push_str(&instr);
        prompt.push_str("\n\n");
    }

    prompt.push_str(&build_base_prompt(context));
    // ... 以降は変更なし
}
```

※ `state.config_path` は `runtime::AppState` の `pub(crate) config_path: Option<PathBuf>` を参照(要確認)。もし `pub(crate)` で無い場合は、`AppState` に `config_dir()` アクセサを追加する。

### REFACTOR: 設計の整理

- 重複: `AgentId::new` / `ProviderId::new` の呼び出しは `llm_for_context` と被るが、一時変数のため許容
- 命名: `build_model_instructions_section` は `build_soul_prompt_section` / `build_agents_prompt_section` と並ぶ名前
- 責務: model 解決 → ファイル読み込み → ラップ。3役だが、それぞれ小さい。
- テストの構造的結合: `<model-instructions>` タグと安定識別子(`Built-in execution playbook`)で順序検証。内部関数を叩かない。
- 次の項目へ進める身軽さ: 注入が通り、回帰テストに進める

### テストリスト更新

- 完了: `T6`
- 追加: `T7`, `T8` は次 Step で扱う
- 次候補: `T7`

### コミット

`feat(agent-loop): inject model_instructions between soul and core prompt`

---

## Step 6: 回帰・フォールバック Cycle - T7, T8

### この Step の目的

未設定時の従来挙動維持(T7)と、実行時 IO エラー時の warn ログ + None フォールバック(T8)を保証する。

### 今回選ぶ項目

- 対象: `T7` および `T8`(両方とも `build_model_instructions_section` の境界)
- 選ぶ理由: 既存ユーザーへの回帰リスクを潰す。ファイル参照の異常時もプロンプト構築が継続することを保証。
- この時点では扱わないこと: 新規機能追加は無し。

### 6-A. RED: 未設定回帰テスト(T7)

- 追加するテスト名: `system_prompt_without_model_instructions_is_unchanged` (既存テスト `system_prompt_without_memory_is_unchanged` と対)
- Given: tempdir に SOUL.md のみ配置(model_instructions 未設定)で `AppState` 構築
- When: `build_system_prompt(&state, &ctx)` を呼ぶ
- Then:
  - `<model-instructions>` を含まない
  - `<soul>` を含む(SOUL は効いている)
  - `cli`(channel) / session 名を含む(Core Instructions も効いている)
- 失敗理由の想定: GREEN 実装が None を返すため、本来は成功するはず。万一失敗したら分岐漏れ。

### 6-A. GREEN

追加実装不要。失敗した場合は Step 5 実装の分岐(`Ok(None) => return None`)を見直す。

### 6-B. RED: IO エラーフォールバックテスト(T8)

- 追加するテスト名: `build_model_instructions_section_returns_none_on_io_error`
- Given:
  - `model_instructions_file = Some("missing.txt")` を設定(実在しないファイル)
  - ※ loader でファイル存在チェックを入れるかどうかは別論点。本テストは「実行時に消えた」ケースを模倣するため、意図的に `Config` を直接構築してテストする(-loader を経由しない)
- When: `build_model_instructions_section(&state, &ctx)` を直接呼ぶ(または `build_system_prompt` 経由)
- Then:
  - 戻り値 `None`(または `build_system_prompt` が `<model-instructions>` を含まず、かつ残りのセクションは正常)
- 失敗理由の想定: `resolve_model_instructions` が `Err` を返すが `build_model_instructions_section` が伝播してしまう場合、または unwrap で panic する場合。

### 6-B. GREEN

`build_model_instructions_section` の Step 5 実装に `Err(e) => warn + None` が既に含まれているため、本来は成功するはず。失敗した場合はエラーハンドリングを見直す。

### REFACTOR: 設計の整理

- 重複: T7 と既存 `system_prompt_without_memory_is_unchanged` が似ているが、別セクションの検証なので許容
- 命名: `_returns_none_on_io_error` は挙動を明示
- 責務: `build_model_instructions_section` は「フォールバックしてプロンプト構築を継続」する責務。これをテストで明文化。
- テストの構造的結合: `<model-instructions>` タグの有無で判定。内部詳細に依存しない。
- 次の項目へ進める身軽さ: 回帰とフォールバックが保証され、docs 更新に進める

### テストリスト更新

- 完了: `T7`, `T8`
- 追加: なし
- 次候補: なし(T9 は対象外)

### コミット

`test(agent-loop): cover model_instructions fallback and no-config regression`

---

## Step 7: docs 更新

実装が確定したタイミングで関連 docs を更新する。機能コミットとは別コミット(コミット7)として切る。

- `docs/config.md`:
  - §2.2 `ModelConfig` フィールド表に `model_instructions` / `model_instructions_file` 行を追加
  - §2.11 完全 YAML 例にコメント付きサンプル追記
  - §9.2 ホットリロード可能フィールド一覧に `providers.<id>.models.*.model_instructions*` を追記
- `docs/system-prompt.md`:
  - §1 セクション構成図に ①.5 Model Instructions を挿入
  - §4 に新セクション(4.1.5 相当)を追加。ラップタグ `<model-instructions>`、SOUL/Core 間の注入位置、resolve チェーン、排他制約、PATH 基点を明記
  - §4.5 Pulse Activation に「model_instructions も自動適用」の注記
  - §4.6 Sleep Batch に「model_instructions は適用外」の注記(T9 対象外理由との整合)

---

## Step 8: 動作確認

- Worktree 内で以下を順に実施:
  - `cargo fmt --check`
  - `cargo check`
  - `cargo clippy --all-targets --all-features -- -D warnings`
  - `cargo test`
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
- 手動確認(任意): tempdir にサンプル `egopulse.config.yaml` + `model_instructions_file` を置き、`egopulse chat` 1ターンで system prompt に `<model-instructions>` が含まれることを `tracing::debug!` または一時ログで確認(本番パスには入れない)。
- 失敗時は該当 Step に戻る:
  - compile error → 該当 Step の GREEN 実装見直し
  - clippy violation → 該当 Step の REFACTOR やり直し
  - test failure → 該当 Cycle の RED/GREEN 検証

---

## Step 9: Plan・仕様書との自己チェック

実装完了後にこの Plan と関連仕様書(`docs/system-prompt.md`, `docs/config.md`)を最初から読み直し、実装・自動テスト・文書が要求した振る舞いと一致しているかを照合する。未実装、過剰実装、テスト不足、仕様書との齟齬を見つけた場合は、該当する TDD Cycle へ戻って修正し、動作確認を再実行してからこの Step を完了する。

チェック項目:
- [ ] Plan のテストリスト T1〜T8 のすべての Cycle が完了条件を満たしている
- [ ] T9(Sleep Batch 影響なし)が「対象外」理由付きで明記されていること
- [ ] `docs/system-prompt.md` §1 セクション構成図に ①.5 Model Instructions が追記されている
- [ ] `docs/system-prompt.md` §4 に Model Instructions セクション(4.1.5 相当)が追加されている
- [ ] `docs/system-prompt.md` §4.5 Pulse Activation にも model_instructions が自動適用される旨が明記されている
- [ ] `docs/system-prompt.md` §4.6 Sleep Batch(または同等位置)に model_instructions は適用外である旨が明記されている(T9 対象外理由との整合)
- [ ] `docs/config.md` §2.2 の `ModelConfig` フィールド表に `model_instructions` / `model_instructions_file` が追記されている
- [ ] `docs/config.md` §2.11 完全 YAML 例にサンプルコメント付きで追記されている
- [ ] `docs/config.md` §9.2 ホットリロード可能フィールド一覧に `providers.<id>.models.*.model_instructions*` が追記されている
- [ ] 変更ファイル一覧・コミット分割・自動テスト一覧が実際の変更と一致している
- [ ] model_instructions が `<model-instructions>` タグでラップされていること
- [ ] Core Instructions のセキュリティルールが最終優先される文脈が維持されていること

---

## Step 10: PR 作成

- PR タイトル: `feat: モデル固有プロンプト(model_instructions)の追加`
- PR description(日本語):
  - 概要:
    - `providers.<id>.models.<model>` 配下に `model_instructions`(インライン)と `model_instructions_file`(ファイルPATH)を追加
    - 設定した内容を `<model-instructions>` タグでラップし、system prompt の SOUL と Core Instructions の間に注入
    - 通常ターン・Pulse 活性化の両方に自動適用。Compaction・Sleep Batch は専用プロンプトのため対象外
    - `model_instructions` / `model_instructions_file` の両立は起動エラー(排他)
    - ファイルPATHは設定ファイルのディレクトリ基点で相対解決(絶対パスも許可)
  - 設計判断: Q1〜Q6 の設計案ベース(インライン/PATH排他・`<model-instructions>`ラップ・毎ターン読み込み・Sleepは対象外)
  - テスト: 自動テスト一覧の8件がすべて通過(`cargo test`)
  - Close #<issue-number> (該当 Issue がある場合)

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
| ---- | ---- | -- |
| `src/config/types.rs` | 変更 | `ModelConfig` に `model_instructions` / `model_instructions_file` 追加 + デシリアライズUT |
| `src/config/loader.rs` | 変更 | `normalize_provider_map` に両立チェック追加 + UT |
| `src/error.rs` | 変更 | `ConfigError` に `ModelInstructionsConflict` / `ModelInstructionsFileUnreadable` 追加 |
| `src/config/resolve.rs` | 変更 | `resolve_model_instructions` 新設 + UT(インライン/ファイル/空文字) |
| `src/config/tests.rs` | 変更 | loader・resolve 系テスト追加(必要に応じて既存パターン拡張) |
| `src/agent_loop/prompt_builder.rs` | 変更 | `build_model_instructions_section` 新設・`build_system_prompt` に注入 + UT(注入順序・未設定回帰・IO エラーフォールバック) |
| `docs/config.md` | 変更 | §2.2 フィールド表更新・§2.11 YAML 例追記・§9.2 ホットリロード追記 |
| `docs/system-prompt.md` | 変更 | §1 構成図更新・§4 新セクション追加・§4.5 Pulse 自動適用の注記 |

---

## コミット分割

1. `feat(config): add model_instructions field to ModelConfig` - `src/config/types.rs`
2. `feat(config): add model_instructions_file field with mutual exclusion` - `src/config/types.rs`, `src/error.rs`, `src/config/loader.rs`
3. `feat(config): add resolve_model_instructions helper (inline)` - `src/config/resolve.rs`, `src/error.rs`
4. `feat(config): resolve model_instructions_file relative to config dir` - `src/config/resolve.rs`
5. `feat(agent-loop): inject model_instructions between soul and core prompt` - `src/agent_loop/prompt_builder.rs`
6. `test(agent-loop): cover model_instructions fallback and no-config regression` - `src/agent_loop/prompt_builder.rs`
7. `docs: document model_instructions in config and system-prompt specs` - `docs/config.md`, `docs/system-prompt.md`

※ 各コミットは対応する TDD Cycle の完了タイミングで切る。docs は機能コミットとは別にまとめる。

---

## 自動テスト一覧(全 8 件)

この一覧は Plan 作成時点で必要と判断した最低限の予定であり、最終テスト件数の上限ではない。実装中に追加された不安には、テストリストと TDD Cycle を追加して対応する。

### `src/config`(全 5 件)

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T1 | `model_config_deserializes_inline_instructions` | Step 1 | `cargo test model_config_deserializes_inline_instructions` |
| T2 | `model_instructions_conflict_when_both_specified` | Step 2 | `cargo test model_instructions_conflict_when_both_specified` |
| T3 | `resolve_model_instructions_returns_inline` | Step 3 | `cargo test resolve_model_instructions_returns_inline` |
| T4 | `resolve_model_instructions_reads_file_relative_to_base_dir` | Step 4 | `cargo test resolve_model_instructions_reads_file_relative_to_base_dir` |
| T5 | `resolve_model_instructions_returns_none_for_blank_content` | Step 4 | `cargo test resolve_model_instructions_returns_none_for_blank_content` |

### `src/agent_loop`(全 3 件)

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T6 | `system_prompt_injects_model_instructions_between_soul_and_core` | Step 5 | `cargo test system_prompt_injects_model_instructions_between_soul_and_core` |
| T7 | `system_prompt_without_model_instructions_is_unchanged` | Step 6 | `cargo test system_prompt_without_model_instructions_is_unchanged` |
| T8 | `build_model_instructions_section_returns_none_on_io_error` | Step 6 | `cargo test build_model_instructions_section_returns_none_on_io_error` |

---

## 工数見積もり

実装コード(テスト含む)と docs 更新を行数ベースで見積もる。Step 0/8/9/10 は作業時間のみでコード行数は無い。

| Step | 内容 | 見積もり |
| -- | -- | -- |
| Step 0 | Worktree 作成 | 0 行(コマンドのみ) |
| Step 1 | T1 Cycle: `ModelConfig.model_instructions` フィールド追加 + UT | ~20 行 |
| Step 2 | T2 Cycle: `model_instructions_file` フィールド追加 + 両立バリデーション + `ConfigError` バリアント + UT | ~50 行 |
| Step 3 | T3 Cycle: `resolve_model_instructions`(インライン) + `ConfigError` バリアント + UT | ~50 行 |
| Step 4 | T4+T5 Cycle: `resolve_model_instructions`(ファイル・空文字フォールバック) + UT | ~60 行 |
| Step 5 | T6 Cycle: `build_model_instructions_section` 新設 + `build_system_prompt` 注入 + UT | ~80 行 |
| Step 6 | T7+T8 Cycle: 回帰・IO エラーフォールバック UT | ~50 行 |
| Step 7 | docs 更新(`docs/config.md`, `docs/system-prompt.md`) | ~120 行 |
| Step 8 | 動作確認(fmt/check/clippy/test/doc) | 0 行(検証のみ) |
| Step 9 | Plan・仕様書との自己チェック | 0 行(検証のみ) |
| Step 10 | PR 作成 | 0 行 |
| **合計** | 実装+テストコード / docs | **~310 行 / ~120 行** |
