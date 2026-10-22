//! Skills and subagents of the agent CLIs. Every fermi account keeps its own
//! config folder, so a skill installed in `~/.claude` isn't visible to an
//! isolated account. This finds them all, copies them between accounts and
//! projects (converting subagents between Claude and Gemini), and moves them
//! in and out of `.zip` bundles.
//!
//! Layout: a skill is a folder with a `SKILL.md` under `<config>/skills/`
//! (Claude Code, Codex, Gemini CLI); a subagent is a Markdown file under
//! `<config>/agents/` (Claude Code, Gemini CLI).

use anyhow::{bail, Context, Result};
use fermi_core::sessions::roots;
use fermi_core::{Config, Provider};
use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const MAX_FILES: usize = 3000;
const MAX_BYTES: u64 = 200 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Skill,
    Agent,
}

/// Somewhere skills and agents live.
#[derive(Debug, Clone, Serialize)]
pub struct Place {
    pub id: String,
    pub label: String,
    /// Whose format it is; None for an import being reviewed.
    pub provider: Option<Provider>,
    /// "account", "cli" (a CLI's own folder no account uses), "project" or "import".
    pub scope: &'static str,
    pub skills: PathBuf,
    /// None where the CLI has no subagents (Codex).
    pub agents: Option<PathBuf>,
}

impl Place {
    fn dir(&self, kind: Kind) -> Result<&Path> {
        match kind {
            Kind::Skill => Ok(&self.skills),
            Kind::Agent => self.agents.as_deref().with_context(|| format!("{} has no subagents", self.label)),
        }
    }
    fn path(&self, kind: Kind, name: &str) -> Result<PathBuf> {
        check_name(name)?;
        Ok(match kind {
            Kind::Skill => self.dir(kind)?.join(name),
            Kind::Agent => self.dir(kind)?.join(format!("{name}.md")),
        })
    }
}

fn cli_name(p: Provider) -> &'static str {
    match p {
        Provider::Claude => "Claude Code",
        Provider::Openai => "Codex",
        Provider::Gemini => "Gemini CLI",
    }
}

fn tilde(p: &Path) -> String {
    match dirs::home_dir().and_then(|h| p.strip_prefix(h).ok().map(Path::to_path_buf)) {
        Some(rest) => format!("~/{}", rest.to_string_lossy().replace('\\', "/")),
        None => p.display().to_string(),
    }
}

/// Every account's config folder, each CLI's default one, and the projects'
/// `.claude` / `.codex` / `.gemini` folders.
pub fn places(cfg: &Config, projects: &[PathBuf]) -> Vec<Place> {
    let mut out = vec![];
    for provider in [Provider::Claude, Provider::Openai, Provider::Gemini] {
        for r in roots(cfg, provider) {
            out.push(Place {
                id: format!("{}:{}", provider.as_str(), r.account.as_deref().unwrap_or("~")),
                label: match &r.account {
                    Some(a) => format!("{a} · {}", cli_name(provider)),
                    None => format!("{} ({})", cli_name(provider), tilde(&r.dir)),
                },
                provider: Some(provider),
                scope: if r.account.is_some() { "account" } else { "cli" },
                skills: r.dir.join("skills"),
                agents: (provider != Provider::Openai).then(|| r.dir.join("agents")),
            });
        }
    }
    for proj in projects {
        let name = proj.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| proj.display().to_string());
        for (provider, sub) in [(Provider::Claude, ".claude"), (Provider::Openai, ".codex"), (Provider::Gemini, ".gemini")] {
            let dir = proj.join(sub);
            out.push(Place {
                id: format!("project:{}:{}", provider.as_str(), proj.display()),
                label: format!("{name} project · {}", cli_name(provider)),
                provider: Some(provider),
                scope: "project",
                skills: dir.join("skills"),
                agents: (provider != Provider::Openai).then(|| dir.join("agents")),
            });
        }
    }
    out
}

