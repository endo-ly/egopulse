# Microclaw DB Schema Overview

> 参考元: `/root/workspace/microclaw/crates/microclaw-storage/src/db.rs`
> スキーマバージョン: v19

## 全体構成

```
microclaw.db (SQLite / WAL mode)
├── Core（チャット・メッセージ・セッション）
│   ├── chats
│   ├── messages
│   └── sessions
├── Task Scheduling（定期実行・DLQ）
│   ├── scheduled_tasks
│   ├── task_run_logs
│   └── scheduled_task_dlq
├── Memory（知識管理・抽出・注入）
│   ├── memories
│   ├── memory_reflector_state
│   ├── memory_reflector_runs
│   ├── memory_injection_logs
│   └── memory_supersede_edges
├── Auth（認証・認可）
│   ├── auth_passwords
│   ├── auth_sessions
│   ├── api_keys
│   └── api_key_scopes
├── Observability（監査・メトリクス・コスト）
│   ├── audit_logs
│   ├── metrics_history
│   └── llm_usage_logs
├── Sub-Agent（並列実行・親子管理）
│   ├── subagent_runs
│   ├── subagent_announces
│   ├── subagent_events
│   └── subagent_focus_bindings
└── Infra（メタデータ・マイグレーション）
    ├── db_meta
    └── schema_migrations
```

## テーブル数とインデックス数

| ドメイン | テーブル数 | 主な用途 |
|----------|-----------|---------|
| Core | 3 | 会話の永続化・再開 |
| Task Scheduling | 3 | Cron/一回限りのバックグラウンドタスク |
| Memory | 5 | LLMの長期記憶の抽出・保存・検索・注入 |
| Auth | 4 | WebUI/APIのアクセス制御 |
| Observability | 3 | LLMコスト追跡・監査ログ |
| Sub-Agent | 4 | 並列エージェントの実行管理 |
| Infra | 2 | スキーマバージョン・メタデータ |
| **合計** | **24** | |

## 設計方針

- **マイグレーション**: バージョンベースのインクリメンタル適用。`db_meta` で現在バージョンを管理し、`schema_migrations` で適用履歴を記録
- **タイムスタンプ**: すべて RFC3339 TEXT 形式（`2024-01-01T00:00:00Z`）
- **JSON 格納**: 複雑な構造（セッションメッセージ等）は TEXT として格納（SQLite JSON 型は不使用）
- **外部キー**: 最小限（明示的な FK はほぼなし）。アプリケーション層で整合性を担保
- **複合インデックス**: 主要なクエリパターン（chat_id + timestamp, status + next_run 等）に最適化

## 関連ドキュメント

- [microclaw-db-core.md](./microclaw-db-core.md) — Core テーブル（chats, messages, sessions）
- [microclaw-db-memory.md](./microclaw-db-memory.md) — Memory テーブル群
- [microclaw-db-task-scheduling.md](./microclaw-db-task-scheduling.md) — Task Scheduling テーブル群
- [microclaw-db-auth.md](./microclaw-db-auth.md) — Auth テーブル群
- [microclaw-db-observability.md](./microclaw-db-observability.md) — Observability テーブル群
- [microclaw-db-subagent.md](./microclaw-db-subagent.md) — Sub-Agent テーブル群
- [microclaw-db-infra.md](./microclaw-db-infra.md) — Infra テーブル群
- [microclaw-db-migration-history.md](./microclaw-db-migration-history.md) — スキーママイグレーション履歴
