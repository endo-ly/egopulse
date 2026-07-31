---
name: check-ai-provider-usage
description: "CodexBar CLIでz.ai、OpenCode Go、Codexの利用量・残り枠・リセット時刻を確認する。quota、rate limit、usage、reset time、リミット開放時刻、利用枠の見方を尋ねられたときに使用する。"
---

# AIプロバイダ利用量確認

CodexBar CLIで z.ai / OpenCode Go / Codex の利用量と次回リセット時刻を確認する。

## 共通の準備

`codexbar` が入っているか確認する。

```bash
codexbar --version
```

未導入の場合は環境に応じて導入する。

```bash
# macOS: アプリとCLI
brew install --cask codexbar

# Linux: CLI
brew install steipete/tap/codexbar
```

Homebrewを使えない場合は、[GitHub Releases](https://github.com/steipete/CodexBar/releases) から環境に合う `CodexBarCLI` のリリース tarballを取得する。tarballを展開すると `CodexBarCLI` バイナリが入るので、`codexbar` としてPATHの通ったディレクトリに配置する。

```bash
# 例: Linux x86_64
tar -xzf CodexBarCLI-v*-linux-x86_64.tar.gz
install -m 0755 CodexBarCLI ~/.local/bin/codexbar
codexbar --version
```

リリースに付属する `.sha256` ファイルでハッシュを検証できる。ファイル内のパスはリリース環境のものが埋め込まれていることがあるため、ハッシュ値を取り出してローカルファイルと比較する。

## 出力形式

```bash
# 人間向け表示
codexbar usage --provider <provider-id> --format text

# スクリプトで扱いやすいJSON
codexbar usage --provider <provider-id> --format json --pretty
```

JSONでは主に次を確認する。

- `provider`: 対象プロバイダ
- `source`: 実際に使用された取得元
- `usage.*.resetsAt`: 次回リセット時刻
- `usage.*.usedPercent`: 使用率
- `updatedAt`: データ取得時刻

プロバイダや契約プランがリセット時刻を公開していない場合、`resetsAt` がないことがある。その場合は推測で時刻を補わない。

## z.ai

CodexBarのプロバイダIDは `zai`。z.aiはAPI tokenでquota APIを取得する。

APIキーは現在のシェル環境の `Z_AI_API_KEY` を使う。キーの実値を回答・ログ・コマンド履歴へ出さない。

```bash
codexbar usage --provider zai --source api --format json --pretty
```

キーをcodexbarに保存する場合は、コマンドライン引数に直接書かずstdinを使う。

```bash
printf '%s' "$Z_AI_API_KEY" \
  | codexbar config set-api-key --provider zai --stdin
```

リージョンは次の2種類。

- Global: `api.z.ai`
- BigModel CN: `open.bigmodel.cn`

BigModel CNを使う場合は、API hostを環境変数で指定する。

```bash
Z_AI_API_HOST=open.bigmodel.cn \
  codexbar usage --provider zai --source api --format json --pretty
```

出力のTokens/MCP quotaと `resetsAt` を確認する。z.aiのteam usageでは、Organization IDとProject IDを含むtoken account設定が別途必要になるため、個人用とteam用を混同しない。

## OpenCode Go

CodexBarのプロバイダIDは `opencodego`。まず自動取得を使う。

```bash
codexbar usage --provider opencodego --source auto --format text
codexbar usage --provider opencodego --source auto --format json --pretty
```

`auto` はOpenCode Webの使用量取得を試し、利用できない場合はローカルのOpenCode履歴へフォールバックする。ローカルSQLiteの既定パスは次の通り。

```text
~/.local/share/opencode/opencode.db
```

Workspaceを明示する必要がある場合は、`CODEXBAR_OPENCODE_WORKSPACE_ID` を設定する。実際のWorkspace IDは回答やログに出さない。

```bash
CODEXBAR_OPENCODE_WORKSPACE_ID="$OPENCODE_WORKSPACE_ID" \
  codexbar usage --provider opencodego --source auto --format json --pretty
```

出力ではrolling 5-hour、weekly、利用可能ならmonthlyのusage windowと、それぞれのリセット時刻を確認する。Linuxではブラウザ自動取得に制限があるため、ローカルSQLiteまたはCodexBarに設定した手動Cookieが必要になる場合がある。

## Codex

CodexBarのプロバイダIDは `codex`。

取得元は環境と認証状態に応じて選ぶ。Linuxなどブラウザcookieが使えない環境では `--source auto` がweb取得で失敗したり不安定になることがあるため、Codex CLIがログイン済みなら `--source cli` を使うのが安定する。まずはCLIの有無とログイン状態を確認する。

```bash
codex --version
```

```bash
# 推奨: Codex CLI経由（codex CLIのログインが必要）
codexbar usage --provider codex --source cli --format json --pretty

# 自動取得（web → cli のフォールバック）
codexbar usage --provider codex --source auto --format text
```

取得元を固定したい場合は、認証状態に応じて選ぶ。

```bash
# OAuth資格情報を使う
codexbar usage --provider codex --source oauth --format json --pretty

# OpenAI Webダッシュボードを使う
codexbar usage --provider codex --source web --format json --pretty
```

JSONでは、通常次を確認する。

- `usage.primary.resetsAt`: 短いセッション枠のリセット時刻
- `usage.secondary.resetsAt`: 週間などの長い枠のリセット時刻
- `usage.primary.usedPercent` / `usage.secondary.usedPercent`: 各枠の使用率
- `credits`: 利用可能なクレジットが返る場合の残高
- `source`: 実際に採用された取得元

複数アカウントを確認する場合は、表示対象アカウントをまとめて取得できる。

```bash
codexbar usage --provider codex --all-accounts --format json --pretty
```

OpenAI Web取得ではログイン済みのブラウザセッション、OAuth取得では有効なCodex OAuth資格情報、CLI取得ではCodex CLIのログイン状態が必要になる場合がある。パスワードやtokenをSkillに入力させない。

## セキュリティ

- APIキー、OAuth token、Cookie、Workspace ID、アカウントemailを実データとして回答へ貼り付けない。
- `.env`、EgoPulse設定、CodexBar設定、ブラウザCookieを読まない。
- APIキーをコマンドライン引数へ直接書く方法を案内しない。`codexbar config set-api-key --stdin` を使う。
- `resetsAt` が取得できない場合に、契約仕様から推測した時刻を提示しない。

詳しい仕様は、[CodexBar CLI](https://github.com/steipete/CodexBar/blob/main/docs/cli.md)、[z.ai provider](https://github.com/steipete/CodexBar/blob/main/docs/zai.md)、[OpenCode provider](https://github.com/steipete/CodexBar/blob/main/docs/opencode.md) を参照する。