/// Folder and file names we create: no paths, no hidden names.
pub fn check_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 100
        && !name.starts_with('.')
        && name.chars().all(|c| c.is_alphanumeric() || "._- ".contains(c));
    if !ok {
        bail!("`{name}` isn't a usable name");
    }
    Ok(())
}

fn sanitize(name: &str) -> String {
    let s: String = name.trim().chars().map(|c| if c.is_alphanumeric() || "._-".contains(c) { c } else { '-' }).collect();
    s.trim_matches(|c| c == '-' || c == '.').chars().take(100).collect()
}

// ── front matter ────────────────────────────────────────────────────────────

/// Top-level `key: value` pairs of a Markdown file's YAML front matter (block
/// scalars and lists folded into one string), and the body after it.
pub fn front_matter(text: &str) -> (Vec<(String, String)>, &str) {
    let t = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(rest) = t.strip_prefix("---\n").or_else(|| t.strip_prefix("---\r\n")) else { return (vec![], t) };
    let end = rest.find("\n---").map(|i| i + 1);
    let Some(end) = end else { return (vec![], t) };
    let (yaml, after) = rest.split_at(end);
    let body = after.trim_start_matches("---").trim_start_matches(['\r', '\n']);
    let mut out: Vec<(String, String)> = vec![];
    for line in yaml.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let indented = line.starts_with(' ') || line.starts_with('\t');
        if indented {
            if let Some((_, v)) = out.last_mut() {
                let piece = line.trim().trim_start_matches("- ").trim();
                if !v.is_empty() && !v.ends_with(' ') {
                    v.push(' ');
                }
                v.push_str(piece);
            }
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            let v = v.trim();
            let v = if matches!(v, "|" | ">" | "|-" | ">-") { "" } else { v };
            out.push((k.trim().to_string(), v.trim_matches(|c| c == '"' || c == '\'').to_string()));
        }
    }
    (out, body)
}

fn fm_get<'a>(fm: &'a [(String, String)], key: &str) -> Option<&'a str> {
    fm.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str()).filter(|v| !v.is_empty())
}

/// Which CLI a subagent file was written for, when it shows.
pub fn agent_flavor(text: &str) -> Option<Provider> {
    let (fm, _) = front_matter(text);
    let has = |k: &str| fm.iter().any(|(key, _)| key == k);
    if ["kind", "max_turns", "temperature", "timeout_mins"].iter().any(|k| has(k)) {
        return Some(Provider::Gemini);
    }
    if ["color", "permissionMode"].iter().any(|k| has(k)) {
        return Some(Provider::Claude);
    }
    // Claude lists tools inline (`tools: Read, Grep`), Gemini as a YAML list.
    let yaml = text.split("\n---").next().unwrap_or("");
    if let Some(line) = yaml.lines().find(|l| l.starts_with("tools:")) {
        return Some(if line["tools:".len()..].trim().is_empty() { Provider::Gemini } else { Provider::Claude });
    }
    None
}

/// A subagent for another CLI: name, description and instructions carry
/// over; tool lists and models don't (their names differ between CLIs).
pub fn convert_agent(text: &str, fallback_name: &str) -> String {
    let (fm, body) = front_matter(text);
    let name = fm_get(&fm, "name").unwrap_or(fallback_name);
    let desc = fm_get(&fm, "description").unwrap_or("");
    let quote = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    format!("---\nname: {}\ndescription: {}\n---\n\n{}", quote(name), quote(desc), body.trim_start())
}

// ── finding ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct Item {
    pub kind: Kind,
    /// Folder name (skill) or file stem (agent): what identifies it.
    pub name: String,
    /// The `name:` in its front matter, when different.
    pub title: Option<String>,
    pub description: String,
    pub place: String,
    pub path: PathBuf,
    /// Content fingerprint, to tell identical copies from diverged ones.
    pub hash: String,
    pub flavor: Option<Provider>,
}

