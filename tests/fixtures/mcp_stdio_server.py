"""Minimal stdio MCP server for process-lifecycle integration tests."""

import json
import os
import pathlib
import sys
import time


def write_pid(path, pid):
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(str(pid))
    temporary.replace(path)


root = pathlib.Path(sys.argv[1])
mode = sys.argv[2]
write_pid(root / f"{mode}.launcher", os.getpid())
child = os.fork()
if child:
    if mode == "early":
        os._exit(0)
    _, status = os.waitpid(child, 0)
    os._exit(os.waitstatus_to_exitcode(status))

write_pid(root / f"{mode}.pid", os.getpid())
for line in sys.stdin:
    message = json.loads(line)
    if "id" not in message:
        continue
    if message["method"] == "initialize":
        result = {
            "protocolVersion": message["params"]["protocolVersion"],
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "lifecycle-fixture", "version": "1"},
        }
    elif message["method"] == "tools/list":
        result = {"tools": []}
    else:
        raise RuntimeError(message["method"])
    print(json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": result}), flush=True)

if mode == "graceful":
    (root / "graceful.closed").write_text(
        "drained" if (root / "turn.drained").exists() else "closed"
    )
else:
    # Keep stdout open and ignore EOF to exercise the forced group cleanup.
    while True:
        time.sleep(60)
