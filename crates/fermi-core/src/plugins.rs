//! Plugins: capabilities fermi gains without changing its code.
//!
//! A plugin is a folder with a `plugin.toml` manifest and a program that
//! speaks MCP over stdio. `fermi plugin install <folder>` checks the manifest
//! (including the plugin API version it needs), copies the folder to
//! `~/.fermi/plugins/<name>/` and enables it: fermi starts the program,
//! discovers its tools and registers the ones the manifest declares as
//! capabilities (tools the manifest doesn't declare are ignored):
//!
//! - `tool`: offered to agents through fermi's MCP bridge;
//! - `command`: run by people (`fermi plugin run`, the dashboard);
//! - `adapter`: a model adapter an account uses instead of the built-in one
//!   (`adapter = "<plugin>.<name>"` on the account); its replies are
//!   normalized into the same events, usage and errors as any other run.
//!
//! Every call starts the program afresh in a sandbox: only allowlisted
//! environment variables (none of fermi's secrets), network only to the
//! hosts the manifest lists, and on Linux file access only to the call's
//! folder, the plugin's own folder and a private temp folder. A crash, a
//! hang or a misbehaving plugin stays in its process and is reported against
//! that plugin. Paths in arguments are checked against the same scope and
//! `[security] deny_read` before a call is made.
//!
//! Two plugins claiming one name: `[plugins] collisions = "namespace"` (the
//! default) registers both as `<plugin>.<name>` and refuses the bare name as
//! ambiguous; `"reject"` registers neither. The result never depends on the
//! order plugins were installed in.
//!
//! Any MCP server is a plugin: `fermi plugin add-mcp <name> -- <command>`
//! writes the manifest from the tools the server offers.

use crate::engine::Engine;
use crate::events::{emit, Event};
use crate::providers::{classify_error, RunCtx, RunResult, Usage};
use crate::util::now;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};

/// Plugin API versions this fermi supports.
pub const API_MIN: u32 = 1;
pub const API_MAX: u32 = 1;
/// MCP revisions fermi speaks: the current one, and the `initialize` era.
const MCP_MODERN: &str = "2026-07-28";
const MCP_LEGACY: &str = "2025-11-25";
/// How long a server gets to answer `server/discover` before it's taken for
/// an older one (which may not answer unknown methods at all).
const PROBE: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Tool,
    Command,
    Adapter,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Tool => "tool",
            Kind::Command => "command",
            Kind::Adapter => "adapter",
        }
    }
}

/// A capability a manifest declares.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapDecl {
    pub kind: Kind,
    pub name: String,
    /// The MCP tool that provides it (default: `name`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default)]
    pub description: String,
}

impl CapDecl {
    fn tool(&self) -> &str {
        self.tool.as_deref().unwrap_or(&self.name)
    }
}

/// `plugin.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    /// The plugin API version it was written for.
    pub api: u32,
    #[serde(default)]
    pub description: String,
    /// The program and its arguments; a relative program is looked up in the
    /// plugin's folder first. `{plugin}` in an argument is the plugin's folder.
    pub command: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<CapDecl>,
    /// Hosts it may reach (`host`, `host:port`, `*.domain`).
    #[serde(default)]
    pub net: Vec<String>,
    /// Environment variables it gets, besides the system ones.
    #[serde(default)]
    pub env: Vec<String>,
    /// Longest a call may take (default 60 s).
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

fn valid_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 40 && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

fn supported() -> String {
    if API_MIN == API_MAX {
        API_MIN.to_string()
    } else {
        format!("{API_MIN} to {API_MAX}")
    }
}

impl Manifest {
    pub fn read(dir: &Path) -> Result<Manifest> {
        let p = dir.join("plugin.toml");
        let text = std::fs::read_to_string(&p).with_context(|| format!("no plugin.toml in {}", dir.display()))?;
        let m: Manifest = toml::from_str(&text).with_context(|| format!("in {}", p.display()))?;
        m.check()?;
        Ok(m)
    }

    pub fn check(&self) -> Result<()> {
        if !valid_name(&self.name) {
            bail!("plugin name `{}`: use lowercase letters, digits, - and _", self.name);
        }
        if !(API_MIN..=API_MAX).contains(&self.api) {
            bail!(
                "plugin `{}` {} needs plugin API {}, but this fermi supports plugin API {}; install a version of the plugin made for API {}",
                self.name,
                self.version,
                self.api,
                supported(),
                supported()
            );
        }
        if self.version.trim().is_empty() {
            bail!("plugin `{}` has no version", self.name);
        }
        if self.command.is_empty() {
            bail!("plugin `{}` has no command to run", self.name);
        }
        let mut seen = HashSet::new();
        for c in &self.capabilities {
            if !valid_name(&c.name) {
                bail!("plugin `{}`: capability name `{}`: use lowercase letters, digits, - and _", self.name, c.name);
            }
            if !seen.insert((c.kind, c.name.clone())) {
                bail!("plugin `{}` declares {} `{}` twice", self.name, c.kind.as_str(), c.name);
            }
        }
        Ok(())
    }
}

