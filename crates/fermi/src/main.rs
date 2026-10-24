mod tui;

use anyhow::Result;
use clap::{Parser, Subcommand};
use fermi::{server, setup};
use fermi_core::{Engine, Event, Task};
use std::io::{IsTerminal, Read};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "fermi", version, about = "Minimal multi-account, multi-provider agent harness")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(clap::Args)]
struct Target {
    /// Profile to use (default: resolved from the working directory)
    #[arg(short, long)]
    profile: Option<String>,
    /// Force a model, e.g. claude:opus or openai:gpt
    #[arg(short, long)]
    model: Option<String>,
    /// Force an account (disables failover)
    #[arg(short, long)]
    account: Option<String>,
    /// Reasoning effort: low, medium, high, xhigh, max
    #[arg(short, long)]
    effort: Option<String>,
    /// Working directory for the task
    #[arg(short = 'C', long)]
    dir: Option<PathBuf>,
    /// Check the work with the project's checks and send failures back to the
    /// agent, up to REPAIRS more turns (default: the project's max_repairs, else 3)
    #[arg(long, value_name = "REPAIRS", num_args = 0..=1)]
    verify: Option<Option<u32>>,
    /// Task prompt (reads stdin when omitted)
    prompt: Vec<String>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create ~/.fermi/config.toml
    Init {
        #[arg(long)]
        force: bool,
    },
    /// Add or log in to accounts
    #[command(subcommand)]
    Account(AccountCmd),
    /// Show profiles, accounts and usage
    Status,
    /// Show which model and account a task would use, without running it
    Route(Target),
    /// Run a task and stream its output
    Run(Target),
    /// Recent tasks from every fermi window on this PC
    History {
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
    },
    /// Print one task's conversation
    Show { id: u64 },
    /// Every model call and tool invocation of a task: inputs, outputs, status, durations
    Trace {
        id: u64,
        /// The full trace as JSON
        #[arg(long)]
        json: bool,
    },
    /// Run a task again from its recorded responses (no CLI, no network) and
    /// check that its events and state come out the same
    Replay {
        id: u64,
        #[arg(long)]
        json: bool,
    },
    /// List sessions from Claude Code, Codex and the Gemini CLI, or import some
    /// (by list number or session id) so they show up and continue in fermi
    Import {
        /// List numbers or session ids (prefixes work); none = list
        which: Vec<String>,
        #[arg(short = 'n', long, default_value_t = 30)]
        limit: usize,
    },
    /// Skills and subagents of your accounts: list, export to a .zip, import
    #[command(subcommand)]
    Skills(SkillsCmd),
    /// Run this repository's checks (build, tests, lint, advisories) as fermi
    /// verifies work: .fermi/checks.toml, or detected from the manifests
    Verify {
        /// Only these checks (repeat for several)
        #[arg(long)]
        check: Vec<String>,
        /// The full report as JSON
        #[arg(long)]
        json: bool,
        /// Repository folder (default: the current one)
        #[arg(short = 'C', long)]
        dir: Option<PathBuf>,
    },
    /// This project's memory: decisions and notes kept across sessions
    #[command(subcommand)]
    Memory(MemoryCmd),
    /// Budgets: spent, reserved by running calls, and what's left
    Budget {
        /// The full state as JSON
        #[arg(long)]
        json: bool,
    },
    /// Usage and cost grouped by task, run (team run), agent, provider, model or account
    Usage {
        #[arg(long, default_value = "provider")]
        by: String,
        /// Only the last N days
        #[arg(long)]
        days: Option<u64>,
        #[arg(long)]
        json: bool,
    },
    /// Code intelligence for this repository: symbols, callers, tests, change impact
    #[command(subcommand)]
    Code(CodeCmd),
    /// Interactive terminal UI (default)
    Tui {
        #[arg(short, long)]
        profile: Option<String>,
    },
    /// Web dashboard in your browser
    Ui {
        #[arg(long, default_value_t = 7878)]
        port: u16,
        /// Don't open the browser
        #[arg(long)]
        no_open: bool,
    },
    /// MCP bridge server (started by Claude sessions)
    #[command(hide = true)]
    Mcp {
        #[arg(long)]
        profile: String,
    },
}

