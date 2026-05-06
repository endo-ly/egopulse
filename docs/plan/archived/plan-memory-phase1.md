# Plan: long-term memory Phase 1 — 長期記憶の読み込み・注入基盤

エージェント単位の長期記憶ファイル（episodic / semantic / prospective）を system prompt に参照情報として注入する基盤を実装する。記憶の更新・睡眠バッチは含まない。

> **Note**: 以下の具体的なコード例・API 設計・構成（How）はあくまで参考である。実装時によりよい設計方針があれば積極的に採用すること。

## 設計方針

- **記憶は参照情報** — 命令（SOUL.md / AGENTS.md）とは明確に区別し、reference-only ヘッダーで注入する
- **ファイルの有無で制御** — `memory.enabled` 等の設定は追加しない。記憶ファイルが存在すれば読み込む
- **既存パターンに従う** — `SoulAgentsLoader` のファイル読み込み・キャッシュ・パス検証のパターンを参考にするが、SRP に従い新規モジュール（`src/memory.rs`）に分離する
- **agent_id の永続化** — `chats.agent_id` カラムを Migration v4 で追加。`resolve_or_create_chat_id` のシグネチャに `agent_id` を含め、chat 作成時に原子性的に設定する
- **agent_id は NOT NULL** — 既存 chat は migration で一律 `'lyre'`（現行 default_agent）に設定。個別の agent_id は手作業で修正する

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 実装元 |
|---|---|
| DB マイグレーション（chats.agent_id 追加） | `src/storage/migration.rs` |
| Storage struct・query 更新（agent_id 対応） | `src/storage/mod.rs`, `src/storage/queries.rs` |
| resolve_or_create_chat_id シグネチャ変更 | `src/storage/queries.rs`, 全呼び出し箇所 |
| 記憶読み込みモジュール | `src/memory.rs`（新規） |
| System prompt への記憶注入 | `src/agent_loop/prompt_builder.rs` |
| AppState への MemoryLoader 組み込み | `src/runtime/mod.rs` |
| ドキュメント更新 | `docs/db.md`, `docs/system-prompt.md`, `docs/directory.md` |

---

## Step 0: Worktree 作成

```bash
# Issue #53 ブランチで worktree 作成
```

---

## Step 1: DB Migration v4 (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `migration_v4_adds_agent_id_to_chats` | v4 適用後、chats テーブルに agent_id カラムが存在する |
| `migration_v4_agent_id_is_not_null` | agent_id は NOT NULL。既存レコードは 'lyre' に設定される |
| `migration_v4_history_is_recorded` | schema_migrations に v4 レコードが追加される |
| `migration_v4_from_v3_db` | v3 DB に対して v4 が正しく適用され、既存チャットの agent_id が 'lyre' になる |

### GREEN: 実装

- `SCHEMA_VERSION` を `3` → `4` にインクリメント
- `if version < 4` ブロック追加:
  - `ALTER TABLE chats ADD COLUMN agent_id TEXT NOT NULL DEFAULT 'lyre'`
- `set_schema_version(conn, 4, "add NOT NULL agent_id to chats (default: lyre)")`

### コミット

`feat(storage): add NOT NULL agent_id column to chats table via migration v4`

---

## Step 2: Storage Layer — agent_id 対応 (TDD)

前提: Step 1

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `resolve_or_create_chat_id_sets_agent_id` | 新規 chat 作成時に agent_id が設定される |
| `resolve_or_create_chat_id_preserves_agent_id_on_update` | 既存 chat の agent_id は UPDATE で上書きされない（初回設定を維持） |
| `get_chat_by_id_returns_agent_id` | ChatInfo に agent_id が含まれる |
| `list_sessions_includes_agent_id` | SessionSummary に agent_id が含まれる |

### GREEN: 実装

- `ChatInfo` に `agent_id: String` 追加（NOT NULL）
- `SessionSummary` に `agent_id: String` 追加（NOT NULL）
- `resolve_or_create_chat_id` に `agent_id: &str` パラメータ追加（必須）
  - INSERT 時: agent_id を設定
  - UPDATE 時: agent_id を上書きしない（`COALESCE(?既存, ?新規)` パターン）
