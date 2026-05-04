# Plan: Safety Compaction

Issue #33 の Safety Compaction 仕様を、既存の message-count based compaction から token-aware な安全装置へ置き換える。context window 上限に近づく前に Middle だけを reference-only summary へ畳み、最新依頼・直近文脈・tool call/result の整合性を保つ。

> **Note**: 以下の具体的なコード例・API 設計・構成（How）はあくまで参考である。実装時によりよい設計方針があれば積極的に採用すること。

## 設計方針

- 既存 `src/agent_loop/compaction.rs` を拡張・整理し、Safety Compaction を唯一の自動 compaction とする。旧 `max_session_messages` 判定は削除し、token estimate を主判定にする。
- context window は provider/model ごとの設定を正とし、未設定時は top-level `default_context_window_tokens` を使う。fallback 値は安全側に倒すため上限を持たせ、provider API からの自動取得や preset hardcode は行わない。
- 追加設定は最小限にする。`enabled` や `model_contexts`、`compaction:` ネストは追加せず、ratio は top-level に置く。
- compaction summary は active instruction ではなく reference-only message として扱う。現行の `[Conversation Summary]` 形式は廃止する。
- secret は summary 生成前後で redaction する。既存 `src/tools/sanitizer.rs` の二層 redaction を再利用し、summary と compaction log に credential を残さない。archive は現行どおり local forensic copy として verbatim 保存し、secret redaction の保証対象外であることを docs に明記する。
- tool call と tool result は不可分ブロックとして保護する。Tail と未完了作業を生で残し、Middle のみを圧縮する。

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 実装元 |
|---|---|
| Config schema | `src/config/mod.rs`, `src/config/loader.rs`, `src/config/persist.rs`, `src/config/resolve.rs`, `egopulse.config.example.yaml` |
| Provider/model 解決 | `src/config/resolve.rs`, `src/runtime.rs` |
| Token estimate / threshold 判定 | `src/agent_loop/compaction.rs`, `src/llm/messages.rs` |
| Head/Middle/Tail 分割 | `src/agent_loop/compaction.rs`, `src/agent_loop/formatting.rs` |
| turn loop 統合 | `src/agent_loop/turn.rs`, `src/slash_commands.rs` |
| Secret redaction | `src/tools/sanitizer.rs`, `src/agent_loop/compaction.rs` |
| Web / Setup config UI | `src/web/config.rs`, `src/setup/provider.rs`, `src/setup/summary.rs` |
| Docs | `docs/config.md`, `docs/session-lifecycle.md`, `docs/system-prompt.md`, `docs/commands.md`, `docs/architecture.md` |

---

## Step 0: Worktree 作成

```bash
git worktree add ../egopulse-safety-compaction -b feat/safety-compaction
cd ../egopulse-safety-compaction
```

---

## Step 1: Config Schema (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `loads_provider_models_with_context_windows` | `providers.<id>.models` を map として読み込み、各 model の `context_window_tokens` を保持できる |
| `uses_default_context_window_when_model_context_missing` | model 固有 `context_window_tokens` がない場合に `default_context_window_tokens` を使う |
| `loads_compaction_ratios_from_top_level` | `compaction_threshold_ratio` と `compaction_target_ratio` を top-level から読み込める |
| `defaults_compaction_ratios_to_issue_values` | ratio 未指定時に threshold `0.80`、target `0.40` を使う |
| `rejects_invalid_compaction_ratios` | `0.0`、`1.0` 超、`target >= threshold` を config error にする |
| `rejects_zero_context_window_tokens` | `default_context_window_tokens` と model context window の `0` を拒否する |
| `rejects_unsafe_default_context_window_tokens` | fallback 用 `default_context_window_tokens` が安全上限を超える場合は config error にする |
| `persists_provider_model_contexts_without_secret_leak` | 設定保存時に model context を保持しつつ secret 実値を YAML に出さない |

### GREEN: 実装

