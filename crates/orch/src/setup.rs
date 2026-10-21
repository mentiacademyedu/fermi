//! `init`, `account add/login`, `status`.

use anyhow::{bail, Context, Result};
use orch_core::util::{fmt_dur, fmt_tokens, now, resolve_bin};
use orch_core::{Auth, Config, Engine, Provider};
use toml_edit::{value, Array, ArrayOfTables, DocumentMut, Item, Table};

const TEMPLATE: &str = include_str!("../config.example.toml");

pub fn init(force: bool) -> Result<()> {
    let path = Config::path();
    if path.exists() && !force {
        bail!("{} already exists (use --force to overwrite)", path.display());
    }
    std::fs::create_dir_all(Config::home())?;
    std::fs::write(&path, TEMPLATE)?;
    println!("wrote {}", path.display());
    println!("It starts with one account using your existing `claude` login. To add more:");
    println!("  orch account add claude-work --provider claude --profile work");
    println!("  orch account login claude-work");
    println!("  orch status");
    if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        println!("\npress enter to continue…");
        let _ = std::io::stdin().read_line(&mut String::new());
    }
    Ok(())
}

pub fn add_account(id: &str, provider: &str, key_env: Option<&str>, home: Option<&str>, profile: Option<&str>) -> Result<()> {
    let provider = Provider::parse(provider).with_context(|| format!("unknown provider `{provider}` (claude | openai)"))?;
    let path = Config::path();
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {} (run `orch init`)", path.display()))?;
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
    println!("added `{id}` ({})", provider.as_str());
    if key_env.is_none() {
        println!("log in with: orch account login {id}");
    }
    Ok(())
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
    let (bin, args, var): (_, &[&str], _) = match acct.provider {
        Provider::Claude => (resolve_bin("claude", cfg.bins.claude.as_deref())?, &["auth", "login"], "CLAUDE_CONFIG_DIR"),
        Provider::Openai => (resolve_bin("codex", cfg.bins.codex.as_deref())?, &["login"], "CODEX_HOME"),
    };
    let mut cmd = std::process::Command::new(bin);
    cmd.args(args);
    if let Some(home) = acct.home_dir() {
        std::fs::create_dir_all(&home)?;
        println!("logging `{id}` in with {var}={}", home.display());
        cmd.env(var, home);
    }
    let status = cmd.status()?;
    if !status.success() {
        bail!("login exited with {status}");
    }
    Ok(())
}

pub fn status(engine: &Engine) {
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
    println!("   {:<18} {:<7} {:<4} {:<22} {:>6} {:>6} {:>9}", "ID", "PROV", "AUTH", "STATUS", "WINDOW", "TASKS", "TOKENS");
    let snap = engine.store.snapshot();
    let t = now();
    for a in &cfg.accounts {
        let s = snap.get(&a.id).cloned().unwrap_or_default();
        let status = if s.limited(t) {
            format!("limited, {} left", fmt_dur(s.limited_until.unwrap_or(t) - t))
        } else {
            match a.credentials_present() {
                Some(false) if a.auth == Auth::Api => "key env not set".into(),
                Some(false) => "not logged in".into(),
                _ => "ready".into(),
            }
        };
        let window = s.utilization.map(|u| format!("{:.0}%", u * 100.0)).unwrap_or_else(|| "-".into());
        println!(
            "   {:<18} {:<7} {:<4} {:<22} {:>6} {:>6} {:>9}",
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
