# Microclaw DB Schema — Migration History

> スキーマバージョン v1〜v19 の全変更履歴。

## マイグレーション戦略

- **起動時自動適用**: `db_meta.schema_version` を読み取り、未適用の ALTER TABLE を逐次実行
- **ロールバックなし**: 前方互換のみ。ダウングレードは未サポート
- **DDLはRustコード内**: 個別SQLファイルではなく、`db.rs` の `run_migrations()` 内で管理

---

## Version History

| Version | 追加テーブル | カラム変更 | 概要 |
|---------|-------------|-----------|------|
| **v1** | db_meta, schema_migrations, chats, messages, sessions, memories | — | 初期スキーマ。バージョン管理基盤導入 |
| **v2** | — | chats: +channel, +external_chat_id<br>memories: +embedding_model, +confidence, +source, +last_seen_at, +is_archived, +archived_at, +chat_channel, +external_chat_id | チャットアイデンティティマッピング + メモリ強化（信頼度・アーカイブ） |
| **v3** | memory_reflector_runs, memory_injection_logs | — | メモリ抽出・注入のログテーブル追加 |
| **v4** | memory_supersede_edges | — | メモリ置換関係グラフの追加 |
| **v5** | auth_passwords, auth_sessions, api_keys, api_key_scopes | — | 認証・認可システムの追加 |
| **v6** | — | sessions: +label, +thinking_level, +verbose_level, +reasoning_level, +parent_session_key, +fork_point | セッション設定の永続化 + フォーク機能 |
| **v7** | metrics_history | — | 時系列メトリクステーブルの追加 |
| **v8** | audit_logs | api_keys: +expires_at, +rotated_from_key_id | APIキーの有効期限・ローテーション + 監査ログ |
| **v9** | scheduled_task_dlq | — | スケジュールタスクのデッドレターキュー |
| **v10** | — | metrics_history: +mcp_rate_limited_rejections, +mcp_bulkhead_rejections, +mcp_circuit_open_rejections | MCP耐障害性メトリクスの追加 |
| **v11** | — | sessions: +skill_envs_json | セッション内スキル環境変数の永続化 |
| **v12** | — | scheduled_tasks: +timezone | スケジュールタスクのタイムゾーン対応 |
| **v13** | subagent_runs | — | サブエージェント実行テーブルの追加 |
| **v14** | — | subagent_runs: +parent_run_id, +depth | サブエージェントの親子関係・ネスト深さ |
| **v15** | subagent_announces | — | サブエージェント完了通知キュー |
| **v16** | subagent_events | — | サブエージェントイベントタイムライン |
| **v17** | subagent_focus_bindings | — | スレッド↔サブエージェントのフォーカスバインディング |
| **v18** | — | subagent_runs: +token_budget, +artifact_json | サブエージェントのトークン予算 + 成果物 |
| **v19** | — | （sessions マイグレーションの再適用） | セッションスキーマの修正適用 |

---

## ドメイン別の成長タイムライン

### Core
```
v1  chats, messages, sessions（基本構成）
v6  sessions 拡張（設定・フォーク）
v11 sessions スキル環境変数
```

### Memory
```
v1  memories（基本）
v2  信頼度・アーカイブ機能追加
v3  抽出・注入ログ
v4  置換関係グラフ
```

### Auth
```
v5  認証・認可システム（パスワード・セッション・APIキー）
v8  APIキーローテーション + 監査ログ
```

### Observability
```
v7  時系列メトリクス
v8  監査ログ
v10 MCP耐障害性メトリクス
```

### Task Scheduling
```
v1  （scheduled_tasks は初期から存在）
v9  DLQ（デッドレターキュー）
v12 タイムゾーン対応
```

### Sub-Agent
```
v13 実行テーブル
v14 親子関係・ネスト
v15 完了通知キュー
v16 イベントタイムライン
v17 フォーカスバインディング
v18 トークン予算 + 成果物
```
