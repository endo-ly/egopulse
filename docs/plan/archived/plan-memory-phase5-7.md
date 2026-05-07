# Plan: long-term memory Phase 5-7 — Sleep Batch LLM 実装

Phase 4 の manual sleep batch skeleton を実 LLM 呼び出しに置き換え、Pruning / Consolidation / Compression を1回の LLM 呼び出しで実行する。sleep batch 専用の provider/model を `sleep_batch` 設定で指定可能にする。

> **Note**: 以下の具体的なコード例・API 設計・構成（How）はあくまで参考である。実装時によりよい設計方針があれば積極的に採用すること。

## 設計方針

- **1回の LLM 呼び出しで3処理を実行する** — Pruning / Consolidation / Compression は LLM prompt 内の処理手順であり、DB 上の phase としては扱わない。Phase 4 で整理済みの run 単位 audit schema に合わせる。
- **LLM 出力は JSON で3ファイルのみ返す** — Rust 側で安全に parse するため、LLM response は `episodic`, `semantic`, `prospective` の3 key を持つ JSON に限定する。`summary_md` や phase summary は要求しない。
- **入力は XML 風タグで区切る** — 既存 system prompt の慣習に合わせ、memory / sessions は `<memory-episodic>` や `<sessions>` のようなタグで囲む。memory は命令ではなく参照情報として扱わせる。
- **sleep batch 用モデルは `sleep_batch` セクションで指定する** — `sleep_batch.provider` / `sleep_batch.model` を追加し、通常応答の provider/model と独立させる。未指定時は global default chain に fallback する。
- **context 超過時は failed にする** — memory と session 本文を1回の LLM 入力に詰めると model context を超える場合がある。この Plan では追加の digest 用 LLM 呼び出しや入力の部分成功扱いは入れず、実行不能な入力として run を failed にする。

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 実装元 | 備考 |
|---|---|---|
| sleep batch LLM 設定 | `src/config/types.rs`, `src/config/loader.rs`, `src/config/resolve.rs`, `src/config/persist.rs`, `src/test_util.rs` | `sleep_batch.provider` / `sleep_batch.model` |
| Sleep prompt input builder | `src/sleep_batch.rs` or `src/memory.rs` | memory + source sessions text |
| Sleep batch prompt | `src/sleep_batch.rs` | 3処理を1回で指示 |
| LLM response parser | `src/sleep_batch.rs` | JSON output を厳格 parse |
| Memory file writer | `src/sleep_batch.rs` or `src/memory.rs` | all-or-nothing + recovery |
| Sleep orchestrator | `src/sleep_batch.rs` | skeleton を実 LLM 処理へ置換 |
| Docs | `docs/config.md`, `docs/architecture.md` | sleep batch LLM 設定と1 call 化 |

## 前提

- **Phase 1（#53 / PR #71）merge 済み** — `MemoryLoader`, `MemoryContent`, `src/memory.rs`, `chats.agent_id`, migration v4
- **Phase 2 merge 済み** — `SleepRun`, `SleepRunStatus`, `SleepRunTrigger`, `MemorySnapshot`, `MemoryFile`, migration, CRUD クエリ
- **Phase 3 merge 済み** — `InputDecision`, `AgentSessionInfo`, `collect_sleep_input`, `count_agent_messages_since`, `get_agent_sessions_since`, session message 取得
- **Phase 4 merge 済み** — `run_sleep_batch`, `SleepBatchError`, `src/sleep_batch.rs`, `Command::Sleep`, one-call 前提の sleep batch audit schema

> **重要**: Phase 5-7 は Phase 4 で phase / summary 系の監査スキーマが削除済みであることを前提にする。

## Step 0: Worktree 作成

```bash
# Issue #57 ブランチで worktree 作成（#57/#58/#59 をまとめる）
```

