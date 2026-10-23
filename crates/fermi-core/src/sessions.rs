//! Conversations from other tools: finds the sessions Claude Code, Codex and
//! the Gemini CLI saved on this PC and turns them into fermi history.
//!
//! An imported session keeps its provider session id when one of the
//! configured accounts owns the folder it was saved in, so a reply resumes
//! it natively. Otherwise a reply passes the conversation on as context.

use crate::config::{Auth, Config, Provider};
use crate::engine::Engine;
use crate::events::Event;
use crate::hub::{RanOn, TaskRec, TaskStatus};
use crate::providers::brief;
use crate::util::now;
use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};

/// The most of a file read to describe it in the list.
const PEEK_BYTES: u64 = 1024 * 1024;
/// Most sessions listed (newest first).
const LIST_LIMIT: usize = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    ClaudeCode,
    Codex,
    Gemini,
}

impl Source {
    pub fn key(self) -> &'static str {
        match self {
            Source::ClaudeCode => "claude-code",
            Source::Codex => "codex",
            Source::Gemini => "gemini",
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Source::ClaudeCode => "Claude Code",
            Source::Codex => "Codex",
            Source::Gemini => "Gemini CLI",
        }
    }
    pub fn provider(self) -> Provider {
        match self {
            Source::ClaudeCode => Provider::Claude,
            Source::Codex => Provider::Openai,
            Source::Gemini => Provider::Gemini,
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        [Source::ClaudeCode, Source::Codex, Source::Gemini].into_iter().find(|x| x.key() == s)
    }
}

/// A session found on disk.
#[derive(Debug, Clone, Serialize)]
pub struct Found {
    pub source: Source,
    /// The provider's session id.
    pub session: String,
    pub path: PathBuf,
    pub cwd: Option<PathBuf>,
    pub title: String,
    /// Last modified (unix seconds).
    pub updated: u64,
    pub size: u64,
    /// The configured account whose config folder holds it.
    pub account: Option<String>,
}

impl Found {
    /// Identifies the session across imports.
    pub fn origin(&self) -> String {
        format!("{}:{}", self.source.key(), self.session)
    }
}

/// A CLI config folder (`~/.claude`, an account's own folder…) and the
/// configured account that uses it, if any.
pub struct Root {
    pub dir: PathBuf,
    pub account: Option<String>,
}

/// Config folders of each provider: every CLI account's own, plus the
/// CLI's default when no account uses it.
pub fn roots(cfg: &Config, provider: Provider) -> Vec<Root> {
    let home = dirs::home_dir().unwrap_or_default();
    let default = match provider {
        Provider::Claude => std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|| home.join(".claude")),
        Provider::Openai => std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".codex")),
        // GEMINI_CLI_HOME is a root holding `.gemini/`, like the home folder.
        Provider::Gemini => std::env::var_os("GEMINI_CLI_HOME").map(PathBuf::from).unwrap_or(home.clone()).join(".gemini"),
    };
    let mut out: Vec<Root> = vec![];
    let mut seen = HashSet::new();
    let accounts = cfg.accounts.iter().filter(|a| a.provider == provider && (a.auth == Auth::Cli || provider == Provider::Gemini));
    for a in accounts {
        let dir = match (a.home_dir(), provider) {
            (Some(h), Provider::Gemini) => h.join(".gemini"),
            (Some(h), _) => h,
            (None, _) => default.clone(),
        };
        if seen.insert(crate::util::norm_path(&dir)) {
            out.push(Root { dir, account: Some(a.id.clone()) });
        }
    }
    if seen.insert(crate::util::norm_path(&default)) {
        out.push(Root { dir: default, account: None });
    }
    out
}