// ── what's installed ────────────────────────────────────────────────────────

/// One installed plugin, as `plugins/state.json` keeps it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Installed {
    pub version: String,
    pub enabled: bool,
    pub installed_at: u64,
    /// The declared capabilities' tool definitions, as the plugin reported them when enabled.
    #[serde(default)]
    pub tools: BTreeMap<String, Value>,
    /// Why it isn't running, if enabling it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn dir(home: &Path) -> PathBuf {
    home.join("plugins")
}

static STATE: Mutex<()> = Mutex::new(());

pub fn installed(home: &Path) -> BTreeMap<String, Installed> {
    std::fs::read(dir(home).join("state.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn update(home: &Path, f: impl FnOnce(&mut BTreeMap<String, Installed>)) -> Result<()> {
    let _g = STATE.lock().unwrap();
    let mut all = installed(home);
    f(&mut all);
    let d = dir(home);
    std::fs::create_dir_all(&d)?;
    let tmp = d.join(format!("state.{}.tmp", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec_pretty(&all)?)?;
    std::fs::rename(&tmp, d.join("state.json"))?;
    Ok(())
}

/// Appends to `plugins/events.jsonl`: installs, registrations, calls and failures.
fn log(home: &Path, mut ev: Value) {
    ev["ts"] = json!(now());
    let d = dir(home);
    let _ = std::fs::create_dir_all(&d);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(d.join("events.jsonl")) {
        let _ = std::io::Write::write_all(&mut f, format!("{ev}\n").as_bytes());
    }
}

// ── the registry ────────────────────────────────────────────────────────────

/// A registered capability.
#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub kind: Kind,
    /// The name the manifest gives it.
    pub name: String,
    /// `<plugin>.<name>`, which always resolves.
    pub id: String,
    pub plugin: String,
    pub version: String,
    /// The plugin's MCP tool behind it.
    pub tool: String,
    pub description: String,
    pub schema: Value,
}

/// Two or more plugins claiming one name, and what the policy did.
#[derive(Debug, Clone, Serialize)]
pub struct Collision {
    pub kind: Kind,
    pub name: String,
    pub plugins: Vec<String>,
    pub outcome: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Registry {
    pub entries: Vec<Entry>,
    pub collisions: Vec<Collision>,
}

pub fn registry(engine: &Engine) -> Registry {
    build(&engine.home, engine.cfg.plugins.collisions)
}

/// The registry of enabled plugins. Plugins are visited by name, so the
/// result doesn't depend on the order they were installed in.
fn build(home: &Path, policy: crate::config::Collisions) -> Registry {
    let mut claims: BTreeMap<(Kind, String), Vec<Entry>> = BTreeMap::new();
    for (name, inst) in installed(home).into_iter().filter(|(_, i)| i.enabled && i.error.is_none()) {
        let Ok(m) = Manifest::read(&dir(home).join(&name)) else { continue };
        for c in &m.capabilities {
            let Some(def) = inst.tools.get(c.tool()) else { continue };
            let description =
                if c.description.is_empty() { def["description"].as_str().unwrap_or_default().to_string() } else { c.description.clone() };
            claims.entry((c.kind, c.name.clone())).or_default().push(Entry {
                kind: c.kind,
                name: c.name.clone(),
                id: format!("{}.{}", m.name, c.name),
                plugin: m.name.clone(),
                version: inst.version.clone(),
                tool: c.tool().to_string(),
                description,
                schema: def.get("inputSchema").cloned().unwrap_or_else(|| json!({ "type": "object" })),
            });
        }
    }
    let mut reg = Registry::default();
    for ((kind, name), list) in claims {
        if list.len() == 1 {
            reg.entries.extend(list);
            continue;
        }
        let plugins: Vec<String> = list.iter().map(|e| e.plugin.clone()).collect();
        let outcome = match policy {
            crate::config::Collisions::Namespace => {
                let ids: Vec<String> = list.iter().map(|e| e.id.clone()).collect();
                reg.entries.extend(list);
                format!("registered as {}; the bare name `{name}` is ambiguous", ids.join(" and "))
            }
            crate::config::Collisions::Reject => {
                format!("claimed by {}; registered to neither ([plugins] collisions = \"reject\")", plugins.join(" and "))
            }
        };
        reg.collisions.push(Collision { kind, name, plugins, outcome });
    }
    reg
}

impl Registry {
    /// A capability by `<plugin>.<name>`, or by its bare name when exactly one plugin offers it.
    pub fn resolve(&self, kind: Kind, name: &str) -> Result<&Entry> {
        if let Some(e) = self.entries.iter().find(|e| e.kind == kind && e.id == name) {
            return Ok(e);
        }
        let bare: Vec<&Entry> = self.entries.iter().filter(|e| e.kind == kind && e.name == name).collect();
        match bare[..] {
            [e] => Ok(e),
            [] => match self.collisions.iter().find(|c| c.kind == kind && c.name == name) {
                Some(c) => bail!("{} `{name}` was {}", kind.as_str(), c.outcome),
                None => bail!("no enabled plugin offers the {} `{name}` (see `fermi plugin list`)", kind.as_str()),
            },
            _ => {
                let ids: Vec<&str> = bare.iter().map(|e| e.id.as_str()).collect();
                bail!("`{name}` is offered by several plugins: use {}", ids.join(" or "))
            }
        }
    }
}

// ── managing plugins ────────────────────────────────────────────────────────

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let to = dst.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &to)?;
        } else {
            std::fs::copy(e.path(), &to)?;
        }
    }
    Ok(())
}