fn walk_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d)? {
            let e = e?;
            let t = e.file_type()?;
            if t.is_symlink() {
                continue;
            }
            if t.is_dir() {
                stack.push(e.path());
            } else {
                out.push(e.path());
                if out.len() > MAX_FILES {
                    bail!("{} has too many files", dir.display());
                }
            }
        }
    }
    out.sort();
    Ok(())
}

fn fingerprint(path: &Path) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut files = vec![];
    if path.is_dir() {
        let _ = walk_files(path, &mut files);
    } else {
        files.push(path.to_path_buf());
    }
    for f in files {
        f.strip_prefix(path).unwrap_or(&f).to_string_lossy().replace('\\', "/").hash(&mut h);
        std::fs::read(&f).unwrap_or_default().hash(&mut h);
    }
    format!("{:016x}", h.finish())
}

/// The skills and agents in one place.
pub fn scan(place: &Place) -> Vec<Item> {
    let mut out = vec![];
    if let Ok(rd) = std::fs::read_dir(&place.skills) {
        for e in rd.filter_map(|e| e.ok()) {
            let name = e.file_name().to_string_lossy().into_owned();
            let md = e.path().join("SKILL.md");
            if name.starts_with('.') || !md.is_file() {
                continue;
            }
            let text = std::fs::read_to_string(&md).unwrap_or_default();
            let (fm, _) = front_matter(&text);
            out.push(Item {
                kind: Kind::Skill,
                title: fm_get(&fm, "name").filter(|t| *t != name).map(String::from),
                description: fm_get(&fm, "description").unwrap_or("").to_string(),
                place: place.id.clone(),
                hash: fingerprint(&e.path()),
                path: e.path(),
                flavor: None,
                name,
            });
        }
    }
    if let Some(Ok(rd)) = place.agents.as_ref().map(std::fs::read_dir) {
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            let Some(name) = p.file_stem().map(|s| s.to_string_lossy().into_owned()) else { continue };
            if !p.is_file() || p.extension().is_none_or(|x| x != "md") || name.starts_with('.') {
                continue;
            }
            let text = std::fs::read_to_string(&p).unwrap_or_default();
            let (fm, _) = front_matter(&text);
            out.push(Item {
                kind: Kind::Agent,
                title: fm_get(&fm, "name").filter(|t| *t != name).map(String::from),
                description: fm_get(&fm, "description").unwrap_or("").to_string(),
                place: place.id.clone(),
                hash: fingerprint(&p),
                flavor: agent_flavor(&text).or(place.provider),
                path: p,
                name,
            });
        }
    }
    out.sort_by_key(|i| i.name.to_lowercase());
    out
}

// ── copying and removing ────────────────────────────────────────────────────

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    let mut files = vec![];
    walk_files(src, &mut files)?;
    let total: u64 = files.iter().filter_map(|f| f.metadata().ok()).map(|m| m.len()).sum();
    if total > MAX_BYTES {
        bail!("{} is too large to copy", src.display());
    }
    std::fs::create_dir_all(dst)?;
    for f in files {
        let to = dst.join(f.strip_prefix(src)?);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&f, &to).with_context(|| format!("copying {}", f.display()))?;
    }
    Ok(())
}

/// Moves something we're replacing or removing to `~/.fermi/trash` instead
/// of deleting it.
pub fn trash(path: &Path) -> Result<PathBuf> {
    let dir = Config::home().join("trash");
    std::fs::create_dir_all(&dir)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let to = dir.join(format!("{}-{name}", fermi_core::util::now()));
    if std::fs::rename(path, &to).is_err() {
        // Another drive: copy, then delete.
        if path.is_dir() {
            copy_dir(path, &to)?;
            std::fs::remove_dir_all(path)?;
        } else {
            std::fs::copy(path, &to)?;
            std::fs::remove_file(path)?;
        }
    }
    Ok(to)
}

