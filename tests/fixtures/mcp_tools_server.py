"""Minimal stdio MCP server that exposes one tool and answers tool calls."""

import json
import sys

tool_name = sys.argv[1] if len(sys.argv) > 1 else "ping"

for line in sys.stdin:
    message = json.loads(line)
    if "id" not in message:
        continue
    method = message["method"]
    if method == "initialize":
        result = {
            "protocolVersion": message["params"]["protocolVersion"],
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "tools-fixture", "version": "1"},
        }
    elif method == "tools/list":
        result = {
            "tools": [
                {
                    "name": tool_name,
                    "description": f"fixture tool {tool_name}",
                    "inputSchema": {"type": "object", "properties": {}},
                }
            ]
        }
    elif method == "tools/call":
        result = {
            "content": [{"type": "text", "text": f"{tool_name}:ok"}],
            "isError": False,
        }
    else:
        raise RuntimeError(method)
    print(json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": result}), flush=True)
