# orch

A minimal, fast harness that runs coding/AI tasks across several providers
(Claude, OpenAI) and several accounts per provider: one harness instead of
separate terminals or machines for work and personal accounts.

- **Profiles** group accounts. `work` = company Claude Team + company OpenAI,
  `personal` = your own Pro + Plus. A task only ever runs on its profile's
  accounts, and failover never crosses profiles.
- **Folder binding**: tasks started under a profile's `paths` use it
  automatically; override with `-p`.
- **Routing modes** per profile: `auto` (best model for the task),
  `pinned` (switch only between the models you chose), `single` (one model).
- **Failover inside the profile**: when an account hits its usage limit or
  isn't logged in, the task moves to the next account in that profile.
- **Cross-provider delegation**: Claude sessions get an `orch` MCP server with
  `generate_image` (OpenAI image model) and `ask_model` (other providers), so
  Claude can hand off image creation or get a second opinion.
- **Live usage**: each Claude account's rate-limit window usage is read from
  the CLI stream and shown in `orch status` and the TUI.

Subscription accounts run through the **official CLIs** (`claude`, `codex`)
in headless mode, each with its own config dir (`CLAUDE_CONFIG_DIR`,
`CODEX_HOME`). API-key accounts call the HTTP APIs directly.

## Install

```sh
cargo build --release      # target/release/orch(.exe), ~5 MB, no runtime deps
```

Needs `claude` (Claude Code) and/or `codex` (Codex CLI) on PATH for
subscription accounts.

## Quick start

```sh
orch init                                            # ~/.orch/config.toml, uses your current claude login
orch account add claude-work --provider claude --profile work
orch account login claude-work                       # logs in inside its own isolated config dir
orch account add openai-key --provider openai --api-key-env OPENAI_API_KEY --profile personal
orch status
```

Bind folders to profiles in `~/.orch/config.toml`:

```toml
[[profiles]]
name = "work"
accounts = ["claude-work"]
paths = ["C:/work"]
mode = "pinned"
models = ["claude:opus", "claude:sonnet"]
```

## Use

```sh
orch                       # TUI: run several tasks at once, watch accounts live
orch run "fix the flaky login test"
orch run -p work -m claude:opus "plan the billing migration"
orch run -a claude-work "..."   # force one account (no failover)
orch route "draw a logo for the landing page"   # dry run: which model/account
```

In the TUI, prefix a task with `@profile`, `m=provider:model` or `a=account`.

## Models

Built in: `claude:fable` (premium, never auto-picked), `claude:opus`,
`claude:sonnet`, `claude:haiku`, `openai:gpt`, `openai:image`. Override or add
models with `[[models]]` in the config (capabilities, tier, CLI/API names).

Auto routing is rule-based (no extra model call, no added latency): image
requests go to an image model, heavy work (refactor, debug, architecture)
goes to tier 3, light work (summarize, translate) to tier 1, everything else
to tier 2.

## Layout

```
crates/orch-core   engine, router, account state, provider adapters, MCP bridge
  config.rs        accounts, profiles, folder → profile resolution
  router.rs        task classification + candidate ordering
  engine.rs        run loop: route → pick account → run → failover
  state.rs         per-account usage (~/.orch/state.json) + usage.jsonl log
  providers/       claude_cli, codex_cli, api (Anthropic/OpenAI), image
  mcp.rs           `orch mcp` stdio server attached to Claude sessions
crates/orch        CLI + TUI (thin clients over the engine's Event stream)
```

Every front end consumes the same serializable `Event` stream, so the planned
web dashboard is another client of the engine, not a rewrite.

## Roadmap

- `orchd` daemon with a local HTTP/WebSocket API + web dashboard
- Gemini (Gemini CLI + API)
- Bridge for Codex sessions (so GPT can delegate to Claude)
- Streaming for API-key text tasks

## Notes

- Retrying after a limit restarts the task on the next account. The new
  run sees any files the previous one already changed.
- Using several accounts is meant for separating contexts (work/personal,
  teams). Check each provider's terms for what your plans allow.
