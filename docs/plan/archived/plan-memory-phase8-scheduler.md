# Plan: long-term memory Phase 8 — Scheduler / 運用

Issue #60 の完了に向け、手動実行できる sleep batch を設定ベースで継続実行できる運用機能へ拡張する。あわせて長期メモリ機能全体の設定・実行・監査・運用ドキュメントを現状コードと一致させる。

> **Note**: 以下の具体的なコード例・API 設計・構成（How）はあくまで参考である。実装時によりよい設計方針があれば積極的に採用すること。

## 設計方針

- **runtime 常駐タスクとして扱う** — sleep scheduler は Web / Discord / Telegram と同じ `egopulse run` のライフサイクル配下に置き、Ctrl-C とチャネル失敗時に停止する。
- **設定は将来の汎用 scheduled jobs を見据えて明示的にする** — Phase 8 では sleep batch だけを対象にするが、schedule / timezone / retry / active defer の概念は将来の CRON 型 agent job と共有できる形にする。
- **schedule/timezone は daily schedule と IANA timezone を正式仕様にする** — Phase 8 の schedule は daily `HH:MM`。timezone は `Asia/Tokyo` などの IANA timezone 名を受け、DST gap/fold の扱いも仕様化する。cron 構文そのものは将来の汎用 jobs で扱う。
- **active agent への影響は deferred execution で定義する** — 対象 agent が会話中なら即時実行せず、設定された defer 分数後に再確認する。会話を停止・中断しない。
- **運用確認は既存監査と status に集約する** — 成功・失敗確認は `sleep_runs`, `memory_snapshots`, tracing log, `egopulse status` で行う。Web UI / TUI の管理画面は追加しない。

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 実装元 | 備考 |
|---|---|---|
| sleep batch scheduler 設定 | `src/config/types.rs`, `src/config/loader.rs`, `src/config/persist.rs`, `src/config/tests.rs`, `src/test_util.rs` | `enabled`, `schedule`, `timezone`, `agents`, `retry` |
| schedule 計算 | `src/sleep_scheduler.rs` | timezone を含む次回実行時刻計算 |
| scheduler 常駐タスク | `src/runtime/mod.rs` | `start_channels` から起動・監視・shutdown |
| sleep batch 実行連携 | `src/sleep_batch.rs`, scheduler module | `SleepRunTrigger::Scheduled` を使う経路 |
| active turn tracking | `src/runtime/mod.rs`, `src/agent_loop/turn.rs` | agent 単位の in-flight turn を追跡 |
| active agent 判定 | `src/storage/queries.rs`, `src/sleep_scheduler.rs` | in-flight turn と直近 activity から defer 判定 |
| retry / defer 実行制御 | `src/sleep_scheduler.rs` | 失敗 retry と active defer |
| 運用ログ / status | `src/sleep_scheduler.rs`, `src/runtime/status.rs`, `src/runtime/mod.rs` | 開始・defer・retry・skip・成功・失敗 |
| Docs | `docs/config.md`, `docs/architecture.md`, `docs/session-lifecycle.md`, `docs/db.md`, `docs/directory.md` | 長期メモリ全体の最終仕様 |

## 決定事項

| 項目 | 決定 |
|---|---|
| schedule 形式 | Phase 8 は `HH:MM` の daily schedule。将来の汎用 job scheduler で cron 構文を追加する前提で、schedule 計算は sleep batch から分離する |
| timezone 形式 | IANA timezone 名を使う。例: `Asia/Tokyo`, `America/New_York`, `UTC` |
| scheduler default | `sleep_batch` 未設定、または `sleep_batch.enabled` 未設定時は disabled |
| enabled 時必須設定 | `sleep_batch.enabled=true` の場合、`sleep_batch.schedule` と `sleep_batch.timezone` は必須 |
| 推奨設定例 | docs には JST 朝4時の例として `schedule: "04:00"` / `timezone: "Asia/Tokyo"` を記載する |
| DST gap | 指定したローカル時刻が存在しない日は、その日の最初に存在する時刻へ繰り下げて1回だけ実行する |
| DST fold | 指定したローカル時刻が2回存在する日は、早い方の instant で1回だけ実行し、遅い方では実行しない |
| retry / backoff | 失敗時は初回を含めて最大3回、5分間隔で retry する。`sleep_batch.retry` 未設定時もこの値を使う |
| agent 対象 | `sleep_batch.agents` 未設定時は全 agent。空配列は実行対象なし。指定時はその agent 群 |
| agent 実行順 | `default_agent` を最初に実行し、残りは agent id 昇順 |
| 同時実行 | agent ごとの既存 `running` 排他を使う。同一 agent が running の場合は skip ログを残して次 agent へ進む |
| active agent 判定 | agent の会話ターン実行中、または直近5分以内に user/assistant message が追加された agent を active とみなす。判定値は Phase 8 仕様として固定する |
| active agent への影響 | active agent は即時実行せず、10分後に再確認する。defer 間隔は Phase 8 仕様として固定する。会話ターンは停止・中断しない |
| scheduler 単独 runtime | `sleep_batch.enabled=true` でも Web / Discord / Telegram が全て無効なら `egopulse run` は起動しない。既存の NoActiveChannels 方針を維持する |
| 運用 UI | Web UI / TUI の管理画面は追加しない。確認手段は DB、tracing log、`egopulse status`、docs とする |
| scheduler module 配置 | `src/sleep_scheduler.rs` に新規作成し、`src/lib.rs` から公開する |
| runtime status | `src/runtime/status.rs` に scheduler の enabled / next_run / last_run / running / deferred / retrying を追加し、scheduler 状態変化ごとに `status.json` を更新する |

