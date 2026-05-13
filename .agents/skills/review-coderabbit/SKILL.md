---
name: review-coderabbit
description: CodeRabbit CLIでコードレビューを受け取り、指摘に対応するスキル。コミット済み・未コミットの両方に対応。
allowed-tools: Bash, Read, Edit, Write
---

# CodeRabbit レビュー対応

CodeRabbit CLIでレビューを実行し、重要な指摘に対応する。

## 使い方

呼び出し時にユーザーが対象を指定する：

- **committed** — コミット済み差分をレビュー（PR前の最終確認など）
- **uncommitted** — 未コミットの作業中差分をレビュー（コミット前の確認）

指定がなければ `uncommitted` で実行する。

## 実行フロー

### 1. CodeRabbit CLIの実行

対象に応じたコマンドを実行する。

```bash
# committed の場合
coderabbit --prompt-only -t committed --base main

# uncommitted の場合
coderabbit --prompt-only -t uncommitted
```

長時間実行タスクのため、最大30分かかる場合がある。
1分ごとに完了を確認する。

### 2. レビュー結果の評価

完了後、指摘内容を確認し重要度で分類する：

| 分類 | 対応 |
|------|------|
| 重要な指摘 | 修正する |
| 些細・不要な指摘 | 無視する |

### 3. 修正と再実行ループ

指摘に対応した後、再度CodeRabbitを実行して改善を確認。
このループは**最大3回**まで実行可能。

### 4. 結果報告

最終的なレビュー結果と対応内容をユーザーに報告する。