/// Copies one skill or agent; returns a note about anything adapted.
pub fn copy(from: &Place, kind: Kind, name: &str, to: &Place, replace: bool) -> Result<Option<String>> {
    let src = from.path(kind, name)?;
    let dst = to.path(kind, name)?;
    if !src.exists() {
        bail!("{name} isn't in {}", from.label);
    }
    if fermi_core::util::norm_path(&src) == fermi_core::util::norm_path(&dst) {
        bail!("that's where it already is");
    }
    if dst.exists() {
        if !replace {
            bail!("{} already has a {} called {name}", to.label, if kind == Kind::Skill { "skill" } else { "subagent" });
        }
        trash(&dst)?;
    }
    match kind {
        Kind::Skill => {
            copy_dir(&src, &dst)?;
            Ok(None)
        }
        Kind::Agent => {
            let text = std::fs::read_to_string(&src)?;
            std::fs::create_dir_all(to.dir(kind)?)?;
            let flavor = agent_flavor(&text).or(from.provider);
            match (flavor, to.provider) {
                (Some(f), Some(t)) if f != t => {
                    std::fs::write(&dst, convert_agent(&text, name))?;
                    Ok(Some(format!("converted for {}: its tool list and model were left out", cli_name(t))))
                }
                _ => {
                    std::fs::write(&dst, text)?;
                    Ok(None)
                }
            }
        }
    }
}

pub fn remove(place: &Place, kind: Kind, name: &str) -> Result<PathBuf> {
    let p = place.path(kind, name)?;
    if !p.exists() {
        bail!("{name} isn't in {}", place.label);
    }
    trash(&p)
}

// ── bundles ─────────────────────────────────────────────────────────────────

/// Writes a `.zip` with `skills/<name>/…` and `agents/<name>.md`, plus a
/// `fermi-bundle.json` listing them. Any agent CLI's skill folder layout.
pub fn export(items: &[(&Place, Kind, String)], out: &Path) -> Result<usize> {
    let tmp = out.with_extension("zip.fermi-tmp");
    let res = (|| -> Result<usize> {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&tmp)?);
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let mut seen = std::collections::HashSet::new();
        let mut listed = vec![];
        for (place, kind, name) in items {
            if !seen.insert((*kind, name.to_lowercase())) {
                continue; // the same name picked from two places
            }
            let src = place.path(*kind, name)?;
            match kind {
                Kind::Skill => {
                    let mut files = vec![];
                    walk_files(&src, &mut files)?;
                    for f in files {
                        let rel = f.strip_prefix(&src)?.to_string_lossy().replace('\\', "/");
                        zip.start_file(format!("skills/{name}/{rel}"), opts)?;
                        zip.write_all(&std::fs::read(&f)?)?;
                    }
                }
                Kind::Agent => {
                    zip.start_file(format!("agents/{name}.md"), opts)?;
                    zip.write_all(&std::fs::read(&src)?)?;
                }
            }
            listed.push(serde_json::json!({ "kind": kind, "name": name, "from": place.label }));
        }
        zip.start_file("fermi-bundle.json", opts)?;
        zip.write_all(serde_json::to_string_pretty(&serde_json::json!({
            "format": "fermi-skills/1", "exported": fermi_core::util::now(), "items": listed,
        }))?.as_bytes())?;
        zip.finish()?;
        Ok(listed.len())
    })();
    match res {
        Ok(n) => {
            std::fs::rename(&tmp, out).with_context(|| format!("writing {}", out.display()))?;
            Ok(n)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Unpacks a zip safely (no paths outside `dst`, bounded size).
fn unzip(src: &Path, dst: &Path) -> Result<()> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(src)?).context("that isn't a valid .zip file")?;
    if zip.len() > MAX_FILES {
        bail!("that archive has too many files");
    }
    let mut total = 0;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i)?;
        let Some(rel) = f.enclosed_name() else { continue };
        let to = dst.join(rel);
        if f.is_dir() {
            std::fs::create_dir_all(&to)?;
            continue;
        }
        total += f.size();
        if total > MAX_BYTES {
            bail!("that archive is too large");
        }
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut buf = Vec::with_capacity(f.size() as usize);
        f.read_to_end(&mut buf)?;
        std::fs::write(&to, buf)?;
    }
    Ok(())
}

