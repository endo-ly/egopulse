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
11. [send_attachment](#11-send_attachment)
12. [agent_send](#12-agent_send)
13. [Skill Catalog](#13-skill-catalog)
14. [web_fetch](#14-web_fetch)
15. [セキュリティガード](#15-セキュリティガード)

---

## 1. 前提

- 実装本体: [egopulse/src/tools/mod.rs](../../egopulse/src/tools/mod.rs)
- workspace ルート: `~/.egopulse/workspace`
- skills ルート: `~/.egopulse/workspace/skills`
- path 解決は workspace 配下に制限される
- tool 実行結果は turn loop で `{"tool":"...","status":"success|error","result":"...","details":{...}}` の JSON に包まれて LLM に返る
- `details` は tool によって `truncation`、`diff`、`firstChangedLine`、`fullOutputPath` などを含む
- LLM が有効な JSON でない引数を返した tool call は実行せず、Tool 実行台帳にも記録しない。Turn / Pulse は失敗させず、パースエラーを含む `status: error` の tool result をモデルへ返して再送を促す。会話履歴にはその tool call の引数を `{}` として保存する
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
- `send_attachment`
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

実装: [egopulse/src/tools/mcp.rs](../../egopulse/src/tools/mcp.rs)

MCP の詳細は以下を参照。

- [mcp.md](./mcp.md)

### Tool 実行台帳

既存 `tool_calls` テーブルを Tool 実行台帳として拡張する（[db.md §tool_calls](./db.md#tool_calls)）。新規台帳テーブルは作らない。`Database`（tool.rs）が claim・状態遷移・input 整合確認・結果再利用・uncertain 判定を担う。`tool_input` は Tool に渡す元の JSON 構造を保ちつつ、文字列値を既存のシークレット redaction でサニタイズして保存する。実行と `input_hash` には元の入力を使う。Bash の `required_env` は claim 前に解決され、同じ値を入力サニタイズと子プロセス環境に使う。

#### 実行フロー

1. **claim**: Tool 実行前に `claim(turn_id, tool_call_id, canonical_input)` を呼ぶ。`turn_id + tool_call_id` で既存行を検索し、未登録なら `pending` 作成、`running` へ遷移する。元の canonical input の hash (`input_hash`) を保存し、サニタイズ済み `tool_input` を同時に保存する。既存行と hash が異なれば conflict として拒否する。
2. **実行**: claim 成功後にのみ Tool を実行する。実行前に台帳行を作ることを保証する。
3. **結果保存**: 成功時は `succeeded` + sanitized `tool_output` + `finished_at` を同一 transaction で保存する。失敗時は `failed` + `error_kind` + sanitized `error_message` を保存する。

#### read-only ツールの記録方針

read-only ツール（`is_read_only: true`）は副作用がなく crash 後の再実行が安全なため、claim-before-execute をスキップし、並列実行の書き込み競合を避ける。代わりに、並列ブロックの実行完了後にブロック全体を 1 transaction で台帳へ記録する。

- 行は `succeeded` / `failed` の終端状態で直接書き込まれ、`pending` / `running` を経由しない。crash recovery の `running` スキャンに影響しない
- `idempotency_class = read_only`。sanitize 済みの `tool_input` / `tool_output` と `error_kind` を非 read-only 行と同じ形式で保持する
- 重複する `(turn_id, id)` は、`input_hash` / `tool_name` / `idempotency_class = read_only` の一致を確認してから既存行を維持する。不一致は conflict とし、並列ブロック全体を rollback する
- Tool の実行前に JSON 解析で拒否された要求は、LLM へ `invalid_input` を返すが台帳へ記録しない。non-read-only Tool と同じ扱いとする。実際に実行された Tool の成功・失敗結果は記録する

#### ガード拒否後の停止

1 Run 内で `guard_denied` が 3 回に達すると、後続の Tool 実行をすべて停止する。Turn / Pulse の次の model request では Tool 定義を空にし、停止報告を促す runtime guard を共通処理で付与する。モデルが Tool 要求を返しても実行層で拒否し、未実行要求は台帳に記録しない。拒否回数には実際のポリシー拒否のみを含み、`invalid_input` / `not_found` / `upstream` / `timeout` は加算しない。Pulse が停止報告を生成できない場合や `PULSE_OK` を返す場合は、固定の Notify メッセージをホームチャネルへ送る。

#### error_kind 分類

`ToolResult` が持つ失敗分類が `error_kind` に記録される。分類されないエラーは `tool_error` となる。

| error_kind | 判定元 |
|---|---|
| `guard_denied` | `command_guard` / `path_guard` / workspace 外アクセス / web_fetch の URL ポリシー・コンテンツ検閲 |
| `invalid_input` | 必須パラメータ欠落・スキーマ不一致・不正 URL 構文・Tool 引数 JSON 解析失敗 |
| `timeout` | bash / web_fetch / grep のタイムアウト |
| `exit_nonzero` | bash・ripgrep・fd の非 0 終了 |
| `not_found` | ファイル / パス / スキル / エージェント / ツール不在 |
| `upstream` | DNS 解決失敗・HTTP エラー・MCP transport エラー・チャネル API エラー |
| `tool_error` | 上記に分類されないその他の失敗 |

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
  - テキスト出力は最大 `2000` 行または `100KB`
  - 続きがある場合は `offset=...` の continuation hint を返す
  - 先頭 1 行だけで `100KB` を超える場合は `bash` fallback を促す
- `details`:
  - `truncation`
- 主な失敗:
  - `Missing required parameter: path`
  - `File not found: ...`
  - `Offset ... is beyond end of file`
  - `Failed to read file: file is not valid UTF-8 text or a supported image.`

実装: [egopulse/src/tools/files.rs](../../egopulse/src/tools/files.rs)

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

実装: [egopulse/src/tools/files.rs](../../egopulse/src/tools/files.rs)

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

実装: [egopulse/src/tools/files.rs](../../egopulse/src/tools/files.rs)

## 6. `bash`

- 目的: workspace を cwd にして bash command を実行する
- 入力:
  - `command: string` 必須
  - `timeout: integer` 任意。秒
- 挙動:
  - workspace を cwd にして、コマンド文字列を加工せず `bash --noprofile --norc -c` に渡して実行する。ログインシェルやユーザー起動ファイルは使用しない
  - Linux では bubblewrap の中で起動し、`.env` と `<state_root>/runtime/` を方針メッセージに差し替える。隔離できない環境では隔離せずに実行する（[security.md §7](./security.md#7-bash-の隔離)）
  - 親プロセスの環境変数は引き継がず、`PATH` / `HOME` / `LANG` / `TERM` と skill の `required_env` の解決値のみを渡す。skill の値は同名の継承キーより優先される。`BASH_ENV` / `ENV` / `SHELLOPTS` / `BASHOPTS` は skill の宣言からも除外する
  - stdout / stderr はプロセス起動時に同一の一時ファイルへ割り当て、1 つのログに結合する。bash 自身の構文エラーもこのログに含まれる
  - 出力は末尾側を最大 `2000` 行または `100KB` に tail truncation する
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

実装: [egopulse/src/tools/shell.rs](../../egopulse/src/tools/shell.rs)

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
  - 結果全体は head 側を `100KB` で truncation
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

実装: [egopulse/src/tools/search.rs](../../egopulse/src/tools/search.rs)

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
  - 結果全体は head 側を `100KB` で truncation
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

実装: [egopulse/src/tools/search.rs](../../egopulse/src/tools/search.rs)

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
  - 結果全体は head 側を `100KB` で truncation
  - entry limit に達した場合は `Use limit=... for more` を追記する
- `details`:
  - `truncation`
  - `entryLimitReached`
- 主な失敗:
  - `Path not found: ...`
  - `Not a directory: ...`
  - `Cannot read directory: ...`

実装: [egopulse/src/tools/search.rs](../../egopulse/src/tools/search.rs)

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
| スコープ | Bash 実行前に毎回最新の値を解決し、Turn 内の `ToolExecutionContext.skill_env` に保持する。次の Bash 実行で再解決し、Turn 終了時に破棄する。値は DB・グローバル状態に保存しない |
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

実装: [egopulse/src/tools/activate_skill.rs](../../egopulse/src/tools/activate_skill.rs) · [egopulse/src/skills.rs](../../egopulse/src/skills.rs)

## 11. `send_attachment`

- 目的: ファイルを明示的に現在の会話チャネルへ添付送信する
- 入力:
  - `attachment_path: string` 必須。添付ファイルのローカルパス
  - `text: string` 任意。添付ファイルと一緒に送信するテキスト
- 挙動:
  - 通常の最終テキスト応答はランタイムが自動送信するため、このツールはファイル配布時に使用する
  - パスをワークスペース内へ解決・固定し、検証済みのファイル内容を channel adapter 経由で添付送信
  - テキストだけの送信には使用しない
- `text` のチャネル別契約:
  - Discord: `content` として最大 2000 Unicode スカラー値まで完全に送信する。上限超過時は切り詰めずエラーにする
  - Telegram: `caption` として最大 1024 UTF-8 バイトまで送信する。超過時は UTF-8 文字境界で切り詰めて残りを破棄し、エラーにはしない
- 成功時:
  - `"Attachment sent successfully"`
- 主な失敗:
  - `"Missing required parameter: attachment_path"`
  - `"Attachment path must stay within workspace: <path>"`
  - `"Failed to read attachment: <reason>"`
  - `"no chat found for chat_id <id>"`
  - `"no adapter for channel '<name>'"`
  - `"File not found: <path>"`
  - `"Failed to send attachment: <reason>"`

実装: [egopulse/src/tools/send_attachment.rs](../../egopulse/src/tools/send_attachment.rs)

## 12. `agent_send`

- 目的: 同一チャネル内の別エージェントにメッセージを送信し、宛先エージェントの Turn を非同期にキューイングする
- 入力:
  - `to: string` 必須。宛先エージェント ID（`config.agents` に存在するエージェントのみ指定可能）
  - `message: string` 必須。送信するメッセージ内容
- 挙動:
  - 全チャネルで利用可能
  - 自己送信 (`to == 自分自身`) は禁止
  - `MessageKind::AgentSend` でメッセージを保存する。マルチエージェントルームでは Channel Log へ、それ以外のチャネルでは送信元セッションの chat へ保存する
  - チャネルへの `[From → To] message` 形式での表示は Discord / Telegram で行う。Web はセッション transcript での表示に委ね、TUI / CLI はチャネル表示を行わない
  - 宛先エージェントの Turn を `ScheduledTurn` として `TurnIntake` 経由で永続受理し、バックグラウンド実行する
  - チェーン深度 (chain depth) が `MAX_AGENT_CHAIN_DEPTH` (4) を超える呼び出しはツール実行時に拒否される（`delivered: false`）。受付済みターンが実行時に上限を超えた場合は Channel Log に SystemEvent を記録して破棄する
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
- prompt への埋め込み: [egopulse/src/agent_loop/turn/mod.rs](../../egopulse/src/agent_loop/turn/mod.rs) と [egopulse/src/agent_loop/prompt/builder.rs](../../egopulse/src/agent_loop/prompt/builder.rs)

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
