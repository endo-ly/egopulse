# EgoPulse MCP

MCP (Model Context Protocol) の設定・接続・ツール動的公開の仕様。静的 built-in tool は [tools.md](./tools.md) を参照。

## 目次

1. [目的](#1-目的)
2. [命名](#2-命名)
3. [Config](#3-config)
4. [Transport](#4-transport)
5. [Tool の公開と実行](#5-tool-の公開と実行)
6. [障害と制約](#6-障害と制約)

---

## 1. 目的

静的 built-in tool に加え、workspace や外部サービスに応じた tool を runtime 側で動的に追加する。

1. 既存の MCP ecosystem にある server をそのまま接続
2. workspace ごとに異なる tool set を切り替え
3. built-in tool と同じ turn loop で外部 tool を利用

## 2. 命名

- 単一 config file: `mcp.json`
- config directory: `mcp.d`
- 動的 tool 名: `mcp_{server}_{tool}`

`.mcp.json` は採用しない。

## 3. Config

### 探索順

以下の 4 箇所を順に探索する。

1. `~/.egopulse/mcp.json`
2. `~/.egopulse/mcp.d/`
3. `~/.egopulse/workspace/mcp.json`
4. `~/.egopulse/workspace/mcp.d/`

- **global** (`~/.egopulse/mcp.*`): 認証済み remote MCP server、個人共通の utility
- **workspace** (`workspace/mcp.*`): repository 固有の local server、global 設定の override

### Merge ルール

1. 探索順で source を読む（directory は `*.json` のみ、ファイル名昇順）
2. 同名 server は後勝ちで上書き
3. 壊れた file は warning を出して skip

優先度: workspace `mcp.d/` > workspace `mcp.json` > global `mcp.d/` > global `mcp.json`

### Config Shape

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

`mcpServers` は `server_name -> server_config` の map。

| 項目 | 型 | 説明 |
|------|-----|------|
| `transport` | `"stdio" \| "streamable_http"` | 接続方式 |
| `request_timeout_secs` | `number` | リクエストタイムアウト |
| `protocol_version` | `string` | MCP プロトコルバージョン |

**`stdio`**: `command`, `args`, `env`
**`streamable_http`**: `endpoint`, `headers`

### 設定例

#### `stdio`

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

#### `streamable_http`

```json
{
  "mcpServers": {
    "remote": {
      "transport": "streamable_http",
      "endpoint": "http://127.0.0.1:8080/mcp",
      "headers": { "Authorization": "Bearer REPLACE_ME" },
      "request_timeout_secs": 60
    }
  }
}
```

## 4. Transport

### `stdio`

subprocess を起動して MCP server に接続。`cwd` は workspace root に固定。接続後に `list_all_tools()` を実行。

### `streamable_http`

HTTP client transport で接続。`endpoint` に接続し `headers` を付与。`Authorization` は専用ハンドリング。`reinit_on_expired_session(true)` 有効。

## 5. Tool の公開と実行

### 起動時初期化

MCP は `AppState` 構築時に `McpManager::new()` で初期化される。各 server に接続後、`tools/list` でツール一覧を取得し、`create_tool_adapters()` で `McpToolAdapter` にラップして `ToolRegistry` に登録する。

### 公開

`McpToolAdapter` 経由で built-in tool と同じ `ToolDefinition` 形式で LLM に公開される。`ToolRegistry` は built-in / MCP の区別なく全 tool を一様に管理する。

### Tool Naming

LLM に見える名前は `mcp_{server}_{tool}` 形式。英数字と `_` 以外は `_` に置換。64 文字超はハッシュ短縮。衝突時は skip。

### 実行フロー

1. `ToolRegistry::execute()` が tool 名を受け取る
2. `McpManager::is_mcp_tool()` で server と tool を特定
3. `McpManager::execute_tool()` が `call_tool` を送信
4. MCP response の content を text に整形

### 入力規則

引数は JSON object である必要がある。object 以外は `mcp_tool_call_failed`。

### 出力整形

| content 種別 | 出力 |
|-------------|------|
| `Text` | そのまま文字列化 |
| `Image` | `[image: <mime> (<bytes> bytes)]` |
| `Audio` | `[audio: <mime> (<bytes> bytes)]` |
| `Resource` | `resource:` / `blob:` 形式の要約 |
| `ResourceLink` | `[resource_link: <uri> (<name>)]` |

出力が空なら `(no output)`。

MCP tool は built-in tool と異なり出力サイズを自己規制しないため、`50KB`（`DEFAULT_MAX_BYTES`）を超える場合は head 側を切断し `... (output truncated to 51200 bytes)` を付加する。built-in tool は各自が同限値で切断済みであるため、このセーフティネットは実質 MCP にのみ作用する。

## 6. 障害とリカバリ

### エラー種別と runtime 方針

- `mcp_config_read_failed` / `mcp_config_parse_failed`
- `mcp_connection_failed` / `mcp_tool_list_failed`
- `mcp_tool_call_failed`

1. config file 単位の失敗は skip して継続
2. server 単位の接続失敗は warning を出して継続（`failed_servers` に保持）
3. tool 実行時の失敗は tool error として LLM に返す
4. 一部 server の失敗で runtime 全体は停止しない

### 自動再接続（MCP Reconnect Loop）

`spawn_mcp_reconnect_loop()` が runtime 起動時にバックグラウンド task として稼働し、失敗した MCP server の自動再接続を試みる。

```text
loop:
  failed_servers が空 → MAX_RETRY_SECS (300s) 待機 → 再評価
  failed_servers が存在 → retry_secs 待機 → 再接続試行
    再接続成功 → reconnected ツールを registry に追加 → retry_secs をリセット
    再接続失敗 → retry_secs を指数バックオフ（×2）して MAX_RETRY_SECS にキャップ
```

| パラメータ | 値 | 説明 |
---|---|---|
| `INITIAL_RETRY_SECS` | 5 秒 | 初回の再接続間隔 |
| `MAX_RETRY_SECS` | 300 秒 | バックオフ上限（5 分） |

### 未実装の機能

- health probe の常駐監視: 接続済み server の定期的な死活確認。現在は tool 呼び出しの失敗で初めて検知
- tool list の TTL cache: 接続時に `list_all_tools()` を1回だけ呼び出し、以降は `cached_tools` を使い回す。server 側がツールを追加・削除しても runtime 再起動まで反映されない
- `defaultProtocolVersion` / `protocol_version`: config から parse されるが接続時には未使用。常に rmcp デフォルトが使われる