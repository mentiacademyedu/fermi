# fermi

A minimal, fast harness that runs coding/AI tasks across several providers
(Claude, OpenAI, Gemini) and several accounts per provider: one harness instead of
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

Subscription accounts run through the **official CLIs** (`claude`, `codex`, `gemini`)
in headless mode, each with its own config dir (`CLAUDE_CONFIG_DIR`,
`CODEX_HOME`, `GEMINI_CLI_HOME`). API-key accounts call the HTTP APIs directly.

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
cargo tauri build -c crates/fermi-desktop  # installers (.msi / .exe) â€” needs `cargo install tauri-cli`
```

The icon is the periodic-table tile of Fermium (Fm, 100), named after Fermi.

## Dashboard

```sh
fermi ui                    # the same dashboard in your browser
```

Everything the TUI does, in the browser: run tasks in parallel with live
output, **reply to a finished task to continue the conversation** (same
session and account; you can **switch the model** for any reply, see below),
**import sessions** from Claude Code, Codex and the Gemini CLI, browse and edit workspace files, review what
agents changed in a **Changes** panel (git status and diffs), open **terminals** in
your workspaces (Ctrl+`; PowerShell on Windows, your `$SHELL` elsewhere), check
a website you're building in the **Preview** tab (an internal browser: it finds dev
servers already running on the usual ports, can serve a plain HTML folder on its
own local port, switches between desktop/tablet/mobile widths, and offers to open
`localhost` links printed in terminals or task output), view and edit **slide
decks** agents make (see below), browse
**trending GitHub repositories** and clone or analyze them with an editable prompt
(read-only "Plan only" by default), keep a history across restarts that every
fermi window on the PC shares (desktop, `fermi ui`, the TUI and `fermi run`;
see `fermi history` and `fermi show <id>`)
(`~/.fermi/history`), edit profiles, see each account's login and usage,
add or remove accounts, and log in
(a terminal window opens for the provider's sign-in, and the page updates
once it's done). It's served by the same binary on `127.0.0.1` only. Every
request needs the access key in the link `fermi ui` prints, and requests from
other sites are refused. Ctrl+C stops it and its running tasks.

## Continuing conversations

- **Switch models mid-conversation**: pick a model (and effort) in the reply
  box. Another model of the same provider resumes the same session; a model of
  another provider starts a fresh session that gets the conversation so far
  (long ones keep the start and the most recent messages). Either way the next
  reply carries on from there.
- **Import sessions** from other tools: the dashboard's *Import sessions* page
  (or `fermi import`, then `fermi import <number>`) lists the conversations
  Claude Code, Codex and the Gemini CLI saved on this PC. Imported ones join the
  shared history. When one of your accounts owns the folder a session was saved
  in (e.g. an account with `home = "default"` for `~/.claude`), a reply resumes
  that session in place; otherwise the conversation is passed on to whichever
  model you pick. Sessions fermi ran itself show up as already imported.

## Slides

Decks open in a slides view (from the file tree, a task's output, or Changes):

- **PowerPoint (.pptx)**, e.g. made with python-pptx or pptxgenjs: thumbnails,
  speaker notes, and editing in place: double-click text to edit it, drag to
  move, drag the corner to resize, arrow keys to nudge, Delete to remove a
  shape, add text boxes, duplicate / reorder / delete slides, undo (Ctrl+Z).
  Saving rewrites only the parts that changed; everything else in the file is
  kept byte for byte. Charts and SmartArt show as placeholders.
- **Markdown decks** (Marp style: `marp: true` front matter or a `*.slides.md`
  name, slides separated by `---`): a live editor next to the slides, with
  Marp's `_class`/`paginate`/`header`/`footer` directives, `![bg](…)`
  backgrounds and HTML-comment speaker notes. Any Markdown file with `---`
  breaks has a *Slides* button.

Both have **Present** (full screen; arrows, space, Esc; F5 starts from the top)
and **PDF** (print to PDF, one page per slide). Markdown decks open as slides
(the code is one click away) and have **Save as PowerPoint**.

Agents make decks with fermi's `create_deck` tool: they describe the slides
(title, bullets, numbered steps, two columns, image, quote, table, big number,
speaker notes; six color themes) and fermi writes a designed `.pptx` itself,
so no script has to run and nothing needs approval.

## Teams

A team run puts several agents with roles on one goal in a git repository
(the dashboard's *Teams* page):

1. **Plan.** The architect (read-only) studies the repo and submits a plan:
   tasks with a role (architect, developer, tester, security reviewer,
   debugger, researcher), dependencies, the files each may change and when it's
   done. You review and edit it, then start.
2. **Build.** Every task runs as its own agent conversation in its own git
   worktree and branch (`fermi/team-…/<task>`), started from the base commit
   plus the branches it depends on, up to N at a time. Each pick its model per
   role. Agents finish with a structured handoff (summary, files, decisions,
   risks, next steps, tests) that the tasks depending on them receive.
3. **Merge branches.** Finished branches are merged in dependency order into an
   integration branch; a conflict goes to a debugger agent in that worktree.
4. **Check.** Optionally a final test pass and a security review run on the
   merged result.
5. **Approve.** You see each branch's diff, the combined changes and every risk
   the agents raised, then merge into your branch (it must be checked out and
   clean) or discard. Your branch isn't touched before that.

Agents that hit an approval show it on their card; with *Let them run commands*
they don't ask. Worktrees live in `~/.fermi/teams/<run>/wt` and are removed on
merge or discard. A run stopped by closing fermi can be resumed.

## Skills & agents

Every fermi account keeps its own config folder, so skills you installed in
`~/.claude` aren't visible to an isolated account. The *Skills & agents* page
(or `fermi skills list`) shows every skill (`skills/<name>/SKILL.md`) and
subagent (`agents/<name>.md`) across your Claude Code, Codex and Gemini CLI
accounts, the CLIs' own folders and your projects' `.claude` / `.codex` /
`.gemini` folders, and where copies differ.

- **Copy** a skill or agent to other accounts and projects. Subagents copied
  between Claude Code and the Gemini CLI are converted (name, description and
  instructions carry over; tool lists and models don't). Codex has no
  subagents.
- **Export** a selection to a `.zip` (`skills/…`, `agents/…`), or
  `fermi skills export out.zip [names]`.
- **Import** a `.zip` / `.skill` archive (a fermi export, a Claude plugin, a
  skill download), a folder or a `.md` agent: review what's in it, pick where it
  goes. Or drop the file on the page, or `fermi skills import <path> --to <place>`.

Replaced and removed items are moved to `~/.fermi/trash`, never deleted.

## Approvals

The CLIs run headless, so a tool call that needs approval (by default, shell
commands under `acceptEdits`) can't stop to ask and gets denied. fermi shows
what was blocked under the turn, with **Allow … and continue** (allows those
programs, e.g. `Bash(node:*)`, for the rest of the conversation) and **Allow
everything for this task**.

## Models

Built in, shown with their versions in the TUI and dashboard:

| Provider | Auto-routed | Pick by hand |
| --- | --- | --- |
| Claude | Opus 5.5, Sonnet 5.5, Haiku 5.5 | Fable 5.1 (premium) |
| OpenAI | GPT (Codex default), GPT Image 2.5 Flare | GPT-6.1 Sol, GPT-6 Astra, GPT-6 Luna |
| Gemini | Gemini Pro (latest), Gemini 3.8 Flash | Gemini 3.5 Flash-Lite |

Override or add models with `[[models]]` in the config (capabilities, tier,
CLI/API names). Reasoning effort (`low` … `max`) can be set per task or as a
profile default; it's passed to Claude and Codex (Gemini ignores it).

Auto routing is rule-based (no extra model call, no added latency): image
requests go to an image model, heavy work (refactor, debug, architecture)
goes to tier 3, light work (summarize, translate) to tier 1, everything else
to tier 2.

## Layout

```
crates/fermi-core   engine, router, account state, provider adapters, MCP bridge
  config.rs        accounts, profiles, folder â†’ profile resolution
  router.rs        task classification + candidate ordering
  engine.rs        run loop: route â†’ pick account â†’ run â†’ failover
  state.rs         per-account usage (~/.fermi/state.json) + usage.jsonl log
  providers/       claude_cli, codex_cli, gemini_cli, api (Anthropic/OpenAI), image
  mcp.rs           `fermi mcp` stdio server attached to Claude sessions
  hub.rs           task registry + broadcast for long-lived front ends, model switching
  team.rs          team runs: plan, worktrees per agent, handoffs, integration, merge
  deck.rs          designed .pptx decks from a slide description (the create_deck tool)
  sessions.rs      finds and imports Claude Code / Codex / Gemini CLI sessions
crates/fermi        CLI, TUI and dashboard (thin clients over the engine's Event stream)
  server.rs        `fermi ui`: local HTTP + WebSocket API
  slides.rs        .pptx parts in and out (the dashboard renders and edits the XML)
  skills.rs        skills and subagents: find, copy/convert, export and import bundles
  web/index.html   the dashboard (single file, no build step, embedded in the binary)
crates/fermi-desktop  native window around the dashboard (Tauri)
```

Every front end consumes the same serializable `Event` stream.

## Roadmap

- Profile editing (folders, mode, models) from the TUI and dashboard
- Bridge for Codex sessions (so GPT can delegate to Claude)
- Streaming for API-key text tasks

## Notes

- Retrying after a limit restarts the task on the next account. The new
  run sees any files the previous one already changed.
- Using several accounts is meant for separating contexts (work/personal,
  teams). Check each provider's terms for what your plans allow.
