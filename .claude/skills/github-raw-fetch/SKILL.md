---
name: "github-raw-fetch"
description: "OSSのからrawファイルを直接取得。Claude Codeの最新CHANGELOG確認など、OSSの最新情報や仕様を手軽に取得が可能"
allowed-tools: "Bash"
---

# GitHub Raw Fetch

GitHub リポジトリからファイルを直接取得する。clone 不要で素早く中身を確認できる。

## 基本形

```bash
curl -s "https://raw.githubusercontent.com/<org>/<repo>/<branch>/<path>"
```

| パラメータ | 説明 |
|-----------|------|
| `<org>` | 組織・ユーザー名 |
| `<repo>` | リポジトリ名 |
| `<branch>` | ブランチ名（通常 `main` か `master`） |
| `<path>` | ファイルパス |

## よく使う例

### CHANGELOG の確認（先頭50行）
```bash
curl -s "https://raw.githubusercontent.com/<org>/<repo>/main/CHANGELOG.md" | head -50
```

### Claude Code の最新 CHANGELOG
```bash
curl -s "https://raw.githubusercontent.com/anthropics/claude-code/main/CHANGELOG.md" | head -100
```

### README の取得
```bash
curl -s "https://raw.githubusercontent.com/<org>/<repo>/main/README.md"
```

### package.json からバージョン取得
```bash
curl -s "https://raw.githubusercontent.com/<org>/<repo>/main/package.json" | jq -r '.version'
```

### ファイル内キーワード検索
```bash
curl -s "https://raw.githubusercontent.com/<org>/<repo>/main/README.md" | grep -i "keyword"
```