## Step 1: 設定 — `sleep_batch.provider/model` 追加 (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `loads_sleep_batch_model` | `sleep_batch.model` がパースされる |
| `loads_sleep_batch_provider` | `sleep_batch.provider` がパースされる |
| `sleep_batch_model_defaults_to_none` | 未指定時は None |
| `sleep_batch_provider_defaults_to_none` | 未指定時は None |
| `resolve_sleep_batch_llm_uses_provider_when_set` | sleep batch provider 指定時はそちらを使う |
| `resolve_sleep_batch_llm_uses_model_when_set` | sleep batch model 指定時はそちらを使う |
| `resolve_sleep_batch_llm_falls_back_to_global_default_model` | 未指定時は global default chain に fallback |
| `rejects_unknown_sleep_batch_provider` | 存在しない provider 参照を拒否する |
| `persist_preserves_sleep_batch_config` | config 保存時に sleep batch 設定が欠落しない |

### GREEN: 実装

`src/config/types.rs`:

- `Config` に `sleep_batch: SleepBatchConfig`
- `SleepBatchConfig`
  - `provider: Option<ProviderId>`
  - `model: Option<String>`

`src/config/loader.rs`:

- YAML の `sleep_batch.provider` / `sleep_batch.model` を parse
- provider 参照の validation を追加

`src/config/resolve.rs`:

- `Config::resolve_sleep_batch_llm(&self) -> Result<ResolvedLlmConfig, ConfigError>`
- fallback:
  - `sleep_batch.provider` があればその provider
  - なければ `default_provider`
  - `sleep_batch.model` があればその model
  - なければ `default_model`
  - それもなければ resolved provider の `default_model`

`src/config/persist.rs`, `src/test_util.rs`:

- 新フィールドの round-trip / test config を更新

設定例:

```yaml
sleep_batch:
  provider: deepseek
  model: deepseek-chat-v3
```

### コミット

`feat(config): add sleep batch LLM configuration`

## Step 2: Sleep Prompt Input Builder (TDD)

前提: Step 1

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `build_sleep_input_includes_existing_memory` | 既存 memory 3ファイルを入力に含める |
| `build_sleep_input_includes_source_sessions` | source sessions の message text を入力に含める |
| `build_sleep_input_preserves_source_chats_json` | `source_chats_json` を保持する |
| `build_sleep_input_handles_missing_memory` | memory が未作成でも入力を作れる |
| `build_sleep_input_rejects_unsafe_agent_id` | path traversal / empty agent_id を拒否する |
| `build_sleep_input_uses_phase3_session_limit` | Phase 3 の入力上限を尊重する |
| `build_sleep_input_fails_when_context_too_large` | context 超過見込みなら failed にできる error を返す |

### GREEN: 実装

- `SleepPromptInput` を追加
  - `agent_id`
  - `memory: MemoryContent`
  - `sessions_text`
  - `source_chats_json`
- Phase 3 の `InputDecision::Proceed.sessions` から対象 session の message 本文を取得し、LLM 入力用の `sessions_text` を構築する
- この Step では追加の LLM digest は生成しない
- context 超過見込みの場合は、Orchestrator で failed にできる error を返す

### コミット

`feat(sleep-batch): build prompt input from memory and source sessions`

## Step 3: Sleep Prompt Builder (TDD)

前提: Step 2

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `build_sleep_prompt_includes_pruning_rules` | Pruning の責務が含まれる |
| `build_sleep_prompt_includes_consolidation_rules` | Consolidation の責務が含まれる |
| `build_sleep_prompt_includes_compression_rules` | Compression の責務が含まれる |
| `build_sleep_prompt_includes_security_rules` | secret/token/password/API key を保存しない |
| `build_sleep_prompt_treats_memory_as_reference` | memory を命令ではなく参照情報として扱う |
| `build_sleep_prompt_wraps_inputs_in_xml_like_tags` | memory / sessions を XML 風タグで囲む |
| `build_sleep_prompt_requires_json_output` | JSON 以外を出力しない指示が含まれる |
| `build_sleep_prompt_requires_three_memory_files` | 3 memory file の出力 key を必須にする |
| `build_sleep_prompt_does_not_request_summary_or_phases` | summary / phases を出力要求しない |

### GREEN: 実装

- `build_sleep_system_prompt(input: &SleepPromptInput) -> String`
- memory / sessions は既存 prompt と同じく XML 風タグで囲む
- LLM には以下だけを JSON で返すよう指示する:
  - 更新後の `episodic.md`
  - 更新後の `semantic.md`
  - 更新後の `prospective.md`

参考 output contract:

