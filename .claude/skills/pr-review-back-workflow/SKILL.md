---
name: pr-review-back-workflow
description: PRレビューコメント対応ワークフロー（レビュー抽出→修正→CIチェック→コミット→サマリーまで一括実行）
allowed-tools: Bash, Read, Write, Edit, AskUserQuestion
---

# PRレビューコメント完全対応ワークフロー

PRのレビューコメントを取得し、修正、CIチェック、コミット、サマリー投稿までを一括で行う統合スキル。

## 実行フロー

### 1. レビューコメントの抽出と評価

まず未解決のレビューコメントを抽出し、各コメントを評価して対応方針を決定する。

```bash
# 未解決のレビューコメントを取得
python3 .claude/skills/pr-review-extraction/extract_reviews.py <PR_NUMBER> --full
```

各コメントについて妥当性を評価し、対応方針を決める

| 分類 | 説明 | アクション |
|------|------|-----------|
| **対応する** | 指摘が妥当で修正が必要 | コードを修正 |
| **対応しない** | 誤検知、または意図的な実装 | サマリーで説明 |


### 2. コードの修正

妥当と判断した項目について、コードを修正する。

### 3. CIチェック

修正した言語に応じて適切なCIチェックを実行する。

#### Kotlinを修正した場合

```bash
cd frontend

# フォーマット（先に単独実行）
./gradlew ktlintFormat

# Lintチェック
./gradlew ktlintCheck

# 静的解析
./gradlew detekt

# 単体テスト
./gradlew :shared:testDebugUnitTest

# ビルド
./gradlew build
```

#### Pythonを修正した場合

```bash
# 該当モジュールのテストを実行
uv run pytest <module>/tests

# Lint & Format
uv run ruff check .
uv run ruff format .
```

**重要**: CIチェックが失敗した場合は、失敗を解消してから次に進む。

### 4. コミット・プッシュ

変更がある場合のみ、コミットしてプッシュする。

### 5. サマリーコメントの投稿

最後に、PR全体に対してサマリーコメントを投稿する。

#### サマリーフォーマット

```markdown
<!-- review-back:done -->

## レビュー対応完了

対応したレビューコメント: <N>件

| 対象ファイル | 修正内容 | ステータス |
| --- | --- | --- |
| `path/to/file.py` | (修正内容とその理由) | ✅ 修正済み |
| `path/to/other.ts` | (対応なし) | ℹ️ 誤検知のため無視 |
```
