# EgoPulse Built-in Tools

全 built-in tools の入力・挙動・エラーを記述したリファレンス。

## 目次

1. [前提](#1-前提)
2. [Tool Registry](#2-tool-registry)
3. [read](#3-read)
4. [write](#4-write)
5. [edit](#5-edit)
6. [bash](#6-bash)
7. [grep](#7-grep)
8. [find](#8-find)
9. [ls](#9-ls)
10. [activate_skill](#10-activate_skill)
11. [send_message](#11-send_message)
12. [agent_send](#12-agent_send)
13. [Skill Catalog](#13-skill-catalog)
14. [web_fetch](#14-web_fetch)
15. [セキュリティガード](#15-セキュリティガード)

---

## 1. 前提

- 実装本体: [egopulse/src/tools.rs](../../egopulse/src/tools.rs)
- workspace ルート: `~/.egopulse/workspace`
- skills ルート: `~/.egopulse/workspace/skills`
- path 解決は workspace 配下に制限される
- tool 実行結果は turn loop で `{"tool":"...","status":"success|error","result":"...","details":{...}}` の JSON に包まれて LLM に返る
- `details` は tool によって `truncation`、`diff`、`firstChangedLine`、`fullOutputPath` などを含む
- マルチモーダル画像対応: `read` tool で画像ファイルを検出した場合、base64 data URL として LLM に直接渡す。マルチモーダルメッセージが含まれる場合は OpenAI Responses API (`/responses`) に自動ルーティングされる（Chat Completions API はマルチモーダル tool result に非対応のため）。セッション永続化時は画像を SHA256 ハッシュで内容重複排除し、参照形式 (`input_image_ref`) で保存する

## 2. Tool Registry

`ToolRegistry` は全 tool を `Box<dyn Tool>` として一元管理する。built-in / MCP の区別なく、統一的に定義列挙・実行 dispatch を行う。

### Built-in tool

registry に登録される tool は次の通り。`agent_send` は常に登録される。

#### 常時登録（10個）

- `read`
- `bash`
- `edit`
- `write`
- `grep`
- `find`
- `ls`
- `activate_skill`
- `send_message`
- `web_fetch`

#### 条件付き登録

- `agent_send` — 常時有効

登録箇所: [egopulse/src/tools/mod.rs](../../egopulse/src/tools/mod.rs)

### MCP tool (Adapter 経由)

MCP が有効な場合、`McpManager.create_tool_adapters()` が各 MCP tool を `McpToolAdapter` (Tool trait 実装) として生成し、`ToolRegistry.register_tool()` で登録する。Registry は MCP の存在を意識しない。

命名規則:

- 英数字と `_` 以外の文字は `_` に置換される
- 合計文字数が 64 文字を超える場合は `mcp_{sha256先頭8文字}` に短縮される
- サニタイズ後の名前が衝突する場合は後続の tool がスキップされる

例:

- `mcp_filesystem_read_file` — 標準的な命名
- `mcp_db_query_1_` — `query(1)` の `(` `)` が `_` に置換される
- `mcp_a1b2c3d4` — server/tool 名の合計が 64 文字を超える場合のハッシュ短縮

実装: [egopulse/src/tools/mcp_adapter.rs](../../egopulse/src/tools/mcp_adapter.rs)

MCP の詳細は以下を参照。

- [mcp.md](./mcp.md)

### Tool 実行台帳

既存 `tool_calls` テーブルを Tool 実行台帳として拡張する（[db.md §tool_calls](./db.md#tool_calls)）。新規台帳テーブルは作らない。`Database`（tool.rs）が claim・状態遷移・input 整合確認・結果再利用・uncertain 判定を担う。

#### 実行フロー

1. **claim**: Tool 実行前に `claim(turn_id, tool_call_id, canonical_input)` を呼ぶ。`turn_id + tool_call_id` で既存行を検索し、未登録なら `pending` 作成、`running` へ遷移する。canonical input の hash (`input_hash`) を保存し、既存行と hash が異なれば conflict として拒否する。
2. **実行**: claim 成功後にのみ Tool を実行する。実行前に台帳行を作ることを保証する。
3. **結果保存**: 成功時は `succeeded` + sanitized `tool_output` + `finished_at` を同一 transaction で保存する。失敗時は `failed` + `error_kind` + sanitized `error_message` を保存する。

#### idempotency 分類

Tool 定義に次のいずれかを明示する。未指定は `non_idempotent`。分類は claim 時に台帳へ記録され、同一 `(turn_id, tool_call_id)` で分類を変更する claim を拒否する。

| 分類 | 意味 |
|---|---|
| `read_only` | 外部状態を変更しない |
| `idempotent` | 同じ idempotency key で重複排除可能 |
| `non_idempotent` | 再実行で副作用が重複する可能性 |

#### 状態と再実行規則

| Tool 状態 | 実行規則 |
|---|---|
| `pending` | claim 後に実行可能 |
| `running` | 通常実行中。recovery 時は結果不明として `uncertain` へ移行 |
| `succeeded` | 保存済み `tool_output` を返し、再実行しない |
| `failed` | 自動再実行しない |
| `uncertain` | 自動再実行しない |

#### crash recovery

起動時に `recover_running_tools()` が `running` の Tool を idempotency 分類に関わらずすべて `uncertain` へ移行する。Turn は起動時に fail-stop するため再開されず、`running` の Tool が再 claim されることはない。`pending`（実行開始前）の行はそのまま残す。

Tool 成功後に LLM が失敗しても、同一 Turn 内で Tool を再実行しない（`succeeded` の結果を再利用する）。

## 3. `read`

- 目的: ファイル内容を読む
- 入力:
  - `path: string` 必須
  - `offset: integer` 任意。1-indexed
  - `limit: integer` 任意。最大行数
- 挙動:
  - workspace 配下の path のみ読める
  - テキストに加えて `png` / `jpg` / `jpeg` / `gif` / `webp` を画像として判定する
  - 画像ファイルは base64 data URL にエンコードし、`MessageContent::Parts` (InputText + InputImage) として LLM に渡す
  - マルチモーダル tool result を含むメッセージ履歴は OpenAI Responses API に自動ルーティングされる
  - テキスト出力は最大 `2000` 行または `50KB`
  - 続きがある場合は `offset=...` の continuation hint を返す
  - 先頭 1 行だけで `50KB` を超える場合は `bash` fallback を促す
- `details`:
  - `truncation`
- 主な失敗:
  - `Missing required parameter: path`
  - `File not found: ...`
  - `Offset ... is beyond end of file`
  - `Failed to read file: file is not valid UTF-8 text or a supported image.`

実装: [egopulse/src/tools.rs](../../egopulse/src/tools.rs)

## 4. `write`

- 目的: ファイルを新規作成または上書きする
- 入力:
  - `path: string` 必須
  - `content: string` 必須
- 挙動:
  - workspace 配下の path のみ書ける
  - 親ディレクトリは自動作成
  - 既存ファイルは上書き
- 成功時:
  - `Successfully wrote <bytes> bytes to <path>`
- 主な失敗:
  - `Missing required parameter: path`
  - `Missing required parameter: content`
  - `Failed to create directories: ...`
  - `Failed to write file: ...`

実装: [egopulse/src/tools.rs](../../egopulse/src/tools.rs)

## 5. `edit`

- 目的: 既存ファイルの exact text replacement
- 入力:
  - `path: string` 必須
  - `edits: array` 必須
  - 各 edit は:
    - `oldText: string`
    - `newText: string`
- 挙動:
  - すべて original file に対してマッチする
  - `oldText` は各 edit ごとに 1 回だけ一致する必要がある
  - overlapping edit は拒否
  - BOM と元の改行コードを保存する
  - exact match で見つからない場合は、末尾空白や smart quotes/dashes/special spaces をある程度正規化した fuzzy match を試みる
- `details`:
  - `diff`
  - `firstChangedLine`
- 成功時:
  - `Successfully edited <path> with <N> replacement(s).`
- 主な失敗:
  - `Missing required parameter: path`
  - `Edit tool input is invalid. edits must contain at least one replacement.`
  - `Each edit must include oldText`
  - `Each edit must include newText`
  - `File not found: ...`
  - `oldText must not be empty in <path>.`
  - `Could not find the exact text in <path>. The old text must match exactly including all whitespace and newlines.`
  - `Found N occurrences of the text in <path>. The text must be unique. Please provide more context to make it unique.`
  - `edits[i] and edits[j] overlap in <path>. Merge them into one edit or target disjoint regions.`
  - `No changes made to <path>. ...`

実装: [egopulse/src/tools.rs](../../egopulse/src/tools.rs)

## 6. `bash`

- 目的: workspace を cwd にして bash command を実行する
- 入力:
  - `command: string` 必須
  - `timeout: integer` 任意。秒
- 挙動:
  - workspace を cwd にして `bash -lc` で実行する
  - stdout / stderr は 1 つのログに結合する
  - 出力は末尾側を最大 `2000` 行または `50KB` に tail truncation する
  - truncation が発生した場合は full output を temp file に保存する
  - 最後の 1 行だけで byte limit を超える場合は、その行の末尾だけを返す special case がある
- `details`:
  - `truncation`
  - `fullOutputPath`
- 終了コードが非 0 の場合は error 扱い
- 成功時:
  - command output
  - output が空なら `(no output)`
- 主な失敗:
  - `Missing required parameter: command`
  - `Failed to execute bash command: ...`
  - `Command timed out after N seconds`
  - `Command exited with code N`

実装: [egopulse/src/tools.rs](../../egopulse/src/tools.rs)

## 7. `grep`

- 目的: file contents を検索する
- 入力:
  - `pattern: string` 必須
  - `path: string` 任意。既定値 `.`
  - `glob: string` 任意
  - `ignoreCase: boolean` 任意
  - `literal: boolean` 任意
  - `context: integer` 任意
  - `limit: integer` 任意。既定値 `100`
- 挙動:
  - `rg` を使用
  - workspace 配下の path のみ検索
  - `limit` は最低でも `1`
  - 1 行は最大 `500` 文字に短縮
  - 結果全体は head 側を `50KB` で truncation
  - マッチ 0 件は success 扱いで `No matches found`
  - result limit や line truncation が起きた場合は notice を追記する
- `details`:
  - `truncation`
  - `matchLimitReached`
  - `linesTruncated`
- 主な失敗:
  - `Missing required parameter: pattern`
  - `Path not found: ...`
  - `ripgrep (rg) is not available and could not be downloaded`
  - `Failed to run ripgrep: ...`
  - `ripgrep exited with code N`

実装: [egopulse/src/tools.rs](../../egopulse/src/tools.rs)

## 8. `find`

- 目的: glob pattern でファイルを探す
- 入力:
  - `pattern: string` 必須
  - `path: string` 任意。既定値 `.`
  - `limit: integer` 任意。既定値 `1000`
- 挙動:
  - `fd` を使用
  - workspace 配下の path のみ検索
  - 検索ルート配下の nested `.gitignore` も `--ignore-file` で明示的に渡す
  - 結果パスは検索ルート相対に正規化し、`/` 区切りに揃える
  - 結果が空なら `No files found matching pattern`
  - 結果全体は head 側を `50KB` で truncation
  - result limit に達した場合は `Use limit=... for more, or refine pattern` を追記する
- `details`:
  - `truncation`
  - `resultLimitReached`
- 主な失敗:
  - `Missing required parameter: pattern`
  - `Path not found: ...`
  - `fd is not available and could not be downloaded`
  - `Failed to run fd: ...`
  - `fd exited with code N`

実装: [egopulse/src/tools.rs](../../egopulse/src/tools.rs)

## 9. `ls`

- 目的: directory contents を一覧する
- 入力:
  - `path: string` 任意。既定値 `.`
  - `limit: integer` 任意。既定値 `500`
- 挙動:
  - workspace 配下の path のみ一覧
  - ディレクトリは `/` suffix を付ける
  - dotfiles を含む
  - case-insensitive に sort する
  - 空 directory は `(empty directory)`
  - 結果全体は head 側を `50KB` で truncation
  - entry limit に達した場合は `Use limit=... for more` を追記する
- `details`:
  - `truncation`
  - `entryLimitReached`
- 主な失敗:
  - `Path not found: ...`
  - `Not a directory: ...`
  - `Cannot read directory: ...`

実装: [egopulse/src/tools.rs](../../egopulse/src/tools.rs)

## 10. `activate_skill`

- 目的: 発見済み skill の本文をロードし、環境変数の可用性を報告する
- 入力:
  - `skill_name: string` 必須
- 挙動:
  - `SkillManager::load_skill_checked()` を呼ぶ
  - 返り値には skill name、description、skill directory、instructions 本文を含む
  - SKILL.md の `required_env` に基づき環境変数の解決可否を確認し、結果（✓/✗）を返す。環境変数自体は `bash` 実行時に自動注入されるため、`activate_skill` の呼び出しは注入の前提条件ではない
- 主な失敗:
  - `Missing required parameter: skill_name`
  - `Skill '<name>' not found. ...`
  - `failed to read skill '<name>': ...`

### Skill 環境変数 (`required_env`)

SKILL.md frontmatter に `required_env` を宣言すると、`activate_skill` でキーの可用性を確認でき、`bash` 実行時に自動注入される。

**SKILL.md 記述例:**

```yaml
---
name: my-skill
description: Example skill
required_env:
  - API_KEY
  - API_SECRET
---
```

スカラー形式でも可:

```yaml
required_env: API_KEY
```

**解決順序:** プロセス環境変数 → `~/.egopulse/.env`（dotenv）

**自動注入（Auto-hydration）:**

インストール済み skill の `required_env` 宣言の和集合を allowlist として扱い、`bash` ツールのサブプロセス実行時に allowlist 内のキーのみを解決・注入する。`activate_skill` を呼ばなくても、required_env に宣言された環境変数は bash サブプロセスで利用可能。Allowlist は実行ごとに SkillManager から動的に計算されるため、ランタイム中に追加された skill も即座に反映される。

**スコープとセキュリティ:**

| 特性 | 内容 |
|---|---|
| スコープ | 実行ごと。bash サブプロセス起動時に解決し、終了後に破棄。値はセッション・グローバル状態に一切保存しない |
| 注入先 | `bash` ツールのサブプロセスのみ（`grep` / `find` / `ls` / MCP には注入しない） |
| Allowlist | インストール済み全 skill の `required_env` の和集合のみ。dotenv 全体は注入しない |
| 秘密値の取り扱い | 解決済みの値は LLM 出力・ログ・ツール結果に一切含まれない。注入された値と redaction 対象は完全に一致する |
| 未解決キー | `tracing::warn!` で警告し、activate_skill の戻り値にキー名（✗ マーク）を表示 |

**返り値の環境変数表示例:**

```
Environment variables:
  ✓ API_KEY
  ✗ API_SECRET (not found in env or dotenv)
```

実装: [egopulse/src/tools.rs](../../egopulse/src/tools.rs) · [egopulse/src/skills.rs](../../egopulse/src/skills.rs)

## 11. `send_message`

- 目的: テキストメッセージまたはファイル添付を明示的にチャネルへ送信する
- 入力:
  - `text: string` 任意。送信するメッセージ本文（`attachment_path` 未指定時は必須）
  - `attachment_path: string` 任意。添付ファイルのローカルパス
  - `caption: string` 任意。添付ファイルのキャプション（`attachment_path` 指定時のみ使用）
- 挙動:
  - 通常のテキスト応答はランタイムが自動送信するため、このツールはファイル添付が必要な場合に使用する
  - `attachment_path` がある場合: パスを解決し、ファイル存在確認後、channel adapter 経由で添付送信
  - `attachment_path` がない場合: `text` を channel adapter 経由で送信
  - `text` も `attachment_path` も空の場合はエラー
- 成功時:
  - `"Message sent successfully"`
- 主な失敗:
  - `"At least one of 'text' or 'attachment_path' must be provided"`
  - `"no chat found for chat_id <id>"`
  - `"no adapter for channel '<name>'"`
  - `"File not found: <path>"`
  - `"Failed to send message: <reason>"`

実装: [egopulse/src/tools/send_message.rs](../../egopulse/src/tools/send_message.rs)

## 12. `agent_send`

- 目的: 同一チャネル内の別エージェントにメッセージを送信し、宛先エージェントの Turn を非同期にキューイングする
- 入力:
  - `to: string` 必須。宛先エージェント ID（`config.agents` に存在するエージェントのみ指定可能）
  - `message: string` 必須。送信するメッセージ内容
- 挙動:
  - 全チャネルで利用可能
  - 自己送信 (`to == 自分自身`) は禁止
  - メッセージを Channel Log に `MessageKind::AgentSend` で保存
  - チャネルに `[From → To] message` 形式で表示
  - 宛先エージェントの Turn を `PendingAgentTurn` として `TurnScheduler` 経由でバックグラウンド実行
  - チェーン深度 (chain depth) が `MAX_AGENT_CHAIN_DEPTH` (4) を超えるターンは Channel Log に SystemEvent を記録して破棄
  - 同一 origin のターン数が `MAX_AGENT_TURNS_PER_INPUT` (12) に達すると SystemEvent を記録して停止
- 成功時:
  - `{"delivered": true, "to": "<agent_id>"}`
- 主な失敗:
  - `"agent '<id>' not found"` — 存在しないエージェント ID
  - `"cannot send a message to yourself"` — 自己送信

実装: [egopulse/src/tools/agent_send.rs](../../egopulse/src/tools/agent_send.rs)

## 13. Skill Catalog

`activate_skill` とは別に、各 turn の system prompt には skill の概要一覧が入る。

- catalog 生成: [egopulse/src/skills.rs](../../egopulse/src/skills.rs) `build_skills_catalog()`
- prompt への埋め込み: [egopulse/src/agent_loop/turn.rs](../../egopulse/src/agent_loop/turn.rs)

つまり skill 本文は初期ロードされず、最初に入るのは概要一覧だけ。

## 14. `web_fetch`

- 目的: URL からコンテンツを取得し、HTML を Markdown に変換して返す
- 入力:
  - `url: string` 必須
  - `timeout_secs: integer` 任意。既定値は設定ファイル参照
  - `max_output_bytes: integer` 任意。本文の最大バイト数（warning は上限外、デフォルト 64KB）
- 挙動:
  - URL scheme 検証（デフォルト HTTPS のみ許可）
  - Host denylist/allowlist チェック
  - SSRF 対策: プライベート IP / ループバックアドレスへのアクセスをブロック（`allow_private_ips: true` で解除可）
  - DNS 解決後の SSRF 再検証
  - HTTP リダイレクトは手動追跡（各ホップで SSRF 再検証、最大リダイレクト数制限あり）
  - HTML は Mozilla Readability.js ベースの本文抽出（`readability-js` クレート）を行い、抽出した clean HTML を Markdown に変換。Readability 失敗時は `htmd` にフォールバック
  - `text/plain` はそのまま返す
  - コンテンツバリデーション: プロンプトインジェクション検出パターンでスキャン
  - `max_fetch_bytes` でフェッチサイズの上限。超過時はエラーにせず取得済み部分を返す（partial content）。`max_output_bytes` で最終出力サイズを制限
  - 末尾に untrusted content warning を付与
  - 本文抽出方式に応じて `extraction` details フィールドに `readability-js`, `fallback-html-to-markdown`, `verbatim` を返す
  - 読み取り専用ツール (`is_read_only: true`)
- `details`:
  - `final_url` (リダイレクト後の最終URL)
  - `content_type`
  - `content_length` (Content-Length ヘッダの値、無い場合は `null`)
  - `fetched_bytes` (実際に取得したバイト数)
  - `response_truncated` (fetch上限で打ち切ったか)
  - `output_truncated` (出力上限で切ったか)
  - `max_fetch_bytes`
  - `max_output_bytes`
  - `extraction` (本文抽出方式)
- 主な失敗:
  - `url must not be empty`
  - `scheme '...' is not allowed`
  - `host '...' is blocked`
  - `private/loopback IP address not allowed`
  - `request timed out after Ns`
  - `request failed: ...`
  - `HTTP 404` (等、HTTP エラーステータス)
  - `too many redirects`
  - `redirect without Location header`
  - `content blocked: ...`
  - `response body is not valid UTF-8`

実装: [egopulse/src/tools/web_fetch/mod.rs](../../egopulse/src/tools/web_fetch/mod.rs)

## 15. セキュリティガード

AI エージェントによるシークレット窃取を防ぐ多層防御。コマンド検閲・パス検閲・出力リダクションの 3 層で構成。

→ 詳細: [security.md](./security.md)
