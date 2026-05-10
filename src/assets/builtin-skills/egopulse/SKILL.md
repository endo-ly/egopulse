---
name: egopulse
description: Use when working on EgoPulse itself implementation, architecture, config, channels, tools, MCP, Web UI/API, storage, deployment, security, system prompts, sleep batch, or docs. Also use for requests about EgoPulse internals, "自分の体", "内部実装" or runtime behavior.
---

# EgoPulse

You are working inside EgoPulse, a self-hosted AI agent runtime.
Use this skill as the map of the runtime before answering, designing, or changing EgoPulse itself.

## How To Use

Start from the user's task, then read only the relevant reference files. Do not load every reference by default.

Treat references as orientation. Source code is authoritative when docs and implementation differ.

## Reference Documents

| Topic | File |
|---|---|
| Overall architecture, module boundaries, startup flow | `references/architecture.md` |
| CLI and slash command behavior | `references/commands.md` |
| YAML configuration, resolution, defaults | `references/config.md` |
| TUI / CLI / Web / Discord / Telegram channels | `references/channels.md` |
| Chat/session lifecycle, compaction, sleep batch flow | `references/session-lifecycle.md` |
| Built-in tool definitions and behavior | `references/tools.md` |
| MCP server integration and dynamic tools | `references/mcp.md` |
| OpenAI Codex provider and auth behavior | `references/openai-codex.md` |
| System prompt construction, skills, memory injection | `references/system-prompt.md` |
| Security model and blocked paths/secrets | `references/security.md` |
| Deployment, systemd, release operations | `references/deploy.md` |
| Runtime and workspace directory layout | `references/directory.md` |
| SQLite schema and migrations | `references/db.md` |
| Web UI API contracts | `references/api.md` |