`ProviderConfig.models` を `Vec<String>` から model metadata map へ置き換える。各 model は最低限 `context_window_tokens: Option<usize>` を持つ。
top-level に `default_context_window_tokens: usize`, `compaction_threshold_ratio: f64`, `compaction_target_ratio: f64` を追加する。
`default_context_window_tokens` は未登録 model の fallback なので、設定可能だが安全上限を超える値は拒否する。明示 model context だけが大きな context window を使える。
既存 `max_session_messages` は削除する。`compaction_timeout_secs`, `max_history_messages` は現行用途が残るため維持し、`compact_keep_recent` は Tail 保護の既存設定としてこの Step では維持する。

参考 YAML:

```yaml
default_context_window_tokens: 32768
compaction_threshold_ratio: 0.80
compaction_target_ratio: 0.40

providers:
  openrouter:
    models:
      openai/gpt-5:
        context_window_tokens: 200000
```

### コミット

`feat: add safety compaction configuration`

---

## Step 2: Token Estimate and Threshold (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `estimates_prompt_tokens_from_system_messages_and_tools` | system prompt、messages、tool schema を含めて prompt token を保守的に見積もる |
| `computes_usable_context_from_context_window_and_reserves` | context window から output/tool schema/system/margin の内部予約を差し引く |
| `triggers_when_estimate_reaches_threshold` | 見積もり tokens が usable context の threshold 以上で発火する |
| `does_not_trigger_below_threshold` | threshold 未満では compaction しない |
| `targets_configured_compaction_ratio` | compaction 後の目標 token 量が `compaction_target_ratio` で計算される |
| `caps_summary_input_to_summarizer_budget` | summary LLM に渡す入力が summarizer 用 budget を超えない |
| `shrinks_summary_input_until_under_budget` | 軽量化後も大きい Middle を段階的に削り、必ず budget 以下にする |

### GREEN: 実装

v0 は tokenizer を導入せず、chars ベースの保守的近似で token estimate を実装する。過小評価を避ける係数にし、tool schema と system prompt も対象に含める。
reserve / margin は設定に増やさず内部定数または実測可能な文字列長から算出する。`LlmUsage` は事後ログのままとし、発火判定には使わない。
summary 生成そのものが context overflow しないよう、summarizer request 用 budget を計算し、Middle の summary 入力は必ずその budget 以下に収める。手順は「古い tool result の軽量化 → message 単位の要点化 → Head 寄り Middle の追加削減」の順に行い、固定 20,000 chars truncate には依存しない。

### コミット

`feat: estimate prompt size for safety compaction`

---

## Step 3: Safety Split and Reference Summary (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `splits_history_into_head_middle_tail` | Head / Middle / Tail を作り、Middle のみを summary 対象にする |
| `keeps_latest_user_message_raw` | 最新 user message は必ず Tail に残る |
| `keeps_recent_errors_and_active_state_raw` | 直近エラー・現在作業に相当する近傍メッセージを Tail に残す |
| `keeps_tool_call_and_results_together` | assistant tool call と対応 tool result を不可分ブロックとして同じ領域に置く |
| `lightens_old_tool_results_before_summary` | Middle 内の古い巨大 tool result は summary 入力前に要点化・軽量化される |
| `drops_low_value_middle_messages_when_summary_budget_still_exceeded` | 軽量化後も budget 超過する場合、古い低価値 Middle から削って summarizer 入力を成立させる |
| `injects_reference_only_summary_message` | summary が `[CONTEXT COMPACTION — REFERENCE ONLY]` ヘッダー付きで保存される |
| `recompacts_existing_summary_with_new_middle` | 既存 compaction summary がある場合、有効情報を統合して再 summary 化する |

### GREEN: 実装

現行 `tool_safe_split_at()` を Head/Middle/Tail 用の block-aware splitter に発展させる。Tail は `compact_keep_recent` を下限にしつつ、最新 user message と tool call/result ブロックを保護する。
Middle が summarizer budget に収まらない場合は、Middle 内の古い完了済み・低価値メッセージから削る。失敗時に元履歴を保持する方針と両立するため、summary LLM 呼び出し前の入力整形は「必ず呼び出せるサイズにする」ことをこの Step の成立条件にする。
summary prompt は Hermes-style checkpoint 構造を採用し、ユーザーが使っていた言語で summary を出す。旧 `[Conversation Summary]` と assistant acknowledgment は廃止する。