/// Sessions saved by the other tools, newest first.
pub fn discover(cfg: &Config) -> Vec<Found> {
    let mut files: Vec<(Source, PathBuf, Option<String>, u64, u64)> = vec![];
    let mut add = |source, path: PathBuf, account: &Option<String>| {
        if let Ok(m) = std::fs::metadata(&path) {
            let updated = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs());
            files.push((source, path, account.clone(), updated.unwrap_or(0), m.len()));
        }
    };
    for r in roots(cfg, Provider::Claude) {
        // <dir>/projects/<folder>/<session>.jsonl (subfolders hold subagents)
        for project in read_dirs(&r.dir.join("projects")) {
            for f in read_files(&project, "jsonl") {
                add(Source::ClaudeCode, f, &r.account);
            }
        }
    }
    for r in roots(cfg, Provider::Openai) {
        // <dir>/sessions/YYYY/MM/DD/rollout-*.jsonl
        for y in read_dirs(&r.dir.join("sessions")) {
            for m in read_dirs(&y) {
                for d in read_dirs(&m) {
                    for f in read_files(&d, "jsonl") {
                        add(Source::Codex, f, &r.account);
                    }
                }
            }
        }
    }
    for r in roots(cfg, Provider::Gemini) {
        // <dir>/tmp/<project>/chats/session-*.json[l]
        for project in read_dirs(&r.dir.join("tmp")) {
            let chats = project.join("chats");
            for f in read_files(&chats, "jsonl").into_iter().chain(read_files(&chats, "json")) {
                if f.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("session-")) {
                    add(Source::Gemini, f, &r.account);
                }
            }
        }
    }
    files.sort_by_key(|f| std::cmp::Reverse(f.3));
    files.truncate(LIST_LIMIT);
    // Describing a file means reading it: spread that over a few threads,
    // and remember unchanged files between scans.
    let describe = |(source, path, account, updated, size): (Source, PathBuf, Option<String>, u64, u64)| {
        let key = (path.clone(), updated, size);
        let cached = described().lock().unwrap().get(&key).cloned();
        let (session, cwd, title) = match cached {
            Some(c) => c?,
            None => {
                let d = describe_file(source, &path);
                described().lock().unwrap().insert(key, d.clone());
                d?
            }
        };
        Some(Found { source, session, cwd, title, updated, size, account, path })
    };
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8);
    let chunk = files.len().div_ceil(threads).max(1);
    let mut out = vec![];
    std::thread::scope(|scope| {
        let jobs: Vec<_> =
            files.chunks(chunk).map(|c| scope.spawn(move || c.iter().cloned().filter_map(describe).collect::<Vec<_>>())).collect();
        for j in jobs {
            out.extend(j.join().unwrap_or_default());
        }
    });
    out
}

/// Session id, folder and title.
type Described = Option<(String, Option<PathBuf>, String)>;
/// Descriptions by (path, modified, size).
type DescribedCache = std::sync::Mutex<std::collections::HashMap<(PathBuf, u64, u64), Described>>;

fn described() -> &'static DescribedCache {
    static D: std::sync::OnceLock<DescribedCache> = std::sync::OnceLock::new();
    D.get_or_init(Default::default)
}

/// Session id, folder and title, from as little of the file as possible
/// (a pasted image can make the first message large).
fn describe_file(source: Source, path: &Path) -> Described {
    let whole = path.extension().is_some_and(|x| x == "json"); // one JSON document
    for limit in [64 * 1024, PEEK_BYTES] {
        let (head, complete) = if whole { (std::fs::read_to_string(path).ok()?, true) } else { peek(path, limit)? };
        let p = parse(source, path, &head);
        if let (Some(session), Some(title)) = (p.session.clone(), p.title.clone().or_else(|| p.first_user())) {
            return Some((session, p.cwd, one_line(&title, 160)));
        }
        if complete {
            break;
        }
    }
    None
}

fn read_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    rd.filter_map(|e| e.ok()).filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.path()).collect()
}

fn read_files(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    rd.filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == ext))
        .collect()
}

/// The start of a file, cut at the last full line, and whether that's all of it.
fn peek(path: &Path, limit: u64) -> Option<(String, bool)> {
    let mut buf = vec![];
    std::fs::File::open(path).ok()?.take(limit).read_to_end(&mut buf).ok()?;
    let complete = (buf.len() as u64) < limit;
    if !complete {
        let end = buf.iter().rposition(|b| *b == b'\n').unwrap_or(0);
        buf.truncate(end);
    }
    Some((String::from_utf8_lossy(&buf).into_owned(), complete))
}

