---
name: "tmux-api-debug"
description: "Generic tmux-based API debugging workflow for cases like HTTP 4xx/5xx from an upstream provider, intermittent API failures, or when local reproduction is needed. Use when you need to run a backend in a tmux session, reproduce a request (including streaming), capture logs, iterate on minimal fixes, and shut down cleanly."
allowed-tools: "Bash, Read, Write"
---

# tmux API Debug (Generic)

Backend API debugging workflow using tmux.

## Workflow (Compressed)
1. Pre-flight: confirm service/port, endpoint/payload, required env settings.
2. Free the port if needed.
3. Start backend in tmux (dedicated session, background run).
4. Reproduce with minimal request (stream-capable client if needed).
5. Capture tmux logs after each request.
6. Iterate one change at a time; restart and retest.
7. Optional: baseline without features, then re-enable and retest.
8. Clean shutdown and summarize findings.

## Why tmux
- Keeps the backend running while you run requests and inspect logs in the main shell.
- Makes log capture deterministic via `capture-pane` (no scrollback loss).
- Avoids stray foreground processes and makes start/stop idempotent.

## Command Templates (Generic)

### Kill a process on a port
```bash
fuser -k <PORT>/tcp
```

### Start backend in tmux
```bash
tmux new-session -d -s <SESSION_NAME> "<START_COMMAND>"
```

### Stream an API request (example)
```bash
curl -N -s -X POST http://127.0.0.1:<PORT>/<PATH> \
  -H "Content-Type: application/json" \
  -d '<JSON_PAYLOAD>'
```

### Capture recent logs from tmux
```bash
tmux capture-pane -p -t <SESSION_NAME> -S -200
```

### Stop backend
```bash
tmux kill-session -t <SESSION_NAME>
```

## Output expectations
- Exact API error payload if available.
- Key log excerpts showing the failure cause.
- State whether the failure is local or upstream.

## Guardrails
- Do not leak secrets in logs.
- Keep the debug loop tight; avoid unrelated commands.
- Prefer minimal, targeted changes.
