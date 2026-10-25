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
    Gemini,
}

impl Provider {
    pub const ALL: [Provider; 3] = [Provider::Claude, Provider::Openai, Provider::Gemini];

    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Claude => "claude",
            Provider::Openai => "openai",
            Provider::Gemini => "gemini",
        }
    }
    pub fn display(self) -> &'static str {
        match self {
            Provider::Claude => "Claude",
            Provider::Openai => "OpenAI (ChatGPT / Codex)",
            Provider::Gemini => "Gemini (Google)",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "claude" | "anthropic" => Some(Provider::Claude),
            "openai" | "chatgpt" | "codex" => Some(Provider::Openai),
            "gemini" | "google" => Some(Provider::Gemini),
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
    /// Override the API base URL (proxies, compatible endpoints). With a
    /// base_url, api_key_env may be left out: a local server needs no key.
    #[serde(default)]
    pub base_url: Option<String>,
    /// What the endpoint speaks, for `openai` API accounts: `responses`
    /// (OpenAI's, the default) or `chat` (chat completions, which local
    /// servers such as llama.cpp, Ollama, vLLM and LM Studio offer).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_format: Option<ApiFormat>,
    /// A plugin's model adapter to run this account's calls instead of the
    /// built-in one: `<plugin>.<adapter>` (see `plugins`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<String>,
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
        if self.adapter.is_some() {
            return None; // the plugin handles its own credentials
        }
        if self.auth == Auth::Api {
            let Some(var) = self.api_key_env.as_deref() else {
                return Some(self.base_url.is_some()); // a local endpoint, no key
            };
            return Some(std::env::var(var).map(|v| !v.is_empty()).unwrap_or(false));
        }
        let (default_dir, file) = match self.provider {
            Provider::Claude if cfg!(target_os = "macos") => return None,
            Provider::Claude => (".claude", ".credentials.json"),
            Provider::Openai => (".codex", "auth.json"),
            // GEMINI_CLI_HOME is a root that holds `.gemini/`, like the user's home.
            Provider::Gemini => {
                let root = self.home_dir().or_else(dirs::home_dir)?;
                return Some(root.join(".gemini").join("oauth_creds.json").is_file());
            }
        };
        let dir = self.home_dir().or_else(|| dirs::home_dir().map(|h| h.join(default_dir)))?;
        Some(dir.join(file).is_file())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiFormat {
    Responses,
    Chat,
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
    #[serde(default)]
    pub gemini_args: Vec<String>,
    /// Spending allowance for this profile's calls (a `[[budgets]]` name).
    #[serde(default)]
    pub budget: Option<String>,
    /// How auto mode orders models: by task fit (auto), cost (economy) or declared quality (quality).
    #[serde(default)]
    pub routing: Routing,
    /// Run this profile's tasks on these workers (`[[workers]]` names), the
    /// first first; a retry after a worker is lost goes to the next.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workers: Vec<String>,
    /// Every task of this profile needs tool calling (editing files, running
    /// commands): models that can't call tools are never used for them.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub require_tools: bool,
}

/// Model ordering policy for a profile (see `router::policy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Routing {
    /// Capability and tier fit (the classic rules).
    #[default]
    Auto,
    /// The cheapest eligible model for the task.
    Economy,
    /// The highest declared quality among eligible models.
    Quality,
}

/// Retrying a rate-limited API call (HTTP 429) on the same account.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct RetryCfg {
    /// Calls to one account for one task, the first included (default 3).
    pub attempts: u32,
    /// Wait before a retry when the provider doesn't say (Retry-After):
    /// this, doubled each time (default 1 second).
    pub backoff_secs: u64,
    /// Longest wait for a retry; a longer Retry-After moves on to the next
    /// account instead (default 60 seconds).
    pub max_wait_secs: u64,
}

impl Default for RetryCfg {
    fn default() -> Self {
        RetryCfg { attempts: 3, backoff_secs: 1, max_wait_secs: 60 }
    }
}

/// Billing settings (see `billing`).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct BillingCfg {
    /// Warn when the estimated input reaches this share of the context window (default 0.8).
    #[serde(default)]
    pub context_warn: Option<f64>,
    /// Also send requests estimated over the context window (default: refuse them).
    #[serde(default)]
    pub allow_over_context: bool,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Bins {
    pub claude: Option<String>,
    pub codex: Option<String>,
    pub gemini: Option<String>,
    /// The program Claude starts as fermi's MCP bridge (default: this fermi).
    #[serde(default)]
    pub fermi: Option<String>,
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
    /// Versioned price tables (see `billing`).
    #[serde(default)]
    pub prices: Vec<crate::billing::PriceVersion>,
    #[serde(default)]
    pub budgets: Vec<crate::billing::BudgetCfg>,
    #[serde(default)]
    pub billing: BillingCfg,
    #[serde(default)]
    pub retry: RetryCfg,
    #[serde(default)]
    pub security: SecurityCfg,
    #[serde(default)]
    pub memory: MemoryCfg,
    #[serde(default)]
    pub trace: TraceCfg,
    #[serde(default)]
    pub plugins: PluginsCfg,
    #[serde(default)]
    pub browser: BrowserCfg,
    /// Execution services that run tasks away from this process (see `remote`).
    #[serde(default)]
    pub workers: Vec<WorkerCfg>,
    #[serde(default)]
    pub remote: RemoteCfg,
}

/// A worker: `fermi worker` running here, in a container or on another machine.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct WorkerCfg {
    pub name: String,
    /// e.g. `http://build-box:8787`
    pub url: String,
    /// The token it was started with, or the variable that holds it.
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub token_env: Option<String>,
}