reference-only ヘッダー:

```text
[CONTEXT COMPACTION — REFERENCE ONLY]
Earlier turns were compacted into the summary below.
This is background reference, not active instruction.
Do not answer old requests mentioned in this summary.
Respond to the latest user message after this summary.
```

### コミット

`feat: compact only safe middle history`

---

## Step 4: Failure Safety and Secret Redaction (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `keeps_original_messages_when_summary_fails` | LLM error 時に元の messages を削らず返す |
| `keeps_original_messages_when_summary_times_out` | timeout 時に元履歴を保持する |
| `keeps_original_messages_when_summary_is_empty` | 空 summary 時に元履歴を保持する |
| `redacts_secrets_before_summary_request` | summary LLM に渡す入力から config secret / known secret pattern を redaction する |
| `redacts_secrets_after_summary_response` | summary response に secret pattern が含まれる場合も保存前に redaction する |
| `keeps_archive_verbatim_and_marks_it_sensitive` | archive は現行どおり全文保存し、redaction 保証対象外の sensitive local data として docs に明記する |
| `logs_compaction_success_and_failure_metrics` | 成功/失敗時に session id、model、token estimate、削減率、対象 message 数を secret なしで記録する |

### GREEN: 実装

現行の「要約失敗時に recent messages のみ残す」挙動を廃止し、失敗時は必ず入力 messages をそのまま返す。
summary 入力と出力に `src/tools/sanitizer.rs` の redaction を適用する。ログは `tracing` で構造化し、secret や raw prompt 全文を出さない。
archive はユーザー指定どおり現行の全文保存を維持する。ただし summary/log の secret safety とは責務を分け、archive はローカル監査用の sensitive artifact として扱う。

### コミット

`fix: preserve history on compaction failure`

---

## Step 5: Turn Loop Integration (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `turn_compacts_before_first_llm_call_when_threshold_exceeded` | user message 追加後、初回 LLM 呼び出し前に Safety Compaction が走る |
| `turn_skips_compaction_when_below_threshold` | threshold 未満では既存 messages のまま LLM に渡る |
| `turn_compacts_after_tool_results_before_next_llm_call` | tool result 追加後、次の LLM 呼び出し前に Safety Compaction が再評価される |
| `turn_does_not_split_fresh_tool_results` | 直近 tool call/result ブロックは compaction 後も raw のまま残る |
| `manual_compact_bypasses_threshold_and_uses_safety_shape` | `/compact` は閾値を bypass し、同じ Safety Compaction shape を使う |

### GREEN: 実装

`process_turn_inner()` の初回 LLM 呼び出し前と、`execute_and_persist_tools()` 後の loop 継続前に Safety Compaction 判定を入れる。
`/compact` は旧 compaction ではなく手動 Safety Compaction として同じ処理を使い、閾値だけ bypass する。summary 生成 LLM は現行どおり `state.llm_for_context(context)` を使う。

### コミット

`feat: run safety compaction before llm calls`

---

## Step 6: Docs and Config Examples (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `docs_config_documents_safety_compaction_fields` | `docs/config.md` に追加設定と defaults が記載されていることをレビューで確認 |
| `docs_session_lifecycle_describes_token_based_compaction` | `docs/session-lifecycle.md` が token-aware Safety Compaction と失敗時保持を説明していることをレビューで確認 |
| `docs_system_prompt_documents_reference_only_summary` | `docs/system-prompt.md` が Hermes-style summary prompt と reference-only 注入を説明していることをレビューで確認 |
| `docs_commands_documents_manual_compact` | `docs/commands.md` が `/compact` を手動 Safety Compaction として説明していることをレビューで確認 |
| `example_config_uses_model_metadata_map` | `egopulse.config.example.yaml` が `providers.<id>.models.<model>.context_window_tokens` 形式になっている |

### GREEN: 実装

