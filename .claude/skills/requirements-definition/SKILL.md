---
name: "requirements-definition"
description: "A skill to break down ambiguous requests into small, immediately implementable requirement definitions through strategic questioning. Focus on WHY and WHAT, excluding HOW. This is used when ambiguous requests such as 'I want to add a feature like XX,' 'I want to fix XX,' or 'I did XX' are made outside of Plan Mode."
allowed-tools: "AskUserQuestion, Bash, Read, Write"
---

# 要件定義 / Requirements Definition

曖昧な要望を、実装可能な粒度の要件定義ドキュメントに変換するスキル。
プロフェッショナルコンサルタントとして振舞い、戦略的な質問を通じてユーザーの真の要望を引き出す。
質問→要約→合意→ドキュメント化までをループで回す。

---

## Workflow

### 1. 初期要望と温度感の把握

ユーザーの要望を受け取り、まず短く要約して認識を揃える。

- 「つまり○○したい。理由は△△。対象は□□。優先は××。合ってる？」の形式で1回だけ確認
- ここで論点がズレていれば修正してから次へ進む

#### ここで使う軽量フレーム：SCQA-lite
- **S**: 今どうなってる？
- **C**: 何が困ってる／物足りない？
- **Q**: じゃあ何を決める必要がある？
- **A**: まずこういう方向で要件にする（仮

また、ユーザーの雰囲気や予想される作業の複雑度から**温度感**を掴み、要件定義の温度感を調整する。

### 2. 質問による明確化（ループ）
曖昧さが排除されるまで、以下の観点で質問を繰り返します。わかりきっている目的などは質問不要。

- **目的 (WHY)**: 何が改善されれば成功か？
- **仕様 (WHAT)**: ユーザーが何をできるようになるか？
- **範囲**: どこまでやるか／やらないか？
- **制約**: 技術的・時間的・外部依存の制約は？
- **成功基準**: どうなれば完了か？

質問のコツ：**クローズドクエスチョンとオープンクエスチョンを使い分ける。**
クローズドクエスチョンは、**AskUserQuestion tool**を使う。使えない場合、質問ごとに選択肢を提示する。
オープンクエスチョンは、チャットで回答を待つ

ユーザーが回答しやすくするため、A,Bなどの選択肢を提示することが望ましい。
---

### 3. User Story Mapping
画面一覧より、ユーザーの行動の流れで要件を並べる。

- 例：`開く → 入力 → 確認 → 保存 → 後で見る/編集`
- 各ステップに「最低限MVP」「あったら嬉しい」を置く

> 目的：MVPラインを引いてスコープ膨張を止める

---

### 4. 受入条件（Acceptance Criteria）を作る
完了判定できる形にする。

Gherkin形式（Given/When/Then）を推奨。

例：
- Given 未ログイン, When 保存を押す, Then ログインを促す
- Given 入力が不正, When 送信, Then エラー表示し送信しない

---

### 5. 非機能・制約を最小限チェック（FURPS-lite）
関係がありそうなものだけ拾う。

- **Performance**：遅いと困る？
- **Reliability**：失敗時どうする？
- **Usability**：迷いやすい？
- **Security/Privacy**：扱うデータは？
- **Constraints**：技術/期限/外部APIなど

---

### 6. リスクと前提（RAID-lite）
要件のブレ戻りを防ぐため、短く残す。

- Risk: 失敗しそうな点
- Assumption: 前提
- Issue: 既知の問題
- Dependency: 依存

---

### 7. 要件のまとめと最終確認
収集した情報を構造化し、ユーザーに最終確認する。
指摘があれば反映し、再度まとめを提示する。

---

### 8. Markdown ドキュメント生成
`docs/00.project/requirements/[機能名].md` に生成する。
フォーマットは `.github/ISSUE_TEMPLATE/requirements.md` を使用する。

---

### 9. Issue 化の確認
ユーザーに GitHub Issue として作成するか確認する。

- Yes: `create_issue.py` または `gh` CLI で Issue 作成
- No: ドキュメントのみで完了

## Scripts

### create_issue.py

GitHub Issue を作成し、ラベルを設定します。

```bash
# 基本
python3 .claude/skills/requirements-definition/scripts/create_issue.py \
  --title "[REQ] 機能名" --file requirements.md \
  --category feature --component frontend --component backend

# 対話形式
python3 .claude/skills/requirements-definition/scripts/create_issue.py --interactive

# または gh CLI
gh issue create --title "[REQ] 機能名" --body "$(cat requirements.md)" \
  --label requirements --label feature --label frontend
```

ラベル例:
CATEGORY_EXAMPLES = ["feature", "fix"]
COMPONENT_EXAMPLES = ["backend", "frontend", "ingest"]

## Best Practices

### プロフェッショナルコンサルタントとして

- **ユーザーの真の要望を引き出す**: 表面的な要望の裏にある本質的な課題を特定
- **選択肢を提示**: 複数のアプローチがある場合、pros/cons を明示
- **ベストプラクティス提案**: 業界標準やプロジェクト慣習に基づく推奨
- **曖昧さを許さない**: 解釈が複数ある場合は必ず確認

### 効率的な質問

- **優先順位をつける**: 重要な質問から順に
- **まとめて質問**: 関連する質問は1度に
- **具体例を使う**: 抽象的な質問は具体例で補足

### ドキュメント品質

- **端的で洗練された日本語**: 冗長な表現を避ける
- **HOW を排除**: 実装方法は書かない（コード例など）
- **本質的な情報のみ**: WHY と WHAT に集中

### Question Packs

#### Pack A: WHY（JTBD-lite）
- それができると何が嬉しい？
- いまの代替手段は？（手作業/別アプリ/我慢）
- “いつ・どんな状況で”困る？（頻度/痛み）
- 成功したら何が変わる？（時間/ミス/ストレス）

#### Pack B: スコープ切り（MoSCoW-lite）
- Must：今回ないと成立しないものは？
- Should：入ると嬉しいが次回でもOKは？
- Could：そのうち、でも良いものは？
- Won’t：今回はやらない（明記してOK）

#### Pack C: 例外・境界
- 失敗時（通信/保存/権限）はどう見せる？
- 空状態（データ0）でどうする？
- 上限（文字数/件数/サイズ）はある？
- 既存データとの整合は？（移行/互換）

## Pitfalls

### ❌ HOW を含めてしまう

悪い例:
「React の useState を使って実装する」

良い例:
「ユーザー入力をリアルタイムで検証し、エラーを表示する」

### ❌ 曖昧なまま進める

悪い例:
「エラーハンドリングを改善する」

良い例:
「APIタイムアウト時に自動リトライし、3回失敗したらエラーメッセージを表示する」

### ❌ 抽象的すぎる質問

悪い例:
「どうしたいですか？」

良い例:
「この機能で解決したい具体的な課題は何ですか？今の運用で困っている点はありますか？」

### ❌ 出力が冗長

悪い例:
長い説明、重複、実装手順

良い例:
端的に、WHY/WHAT、スコープ、受入条件

---

関連リソース:

- `.github/ISSUE_TEMPLATE/requirements.md`
- `docs/00.project/requirements/`

