# Plan: Pulse Phase 1 Temporal Activation

Pulse Phase 1 として、`PULSE.md` の Temporal Intention が due になったとき、非 LLM Gate で有効性を判定し、有効なら agent を Pulse Activation として起こす。
Pulse は注意機構であり、Gate 通過後の agent は通常 turn と同等に tools を使える。ただし起点は Pulse Capsule であり、通常 session には capsule 全文を混ぜない。

> **Note**: 以下の具体的なコード例・API 設計・構成（How）はあくまで参考である。実装時によりよい設計方針があれば積極的に採用すること。

## 設計方針

- Pulse の核は Attention Activation Layer として維持する。Phase 1 では Temporal Intention のみを signal source とし、Gate は due / duplicate / active turn の非 LLM 判定だけにする。
- lightweight LLM / tiny judge / salience scoring は将来の Gate 強化点として残すが、Phase 1 では実装しない。
- Gate 通過後は agent を Pulse Activation として起こす。Activation は通常 turn と同等に tools を使えるが、通常 session 全文ではなく Pulse Capsule から始める。
- 既存 `process_turn()` をそのまま Pulse に流用しない。LLM/tool loop と通常 session 永続化を分離できる低レベル実行入口を作り、LLM には Pulse Capsule を渡し、通常 session には合意済みの synthetic input / visible output だけを残す。
- Home Surface は agent の chat 履歴を `last_message_time DESC` で見て、最初に見つかった送信可能 channel を採用する。Phase 1 の送信可能 channel は Discord / Telegram。Web / CLI / TUI は飛ばし、送信可能 chat が無ければ skipped。
- 通知本文ありの場合は、元の `docs/pulse.md` の「通知本文だけ保存」から仕様変更し、通常会話と同等に永続化する。ただし Pulse Capsule 全文は保存せず、通常 session の起点は synthetic input `[Pulse: <intention_id>]` にする。`PULSE_OK` の場合は送信も通常 session 保存もしない。

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 実装元 |
|---|---|
| Pulse 設定 | `src/config/types.rs`, `src/config/loader.rs`, `src/config/resolve.rs`, `docs/config.md` |
| `PULSE.md` 読み込み | 新規 `src/pulse/definition.rs`、`agents/{agent_id}/PULSE.md` |
| due 判定 / due_key | 新規 `src/pulse/due.rs`、既存 `chrono`, `chrono_tz` |
| `pulse_runs` | `src/storage/migration.rs`, `src/storage/mod.rs`, `src/storage/queries.rs`, `docs/db.md` |
| Gate / Home Surface | 新規 `src/pulse/gate.rs`, `src/pulse/home_surface.rs` |
| Capsule / Activation | 新規 `src/pulse/capsule.rs`, `src/pulse/runner.rs`、既存 `agent_loop` / `LlmProvider` / `ToolRegistry`、turn loop 分離 |
| Scheduler / Runtime 接続 | 新規 `src/pulse/scheduler.rs`, `src/runtime/mod.rs` |
| ドキュメント | `docs/pulse.md`, `docs/config.md`, `docs/db.md`, `docs/architecture.md`, `docs/channels.md` |

---

## Step 0: Worktree 作成

- ブランチ例: `feature/pulse-phase1`
- `git worktree add ../egopulse-pulse-phase1 -b feature/pulse-phase1`
- 作業前に `git status --short` で既存差分を確認する。

---

## Step 1: Pulse Config (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `pulse_config_defaults_disabled` | `pulse` 未指定時は disabled |
| `pulse_config_loads_runtime_fields` | `enabled`, `tick_interval_secs`, `timezone` を YAML から読み込む |
| `pulse_config_rejects_invalid_timezone` | 不正な IANA timezone を拒否する |
| `pulse_config_rejects_zero_tick_interval` | `tick_interval_secs: 0` を拒否する |

### GREEN: 実装

`PulseConfig` を追加し、Phase 1 の config は `enabled`, `tick_interval_secs`, `timezone` に限定する。`pulse.default_delivery`, `pulse.provider`, `pulse.model` は Phase 1 では実装しない。

### コミット

`feat: add pulse runtime config`

---