関連 docs と example config を更新する。archive は現行どおり全文保存を維持し、secret を含む可能性がある運用リスクと、summary/log redaction の保証対象外であることを Plan/Docs に明記する。

### コミット

`docs: describe safety compaction`

---

## Step 7: 動作確認

- `cargo fmt --check`
- `cargo test -p egopulse config::`
- `cargo test -p egopulse agent_loop::compaction`
- `cargo test -p egopulse agent_loop::turn`
- `cargo test -p egopulse slash_commands::`
- `cargo check -p egopulse`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test -p egopulse`
- Manual QA: test config で短い `default_context_window_tokens` を設定し、`cargo run -p egopulse -- chat --session safety-compaction-qa` から長文 + tool 使用 + `/compact` を実行して、reference-only summary と履歴継続を確認する。

---

## Step 8: PR 作成

- Branch: `feat/safety-compaction`
- PR title: `feat: add token-aware safety compaction`
- PR description は日本語で作成し、`Close #33` を明記する。
- PR 作成後、Coderabbit review を待ち、指摘対応は `pr-review-back-workflow` skill で行う。

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
|---|---|---|
| `src/config/mod.rs` | 変更 | provider model metadata、context window、ratio 設定を追加し、`max_session_messages` を削除 |
| `src/config/loader.rs` | 変更 | 新 YAML schema の読み込み・validation |
| `src/config/persist.rs` | 変更 | 新 schema の保存と secret 非漏洩確認 |
| `src/config/resolve.rs` | 変更 | provider/model から context window と ratio を解決 |
| `src/runtime.rs` | 変更 | resolved LLM config / provider cache に必要な context 情報を渡す |
| `src/web/config.rs` | 変更 | Web 設定 API の provider models payload/update を map schema に対応 |
| `src/setup/provider.rs` | 変更 | provider preset の models 生成を map schema に対応 |
| `src/setup/summary.rs` | 変更 | setup 生成 config に default context window と model context を出力 |
| `src/llm/messages.rs` | 変更 | token estimate 用に request body / tool schema 見積もりを再利用可能化 |
| `src/agent_loop/compaction.rs` | 変更 | Safety Compaction 判定、分割、summary、失敗時保持、redaction、logging |
| `src/agent_loop/formatting.rs` | 変更 | tool result 軽量化・summary 入力整形の責務整理 |
| `src/agent_loop/turn.rs` | 変更 | 初回 LLM 前と tool result 後の compaction 統合 |
| `src/slash_commands.rs` | 変更 | `/compact` を手動 Safety Compaction 化 |
| `src/test_util.rs` | 変更 | test config の新 schema 対応 |
| `src/web/auth.rs` | 変更 | test config fixture の新 schema 対応 |
| `src/tools/sanitizer.rs` | 変更 | 必要に応じて compaction 入出力向け redaction helper を公開範囲内で整理 |
| `egopulse.config.example.yaml` | 変更 | `models` map と context window、top-level ratio 設定例へ更新 |
| `docs/config.md` | 変更 | 新設定仕様と削除設定を反映 |
| `docs/session-lifecycle.md` | 変更 | Safety Compaction lifecycle へ更新 |
| `docs/system-prompt.md` | 変更 | reference-only summary prompt を記載 |
| `docs/commands.md` | 変更 | `/compact` の新挙動を記載 |
| `docs/architecture.md` | 変更 | request flow の compaction 判定を token-aware に更新 |

---

## コミット分割

1. `feat: add safety compaction configuration` — config schema / loader / persist / resolve / example config
2. `feat: estimate prompt size for safety compaction` — token estimate / threshold 判定
3. `feat: compact only safe middle history` — Head/Middle/Tail 分割 / reference-only summary
4. `fix: preserve history on compaction failure` — 失敗時保持 / redaction / logging
5. `feat: run safety compaction before llm calls` — turn loop / tool result 後 / `/compact`
6. `docs: describe safety compaction` — docs 更新

---

## テストケース一覧（全 40 件）

