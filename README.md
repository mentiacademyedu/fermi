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
  isn't logged in, the task moves to the next account in that profile. If the
  account stopped partway, after running tools, its work stays in the folder
  and the next account is told which tool calls already ran and what changed,
  so it carries on instead of repeating edits or side effects.
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
cargo tauri build -c crates/fermi-desktop  # installers (.msi / .exe) — needs `cargo install tauri-cli`
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

**Isolated tasks**: tick *Isolated* in the new-task box (or send `"isolate":
true` to `POST /api/tasks`) and the task works in a git worktree of its own,
on branch `fermi/task-<id>` from the repository's current commit, under
`~/.fermi/worktrees/`. Tasks started together on one repository can't touch
each other's files, and your checkout stays as it is. The work stays in the
worktree, uncommitted, for review: **Trace → Changes** (or
`GET /api/tasks/<id>/diff`) shows the changed files, the full patch against
the starting commit and the same fingerprint the checks report, so you can see
that what you review is what was tested. Follow-ups continue in the same
worktree; deleting the task removes it and its branch.

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

## Code intelligence

fermi indexes each repository with tree-sitter (Rust, Python, JavaScript,
TypeScript/TSX, Go) and keeps the relationships an agent needs before changing
code:

- **Symbols**: functions, methods (with their type), classes, interfaces,
  constants, with signatures and doc comments.
- **Call graph**: references resolved to definitions (same file first, then
  imported files; method calls only to methods), so you see who calls a
  function and what it calls.
- **Imports and dependencies**: per-language resolution (Rust modules and
  workspace crates, Python packages, JS/TS relative paths, Go module packages)
  into a file graph, plus the external packages declared in `Cargo.toml`,
  `package.json`, `go.mod`, `pyproject.toml` and `requirements.txt` and where
  they're used.
- **Tests**: which tests reach which code through calls (and line coverage
  from an `lcov.info` report, if present).
- **Change impact**: from a git diff (uncommitted, or since a commit or
  branch), the changed symbols, every caller up the graph, the files importing
  them, the tests to run, and changes no test reaches, with a risk level.

Agents (Claude, through fermi's bridge) get `code_symbol`, `code_impact`,
`code_search` and `code_file` tools and are told to use them; team agents get
the base of their branch for `code_impact`. You get the *Code map* page and
`fermi code stats|search|show|file|impact|deps|rename`.

- **Rename** (`code_rename` for agents, `fermi code rename <target> <new>`):
  one symbol, chosen by name, `Type::name`, `module::name` or `path:line`, is
  renamed at its definition, at the calls the index resolves to it and in the
  imports that name it, through the syntax tree. Strings, comments and other
  symbols with the same name are left alone and listed. An ambiguous target, a
  name already taken or an edit that wouldn't parse is refused, and nothing is
  written.
- **Syntax errors**: a file that stops parsing keeps the symbols of its last
  valid version, and every result from it says so (`stale`, with the file,
  line and column of the error) until the file parses again.
 The index is incremental
(only changed files are parsed again), parallel, respects `.gitignore`, skips
vendored code, and is cached in `~/.fermi/index`; fermi's own repository
re-indexes in well under a second.

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
   integration branch. A conflict stops the run and waits for you: the run
   shows the conflicting files and what each side changed in them, and the
   integration worktree is left mid-merge. Resolve it there (fix the files and
   `git add` them) and choose **I resolved it — continue**, which fermi refuses
   while files are unmerged or still have conflict markers; or choose **Let an
   agent resolve it** to hand it to a debugger. Nothing picks a side for you. A
   run created with `"conflicts": "agent"` (`POST /api/teams`) sends conflicts
   straight to the debugger instead, for unattended runs
   (`POST /api/teams/<id>/resolve` with `{"how": "manual"}` or `{"how": "agent"}`).
4. **Check.** Optionally a final test pass and a security review run on the
   merged result. Then fermi runs the project's checks itself (see
   *Verification*); if they fail, a debugger fixes the integrated branch and
   they run again, up to `max_repairs` times.
5. **Approve.** You see each branch's diff, the combined changes, the check
   results and every risk the agents raised, then merge into your branch (it
   must be checked out and clean) or discard. Merging needs the checks to have
   passed on exactly the commit being merged; your branch isn't touched before
   that.

Outside team runs, an agent can hand a self-contained subtask to another one
with the `delegate` tool. The other agent gets only what it's given: the
instructions, the files listed (from the task's folder, never ones the security
policy keeps from agents) and the project memory meant for agents, never the
delegating conversation. It runs with the permission it's given (read-only
`plan` by default), never broader than the delegating agent's. What each
delegation received is kept in `~/.fermi/delegations/<task>/`.

