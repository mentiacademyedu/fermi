mod server;
mod setup;
mod tui;

use anyhow::Result;
use clap::{Parser, Subcommand};
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
    /// Working directory for the task
    #[arg(short = 'C', long)]
    dir: Option<PathBuf>,
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
    Login {
        id: String,
        /// Wait for Enter at the end (used when the dashboard opens a login window)
        #[arg(long, hide = true)]
        pause: bool,
    },
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
        Cmd::Account(AccountCmd::Login { id, pause }) => {
            let r = setup::login(&id);
            if pause {
                match &r {
                    Ok(()) => println!("\ndone. You can close this window and go back to the dashboard."),
                    Err(e) => println!("\nlogin failed: {e:#}"),
                }
                println!("press Enter to close…");
                let _ = std::io::stdin().read_line(&mut String::new());
            }
            r?
        }
        Cmd::Status => setup::status(&*Engine::load()?).await,
        Cmd::Route(t) => route(t)?,
        Cmd::Run(t) => return run(t).await,
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
        Some(d) => std::fs::canonicalize(d)?,
        None => std::env::current_dir()?,
    };
    let mut task = Task::new(prompt, cwd);
    task.profile = t.profile;
    task.model = t.model;
    task.account = t.account;
    Ok(task)
}

fn route(t: Target) -> Result<()> {
    let engine = Engine::load()?;
    let task = task_from(t)?;
    let plan = engine.plan(&task)?;
    println!("profile  {} ({:?})", plan.profile.name, plan.profile.mode);
    println!("need     {:?} tier {} — {}", plan.need.cap, plan.need.tier, plan.need.why);
    println!("try      {}", plan.models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>().join(" → "));
    for m in &plan.models {
        let Some(p) = m.provider() else { continue };
        let accts: Vec<_> = match &task.account {
            Some(id) => vec![id.clone()],
            None => engine.cfg.profile_accounts(&plan.profile).filter(|a| a.provider == p).map(|a| a.id.clone()).collect(),
        };
        println!("  {:<14} accounts: {}", m.id, accts.join(", "));
    }
    Ok(())
}

async fn run(t: Target) -> Result<ExitCode> {
    let engine = Engine::load()?;
    let task = task_from(t)?;
    let color = std::io::stderr().is_terminal();
    let paint = |code: &str, s: String| if color { format!("\x1b[{code}m{s}\x1b[0m") } else { s };

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let job = tokio::spawn({
        let engine = engine.clone();
        async move { engine.run(&task, &tx).await }
    });

    let mut ok = false;
    while let Some(ev) = rx.recv().await {
        match ev {
            Event::Routed { profile, model, account, why } => {
                eprintln!("{}", paint("33", format!("▸ {model} @ {account}  [{profile}]  {why}")))
            }
            Event::Text { text } => println!("{text}"),
            Event::Tool { name, detail } => eprintln!("{}", paint("36", format!("  · {name} {detail}"))),
            Event::Limit { account, utilization, blocked, .. } => {
                if blocked {
                    eprintln!("{}", paint("31", format!("  ! {account} is rate limited")));
                } else if let Some(u) = utilization.filter(|u| *u >= 0.8) {
                    eprintln!("{}", paint("33", format!("  ! {account} at {:.0}% of its window", u * 100.0)));
                }
            }
            Event::Failover { account, reason } => eprintln!("{}", paint("31", format!("  ↷ {account}: {reason} — trying next"))),
            Event::Artifact { path } => eprintln!("{}", paint("35", format!("  ⇒ {path}"))),
            Event::Log { line } => eprintln!("{}", paint("2", format!("  {line}"))),
            Event::Done { ok: success, summary } => {
                ok = success;
                eprintln!("{}", paint(if success { "32" } else { "31" }, format!("{} {summary}", if success { "✓" } else { "✗" })));
            }
        }
    }
    let _ = job.await;
    Ok(if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}