### Config Schema (8)
1. `loads_provider_models_with_context_windows` — provider 配下の model metadata を読み込む
2. `uses_default_context_window_when_model_context_missing` — model context 未設定時の fallback
3. `loads_compaction_ratios_from_top_level` — top-level ratio を読み込む
4. `defaults_compaction_ratios_to_issue_values` — ratio default を検証
5. `rejects_invalid_compaction_ratios` — ratio validation
6. `rejects_zero_context_window_tokens` — context window validation
7. `rejects_unsafe_default_context_window_tokens` — fallback context window の安全上限
8. `persists_provider_model_contexts_without_secret_leak` — persist と secret 非漏洩

### Token Estimate and Threshold (7)
9. `estimates_prompt_tokens_from_system_messages_and_tools` — prompt 全体を見積もる
10. `computes_usable_context_from_context_window_and_reserves` — usable context 算出
11. `triggers_when_estimate_reaches_threshold` — threshold 発火
12. `does_not_trigger_below_threshold` — threshold 未満 skip
13. `targets_configured_compaction_ratio` — target ratio 算出
14. `caps_summary_input_to_summarizer_budget` — summarizer 入力上限
15. `shrinks_summary_input_until_under_budget` — 段階的削減

### Safety Split and Reference Summary (8)
16. `splits_history_into_head_middle_tail` — 3 領域分割
17. `keeps_latest_user_message_raw` — 最新 user message 保護
18. `keeps_recent_errors_and_active_state_raw` — 直近エラー/作業保護
19. `keeps_tool_call_and_results_together` — tool block 保護
20. `lightens_old_tool_results_before_summary` — 古い tool result 軽量化
21. `drops_low_value_middle_messages_when_summary_budget_still_exceeded` — budget 超過時の Middle 削減
22. `injects_reference_only_summary_message` — reference-only 注入
23. `recompacts_existing_summary_with_new_middle` — 再圧縮

### Failure Safety and Secret Redaction (7)
24. `keeps_original_messages_when_summary_fails` — LLM error 時に元履歴保持
25. `keeps_original_messages_when_summary_times_out` — timeout 時に元履歴保持
26. `keeps_original_messages_when_summary_is_empty` — 空 summary 時に元履歴保持
27. `redacts_secrets_before_summary_request` — summary 入力 redaction
28. `redacts_secrets_after_summary_response` — summary 出力 redaction
29. `keeps_archive_verbatim_and_marks_it_sensitive` — archive の責務明確化
30. `logs_compaction_success_and_failure_metrics` — metrics logging

### Turn Loop Integration (5)
31. `turn_compacts_before_first_llm_call_when_threshold_exceeded` — 初回 LLM 前 compaction
32. `turn_skips_compaction_when_below_threshold` — below threshold skip
33. `turn_compacts_after_tool_results_before_next_llm_call` — tool result 後 compaction
34. `turn_does_not_split_fresh_tool_results` — fresh tool result 保護
35. `manual_compact_bypasses_threshold_and_uses_safety_shape` — `/compact` 手動実行

### Docs and Config Examples (5)
36. `docs_config_documents_safety_compaction_fields` — config docs 確認
37. `docs_session_lifecycle_describes_token_based_compaction` — lifecycle docs 確認
38. `docs_system_prompt_documents_reference_only_summary` — prompt docs 確認
39. `docs_commands_documents_manual_compact` — commands docs 確認
40. `example_config_uses_model_metadata_map` — example config 確認

---

## 工数見積もり

| Step | 内容 | 見積もり |
|---|---|---:|
| Step 0 | Worktree 作成 | ~0 行 |
| Step 1 | Config schema / validation / fixtures | ~430 行 |
| Step 2 | Token estimate / threshold 判定 | ~300 行 |
| Step 3 | Head/Middle/Tail / reference summary | ~420 行 |
| Step 4 | 失敗時保持 / redaction / logging | ~250 行 |
| Step 5 | turn loop / `/compact` 統合 | ~260 行 |
| Step 6 | docs / example config | ~320 行 |
| Step 7 | 動作確認調整 | ~40 行 |
| Step 8 | PR 作成 | ~0 行 |
| **合計** |  | **~1,990 行** |
