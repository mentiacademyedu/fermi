//! `init`, `account add/login`, `status`.

use anyhow::{bail, Context, Result};
use fermi_core::util::{fmt_dur, fmt_tokens, now, resolve_bin};
use fermi_core::{Auth, Config, Engine, Provider};
use toml_edit::{value, Array, ArrayOfTables, DocumentMut, Item, Table};

const TEMPLATE: &str = include_str!("../config.example.toml");

/// Writes the starter config if there is none yet. Returns true if it did.
pub fn ensure_config() -> Result<bool> {
    let path = Config::path();
    if path.exists() {
        return Ok(false);
    }
    std::fs::create_dir_all(Config::home())?;
    std::fs::write(&path, TEMPLATE)?;
    Ok(true)
}

pub fn init(force: bool) -> Result<()> {
    let path = Config::path();
    if path.exists() && !force {
        bail!("{} already exists (use --force to overwrite)", path.display());
    }
    std::fs::create_dir_all(Config::home())?;
    std::fs::write(&path, TEMPLATE)?;
    println!("wrote {}", path.display());
    println!("It starts with one account using your existing `claude` login. To add more:");
    println!("  fermi account add claude-work --provider claude --profile work");
    println!("  fermi account login claude-work");
    println!("  fermi status");
    if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        println!("\npress enter to continue…");
        let _ = std::io::stdin().read_line(&mut String::new());
    }
    Ok(())
}

/// Adds an account (and optionally puts it in a profile). Returns a message
/// for the user instead of printing, so the TUI can use it too.
pub fn add_account(id: &str, provider: &str, key_env: Option<&str>, home: Option<&str>, profile: Option<&str>) -> Result<String> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        bail!("account id must be letters, digits, - or _ (e.g. claude-work)");
    }
    let provider = Provider::parse(provider).with_context(|| format!("unknown provider `{provider}` (claude | openai)"))?;
    let path = Config::path();
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {} (run `fermi init`)", path.display()))?;
    let mut doc: DocumentMut = text.parse()?;

    let mut t = Table::new();
    t["id"] = value(id);
    t["provider"] = value(provider.as_str());
    t["auth"] = value(if key_env.is_some() { "api" } else { "cli" });
    if let Some(k) = key_env {
        t["api_key_env"] = value(k);
    }
    if let Some(h) = home {
        t["home"] = value(h);
    }
    push_table(&mut doc, "accounts", t);

    if let Some(p) = profile {
        let profiles = doc.entry("profiles").or_insert(Item::ArrayOfTables(ArrayOfTables::new()));
        let Some(arr) = profiles.as_array_of_tables_mut() else { bail!("`profiles` is not [[profiles]]") };
        let existing = arr.iter().position(|t| t.get("name").and_then(|n| n.as_str()) == Some(p));
        match existing {
            Some(i) => {
                let t = arr.get_mut(i).expect("index from position");
                let accts = t.entry("accounts").or_insert(value(Array::new()));
                accts.as_array_mut().context("profile accounts is not an array")?.push(id);
            }
            None => {
                let mut t = Table::new();
                t["name"] = value(p);
                let mut a = Array::new();
                a.push(id);
                t["accounts"] = value(a);
                t["paths"] = value(Array::new());
                t["mode"] = value("auto");
                arr.push(t);
            }
        }
    }

    let out = doc.to_string();
    Config::parse(&out).context("the result would be an invalid config")?;
    std::fs::write(&path, out)?;
    Ok(match key_env {
        Some(k) => format!("added `{id}` ({}); it reads its key from {k}", provider.as_str()),
        None => format!("added `{id}` ({}); log in with: fermi account login {id}", provider.as_str()),
    })
}

fn push_table(doc: &mut DocumentMut, key: &str, t: Table) {
    match doc.get_mut(key).and_then(|i| i.as_array_of_tables_mut()) {
        Some(arr) => arr.push(t),
        None => {
            let mut arr = ArrayOfTables::new();
            arr.push(t);
            doc[key] = Item::ArrayOfTables(arr);
        }
    }
}

