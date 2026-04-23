# EgoPulse MCP

`egopulse` がどのように MCP (Model Context Protocol) config を読み込み、server に接続し、tool を動的公開し、turn loop から実行するかをまとめた仕様。

この文書は MCP client 統合だけを対象にする。静的 built-in tool の個別仕様は [tools.md](./tools.md) を参照。

## 1. スコープ

- MCP config file の命名と配置
- global / workspace config source の探索順
- config merge ルール
- `stdio` / `streamable_http` transport
- runtime 起動時の接続処理
- tool definition の動的公開
- `mcp_{server}_{tool}` naming
- tool 実行時の dispatch
- MCP 固有 error と失敗時の継続方針
- 現実装の制約

## 2. 目的

MCP 導入の目的は、EgoPulse の静的 built-in tool だけではなく、workspace や外部サービスに応じた tool を runtime 側で動的に追加できるようにすることにある。

これにより以下を実現する。

1. 既存の MCP ecosystem にある server をそのまま接続できる
2. workspace ごとに異なる tool set を切り替えられる
3. built-in tool と同じ turn loop で外部 tool を利用できる

## 3. 関連コンポーネント

MCP 統合に関係する主要ファイルは以下である。

- [`egopulse/src/mcp.rs`](../../egopulse/src/mcp.rs)
  - config 読み込み
  - server 接続
  - adapter 生成 (`create_tool_adapters()`)
  - tool 実行
- [`egopulse/src/tools/mcp_adapter.rs`](../../egopulse/src/tools/mcp_adapter.rs)
  - MCP tool → `Tool` trait の adapter
  - tool definition 変換
  - MCP server 呼び出し
- [`egopulse/src/tools/sanitizer.rs`](../../egopulse/src/tools/sanitizer.rs)
  - 秘匿情報マスキング (全 tool の出力に適用)
- [`egopulse/src/runtime.rs`](../../egopulse/src/runtime.rs)
  - `AppState` 構築時の `McpManager` 初期化
  - adapter を `ToolRegistry` に登録
- [`egopulse/src/error.rs`](../../egopulse/src/error.rs)
  - MCP 固有 error 型

## 4. 命名

MCP 関連ファイルの命名は以下に統一する。

- 単一 config file: `mcp.json`
- config directory: `mcp.d`
- 動的 tool 名: `mcp_{server}_{tool}`

`.mcp.json` は採用しない。

## 5. Config Source

`egopulse` は以下の 4 つの場所を順に探索する。

1. `~/.egopulse/mcp.json`
2. `~/.egopulse/mcp.d/`
3. `~/.egopulse/workspace/mcp.json`
4. `~/.egopulse/workspace/mcp.d/`

実装は [`egopulse/src/mcp.rs`](../../egopulse/src/mcp.rs) の `mcp_config_paths()` にある。

### 役割分担

- global `mcp.json` / `mcp.d`
  - どの workspace でも共通で使う MCP server
  - 個人共通の utility MCP server
  - 認証済み remote MCP server
- workspace `mcp.json` / `mcp.d`
  - repository 固有の local MCP server
  - filesystem / database / local dev server
  - global 設定の override

## 6. Config Merge

config merge は次のルールで行う。

1. 上記の探索順で source を読む
2. directory は `*.json` だけを対象にする
3. `mcp.d/` 内はファイル名昇順で読む
4. 同名 server が複数回出た場合は後勝ちで上書きする
5. 壊れた file は warning を出して skip する

優先度は次の通り。

1. workspace `mcp.d/*.json`
2. workspace `mcp.json`
3. global `mcp.d/*.json`
4. global `mcp.json`

つまり、後に読まれた source ほど優先度が高い。

## 7. Config Shape

トップレベル JSON は次の形を取る。

```json
{
  "defaultProtocolVersion": "2024-11-05",
  "mcpServers": {
    "filesystem": {
      "transport": "stdio",
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "."]
    }
  }
}
```

主なキーは以下である。

- `defaultProtocolVersion`
- `mcpServers`

`mcpServers` は `server_name -> server_config` の map である。

### server_config の主な項目

- 共通
  - `transport`
  - `request_timeout_secs`
  - `protocol_version`
- `stdio`
  - `command`
  - `args`
  - `env`
- `streamable_http`
  - `endpoint`
  - `headers`

## 8. Transport

現在サポートする transport は次の 2 つである。

- `stdio`
- `streamable_http`

### `stdio`

`stdio` では subprocess を起動して MCP server に接続する。

挙動:

- `command` と `args` で process を起動する
- `env` を追加で渡す
- `cwd` は workspace root に固定する
- 接続後に `list_all_tools()` を実行する

### `streamable_http`

`streamable_http` では HTTP client transport を使う。

挙動:

- `endpoint` に接続する
- `headers` を追加する
- `Authorization` は専用ハンドリングする
- `reinit_on_expired_session(true)` を有効にする
- 接続後に `list_all_tools()` を実行する

## 9. 起動時初期化

MCP は `AppState` 構築時に初期化される。

実装: [`egopulse/src/runtime.rs`](../../egopulse/src/runtime.rs)