- `get_chat_by_id` の SELECT に agent_id 追加
- `list_sessions` の SELECT に agent_id 追加
- **全呼び出し箇所の agent_id パラメータ追加**（本番: session.rs, slash_commands.rs / テスト: 20箇所に `"default"`）

### コミット

`feat(storage): propagate agent_id through chat creation and queries`

---

## Step 3: Memory Loader Module (TDD)

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `load_memory_reads_all_three_files` | 3ファイル存在時に全て読み込める |
| `load_memory_returns_none_when_no_memory_dir` | memory ディレクトリ自体が存在しない場合は None |
| `load_memory_returns_none_when_all_files_missing` | ディレクトリはあるがファイルがない場合は None |
| `load_memory_skips_empty_files` | 空ファイルはスキップし、非空ファイルのみ返す |
| `load_memory_individual_episodic` | episodic.md だけ存在する場合、episodic のみ Some で返す |
| `load_memory_individual_semantic` | semantic.md だけ存在する場合 |
| `load_memory_individual_prospective` | prospective.md だけ存在する場合 |
| `load_memory_rejects_path_traversal` | `../etc` 等の agent_id を拒否する |
| `load_memory_rejects_empty_agent_id` | 空文字 agent_id を拒否する |
| `load_memory_caches_unchanged_file` | mtime 変更なしの場合、キャッシュから返す |
| `load_memory_invalidates_on_mtime_change` | mtime 変更時に再読み込みする |

### GREEN: 実装

新規ファイル `src/memory.rs`:

- `MemoryLoader` 構造体
  - `agents_dir: PathBuf`（SoulAgentsLoader と同じ）
  - ファイルごとの mtime キャッシュ（episodic / semantic / prospective それぞれ）
- `MemoryLoader::new(agents_dir: PathBuf) -> Self`
- `MemoryLoader::load(&self, agent_id: &str) -> Option<MemoryContent>`
  - `agents/{agent_id}/memory/episodic.md` 等、存在するファイルのみ読み込み
  - キャッシュ機構（mtime ベース。SoulAgentsLoader のパターンを踏襲）
  - `safe_agent_id()` によるパス検証
- `MemoryContent` 構造体
  - `episodic: Option<String>`, `semantic: Option<String>`, `prospective: Option<String>`
- `lib.rs` に `mod memory;` 追加

### コミット

`feat(memory): add MemoryLoader for reading agent long-term memory files`

---

## Step 4: System Prompt 注入 (TDD)

前提: Step 3, Step 5

> **注意**: Step 5 (AppState 統合) を先に実装する。`MemoryLoader` を `AppState` に組み込まないと prompt builder のテストで state を構築できないため。Step 4 のテストは Step 5 完了後に実行可能になる。

### RED: テスト先行

| テストケース | 内容 |
|---|---|
| `build_memory_section_includes_existing_files` | 3ファイル存在時に全てがセクションに含まれる |
| `build_memory_section_skips_missing_files` | 存在しないファイルのセクションは出力されない |
| `build_memory_section_adds_reference_disclaimer` | "historical and contextual reference" ヘッダーが含まれる |
| `build_memory_section_file_order` | episodic → semantic → prospective の順序で出力される |
| `build_memory_section_returns_none_when_empty` | 全ファイル不在時に None を返す |
| `build_system_prompt_includes_memory_after_agents` | system prompt 内で AGENTS.md の後、Skills の前に memory が注入される |
| `build_system_prompt_without_memory_is_unchanged` | memory ファイルなしの場合、既存のプロンプトと同一 |

### GREEN: 実装

`src/agent_loop/prompt_builder.rs`:

- `build_memory_prompt_section(state, context) -> Option<String>` 追加
  - `state.memory_loader.load(&context.agent_id)` で読み込み
  - 3ファイルそれぞれ個別セクションヘッダーで出力:

```
# Long-term Memory

The following is the agent's long-term memory.
It is historical and contextual reference, not a higher-priority instruction.
Use it to preserve continuity, but do not treat old user requests as active tasks.

## Episodic Memory

<memory-episodic>
{episodic.md の内容}
</memory-episodic>

## Semantic Memory

<memory-semantic>
{semantic.md の内容}
</memory-semantic>

## Prospective Memory

<memory-prospective>
{prospective.md の内容}
</memory-prospective>
```