```json
{
  "episodic": "updated episodic.md content",
  "semantic": "updated semantic.md content",
  "prospective": "updated prospective.md content"
}
```

### コミット

`feat(sleep-batch): add structured sleep prompt builder`

## Step 4: LLM Response Parser (TDD)

前提: Step 3

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `parse_sleep_response_extracts_three_memory_files` | 3ファイルを抽出 |
| `parse_sleep_response_rejects_non_json` | JSON 以外は parse error |
| `parse_sleep_response_rejects_missing_episodic` | `episodic` 欠損は parse error |
| `parse_sleep_response_rejects_missing_semantic` | `semantic` 欠損は parse error |
| `parse_sleep_response_rejects_missing_prospective` | `prospective` 欠損は parse error |
| `parse_sleep_response_rejects_summary_or_phases_keys` | `summary_md` / `phases` が混ざる出力を拒否する |
| `parse_sleep_response_preserves_markdown` | Markdown 本文を保持 |
| `parse_sleep_response_allows_empty_file_content` | key があれば空文字は許容 |

### GREEN: 実装

- `SleepBatchOutput`
  - `episodic: String`
  - `semantic: String`
  - `prospective: String`
- `parse_sleep_response(response: &str) -> Result<SleepBatchOutput, SleepBatchError>`
- 欠損・壊れた output・余分な summary/phase output は `SleepBatchError::ParseFailed` とする

### コミット

`feat(sleep-batch): parse sleep LLM memory output`

## Step 5: Memory File Writer + Recovery (TDD)

前提: Step 4

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `write_memory_files_writes_all_three_files` | 3ファイルを書き込む |
| `write_memory_files_creates_memory_directory` | memory dir がない場合に作成 |
| `write_memory_files_rejects_unsafe_agent_id` | path traversal を拒否 |
| `write_memory_files_preserves_existing_on_write_error` | write error 時に既存を保持 |
| `write_memory_files_recovers_backup_on_start` | backup が残っている場合に復旧する |
| `write_memory_files_cleans_tmp_dirs` | stale tmp dir を掃除する |
| `write_memory_files_documents_rename_limit` | rename 2回方式の限界をコメントで明示 |

### GREEN: 実装

- writer 実行前に `recover_memory_write(agents_dir, agent_id)` を呼ぶ
- `memory.tmp-{uuid}` に3ファイルを書き込む
- 既存 `memory` を backup に移動し、tmp を `memory` に移動する
- 成功後 backup を削除する
- エラー時は可能な限り backup を `memory` に戻す

> 厳密な常時 atomic 観測は保証しない。sleep batch の排他制御下で、失敗時の復旧可能性と all-or-nothing 結果を担保する。

### コミット

`feat(sleep-batch): add recoverable all-or-nothing memory writer`

## Step 6: Orchestrator 改修 (TDD)

前提: Step 1-5, Phase 4 merge 済み

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `run_sleep_batch_calls_llm_once` | LLM 呼び出しが1回だけ |
| `run_sleep_batch_uses_configured_sleep_batch_llm` | `sleep_batch.provider/model` を使用 |
| `run_sleep_batch_uses_provider_cache_or_override` | AppState の cache/override 方針と整合 |
| `run_sleep_batch_records_aggregate_snapshots` | run 単位の before/after snapshot を保存 |
| `run_sleep_batch_records_token_usage` | token usage を保存 |
| `run_sleep_batch_writes_updated_memory_files` | parse 結果を memory に反映 |
| `run_sleep_batch_marks_failed_on_llm_error` | LLM error で failed |
| `run_sleep_batch_marks_failed_on_parse_error` | parse error で failed、file は保持 |
| `run_sleep_batch_marks_failed_on_write_error` | write error で failed、file は保持 |
| `run_sleep_batch_marks_failed_on_context_overflow` | context 超過見込みで failed |
| `run_sleep_batch_preserves_phase4_behaviors` | Skip / AlreadyRunning / default agent が維持される |

### GREEN: 実装

`run_sleep_batch` の流れ:

1. agent_id 解決
2. `collect_sleep_input`
   - `Skip` は Phase 4 と同じ扱い