/// Installs the plugin in `src` and enables it. A manifest that doesn't
/// check out (an unsupported API version, say) is refused before anything is
/// copied, registered or started.
pub async fn install(engine: &Engine, src: &Path) -> Result<Vec<Entry>> {
    let m = match Manifest::read(src) {
        Ok(m) => m,
        Err(e) => {
            log(&engine.home, json!({ "event": "rejected", "source": src, "reason": format!("{e:#}") }));
            return Err(e);
        }
    };
    let to = dir(&engine.home).join(&m.name);
    if std::fs::canonicalize(src).ok() != std::fs::canonicalize(&to).ok() {
        let _ = std::fs::remove_dir_all(&to);
        copy_dir(src, &to)?;
    }
    update(&engine.home, |all| {
        all.insert(m.name.clone(), Installed { version: m.version.clone(), enabled: false, installed_at: now(), ..Default::default() });
    })?;
    log(&engine.home, json!({ "event": "installed", "plugin": m.name, "version": m.version, "api": m.api }));
    enable(engine, &m.name).await
}

/// Starts the plugin once to discover its tools, then registers the declared ones.
pub async fn enable(engine: &Engine, name: &str) -> Result<Vec<Entry>> {
    let home = &engine.home;
    if !installed(home).contains_key(name) {
        bail!("no plugin `{name}` is installed");
    }
    let pdir = dir(home).join(name);
    let m = Manifest::read(&pdir)?;
    let found = async {
        let mut c = Client::start(engine, &m, &pdir, &pdir).await?;
        let tools = c.list_tools().await;
        c.stop().await;
        tools
    }
    .await;
    let fail = |why: String| {
        let _ = update(home, |all| {
            if let Some(i) = all.get_mut(name) {
                i.enabled = false;
                i.error = Some(why.clone());
            }
        });
        log(home, json!({ "event": "enable_failed", "plugin": name, "version": m.version, "reason": why }));
        anyhow!("plugin `{name}` {} couldn't be enabled: {why}", m.version)
    };
    let tools = found.map_err(|e| fail(format!("{e:#}")))?;
    let offered: BTreeMap<String, Value> = tools.into_iter().filter_map(|t| Some((t["name"].as_str()?.to_string(), t))).collect();
    let missing: Vec<&str> = m.capabilities.iter().map(CapDecl::tool).filter(|t| !offered.contains_key(*t)).collect();
    if !missing.is_empty() {
        return Err(fail(format!("it declares {} but its program doesn't offer them", missing.join(", "))));
    }
    let declared: BTreeMap<String, Value> = offered
        .iter()
        .filter(|(k, _)| m.capabilities.iter().any(|c| c.tool() == k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let ignored: Vec<&String> = offered.keys().filter(|k| !declared.contains_key(*k)).collect();
    update(home, |all| {
        if let Some(i) = all.get_mut(name) {
            i.enabled = true;
            i.error = None;
            i.version = m.version.clone();
            i.tools = declared.clone();
        }
    })?;
    let reg = registry(engine);
    let mine: Vec<Entry> = reg.entries.iter().filter(|e| e.plugin == name).cloned().collect();
    log(
        home,
        json!({ "event": "enabled", "plugin": name, "version": m.version,
            "registered": mine.iter().map(|e| json!({ "kind": e.kind, "id": e.id })).collect::<Vec<_>>(),
            "ignored_undeclared": ignored,
            "collisions": reg.collisions.iter().filter(|c| c.plugins.iter().any(|p| p == name)).collect::<Vec<_>>() }),
    );
    Ok(mine)
}

pub fn disable(engine: &Engine, name: &str) -> Result<()> {
    let mut found = false;
    update(&engine.home, |all| {
        if let Some(i) = all.get_mut(name) {
            i.enabled = false;
            found = true;
        }
    })?;
    if !found {
        bail!("no plugin `{name}` is installed");
    }
    log(&engine.home, json!({ "event": "disabled", "plugin": name }));
    Ok(())
}

pub fn remove(engine: &Engine, name: &str) -> Result<()> {
    let mut found = false;
    update(&engine.home, |all| found = all.remove(name).is_some())?;
    if !found {
        bail!("no plugin `{name}` is installed");
    }
    let _ = std::fs::remove_dir_all(dir(&engine.home).join(name));
    log(&engine.home, json!({ "event": "removed", "plugin": name }));
    Ok(())
}

/// Makes an MCP server a plugin: discovers its tools and installs a manifest declaring them.
pub async fn add_mcp(engine: &Engine, name: &str, command: Vec<String>) -> Result<Vec<Entry>> {
    let staging = dir(&engine.home).join(format!(".new-{name}"));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    let mut m = Manifest {
        name: name.to_string(),
        version: "mcp".into(),
        api: API_MAX,
        description: format!("MCP server: {}", command.join(" ")),
        command,
        capabilities: vec![],
        net: vec![],
        env: vec![],
        timeout_secs: None,
    };
    m.check()?;
    let tools = async {
        let mut c = Client::start(engine, &m, &staging, &staging).await?;
        let tools = c.list_tools().await;
        c.stop().await;
        tools
    }
    .await;
    let tools = tools.inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&staging);
    })?;
    m.capabilities = tools
        .iter()
        .filter_map(|t| {
            let tool = t["name"].as_str()?;
            let name: String = tool.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
            Some(CapDecl { kind: Kind::Tool, name, tool: Some(tool.to_string()), description: String::new() })
        })
        .collect();
    std::fs::write(staging.join("plugin.toml"), toml::to_string_pretty(&m)?)?;
    let out = install(engine, &staging).await;
    let _ = std::fs::remove_dir_all(&staging);
    out
}