impl WorkerCfg {
    pub fn token(&self) -> String {
        self.token.clone().or_else(|| self.token_env.as_deref().and_then(|v| std::env::var(v).ok())).unwrap_or_default()
    }
}

/// How fermi follows tasks on workers.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RemoteCfg {
    /// A worker unreachable this long (seconds, default 30) is taken for lost:
    /// the attempt is interrupted and retried. Shorter outages are waited out.
    #[serde(default)]
    pub heartbeat_secs: Option<u64>,
    /// Retries after a lost worker (default 1).
    #[serde(default)]
    pub retries: Option<u32>,
}

/// The browser agents drive (see `browser`).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct BrowserCfg {
    /// A Chromium-based browser (Chrome, Edge, Chromium); default: the first one found.
    #[serde(default)]
    pub path: Option<String>,
    /// Start it with `--no-sandbox` (some containers need it).
    #[serde(default)]
    pub no_sandbox: bool,
    /// Offer the browser tools to agents (default on when a browser is found).
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// Plugins (see `plugins`).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PluginsCfg {
    /// Two plugins claiming one name: "namespace" (both, as `<plugin>.<name>`) or "reject" (neither).
    #[serde(default)]
    pub collisions: Collisions,
    /// Run plugins sandboxed (default on): see `sandbox`.
    #[serde(default)]
    pub sandbox: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Collisions {
    #[default]
    Namespace,
    Reject,
}

/// What task traces keep (see `trace` and `redact`).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TraceCfg {
    /// More patterns (regular expressions) whose matches are removed before anything is saved.
    #[serde(default)]
    pub redact: Vec<String>,
    /// Keep each CLI's responses so the task can be replayed (default on).
    #[serde(default)]
    pub tapes: Option<bool>,
}

/// What agents may not touch (see `policy`).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SecurityCfg {
    /// Globs agents must not read: absolute, `~/…` or `**/…`.
    #[serde(default)]
    pub deny_read: Vec<String>,
    /// Shell commands agents may never run (Claude patterns, e.g. `git push *`).
    #[serde(default)]
    pub deny_commands: Vec<String>,
    /// Shell commands that wait for your approval (e.g. `rm *`), once per exact command.
    #[serde(default)]
    pub confirm_commands: Vec<String>,
    /// Run agent CLIs in a sandbox (see `sandbox`).
    #[serde(default)]
    pub sandbox: bool,
    /// Environment variables a sandboxed agent gets besides the system ones.
    #[serde(default)]
    pub env_allow: Vec<String>,
    /// Hosts a sandboxed agent may reach (`host`, `host:port`, `*.domain`), besides the providers'.
    #[serde(default)]
    pub net_allow: Vec<String>,
    /// More folders a sandboxed agent may read, or read and write.
    #[serde(default)]
    pub read_only: Vec<String>,
    #[serde(default)]
    pub read_write: Vec<String>,
}

/// Project memory (see `memory`).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct MemoryCfg {
    /// Give new tasks the project's most relevant memory (default on).
    #[serde(default)]
    pub auto_context: Option<bool>,
    /// How many records at most (default 5).
    #[serde(default)]
    pub brief_size: Option<usize>,
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
        let text = std::fs::read_to_string(&p).with_context(|| format!("reading {} (run `fermi init` first)", p.display()))?;
        Self::parse(&text).with_context(|| format!("in {}", p.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let cfg: Config = toml::from_str(text)?;
        cfg.validate()?;
        crate::billing::validate(&cfg)?;
        crate::policy::validate(&cfg.security.deny_read)?;
        crate::redact::Redactor::new(&cfg.trace.redact)?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        let mut ids = HashSet::new();
        for a in &self.accounts {
            if !ids.insert(a.id.as_str()) {
                bail!("duplicate account id `{}`", a.id);
            }
            if a.auth == Auth::Api && a.api_key_env.is_none() && a.base_url.is_none() {
                bail!("account `{}` uses auth = \"api\" but has no api_key_env (or base_url, for a local endpoint)", a.id);
            }
            if a.api_format == Some(ApiFormat::Chat) && (a.provider != Provider::Openai || a.auth != Auth::Api) {
                bail!("account `{}`: api_format = \"chat\" is for openai accounts with auth = \"api\"", a.id);
            }
        }
        let catalog = self.catalog();
        for m in &catalog {
            if m.provider().is_none() {
                bail!("model `{}` must be `claude:<name>`, `openai:<name>` or `gemini:<name>`", m.id);
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
            if p.require_tools && p.mode != Mode::Auto {
                let without: Vec<&str> =
                    p.models.iter().filter(|id| catalog.iter().any(|m| &m.id == *id && !m.tool_calling())).map(String::as_str).collect();
                if without.len() == p.models.len() {
                    bail!(
                        "profile `{}` requires tool calling, but none of its models can call tools ({}); add one that can, or drop require_tools",
                        p.name,
                        without.join(", ")
                    );
                }
            }
        }
        if self.retry.attempts == 0 {
            bail!("[retry] attempts must be at least 1");
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
            return self.profiles.iter().find(|p| p.name == n).cloned().ok_or_else(|| anyhow!("unknown profile `{n}`"));
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
                gemini_args: vec![],
                budget: None,
                routing: Routing::Auto,
                workers: vec![],
                require_tools: false,
            });
        }
        bail!("no profile matches {} and no default_profile is set (use -p <profile>)", cwd.display())
    }

    pub fn profile_accounts<'a>(&'a self, p: &'a Profile) -> impl Iterator<Item = &'a Account> + 'a {
        p.accounts.iter().filter_map(|id| self.account(id))
    }
}