Agents that hit an approval show it on their card; with *Let them run commands*
they don't ask. Worktrees live in `~/.fermi/teams/<run>/wt` and are removed on
merge or discard. A run stopped by closing fermi can be resumed.

## Verification

fermi runs a project's checks itself instead of trusting an agent's "all tests
pass". The checks come from `.fermi/checks.toml` in the repository, or are
detected from its manifests (Cargo, npm, Go, pytest):

```toml
max_repairs = 3             # fix turns a verified task gets
protect = ["tests/**"]      # files the agent must leave as they are

[[check]]
name = "build"
kind = "build"              # build | test | lint | audit | other
run = "cargo check --workspace --all-targets --message-format=json"

[[check]]
name = "tests"
kind = "test"
run = "cargo test --workspace --no-fail-fast --message-format=json"
needs = ["build"]           # not run unless build passed

[[check]]
name = "advisories"
kind = "audit"              # no command: fermi's own lockfile scanner
advisories = "~/.fermi/advisories"   # OSV records (JSON)
```

Each check ends passed, failed, skipped or not run, with compiler and linter
diagnostics (file, line, column, rule), failing tests with their output, or the
advisories found, plus the exact revision checked. Mandatory checks (the
default) must all pass.

- **Verified tasks** (*Check & fix* in the composer, or `fermi run --verify`):
  after the agent finishes, fermi runs the checks and sends the failures back
  to the same conversation, up to `max_repairs` times. The task ends done only
  when the checks pass.
- **Agents** get a `run_checks` tool, and `code_replace`, which rewrites one
  whole definition and refuses edits that don't parse.
- `fermi verify [--check NAME] [--json]` runs them in the terminal.

## Costs and budgets

fermi prices every call from versioned tables you put in `config.toml` (USD
per million tokens) and can hold a profile to a spending allowance:

```toml
[[prices]]
version = "2026-10"
effective = "2026-10-01"
[prices.models."claude:sonnet"]
input = 3.0            # uncached input
cached_input = 0.3     # input read from the prompt cache (no discount without it)
output = 15.0
context = 200000       # warn near the context window, refuse over it
quality = 80           # declared rank for routing = "quality"

[[budgets]]
name = "work"
limit = 50             # USD
period = "month"       # total | day | month

[[profiles]]
name = "work"
budget = "work"
routing = "economy"    # auto | economy | quality
```

- **Cost**: each call records its tokens (cached ones apart), the price
  version in effect and the exact cost; a new price version never changes past
  records. Costs are exact to the micro-dollar (rounded half up per call).
- **Budgets**: before a call, fermi reserves its estimate in a shared ledger
  (`~/.fermi/ledger.jsonl`) and refuses the call if it doesn't fit what's left
  after spent and running calls. When the call ends, its actual cost settles
  the reservation. A long agent run can cost more than its estimate, so a
  budget stops new calls; it is not a hard cap on an invoice. The status bar
  and `fermi budget` show where each budget stands.
- **Routing**: `economy` picks the cheapest model that has the capability the
  task needs and fits the budget; `quality` the highest declared rank. The
  decision, with every candidate and why, is in the conversation.
- **Usage**: `fermi usage --by task|run|agent|provider|model|account` (or
  `/api/usage/report`) totals calls, tokens and cost, with failed attempts and
  unattributed calls shown.

## Project memory