```rust
let mut tools = ToolRegistry::new(&config, Arc::clone(&skills));

let mcp_manager = McpManager::new(&workspace_dir).await?;
let mcp_arc = Arc::new(RwLock::new(mcp_manager));

// MCP tool を adapter 経由で registry に登録
let adapters = McpManager::create_tool_adapters(&mcp_arc).await;
for adapter in adapters {
    tools.register_tool(adapter);
}

// AppState が mcp_manager を直接保持 (status snapshot 用)
AppState { ..., mcp_manager: Some(mcp_arc), ... }
```

処理順は次の通り。

1. built-in tool registry を作る
2. `McpManager::new()` で config を読み込む
3. 各 server に接続する
4. 各 server から `tools/list` を取得する
5. `create_tool_adapters()` で各 MCP tool を `McpToolAdapter` にラップする
6. 各 adapter を `register_tool()` で registry に登録する
7. `McpManager` への参照は `AppState` が直接保持する
8. turn loop で LLM へ全 tool definitions を返す

## 10. Tool 公開

MCP tool は `McpToolAdapter` 経由で built-in tool と同じ `ToolDefinition` 形式で LLM に公開される。`ToolRegistry` は built-in / MCP の区別なく全 tool を一様に管理する。

実装: [`egopulse/src/tools/mcp_adapter.rs`](../../egopulse/src/tools/mcp_adapter.rs)

### definition 列挙

- `ToolRegistry::definitions_async()`
  - 全 tool (built-in + MCP adapter) を一様に iterate して定義を収集
  - MCP 特有の分岐なし

### dispatch

- `ToolRegistry::execute()`
  - 全 tool を名前で探索して dispatch
  - MCP adapter の `execute()` が内部で MCP server に委譲
  - MCP 特有の分岐なし

## 11. Tool Naming

LLM に見える名前は `mcp_{server}_{tool}` 形式である。

例:

- `filesystem` server の `read_file`
  - `mcp_filesystem_read_file`
- `remote-db` server の `query(1)`
  - `mcp_remote_db_query_1_`

sanitize ルールは次の通り。

- 英数字と `_` 以外は `_` に置換する
- 長さが 64 文字を超える場合は short hash に置き換える

同一 server 内で sanitize 後の名前が衝突した場合、その tool は skip する。

## 12. Tool 実行

LLM から MCP tool が呼ばれたときの流れは次の通り。

1. `ToolRegistry::execute()` が tool 名を受け取る
2. `McpManager::is_mcp_tool()` で server index と元の tool 名を特定する
3. `McpManager::execute_tool()` が `call_tool` を送る
4. MCP response の content を text に整形する
5. `ToolResult::success` または `ToolResult::error` に変換する

### 入力規則

MCP tool へ渡す引数は JSON object である必要がある。

例:

- 正常: `{"query":"status"}`
- 異常: `"status"`
- 異常: `["status"]`

object 以外が渡された場合は `mcp_tool_call_failed` になる。

## 13. 出力整形

MCP response の content は次のように整形される。

- `Text`
  - そのまま文字列化
- `Image`
  - `[image: <mime> (<bytes> bytes)]`
- `Audio`
  - `[audio: <mime> (<bytes> bytes)]`
- `Resource`
  - `resource:` または `blob:` 形式の要約
- `ResourceLink`
  - `[resource_link: <uri> (<name>)]`
- `structured_content`
  - `[structured_content: ...]`

出力が空なら `(no output)` を返す。

## 14. 失敗時の扱い

MCP 固有の主な error は以下である。

- `mcp_config_read_failed`
- `mcp_config_parse_failed`
- `mcp_connection_failed`
- `mcp_tool_list_failed`
- `mcp_tool_call_failed`

実装: [`egopulse/src/error.rs`](../../egopulse/src/error.rs)

runtime 方針は次の通り。

1. config file 単位の失敗は skip して継続する
2. server 単位の接続失敗は warning を出して継続する
3. tool 実行時の失敗は tool error として LLM に返す
4. 一部 server の失敗で runtime 全体は停止しない

## 15. 設定例

### `stdio`

```json
{
  "mcpServers": {
    "filesystem": {
      "transport": "stdio",
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "."],
      "request_timeout_secs": 120
    }
  }
}
```

### `streamable_http`

```json
{
  "mcpServers": {
    "remote": {
      "transport": "streamable_http",
      "endpoint": "http://127.0.0.1:8080/mcp",
      "headers": {
        "Authorization": "Bearer REPLACE_ME"
      },
      "request_timeout_secs": 60
    }
  }
}
```

### global + workspace override

global:

```json
{
  "mcpServers": {
    "shared": {
      "transport": "stdio",
      "command": "npx",
      "args": ["-y", "shared-server"]
    }
  }
}
```

workspace:

```json
{
  "mcpServers": {
    "shared": {
      "transport": "stdio",
      "command": "npx",
      "args": ["-y", "override-server"]
    },
    "local": {
      "transport": "stdio",
      "command": "node",
      "args": ["local.js"]
    }
  }
}
```

結果:

- `shared` は workspace 定義で上書きされる
- `local` は workspace から追加される

## 16. 現実装の制約

現実装の制約は次の通り。

- health probe の常駐監視は未実装
- retry / backoff / circuit breaker は未実装
- tool list の TTL cache は未実装
- `defaultProtocolVersion` / `protocol_version` は parse されるが接続時未使用