fn one_line(s: &str, max: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > max {
        format!("{}…", s.chars().take(max).collect::<String>())
    } else {
        s
    }
}

// ── parsing ─────────────────────────────────────────────────────────────────

/// What a session file holds, in fermi's terms.
#[derive(Debug, Default)]
pub struct Parsed {
    pub session: Option<String>,
    pub cwd: Option<PathBuf>,
    pub title: Option<String>,
    /// The provider's own model name (last one used).
    pub model: Option<String>,
    pub started: Option<u64>,
    pub ended: Option<u64>,
    pub events: Vec<Event>,
}

impl Parsed {
    fn first_user(&self) -> Option<String> {
        self.events.iter().find_map(|e| match e {
            Event::User { text } => Some(text.clone()),
            _ => None,
        })
    }
    fn stamp(&mut self, v: &Value) {
        if let Some(t) = v.as_str().and_then(parse_time) {
            self.started.get_or_insert(t);
            self.ended = Some(t);
        }
    }
    fn user(&mut self, text: String) {
        // A fermi model switch: the new message follows the passed-on conversation.
        let text = match text.strip_prefix(crate::hub::HANDOFF_START).and_then(|r| r.rsplit_once(crate::hub::HANDOFF_MARK)) {
            Some((_, msg)) => msg.trim().to_string(),
            None => text.trim().to_string(),
        };
        if !text.is_empty() {
            self.events.push(Event::User { text });
        }
    }
    fn text(&mut self, text: &str) {
        if !text.trim().is_empty() {
            self.events.push(Event::Text { text: text.trim_end().to_string() });
        }
    }
    fn tool(&mut self, name: &str, input: &Value) {
        let input = match input {
            // Codex keeps arguments as a JSON string.
            Value::String(s) => serde_json::from_str(s).unwrap_or_else(|_| input.clone()),
            _ => input.clone(),
        };
        self.events.push(Event::Tool { name: name.to_string(), detail: brief(&input) });
    }
}

pub fn parse(source: Source, path: &Path, text: &str) -> Parsed {
    match source {
        Source::ClaudeCode => parse_claude(path, text),
        Source::Codex => parse_codex(text),
        Source::Gemini => parse_gemini(path, text),
    }
}

fn lines(text: &str) -> impl Iterator<Item = Value> + '_ {
    text.lines().filter(|l| !l.trim().is_empty()).filter_map(|l| serde_json::from_str(l).ok())
}

/// Text a CLI injected into a user turn rather than something the user typed.
fn injected(text: &str) -> bool {
    let t = text.trim_start();
    [
        "<local-command-",
        "<system-reminder>",
        "<command-message>",
        "<environment_context>",
        "<user_instructions>",
        "<permissions instructions>",
        "<app-context>",
        "# AGENTS.md instructions",
        "Caveat: The messages below",
        "[Request interrupted",
    ]
    .iter()
    .any(|p| t.starts_with(p))
}

/// `<command-name>/model</command-name><command-args>opus</command-args>` → `/model opus`.
fn slash_command(text: &str) -> Option<String> {
    let tag = |name: &str| {
        let open = format!("<{name}>");
        let start = text.find(&open)? + open.len();
        let end = text[start..].find(&format!("</{name}>"))? + start;
        Some(text[start..end].trim().to_string())
    };
    let cmd = tag("command-name")?;
    let args = tag("command-args").unwrap_or_default();
    Some(format!("{cmd} {args}").trim().to_string())
}