Decisions and notes that outlive a conversation, kept per repository (the
dashboard's *Memory* page, or `fermi memory`):

- **Records**: a decision, note or fact with a topic, its author and the files
  it rests on (with the commit and a hash of each). Every new version keeps the
  old ones; nothing is overwritten. Retiring a record keeps it in the history.
- **For agents**: new tasks get the most relevant records ahead of the prompt,
  each with its author and sources, framed as information and never as
  instructions. Agents can also search, read and record (`memory_search`,
  `memory_get`, `memory_record`). *Private* records are only for you.
- **Stale sources**: a record whose source file changed or disappeared is
  marked stale, for you and for agents.
- **Disagreements**: several records on one topic are shown side by side as a
  possible conflict, not merged.
- **Comments**: *Import comments* (`fermi memory ingest`) records `NOTE:`,
  `DECISION:` and `MEMORY:` comments as untrusted records.
- **Search** is deterministic: stemming, a pinned table of related terms
  (database ~ storage ~ postgres…) and BM25 ranking. It matches paraphrases
  that share words or listed synonyms; it isn't an embedding model.

Memory lives in `~/.fermi/memory/<project>/log.jsonl`, one folder per
repository; a project can't read another's.

### Security policy

```toml
[security]
deny_read = ["~/.ssh/**", "**/.env", "C:/work/secrets/**"]   # never read
deny_commands = ["git push *", "curl *"]                      # never run
confirm_commands = ["rm *", "docker *"]                       # ask me first
sandbox = true                                                # see below
net_allow = ["api.github.com"]                                # extra hosts in the sandbox
env_allow = ["GITHUB_TOKEN"]                                  # extra env vars in the sandbox
```

When any of these is set, every tool call a Claude agent makes is decided by
one policy (through Claude Code's permission hook), so the same rule applies
whether the agent uses the file tool, the shell, or one of fermi's own tools:

- **deny_read**: these paths are never read. A plain read is refused, a shell
  command that names the path is refused, and fermi's own tools refuse it too.
- **deny_commands** / **confirm_commands**: Claude-style patterns (`rm *`).
  Denied commands never run; confirmed ones wait for you. An approval covers
  exactly that command once — a replay or a broader command is asked again.
- **Approvals** are audited: `~/.fermi/approvals/` records who decided, when,
  for which task and exactly what. Held actions show in the conversation with
  *Approve once and continue* or *Deny*.

Programs an agent starts itself aren't bound by the permission hook; that takes
the sandbox. Codex and Gemini agents don't have the permission hook, so the
policy reaches them only through the sandbox.

### Sandbox

With `sandbox = true`, each agent CLI (and everything it starts) runs confined:

- **Environment**: only an allowlist of variables (the system ones a program
  needs, plus `env_allow`); fermi's own secrets, including API keys, are left
  out. Every platform.
- **Network**: traffic goes through fermi's egress proxy, which passes only the
  providers' API hosts plus `net_allow` and logs every decision to
  `~/.fermi/egress.jsonl`.
- **Files and ports** (Linux, via Landlock, kernel 6.7+): read/write only in
  the task's folder, its account's config and a private temp; read-only system
  folders; TCP only to the proxy, so a program can't go around it. On Windows
  and macOS only the environment allowlist and the proxy apply; the file and
  network boundaries aren't drawn by the OS there.

## Traces, resume and replay

Every task keeps a trace: each model call (with the usage the provider reported
for it, field for field) and each tool invocation (inputs, output, status,
error code, and how long it took on fermi's clock), with its sequence number,
the attempt it belongs to and, for tools, the model call that asked for it.

- `fermi trace <id>` (or **Trace** on a conversation in the dashboard, or
  `GET /api/tasks/<id>/trace`) lists the operations; failures show their code
  and diagnostics. A tool that never reported back is marked `no_result`, never
  counted as a success.
- The exported trace also has a `timeline` from the request to the outcome:
  each prompt, the route chosen (model, account and why), model calls and
  tools by id, failovers, every check fermi ran (its command, status and the
  revision it tested) and the result, plus `tested_revision`, the fingerprint
  the last checks ran on.
- **Resume**: a task that was cancelled or interrupted (its window closed)
  continues in the same provider session as the next attempt of the same task,
  so its trace stays one sequence (dashboard **Resume**, or
  `POST /api/tasks/<id>/resume`).
- **Replay**: fermi records each CLI's responses for a task in
  `~/.fermi/tapes/<id>/`. `fermi replay <id>` runs the task again from them,
  starting no CLI and touching no network, on a fixed clock, and checks the
  events and state come out the same. The text is the recorded text: a live
  model wouldn't reproduce it word for word. Turn recordings off with
  `[trace] tapes = false`.
- **Redaction**: before anything is saved (history, tapes, exports) or shown,
  fermi removes credentials (API keys, tokens, private keys, `password=…`
  values), the values of this machine's secret-looking environment variables,
  and anything matching your own patterns:

```toml
[trace]
redact = ["ACME-[0-9a-f]{16}"]   # regular expressions
```

## Plugins

Extend fermi without changing it. A plugin is a folder with a `plugin.toml`
and a program that speaks [MCP](https://modelcontextprotocol.io) over stdio:

```toml
name = "acme-tools"
version = "1.2.0"
api = 1                                  # the plugin API it was written for
command = ["acme-mcp", "--stdio"]        # looked up in the plugin's folder, then PATH
net = ["api.acme.example"]               # hosts it may reach (default: none)
env = ["ACME_TOKEN"]                     # variables it gets (default: none)

[[capabilities]]
kind = "tool"                            # offered to agents
name = "search_tickets"

[[capabilities]]
kind = "command"                         # run by you
name = "deploy-status"
tool = "status"                          # the MCP tool behind it (default: name)

[[capabilities]]
kind = "adapter"                         # a model adapter for an account
name = "proxy"
```

- `fermi plugin install <folder>` checks the manifest and enables the plugin;
  only the capabilities it declares are registered. `fermi plugin list`,
  `enable`, `disable`, `remove`, and `fermi plugin run <command> '{"arg": 1}'`.
  The dashboard's **Plugins** page does the same.
- **MCP servers**: `fermi plugin add-mcp github --env GITHUB_PERSONAL_ACCESS_TOKEN --net api.github.com --net registry.npmjs.org --write ~/.npm -- npx -y @modelcontextprotocol/server-github`
  installs a server as a plugin offering its tools to agents. `--env`,
  `--net` and `--write` are what it may use inside its sandbox (`npx` needs
  the npm registry and its cache); a manifest says the same with `env`, `net`,
  `read_only` and `read_write`. Current servers
  (`server/discover`) and older ones (`initialize`) both work; arguments are
  checked against each tool's schema before a call.
- **Isolation**: every call runs the plugin in its own sandboxed process, with
  only the variables in `env` (none of fermi's secrets), network only to `net`
  through fermi's proxy, and on Linux file access only to the task's folder, the
  plugin's own folder and a temp folder. Path arguments outside that, or under
  `[security] deny_read`, are refused before the plugin sees them. A crash or a
  hang (`timeout_secs`, default 60) fails that call only. Turn the sandbox off
  with `[plugins] sandbox = false`.
- **Name collisions**: `[plugins] collisions = "namespace"` (default) keeps
  both as `<plugin>.<name>` and refuses the bare name as ambiguous;
  `"reject"` registers neither. The outcome never depends on install order.
- **Model adapters**: `adapter = "<plugin>.<name>"` on an account sends its
  calls to the plugin. It receives `{prompt, model, effort, resume, cwd}` and
  answers `{text, session, usage, calls, tools, error}`; fermi turns that into
  the same events, usage records and failover as its own adapters.
- A plugin needing a plugin API this fermi doesn't support is refused at
  install, saying which versions are needed and supported.

## Browser

Claude agents can check the web app they're working on in a real browser.
fermi starts a headless Chromium-based browser (Chrome, Edge or Chromium,
the first found, or `[browser] path`) and offers it through its MCP bridge:
`browser_open`, `browser_click`, `browser_fill`, `browser_wait`,
`browser_read`, `browser_check`, `browser_viewport`, `browser_screenshot`,
`browser_state`, `browser_close`.

- Actions wait for what they need: a click happens only once the element is
  visible, enabled and not covered; a wait that runs out is reported as an
  error. Opening a page fails if it can't be reached or answers with an HTTP
  error.
- `browser_state` reports console errors, failed requests (URL, method,
  status), the navigations so far and whether the page overflows sideways;
  `browser_viewport` (or `width`/`height`/`mobile` on `browser_open`) checks
  phone layouts.
- Each task gets a trace of every action, navigation, console message and
  failed request (redacted) and its screenshots, in `~/.fermi/browser/<task>/`.

```toml
[browser]
path = "C:/Program Files/Google/Chrome/Application/chrome.exe"   # optional
no_sandbox = false     # some containers need true
enabled = true         # false hides the tools
```

## Workers

Run tasks somewhere else: a build box, a container, a bigger machine. A
worker is fermi running as a service with its own accounts:

```bash
FERMI_WORKER_TOKEN=… fermi worker --listen 0.0.0.0:8787 --name build-box
```

```toml
[[workers]]
name = "build-box"
url = "http://build-box:8787"
token_env = "FERMI_WORKER_TOKEN"

[[profiles]]
name = "heavy"
accounts = []
workers = ["build-box", "spare-box"]   # tasks of this profile run there

[remote]
heartbeat_secs = 30   # silent this long = the worker is lost
retries = 1           # then try the next worker, at most this many times
```

- fermi sends the task's folder (without `.git`, `target`, `node_modules`) and
  follows the job's numbered events; when it finishes, the artifacts are
  downloaded, checked against the worker's checksums, and its patch is applied
  to your folder, once.
- **Jobs don't need the window.** Closing the dashboard or the desktop app
  leaves tasks on workers running; the next fermi window to open follows them
  again from the last event it saw.
- **Connections drop.** Short outages are waited out and nothing is resent
  or repeated; a worker silent past `heartbeat_secs` counts as lost, and the
  task is retried on the next worker. Only a finished job's result is applied.
- **Cancelling** stops the job and everything it started on the worker and
  frees its folder; fermi keeps asking until the worker confirms.
- Both sides check the worker protocol version and the token before any job
  runs. A worker in a container works the same way (the acceptance tests run
  one in `ubuntu:24.04`).

## Evaluations

Measure which model and configuration does best on the same tasks, and keep
changes honest.

```bash
fermi eval run datasets/smoke --name sonnet --model claude:sonnet
fermi eval run datasets/smoke --name gpt --model openai:gpt --verify
fermi eval compare <run A> <run B>
```

- **Datasets** are folders with a `dataset.toml`: a name, a version, and tasks
  (a prompt, a fixture repository, the paths the candidate may read but not
  change, files only the evaluator sees, such as hidden tests, and weighted
  check commands). `tests/fixtures/F-EVAL/smoke` is a five-task example.
  Each version is registered by content hash: change a dataset and it needs a
  new version, and old results stay with the version they ran on.
- **Runs**: every task starts from a fresh copy of its fixture with the
  protected files locked. The candidate works through the normal engine; then
  fermi's evaluator scores it in a separate folder (the pristine fixture, the
  candidate's changes, the hidden files). A failed run, no change at all, or a
  protected file that changed anyway earns nothing. Reports record the dataset
  and fixture hashes, the evaluator, the configuration's fingerprint, each
  task's score and evidence, latency, tokens and cost (shown as unknown when
  no price applies, never as zero). The patch kept as evidence is the
  candidate's change only: build output it leaves behind (`target/`,
  `node_modules/`, caches) is left out.
- **Comparisons** pair runs of the same dataset version only, per task and in
  total, with missing measurements and the sample size shown. Five tasks are a
  smoke test: the comparison describes those runs and claims no statistical
  significance. The dashboard's **Evaluations** page shows runs, reports and
  comparisons.
- **Improvements**: `fermi eval propose <run>…` asks an agent for one
  configuration change, citing the failures. It stays a proposal until
  `fermi eval evaluate <proposal> <dataset>` shows no lower score and no
  regressions against the active configuration, and a person adopts it
  (`fermi eval adopt <proposal> --reviewer <name>`, or the dashboard).

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
CLI/API names, whether it can call tools). Reasoning effort (`low` … `max`)
can be set per task or as a profile default; it's passed to Claude and Codex
(Gemini ignores it).

A model declared with `tools = false` never gets a task that needs tools
(editing files, running commands): a task marked that way, or any task of a
profile with `require_tools = true`, skips it, or is refused before anything
is sent when it's the only choice. Such tasks also skip API-key accounts,
which send plain text.

**Local models.** Any server with an OpenAI-compatible chat completions API
(llama.cpp, Ollama, vLLM, LM Studio) works as an account; no hosted account
or key is needed:

```toml
[[accounts]]
id = "local"
provider = "openai"
auth = "api"
base_url = "http://localhost:11434/v1"
api_format = "chat"

[[models]]
id = "openai:qwen-local"
caps = ["code", "general"]
api = "qwen2.5-coder"
tools = false
```

**Rate limits.** An API call answered with HTTP 429 is retried on the same
account after the provider's Retry-After (or a doubling backoff), at most
`attempts` calls in all; a longer wait moves on to the next account:

```toml
[retry]
attempts = 3        # calls to one account for one task, the first included
backoff_secs = 1    # when the provider doesn't say how long to wait
max_wait_secs = 60
```

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
  providers/       claude_cli, codex_cli, gemini_cli, api (Anthropic/OpenAI), image
  mcp.rs           `fermi mcp` stdio server attached to Claude sessions
  hub.rs           task registry + broadcast for long-lived front ends, model switching
  team.rs          team runs: plan, worktrees per agent, handoffs, integration, merge
  code.rs          code intelligence: tree-sitter symbol index, call/import graphs, tests, change impact
  deck.rs          designed .pptx decks from a slide description (the create_deck tool)
  sessions.rs      finds and imports Claude Code / Codex / Gemini CLI sessions
  trace.rs         operation traces, tapes of CLI responses, replay
  redact.rs        removes secrets before anything about a task is saved
  plugins.rs       plugin manifests, registry, sandboxed MCP client, model adapters
  eval.rs          evaluation datasets, runs, scoring, comparisons, gated improvements
  browser.rs       headless browser for agents (DevTools protocol), traces, screenshots
  worker.rs        the worker service: jobs, numbered events, artifacts, cancellation
  remote.rs        running tasks on workers: dispatch, follow, retry, apply results
  workspace.rs     isolated tasks: a git worktree and branch per task, its diff for review
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
