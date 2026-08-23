# EgoPulse

> Stricter than OpenClaw. Freer than Hermes Agent.

A runtime where agents remember, notice, and stand side by side. Supports Web UI / Discord / Telegram / TUI / CLI.

## Features

### Agent-First

Session / Memory / Tool / PULSE are all keyed by `agent_id` as the dominant identifier. Multiple agents can run side by side on the same runtime, each with independent memory, and delegate to each other. Channels and tools are all bound to agents.

### Sleep batch & Long time memory

Conversation history is distilled into three layers: episodic (episodic memory) / semantic (semantic memory) / prospective (prospective memory). Agents organize the past through Sleep batches and retain memory long-term.

### PULSE

Receives signals from time, memory, and the outside world, selects what should be brought to conscious attention now, and activates it briefly. It speaks in the usual conversation place only when needed. Not "what to run at what time" but "what to pay attention to at what time".

---

## Getting Started

```bash
curl -fsSL https://raw.githubusercontent.com/endo-ly/egopulse/main/scripts/install.sh | bash
egopulse setup
egopulse gateway install   # register systemd service + start
```

After startup, the WebUI is available at http://127.0.0.1:10961 in your browser.

| Mode | Command | Description |
|---|---|---|
| Gateway | `egopulse gateway install` | Start Web / Discord / Telegram as a service |
| Stop gateway | `egopulse gateway stop` | Stop the systemd service (registration is kept) |
| CLI headless | `egopulse -p "your prompt"` | Run a prompt directly from the terminal |
| TUI | `egopulse` | Session browser + chat (in interactive mode, press `q` to exit) |

See [channels.md](./docs/channels.md) for Discord / Telegram configuration.

---

## Tech Stack

| Layer | Technology |
|---|---|
| Runtime | Rust (Tokio) |
| Persistence | SQLite (WAL mode) |
| Web Server | Axum |
| Web UI | React, Vite |
| LLM | OpenAI-compatible API |

---

## Configuration

Configuration is written in YAML at `~/.egopulse/egopulse.config.yaml`.
You can configure providers, models, channels, Sleep schedules, PULSE intervals, and more.
See [config.md](./docs/config.md) for details.

---

## Documentation

| Topic | Document |
|---|---|
| Architecture overview | [architecture.md](./docs/architecture.md) |
| Command reference | [commands.md](./docs/commands.md) |
| Configuration reference | [config.md](./docs/config.md) |
| Channels (Web/Discord/Telegram/TUI/CLI) | [channels.md](./docs/channels.md) |
| Session lifecycle | [session-lifecycle.md](./docs/session-lifecycle.md) |
| Built-in Tools | [tools.md](./docs/tools.md) |
| MCP integration | [mcp.md](./docs/mcp.md) |
| System Prompt construction | [system-prompt.md](./docs/system-prompt.md) |
| Security | [security.md](./docs/security.md) |
| Deployment | [deploy.md](./docs/deploy.md) |
| DB schema | [db.md](./docs/db.md) |
| WebUI API | [api.md](./docs/api.md) |

---

## License

Licensed under either of [MIT](./LICENSE-MIT) or [Apache License 2.0](./LICENSE-APACHE) at your option.