// ── calling a capability ────────────────────────────────────────────────────

/// What a call returned, attributed to the plugin version that answered.
#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    pub plugin: String,
    pub version: String,
    pub capability: String,
    /// False when the tool reported an error (`isError`).
    pub ok: bool,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured: Option<Value>,
    pub ms: u64,
}

/// Calls a registered capability from `cwd`. The arguments are checked
/// against the tool's schema and the plugin's scope first; a plugin that
/// crashes, hangs or breaks the protocol fails this call only.
pub async fn call(engine: &Engine, kind: Kind, name: &str, args: &Value, cwd: &Path) -> Result<Outcome> {
    let reg = registry(engine);
    let e = reg.resolve(kind, name)?.clone();
    let home = &engine.home;
    let pdir = dir(home).join(&e.plugin);
    let m = Manifest::read(&pdir)?;
    let refuse = |why: String| {
        log(home, json!({ "event": "refused", "plugin": e.plugin, "version": e.version, "capability": e.id, "reason": why }));
        anyhow!("{} refused the call: {why}", e.id)
    };
    validate(&e.schema, args).map_err(refuse)?;
    if kind != Kind::Adapter {
        scope(engine, &pdir, args, cwd).map_err(refuse)?;
    }
    let started = Instant::now();
    let res = async {
        let mut c = Client::start(engine, &m, &pdir, cwd).await?;
        let r = c.call_tool(&e.tool, args).await;
        c.stop().await;
        r
    }
    .await;
    let ms = started.elapsed().as_millis() as u64;
    match res {
        Ok((ok, text, structured)) => {
            log(home, json!({ "event": "call", "plugin": e.plugin, "version": e.version, "capability": e.id, "ok": ok, "ms": ms }));
            Ok(Outcome { plugin: e.plugin, version: e.version, capability: e.id, ok, text, structured, ms })
        }
        Err(err) => {
            let why = format!("{err:#}");
            log(home, json!({ "event": "fault", "plugin": e.plugin, "version": e.version, "capability": e.id, "error": why, "ms": ms }));
            bail!("plugin `{}` {} failed in `{}`: {why}", e.plugin, e.version, e.name)
        }
    }
}

/// Every string in `v`.
fn strings(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => out.push(s.clone()),
        Value::Array(a) => a.iter().for_each(|x| strings(x, out)),
        Value::Object(o) => o.values().for_each(|x| strings(x, out)),
        _ => {}
    }
}