3. `create_sleep_run`
4. `resolve_sleep_batch_llm` + provider 取得
5. memory / sessions から `SleepPromptInput` 構築
6. snapshot before 用の memory を保持
7. LLM を1回呼び出す
8. response を parse
9. memory files を recoverable writer で書き戻す
10. 書き戻し後の memory を読み直す
11. run 単位の aggregate snapshot を保存
12. `update_sleep_run_success` で token usage と source chats を保存
13. 4-10 の失敗は `update_sleep_run_failed`

`PhaseResult` / `LogicalPhaseResult` / phase summary は追加しない。

### コミット

`feat(sleep-batch): replace skeleton with one-call LLM processing`

## Step 7: ドキュメント更新 (TDD)

前提: Step 1-6

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `docs_config_mentions_sleep_batch_model` | `docs/config.md` に `sleep_batch.model` がある |
| `docs_config_mentions_sleep_batch_provider` | `docs/config.md` に `sleep_batch.provider` がある |
| `docs_architecture_mentions_one_call_sleep_batch` | `docs/architecture.md` に1回呼び出し方針がある |

### GREEN: 実装

| ファイル | 更新内容 |
|---|---|
| `docs/config.md` | `sleep_batch.provider/model` と fallback |
| `docs/architecture.md` | 1 call sleep batch の概要 |

### コミット

`docs: update sleep batch LLM configuration and architecture`

## Step 8: 動作確認

```bash
cargo fmt --check
cargo test -p egopulse
cargo check -p egopulse
cargo clippy --all-targets --all-features -- -D warnings
```

Phase 4 の既存テスト（Skip / Proceed / AlreadyRunning / default agent / CLI parser）も全て通ることを確認する。

## Step 9: PR 作成

- ブランチ: `feat/memory-phase5-7-sleep-batch-llm`
- PR description: 日本語
- `Close #57`, `Close #58`, `Close #59` 明記
- 追加要件として「LLM 呼び出し1回」「sleep batch model/provider 設定可能」を明記

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
|---|---|---|
| `src/config/types.rs` | 変更 | SleepBatchConfig |
| `src/config/loader.rs` | 変更 | `sleep_batch.*` parse/validation |
| `src/config/resolve.rs` | 変更 | `resolve_sleep_batch_llm` |
| `src/config/persist.rs` | 変更 | sleep batch config round-trip |
| `src/test_util.rs` | 変更 | test config 更新 |
| `src/sleep_batch.rs` | 変更 | input builder / prompt / parser / writer / orchestrator |
| `docs/config.md` | 変更 | sleep batch LLM 設定 |
| `docs/architecture.md` | 変更 | sleep batch LLM 概要 |

## コミット分割

1. `feat(config): add sleep batch LLM configuration` — `src/config/*`, `src/test_util.rs`
2. `feat(sleep-batch): build prompt input from memory and source sessions` — `src/sleep_batch.rs` or `src/memory.rs`
3. `feat(sleep-batch): add structured sleep prompt builder` — `src/sleep_batch.rs`
4. `feat(sleep-batch): parse sleep LLM memory output` — `src/sleep_batch.rs`
5. `feat(sleep-batch): add recoverable all-or-nothing memory writer` — `src/sleep_batch.rs` or `src/memory.rs`
6. `feat(sleep-batch): replace skeleton with one-call LLM processing` — `src/sleep_batch.rs`
7. `docs: update sleep batch LLM configuration and architecture` — `docs/`

## テストケース一覧（全 54 件）

### 設定 (9)

1. `loads_sleep_batch_model` — `sleep_batch.model` パース
2. `loads_sleep_batch_provider` — `sleep_batch.provider` パース
3. `sleep_batch_model_defaults_to_none` — model 未指定
4. `sleep_batch_provider_defaults_to_none` — provider 未指定
5. `resolve_sleep_batch_llm_uses_provider_when_set` — provider 指定
6. `resolve_sleep_batch_llm_uses_model_when_set` — model 指定
7. `resolve_sleep_batch_llm_falls_back_to_global_default_model` — default fallback
8. `rejects_unknown_sleep_batch_provider` — 不正 provider 拒否
9. `persist_preserves_sleep_batch_config` — persist round-trip

### Prompt Input (7)

