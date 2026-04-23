複数のCLIコードレビューツールを並列実行し、統合レビュー結果を基に対応を行うワークフロー。

## 使用方法

```bash
/review-multi-cli
```

## ワークフロー

### Phase 1: パラメータ収集

`AskUserQuestion`で以下を確認：

**質問1: 使用するCLIツール**（複数選択可）
- CodeRabbit
- claude code (code-reviewer SubAgent)
- Codex
- Gemini

**質問2: レビュー対象**
- mainブランチとの差分
- uncommittedの差分

### Phase 2: 並列レビュー実行

選択されたツールごとに`other-cli-reviewer`サブエージェントを起動。

複数のサブエージェントを並列で起動する場合、**1つのメッセージで複数のTask tool callを送る**。

ただし、claude code (code-reviewer SubAgent)は、`code-reviewer`サブエージェントを呼び出す。

**例（CodeRabbitとGeminiを使用する場合）**:
```
1つのメッセージで以下の2つのTask tool callを送る：

Task 1:
- subagent_type: "other-cli-reviewer"
- description: "CodeRabbitレビュー実行"
- prompt: "
  以下のパラメータでコードレビューを実行してください：
  - 使用ツール: coderabbit
  - レビュー対象: main
  "

Task 2:
- subagent_type: "other-cli-reviewer"
- description: "Geminiレビュー実行"
- prompt: "
  以下のパラメータでコードレビューを実行してください：
  - 使用ツール: gemini
  - レビュー対象: main
  "
```

**例2（CodeRabbitとClaude Codeを使用する場合）**:
```
1つのメッセージで以下の2つのTask tool callを送る：

Task 1:
- subagent_type: "other-cli-reviewer"
- description: "CodeRabbitレビュー実行"
- prompt: "
  以下のパラメータでコードレビューを実行してください：
  - 使用ツール: coderabbit
  - レビュー対象: main
  "

-> レビューコマンドを実施するサブエージェントを実行するイメージ

Task 2: "code-reviewer"サブエージェントを呼び出し

-> サブエージェント自身のレビューさせるイメージ

```

### Phase 3: レビュー結果の統合

各サブエージェントから返される結果を統合：

1. **重複指摘の除去**: 複数ツールが同じ指摘をする場合、1つにまとめる
2. **優先度の分類**: MUST/SHOULD/IMO/NITSに分類
3. **対応方針の提案**:
   - 即座対応（MUST）: セキュリティ、バグ
   - 検討対応（SHOULD）: パフォーマンス、可読性
   - 任意対応（IMO/NITS）: スタイル、コメント

### Phase 4: ユーザーへの提示

統合結果をユーザーに提示：

```markdown
# レビュー結果サマリー

## 実行情報
- レビュー対象: [main/uncommitted]
- 使用ツール: [coderabbit, codex, gemini]

## 統合レビュー結果

### MUST（必須対応）: X件
1. [指摘内容]
2. ...

### SHOULD（推奨対応）: Y件
1. [指摘内容]
2. ...

### IMO/NITS（任意対応）: Z件
（一覧表示）

## ツール別詳細

### CodeRabbit
[出力]

### Codex
[出力]

### Gemini
[出力]

---

次のステップを選択してください：
1. このまま対応を開始する
2. 対応方針を調整する
3. レビューのみで終了する
```

### Phase 5: ユーザー承認と修正実行

ユーザーの選択に応じて：

**1. このまま対応を開始する**:
- `TodoWrite`で対応項目をタスク化
- MUST → SHOULD → IMO/NITSの順に修正

**2. 対応方針を調整する**:
- `AskUserQuestion`で詳細確認

**3. レビューのみで終了する**:
- ワークフロー終了