fn parse_claude(path: &Path, text: &str) -> Parsed {
    let mut p = Parsed { session: path.file_stem().and_then(|s| s.to_str()).map(String::from), ..Default::default() };
    for v in lines(text) {
        let kind = v["type"].as_str().unwrap_or_default();
        if kind == "ai-title" {
            if let Some(t) = v["aiTitle"].as_str() {
                p.title = Some(t.to_string());
            }
            continue;
        }
        if kind != "user" && kind != "assistant" {
            continue;
        }
        if v["isSidechain"] == true || v["isMeta"] == true || v["isCompactSummary"] == true {
            continue;
        }
        if p.cwd.is_none() {
            p.cwd = v["cwd"].as_str().map(PathBuf::from);
        }
        p.stamp(&v["timestamp"]);
        let msg = &v["message"];
        if kind == "user" {
            let mut parts = vec![];
            match &msg["content"] {
                Value::String(s) => parts.push(s.clone()),
                Value::Array(blocks) => {
                    for b in blocks {
                        match b["type"].as_str() {
                            Some("text") => parts.push(b["text"].as_str().unwrap_or_default().to_string()),
                            Some("image") => parts.push("[image]".into()),
                            _ => {} // tool results
                        }
                    }
                }
                _ => {}
            }
            let joined = parts.join("\n");
            if let Some(cmd) = slash_command(&joined) {
                p.user(cmd);
            } else if !injected(&joined) {
                p.user(joined);
            }
        } else {
            if let Some(m) = msg["model"].as_str().filter(|m| !m.starts_with('<')) {
                p.model = Some(m.to_string());
            }
            for b in msg["content"].as_array().into_iter().flatten() {
                match b["type"].as_str() {
                    Some("text") => p.text(b["text"].as_str().unwrap_or_default()),
                    Some("tool_use") => p.tool(b["name"].as_str().unwrap_or("tool"), &b["input"]),
                    _ => {} // thinking
                }
            }
        }
    }
    p
}

