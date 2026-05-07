# Plan: long-term memory Phase 3 — 入力収集

睡眠バッチの入力となるセッション情報を agent 単位で収集し、実行すべきかスキップすべきかを判定する。実際のバッチ実行・LLM 呼び出し・記憶ファイルの変更は含まない。

> **Note**: 以下の具体的なコード例・API 設計・構成（How）はあくまで参考である。実装時によりよい設計方針があれば積極的に採用すること。

## 設計方針

- **メッセージ数でスキップ判定** — 前回 successful run 以降の新規メッセージ数が 4件（2往復）以下ならスキップ。5件以上で Proceed。セッション数ではなくメッセージ数で判定する
- **Config は追加しない** — スキップ閾値・上限値は定数ハードコード。将来 Phase 8 (Scheduler) で設定化する
- **古いものから切る** — セッション数が上限を超える場合、古いもの（updated_at が古い）から除外する。LLM による digest 生成は行わない
- **Phase 1 + Phase 2 が merge 済みであることが前提** — chats.agent_id（v4）と sleep_runs テーブル（v5）に依存する。`src/memory.rs` と `src/lib.rs` の `mod memory;` は Phase 1 で追加済み
- **source_chats_json にメタデータを記録** — 設計書（#70）の source_chats_json 形式に従い、各セッションの chat_id / channel / updated_at / message_count / estimated_tokens を保存する

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 実装元 |
|---|---|
| セッション列挙・メッセージ計数クエリ | `src/storage/queries.rs` |
| 入力セッションのメタデータ構造体 | `src/storage/mod.rs` |
| 入力収集ロジック（スキップ判定含む） | `src/memory.rs` |
| ドキュメント更新 | `docs/db.md` |

---

## Step 0: Worktree 作成

```bash
# Issue #55 ブランチで worktree 作成
```

---

## Step 1: Storage — 入力セッション用クエリ (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `count_agent_messages_since_counts_correctly` | 指定時刻以降のメッセージ数が正しくカウントされる |
| `count_agent_messages_since_with_no_cutoff` | cutoff なし（初回実行）で全メッセージをカウント |
| `count_agent_messages_since_returns_zero_for_unknown_agent` | 存在しない agent_id で 0 を返す |
| `count_agent_messages_since_excludes_other_agents` | 他 agent のメッセージは含まれない |
| `get_agent_sessions_since_returns_sessions` | 指定時刻以降のセッションメタデータを返す |
| `get_agent_sessions_since_ordered_by_updated_at_desc` | updated_at 降順で返す（最新が先頭） |
| `get_agent_sessions_since_respects_limit` | limit で件数制限される |
| `get_agent_sessions_since_with_no_cutoff` | cutoff なしで全セッションを返す |
| `get_agent_sessions_since_returns_empty_for_unknown_agent` | 存在しない agent_id で空 Vec |
| `get_agent_sessions_includes_message_count` | AgentSessionInfo の message_count が正しい |
| `get_agent_sessions_includes_estimated_tokens` | AgentSessionInfo の estimated_tokens が正（chars/3 近似） |

### GREEN: 実装

`src/storage/mod.rs` に追加:

- `AgentSessionInfo` 構造体
  - `chat_id: i64`
  - `channel: String`
  - `external_chat_id: String`
  - `updated_at: String`（sessions.updated_at）
  - `message_count: i64`
  - `estimated_tokens: i64`（chars/3 近似）

`src/storage/queries.rs` に追加:

- `count_agent_messages_since(agent_id, since: Option<&str>) -> Result<i64, StorageError>`
  - `since` = None の場合、その agent の全メッセージをカウント
  - SQL: messages JOIN chats WHERE agent_id AND timestamp > since
- `get_agent_sessions_since(agent_id, since: Option<&str>, limit: usize) -> Result<Vec<AgentSessionInfo>, StorageError>`
  - chats JOIN sessions WHERE agent_id AND updated_at > since
  - ORDER BY updated_at DESC LIMIT
  - message_count はサブクエリ `(SELECT COUNT(*) FROM messages WHERE chat_id = c.chat_id)`
  - estimated_tokens は `LENGTH(s.messages_json) / 3`（chars/3 近似、既存 compaction と同じ手法）

### コミット

`feat(storage): add queries for agent session enumeration and message counting`

---

## Step 2: 入力収集ロジック (TDD)

前提: Step 1

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `collect_returns_skip_when_no_messages` | 新規メッセージ 0件で Skip を返す |
| `collect_returns_skip_when_below_threshold` | 新規メッセージ 4件（閾値と同数）で Skip を返す |
| `collect_returns_proceed_above_threshold` | 新規メッセージ 5件（閾値超過）で Proceed を返す |
| `collect_returns_proceed_with_many_messages` | 新規メッセージ 10件で Proceed を返す |
| `collect_since_last_successful_run` | 前回 run の finished_at 以降のみカウント（started_at だと実行中のメッセージを取りこぼす） |
| `collect_first_run_no_previous_run` | 前回 run なしの場合、全セッションが対象 |
| `collect_respects_max_sessions_limit` | セッション数が上限超過時、最新 N 件のみ |
| `collect_source_chats_json_format` | source_chats_json が設計書の形式に一致する |
| `collect_source_chats_json_sorted_newest_first` | source_chats_json が updated_at 降順 |
| `collect_skip_includes_reason_and_count` | Skip に reason と new_message_count が含まれる |