## 前提

- Phase 1: memory files を system prompt に参照情報として注入済み。
- Phase 2: `sleep_runs` / `memory_snapshots` による監査基盤が実装済み。
- Phase 3: agent 単位の sleep input 収集と閾値判定が実装済み。
- Phase 4: 手動 `egopulse sleep --agent` と sleep batch skeleton が実装済み。
- Phase 5-7: 1回の LLM 呼び出し、memory file writer、復旧、snapshot、context overflow 判定が実装済み。

## Step 0: Worktree 作成

- Issue #60 用ブランチで worktree を作成する。
- ブランチ名: `feat/memory-phase8-scheduler`
- 既存の Phase 8 用 worktree がある場合はそれを再利用する。

## Step 1: Active Turn Tracking (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `active_turn_tracker_marks_agent_running_during_turn` | process_turn 中の agent が active として見える |
| `active_turn_tracker_clears_agent_after_success` | 成功後に active 状態が解除される |
| `active_turn_tracker_clears_agent_after_error` | error 後も active 状態が解除される |
| `active_turn_tracker_counts_parallel_turns_per_agent` | 同一 agent の並列 turn が全て終わるまで active |
| `active_turn_tracker_is_agent_scoped` | 他 agent の turn は対象 agent に影響しない |

### GREEN: 実装

- `AppState` に agent 単位の in-flight turn tracker を追加する。
- `process_turn_inner` の開始時に対象 agent を active にし、成功・失敗・早期 return の全経路で解除する guard を置く。
- scheduler の active 判定は、この tracker と直近 message activity の OR 条件にする。

### コミット

`feat(runtime): track active agent turns`

## Step 2: Scheduler 設定モデル (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `loads_sleep_batch_enabled` | `sleep_batch.enabled` がパースされる |
| `sleep_batch_enabled_defaults_to_false` | 未指定時は定期実行しない |
| `loads_sleep_batch_schedule` | schedule 文字列が保持される |
| `loads_sleep_batch_timezone` | timezone 文字列が保持される |
| `sleep_batch_enabled_requires_schedule` | enabled 時に schedule 未設定なら config error |
| `sleep_batch_enabled_requires_timezone` | enabled 時に timezone 未設定なら config error |
| `sleep_batch_disabled_allows_missing_schedule_timezone` | disabled 時は schedule/timezone 未設定を許容する |
| `loads_sleep_batch_agents` | agent 一覧が正規化される |
| `sleep_batch_agents_defaults_to_all_agents` | agents 未設定時の対象が全 agent として解決される |
| `sleep_batch_agents_empty_means_no_agents` | agents 空配列は実行対象なしとして解決される |
| `sleep_batch_agent_order_puts_default_first` | default_agent が最初、残りが agent id 昇順になる |
| `loads_sleep_batch_retry_config` | retry 回数・間隔がパースされる |
| `rejects_unknown_sleep_batch_agent` | 存在しない agent を拒否する |
| `rejects_invalid_sleep_batch_schedule` | 不正 schedule を拒否する |
| `rejects_invalid_sleep_batch_timezone` | 不正 timezone を拒否する |
| `persist_preserves_sleep_batch_scheduler_config` | scheduler 設定が保存時に欠落しない |