## Step 2: PULSE.md Parser (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `parse_pulse_definition_loads_front_matter_and_body` | front matter と Markdown body を分離する |
| `parse_daily_weekly_once_intentions` | daily / weekly / once の schedule を型として読む |
| `parse_rejects_duplicate_intention_ids` | agent 内 intention id 重複を拒否する |
| `parse_rejects_invalid_hhmm_and_weekday` | invalid time / weekday を拒否する |
| `load_missing_pulse_definition_returns_empty` | `PULSE.md` が無い agent は intentions 空 |
| `load_rejects_unsafe_agent_id` | path traversal を含む agent_id を拒否する |
| `scheduler_warns_and_continues_on_pulse_parse_error` | `PULSE.md` parse error は本体を止めず、その agent だけ今回 scan から外す |

### GREEN: 実装

`PulseDefinition`, `TemporalIntention`, `TemporalSchedule` を定義する。parse error は `tracing::warn` に agent_id と error を出し、`pulse_runs` には残さず他 agent の scan を継続する。

### コミット

`feat: parse agent pulse definitions`

---

## Step 3: Due Resolver (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `daily_due_after_local_time` | timezone の当日 `HH:MM` 到達後に due |
| `daily_not_due_before_local_time` | 到達前は due ではない |
| `weekly_due_only_on_matching_day` | 指定曜日かつ時刻到達後だけ due |
| `once_due_after_rfc3339_at` | RFC3339 の instant 到達後に due |
| `due_key_daily_uses_local_date` | daily due_key が local date を含む |
| `due_key_weekly_uses_iso_week` | weekly due_key が ISO week を含む |
| `due_key_once_uses_once_instant` | once due_key が scheduled instant を含む |
| `due_resolver_handles_dst_gap_and_fold` | DST gap/fold で安全に判定する |

### GREEN: 実装

daily / weekly は config timezone、once は RFC3339 instant を基準に due 判定と due_key 生成を行う。scheduler 停止中に時刻を過ぎても、当日/当週/once の due_key が未実行なら due とする。

### コミット

`feat: resolve temporal pulse due keys`

---

## Step 4: pulse_runs Schema And Queries (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `migration_adds_pulse_runs_table` | schema v9 で `pulse_runs` と index が作成される |
| `try_create_pulse_run_enforces_due_key_unique` | `(agent_id, intention_id, due_key)` の重複を防ぐ |
| `update_pulse_run_success_notify_records_message` | notify 成功時に `chat_id`, `message_id`, `output_text` を保存する |
| `update_pulse_run_success_silent_records_pulse_ok` | `PULSE_OK` は silent として記録する |
| `update_pulse_run_failed_records_error` | failed と error_message を保存する |
| `update_pulse_run_skipped_records_reason` | Home Surface なしを skipped として保存する |
| `has_pulse_due_run_detects_terminal_or_running_record` | running / success / failed / skipped の既存 due_key を検出する |
| `failed_consumes_due_key_without_retry` | failed は Phase 1 で自動再試行しない |

### GREEN: 実装

`PulseRunStatus`, `PulseOutputKind`, `PulseRun` と CRUD helper を追加する。active turn defer は run record を作らず due_key を消費しない。Home Surface なしは skipped、LLM / tool / 送信失敗は failed として due_key を消費する。

### コミット

`feat: persist pulse run audit records`

---

## Step 5: Gate And Home Surface Resolver (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `gate_blocks_duplicate_due_key` | 既存 due_key があれば落とす |
| `gate_defers_active_agent_without_run_record` | active turn 中は defer し run record を作らない |
| `gate_allows_deferred_due_key_on_next_tick` | defer 後は同じ due_key を再評価できる |
| `home_surface_uses_latest_sendable_agent_chat` | 新しい順に最初の Discord / Telegram chat を採用する |
| `home_surface_skips_web_cli_tui_and_uses_previous_sendable_chat` | Web / CLI / TUI を飛ばし前の送信可能 chat を使う |
| `home_surface_skips_when_no_sendable_chat` | 送信可能 chat が無ければ skipped |
| `home_surface_does_not_use_default_delivery` | Phase 1 では `pulse.default_delivery` に fallback しない |

### GREEN: 実装

agent の chats を `last_message_time DESC` で取得し、Discord / Telegram かつ adapter が存在する最初の chat を Home Surface とする。Web / CLI / TUI は送信不可としてスキップする。

### コミット

`feat: resolve pulse gate and home surface`

---