fn looks_like_path(s: &str) -> bool {
    !s.contains("://")
        && !s.chars().any(char::is_whitespace)
        && (s.contains('/') || s.contains('\\') || s.starts_with('.') || s.starts_with('~'))
}

fn norm(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let s = s.trim_start_matches("//?/").trim_end_matches('/').to_string();
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s
    }
}

fn within(p: &Path, dir: &Path) -> bool {
    let (p, d) = (norm(p), norm(dir));
    p == d || p.starts_with(&format!("{d}/"))
}

/// Paths in a plugin call's arguments must stay in its scope (the call's
/// folder and the plugin's own) and clear of `[security] deny_read`, so a
/// plugin can't be pointed at what the sandbox keeps it from.
fn scope(engine: &Engine, pdir: &Path, args: &Value, cwd: &Path) -> std::result::Result<(), String> {
    let mut all = vec![];
    strings(args, &mut all);
    for s in all.iter().filter(|s| looks_like_path(s)) {
        let p = crate::policy::resolve(s, cwd);
        let real = std::fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
        let inside = |q: &Path| within(q, cwd) || within(q, pdir);
        if !inside(&p) || !inside(&real) {
            return Err(format!("`{s}` is outside the plugin's scope (the working folder and the plugin's own folder)"));
        }
        if crate::policy::denied(&engine.cfg.security.deny_read, &p) || crate::policy::denied(&engine.cfg.security.deny_read, &real) {
            return Err(format!("`{s}` is off limits (deny_read)"));
        }
    }
    Ok(())
}