### GREEN: 実装

- `SleepBatchConfig` に scheduler 用フィールドを追加する。
- YAML 読み込み・正規化・保存の round-trip を更新する。
- agent 参照は `Config.agents` に存在するものだけ許可する。
- agents 未設定時は全 agent を対象にし、空配列は実行対象なしとして扱う。
- retry は未設定時も最大3回・5分間隔にする。active 判定は直近5分、active defer は10分として固定する。
- provider/model 既存仕様は維持し、scheduler の有効化とは独立させる。

### コミット

`feat(config): add sleep batch scheduler configuration`

## Step 3: Schedule 計算 (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `next_run_returns_today_when_time_is_future` | 当日の指定時刻が未来なら当日実行 |
| `next_run_returns_tomorrow_when_time_has_passed` | 指定時刻を過ぎていれば翌日実行 |
| `next_run_uses_configured_iana_timezone` | IANA timezone のローカル時刻で判定する |
| `next_run_handles_utc_timezone` | `UTC` が正しく扱われる |
| `next_run_moves_dst_gap_to_first_valid_time` | DST gap の日は最初の有効時刻へ繰り下げる |
| `next_run_uses_earliest_instant_for_dst_fold` | DST fold の日は早い方の instant で1回だけ実行する |
| `next_run_rejects_invalid_local_time` | 不正な時刻表現を拒否する |
| `scheduler_config_disabled_has_no_next_run` | disabled の場合は次回実行なし |

### GREEN: 実装

- schedule 計算を Tokio timer から独立した純粋関数として分離する。
- timezone は IANA timezone 名として解決する。
- DST gap / fold の挙動を決定事項どおりに実装し、同一 schedule で同じ agent が1日に2回走らないようにする。
- scheduler loop はこの純粋関数の結果だけを使って `tokio::time::sleep_until` に渡す。

### コミット

`feat(sleep-scheduler): calculate next scheduled run`

## Step 4: Scheduler 実行ループ (TDD)

前提: Step 1, Step 2, Step 3

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `scheduler_skips_when_disabled` | disabled では sleep batch を呼ばない |
| `scheduler_runs_configured_agents` | 設定された agent だけ実行する |
| `scheduler_runs_all_agents_when_agents_unset` | agents 未設定時は全 agent を実行する |
| `scheduler_runs_no_agents_when_agents_empty` | agents 空配列では何も実行しない |
| `scheduler_runs_default_agent_first` | default_agent を最初に実行する |
| `scheduler_uses_scheduled_trigger` | `SleepRunTrigger::Scheduled` で run を作る |
| `scheduler_continues_after_agent_failure` | 1 agent 失敗後も他 agent を処理する |
| `scheduler_logs_already_running_as_skip` | `AlreadyRunning` は致命エラーにしない |
| `scheduler_retries_failed_agent` | 失敗時に retry 設定どおり再実行する |
| `scheduler_defers_active_agent` | active agent を10分後へ延期する |
| `scheduler_runs_agent_after_active_defer_when_inactive` | defer 後に inactive なら実行する |

### GREEN: 実装

- scheduler から sleep batch を呼ぶ実行関数を追加する。
- 手動実行と scheduled 実行で共通の core path を使い、trigger だけを分ける。
- agent ごとの失敗は tracing に残し、scheduler task 自体は継続する。
- 失敗した agent は最大3回・5分間隔で再実行する。
- active agent は10分後に再確認し、inactive になっていれば実行する。
- active 状態が続く限り10分ごとに再確認し、同じ calendar day の schedule 枠としては翌日の定刻をまたがない範囲で defer する。

### コミット

`feat(sleep-scheduler): run scheduled sleep batches`

## Step 5: Runtime 統合と shutdown (TDD)

前提: Step 4

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `start_channels_starts_sleep_scheduler_when_enabled` | `egopulse run` で scheduler task が登録される |
| `start_channels_requires_channel_even_when_scheduler_enabled` | scheduler 有効でも channel なしなら NoActiveChannels になる |
| `start_channels_does_not_start_scheduler_when_disabled` | disabled では task 登録されない |
| `shutdown_channel_tasks_stops_scheduler` | shutdown 時に scheduler task が停止対象になる |
| `scheduler_task_failure_is_reported` | scheduler task の予期せぬ終了が runtime error になる |

### GREEN: 実装

