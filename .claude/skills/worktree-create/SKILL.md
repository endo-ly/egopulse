Git worktree を作成してください。

## 実行手順

1. `$ARGUMENTS` から `topic-kebab-case` を作る。
2. ブランチ名を決める（形式: `<type>/<topic-kebab-case>`。`type` は `feat|fix|refactor|docs|chore` から内容に合わせる）。
3. worktree名を決める（形式: `wt-<topic-kebab-case>`。重複時は `-2`, `-3` を付ける）。
4. `origin/main` を最新化する。

```bash
git fetch origin main
```

5. 次を実行して、リポジトリ直下に worktree を作成する。

```bash
git worktree add "./<worktree_name>" -b "<branch_name>" origin/main
```

## 出力フォーマット

- 作成完了: `<worktree_path>`
- ブランチ: `<branch_name>`
- 開始コマンド: `cd <worktree_path>`