pub fn login(id: &str) -> Result<()> {
    let cfg = Config::load()?;
    let acct = cfg.account(id).with_context(|| format!("unknown account `{id}`"))?;
    if acct.auth == Auth::Api {
        println!("`{id}` uses an API key: set {} in your environment.", acct.api_key_env.as_deref().unwrap_or("?"));
        return Ok(());
    }
    match acct.home_dir() {
        Some(home) => println!("logging `{id}` in (its login lives in {})", home.display()),
        None => println!("logging `{id}` in (the CLI's default login)"),
    }
    if acct.provider == Provider::Gemini {
        println!("Sign in with Google in the browser. When Gemini's prompt appears, type /quit to finish.\n");
    }
    let status = login_command(&cfg, acct)?.status()?;
    if !status.success() {
        bail!("login exited with {status}");
    }
    Ok(())
}

/// The provider CLI's own interactive login, pointed at the account's
/// isolated config dir.
pub fn login_command(cfg: &Config, acct: &fermi_core::Account) -> Result<std::process::Command> {
    let (bin, args, var): (_, &[&str], _) = match acct.provider {
        Provider::Claude => (resolve_bin("claude", cfg.bins.claude.as_deref())?, &["auth", "login"], "CLAUDE_CONFIG_DIR"),
        Provider::Openai => (resolve_bin("codex", cfg.bins.codex.as_deref())?, &["login"], "CODEX_HOME"),
        // Gemini has no login subcommand: its interactive start signs in
        // (Google is pre-selected), then the window can be closed.
        Provider::Gemini => {
            fermi_core::providers::gemini_cli::prepare_login(acct.home_dir().as_deref())?;
            (resolve_bin("gemini", cfg.bins.gemini.as_deref())?, &[], "GEMINI_CLI_HOME")
        }
    };
    let mut cmd = std::process::Command::new(bin);
    cmd.args(args);
    if let Some(home) = acct.home_dir() {
        std::fs::create_dir_all(&home)?;
        cmd.env(var, home);
    }
    Ok(cmd)
}

pub async fn status(engine: &Engine) {
    let probes: Vec<_> = engine
        .cfg
        .accounts
        .iter()
        .map(|a| {
            let (cfg, a) = (engine.cfg.clone(), a.clone());
            tokio::spawn(async move { fermi_core::auth::probe(&cfg, &a).await })
        })
        .collect();
    let mut who = vec![];
    for p in probes {
        who.push(p.await.unwrap_or_default());
    }
    let cfg = &engine.cfg;
    let cwd = std::env::current_dir().unwrap_or_default();
    let here = cfg.resolve_profile(None, &cwd).ok().map(|p| p.name);
    println!("PROFILES");
    for p in &cfg.profiles {
        let mark = if here.as_deref() == Some(&p.name) { "*" } else { " " };
        let models = if p.models.is_empty() { String::new() } else { format!(" [{}]", p.models.join(", ")) };
        println!(
            " {mark} {:<12} {:<7}{models}  accounts: {}{}",
            p.name,
            format!("{:?}", p.mode).to_lowercase(),
            p.accounts.join(", "),
            if p.paths.is_empty() { String::new() } else { format!("  paths: {}", p.paths.join(", ")) },
        );
    }
    println!("\nACCOUNTS");
    println!(
        "   {:<18} {:<7} {:<4} {:<22} {:>6} {:>6} {:>9}  LOGGED IN AS",
        "ID", "PROV", "AUTH", "STATUS", "WINDOW", "TASKS", "TOKENS"
    );
    let snap = engine.store.snapshot();
    let t = now();
    for (a, info) in cfg.accounts.iter().zip(&who) {
        let s = snap.get(&a.id).cloned().unwrap_or_default();
        let status = if s.limited(t) {
            format!("limited, {} left", fmt_dur(s.limited_until.unwrap_or(t) - t))
        } else {
            match info.logged_in {
                Some(false) if a.auth == Auth::Api => "key env not set".into(),
                Some(false) => "not logged in".into(),
                _ => "ready".into(),
            }
        };
        let identity = match (&info.email, &info.plan) {
            (Some(e), Some(p)) => format!("{e} ({p})"),
            (Some(e), None) => e.clone(),
            (None, Some(p)) => p.clone(),
            _ => String::new(),
        };
        let window = s.utilization.map(|u| format!("{:.0}%", u * 100.0)).unwrap_or_else(|| "-".into());
        println!(
            "   {:<18} {:<7} {:<4} {:<22} {:>6} {:>6} {:>9}  {identity}",
            a.id,
            a.provider.as_str(),
            format!("{:?}", a.auth).to_lowercase(),
            status,
            window,
            s.tasks,
            fmt_tokens(s.input_tokens + s.output_tokens),
        );
    }
}