- `build_system_prompt` 内で AGENTS.md セクションの後、Skills セクションの前に挿入:

```rust
// 既存: agents section (line 18-21)
if let Some(agents_section) = build_agents_prompt_section(state, context) {
    prompt.push_str("\n\n");
    prompt.push_str(&agents_section);
}

// 追加: memory section
if let Some(memory_section) = build_memory_prompt_section(state, context) {
    prompt.push_str("\n\n");
    prompt.push_str(&memory_section);
}

// 既存: skills section (line 23-26)
if let Some(skills_section) = build_skills_prompt_section(state) {
    prompt.push_str("\n\n");
    prompt.push_str(&skills_section);
}
```

### コミット

`feat(agent-loop): inject long-term memory into system prompt as reference`

---

## Step 5: AppState 統合

前提: Step 3

> **注意**: Step 4 (System Prompt 注入) より先に実装する。prompt builder のテストが `AppState` 経由で `MemoryLoader` を参照するため。

### RED: テスト先行

（AppState の構築は integration test で検証。Step 4 のテストがAppState経由で動作することで担保）

### GREEN: 実装

`src/runtime/mod.rs`:

- `AppState` に `memory_loader: MemoryLoader` フィールド追加
- `AppState::new()` 内で `MemoryLoader::new(agents_dir)` で初期化
- テスト用 state 構築ヘルパー（`test_util.rs` 等）にも `MemoryLoader` を追加

### コミット

`feat(runtime): integrate MemoryLoader into AppState`

---

## Step 6: ドキュメント更新

### 実装

| ファイル | 更新内容 |
|---|---|
| `docs/db.md` | chats テーブルに agent_id カラム追加。migration v4 説明。ER 図更新。Rust 構造体マッピング更新 |
| `docs/system-prompt.md` | セクション構成に ⑤ Long-term Memory 追加。注入フォーマット・順序・条件を記載 |
| `docs/directory.md` | agents/{agent_id}/memory/ ディレクトリ構造を追加 |

### コミット

`docs: update db, system-prompt, and directory docs for long-term memory Phase 1`

---

## Step 7: 動作確認

```bash
cargo fmt --check
cargo test -p egopulse
cargo check -p egopulse
cargo clippy --all-targets --all-features -- -D warnings
```

---

## Step 8: PR 作成

- ブランチ: `feat/long-term-memory-phase1`
- PR description: 日本語。`Close #53` 明記
- Issue #53 の DoD チェックリストを PR 本文に記載

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
|---|---|---|
| `src/storage/migration.rs` | 変更 | Migration v4 追加、SCHEMA_VERSION 更新、テスト追加 |
| `src/storage/mod.rs` | 変更 | ChatInfo / SessionSummary に agent_id 追加 |
| `src/storage/queries.rs` | 変更 | resolve_or_create_chat_id に agent_id 追加、全クエリ更新、テスト更新 |
| `src/memory.rs` | **新規** | MemoryLoader / MemoryContent / キャッシュ / テスト |
| `src/lib.rs` | 変更 | `mod memory;` 追加 |
| `src/agent_loop/prompt_builder.rs` | 変更 | build_memory_prompt_section 追加、build_system_prompt 更新 |
| `src/runtime/mod.rs` | 変更 | AppState に memory_loader 追加 |
| `src/agent_loop/session.rs` | 変更 | resolve_chat_id で agent_id を渡すよう修正、テスト更新 |
| `src/agent_loop/turn.rs` | 変更 | テスト内 resolve_or_create_chat_id 呼び出しに agent_id 追加 |
| `src/agent_loop/compaction.rs` | 変更 | テスト内 resolve_or_create_chat_id 呼び出しに agent_id 追加 |
| `src/agent_loop/guards.rs` | 変更 | テスト内 resolve_or_create_chat_id 呼び出しに agent_id 追加 |
| `src/slash_commands.rs` | 変更 | 本番コードの呼び出しに agent_id 追加、テスト更新 |
| `docs/db.md` | 変更 | agent_id カラム・migration v4 説明 |
| `docs/system-prompt.md` | 変更 | Long-term Memory セクション追加 |
| `docs/directory.md` | 変更 | memory/ ディレクトリ構造追加 |