/// Checks `v` against the parts of JSON Schema tools use: types, required
/// and unknown properties, enums, items, lengths and bounds.
pub fn validate(schema: &Value, v: &Value) -> std::result::Result<(), String> {
    check(schema, v, "arguments")
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn check(s: &Value, v: &Value, at: &str) -> std::result::Result<(), String> {
    let types: Vec<&str> = match &s["type"] {
        Value::String(t) => vec![t.as_str()],
        Value::Array(a) => a.iter().filter_map(|t| t.as_str()).collect(),
        _ => vec![],
    };
    let actual = type_name(v);
    if !types.is_empty() && !types.iter().any(|t| *t == actual || (*t == "number" && actual == "integer")) {
        return Err(format!("{at} should be {}, not {actual}", types.join(" or ")));
    }
    if let Some(options) = s["enum"].as_array() {
        if !options.contains(v) {
            return Err(format!("{at} should be one of {}", Value::Array(options.clone())));
        }
    }
    match v {
        Value::Object(o) => {
            for r in s["required"].as_array().into_iter().flatten().filter_map(|r| r.as_str()) {
                if !o.contains_key(r) {
                    return Err(format!("{at}.{r} is required"));
                }
            }
            let props = s["properties"].as_object();
            for (k, x) in o {
                match props.and_then(|p| p.get(k)) {
                    Some(ps) => check(ps, x, &format!("{at}.{k}"))?,
                    None if s["additionalProperties"] == false => return Err(format!("{at}.{k} isn't a known argument")),
                    None => {}
                }
            }
        }
        Value::Array(a) => {
            if s["items"].is_object() {
                for (i, x) in a.iter().enumerate() {
                    check(&s["items"], x, &format!("{at}[{i}]"))?;
                }
            }
        }
        Value::String(t) => {
            let n = t.chars().count() as u64;
            if s["minLength"].as_u64().is_some_and(|m| n < m) || s["maxLength"].as_u64().is_some_and(|m| n > m) {
                return Err(format!("{at} has the wrong length ({n} characters)"));
            }
        }
        Value::Number(n) => {
            let x = n.as_f64().unwrap_or(0.0);
            if s["minimum"].as_f64().is_some_and(|m| x < m) || s["maximum"].as_f64().is_some_and(|m| x > m) {
                return Err(format!("{at} is out of range ({x})"));
            }
        }
        _ => {}
    }
    Ok(())
}

// ── model adapters ──────────────────────────────────────────────────────────

/// Runs a model call through a plugin's adapter, normalized like the built-in
/// adapters: tools, model calls and text become the usual events, usage the
/// usual record, and errors are classified (limits, outages) the same way.
///
/// The adapter gets `{prompt, model, effort, resume, cwd, permission}` and
/// answers (as structured content, or JSON text) with `{text, session,
/// usage, calls: [{id, model, usage}], tools: [{id, name, input, output,
/// status, code}], error}`; `usage` uses Anthropic's field names.
pub async fn run_adapter(ctx: &RunCtx<'_>, spec: &str) -> Result<RunResult> {
    if ctx.engine.replay.is_some() {
        bail!("replays cover CLI accounts; `{}` runs through the plugin adapter `{spec}`", ctx.account.id);
    }
    let args = json!({
        "prompt": ctx.task.input(),
        "model": ctx.model.cli.clone().unwrap_or_else(|| ctx.model.id.clone()),
        "effort": ctx.effort(),
        "resume": ctx.task.resume,
        "cwd": ctx.task.cwd,
        "permission": ctx.permission(),
    });
    let out = match call(ctx.engine, Kind::Adapter, spec, &args, &ctx.task.cwd).await {
        Ok(o) => o,
        // A broken adapter is like an unreachable provider: try the next account.
        Err(e) => return Ok(RunResult::failed(format!("{e:#}"), true)),
    };
    let r = out.structured.clone().or_else(|| serde_json::from_str::<Value>(&out.text).ok()).unwrap_or_else(|| json!({ "text": out.text }));
    if !out.ok || r["error"].is_string() {
        let msg = r["error"].as_str().map(str::to_string).unwrap_or_else(|| out.text.clone());
        return Ok(classify_error(msg, None));
    }
    let session = r["session"].as_str().map(str::to_string);
    if let Some(id) = &session {
        emit(ctx.tx, Event::Session { id: id.clone() });
    }
    let mut ops = crate::trace::Ops::new(ctx);
    for t in r["tools"].as_array().into_iter().flatten() {
        let id = ops.tool(t["id"].as_str(), t["name"].as_str().unwrap_or("tool"), &t["input"], t["call"].as_str());
        let status = t["status"].as_str().unwrap_or("ok");
        ops.result(&id, status, &crate::trace::content_text(&t["output"]), t["code"].as_str().map(str::to_string));
    }
    for c in r["calls"].as_array().into_iter().flatten() {
        ops.call(c["id"].as_str().unwrap_or("call"), c["model"].as_str(), &c["usage"]);
    }
    ops.finish();
    let text = r["text"].as_str().unwrap_or_default().to_string();
    if !text.trim().is_empty() {
        emit(ctx.tx, Event::Text { text: text.clone() });
    }
    let u = &r["usage"];
    let n = |k: &str| u[k].as_u64().unwrap_or(0);
    let usage = Usage {
        input: n("input_tokens") + n("cache_read_input_tokens") + n("cache_creation_input_tokens"),
        cached_input: n("cache_read_input_tokens"),
        cache_write: n("cache_creation_input_tokens"),
        output: n("output_tokens"),
        cost_usd: r["cost_usd"].as_f64(),
        reported: Usage::present(
            u,
            &[
                ("input_tokens", Usage::INPUT),
                ("cache_read_input_tokens", Usage::CACHED),
                ("cache_creation_input_tokens", Usage::CACHE_WRITE),
                ("output_tokens", Usage::OUTPUT),
            ],
        ),
    };
    let mut res = RunResult::ok(text, usage);
    res.session = session;
    Ok(res)
}

// ── the MCP client ──────────────────────────────────────────────────────────

/// Why a request didn't get a result.
enum Fail {
    Rpc { code: i64, message: String, data: Value },
    Gone(String),
    Timeout,
}

impl Fail {
    fn into_err(self, what: &str, secs: u64) -> anyhow::Error {
        match self {
            Fail::Rpc { code, message, .. } => anyhow!("{what}: {message} (error {code})"),
            Fail::Gone(why) => anyhow!("{what}: {why}"),
            Fail::Timeout => anyhow!("{what}: no answer within {secs} s"),
        }
    }
}

/// One plugin process, for one call (or one discovery).
struct Client {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    next: u64,
    modern: bool,
    tail: Arc<Mutex<VecDeque<String>>>,
    deadline: Instant,
    secs: u64,
    tmp: Option<PathBuf>,
    _proxy: Option<crate::sandbox::Proxy>,
}

fn program(m: &Manifest, pdir: &Path) -> Result<PathBuf> {
    let first = &m.command[0];
    let local = pdir.join(first);
    if local.is_file() {
        return Ok(local);
    }
    if Path::new(first).is_absolute() || first.contains(['/', '\\']) {
        return Ok(PathBuf::from(first));
    }
    for ext in if cfg!(windows) { &[".exe", ".cmd", ""][..] } else { &[""][..] } {
        let p = pdir.join(format!("{first}{ext}"));
        if p.is_file() {
            return Ok(p);
        }
    }
    crate::util::resolve_bin(first, None)
}

impl Client {
    async fn start(engine: &Engine, m: &Manifest, pdir: &Path, cwd: &Path) -> Result<Client> {
        let prog = program(m, pdir)?;
        let mut cmd = tokio::process::Command::new(&prog);
        let pd = pdir.display().to_string();
        cmd.args(m.command[1..].iter().map(|a| a.replace("{plugin}", &pd)));
        let (mut tmp, mut proxy) = (None, None);
        if engine.cfg.plugins.sandbox != Some(false) {
            let t = engine.home.join("sandbox").join(format!("plugin-{}-{}", m.name, &crate::util::uuid_v4()[..8]));
            std::fs::create_dir_all(&t)?;
            let p = crate::sandbox::Proxy::start(m.net.clone(), dir(&engine.home).join("egress.jsonl")).await?;
            let mut read_only = crate::sandbox::system_dirs();
            read_only.push(pdir.to_path_buf());
            read_only.extend(std::fs::canonicalize(&prog).ok().and_then(|r| r.parent().map(Path::to_path_buf)));
            let jail = crate::sandbox::Jail {
                read_write: vec![cwd.to_path_buf(), t.clone()],
                read_only,
                env_allow: m.env.clone(),
                proxy: Some(p.port),
                tmp: t.clone(),
            };
            crate::sandbox::apply(&mut cmd, &jail)?;
            (tmp, proxy) = (Some(t), Some(p));
        }
        cmd.current_dir(cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = cmd.spawn().with_context(|| format!("starting {}", prog.display()))?;
        let tail = Arc::new(Mutex::new(VecDeque::new()));
        let mut err = BufReader::new(child.stderr.take().expect("piped stderr")).lines();
        let t2 = tail.clone();
        tokio::spawn(async move {
            while let Ok(Some(line)) = err.next_line().await {
                let mut t = t2.lock().unwrap();
                if t.len() == 20 {
                    t.pop_front();
                }
                t.push_back(line);
            }
        });
        let secs = m.timeout_secs.unwrap_or(60);
        let mut c = Client {
            stdin: child.stdin.take().expect("piped stdin"),
            lines: BufReader::new(child.stdout.take().expect("piped stdout")).lines(),
            child,
            next: 0,
            modern: false,
            tail,
            deadline: Instant::now() + Duration::from_secs(secs),
            secs,
            tmp,
            _proxy: proxy,
        };
        if let Err(e) = c.handshake().await {
            c.stop().await;
            return Err(e);
        }
        Ok(c)
    }

    fn meta(&self) -> Value {
        json!({ "io.modelcontextprotocol/protocolVersion": MCP_MODERN,
            "io.modelcontextprotocol/clientInfo": { "name": "fermi", "version": env!("CARGO_PKG_VERSION") },
            "io.modelcontextprotocol/clientCapabilities": {} })
    }

    /// Modern servers answer `server/discover`; anything else gets the
    /// `initialize` handshake of earlier revisions.
    async fn handshake(&mut self) -> Result<()> {
        let probe = self.request("server/discover", json!({ "_meta": self.meta() }), Some(PROBE)).await;
        match probe {
            Ok(r) => {
                let versions: Vec<&str> = r["supportedVersions"].as_array().into_iter().flatten().filter_map(|v| v.as_str()).collect();
                if !versions.contains(&MCP_MODERN) {
                    bail!("it speaks MCP {}; fermi speaks {MCP_MODERN} and {MCP_LEGACY}", versions.join(", "));
                }
                self.modern = true;
                Ok(())
            }
            Err(Fail::Rpc { code: -32022, data, .. }) => {
                bail!("it speaks MCP {}; fermi speaks {MCP_MODERN} and {MCP_LEGACY}", data["supported"]);
            }
            Err(Fail::Gone(why)) => Err(anyhow!("{why}")),
            Err(_) => {
                let init = json!({ "protocolVersion": MCP_LEGACY, "capabilities": {},
                    "clientInfo": { "name": "fermi", "version": env!("CARGO_PKG_VERSION") } });
                self.request("initialize", init, None).await.map_err(|f| f.into_err("initialize", self.secs))?;
                self.notify("notifications/initialized").await;
                Ok(())
            }
        }
    }

    async fn write(&mut self, msg: &Value) -> std::result::Result<(), Fail> {
        let line = format!("{msg}\n");
        self.stdin.write_all(line.as_bytes()).await.map_err(|_| self.gone())?;
        self.stdin.flush().await.map_err(|_| self.gone())
    }

    fn gone(&mut self) -> Fail {
        let code = self.child.try_wait().ok().flatten().map(|s| s.to_string()).unwrap_or_else(|| "it closed its output".into());
        let tail: Vec<String> = self.tail.lock().unwrap().iter().cloned().collect();
        Fail::Gone(if tail.is_empty() {
            format!("the plugin exited ({code})")
        } else {
            format!("the plugin exited ({code}): {}", tail.join("\n"))
        })
    }

    async fn notify(&mut self, method: &str) {
        let _ = self.write(&json!({ "jsonrpc": "2.0", "method": method })).await;
    }

    async fn request(&mut self, method: &str, mut params: Value, limit: Option<Duration>) -> std::result::Result<Value, Fail> {
        self.next += 1;
        let id = self.next;
        if self.modern {
            params["_meta"] = self.meta();
        }
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })).await?;
        let until = limit.map(|l| (Instant::now() + l).min(self.deadline)).unwrap_or(self.deadline);
        loop {
            let left = until.saturating_duration_since(Instant::now());
            let line = match tokio::time::timeout(left, self.lines.next_line()).await {
                Err(_) => return Err(Fail::Timeout),
                Ok(Ok(Some(l))) => l,
                Ok(_) => {
                    // Let stderr finish so the diagnosis includes it.
                    let _ = tokio::time::timeout(Duration::from_millis(500), self.child.wait()).await;
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    return Err(self.gone());
                }
            };
            let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
            if msg.get("method").is_some() {
                // The server asking something of fermi (sampling, roots…): not offered.
                if let Some(rid) = msg.get("id") {
                    let no = json!({ "jsonrpc": "2.0", "id": rid, "error": { "code": -32601, "message": "fermi doesn't offer this" } });
                    let _ = self.write(&no).await;
                }
                continue;
            }
            if msg["id"] != json!(id) {
                continue;
            }
            if let Some(e) = msg.get("error") {
                return Err(Fail::Rpc {
                    code: e["code"].as_i64().unwrap_or(0),
                    message: e["message"].as_str().unwrap_or("error").to_string(),
                    data: e["data"].clone(),
                });
            }
            return Ok(msg["result"].clone());
        }
    }

    async fn list_tools(&mut self) -> Result<Vec<Value>> {
        let mut out = vec![];
        let mut cursor: Option<String> = None;
        loop {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let r = self.request("tools/list", params, None).await.map_err(|f| f.into_err("tools/list", self.secs))?;
            out.extend(r["tools"].as_array().cloned().unwrap_or_default());
            cursor = r["nextCursor"].as_str().map(str::to_string);
            if cursor.is_none() {
                return Ok(out);
            }
        }
    }

    /// (ok, text, structured content)
    async fn call_tool(&mut self, tool: &str, args: &Value) -> Result<(bool, String, Option<Value>)> {
        let r = self
            .request("tools/call", json!({ "name": tool, "arguments": args }), None)
            .await
            .map_err(|f| f.into_err(&format!("calling `{tool}`"), self.secs))?;
        if r["resultType"] == "input_required" {
            bail!("`{tool}` asked for more input, which fermi can't provide");
        }
        let text = crate::trace::content_text(&r["content"]);
        let structured = r.get("structuredContent").filter(|v| !v.is_null()).cloned();
        Ok((r["isError"] != true, text, structured))
    }

    async fn stop(mut self) {
        if let Some(pid) = self.child.id() {
            if matches!(self.child.try_wait(), Ok(None)) {
                crate::providers::proc::kill_tree(pid);
            }
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await;
        if let Some(t) = self.tmp.take() {
            let _ = std::fs::remove_dir_all(t);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_and_paths() {
        let s = json!({ "type": "object", "properties": { "text": { "type": "string", "minLength": 1 }, "n": { "type": "integer", "maximum": 9 } },
            "required": ["text"], "additionalProperties": false });
        assert!(validate(&s, &json!({ "text": "hi", "n": 3 })).is_ok());
        assert_eq!(validate(&s, &json!({ "n": 3 })).unwrap_err(), "arguments.text is required");
        assert!(validate(&s, &json!({ "text": 5 })).unwrap_err().contains("should be string"));
        assert!(validate(&s, &json!({ "text": "a", "x": 1 })).unwrap_err().contains("isn't a known argument"));
        assert!(validate(&s, &json!({ "text": "a", "n": 12 })).unwrap_err().contains("out of range"));
        assert!(
            looks_like_path("../secret.txt")
                && looks_like_path("C:\\x\\y")
                && !looks_like_path("hello world/x")
                && !looks_like_path("https://a/b")
        );
        assert!(within(Path::new("/w/proj/a.txt"), Path::new("/w/proj")) && !within(Path::new("/w/project2/a"), Path::new("/w/proj")));
        let bad = Manifest {
            name: "x".into(),
            version: "1".into(),
            api: 99,
            description: String::new(),
            command: vec!["x".into()],
            capabilities: vec![],
            net: vec![],
            env: vec![],
            timeout_secs: None,
        };
        let e = bad.check().unwrap_err().to_string();
        assert!(e.contains("needs plugin API 99") && e.contains("supports plugin API 1"), "{e}");
    }
}