- `start_channels` の supervision 対象に scheduler を追加する。
- scheduler は既存 channel runtime に付随する background task として扱い、scheduler 単独では runtime active condition を満たさない。
- Ctrl-C / channel failure 時に scheduler も既存 task shutdown 経路で停止する。

### コミット

`feat(runtime): supervise sleep batch scheduler`

## Step 6: 運用ログと監査確認 (TDD)

前提: Step 4, Step 5

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `scheduled_run_records_success_status` | scheduled 実行成功が `sleep_runs` に残る |
| `scheduled_run_records_failed_status` | scheduled 実行失敗が `sleep_runs` に残る |
| `scheduled_run_records_memory_snapshots` | scheduled 実行でも snapshot が残る |
| `scheduled_run_records_source_chats_json` | scheduled 実行でも source metadata が残る |
| `scheduled_run_logs_agent_start_and_finish` | agent 単位の開始・終了ログを出す |
| `status_includes_sleep_scheduler_state` | `egopulse status` に scheduler 状態が出る |
| `status_updates_when_scheduler_state_changes` | defer / retry / running などの状態変化が status.json に反映される |

### GREEN: 実装

- 既存 `sleep_runs` / `memory_snapshots` を成功・失敗確認の正式な監査手段にする。
- scheduler 固有ログは機密情報を含めず、agent_id / run_id / trigger / status を中心にする。
- runtime status に scheduler enabled / next_run / last_run / running / deferred / retrying を出す。
- scheduler 状態が変化するたびに `status.json` を更新し、`egopulse status` が起動時だけでなく現在の scheduler 状態を読めるようにする。
- 追加テーブルは作らない。

### コミット

`feat(sleep-scheduler): log scheduled run outcomes`

## Step 7: Docs と長期メモリ全体の整合 (TDD)

前提: Step 1-6

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `docs_config_mentions_sleep_batch_enabled` | `sleep_batch.enabled` が docs/config.md にある |
| `docs_config_mentions_sleep_batch_schedule` | `sleep_batch.schedule` が docs/config.md にある |
| `docs_config_mentions_sleep_batch_timezone` | `sleep_batch.timezone` が docs/config.md にある |
| `docs_config_mentions_jst_4am_example` | JST 朝4時の推奨例が docs/config.md にある |
| `docs_config_mentions_sleep_batch_agents` | `sleep_batch.agents` が docs/config.md にある |
| `docs_config_mentions_sleep_batch_retry` | `sleep_batch.retry` が docs/config.md にある |
| `docs_config_mentions_sleep_batch_active_policy` | active 判定5分・defer10分の固定仕様が docs/config.md にある |
| `docs_architecture_mentions_sleep_scheduler` | architecture に scheduler が記載される |
| `docs_architecture_mentions_active_turn_tracking` | architecture に active turn tracking が記載される |
| `docs_db_mentions_sleep_run_scheduled_trigger` | DB docs に scheduled trigger が記載される |
| `docs_session_lifecycle_mentions_active_agent_policy` | active agent への影響方針が記載される |
| `docs_directory_mentions_memory_files` | memory file 配置が最新仕様と一致する |

### GREEN: 実装

- `docs/config.md` に scheduler 設定、デフォルト、JST 朝4時の推奨例を追加する。
- `docs/architecture.md` に runtime scheduler、active turn tracking、sleep batch の関係を追加する。
- `docs/session-lifecycle.md` に会話中 agent と scheduled sleep batch の関係を明記する。
- `docs/db.md` に `sleep_runs` / `memory_snapshots` と scheduled trigger の確認方法を追加する。
- `docs/directory.md` に memory file と backup/tmp の運用上の意味を反映する。
- `docs/commands.md` に `egopulse status` の scheduler 表示を反映する。

### コミット

`docs: document long-term memory scheduler operations`

## Step 8: 動作確認

```bash
cargo fmt --check
cargo test -p egopulse
cargo check -p egopulse
cargo clippy --all-targets --all-features -- -D warnings
```

追加で確認する項目:

- scheduler disabled の既存挙動が変わらないこと。
- `egopulse sleep --agent <id>` の手動実行が引き続き動くこと。
- `egopulse run` で scheduler enabled 時に status とログから次回予定・defer・retry・実行結果を追えること。

## Step 9: PR 作成

