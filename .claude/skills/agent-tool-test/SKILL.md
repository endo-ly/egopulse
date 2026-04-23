---
name: "agent-tool-test"
description: "ToolCall テスト実行スキル。AIが自律的にTmuxでBEを起動し、ログを収集しながら全モデル×全ツールのテストを実行し、テスト結果とログから不具合原因を特定する。"
allowed-tools: "Bash, Read, Write"
---

# Agent Tool Test

ToolCall テストを自律実行するスキル。バックエンドで定義されている全ツールについて、全モデルでの実行テストを行います。

## 概要

- Tmuxでバックエンドサーバーを起動
- バックエンドで定義されている全ツール × 全モデルの組み合わせでテスト実行
- ログを収集しながら結果を分析
- 不具合が発生した場合はログから原因を特定

## ワークフロー

### 1. 事前確認

以下の情報を確認します：

- サービスポート（デフォルト: 8000）
- テスト対象モデル（未指定なら全モデル）
- テストパラメータ（日付範囲、limit、granularity）

### 2. 利用可能なツール・モデルの確認

```bash
# モデル一覧取得
curl -s http://127.0.0.1:8000/v1/chat/models | jq '.models[].id'

# ツール一覧取得（動的）
curl -s http://127.0.0.1:8000/v1/chat/tools | jq '.tools[].name'
```

### 3. ポート確保とBE起動

```bash
# ポート解放
fuser -k 8000/tcp 2>/dev/null || true

# tmuxセッションでBE起動
cd /root/workspace/ego-graph
tmux new-session -d -s "egograph-backend" "uv run python -m backend.main"
```

### 4. 起動確認

```bash
# 健康チェック
sleep 3
curl -s http://127.0.0.1:8000/health || echo "BE起動失敗"
```

### 5. ツールマトリクステスト実行

スキルフォルダ内のスクリプトを実行します：

```bash
# スキルフォルダのスクリプトを実行
uv run python .claude/skills/agent-tool-test/run_tool_matrix.py \
  --api-url http://127.0.0.1:8000 \
  --models "glm-4.7" \
  --start-date 2026-01-01 \
  --end-date 2026-01-31 \
  --limit 5
```

または、プロジェクトのスクリプトを使用：

```bash
uv run python backend/scripts/run_tool_matrix.py \
  --api-url http://127.0.0.1:8000 \
  --models "<MODEL_LIST>" \
  --start-date 2026-01-01 \
  --end-date 2026-01-31 \
  --limit 5 \
  --granularity day
```

### 6. ログ収集と分析

```bash
# tmuxログ取得
tmux capture-pane -p -t "egograph-backend" -S -500

# 結果ファイル確認（スクリプト使用時）
cat tool_matrix_results_*.json | jq '.results[] | select(.llm_response | test("error|Error|ERROR"; "i"))'
```

### 7. 結果分析と不具合特定

以下の観点で分析します：

- **ステータスコード**: HTTP 200以外のレスポンス
- **ツール呼び出し成功率**: 各ツールが正しく呼び出されたか
- **LLM応答内容**: 期待通りのツール呼び出しが行われたか
- **エラーログ**: バックエンド側のエラーログ
- **モデル別傾向**: 特定のモデルでのみ失敗するケース

### 8. クリーンアップ

```bash
# セッション終了
tmux kill-session -t "egograph-backend"
```

## 期待出力

- テスト実行のサマリー（成功/失敗数）
- 失敗ケースの詳細（モデル、ツール、エラー内容）
- 不具合原因の特定結果
- 必要に応じて修正提案

## ガードレール

- シークレット（APIキー等）をログに含めない
- 1変更ずつテストし、影響範囲を明確にする
- 最小限の再現ケースから始める
- ツール定義の変更を検知した場合は再テスト

## コマンドテンプレート

### 特定モデルのみテスト

```bash
uv run python .claude/skills/agent-tool-test/run_tool_matrix.py \
  --api-url http://127.0.0.1:8000 \
  --models "glm-4.7" \
  --limit 3
```

### 複数モデルを指定

```bash
uv run python .claude/skills/agent-tool-test/run_tool_matrix.py \
  --api-url http://127.0.0.1:8000 \
  --models "glm-4.7,xiaomi/mimo-v2-flash:free"
```

### タイムアウト延长

```bash
uv run python .claude/skills/agent-tool-test/run_tool_matrix.py \
  --api-url http://127.0.0.1:8000 \
  --timeout 300.0
```

### 単一ツールの直接テスト（デバッグ用）

```bash
curl -N -s -X POST http://127.0.0.1:8000/v1/chat \
  -H "Content-Type: application/json" \
  -d '{
    "model_name": "glm-4.7",
    "stream": true,
    "messages": [
      {
        "role": "user",
        "content": "Call get_top_tracks with start_date=2026-01-01, end_date=2026-01-31, limit=5. Return only tool calls."
      }
    ]
  }'
```
