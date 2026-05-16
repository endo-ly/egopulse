# Refactor AS-IS / TO-BE Memo

This memo records the proposed directory shape before moving files. It is an
orientation note, not an implementation plan.

## AS-IS

```text
src/
├── agent_loop/       # turn execution, sessions, prompts, compaction
├── runtime/          # AppState assembly, lifecycle, gateway, status, schedulers
├── channels/         # CLI, TUI, Web, Discord, Telegram
├── config/           # config types, load, persist, resolve
├── storage/          # SQLite types, migrations, queries
├── tools/            # tool registry, built-in tools, MCP, guards
├── llm/              # provider trait and OpenAI-compatible client
├── pulse/            # pulse definitions, runner, scheduler, output
├── setup/            # setup wizard
├── memory.rs
├── skills.rs
├── sleep_batch.rs
├── sleep_scheduler.rs
├── soul_agents.rs
└── main.rs / lib.rs
```

## TO-BE Candidate

Prefer product vocabulary over generic DDD layer names. Keep the existing
successful boundaries where they are already clear, and move only when the new
location makes ownership and dependencies easier to understand.

```text
src/
├── agent/            # turn execution, sessions, prompts, compaction
├── runtime/          # runtime state, service assembly, lifecycle, gateway, status
├── channels/         # CLI, TUI, Web, Discord, Telegram
├── config/           # config domain, load, persist, resolve
├── storage/          # persistence models, migrations, repositories/queries
├── llm/              # provider abstraction and concrete clients
├── tools/            # tool registry and built-in tools
├── skills/           # skill discovery, activation support, catalogs
├── memory/           # memory loading and sleep input collection
├── sleep/            # sleep batch input, prompt, runner, memory writer, audit
├── pulse/            # pulse definitions, runner, scheduler, output
├── assets/           # embedded/static asset management
├── setup/            # setup wizard
└── main.rs / lib.rs
```

## Migration Order

1. Clarify runtime state ownership before moving directories.
2. Rename `agent_loop` to `agent` only after `AppState` no longer behaves like a
   broad mutable service bag.
3. Promote single-file domains (`skills.rs`, `memory.rs`, `sleep_batch.rs`) into
   directories when each has a stable internal API.
4. Split `storage::queries` by persistence concern after storage call sites are
   grouped around explicit repository-like methods.
5. Remove compatibility-only entry points instead of preserving old paths during
   moves.

