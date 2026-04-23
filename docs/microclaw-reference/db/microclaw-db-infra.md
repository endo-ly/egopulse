# Microclaw DB Schema — Infra テーブル群

> スキーマバージョン管理とメタデータ。

---

## db_meta

キーバリュー形式のデータベースメタデータ。

```sql
CREATE TABLE IF NOT EXISTS db_meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
```

| カラム | 型 | 説明 |
|--------|----|------|
| key | TEXT PK | メタデータキー |
| value | TEXT | メタデータ値 |

**用途**:
- 現在のスキーマバージョンの追跡（key = `schema_version`）
- その他ランタイム設定の永続化

**格納例**:

| key | value |
|-----|-------|
| schema_version | `19` |

---

## schema_migrations

スキーママイグレーションの適用履歴。

```sql
CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at TEXT NOT NULL,
    note TEXT
);
```

| カラム | 型 | 説明 |
|--------|----|------|
| version | INTEGER PK | マイグレーションバージョン番号 |
| applied_at | TEXT | 適用日時 |
| note | TEXT | マイグレーションの説明 |

**設計ポイント**:
- `db_meta` の `schema_version` と合わせて、起動時に未適用のマイグレーションを検出して実行
- `note` で各バージョンの変更内容を人間が確認可能
- 詳細なマイグレーション履歴は [microclaw-db-migration-history.md](./microclaw-db-migration-history.md) を参照