## Step 6: Pulse Capsule And Activation (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `capsule_includes_contract_intention_notes_memory_and_recent_context` | contract, attention, notes, prospective memory, recent context を含む |
| `capsule_uses_recent_visible_messages_from_messages_table` | `messages` テーブルから直近 10 件を使う |
| `capsule_excludes_full_session_and_internal_history` | session 全文や capsule 内部履歴を含めない |
| `activation_uses_agent_channel_model_resolution` | 通常の agent/channel model 解決を使う |
| `activation_separates_llm_input_from_persisted_session_input` | LLM には Pulse Capsule、通常 session には synthetic input を使える |
| `activation_has_normal_tool_execution_capability` | 通常 turn と同等に tools を実行できる |
| `activation_does_not_use_tiny_llm_gate` | Phase 1 Gate では LLM judge を呼ばない |

### GREEN: 実装

Pulse Capsule を構築し、Gate 通過後に agent を起こす。Activation は通常 turn の tool execution capability を持つ。`pulse.provider` / `pulse.model` は持たず、通常の agent/channel 解決を使う。Recent Visible Context は Home Surface の user-visible messages 直近 10 件とする。

既存 `agent_loop::process_turn()` は `user_input` の永続化、session 復元、LLM/tool loop、出力保存が一体化しているため、Pulse ではそのまま使わない。先に agent loop 内部を分離し、以下を満たす低レベル入口を作る。

- LLM request には Pulse Capsule を今回 turn の入力として渡せる。
- tool execution / tool result handling / model 解決 / active turn tracking は通常 turn と共通化する。
- 通常 session の persisted user message は `[Pulse: <intention_id>]` に差し替えられる。
- Pulse Capsule 全文、Core Contract、front matter 内部値は通常 session に保存しない。
- `PULSE_OK` の場合は synthetic input を含め通常 session に何も保存しない。

### コミット

`feat: execute pulse activations with tools`

---

## Step 7: Output And Session Persistence (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `pulse_ok_sends_nothing_and_persists_no_session` | `PULSE_OK` は送信も通常 session 保存もしない |
| `notify_sends_text_to_home_surface` | 通知本文を Home Surface へ送る |
| `notify_persists_synthetic_input_and_turn_like_normal_conversation` | `[Pulse: intention_id]` 起点と tool calls / assistant response を通常会話と同等に保存する |
| `notify_does_not_store_pulse_capsule_body` | Pulse Capsule 全文や Core Contract は通常 session に保存しない |
| `notify_updates_pulse_run_with_message_id` | 保存した assistant message id を `pulse_runs.message_id` に記録する |
| `notify_marks_failed_when_send_fails` | 送信失敗時は failed とし due_key を消費する |

### GREEN: 実装

通知本文ありの場合、通常 session には synthetic input `[Pulse: <intention_id>]` を起点として、tool call 履歴と最終 assistant response を通常会話と同等に保存する。これは元の `docs/pulse.md` の「通知本文だけ保存」から、壁打ちで決めた仕様変更として扱い、Step 9 で `docs/pulse.md` 側も更新する。`PULSE_OK` の場合は synthetic input も含めて保存しない。

### コミット

`feat: persist pulse notification turns`

---

## Step 8: Scheduler And Runtime Wiring (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `scheduler_disabled_exits_without_scan` | disabled では scan しない |
| `scheduler_scan_loads_all_configured_agents` | configured agents を scan する |
| `scheduler_runs_due_intention_once` | due intention を due_key ごとに 1 回実行する |
| `scheduler_continues_after_agent_parse_error` | 1 agent の parse error で他 agent を止めない |
| `runtime_starts_pulse_scheduler_when_enabled` | `start_channels` が scheduler handle を監視対象に追加する |
| `runtime_requires_channel_even_when_pulse_scheduler_enabled` | scheduler 単独では `NoActiveChannels` のまま |
| `scheduler_continues_after_agent_error` | 1 agent の実行失敗で scan 全体を止めない |

### GREEN: 実装

`src/pulse/scheduler.rs` に tick loop と single scan function を実装し、`runtime::start_channels` に sleep scheduler と同じ supervision pattern で接続する。Phase 1 では Pulse UI / API / 手動実行コマンドは作らない。

### コミット

`feat: run pulse scheduler from runtime`

---

## Step 9: Documentation (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `docs_reference_phase1_pulse_scope` | `docs/pulse.md` に Phase 1 の差分が反映されている |
| `docs_reference_pulse_config_fields` | `docs/config.md` に Phase 1 config が載っている |
| `docs_reference_pulse_runs_schema` | `docs/db.md` に `pulse_runs` が載っている |

### GREEN: 実装