- ブランチ: `feat/memory-phase8-scheduler`
- PR description: 日本語
- `Close #60` を明記する。
- 親 Issue #70 の最終 phase であること、長期メモリ全体の運用完了であることを明記する。

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
|---|---|---|
| `Cargo.toml`, `Cargo.lock` | 変更 | IANA timezone 対応依存関係 |
| `src/config/types.rs` | 変更 | SleepBatchConfig scheduler fields |
| `src/config/loader.rs` | 変更 | scheduler config parse / validation |
| `src/config/persist.rs` | 変更 | scheduler config serialization |
| `src/config/tests.rs` | 変更 | config / validation / persist tests |
| `src/test_util.rs` | 変更 | test config default 更新 |
| `src/sleep_scheduler.rs` | **新規** | schedule 計算と scheduler 実行ループ |
| `src/lib.rs` | 変更 | scheduler module 公開範囲追加 |
| `src/sleep_batch.rs` | 変更 | scheduled trigger 用 entrypoint 整理 |
| `src/agent_loop/turn.rs` | 変更 | process_turn 中の active turn tracking |
| `src/runtime/mod.rs` | 変更 | active turn tracker、scheduler task 起動・監視・停止 |
| `src/runtime/status.rs` | 変更 | scheduler status 表示 |
| `src/storage/queries.rs` | 変更 | active agent 判定用 query |
| `docs/config.md` | 変更 | scheduler 設定仕様 |
| `docs/architecture.md` | 変更 | runtime scheduler 概要 |
| `docs/session-lifecycle.md` | 変更 | active agent policy |
| `docs/db.md` | 変更 | 運用確認方法 |
| `docs/directory.md` | 変更 | memory files / backup 運用 |
| `docs/commands.md` | 変更 | status 表示 |

## コミット分割

1. `feat(runtime): track active agent turns` — `src/runtime/mod.rs`, `src/agent_loop/turn.rs`
2. `feat(config): add sleep batch scheduler configuration` — `src/config/*`, `src/test_util.rs`
3. `feat(sleep-scheduler): calculate next scheduled run` — `Cargo.toml`, `Cargo.lock`, `src/sleep_scheduler.rs`, `src/lib.rs`
4. `feat(sleep-scheduler): run scheduled sleep batches` — `src/sleep_scheduler.rs`, `src/sleep_batch.rs`, `src/storage/queries.rs`
5. `feat(runtime): supervise sleep batch scheduler` — `src/runtime/mod.rs`
6. `feat(status): expose sleep scheduler state` — `src/runtime/status.rs`, `src/sleep_scheduler.rs`, `src/runtime/mod.rs`
7. `docs: document long-term memory scheduler operations` — `docs/*`

## テストケース一覧（全 68 件）

### Active Turn Tracking (5)

1. `active_turn_tracker_marks_agent_running_during_turn` — process_turn 中は active
2. `active_turn_tracker_clears_agent_after_success` — success 後に解除
3. `active_turn_tracker_clears_agent_after_error` — error 後に解除
4. `active_turn_tracker_counts_parallel_turns_per_agent` — 並列 turn 全完了まで active
5. `active_turn_tracker_is_agent_scoped` — agent 単位で分離

### 設定 (17)

6. `loads_sleep_batch_enabled` — scheduler 有効化を parse
7. `sleep_batch_enabled_defaults_to_false` — 未指定時 disabled
8. `loads_sleep_batch_schedule` — schedule parse
9. `loads_sleep_batch_timezone` — timezone parse
10. `sleep_batch_enabled_requires_schedule` — enabled 時 schedule 必須
11. `sleep_batch_enabled_requires_timezone` — enabled 時 timezone 必須
12. `sleep_batch_disabled_allows_missing_schedule_timezone` — disabled 時は schedule/timezone なしを許容
13. `loads_sleep_batch_agents` — agents parse
14. `sleep_batch_agents_defaults_to_all_agents` — 全 agent fallback
15. `sleep_batch_agents_empty_means_no_agents` — 空配列は対象なし
16. `sleep_batch_agent_order_puts_default_first` — default_agent が先頭
17. `loads_sleep_batch_retry_config` — retry 設定 parse
18. `rejects_unknown_sleep_batch_agent` — unknown agent 拒否
19. `rejects_invalid_sleep_batch_schedule` — invalid schedule 拒否
20. `rejects_invalid_sleep_batch_timezone` — invalid timezone 拒否
21. `rejects_invalid_sleep_batch_retry_config` — invalid retry 拒否
22. `persist_preserves_sleep_batch_scheduler_config` — persist round-trip

