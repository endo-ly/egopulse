# Microclaw DB Schema — Observability テーブル群

> 監査ログ・時系列メトリクス・LLM使用量追跡。オブザーバビリティファーストの設計。

## テーブル一覧

| テーブル | 用途 | 粒度 |
|----------|------|------|
| audit_logs | セキュリティ・コンプライアンス監査 | イベント単位 |
| metrics_history | 時系列メトリクス集計 | タイムスタンプ単位 |
| llm_usage_logs | LLMリクエスト単位の使用量 | リクエスト単位 |

---

## audit_logs

セキュリティイベントとコンプライアンス監査証跡。

```sql
CREATE TABLE IF NOT EXISTS audit_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL,
    actor TEXT NOT NULL,
    action TEXT NOT NULL,
    target TEXT,
    status TEXT NOT NULL,
    detail TEXT,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_audit_logs_kind_created
    ON audit_logs(kind, created_at DESC);
```

| カラム | 型 | 説明 |
|--------|----|------|
| kind | TEXT | イベント種別（例: `auth`, `api_key`, `admin`） |
| actor | TEXT | 実行者（ユーザーID、セッションID等） |
| action | TEXT | アクション（例: `login`, `create_key`, `revoke`） |
| target | TEXT | 対象リソース（nullable） |
| status | TEXT | 結果（例: `success`, `failure`, `denied`） |
| detail | TEXT | 追加詳細（JSON等） |

**設計ポイント**:
- `(kind, created_at DESC)` で種別ごとの最新イベントを高速に取得
- セキュリティインシデントの調査とコンプライアンス要件の両方に対応

---

## metrics_history

時系列メトリクスの集計テーブル。ダッシュボードやアラートのデータソース。

```sql
CREATE TABLE IF NOT EXISTS metrics_history (
    timestamp_ms INTEGER PRIMARY KEY,
    llm_completions INTEGER NOT NULL DEFAULT 0,
    llm_input_tokens INTEGER NOT NULL DEFAULT 0,
    llm_output_tokens INTEGER NOT NULL DEFAULT 0,
    http_requests INTEGER NOT NULL DEFAULT 0,
    tool_executions INTEGER NOT NULL DEFAULT 0,
    mcp_calls INTEGER NOT NULL DEFAULT 0,
    mcp_rate_limited_rejections INTEGER NOT NULL DEFAULT 0,    -- v10
    mcp_bulkhead_rejections INTEGER NOT NULL DEFAULT 0,        -- v10
    mcp_circuit_open_rejections INTEGER NOT NULL DEFAULT 0,    -- v10
    active_sessions INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_metrics_history_ts
    ON metrics_history(timestamp_ms);
```

| カラム | 型 | 説明 |
|--------|----|------|
| timestamp_ms | INTEGER PK | エポックミリ秒（集計ウィンドウの開始） |
| llm_completions | INTEGER | LLM完了リクエスト数 |
| llm_input_tokens | INTEGER | 入力トークン合計 |
| llm_output_tokens | INTEGER | 出力トークン合計 |
| http_requests | INTEGER | HTTPリクエスト数 |
| tool_executions | INTEGER | ツール実行回数 |
| mcp_calls | INTEGER | MCP呼び出し回数 |
| mcp_rate_limited_rejections | INTEGER | レート制限による拒否数 |
| mcp_bulkhead_rejections | INTEGER | バルクヘッド（並行数制限）による拒否数 |
| mcp_circuit_open_rejections | INTEGER | サーキットブレーカーによる拒否数 |
| active_sessions | INTEGER | アクティブセッション数 |

**設計ポイント**:
- タイムスタンプをミリ秒精度のINTEGER PKとして格納（集計バケットごとに1行）
- MCP（Model Context Protocol）の耐障害性メトリクスを組み込み
- ポイント読み取りと範囲クエリの両方に最適化

---

## llm_usage_logs

LLMリクエスト単位の詳細な使用量ログ。コスト分析とデバッグに使用。

```sql
CREATE TABLE IF NOT EXISTS llm_usage_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    chat_id INTEGER NOT NULL,
    caller_channel TEXT NOT NULL,
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    input_tokens INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    total_tokens INTEGER NOT NULL,
    request_kind TEXT NOT NULL DEFAULT 'agent_loop',
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_llm_usage_chat_created
    ON llm_usage_logs(chat_id, created_at);
CREATE INDEX IF NOT EXISTS idx_llm_usage_created
    ON llm_usage_logs(created_at);
```

| カラム | 型 | 説明 |
|--------|----|------|
| chat_id | INTEGER | 対象チャット |
| caller_channel | TEXT | 呼び出し元チャンネル（`cli`, `web`, `discord` 等） |
| provider | TEXT | LLMプロバイダー（`openai`, `anthropic`, `ollama` 等） |
| model | TEXT | モデル名 |
| input_tokens | INTEGER | 入力トークン数 |
| output_tokens | INTEGER | 出力トークン数 |
| total_tokens | INTEGER | 合計トークン数 |
| request_kind | TEXT | リクエスト種別（`agent_loop`, `memory_reflector`, `summarize` 等） |
| created_at | TEXT | リクエスト日時 |

**設計ポイント**:
- `request_kind` でエージェントループ以外のLLM呼び出し（メモリ抽出、要約等）も識別可能
- `(chat_id, created_at)` でチャットごとのコスト追跡
- `(created_at)` でグローバルな時系列コスト分析