`docs/pulse.md` を実装判断に合わせて更新する。特に `default_delivery` 未実装、Home Surface の送信可能 chat 探索、tools 使用可能、通知本文ありの通常会話同等保存、`PULSE_OK` 非保存を明記する。元仕様と変える箇所は「Phase 1 decisions」または同等の節で、実装者が迷わないよう明示する。

### コミット

`docs: document pulse phase 1 decisions`

---

## Step 10: 動作確認

- `cargo fmt --check`
- `cargo test`
- `cargo check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- 必要に応じて `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
- 手動確認: tempfile state root で `pulse.enabled: true`、短い tick、due な `PULSE.md`、mock provider / mock tool を使い、notify / `PULSE_OK` / skipped / failed / active defer を確認する。

---

## Step 11: PR 作成

- `git status --short` で差分確認
- コミットが意味ごとに分かれていることを確認
- `git push -u origin feature/pulse-phase1`
- Draft PR を作成し、PR description は日本語で `docs/pulse.md` Phase 1 実装、元仕様からの差分、検証コマンド、スコープ外項目を明記する。

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
|---|---|---|
| `src/pulse/mod.rs` | **新規** | Pulse module entrypoint |
| `src/pulse/definition.rs` | **新規** | `PULSE.md` parser |
| `src/pulse/due.rs` | **新規** | due 判定と due_key |
| `src/pulse/gate.rs` | **新規** | duplicate / active turn Gate |
| `src/pulse/home_surface.rs` | **新規** | Home Surface 解決 |
| `src/pulse/capsule.rs` | **新規** | Pulse Capsule 構築 |
| `src/pulse/runner.rs` | **新規** | activation / output / persistence |
| `src/pulse/scheduler.rs` | **新規** | tick loop と scan |
| `src/pulse/pulse_core_contract.md` | **新規** | binary embedded contract |
| `src/agent_loop/turn.rs` | 変更 | 通常 turn と Pulse Activation が共有できる LLM/tool loop の分離 |
| `src/config/types.rs` | 変更 | `PulseConfig` |
| `src/config/loader.rs` | 変更 | YAML 読み込み・検証 |
| `src/config/resolve.rs` | 変更 | pulse accessor |
| `src/storage/migration.rs` | 変更 | schema v9 `pulse_runs` |
| `src/storage/mod.rs` | 変更 | PulseRun 型 |
| `src/storage/queries.rs` | 変更 | PulseRun CRUD / Home Surface query |
| `src/runtime/mod.rs` | 変更 | pulse scheduler 起動・監視 |
| `src/lib.rs` | 変更 | module 追加 |
| `docs/pulse.md` | 変更 | Phase 1 合意差分 |
| `docs/config.md` | 変更 | Phase 1 pulse config |
| `docs/db.md` | 変更 | `pulse_runs` schema |
| `docs/architecture.md` | 変更 | Pulse scheduler/runtime |
| `docs/channels.md` | 変更 | Home Surface と送信可能 channel |

---

## コミット分割

1. `feat: add pulse runtime config` — config 型・loader・docs
2. `feat: parse agent pulse definitions` — parser / parse error handling
3. `feat: resolve temporal pulse due keys` — due resolver
4. `feat: persist pulse run audit records` — DB schema/query
5. `feat: resolve pulse gate and home surface` — Gate / Home Surface
6. `feat: execute pulse activations with tools` — Capsule / Activation / tools
7. `feat: persist pulse notification turns` — output / session persistence
8. `feat: run pulse scheduler from runtime` — scheduler / runtime wiring
9. `docs: document pulse phase 1 decisions` — docs updates

---

## テストケース一覧（全 59 件）

### Pulse Config (4)
1. `pulse_config_defaults_disabled` — 未指定時の既定値
2. `pulse_config_loads_runtime_fields` — runtime fields 読み込み
3. `pulse_config_rejects_invalid_timezone` — 不正 timezone
4. `pulse_config_rejects_zero_tick_interval` — 0 秒 tick 拒否

### PULSE.md Parser (7)
5. `parse_pulse_definition_loads_front_matter_and_body` — front matter/body 分離
6. `parse_daily_weekly_once_intentions` — schedule 3 種
7. `parse_rejects_duplicate_intention_ids` — id 重複拒否
8. `parse_rejects_invalid_hhmm_and_weekday` — invalid schedule
9. `load_missing_pulse_definition_returns_empty` — missing file
10. `load_rejects_unsafe_agent_id` — unsafe agent id
11. `scheduler_warns_and_continues_on_pulse_parse_error` — parse error 継続