#[derive(Subcommand)]
enum CodeCmd {
    /// Index size, languages, call graph and how much code tests reach
    Stats,
    /// Find symbols by name
    Search { query: String },
    /// What a function does, who calls it, what it calls, which tests reach it (name, Type::name or path:line)
    Show { target: String },
    /// A file's outline, imports and importers
    File { path: String },
    /// What your uncommitted changes (or changes since --base) could break, and the tests to run
    Impact {
        #[arg(long)]
        base: Option<String>,
    },
    /// External dependencies and where they're used
    Deps,
}

#[derive(Subcommand)]
enum MemoryCmd {
    /// Current records (private ones included)
    List,
    /// Find records by wording
    Search { query: Vec<String> },
    /// One record with its versions and stale sources
    Show { id: String },
    /// Everything on a topic, with disagreements shown side by side
    Topic { topic: Vec<String> },
    /// Record a decision or note
    Add {
        title: String,
        #[arg(long, default_value = "")]
        body: String,
        /// decision | note | fact
        #[arg(long, default_value = "decision")]
        kind: String,
        #[arg(long, default_value = "")]
        topic: String,
        /// Files it rests on: path or path:line (repeat for several)
        #[arg(long)]
        source: Vec<String>,
        /// Keep it from agents
        #[arg(long)]
        private: bool,
    },
    /// A new version of a record (the old one stays in its history)
    Revise {
        id: String,
        body: String,
        #[arg(long, default_value = "")]
        change: String,
    },
    /// Stop using a record (it stays in the log)
    Retire { id: String },
    /// Record NOTE:/DECISION:/MEMORY: comments from the project's files (untrusted)
    Ingest,
}

#[derive(Subcommand)]
enum SkillsCmd {
    /// Where skills and agents live, and what's in each place
    List,
    /// Write skills and agents (all, or the names given) to a .zip
    Export { out: PathBuf, names: Vec<String> },
    /// Install from a .zip / .skill archive, a folder, or a .md agent
    Import {
        src: PathBuf,
        /// Place ids or account names (see `fermi skills list`); repeat for several
        #[arg(long, required = true)]
        to: Vec<String>,
        /// Replace existing ones with the same name (old copies go to ~/.fermi/trash)
        #[arg(long)]
        replace: bool,
    },
}

#[derive(Subcommand)]
enum AccountCmd {
    /// Add an account to the config
    Add {
        id: String,
        /// claude | openai
        #[arg(long)]
        provider: String,
        /// Use an API key from this env var instead of a CLI login
        #[arg(long)]
        api_key_env: Option<String>,
        /// CLI config dir ("default" = the CLI's normal login)
        #[arg(long)]
        home: Option<String>,
        /// Also add it to this profile (created if missing)
        #[arg(long)]
        profile: Option<String>,
    },
    /// Log in to a CLI account in its own isolated config dir
    Login { id: String },
}

