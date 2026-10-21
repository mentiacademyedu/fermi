use crate::models::{self, ModelSpec};
use crate::util::{expand, norm_path};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Claude,
    Openai,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Claude => "claude",
            Provider::Openai => "openai",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "claude" | "anthropic" => Some(Provider::Claude),
            "openai" | "chatgpt" | "codex" => Some(Provider::Openai),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Auth {
    /// Subscription login through the provider's official CLI.
    #[default]
    Cli,
    /// API key from an environment variable.
    Api,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Account {
    pub id: String,
    pub provider: Provider,
    #[serde(default)]
    pub auth: Auth,
    /// CLI config dir. Unset = `~/.fermi/accounts/<id>`; `"default"` = the CLI's own home.
    #[serde(default)]
    pub home: Option<String>,
    /// Env var holding the API key (auth = "api").
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// Override the API base URL (proxies, compatible endpoints).
    #[serde(default)]
    pub base_url: Option<String>,
}

impl Account {
    /// Config dir to isolate this login, or None to use the CLI default.
    pub fn home_dir(&self) -> Option<PathBuf> {
        match self.home.as_deref() {
            Some("default") => None,
            Some(h) => Some(expand(h)),
            None => Some(Config::home().join("accounts").join(&self.id)),
        }
    }

    /// Best-effort check that a login/key exists. None = can't tell (e.g. macOS keychain).
    pub fn credentials_present(&self) -> Option<bool> {
        if self.auth == Auth::Api {
            let var = self.api_key_env.as_deref()?;
            return Some(std::env::var(var).map(|v| !v.is_empty()).unwrap_or(false));
        }
        let (default_dir, file) = match self.provider {
            Provider::Claude if cfg!(target_os = "macos") => return None,
            Provider::Claude => (".claude", ".credentials.json"),
            Provider::Openai => (".codex", "auth.json"),
        };
        let dir = self.home_dir().or_else(|| dirs::home_dir().map(|h| h.join(default_dir)))?;
        Some(dir.join(file).is_file())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Pick the best model per task among the profile's providers.
    #[default]
    Auto,
    /// Only switch between `models`.
    Pinned,
    /// Always `models[0]`.
    Single,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Profile {
    pub name: String,
    /// Accounts in priority order. Tasks never leave their profile's accounts.
    pub accounts: Vec<String>,
    /// Directories whose tasks use this profile (longest prefix wins).
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub mode: Mode,
    #[serde(default)]
    pub models: Vec<String>,
    /// Spread tasks across same-provider accounts instead of strict priority order.
    #[serde(default)]
    pub rotate: bool,
    /// Passed to `claude --permission-mode`.
    #[serde(default)]
    pub permission_mode: Option<String>,
    /// Passed to `codex exec --sandbox`.
    #[serde(default)]
    pub codex_sandbox: Option<String>,
    /// Default reasoning effort for this profile (low … max). Unset = the CLI's default.
    #[serde(default)]
    pub effort: Option<String>,
    /// Extra arguments for every `claude` / `codex exec` run in this profile.
    #[serde(default)]
    pub claude_args: Vec<String>,
    #[serde(default)]
    pub codex_args: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Bins {
    pub claude: Option<String>,
    pub codex: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Config {
    #[serde(default)]
    pub default_profile: Option<String>,
    #[serde(default)]
    pub accounts: Vec<Account>,
    #[serde(default)]
    pub profiles: Vec<Profile>,
    #[serde(default)]
    pub models: Vec<ModelSpec>,
    #[serde(default)]
    pub bins: Bins,
}

impl Config {
    pub fn home() -> PathBuf {
        match std::env::var_os("FERMI_HOME") {
            Some(h) => PathBuf::from(h),
            None => dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".fermi"),
        }
    }

    pub fn path() -> PathBuf {
        Self::home().join("config.toml")
    }

    pub fn load() -> Result<Self> {
        let p = Self::path();
        let text = std::fs::read_to_string(&p)
            .with_context(|| format!("reading {} (run `fermi init` first)", p.display()))?;
        Self::parse(&text).with_context(|| format!("in {}", p.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let cfg: Config = toml::from_str(text)?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        let mut ids = HashSet::new();
        for a in &self.accounts {
            if !ids.insert(a.id.as_str()) {
                bail!("duplicate account id `{}`", a.id);
            }
            if a.auth == Auth::Api && a.api_key_env.is_none() {
                bail!("account `{}` uses auth = \"api\" but has no api_key_env", a.id);
            }
        }
        let catalog = self.catalog();
        for m in &catalog {
            if m.provider().is_none() {
                bail!("model `{}` must be `claude:<name>` or `openai:<name>`", m.id);
            }
        }
        let mut names = HashSet::new();
        for p in &self.profiles {
            if !names.insert(p.name.as_str()) {
                bail!("duplicate profile `{}`", p.name);
            }
            for id in &p.accounts {
                if !ids.contains(id.as_str()) {
                    bail!("profile `{}` lists unknown account `{id}`", p.name);
                }
            }
            for id in &p.models {
                if !catalog.iter().any(|m| &m.id == id) {
                    bail!("profile `{}` lists unknown model `{id}`", p.name);
                }
            }
            if let Some(e) = p.effort.as_deref().filter(|e| !crate::models::EFFORTS.contains(e)) {
                bail!("profile `{}` has unknown effort `{e}` (use {})", p.name, crate::models::EFFORTS.join(", "));
            }
            if p.mode != Mode::Auto && p.models.is_empty() {
                bail!("profile `{}` is {:?} but lists no models", p.name, p.mode);
            }
        }
        if let Some(d) = &self.default_profile {
            if !names.contains(d.as_str()) {
                bail!("default_profile `{d}` does not exist");
            }
        }
        Ok(())
    }

    pub fn account(&self, id: &str) -> Option<&Account> {
        self.accounts.iter().find(|a| a.id == id)
    }

    /// Built-in models with user `[[models]]` overriding by id or appended.
    pub fn catalog(&self) -> Vec<ModelSpec> {
        let mut out = models::builtin();
        for m in &self.models {
            match out.iter_mut().find(|b| b.id == m.id) {
                Some(b) => *b = m.clone(),
                None => out.push(m.clone()),
            }
        }
        out
    }

    /// Explicit name, else the profile whose `paths` best match `cwd`, else
    /// `default_profile`, else the only profile, else an implicit profile
    /// with every account.
    pub fn resolve_profile(&self, name: Option<&str>, cwd: &Path) -> Result<Profile> {
        if let Some(n) = name {
            return self
                .profiles
                .iter()
                .find(|p| p.name == n)
                .cloned()
                .ok_or_else(|| anyhow!("unknown profile `{n}`"));
        }
        let here = norm_path(cwd);
        let best = self
            .profiles
            .iter()
            .flat_map(|p| p.paths.iter().map(move |path| (p, norm_path(&expand(path)))))
            .filter(|(_, path)| here == *path || here.starts_with(&format!("{path}/")))
            .max_by_key(|(_, path)| path.len());
        if let Some((p, _)) = best {
            return Ok(p.clone());
        }
        if let Some(d) = &self.default_profile {
            return self.resolve_profile(Some(d), cwd);
        }
        if self.profiles.len() == 1 {
            return Ok(self.profiles[0].clone());
        }
        if self.profiles.is_empty() {
            return Ok(Profile {
                name: "all".into(),
                accounts: self.accounts.iter().map(|a| a.id.clone()).collect(),
                paths: vec![],
                mode: Mode::Auto,
                models: vec![],
                rotate: false,
                permission_mode: None,
                codex_sandbox: None,
                effort: None,
                claude_args: vec![],
                codex_args: vec![],
            });
        }
        bail!("no profile matches {} and no default_profile is set (use -p <profile>)", cwd.display())
    }

    pub fn profile_accounts<'a>(&'a self, p: &'a Profile) -> impl Iterator<Item = &'a Account> + 'a {
        p.accounts.iter().filter_map(|id| self.account(id))
    }
}
