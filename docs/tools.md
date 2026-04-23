# EgoPulse Built-in Tools

現在の `egopulse` に実装されている built-in tools の一覧と仕様。  

## 参考元

- `pi-mono` repository
  - https://github.com/badlogic/pi-mono
- `coding-agent` README
  - https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/README.md
- built-in tools 実装ディレクトリ
  - https://github.com/badlogic/pi-mono/tree/main/packages/coding-agent/src/core/tools

## 前提

- 実装本体: [egopulse/src/tools.rs](../../egopulse/src/tools.rs)
- workspace ルート: `~/.egopulse/workspace`
- skills ルート: `~/.egopulse/workspace/skills`
- path 解決は workspace 配下に制限される
- tool 実行結果は turn loop で `{"tool":"...","status":"success|error","result":"...","details":{...}}` の JSON に包まれて LLM に返る
- `details` は tool によって `truncation`、`diff`、`firstChangedLine`、`fullOutputPath` などを含む
- マルチモーダル画像対応: `read` tool で画像ファイルを検出した場合、base64 data URL として LLM に直接渡す。マルチモーダルメッセージが含まれる場合は OpenAI Responses API (`/responses`) に自動ルーティングされる（Chat Completions API はマルチモーダル tool result に非対応のため）。セッション永続化時は画像を SHA256 ハッシュで内容重複排除し、参照形式 (`input_image_ref`) で保存する

## Tool Registry

`ToolRegistry` は全 tool を `Box<dyn Tool>` として一元管理する。built-in / MCP の区別なく、統一的に定義列挙・実行 dispatch を行う。

### Built-in tool

registry に静的登録されている tool は次の 8 つ。

- `read`
- `bash`
- `edit`
- `write`
- `grep`
- `find`
- `ls`
- `activate_skill`

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

## `read`

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

## `write`

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

## `edit`

- 目的: 既存ファイルの exact text replacement
- 入力:
  - `path: string` 必須
  - `edits: array` 必須
  - legacy 互換として top-level `oldText` / `newText` も受け取り、内部で `edits[]` に畳み込む
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

## `bash`

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

## `grep`

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

## `find`

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

## `ls`

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

## `activate_skill`

- 目的: 発見済み skill の本文をロードする
- 入力:
  - `skill_name: string` 必須
- 挙動:
  - `SkillManager::load_skill_checked()` を呼ぶ
  - 返り値には skill name、description、skill directory、instructions 本文を含む
- 主な失敗:
  - `Missing required parameter: skill_name`
  - `Skill '<name>' not found. ...`
  - `failed to read skill '<name>': ...`

実装: [egopulse/src/tools.rs](../../egopulse/src/tools.rs)

## Skill Catalog

`activate_skill` とは別に、各 turn の system prompt には skill の概要一覧が入る。

- catalog 生成: [egopulse/src/skills.rs](../../egopulse/src/skills.rs) `build_skills_catalog()`
- prompt への埋め込み: [egopulse/src/agent_loop/turn.rs](../../egopulse/src/agent_loop/turn.rs)

つまり skill 本文は初期ロードされず、最初に入るのは概要一覧だけ。

## セキュリティガード

AI エージェントによるシークレット窃取を防ぐ多層防御。コマンド検閲・パス検閲・出力リダクションの 3 層で構成。

→ 詳細: [security.md](./security.md)

## Path and Directory Rules

- workspace root:
  - [egopulse/src/config.rs](../../egopulse/src/config.rs)
  - `~/.egopulse/workspace`
- skills root:
  - [egopulse/src/config.rs](../../egopulse/src/config.rs)
  - `~/.egopulse/workspace/skills`
- path guard:
  - [egopulse/src/tools/path_guard.rs](../../egopulse/src/tools/path_guard.rs)
  - `..` で workspace 外へ出る path は拒否する
  - `.ssh`, `.aws`, `.env` 等の機密パスはブロック（詳細: [security.md](./security.md#2-パスガード)）

## 現在残っている主な非互換

- `read` は画像ファイルを検出して LLM に渡すところまで対応済み（Responses API 経由）
- セッション永続化でも画像を SHA256 参照形式で保存・復元できる
- 未対応: ストリーミングでのマルチモーダル tool result（tools 使用時はストリーミングを無効化して通常 API にフォールバックしている）