fn parse_codex(text: &str) -> Parsed {
    let mut p = Parsed::default();
    for v in lines(text) {
        let pl = &v["payload"];
        match v["type"].as_str().unwrap_or_default() {
            "session_meta" => {
                p.session = pl["id"].as_str().map(String::from);
                p.cwd = pl["cwd"].as_str().map(PathBuf::from);
                p.stamp(&pl["timestamp"]);
            }
            "turn_context" => {
                if let Some(m) = pl["model"].as_str() {
                    p.model = Some(m.to_string());
                }
            }
            "response_item" => {
                p.stamp(&v["timestamp"]);
                match pl["type"].as_str().unwrap_or_default() {
                    "message" => {
                        let text: Vec<&str> = pl["content"].as_array().into_iter().flatten().filter_map(|c| c["text"].as_str()).collect();
                        let text = text.join("\n");
                        match pl["role"].as_str() {
                            Some("user") if !injected(&text) => p.user(text),
                            Some("assistant") => p.text(&text),
                            _ => {}
                        }
                    }
                    "function_call" => p.tool(pl["name"].as_str().unwrap_or("tool"), &pl["arguments"]),
                    "custom_tool_call" => p.tool(pl["name"].as_str().unwrap_or("tool"), &pl["input"]),
                    "local_shell_call" => p.tool("shell", &pl["action"]["command"]),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    p
}

/// Gemini text: a string or a list of parts.
fn gemini_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter_map(|x| x["text"].as_str().or(x.as_str())).collect::<Vec<_>>().join(""),
        Value::Object(_) => v["text"].as_str().unwrap_or_default().to_string(),
        _ => String::new(),
    }
}

fn parse_gemini(path: &Path, text: &str) -> Parsed {
    let mut p = Parsed::default();
    // Messages by id in order; the JSONL form appends patches and rewinds.
    let mut msgs: Vec<(String, Value)> = vec![];
    let meta = |v: &Value, p: &mut Parsed| {
        if let Some(id) = v["sessionId"].as_str() {
            p.session = Some(id.to_string());
        }
        p.stamp(&v["startTime"]);
    };
    if path.extension().is_some_and(|x| x == "json") {
        // Older CLIs: one JSON document.
        if let Ok(v) = serde_json::from_str::<Value>(text) {
            meta(&v, &mut p);
            for m in v["messages"].as_array().into_iter().flatten() {
                msgs.push((m["id"].as_str().unwrap_or_default().to_string(), m.clone()));
            }
        }
    } else {
        for v in lines(text) {
            if let Some(to) = v["$rewindTo"].as_str() {
                match msgs.iter().position(|(id, _)| id == to) {
                    Some(i) => msgs.truncate(i),
                    None => msgs.clear(),
                }
                continue;
            }
            if v.get("$set").is_some() {
                continue;
            }
            let id = v["id"].as_str().map(String::from);
            match (id, v["type"].as_str()) {
                (Some(id), Some(_)) => match msgs.iter_mut().find(|(x, _)| *x == id) {
                    Some(slot) => slot.1 = v,
                    None => msgs.push((id, v)),
                },
                // A patch to an earlier message.
                (Some(id), None) => {
                    if let Some((_, m)) = msgs.iter_mut().find(|(x, _)| *x == id) {
                        if let (Some(obj), Some(c)) = (m.as_object_mut(), v.get("content")) {
                            obj.insert("content".into(), c.clone());
                        }
                    }
                }
                (None, _) => meta(&v, &mut p),
            }
        }
    }
    for (_, m) in &msgs {
        p.stamp(&m["timestamp"]);
        match m["type"].as_str() {
            Some("user") => {
                let t = gemini_text(&m["content"]);
                if !injected(&t) {
                    p.user(t);
                }
            }
            Some("gemini") => {
                if let Some(model) = m["model"].as_str() {
                    p.model = Some(model.to_string());
                }
                p.text(&gemini_text(&m["content"]));
                for tc in m["toolCalls"].as_array().into_iter().flatten() {
                    p.tool(tc["name"].as_str().unwrap_or("tool"), &tc["args"]);
                }
            }
            _ => {}
        }
    }
    // tmp/<project>/.project_root holds the folder the chats belong to.
    let root = path.parent().and_then(|c| c.parent()).map(|d| d.join(".project_root"));
    p.cwd = root.and_then(|r| std::fs::read_to_string(r).ok()).map(|s| PathBuf::from(s.trim())).filter(|d| !d.as_os_str().is_empty());
    p
}

/// `2026-09-15T14:57:15.117Z` → unix seconds (UTC offsets are honored).
pub fn parse_time(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || (b[10] != b'T' && b[10] != b' ') {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, se) = (num(11..13)?, num(14..16)?, num(17..19)?);
    // Days from civil (Howard Hinnant's algorithm).
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * (mo + if mo > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let mut t = days * 86400 + h * 3600 + mi * 60 + se;
    // Offset after the seconds (and optional fraction): Z, +hh:mm or -hh:mm.
    let rest = &s[19..];
    let rest = rest.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    if let Some(sign) = rest.chars().next().filter(|c| *c == '+' || *c == '-') {
        let off = rest[1..].replace(':', "");
        if let (Some(oh), Some(om)) = (off.get(0..2).and_then(|x| x.parse::<i64>().ok()), off.get(2..4).and_then(|x| x.parse::<i64>().ok()))
        {
            let o = oh * 3600 + om * 60;
            t += if sign == '+' { -o } else { o };
        }
    }
    u64::try_from(t).ok()
}

// ── into fermi history ──────────────────────────────────────────────────────

/// The catalog model a provider model name belongs to.
fn catalog_model(engine: &Engine, provider: Provider, raw: Option<&str>) -> Option<String> {
    let of_provider = |id: &str| id.split(':').next() == Some(provider.as_str());
    let models: Vec<_> = engine.catalog.iter().filter(|m| of_provider(&m.id) && !m.image_only()).collect();
    if let Some(raw) = raw {
        if let Some(m) = models.iter().find(|m| m.api.as_deref() == Some(raw) || m.cli.as_deref() == Some(raw)) {
            return Some(m.id.clone());
        }
        let lower = raw.to_lowercase();
        let family = |names: &[&str]| names.iter().find(|n| lower.contains(*n)).map(|n| format!("{}:{n}", provider.as_str()));
        let guess = match provider {
            Provider::Claude => family(&["fable", "opus", "sonnet", "haiku"]),
            Provider::Gemini => family(&["flash-lite", "flash", "pro"]),
            Provider::Openai => None,
        };
        if let Some(id) = guess.filter(|id| models.iter().any(|m| &m.id == id)) {
            return Some(id);
        }
    }
    // The provider's default model.
    let fallback = match provider {
        Provider::Claude => "claude:sonnet",
        Provider::Openai => "openai:gpt",
        Provider::Gemini => "gemini:pro",
    };
    models.iter().find(|m| m.id == fallback).or(models.first()).map(|m| m.id.clone())
}

/// Reads a found session in full and builds its history record.
pub fn to_rec(engine: &Engine, found: &Found, id: u64) -> Result<TaskRec> {
    let text = std::fs::read_to_string(&found.path).with_context(|| format!("reading {}", found.path.display()))?;
    let parsed = parse(found.source, &found.path, &text);
    if !parsed.events.iter().any(|e| matches!(e, Event::User { .. })) {
        bail!("this session has no messages to import");
    }
    let provider = found.source.provider();
    let model = catalog_model(engine, provider, parsed.model.as_deref());
    let label = model
        .as_deref()
        .and_then(|id| engine.catalog.iter().find(|m| m.id == id))
        .map(|m| m.name().to_string())
        .unwrap_or_else(|| found.source.name().to_string());
    // The folder must still exist to continue there.
    let cwd = parsed.cwd.clone().filter(|d| d.is_dir());
    let profile = found.account.as_deref().and_then(|acct| {
        let named = engine.cfg.profiles.iter().find(|p| p.accounts.iter().any(|a| a == acct)).map(|p| p.name.clone());
        named.or_else(|| cwd.as_deref().and_then(|c| engine.cfg.resolve_profile(None, c).ok()).map(|p| p.name))
    });
    // Resumable in place when an account owns the session's folder.
    let native = found.account.is_some() && profile.is_some() && model.is_some() && cwd.is_some();
    let ran_on = native.then(|| RanOn {
        profile: profile.clone().unwrap_or_default(),
        model: model.clone().unwrap_or_default(),
        account: found.account.clone().unwrap_or_default(),
    });
    let prompt = parsed.first_user().unwrap_or_else(|| found.title.clone());
    let turns = parsed.events.iter().filter(|e| matches!(e, Event::User { .. })).count();
    let mut events = parsed.events;
    events.push(Event::Done {
        ok: true,
        summary: format!("imported from {} · {turns} message{}", found.source.name(), if turns == 1 { "" } else { "s" }),
    });
    let started = parsed.started.unwrap_or(found.updated);
    Ok(TaskRec {
        id,
        prompt,
        profile: profile.clone(),
        model: model.clone(),
        account: found.account.clone(),
        effort: None,
        permission: None,
        cwd: cwd.or_else(dirs::home_dir).unwrap_or_default(),
        status: TaskStatus::Done,
        route: Some(match &found.account {
            Some(a) => format!("{label} @ {a}"),
            None => format!("{label} · {}", found.source.name()),
        }),
        started,
        ended: Some(parsed.ended.unwrap_or(found.updated).max(started)),
        ran_on,
        session: native.then(|| found.session.clone()),
        past_sessions: vec![],
        allowed: vec![],
        team: None,
        updated: now(),
        via: Some(found.source.key().to_string()),
        origin: Some(found.origin()),
        events,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times() {
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_time("2026-09-15T14:57:15.117Z"), Some(1_789_484_235));
        assert_eq!(parse_time("2026-09-15T16:57:15+02:00"), Some(1_789_484_235));
        assert_eq!(parse_time("yesterday"), None);
    }

    #[test]
    fn claude_sessions() {
        let text = [
            r#"{"type":"mode","mode":"normal","sessionId":"s1"}"#,
            r#"{"type":"user","cwd":"C:\\work","timestamp":"2026-01-01T00:00:00Z","message":{"role":"user","content":[{"type":"text","text":"Fix the bug"},{"type":"image","source":{}}]}}"#,
            r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"<system-reminder>x</system-reminder>"}}"#,
            r#"{"type":"ai-title","aiTitle":"Bug fix"}"#,
            r#"{"type":"assistant","timestamp":"2026-01-01T00:00:05Z","message":{"model":"claude-opus-5-5","content":[{"type":"thinking","thinking":"hm"}]}}"#,
            r#"{"type":"assistant","message":{"model":"claude-opus-5-5","content":[{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"a.rs"}]}}"#,
            r#"{"type":"assistant","isSidechain":true,"message":{"content":[{"type":"text","text":"subagent"}]}}"#,
            r#"{"type":"assistant","timestamp":"2026-01-01T00:00:09Z","message":{"model":"claude-opus-5-5","content":[{"type":"text","text":"Fixed."}]}}"#,
            r#"{"type":"user","message":{"role":"user","content":"<command-name>/model</command-name>\n<command-args>sonnet</command-args>"}}"#,
        ]
        .join("\n");
        let p = parse(Source::ClaudeCode, Path::new("/x/s1.jsonl"), &text);
        assert_eq!(p.session.as_deref(), Some("s1"));
        assert_eq!(p.title.as_deref(), Some("Bug fix"));
        assert_eq!(p.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(p.cwd, Some(PathBuf::from("C:\\work")));
        assert_eq!((p.started, p.ended), (Some(1_767_225_600), Some(1_767_225_609)));
        let kinds: Vec<String> = p
            .events
            .iter()
            .map(|e| match e {
                Event::User { text } => format!("user:{text}"),
                Event::Text { text } => format!("text:{text}"),
                Event::Tool { name, detail } => format!("tool:{name}:{detail}"),
                _ => "other".into(),
            })
            .collect();
        assert_eq!(kinds, ["user:Fix the bug\n[image]", "tool:Bash:ls", "text:Fixed.", "user:/model sonnet"]);
    }

    #[test]
    fn handed_off_sessions_show_the_new_message() {
        let prompt = format!(
            "{} that started with X.]\n\n<conversation>\nUser: a\n</conversation>\n\n{}\n\nWhat now?",
            crate::hub::HANDOFF_START,
            crate::hub::HANDOFF_MARK
        );
        let line = serde_json::json!({ "type": "user", "message": { "content": prompt } }).to_string();
        let p = parse(Source::ClaudeCode, Path::new("/x/s.jsonl"), &line);
        assert!(matches!(&p.events[..], [Event::User { text }] if text == "What now?"));
    }

    #[test]
    fn codex_sessions() {
        let text = [
            r#"{"type":"session_meta","payload":{"id":"t1","cwd":"/w","timestamp":"2026-01-01T00:00:00Z"}}"#,
            r#"{"type":"turn_context","payload":{"model":"gpt-6-astra"}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"rules"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>x</environment_context>"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Add tests"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"command\":\"cargo test\"}"}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Done"}]}}"#,
        ]
        .join("\n");
        let p = parse(Source::Codex, Path::new("/x/rollout.jsonl"), &text);
        assert_eq!(p.session.as_deref(), Some("t1"));
        assert_eq!(p.model.as_deref(), Some("gpt-6-astra"));
        assert_eq!(p.events.len(), 3);
        assert!(matches!(&p.events[1], Event::Tool { detail, .. } if detail == "cargo test"));
    }

    #[test]
    fn gemini_sessions() {
        let text = [
            r#"{"sessionId":"g1","projectHash":"h","startTime":"2026-01-01T00:00:00Z"}"#,
            r#"{"id":"m1","type":"user","timestamp":"2026-01-01T00:00:01Z","content":[{"text":"first try"}]}"#,
            r#"{"$rewindTo":"m1"}"#,
            r#"{"id":"m2","type":"user","timestamp":"2026-01-01T00:00:02Z","content":"Explain this"}"#,
            r#"{"id":"m3","type":"gemini","model":"gemini-3.8-flash","content":"","toolCalls":[{"name":"read_file","args":{"path":"a.rs"}}]}"#,
            r#"{"id":"m3","content":"It parses input."}"#,
        ]
        .join("\n");
        let p = parse(Source::Gemini, Path::new("/x/chats/session-1-g1.jsonl"), &text);
        assert_eq!(p.session.as_deref(), Some("g1"));
        assert_eq!(p.model.as_deref(), Some("gemini-3.8-flash"));
        let users: Vec<_> = p.events.iter().filter(|e| matches!(e, Event::User { .. })).collect();
        assert_eq!(users.len(), 1, "the rewound message is gone");
        assert!(p.events.iter().any(|e| matches!(e, Event::Text { text } if text == "It parses input.")));
    }
}