### GREEN: 実装

`src/memory.rs` に追加:

- 定数:
    - `const SKIP_THRESHOLD: i64 = 4` — 新規メッセージが 4件以下（2往復以下）ならスキップ
  - `const MAX_SOURCE_SESSIONS: usize = 20` — 最大入力セッション数
- `InputDecision` enum:
  - `Skip { reason: String, new_message_count: i64 }`
  - `Proceed { sessions: Vec<AgentSessionInfo>, source_chats_json: String }`
- `collect_sleep_input(db: &Database, agent_id: &str) -> Result<InputDecision, StorageError>`
  1. `db.get_latest_successful_run(agent_id)` で cutoff を決定（**`finished_at` を使用**。`started_at` だと run 実行中に到着したメッセージを取りこぼすため）
  2. `db.count_agent_messages_since(agent_id, cutoff)` で新規メッセージ数をカウント
  3. メッセージ数 ≤ `SKIP_THRESHOLD`（4）なら `Skip` を返す（4件以下で Skip）
  4. `db.get_agent_sessions_since(agent_id, cutoff, MAX_SOURCE_SESSIONS)` でセッション一覧を取得
  5. `source_chats_json` を設計書形式で生成
  6. `Proceed` を返す

`source_chats_json` の形式（設計書 #70 より）:
```json
[
  {
    "chat_id": 12,
    "channel": "discord",
    "external_chat_id": "1234567890",
    "updated_at": "2026-05-05T02:40:00+09:00",
    "message_count": 84,
    "estimated_tokens": 21000
  }
]
```

### コミット

`feat(memory): add sleep input collection with skip logic based on message count`

---

## Step 3: ドキュメント更新

### 実装

| ファイル | 更新内容 |
|---|---|
| `docs/db.md` | AgentSessionInfo 構造体マッピング追加、操作一覧に新クエリ追加 |

### コミット

`docs: update db.md with agent session queries for sleep input collection`

---

## Step 4: 動作確認

```bash
cargo fmt --check
cargo test -p egopulse
cargo check -p egopulse
cargo clippy --all-targets --all-features -- -D warnings
```

---

## Step 5: PR 作成

- ブランチ: `feat/memory-phase3-input-collection`
- PR description: 日本語。`Close #55` 明記
- Issue #55 の DoD チェックリストを PR 本文に記載

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
|---|---|---|
| `src/storage/mod.rs` | 変更 | AgentSessionInfo 構造体追加 |
| `src/storage/queries.rs` | 変更 | count_agent_messages_since / get_agent_sessions_since クエリ追加、テスト追加 |
| `src/memory.rs` | 変更 | InputDecision enum / collect_sleep_input / 定数 / テスト追加（Phase 1 で `mod memory;` 登録済み） |
| `docs/db.md` | 変更 | AgentSessionInfo マッピング、新クエリ操作一覧 |

---

## コミット分割

1. `feat(storage): add queries for agent session enumeration and message counting` — storage/mod.rs, queries.rs
2. `feat(memory): add sleep input collection with skip logic based on message count` — memory.rs
3. `docs: update db.md with agent session queries for sleep input collection` — docs/

---

## テストケース一覧（全 21 件）

### Storage Queries (11)

1. `count_agent_messages_since_counts_correctly` — 指定時刻以降のメッセージ数
2. `count_agent_messages_since_with_no_cutoff` — 初回全件カウント
3. `count_agent_messages_since_returns_zero_for_unknown_agent` — 存在しない agent で 0
4. `count_agent_messages_since_excludes_other_agents` — 他 agent 除外
5. `get_agent_sessions_since_returns_sessions` — セッションメタデータ取得
6. `get_agent_sessions_since_ordered_by_updated_at_desc` — 降順
7. `get_agent_sessions_since_respects_limit` — 件数制限
8. `get_agent_sessions_since_with_no_cutoff` — 初回全件
9. `get_agent_sessions_since_returns_empty_for_unknown_agent` — 空結果
10. `get_agent_sessions_includes_message_count` — message_count 正確
11. `get_agent_sessions_includes_estimated_tokens` — estimated_tokens 正確

### Input Collection Logic (10)

12. `collect_returns_skip_when_no_messages` — 0件 Skip
13. `collect_returns_skip_when_at_threshold` — 4件 Skip（≤ 4）
14. `collect_returns_proceed_above_threshold` — 5件 Proceed（> 4）
15. `collect_returns_proceed_with_many_messages` — 10件 Proceed
16. `collect_since_last_successful_run` — 前回 run 以降のみ
17. `collect_first_run_no_previous_run` — 初回全セッション
18. `collect_respects_max_sessions_limit` — セッション数上限
19. `collect_source_chats_json_format` — JSON 形式一致
20. `collect_source_chats_json_sorted_newest_first` — 降順
21. `collect_skip_includes_reason_and_count` — Skip に reason/count 含む

---

## 工数見積もり

| Step | 内容 | 見積もり |
|---|---|---|
| Step 1 | Storage クエリ + テスト | ~180 行（テスト 110 + 実装 70） |
| Step 2 | 入力収集ロジック + テスト | ~200 行（テスト 120 + 実装 80） |
| Step 3 | ドキュメント更新 | ~40 行 |
| **合計** | | **~420 行** |
