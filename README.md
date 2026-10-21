# fermi

A minimal, fast harness that runs coding/AI tasks across several providers
(Claude, OpenAI) and several accounts per provider: one harness instead of
separate terminals or machines for work and personal accounts.

Named after Enrico Fermi, who was famous for quick, good-enough estimates
from first principles. That's the job of the router here too: a fast,
cheap guess at the right model for each task.

- **Profiles** group accounts. `work` = company Claude Team + company OpenAI,
  `personal` = your own Pro + Plus. A task only ever runs on its profile's
  accounts, and failover never crosses profiles.
- **Folder binding**: tasks started under a profile's `paths` use it
  automatically; override with `-p`.
- **Routing modes** per profile: `auto` (best model for the task),
  `pinned` (switch only between the models you chose), `single` (one model).
- **Failover inside the profile**: when an account hits its usage limit or
  isn't logged in, the task moves to the next account in that profile.
- **Cross-provider delegation**: Claude sessions get an `fermi` MCP server with
  `generate_image` (OpenAI image model) and `ask_model` (other providers), so
  Claude can hand off image creation or get a second opinion.
- **Live usage**: each Claude account's rate-limit window usage is read from
  the CLI stream and shown in `fermi status` and the TUI.

Subscription accounts run through the **official CLIs** (`claude`, `codex`)
in headless mode, each with its own config dir (`CLAUDE_CONFIG_DIR`,
`CODEX_HOME`). API-key accounts call the HTTP APIs directly.

## Install

```sh
cargo install --path crates/fermi   # puts `fermi` in ~/.cargo/bin (on PATH); ~5 MB, no runtime deps
```

Running `fermi` the first time creates `~/.fermi/config.toml` and opens the TUI.

Needs `claude` (Claude Code) and/or `codex` (Codex CLI) on PATH for
subscription accounts.

## Quick start

```sh
fermi init                                            # ~/.fermi/config.toml, uses your current claude login
fermi account add claude-work --provider claude --profile work
fermi account login claude-work                       # logs in inside its own isolated config dir
fermi account add openai-key --provider openai --api-key-env OPENAI_API_KEY --profile personal
fermi status
```

Bind folders to profiles in `~/.fermi/config.toml`:

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
fermi                       # TUI: run several tasks at once, watch accounts live
fermi run "fix the flaky login test"
fermi run -p work -m claude:opus "plan the billing migration"
fermi run -a claude-work "..."   # force one account (no failover)
fermi route "draw a logo for the landing page"   # dry run: which model/account
```

In the TUI, prefix a task with `@profile`, `m=provider:model` or `a=account`.

## Desktop app

`crates/fermi-desktop` is a native app (Tauri: the system WebView, no bundled
browser, about 10 MB) that shows the dashboard below in its own window,
with its own icon and no console. Its dashboard server runs inside the app on
a random local port, so the desktop app and `fermi ui` are the same interface.

```sh
cargo run -p fermi-desktop                 # run it
cargo tauri build -c crates/fermi-desktop  # installers (.msi / .exe) — needs `cargo install tauri-cli`
```

The icon is the periodic-table tile of Fermium (Fm, 100), named after Fermi.

## Dashboard

```sh
fermi ui                    # the same dashboard in your browser
```

Everything the TUI does, in the browser: run tasks in parallel with live
output, see each account's login and usage, add accounts, and log in
(a terminal window opens for the provider's sign-in, and the page updates
once it's done). It's served by the same binary on `127.0.0.1` only. Every
request needs the access key in the link `fermi ui` prints, and requests from
other sites are refused. Ctrl+C stops it and its running tasks.

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
crates/fermi-core   engine, router, account state, provider adapters, MCP bridge
  config.rs        accounts, profiles, folder → profile resolution
  router.rs        task classification + candidate ordering
  engine.rs        run loop: route → pick account → run → failover
  state.rs         per-account usage (~/.fermi/state.json) + usage.jsonl log
  providers/       claude_cli, codex_cli, api (Anthropic/OpenAI), image
  mcp.rs           `fermi mcp` stdio server attached to Claude sessions
  hub.rs           task registry + broadcast for long-lived front ends
crates/fermi        CLI, TUI and dashboard (thin clients over the engine's Event stream)
  server.rs        `fermi ui`: local HTTP + WebSocket API
  web/index.html   the dashboard (single file, no build step, embedded in the binary)
crates/fermi-desktop  native window around the dashboard (Tauri)
```

Every front end consumes the same serializable `Event` stream.

## Roadmap

- Gemini (Gemini CLI + API)
- Profile editing (folders, mode, models) from the TUI and dashboard
- Bridge for Codex sessions (so GPT can delegate to Claude)
- Streaming for API-key text tasks

## Notes

- Retrying after a limit restarts the task on the next account. The new
  run sees any files the previous one already changed.
- Using several accounts is meant for separating contexts (work/personal,
  teams). Check each provider's terms for what your plans allow.