### Schedule 計算 (8)

23. `next_run_returns_today_when_time_is_future` — 当日未来時刻
24. `next_run_returns_tomorrow_when_time_has_passed` — 翌日繰り越し
25. `next_run_uses_configured_iana_timezone` — IANA timezone 反映
26. `next_run_handles_utc_timezone` — UTC 対応
27. `next_run_moves_dst_gap_to_first_valid_time` — DST gap 繰り下げ
28. `next_run_uses_earliest_instant_for_dst_fold` — DST fold は早い instant
29. `next_run_rejects_invalid_local_time` — 不正時刻拒否
30. `scheduler_config_disabled_has_no_next_run` — disabled は次回なし

### Scheduler 実行 (13)

31. `scheduler_skips_when_disabled` — disabled skip
32. `scheduler_runs_configured_agents` — configured agents 実行
33. `scheduler_runs_all_agents_when_agents_unset` — 未設定なら全 agent 実行
34. `scheduler_runs_no_agents_when_agents_empty` — 空配列なら実行なし
35. `scheduler_runs_default_agent_first` — default_agent 先頭
36. `scheduler_uses_scheduled_trigger` — scheduled trigger
37. `scheduler_continues_after_agent_failure` — agent failure 継続
38. `scheduler_logs_already_running_as_skip` — AlreadyRunning skip
39. `scheduler_retries_failed_agent` — retry 実行
40. `scheduler_waits_five_minutes_between_retries` — retry 間隔5分
41. `scheduler_stops_retry_after_three_attempts` — 初回含め最大3回
42. `scheduler_defers_active_agent` — active defer
43. `scheduler_runs_agent_after_active_defer_when_inactive` — defer 後 inactive なら実行

### Runtime 統合 (5)

44. `start_channels_starts_sleep_scheduler_when_enabled` — runtime 起動
45. `start_channels_requires_channel_even_when_scheduler_enabled` — scheduler 単独では起動不可
46. `start_channels_does_not_start_scheduler_when_disabled` — disabled 非起動
47. `shutdown_channel_tasks_stops_scheduler` — shutdown 対象
48. `scheduler_task_failure_is_reported` — task failure 報告

### 監査・ログ・Status (6)

49. `scheduled_run_records_success_status` — success run 記録
50. `scheduled_run_records_failed_status` — failed run 記録
51. `scheduled_run_records_memory_snapshots` — snapshot 記録
52. `scheduled_run_records_source_chats_json` — source metadata 記録
53. `scheduled_run_logs_agent_start_and_finish` — operational log
54. `status_includes_sleep_scheduler_state` — status 表示
55. `status_updates_when_scheduler_state_changes` — status.json 更新

### Docs (12)

56. `docs_config_mentions_sleep_batch_enabled` — enabled docs
57. `docs_config_mentions_sleep_batch_schedule` — schedule docs
58. `docs_config_mentions_sleep_batch_timezone` — timezone docs
59. `docs_config_mentions_jst_4am_example` — JST 04:00 example
60. `docs_config_mentions_sleep_batch_agents` — agents docs
61. `docs_config_mentions_sleep_batch_retry` — retry docs
62. `docs_config_mentions_sleep_batch_active_policy` — active policy docs
63. `docs_architecture_mentions_sleep_scheduler` — architecture docs
64. `docs_architecture_mentions_active_turn_tracking` — active tracking docs
65. `docs_db_mentions_sleep_run_scheduled_trigger` — db docs
66. `docs_session_lifecycle_mentions_active_agent_policy` — active policy docs
67. `docs_directory_mentions_memory_files` — directory docs

### Regression (1)

68. `manual_sleep_batch_still_uses_manual_trigger` — 手動 sleep は manual trigger のまま

## 工数見積もり

| Step | 内容 | 見積もり |
|---|---|---|
| Step 1 | Active Turn Tracking | ~180 行 |
| Step 2 | Scheduler 設定モデル | ~240 行 |
| Step 3 | Schedule 計算 | ~260 行 |
| Step 4 | Scheduler 実行ループ | ~360 行 |
| Step 5 | Runtime 統合と shutdown | ~160 行 |
| Step 6 | 運用ログと監査確認 | ~240 行 |
| Step 7 | Docs と長期メモリ全体の整合 | ~280 行 |
| Step 8 | 動作確認 | ~0 行 |
| Step 9 | PR 作成 | ~0 行 |
| **合計** |  | **~1,720 行** |