### Due Resolver (8)
12. `daily_due_after_local_time` — daily due
13. `daily_not_due_before_local_time` — daily not due
14. `weekly_due_only_on_matching_day` — weekly due
15. `once_due_after_rfc3339_at` — once due
16. `due_key_daily_uses_local_date` — daily due_key
17. `due_key_weekly_uses_iso_week` — weekly due_key
18. `due_key_once_uses_once_instant` — once due_key
19. `due_resolver_handles_dst_gap_and_fold` — DST

### pulse_runs (8)
20. `migration_adds_pulse_runs_table` — migration
21. `try_create_pulse_run_enforces_due_key_unique` — unique
22. `update_pulse_run_success_notify_records_message` — notify success
23. `update_pulse_run_success_silent_records_pulse_ok` — silent success
24. `update_pulse_run_failed_records_error` — failed
25. `update_pulse_run_skipped_records_reason` — skipped
26. `has_pulse_due_run_detects_terminal_or_running_record` — duplicate query
27. `failed_consumes_due_key_without_retry` — failed は再試行しない

### Gate / Home Surface (7)
28. `gate_blocks_duplicate_due_key` — duplicate gate
29. `gate_defers_active_agent_without_run_record` — active defer
30. `gate_allows_deferred_due_key_on_next_tick` — defer 後の再評価
31. `home_surface_uses_latest_sendable_agent_chat` — 最新送信可能 chat
32. `home_surface_skips_web_cli_tui_and_uses_previous_sendable_chat` — Web 等を飛ばす
33. `home_surface_skips_when_no_sendable_chat` — no surface skipped
34. `home_surface_does_not_use_default_delivery` — no fallback

### Capsule / Activation (7)
35. `capsule_includes_contract_intention_notes_memory_and_recent_context` — capsule includes
36. `capsule_uses_recent_visible_messages_from_messages_table` — messages 直近10件
37. `capsule_excludes_full_session_and_internal_history` — session 全文除外
38. `activation_uses_agent_channel_model_resolution` — 通常 model 解決
39. `activation_separates_llm_input_from_persisted_session_input` — LLM input と保存 input を分離
40. `activation_has_normal_tool_execution_capability` — tools 利用可能
41. `activation_does_not_use_tiny_llm_gate` — Gate LLM なし

### Output / Session (6)
42. `pulse_ok_sends_nothing_and_persists_no_session` — `PULSE_OK` 非保存
43. `notify_sends_text_to_home_surface` — 送信
44. `notify_persists_synthetic_input_and_turn_like_normal_conversation` — 通常会話同等保存
45. `notify_does_not_store_pulse_capsule_body` — capsule 非保存
46. `notify_updates_pulse_run_with_message_id` — message_id link
47. `notify_marks_failed_when_send_fails` — send failure

### Scheduler / Runtime (7)
48. `scheduler_disabled_exits_without_scan` — disabled
49. `scheduler_scan_loads_all_configured_agents` — all agents
50. `scheduler_runs_due_intention_once` — once per due_key
51. `scheduler_continues_after_agent_parse_error` — parse error isolation
52. `runtime_starts_pulse_scheduler_when_enabled` — runtime wiring
53. `runtime_requires_channel_even_when_pulse_scheduler_enabled` — scheduler 単独不可
54. `scheduler_continues_after_agent_error` — error isolation

### Documentation (3)
55. `docs_reference_phase1_pulse_scope` — pulse scope docs
56. `docs_reference_pulse_config_fields` — config docs
57. `docs_reference_pulse_runs_schema` — db docs

### Integration / Regression (2)
58. `pulse_notify_can_be_followed_by_normal_turn` — 通知後の通常 turn が文脈を引き継ぐ
59. `pulse_skipped_records_when_no_home_surface` — skipped 記録

---

## 工数見積もり

| Step | 内容 | 見積もり |
|---|---|---|
| Step 1 | Pulse Config | ~140 行 |
| Step 2 | PULSE.md Parser | ~300 行 |
| Step 3 | Due Resolver | ~240 行 |
| Step 4 | pulse_runs Schema And Queries | ~380 行 |
| Step 5 | Gate And Home Surface Resolver | ~260 行 |
| Step 6 | Pulse Capsule And Activation | ~560 行 |
| Step 7 | Output And Session Persistence | ~360 行 |
| Step 8 | Scheduler And Runtime Wiring | ~260 行 |
| Step 9 | Documentation | ~220 行 |
| **合計** |  | **~2,720 行** |