/// Collects skills (folders with a SKILL.md) and subagents (Markdown with a
/// name and description, or anything under an `agents` folder) from `root`
/// into `out/skills` and `out/agents`.
fn collect(root: &Path, out: &Path) -> Result<()> {
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if dir.join("SKILL.md").is_file() {
            let text = std::fs::read_to_string(dir.join("SKILL.md")).unwrap_or_default();
            let (fm, _) = front_matter(&text);
            let folder = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let mut name = sanitize(if dir == root { fm_get(&fm, "name").unwrap_or(&folder) } else { &folder });
            if name.is_empty() {
                name = sanitize(fm_get(&fm, "name").unwrap_or("skill"));
            }
            copy_dir(&dir, &out.join("skills").join(&name))?;
            continue; // its own files aren't agents
        }
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let in_agents = dir.strip_prefix(root).is_ok_and(|r| r.components().any(|c| c.as_os_str().eq_ignore_ascii_case("agents")));
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            let fname = e.file_name().to_string_lossy().into_owned();
            let Ok(t) = e.file_type() else { continue };
            if t.is_dir() {
                if depth < 8 && !fname.starts_with('.') && fname != "node_modules" && fname != "__MACOSX" {
                    stack.push((p, depth + 1));
                }
            } else if t.is_file() && fname.to_lowercase().ends_with(".md") && !fname.eq_ignore_ascii_case("README.md") {
                let text = std::fs::read_to_string(&p).unwrap_or_default();
                let (fm, _) = front_matter(&text);
                if in_agents || (fm_get(&fm, "name").is_some() && fm_get(&fm, "description").is_some()) {
                    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    let name = sanitize(&stem);
                    if !name.is_empty() {
                        std::fs::create_dir_all(out.join("agents"))?;
                        std::fs::write(out.join("agents").join(format!("{name}.md")), text)?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Unpacks `src` (a folder, a `.zip` / `.skill` archive, a SKILL.md or an
/// agent `.md`) into `dir` so it can be reviewed and installed like a place.
pub fn stage(src: &Path, dir: &Path, id: &str) -> Result<Place> {
    std::fs::create_dir_all(dir)?;
    let lower = src.to_string_lossy().to_lowercase();
    if src.is_dir() {
        collect(src, dir)?;
    } else if lower.ends_with(".zip") || lower.ends_with(".skill") {
        let raw = dir.join(".raw");
        unzip(src, &raw)?;
        collect(&raw, dir)?;
        let _ = std::fs::remove_dir_all(&raw);
    } else if lower.ends_with("skill.md") {
        collect(src.parent().context("no folder")?, dir)?;
    } else if lower.ends_with(".md") {
        let name = sanitize(&src.file_stem().unwrap_or_default().to_string_lossy());
        std::fs::create_dir_all(dir.join("agents"))?;
        std::fs::copy(src, dir.join("agents").join(format!("{}.md", if name.is_empty() { "agent" } else { &name })))?;
    } else {
        bail!("pick a .zip or .skill archive, a folder, or a .md file");
    }
    let place = Place {
        id: format!("import:{id}"),
        label: src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "import".into()),
        provider: None,
        scope: "import",
        skills: dir.join("skills"),
        agents: Some(dir.join("agents")),
    };
    if scan(&place).is_empty() {
        bail!("no skills (folders with a SKILL.md) or subagents (.md with a name and description) in there");
    }
    Ok(place)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fermi-skills-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn place(root: &Path, id: &str, provider: Option<Provider>) -> Place {
        Place {
            id: id.into(),
            label: id.into(),
            provider,
            scope: "account",
            skills: root.join("skills"),
            agents: (provider != Some(Provider::Openai)).then(|| root.join("agents")),
        }
    }

    #[test]
    fn front_matter_and_flavors() {
        let claude = "---\nname: reviewer\ndescription: Reviews code\ntools: Read, Grep\nmodel: sonnet\ncolor: blue\n---\nBe strict.\n";
        let gemini = "---\nname: reviewer\ndescription: >\n  Reviews code\n  carefully\nkind: local\ntools:\n  - read_file\n---\nBe strict.\n";
        let (fm, body) = front_matter(gemini);
        assert_eq!(fm_get(&fm, "description"), Some("Reviews code carefully"));
        assert_eq!(body, "Be strict.\n");
        assert_eq!(agent_flavor(claude), Some(Provider::Claude));
        assert_eq!(agent_flavor(gemini), Some(Provider::Gemini));
        assert_eq!(agent_flavor("---\nname: a\ndescription: b\n---\nx"), None);
        let converted = convert_agent(claude, "x");
        assert_eq!(converted, "---\nname: \"reviewer\"\ndescription: \"Reviews code\"\n---\n\nBe strict.\n");
    }

    #[test]
    fn copies_between_places_and_round_trips_a_bundle() {
        let base = tmp("copy");
        let (a, b, c) = (base.join("a"), base.join("b"), base.join("c"));
        std::fs::create_dir_all(a.join("skills/pdf/scripts")).unwrap();
        std::fs::write(a.join("skills/pdf/SKILL.md"), "---\nname: pdf\ndescription: Work with PDFs\n---\nSteps").unwrap();
        std::fs::write(a.join("skills/pdf/scripts/x.py"), "print(1)").unwrap();
        std::fs::create_dir_all(a.join("agents")).unwrap();
        std::fs::write(a.join("agents/reviewer.md"), "---\nname: reviewer\ndescription: Reviews\ntools: Read\n---\nBe strict.").unwrap();
        let (pa, pb, pc) = (place(&a, "a", Some(Provider::Claude)), place(&b, "b", Some(Provider::Gemini)), place(&c, "c", Some(Provider::Openai)));

        let items = scan(&pa);
        assert_eq!(items.iter().map(|i| (i.kind, i.name.as_str())).collect::<Vec<_>>(), [(Kind::Skill, "pdf"), (Kind::Agent, "reviewer")]);

        assert!(copy(&pa, Kind::Skill, "pdf", &pb, false).unwrap().is_none());
        assert!(b.join("skills/pdf/scripts/x.py").is_file());
        assert_eq!(scan(&pb)[0].hash, items[0].hash, "identical copies fingerprint the same");
        assert!(copy(&pa, Kind::Skill, "pdf", &pb, false).is_err(), "won't overwrite without asking");
        assert!(copy(&pa, Kind::Agent, "reviewer", &pb, false).unwrap().is_some(), "converted for Gemini");
        assert!(!std::fs::read_to_string(b.join("agents/reviewer.md")).unwrap().contains("tools"));
        assert!(copy(&pa, Kind::Agent, "reviewer", &pc, false).is_err(), "Codex has no subagents");
        assert!(copy(&pa, Kind::Skill, "../evil", &pb, false).is_err());

        let zip = base.join("bundle.zip");
        assert_eq!(export(&[(&pa, Kind::Skill, "pdf".into()), (&pa, Kind::Agent, "reviewer".into())], &zip).unwrap(), 2);
        let staged = stage(&zip, &base.join("stage"), "t").unwrap();
        let got: Vec<_> = scan(&staged).into_iter().map(|i| (i.kind, i.name)).collect();
        assert_eq!(got, [(Kind::Skill, "pdf".to_string()), (Kind::Agent, "reviewer".to_string())]);
        assert!(base.join("stage/skills/pdf/scripts/x.py").is_file());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn names() {
        assert!(check_name("pdf-tools").is_ok());
        assert!(check_name("../x").is_err() && check_name(".hidden").is_err() && check_name("a/b").is_err());
        assert_eq!(sanitize("My Skill!"), "My-Skill");
    }
}
