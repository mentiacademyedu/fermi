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

// ── editing the config (keeps comments and layout) ───────────────────────────

fn load_doc() -> Result<DocumentMut> {
    let path = Config::path();
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {} (run `fermi init`)", path.display()))?;
    Ok(text.parse()?)
}

/// Validates the edited config before anything touches the disk.
fn save_doc(doc: &DocumentMut) -> Result<()> {
    let out = doc.to_string();
    Config::parse(&out).context("that change would make the config invalid")?;
    std::fs::write(Config::path(), out)?;
    Ok(())
}

fn str_array<S: AsRef<str>>(items: &[S]) -> Item {
    let mut a = Array::new();
    for s in items {
        a.push(s.as_ref());
    }
    value(a)
}

fn valid_name(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A profile as edited in the dashboard.
#[derive(Debug, serde::Deserialize)]
pub struct ProfileEdit {
    /// Current name when editing (None = new profile).
    pub original: Option<String>,
    pub name: String,
    pub accounts: Vec<String>,
    #[serde(default)]
    pub paths: Vec<String>,
    pub mode: String,
    #[serde(default)]
    pub models: Vec<String>,
    pub effort: Option<String>,
    pub permission_mode: Option<String>,
    #[serde(default)]
    pub default: bool,
}

/// Creates or updates a profile. Keys the dashboard doesn't edit (sandbox,
/// extra CLI args, …) are left as they are.
pub fn save_profile(p: &ProfileEdit) -> Result<String> {
    let mut doc = load_doc()?;
    let msg = apply_profile(&mut doc, p)?;
    save_doc(&doc)?;
    Ok(msg)
}

fn apply_profile(doc: &mut DocumentMut, p: &ProfileEdit) -> Result<String> {
    let name = p.name.trim();
    if !valid_name(name) {
        bail!("profile name must be letters, digits, - or _ (e.g. work)");
    }
    let profiles = doc.entry("profiles").or_insert(Item::ArrayOfTables(ArrayOfTables::new()));
    let Some(arr) = profiles.as_array_of_tables_mut() else { bail!("`profiles` is not [[profiles]]") };
    let find = |arr: &ArrayOfTables, n: &str| arr.iter().position(|t| t.get("name").and_then(|v| v.as_str()) == Some(n));

    let original = p.original.as_deref().filter(|o| !o.is_empty());
    if original != Some(name) && find(arr, name).is_some() {
        bail!("a profile named `{name}` already exists");
    }
    let idx = match original.and_then(|o| find(arr, o)) {
        Some(i) => i,
        None if original.is_some() => bail!("profile `{}` no longer exists", original.unwrap_or_default()),
        None => {
            arr.push(Table::new());
            arr.len() - 1
        }
    };
    let t = arr.get_mut(idx).expect("index is valid");
    t["name"] = value(name);
    t["accounts"] = str_array(&p.accounts);
    let paths: Vec<&str> = p.paths.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    t["paths"] = str_array(&paths);
    t["mode"] = value(p.mode.as_str());
    let set_or_remove = |t: &mut Table, key: &str, v: Option<Item>| match v {
        Some(v) => {
            t[key] = v;
        }
        None => {
            t.remove(key);
        }
    };
    set_or_remove(t, "models", (!p.models.is_empty()).then(|| str_array(&p.models)));
    set_or_remove(t, "effort", p.effort.as_deref().filter(|e| !e.is_empty()).map(value));
    set_or_remove(t, "permission_mode", p.permission_mode.as_deref().filter(|e| !e.is_empty()).map(value));

    let current_default = doc.get("default_profile").and_then(|v| v.as_str()).map(str::to_string);
    if p.default {
        doc["default_profile"] = value(name);
    } else if current_default.as_deref().is_some_and(|d| Some(d) == original || d == name) {
        doc.remove("default_profile");
    }
    Ok(match original {
        Some(o) if o != name => format!("renamed profile `{o}` to `{name}`"),
        Some(_) => format!("saved profile `{name}`"),
        None => format!("created profile `{name}`"),
    })
}

pub fn delete_profile(name: &str) -> Result<String> {
    let mut doc = load_doc()?;
    let Some(arr) = doc.get_mut("profiles").and_then(|i| i.as_array_of_tables_mut()) else { bail!("no profiles") };
    let Some(i) = arr.iter().position(|t| t.get("name").and_then(|v| v.as_str()) == Some(name)) else { bail!("no profile named `{name}`") };
    arr.remove(i);
    if doc.get("default_profile").and_then(|v| v.as_str()) == Some(name) {
        doc.remove("default_profile");
    }
    save_doc(&doc)?;
    Ok(format!("deleted profile `{name}`"))
}

/// Removes an account and takes it out of every profile. Its login folder is
/// left on disk, so adding it back under the same name needs no new login.
pub fn remove_account(id: &str) -> Result<String> {
    let mut doc = load_doc()?;
    let Some(arr) = doc.get_mut("accounts").and_then(|i| i.as_array_of_tables_mut()) else { bail!("no accounts") };
    let Some(i) = arr.iter().position(|t| t.get("id").and_then(|v| v.as_str()) == Some(id)) else { bail!("no account named `{id}`") };
    arr.remove(i);
    if let Some(profiles) = doc.get_mut("profiles").and_then(|i| i.as_array_of_tables_mut()) {
        for t in profiles.iter_mut() {
            if let Some(a) = t.get_mut("accounts").and_then(|v| v.as_array_mut()) {
                a.retain(|v| v.as_str() != Some(id));
            }
        }
    }
    save_doc(&doc)?;
    Ok(format!("removed account `{id}`"))
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
    println!("   {:<18} {:<7} {:<4} {:<22} {:>6} {:>6} {:>9}  LOGGED IN AS", "ID", "PROV", "AUTH", "STATUS", "WINDOW", "TASKS", "TOKENS");
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

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(name: &str, original: Option<&str>, default: bool) -> ProfileEdit {
        ProfileEdit {
            original: original.map(Into::into),
            name: name.into(),
            accounts: vec!["claude-personal".into()],
            paths: vec!["C:/work".into()],
            mode: "auto".into(),
            models: vec![],
            effort: Some("high".into()),
            permission_mode: None,
            default,
        }
    }

    #[test]
    fn profile_edits_keep_config_valid() {
        let mut doc: DocumentMut = TEMPLATE.parse().unwrap();
        apply_profile(&mut doc, &edit("work", None, false)).unwrap();
        apply_profile(&mut doc, &edit("acme", Some("work"), true)).unwrap();
        let text = doc.to_string();
        let cfg = Config::parse(&text).unwrap();
        assert_eq!(cfg.default_profile.as_deref(), Some("acme"), "default must stay a top-level key:\n{text}");
        let acme = cfg.profiles.iter().find(|p| p.name == "acme").unwrap();
        assert_eq!(acme.effort.as_deref(), Some("high"));
        assert!(cfg.profiles.iter().all(|p| p.name != "work"));
        // Comments survive.
        assert!(text.contains("# ── Accounts"));
    }

    #[test]
    fn default_added_back_stays_top_level() {
        let mut doc: DocumentMut = TEMPLATE.parse().unwrap();
        doc.remove("default_profile");
        apply_profile(&mut doc, &edit("work", None, true)).unwrap();
        let text = doc.to_string();
        let cfg = Config::parse(&text).unwrap();
        assert_eq!(cfg.default_profile.as_deref(), Some("work"), "default must stay a top-level key:\n{text}");
    }
}