#[tokio::main]
async fn main() -> ExitCode {
    match real_main().await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn real_main() -> Result<ExitCode> {
    let cli = Cli::parse();
    match cli.cmd.unwrap_or(Cmd::Tui { profile: None }) {
        Cmd::Init { force } => setup::init(force)?,
        Cmd::Account(AccountCmd::Add { id, provider, api_key_env, home, profile }) => {
            println!("{}", setup::add_account(&id, &provider, api_key_env.as_deref(), home.as_deref(), profile.as_deref())?)
        }
        Cmd::Account(AccountCmd::Login { id }) => setup::login(&id)?,
        Cmd::Status => setup::status(&*Engine::load()?).await,
        Cmd::Route(t) => route(t)?,
        Cmd::Run(t) => return run(t).await,
        Cmd::History { limit } => history(limit)?,
        Cmd::Show { id } => show(id)?,
        Cmd::Trace { id, json } => trace_cmd(id, json)?,
        Cmd::Replay { id, json } => return replay_cmd(id, json).await,
        Cmd::Import { which, limit } => import(which, limit)?,
        Cmd::Skills(cmd) => skills_cmd(cmd)?,
        Cmd::Code(cmd) => code_cmd(cmd)?,
        Cmd::Verify { check, json, dir } => return verify_cmd(check, json, dir).await,
        Cmd::Budget { json } => budget_cmd(json)?,
        Cmd::Memory(cmd) => memory_cmd(cmd)?,
        Cmd::Usage { by, days, json } => usage_cmd(&by, days, json)?,
        Cmd::Tui { profile } => {
            if !fermi_core::Config::path().exists() {
                setup::init(false)?;
            }
            tui::run(Engine::load()?, profile).await?
        }
        Cmd::Ui { port, no_open } => {
            if !fermi_core::Config::path().exists() {
                setup::init(false)?;
            }
            server::run(Engine::load()?, port, !no_open).await?
        }
        Cmd::Mcp { profile } => fermi_core::mcp::serve(Engine::load()?, profile).await?,
    }
    Ok(ExitCode::SUCCESS)
}

fn task_from(t: Target) -> Result<Task> {
    let mut prompt = t.prompt.join(" ");
    if prompt.trim().is_empty() {
        std::io::stdin().read_to_string(&mut prompt)?;
    }
    anyhow::ensure!(!prompt.trim().is_empty(), "empty prompt");
    let cwd = match t.dir {
        Some(d) => fermi::files::canon(&d).ok_or_else(|| anyhow::anyhow!("folder not found: {}", d.display()))?,
        None => std::env::current_dir()?,
    };
    let mut task = Task::new(prompt, cwd);
    task.profile = t.profile;
    task.model = t.model;
    task.account = t.account;
    task.effort = t.effort;
    task.verify = t.verify.map(|max_repairs| fermi_core::verify::VerifyOpts { max_repairs, checks: vec![] });
    Ok(task)
}

fn memory_cmd(cmd: MemoryCmd) -> Result<()> {
    use fermi_core::memory::{Audience, Caller, Kind, Memory, NewRecord, Provenance};
    let home = fermi_core::Config::home();
    let mem = Memory::open(&home, &std::env::current_dir()?)?;
    let pretty = |v: &serde_json::Value| serde_json::to_string_pretty(v).unwrap_or_default();
    let line = |v: &fermi_core::memory::View| {
        let r = &v.record;
        format!(
            "{}  v{}  {:?}  [{}]  {}{}{}",
            r.id,
            r.version,
            r.kind,
            r.topic,
            r.title,
            if r.audience == Audience::Private { "  (private)" } else { "" },
            if v.stale.is_empty() { String::new() } else { format!("  ⚠ {}", v.stale.join("; ")) }
        )
    };
    match cmd {
        MemoryCmd::List => {
            for v in mem.list(Caller::User) {
                println!("{}", line(&v));
            }
        }
        MemoryCmd::Search { query } => {
            for h in mem.search(Caller::User, &query.join(" "), 10) {
                println!("{:>6.2}  {}", h.score, line(&h.view));
            }
        }
        MemoryCmd::Show { id } => println!("{}", pretty(&serde_json::to_value(mem.get(Caller::User, &id)?)?)),
        MemoryCmd::Topic { topic } => println!("{}", mem.topic(Caller::User, &topic.join(" "))["summary"].as_str().unwrap_or("")),
        MemoryCmd::Add { title, body, kind, topic, source, private } => {
            let kind = match kind.as_str() {
                "note" => Kind::Note,
                "fact" => Kind::Fact,
                _ => Kind::Decision,
            };
            let audience = if private { Audience::Private } else { Audience::Agents };
            let v = mem.record(NewRecord { kind, topic, title, body, audience, sources: source }, "user", Provenance::User)?;
            println!("{}", line(&v));
        }
        MemoryCmd::Revise { id, body, change } => println!("{}", line(&mem.revise(Caller::User, &id, None, body, &change, "user")?)),
        MemoryCmd::Retire { id } => mem.retire(Caller::User, &id, "user")?,
        MemoryCmd::Ingest => {
            let deny = fermi_core::Config::load().map(|c| c.security.deny_read).unwrap_or_default();
            println!("{} comment(s) recorded", mem.ingest_comments(&deny)?);
        }
    }
    Ok(())
}

fn trace_cmd(id: u64, as_json: bool) -> Result<()> {
    let dir = fermi_core::hub::history_dir(&fermi_core::Config::home());
    let t = fermi_core::hub::read_task(&dir, id).ok_or_else(|| anyhow::anyhow!("no task {id} (see `fermi history`)"))?;
    let tr = fermi_core::trace::export(&t);
    if as_json {
        println!("{}", serde_json::to_string_pretty(&tr)?);
        return Ok(());
    }
    println!("task {id} · {:?} · {} attempt(s) · {} events", t.status, tr["attempts"], tr["events"]);
    for o in tr["operations"].as_array().into_iter().flatten() {
        let at = format!("#{:<4} a{} t{}", o["seq"], o["attempt"], o["try"]);
        if o["kind"] == "model_call" {
            let u = &o["usage"];
            let n = |k: &str| u[k].as_u64().map(|v| v.to_string()).unwrap_or_else(|| "?".into());
            println!("{at}  call {}  {} in / {} out", o["id"].as_str().unwrap_or(""), n("input_tokens"), n("output_tokens"));
        } else {
            let ms = o["ms"].as_u64().map(|m| format!(" {m} ms")).unwrap_or_default();
            let code = o["code"].as_str().map(|c| format!(" ({c})")).unwrap_or_default();
            println!(
                "{at}    {} {} {}  → {}{code}{ms}",
                o["id"].as_str().unwrap_or(""),
                o["name"].as_str().unwrap_or(""),
                o["detail"].as_str().unwrap_or(""),
                o["status"].as_str().unwrap_or("")
            );
        }
    }
    Ok(())
}

async fn replay_cmd(id: u64, as_json: bool) -> Result<ExitCode> {
    let engine = Engine::load()?;
    let scratch = std::env::temp_dir().join(format!("fermi-replay-{id}-{}", std::process::id()));
    let r = fermi_core::trace::replay(&engine.cfg, &engine.home, id, &scratch).await;
    let _ = std::fs::remove_dir_all(&scratch);
    let r = r?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&r)?);
    } else {
        println!("task {id}: replayed attempts {:?} from {} recording(s), {} events compared", r.attempts, r.tapes.len(), r.compared);
        if r.matches {
            println!("✓ the replay matches: same events and state ({})", &r.replay_hash[..12]);
        } else {
            println!("✗ the replay differs:");
            for d in &r.differences {
                println!("  {d}");
            }
        }
        println!("  (the text comes from the recordings; a live model wouldn't reproduce it word for word)");
    }
    Ok(if r.matches { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

fn budget_cmd(as_json: bool) -> Result<()> {
    use fermi_core::billing::fmt_usd;
    let engine = Engine::load()?;
    let states: Vec<_> = engine.cfg.budgets.iter().map(|b| engine.ledger.state(b, fermi_core::util::now())).collect();
    if as_json {
        println!("{}", serde_json::to_string_pretty(&states)?);
        return Ok(());
    }
    if states.is_empty() {
        println!("No budgets. Add [[budgets]] to {} and `budget = \"name\"` to a profile.", fermi_core::Config::path().display());
    }
    for s in &states {
        let users: Vec<&str> =
            engine.cfg.profiles.iter().filter(|p| p.budget.as_deref() == Some(s.name.as_str())).map(|p| p.name.as_str()).collect();
        println!(
            "{:<12} {} of {} left ({:?}) · {} spent · {} reserved by {} running call(s){}{} · profiles: {}",
            s.name,
            fmt_usd(s.remaining),
            fmt_usd(s.limit),
            s.period,
            fmt_usd(s.spent),
            fmt_usd(s.reserved),
            s.open,
            if s.overrun > 0 { format!(" · {} over estimates", fmt_usd(s.overrun)) } else { String::new() },
            if s.denied > 0 { format!(" · {} refused", s.denied) } else { String::new() },
            if users.is_empty() { "none".to_string() } else { users.join(", ") }
        );
    }
    Ok(())
}

fn usage_cmd(by: &str, days: Option<u64>, as_json: bool) -> Result<()> {
    use fermi_core::billing::fmt_usd;
    let since = days.map(|d| fermi_core::util::now().saturating_sub(d * 86_400)).unwrap_or(0);
    let r = fermi_core::billing::report(&fermi_core::Config::home(), by, since)?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&r)?);
        return Ok(());
    }
    println!("{:<28} {:>6} {:>7} {:>12} {:>12} {:>12}", by, "calls", "failed", "input", "output", "cost");
    let line = |name: &str, g: &serde_json::Value| {
        let n = |k: &str| g[k].as_i64().unwrap_or(0);
        let unpriced = if n("unpriced_calls") > 0 { format!(" (+{} unpriced)", n("unpriced_calls")) } else { String::new() };
        println!(
            "{name:<28} {:>6} {:>7} {:>12} {:>12} {:>12}{unpriced}",
            n("calls"),
            n("failed_calls"),
            n("input_tokens"),
            n("output_tokens"),
            fmt_usd(n("cost_micros"))
        );
    };
    for (k, g) in r["groups"].as_object().into_iter().flatten() {
        line(k, g);
    }
    line("total", &r["total"]);
    if r["reconciled"] != true {
        eprintln!("warning: the groups don't add up to the total");
    }
    Ok(())
}