10. `build_sleep_input_includes_existing_memory` — memory 含有
11. `build_sleep_input_includes_source_sessions` — session 本文含有
12. `build_sleep_input_preserves_source_chats_json` — source metadata 保持
13. `build_sleep_input_handles_missing_memory` — memory 欠損対応
14. `build_sleep_input_rejects_unsafe_agent_id` — unsafe agent 拒否
15. `build_sleep_input_uses_phase3_session_limit` — session limit 尊重
16. `build_sleep_input_fails_when_context_too_large` — context 超過検出

### Prompt Builder (9)

17. `build_sleep_prompt_includes_pruning_rules` — pruning rules
18. `build_sleep_prompt_includes_consolidation_rules` — consolidation rules
19. `build_sleep_prompt_includes_compression_rules` — compression rules
20. `build_sleep_prompt_includes_security_rules` — secret 保存禁止
21. `build_sleep_prompt_treats_memory_as_reference` — memory は参照情報
22. `build_sleep_prompt_wraps_inputs_in_xml_like_tags` — 入力を XML 風タグで囲む
23. `build_sleep_prompt_requires_json_output` — JSON output 指示
24. `build_sleep_prompt_requires_three_memory_files` — 3ファイル必須
25. `build_sleep_prompt_does_not_request_summary_or_phases` — summary/phases 不要求

### Parser (8)

26. `parse_sleep_response_extracts_three_memory_files` — 3ファイル抽出
27. `parse_sleep_response_rejects_non_json` — 非 JSON 拒否
28. `parse_sleep_response_rejects_missing_episodic` — episodic 欠損拒否
29. `parse_sleep_response_rejects_missing_semantic` — semantic 欠損拒否
30. `parse_sleep_response_rejects_missing_prospective` — prospective 欠損拒否
31. `parse_sleep_response_rejects_summary_or_phases_keys` — summary/phases 拒否
32. `parse_sleep_response_preserves_markdown` — Markdown 保持
33. `parse_sleep_response_allows_empty_file_content` — 空本文許容

### Writer (7)

34. `write_memory_files_writes_all_three_files` — 3ファイル書き込み
35. `write_memory_files_creates_memory_directory` — directory 作成
36. `write_memory_files_rejects_unsafe_agent_id` — unsafe agent 拒否
37. `write_memory_files_preserves_existing_on_write_error` — error 時保持
38. `write_memory_files_recovers_backup_on_start` — backup 復旧
39. `write_memory_files_cleans_tmp_dirs` — tmp 掃除
40. `write_memory_files_documents_rename_limit` — rename 限界を明示

### Orchestrator (11)

41. `run_sleep_batch_calls_llm_once` — LLM 1回
42. `run_sleep_batch_uses_configured_sleep_batch_llm` — sleep batch LLM 設定
43. `run_sleep_batch_uses_provider_cache_or_override` — cache/override 整合
44. `run_sleep_batch_records_aggregate_snapshots` — aggregate snapshot
45. `run_sleep_batch_records_token_usage` — token usage
46. `run_sleep_batch_writes_updated_memory_files` — memory 反映
47. `run_sleep_batch_marks_failed_on_llm_error` — LLM error
48. `run_sleep_batch_marks_failed_on_parse_error` — parse error
49. `run_sleep_batch_marks_failed_on_write_error` — write error
50. `run_sleep_batch_marks_failed_on_context_overflow` — context 超過
51. `run_sleep_batch_preserves_phase4_behaviors` — Phase 4 挙動維持

### Docs (3)

52. `docs_config_mentions_sleep_batch_model` — config model
53. `docs_config_mentions_sleep_batch_provider` — config provider
54. `docs_architecture_mentions_one_call_sleep_batch` — architecture one call

## 工数見積もり

| Step | 内容 | 見積もり |
|---|---|---|
| Step 1 | 設定（sleep_batch provider/model） | ~180 行 |
| Step 2 | Prompt input builder | ~180 行 |
| Step 3 | Prompt builder | ~160 行 |
| Step 4 | Response parser | ~160 行 |
| Step 5 | Memory writer + recovery | ~240 行 |
| Step 6 | Orchestrator 改修 | ~300 行 |
| Step 7 | Docs 更新 | ~80 行 |
| **合計** | | **~1,300 行** |