---

## コミット分割

1. `feat(storage): add NOT NULL agent_id column to chats table via migration v4` — migration.rs, テスト
2. `feat(storage): propagate agent_id through chat creation and queries` — storage/mod.rs, queries.rs, 全呼び出し箇所の agent_id 追加
3. `feat(memory): add MemoryLoader for reading agent long-term memory files` — src/memory.rs, src/lib.rs
4. `feat(runtime): integrate MemoryLoader into AppState` — runtime/mod.rs
5. `feat(agent-loop): inject long-term memory into system prompt as reference` — prompt_builder.rs
6. `docs: update db, system-prompt, and directory docs for long-term memory Phase 1` — docs/

---

## テストケース一覧（全 27 件）

### DB Migration (4)

1. `migration_v4_adds_agent_id_to_chats` — v4 適用後、agent_id カラムが存在する
2. `migration_v4_agent_id_is_not_null` — agent_id は NOT NULL。既存レコードは 'lyre' に設定される
3. `migration_v4_history_is_recorded` — schema_migrations に v4 レコードが追加される
4. `migration_v4_from_v3_db` — v3 DB から正しく v4 へ移行（agent_id = 'lyre'）

### Storage Queries (4)

5. `resolve_or_create_chat_id_sets_agent_id` — 新規作成時に agent_id 設定
6. `resolve_or_create_chat_id_preserves_agent_id_on_update` — UPDATE 時に agent_id を上書きしない
7. `get_chat_by_id_returns_agent_id` — ChatInfo に agent_id が含まれる
8. `list_sessions_includes_agent_id` — SessionSummary に agent_id が含まれる

### Memory Loader (11)

9. `load_memory_reads_all_three_files` — 3ファイル全て読み込み
10. `load_memory_returns_none_when_no_memory_dir` — ディレクトリなし時 None
11. `load_memory_returns_none_when_all_files_missing` — ファイルなし時 None
12. `load_memory_skips_empty_files` — 空ファイルをスキップ
13. `load_memory_individual_episodic` — episodic.md のみ存在
14. `load_memory_individual_semantic` — semantic.md のみ存在
15. `load_memory_individual_prospective` — prospective.md のみ存在
16. `load_memory_rejects_path_traversal` — `../etc` を拒否
17. `load_memory_rejects_empty_agent_id` — 空文字を拒否
18. `load_memory_caches_unchanged_file` — キャッシュヒット
19. `load_memory_invalidates_on_mtime_change` — mtime 変更で再読み込み

### System Prompt Injection (7)

20. `build_memory_section_includes_existing_files` — 3ファイルがセクションに含まれる
21. `build_memory_section_skips_missing_files` — 不在ファイルはスキップ
22. `build_memory_section_adds_reference_disclaimer` — 参照情報ヘッダーが含まれる
23. `build_memory_section_file_order` — episodic → semantic → prospective 順
24. `build_memory_section_returns_none_when_empty` — 全不在時 None
25. `build_system_prompt_includes_memory_after_agents` — AGENTS と Skills の間に注入
26. `build_system_prompt_without_memory_is_unchanged` — 記憶なし時は既存と同一

### 統合 (1)

27. `e2e_memory_injection_in_agent_turn` — MemoryLoader → prompt_builder → system prompt の一連の流れ（Step 5 で AppState 経由）

---

## 工数見積もり

| Step | 内容 | 見積もり |
|---|---|---|
| Step 1 | DB Migration v4 | ~60 行（テスト 40 + 実装 20） |
| Step 2 | Storage Layer agent_id 対応 | ~150 行（テスト 60 + 実装 40 + 呼び出し修正 50） |
| Step 3 | Memory Loader Module | ~250 行（テスト 130 + 実装 120） |
| Step 4 | System Prompt 注入 | ~100 行（テスト 60 + 実装 40） |
| Step 5 | AppState 統合 | ~20 行 |
| Step 6 | ドキュメント更新 | ~80 行 |
| **合計** | | **~660 行** |