async fn verify_cmd(only: Vec<String>, as_json: bool, dir: Option<PathBuf>) -> Result<ExitCode> {
    use fermi_core::verify::{run, text, RunOpts};
    let dir = match dir {
        Some(d) => d,
        None => std::env::current_dir()?,
    };
    let report = run(&dir, RunOpts { only: &only, ..Default::default() }).await?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", text(&report));
    }
    Ok(if report.passed { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

fn route(t: Target) -> Result<()> {
    let engine = Engine::load()?;
    let task = task_from(t)?;
    let plan = engine.plan(&task)?;
    println!("profile  {} ({:?})", plan.profile.name, plan.profile.mode);
    println!("need     {:?} tier {} — {}", plan.need.cap, plan.need.tier, plan.need.why);
    println!("try      {}", plan.models.iter().map(|m| m.name()).collect::<Vec<_>>().join(" → "));
    let effort = task.effort.clone().or(plan.profile.effort.clone());
    println!("effort   {}", effort.as_deref().unwrap_or("model default"));
    for m in &plan.models {
        let Some(p) = m.provider() else { continue };
        let accts: Vec<_> = match &task.account {
            Some(id) => vec![id.clone()],
            None => engine.cfg.profile_accounts(&plan.profile).filter(|a| a.provider == p).map(|a| a.id.clone()).collect(),
        };
        println!("  {:<22} {:<20} accounts: {}", m.name(), m.id, accts.join(", "));
    }
    Ok(())
}

async fn run(t: Target) -> Result<ExitCode> {
    let engine = Engine::load()?;
    let mut task = task_from(t)?;
    let color = std::io::stderr().is_terminal();
    let paint = |code: &str, s: String| if color { format!("\x1b[{code}m{s}\x1b[0m") } else { s };

    // Saved to the shared history, so the desktop app and dashboard show it too.
    let mut history = fermi_core::hub::Recorder::start(&engine, &mut task, "cli");
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let job = tokio::spawn({
        let engine = engine.clone();
        async move {
            match task.verify.clone() {
                Some(v) => fermi_core::verify::run_task(&engine, task, &v, &tx).await,
                None => {
                    let _ = engine.run(&task, &tx).await;
                }
            }
        }
    });

    let mut ok = false;
    // Text already printed live from deltas, so the finished message isn't repeated.
    let mut streamed = false;
    let mut beat = tokio::time::interval(fermi_core::hub::HEARTBEAT);
    loop {
        let ev = tokio::select! {
            ev = rx.recv() => match ev { Some(ev) => ev, None => break },
            _ = beat.tick() => { history.heartbeat(); continue; }
        };
        history.record(&ev);
        if streamed && !matches!(ev, Event::Delta { .. } | Event::Text { .. }) {
            println!();
            streamed = false;
        }
        match ev {
            Event::Routed { profile, label, effort, account, why, .. } => {
                let effort = effort.map(|e| format!(" · effort {e}")).unwrap_or_default();
                eprintln!("{}", paint("33", format!("▸ {label}{effort} @ {account}  [{profile}]  {why}")))
            }
            Event::User { .. } | Event::Session { .. } => {}
            Event::Delta { text } => {
                print!("{text}");
                let _ = std::io::Write::flush(&mut std::io::stdout());
                streamed = true;
            }
            Event::Text { text } if streamed => {
                if !text.ends_with('\n') {
                    println!();
                }
                streamed = false;
            }
            Event::Text { text } => println!("{text}"),
            Event::Tool { name, detail, .. } => eprintln!("{}", paint("36", format!("  · {name} {detail}"))),
            Event::ToolResult { status, output, code, .. } if status != "ok" => {
                let first = output.lines().next().unwrap_or_default();
                eprintln!("{}", paint("31", format!("    ✗ {status}{} {first}", code.map(|c| format!(" ({c})")).unwrap_or_default())))
            }
            Event::ToolResult { .. } | Event::Call { .. } => {}
            Event::Limit { account, utilization, blocked, .. } => {
                if blocked {
                    eprintln!("{}", paint("31", format!("  ! {account} is rate limited")));
                } else if let Some(u) = utilization.filter(|u| *u >= 0.8) {
                    eprintln!("{}", paint("33", format!("  ! {account} at {:.0}% of its window", u * 100.0)));
                }
            }
            Event::Failover { account, reason } => eprintln!("{}", paint("31", format!("  ↷ {account}: {reason} — trying next"))),
            Event::Blocked { tool, detail, .. } => eprintln!("{}", paint("33", format!("  ⚠ needed approval: {tool} {detail}"))),
            Event::Artifact { path } => eprintln!("{}", paint("35", format!("  ⇒ {path}"))),
            Event::Log { line } => eprintln!("{}", paint("2", format!("  {line}"))),
            Event::Estimate { estimate } => {
                if let Some(w) = estimate["warning"].as_str() {
                    eprintln!("{}", paint("33", format!("  ! {w}")));
                } else if let Some(m) = estimate["micros"].as_i64() {
                    let (i, o) = (estimate["input_tokens"].as_u64().unwrap_or(0), estimate["output_tokens"].as_u64().unwrap_or(0));
                    eprintln!("{}", paint("2", format!("  ≈ {i} in / {o} out tokens, about {}", fermi_core::billing::fmt_usd(m))));
                }
            }
            Event::Budget { action, note, .. } => {
                eprintln!("{}", paint(if action == "denied" { "31" } else { "2" }, format!("  $ {note}")))
            }
            Event::Decision { decision } => {
                eprintln!(
                    "{}",
                    paint(
                        "33",
                        format!(
                            "▸ {} policy picked {} ({})",
                            decision["policy"].as_str().unwrap_or(""),
                            decision["selected"].as_str().unwrap_or(""),
                            decision["rule"].as_str().unwrap_or("")
                        )
                    )
                )
            }
            Event::Verify { passed, report, .. } => {
                let summary = serde_json::from_value::<fermi_core::verify::Report>(report).map(|r| r.summary()).unwrap_or_default();
                eprintln!("{}", paint(if passed { "32" } else { "31" }, format!("  {} {summary}", if passed { "✓" } else { "✗" })));
            }
            Event::Repair { attempt, max, .. } => {
                eprintln!("{}", paint("33", format!("  ↻ repair {attempt} of {max}: the failures go back to the agent")))
            }
            Event::Done { ok: success, summary } => {
                ok = success;
                eprintln!("{}", paint(if success { "32" } else { "31" }, format!("{} {summary}", if success { "✓" } else { "✗" })));
            }
        }
    }
    let _ = job.await;
    eprintln!("{}", paint("2", format!("  saved as task {} (fermi show {})", history.id(), history.id())));
    Ok(if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

/// `fermi history`: recent tasks from every front end on this PC.
fn code_cmd(cmd: CodeCmd) -> Result<()> {
    use fermi_core::code::{self, text, Index};
    let root = code::repo_root(&std::env::current_dir()?);
    let started = std::time::Instant::now();
    let idx = Index::open(&root)?;
    let pretty = |v: &serde_json::Value| serde_json::to_string_pretty(v).unwrap_or_default();
    match cmd {
        CodeCmd::Stats => {
            println!("{}", pretty(&idx.stats()));
            eprintln!("(indexed in {} ms)", started.elapsed().as_millis());
        }
        CodeCmd::Search { query } => println!("{}", text::search(&idx.search(&query, 40))),
        CodeCmd::Show { target } => println!("{}", text::symbol(&idx.symbol(&target))),
        CodeCmd::File { path } => println!("{}", text::file(&idx.file(&path))),
        CodeCmd::Impact { base } => println!("{}", text::impact(&idx.impact(&idx.git_changes(base.as_deref())?))),
        CodeCmd::Deps => println!("{}", pretty(&idx.deps())),
    }
    Ok(())
}

fn skills_cmd(cmd: SkillsCmd) -> Result<()> {
    use fermi::skills::{self, Kind};
    let engine = Engine::load()?;
    let places = skills::places(&engine.cfg, &[std::env::current_dir()?]);
    let kind_name = |k: Kind| if k == Kind::Skill { "skill" } else { "agent" };
    match cmd {
        SkillsCmd::List => {
            for p in &places {
                let items = skills::scan(p);
                if items.is_empty() && p.scope == "project" {
                    continue;
                }
                println!("{}  [{}]", p.label, p.id);
                for i in &items {
                    let d: String = i.description.chars().take(70).collect();
                    println!("    {:<6} {:<28} {d}", kind_name(i.kind), i.name);
                }
                if items.is_empty() {
                    println!("    (none)");
                }
            }
        }
        SkillsCmd::Export { out, names } => {
            let mut picked = vec![];
            let mut seen = std::collections::HashSet::new();
            for p in &places {
                for i in skills::scan(p) {
                    if (names.is_empty() || names.contains(&i.name)) && seen.insert((i.kind, i.name.clone())) {
                        picked.push((p, i.kind, i.name));
                    }
                }
            }
            anyhow::ensure!(!picked.is_empty(), "nothing to export");
            let n = skills::export(&picked, &out)?;
            println!("exported {n} to {}", out.display());
        }
        SkillsCmd::Import { src, to, replace } => {
            let targets: Vec<&skills::Place> = to
                .iter()
                .map(|t| {
                    places
                        .iter()
                        .find(|p| &p.id == t || p.label.split(" · ").next() == Some(t.as_str()))
                        .ok_or_else(|| anyhow::anyhow!("unknown place `{t}` (see `fermi skills list`)"))
                })
                .collect::<Result<_>>()?;
            let dir = engine.home.join("staging").join(format!("cli-{}", std::process::id()));
            let staged = skills::stage(&src, &dir, "cli");
            let res = staged.map(|from| {
                for i in skills::scan(&from) {
                    for t in &targets {
                        if i.kind == Kind::Agent && t.agents.is_none() {
                            continue;
                        }
                        match skills::copy(&from, i.kind, &i.name, t, replace) {
                            Ok(note) => println!(
                                "✓ {} {} → {}{}",
                                kind_name(i.kind),
                                i.name,
                                t.label,
                                note.map(|n| format!(" ({n})")).unwrap_or_default()
                            ),
                            Err(e) => println!("✗ {} {} → {}: {e:#}", kind_name(i.kind), i.name, t.label),
                        }
                    }
                }
            });
            let _ = std::fs::remove_dir_all(&dir);
            res?;
        }
    }
    Ok(())
}

fn import(which: Vec<String>, limit: usize) -> Result<()> {
    let engine = Engine::load()?;
    let found = fermi_core::sessions::discover(&engine.cfg);
    let now = fermi_core::util::now();
    if which.is_empty() {
        // Sessions fermi ran itself, or imported already.
        let known: std::collections::HashSet<String> =
            fermi_core::hub::load_history(&fermi_core::hub::history_dir(&engine.home), usize::MAX)
                .into_iter()
                .flat_map(|t| {
                    let origin = t.origin.and_then(|o| o.split_once(':').map(|x| x.1.to_string()));
                    t.session.into_iter().chain(t.past_sessions).chain(origin)
                })
                .collect();
        if found.is_empty() {
            println!("no Claude Code, Codex or Gemini CLI sessions found");
        }
        for (i, f) in found.iter().take(limit).enumerate() {
            let place = f.cwd.as_ref().and_then(|c| c.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let title: String = f.title.chars().take(60).collect();
            println!(
                "{:>3}  {:>6} ago  {:<11} {:<20} {:<9} {title}",
                i + 1,
                fermi_core::util::fmt_dur(now.saturating_sub(f.updated)),
                f.source.name(),
                place.chars().take(20).collect::<String>(),
                if known.contains(&f.session) { "in fermi" } else { "" },
            );
        }
        println!(
            "
import with: fermi import <number or session id> ..."
        );
        return Ok(());
    }
    let dir = fermi_core::hub::history_dir(&engine.home);
    std::fs::create_dir_all(&dir)?;
    let hub = fermi_core::hub::Hub::new(engine, "cli");
    for w in &which {
        let pick = match w.parse::<usize>() {
            Ok(n) if (1..=found.len()).contains(&n) => found.get(n - 1),
            _ => found.iter().find(|f| f.session.starts_with(w.as_str())),
        };
        match pick {
            Some(f) => match hub.import(f) {
                Ok(id) => println!("✓ {} → task {id}  ({})", f.title.chars().take(60).collect::<String>(), f.source.name()),
                Err(e) => println!("✗ {}: {e:#}", f.title.chars().take(60).collect::<String>()),
            },
            None => println!("✗ {w}: no such session (run `fermi import` to list them)"),
        }
    }
    Ok(())
}

fn history(limit: usize) -> Result<()> {
    let home = fermi_core::Config::home();
    let tasks = fermi_core::hub::load_history(&fermi_core::hub::history_dir(&home), limit);
    if tasks.is_empty() {
        println!("no tasks yet");
    }
    let now = fermi_core::util::now();
    for t in tasks.iter().rev() {
        let glyph = match t.status {
            fermi_core::hub::TaskStatus::Running => "◌",
            fermi_core::hub::TaskStatus::Done => "✓",
            fermi_core::hub::TaskStatus::Failed => "✗",
            fermi_core::hub::TaskStatus::Cancelled => "⊘",
        };
        let title: String = t.prompt.lines().next().unwrap_or("").chars().take(70).collect();
        println!(
            "{glyph} {:<17} {:>6} ago  {:<8} {:<40}  {title}",
            t.id,
            fermi_core::util::fmt_dur(now.saturating_sub(t.started)),
            t.via.as_deref().unwrap_or("-"),
            t.route.as_deref().unwrap_or("-").chars().take(40).collect::<String>(),
        );
    }
    Ok(())
}

/// `fermi show <id>`: one conversation, as text.
fn show(id: u64) -> Result<()> {
    let dir = fermi_core::hub::history_dir(&fermi_core::Config::home());
    let t = fermi_core::hub::read_task(&dir, id).ok_or_else(|| anyhow::anyhow!("no task {id} (see `fermi history`)"))?;
    println!("task {id} · {:?} · {} · {}", t.status, t.route.as_deref().unwrap_or("-"), t.cwd.display());
    for ev in &t.events {
        match ev {
            Event::User { text } => println!("\n> {}\n", text.replace('\n', "\n> ")),
            Event::Text { text } => println!("{text}\n"),
            Event::Tool { name, detail, .. } => println!("  · {name} {detail}"),
            Event::Artifact { path } => println!("  ⇒ {path}"),
            Event::Failover { account, reason } => println!("  ↷ {account}: {reason}"),
            Event::Done { ok, summary } => println!("{} {summary}", if *ok { "✓" } else { "✗" }),
            Event::Log { line } if line.starts_with("interrupted") => println!("  {line}"),
            _ => {}
        }
    }
    Ok(())
}
