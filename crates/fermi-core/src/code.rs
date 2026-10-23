//! Code intelligence: a symbol index of a repository built with tree-sitter,
//! with the relationships an agent needs before changing code.
//!
//! - Symbols (functions, methods, types…) with signatures and doc comments.
//! - References (calls, type uses) resolved to definitions, giving a call
//!   graph: who calls a function and what it calls.
//! - Imports resolved per language (Rust modules and workspace crates,
//!   Python packages, JS/TS relative paths, Go module packages) into a file
//!   dependency graph, plus external dependencies from the manifests.
//! - Tests: which tests reach which code through the call graph, and line
//!   coverage when an `lcov.info` report exists.
//! - Change impact: from a git diff, the changed symbols, everything that
//!   calls or imports them (transitively) and the tests to run.
//!
//! Indexing is incremental (only changed files are parsed), parallel and
//! respects `.gitignore`; the index is cached under `~/.fermi/index`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tree_sitter_tags::{TagsConfiguration, TagsContext};

const MAX_FILES: usize = 40_000;
const MAX_FILE_BYTES: u64 = 1_000_000;
/// Bump when the stored format or the extraction changes.
const FORMAT: u32 = 5;

// ── languages ───────────────────────────────────────────────────────────────

struct LangDef {
    id: &'static str,
    exts: &'static [&'static str],
}

const LANGS: [LangDef; 6] = [
    LangDef { id: "rust", exts: &["rs"] },
    LangDef { id: "python", exts: &["py", "pyi"] },
    LangDef { id: "javascript", exts: &["js", "jsx", "mjs", "cjs"] },
    LangDef { id: "typescript", exts: &["ts", "mts", "cts"] },
    LangDef { id: "tsx", exts: &["tsx"] },
    LangDef { id: "go", exts: &["go"] },
];

/// Extra patterns on top of each grammar's own tags query: imports, more
/// call shapes, constants. (`@local.reference` is a capture the tags crate
/// ignores, used here only to test a node's text.)
const RUST_EXTRA: &str = r#"
(call_expression function: (scoped_identifier name: (identifier) @name)) @reference.path
(call_expression function: (generic_function function: (identifier) @name)) @reference.call
(call_expression function: (generic_function function: (scoped_identifier name: (identifier) @name))) @reference.path
(call_expression function: (generic_function function: (field_expression field: (field_identifier) @name))) @reference.method
(call_expression function: (field_expression field: (field_identifier) @name)) @reference.method
(struct_expression name: (type_identifier) @name) @reference.class
(const_item name: (identifier) @name) @definition.constant
(static_item name: (identifier) @name) @definition.constant
(use_declaration argument: (_) @name) @reference.import
(mod_item name: (identifier) @name !body) @reference.modfile
(token_tree (identifier) @name . (token_tree)) @reference.call
(token_tree "." . (identifier) @name . (token_tree)) @reference.method
(impl_item type: (type_identifier) @name) @definition.impl
(impl_item type: (generic_type type: (type_identifier) @name)) @definition.impl
"#;
const PY_EXTRA: &str = r#"
(call function: (attribute attribute: (identifier) @name)) @reference.method
(import_statement name: (dotted_name) @name) @reference.import
(import_statement name: (aliased_import name: (dotted_name) @name)) @reference.import
(import_from_statement module_name: (_) @name) @reference.import
"#;
const JS_EXTRA: &str = r#"
(call_expression function: (member_expression property: (property_identifier) @name)) @reference.method
(import_statement source: (string) @name) @reference.import
(export_statement source: (string) @name) @reference.import
(call_expression function: (identifier) @local.reference arguments: (arguments (string) @name) (#eq? @local.reference "require")) @reference.import
"#;
const GO_EXTRA: &str = r#"
(call_expression function: (selector_expression field: (field_identifier) @name)) @reference.method
(import_spec path: (interpreted_string_literal) @name) @reference.import
"#;

fn configs() -> &'static Vec<Option<TagsConfiguration>> {
    static C: OnceLock<Vec<Option<TagsConfiguration>>> = OnceLock::new();
    C.get_or_init(|| {
        // Our patterns go first: for the same name the first matching pattern wins.
        let js = format!("{JS_EXTRA}{}", tree_sitter_javascript::TAGS_QUERY);
        let ts = format!("{JS_EXTRA}{}{}", tree_sitter_javascript::TAGS_QUERY, tree_sitter_typescript::TAGS_QUERY);
        let make = |lang: tree_sitter::Language, q: &str| {
            TagsConfiguration::new(lang, q, "").map_err(|e| eprintln!("fermi: code index query: {e:?}")).ok()
        };
        vec![
            make(tree_sitter_rust::LANGUAGE.into(), &format!("{RUST_EXTRA}{}", tree_sitter_rust::TAGS_QUERY)),
            make(tree_sitter_python::LANGUAGE.into(), &format!("{PY_EXTRA}{}", tree_sitter_python::TAGS_QUERY)),
            make(tree_sitter_javascript::LANGUAGE.into(), &js),
            make(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), &ts),
            make(tree_sitter_typescript::LANGUAGE_TSX.into(), &ts),
            make(tree_sitter_go::LANGUAGE.into(), &format!("{GO_EXTRA}{}", tree_sitter_go::TAGS_QUERY)),
        ]
    })
}

fn lang_of(path: &str) -> Option<usize> {
    let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
    LANGS.iter().position(|l| l.exts.contains(&ext.as_str()))
}

/// Test files by the usual conventions of each ecosystem.
pub fn is_test_file(rel: &str) -> bool {
    let l = rel.to_lowercase();
    let name = l.rsplit('/').next().unwrap_or(&l);
    l.split('/').any(|seg| matches!(seg, "tests" | "test" | "__tests__" | "spec" | "testing"))
        || name.starts_with("test_")
        || name.ends_with("_test.py")
        || name.ends_with("_test.go")
        || name.ends_with("_tests.rs")
        || [".test.", ".spec."].iter().any(|m| name.contains(m))
}

// ── the stored index ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sym {
    pub name: String,
    /// function, method, class, interface, module, constant, macro, type…
    pub kind: String,
    /// 1-based lines.
    pub line: u32,
    pub end: u32,
    pub sig: String,
    #[serde(default)]
    pub doc: Option<String>,
    /// Index of the enclosing symbol (a method's class).
    #[serde(default)]
    pub parent: Option<u32>,
    #[serde(default)]
    pub test: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ref {
    pub name: String,
    pub kind: String,
    pub line: u32,
    /// Index of the innermost symbol it's in.
    #[serde(default)]
    pub from: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileIdx {
    pub lang: String,
    pub mtime: u64,
    pub size: u64,
    pub syms: Vec<Sym>,
    pub refs: Vec<Ref>,
    /// Import specs as written (`crate::a::b`, `./util`, `os.path`), with their line.
    pub imports: Vec<(String, u32)>,
    pub test: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct Stored {
    format: u32,
    files: BTreeMap<String, FileIdx>,
    manifests: BTreeMap<String, String>,
    /// Files seen but not indexed (no supported language), so nobody
    /// mistakes them for covered code.
    #[serde(default)]
    unsupported: Vec<String>,
    /// The git commit the index was last brought up to date with.
    #[serde(default)]
    revision: Option<String>,
}

/// What the call graph and impact analysis can and can't see.
pub const SCOPE: &str = "Static analysis of indexed files (Rust, Python, JavaScript, TypeScript, Go): calls resolved by name \
    within the file, then through imports. Not covered: dynamic dispatch (trait objects, interfaces, callbacks, reflection), \
    macros that generate calls, and code outside the index (dependencies, generated or unsupported files).";

/// A symbol's place: (file number, symbol number). `u32::MAX` as the symbol
/// stands for a file's top-level code.
pub type SymId = (u32, u32);
const TOP: u32 = u32::MAX;

#[derive(Default)]
struct Graph {
    paths: Vec<String>,
    file_no: HashMap<String, u32>,
    by_name: HashMap<String, Vec<SymId>>,
    /// Resolved file imports and the files importing each file.
    deps: HashMap<u32, Vec<u32>>,
    importers: HashMap<u32, Vec<u32>>,
    /// External packages each file imports.
    externals: HashMap<u32, Vec<String>>,
    callees: HashMap<SymId, Vec<(SymId, u32)>>,
    callers: HashMap<SymId, Vec<(SymId, u32)>>,
    /// Tests that reach a symbol through calls.
    tested_by: HashMap<SymId, Vec<SymId>>,
    /// Line hits from an lcov report, per file.
    coverage: HashMap<u32, HashMap<u32, u64>>,
}

pub struct Index {
    pub root: PathBuf,
    stored: Stored,
    graph: Graph,
    pub updated: std::time::Instant,
}

pub struct UpdateStats {
    pub files: usize,
    pub parsed: usize,
    pub removed: usize,
    pub millis: u128,
}

fn cache_path(root: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    crate::util::norm_path(root).hash(&mut h);
    crate::config::Config::home().join("index").join(format!("{:016x}.json", h.finish()))
}

fn mtime(m: &std::fs::Metadata) -> u64 {
    m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64).unwrap_or(0)
}

const MANIFESTS: [&str; 6] = ["Cargo.toml", "package.json", "go.mod", "pyproject.toml", "requirements.txt", "setup.py"];

impl Index {
    /// Loads the cached index of `root` and brings it up to date.
    pub fn open(root: &Path) -> Result<Index> {
        let root = canonical(root)?;
        let stored: Stored = std::fs::read(cache_path(&root))
            .ok()
            .and_then(|b| serde_json::from_slice::<Stored>(&b).ok())
            .filter(|s| s.format == FORMAT)
            .unwrap_or(Stored { format: FORMAT, ..Default::default() });
        let mut idx = Index { root, stored, graph: Graph::default(), updated: std::time::Instant::now() };
        idx.update()?;
        Ok(idx)
    }

    /// Re-parses changed files, drops deleted ones and rebuilds the graph.
    pub fn update(&mut self) -> Result<UpdateStats> {
        let started = std::time::Instant::now();
        let mut seen: Vec<(String, u64, u64)> = vec![];
        let mut manifests = BTreeMap::new();
        let mut unsupported: Vec<String> = vec![];
        let walker = ignore::WalkBuilder::new(&self.root).hidden(true).git_ignore(true).git_global(false).parents(true).build();
        for e in walker.filter_map(|e| e.ok()) {
            if !e.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let p = e.path();
            let Ok(rel) = p.strip_prefix(&self.root) else { continue };
            let rel = rel.to_string_lossy().replace('\\', "/");
            let name = rel.rsplit('/').next().unwrap_or("");
            if MANIFESTS.contains(&name) && !rel.contains("node_modules/") {
                if let Ok(text) = std::fs::read_to_string(p) {
                    manifests.insert(rel.clone(), text);
                }
                continue;
            }
            let vendored =
                rel.split('/').any(|seg| matches!(seg, "node_modules" | "vendor" | "third_party" | "third-party" | "dist" | "build"));
            if lang_of(&rel).is_none() || vendored || rel.ends_with(".min.js") {
                if !vendored && unsupported.len() < 2000 {
                    unsupported.push(rel);
                }
                continue;
            }
            let Ok(m) = e.metadata() else { continue };
            if m.len() > MAX_FILE_BYTES {
                continue;
            }
            seen.push((rel, mtime(&m), m.len()));
            if seen.len() >= MAX_FILES {
                break;
            }
        }
        let present: HashSet<&str> = seen.iter().map(|s| s.0.as_str()).collect();
        let before = self.stored.files.len();
        self.stored.files.retain(|k, _| present.contains(k.as_str()));
        let removed = before - self.stored.files.len();
        let todo: Vec<&(String, u64, u64)> =
            seen.iter().filter(|(rel, mt, sz)| self.stored.files.get(rel).is_none_or(|f| f.mtime != *mt || f.size != *sz)).collect();
        let parsed = todo.len();
        let root = self.root.clone();
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(12);
        let chunk = todo.len().div_ceil(threads).max(1);
        let mut results: Vec<(String, FileIdx)> = vec![];
        std::thread::scope(|s| {
            let jobs: Vec<_> = todo
                .chunks(chunk)
                .map(|part| {
                    let root = &root;
                    s.spawn(move || {
                        let mut ctx = TagsContext::new();
                        part.iter()
                            .filter_map(|(rel, mt, sz)| {
                                let src = std::fs::read(root.join(rel)).ok()?;
                                let mut f = parse_file(&mut ctx, lang_of(rel)?, &src, rel)?;
                                f.mtime = *mt;
                                f.size = *sz;
                                Some((rel.clone(), f))
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            for j in jobs {
                results.extend(j.join().unwrap_or_default());
            }
        });
        unsupported.sort();
        let revision = git_head(&self.root);
        let changed = parsed > 0
            || removed > 0
            || manifests != self.stored.manifests
            || unsupported != self.stored.unsupported
            || revision != self.stored.revision;
        self.stored.unsupported = unsupported;
        self.stored.revision = revision;
        for (rel, f) in results {
            self.stored.files.insert(rel, f);
        }
        self.stored.manifests = manifests;
        if changed || self.graph.paths.is_empty() {
            self.build_graph();
            let path = cache_path(&self.root);
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(b) = serde_json::to_vec(&self.stored) {
                let tmp = path.with_extension("tmp");
                if std::fs::write(&tmp, b).is_ok() {
                    let _ = std::fs::rename(&tmp, &path);
                }
            }
        }
        self.updated = std::time::Instant::now();
        Ok(UpdateStats { files: self.stored.files.len(), parsed, removed, millis: started.elapsed().as_millis() })
    }
}

// ── parsing one file ────────────────────────────────────────────────────────

fn line_starts(src: &[u8]) -> Vec<usize> {
    std::iter::once(0).chain(src.iter().enumerate().filter(|(_, b)| **b == b'\n').map(|(i, _)| i + 1)).collect()
}

fn line_at(starts: &[usize], byte: usize) -> u32 {
    (starts.partition_point(|s| *s <= byte)) as u32
}

fn kind_name(k: &str) -> &str {
    match k {
        "call" | "class" | "type" | "implementation" | "interface" => k,
        other => other,
    }
}

fn parse_file(ctx: &mut TagsContext, lang: usize, src: &[u8], rel: &str) -> Option<FileIdx> {
    let config = configs()[lang].as_ref()?;
    let (tags, _) = ctx.generate_tags(config, src, None).ok()?;
    let starts = line_starts(src);
    let text = String::from_utf8_lossy(src);
    let lines: Vec<&str> = text.lines().collect();
    let mut defs: Vec<(usize, usize, Sym)> = vec![];
    let mut refs: Vec<(usize, Ref)> = vec![];
    let mut imports = vec![];
    let mut seen_defs = HashSet::new();
    for tag in tags.filter_map(|t| t.ok()) {
        let kind = kind_name(config.syntax_type_name(tag.syntax_type_id)).to_string();
        let name = String::from_utf8_lossy(&src[tag.name_range.clone()]).into_owned();
        let line = line_at(&starts, if tag.is_definition { tag.range.start } else { tag.name_range.start });
        if tag.is_definition {
            if !seen_defs.insert((tag.range.start, name.clone())) {
                continue; // a method also matches the plain function pattern
            }
            let end = line_at(&starts, tag.range.end.saturating_sub(1).max(tag.range.start));
            let first = lines.get(line as usize - 1).copied().unwrap_or("").trim();
            let sig = first.split(" {").next().unwrap_or(first).trim_end_matches('{').trim().chars().take(180).collect::<String>();
            let doc = tag.docs.clone().filter(|d| !d.trim().is_empty()).or_else(|| doc_above(&lines, line as usize, LANGS[lang].id));
            defs.push((tag.range.start, tag.range.end, Sym { name, kind, line, end, sig, doc, parent: None, test: false }));
        } else if kind == "import" || kind == "modfile" {
            let spec = if kind == "modfile" {
                format!("mod:{name}")
            } else {
                name.trim_matches(|c| c == '"' || c == '\'' || c == '`').to_string()
            };
            imports.push((spec, line));
        } else {
            refs.push((tag.name_range.start, Ref { name, kind, line, from: None }));
        }
    }
    // Nesting: each definition's parent, each reference's innermost definition.
    defs.sort_by_key(|d| (d.0, std::cmp::Reverse(d.1)));
    let mut stack: Vec<usize> = vec![];
    for i in 0..defs.len() {
        while stack.last().is_some_and(|&p| defs[p].1 <= defs[i].0) {
            stack.pop();
        }
        defs[i].2.parent = stack.last().map(|&p| p as u32);
        stack.push(i);
    }
    let test_file = is_test_file(rel);
    for i in 0..defs.len() {
        let parent_kind = defs[i].2.parent.map(|p| defs[p as usize].2.kind.clone());
        let d = &mut defs[i].2;
        if d.kind == "function" && matches!(parent_kind.as_deref(), Some("class" | "interface")) {
            d.kind = "method".into();
        }
        // Rust tags functions in any `{ … }` list as methods, modules included.
        if d.kind == "method" && !matches!(parent_kind.as_deref(), Some("impl" | "interface" | "class")) {
            d.kind = "function".into();
        }
        d.test = is_test_sym(d, test_file, &lines, LANGS[lang].id);
    }
    for (at, r) in refs.iter_mut() {
        r.from = defs.iter().enumerate().filter(|(_, d)| d.0 <= *at && *at < d.1).max_by_key(|(_, d)| d.0).map(|(i, _)| i as u32);
    }
    Some(FileIdx {
        lang: LANGS[lang].id.to_string(),
        mtime: 0,
        size: 0,
        syms: defs.into_iter().map(|d| d.2).collect(),
        refs: refs.into_iter().map(|r| r.1).collect(),
        imports,
        test: test_file,
    })
}

/// Comment lines right above a definition (or a Python docstring under it).
fn doc_above(lines: &[&str], line: usize, lang: &str) -> Option<String> {
    let mut out: Vec<String> = vec![];
    let mut i = line.saturating_sub(1);
    while i > 0 {
        let l = lines[i - 1].trim();
        let attr = l.starts_with("#[") || l.starts_with('@');
        let doc = ["///", "//!", "//", "*", "/**", "/*", "#"].iter().any(|p| l.starts_with(p)) && !(lang == "rust" && l.starts_with("#["));
        if attr {
            i -= 1;
            continue;
        }
        if !doc || l.is_empty() {
            break;
        }
        let t = l.trim_start_matches(['/', '*', '!', '#']).trim();
        if !t.is_empty() && !t.starts_with('@') {
            out.push(t.to_string());
        }
        i -= 1;
    }
    out.reverse();
    if out.is_empty() && lang == "python" {
        // def f():\n    """Docstring."""
        let next = lines.get(line).map(|l| l.trim()).unwrap_or("");
        for q in ["\"\"\"", "'''"] {
            if let Some(rest) = next.strip_prefix(q) {
                let mut body = vec![rest.trim_end_matches(q).trim().to_string()];
                if !rest.contains(q) {
                    for l in lines.iter().skip(line + 1).take(20) {
                        if let Some((before, _)) = l.split_once(q) {
                            body.push(before.trim().to_string());
                            break;
                        }
                        body.push(l.trim().to_string());
                    }
                }
                out = body.into_iter().filter(|s| !s.is_empty()).collect();
            }
        }
    }
    let doc = out.join(" ");
    (!doc.is_empty()).then(|| doc.chars().take(600).collect())
}

fn is_test_sym(d: &Sym, test_file: bool, lines: &[&str], lang: &str) -> bool {
    if !matches!(d.kind.as_str(), "function" | "method") {
        return false;
    }
    let above = (d.line.saturating_sub(4) as usize..d.line as usize).filter_map(|i| lines.get(i.wrapping_sub(1))).any(|l| {
        let l = l.trim();
        l.starts_with("#[test") || l.starts_with("#[tokio::test") || l.contains("::test]") || l.starts_with("#[rstest")
    });
    match lang {
        "rust" => above,
        "go" => test_file && (d.name.starts_with("Test") || d.name.starts_with("Benchmark") || d.name.starts_with("Fuzz")),
        "python" => test_file && d.name.starts_with("test"),
        _ => test_file && d.name.to_lowercase().starts_with("test"),
    }
}

// ── resolving imports and calls ─────────────────────────────────────────────

impl Index {
    fn files(&self) -> &BTreeMap<String, FileIdx> {
        &self.stored.files
    }

    fn sym(&self, id: SymId) -> Option<(&str, &Sym)> {
        let path = self.graph.paths.get(id.0 as usize)?;
        let f = self.stored.files.get(path)?;
        f.syms.get(id.1 as usize).map(|s| (path.as_str(), s))
    }

    fn build_graph(&mut self) {
        let mut g = Graph::default();
        for (n, path) in self.stored.files.keys().enumerate() {
            g.paths.push(path.clone());
            g.file_no.insert(path.clone(), n as u32);
        }
        for (n, f) in self.stored.files.values().enumerate() {
            for (k, s) in f.syms.iter().enumerate() {
                if s.kind != "module" && s.kind != "impl" {
                    g.by_name.entry(s.name.clone()).or_default().push((n as u32, k as u32));
                }
            }
        }
        // Imports.
        let ctx = ResolveCtx::new(&self.stored);
        for (n, (path, f)) in self.stored.files.iter().enumerate() {
            let mut deps: Vec<u32> = vec![];
            let mut ext: Vec<String> = vec![];
            for (spec, _) in &f.imports {
                match ctx.resolve(&f.lang, path, spec) {
                    Resolved::Files(list) => deps.extend(list.iter().filter_map(|p| g.file_no.get(p)).copied()),
                    Resolved::External(e) => ext.push(e),
                    Resolved::Unknown => {}
                }
            }
            // Go files in one package see each other without imports.
            if f.lang == "go" {
                let dir = path.rsplit_once('/').map(|x| x.0).unwrap_or("");
                deps.extend(
                    self.stored
                        .files
                        .keys()
                        .filter(|p| *p != path && p.rsplit_once('/').map(|x| x.0).unwrap_or("") == dir && p.ends_with(".go"))
                        .filter_map(|p| g.file_no.get(p))
                        .copied(),
                );
            }
            deps.sort_unstable();
            deps.dedup();
            deps.retain(|d| *d != n as u32);
            ext.sort();
            ext.dedup();
            for d in &deps {
                g.importers.entry(*d).or_default().push(n as u32);
            }
            if !deps.is_empty() {
                g.deps.insert(n as u32, deps);
            }
            if !ext.is_empty() {
                g.externals.insert(n as u32, ext);
            }
        }
        // Calls: a reference resolves to definitions in the same file, else in
        // imported files, else anywhere when the name is rare enough.
        for (n, f) in self.stored.files.values().enumerate() {
            let n = n as u32;
            let imported: HashSet<u32> = g.deps.get(&n).map(|d| d.iter().copied().collect()).unwrap_or_default();
            for r in &f.refs {
                let Some(all) = g.by_name.get(&r.name) else { continue };
                let from = (n, r.from.unwrap_or(TOP));
                let method = r.kind == "method";
                // `x.name()` can only be a method, of this file or one it imports.
                let cands: Vec<SymId> = if method {
                    let files = &self.stored.files;
                    all.iter()
                        .copied()
                        .filter(|c| {
                            g.paths
                                .get(c.0 as usize)
                                .and_then(|p| files.get(p))
                                .and_then(|f| f.syms.get(c.1 as usize))
                                .is_some_and(|s| s.kind == "method" || s.parent.is_some())
                        })
                        .collect()
                } else {
                    all.clone()
                };
                let cands = &cands;
                let mut local: Vec<SymId> = cands.iter().filter(|c| c.0 == n).copied().collect();
                // `self.save()` in a method of Hub means Hub's save, not another type's.
                if method && local.len() > 1 {
                    let owner_of = |k: u32| f.syms.get(k as usize).and_then(|s| s.parent);
                    if let Some(owner) = r.from.and_then(owner_of) {
                        let same: Vec<SymId> = local.iter().copied().filter(|c| owner_of(c.1) == Some(owner)).collect();
                        if !same.is_empty() {
                            local = same;
                        }
                    }
                }
                let targets: Vec<SymId> = if !local.is_empty() {
                    local
                } else {
                    let via: Vec<SymId> = cands.iter().filter(|c| imported.contains(&c.0)).copied().collect();
                    if !via.is_empty() {
                        via
                    } else if r.kind != "path" && !method && cands.len() <= 2 {
                        cands.clone()
                    } else {
                        continue; // too common a name to guess
                    }
                };
                for t in targets {
                    if t == from {
                        continue;
                    }
                    g.callees.entry(from).or_default().push((t, r.line));
                    g.callers.entry(t).or_default().push((from, r.line));
                }
            }
        }
        for v in g.callees.values_mut().chain(g.callers.values_mut()) {
            v.sort_unstable();
            v.dedup_by_key(|x| x.0);
        }
        // Tests reach code through calls (a few levels deep).
        let mut roots: Vec<SymId> = vec![];
        for (n, f) in self.stored.files.values().enumerate() {
            for (k, s) in f.syms.iter().enumerate() {
                if s.test {
                    roots.push((n as u32, k as u32));
                }
            }
            if f.test {
                roots.push((n as u32, TOP));
            }
        }
        for root in roots {
            let mut seen = HashSet::from([root]);
            let mut q = VecDeque::from([(root, 0)]);
            while let Some((s, d)) = q.pop_front() {
                for (t, _) in g.callees.get(&s).into_iter().flatten() {
                    if d < 6 && seen.insert(*t) {
                        let list = g.tested_by.entry(*t).or_default();
                        if list.len() < 30 {
                            list.push(root);
                        }
                        q.push_back((*t, d + 1));
                    }
                }
            }
        }
        // Line coverage from an lcov report, if there is one.
        for name in ["lcov.info", "coverage/lcov.info", "target/lcov.info", "coverage.lcov"] {
            let Ok(text) = std::fs::read_to_string(self.root.join(name)) else { continue };
            let mut cur: Option<u32> = None;
            for l in text.lines() {
                if let Some(sf) = l.strip_prefix("SF:") {
                    let p = PathBuf::from(sf);
                    let rel = p.strip_prefix(&self.root).unwrap_or(&p).to_string_lossy().replace('\\', "/");
                    cur = g.file_no.get(rel.trim_start_matches("./")).copied();
                } else if let (Some(f), Some(da)) = (cur, l.strip_prefix("DA:")) {
                    let mut it = da.split(',');
                    if let (Some(Ok(line)), Some(Ok(hits))) = (it.next().map(str::parse::<u32>), it.next().map(str::parse::<u64>)) {
                        g.coverage.entry(f).or_default().insert(line, hits);
                    }
                }
            }
            break;
        }
        self.graph = g;
    }
}

enum Resolved {
    Files(Vec<String>),
    External(String),
    Unknown,
}

struct ResolveCtx<'a> {
    files: HashSet<&'a str>,
    /// Rust crate name → its `src` folder.
    crates: HashMap<String, String>,
    go_module: Option<(String, String)>,
}

fn dir_of(p: &str) -> &str {
    p.rsplit_once('/').map(|x| x.0).unwrap_or("")
}

fn join(dir: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = if dir.is_empty() { vec![] } else { dir.split('/').collect() };
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

impl<'a> ResolveCtx<'a> {
    fn new(s: &'a Stored) -> Self {
        let mut crates = HashMap::new();
        let mut go_module = None;
        for (path, text) in &s.manifests {
            let dir = dir_of(path);
            if path.ends_with("Cargo.toml") {
                let name =
                    text.lines().skip_while(|l| l.trim() != "[package]").skip(1).take_while(|l| !l.trim().starts_with('[')).find_map(|l| {
                        let (k, v) = l.split_once('=')?;
                        (k.trim() == "name").then(|| v.trim().trim_matches('"').replace('-', "_"))
                    });
                if let Some(n) = name {
                    crates.insert(n, join(dir, "src"));
                }
            } else if path.ends_with("go.mod") {
                if let Some(m) = text.lines().find_map(|l| l.trim().strip_prefix("module ")) {
                    go_module = Some((m.trim().to_string(), dir.to_string()));
                }
            }
        }
        ResolveCtx { files: s.files.keys().map(String::as_str).collect(), crates, go_module }
    }

    fn has(&self, p: &str) -> bool {
        self.files.contains(p)
    }

    fn resolve(&self, lang: &str, file: &str, spec: &str) -> Resolved {
        match lang {
            "rust" => self.rust(file, spec),
            "python" => self.python(file, spec),
            "go" => self.go(spec),
            _ => self.js(file, spec),
        }
    }

    /// The folder holding a Rust file's submodules.
    fn rust_mod_dir(file: &str) -> String {
        let name = file.rsplit('/').next().unwrap_or("");
        if matches!(name, "mod.rs" | "lib.rs" | "main.rs") {
            dir_of(file).to_string()
        } else {
            file.trim_end_matches(".rs").to_string()
        }
    }

    fn rust_find(&self, dir: &str, segs: &[&str]) -> Option<String> {
        let mut dir = dir.to_string();
        let mut found = None;
        for seg in segs {
            let a = join(&dir, &format!("{seg}.rs"));
            let b = join(&dir, &format!("{seg}/mod.rs"));
            if self.has(&a) {
                found = Some(a);
                dir = join(&dir, seg);
            } else if self.has(&b) {
                found = Some(b);
                dir = join(&dir, seg);
            } else {
                break; // the rest are items in the module found so far
            }
        }
        found
    }

    fn rust(&self, file: &str, spec: &str) -> Resolved {
        if let Some(m) = spec.strip_prefix("mod:") {
            let dir = Self::rust_mod_dir(file);
            return self.rust_find(&dir, &[m]).map(|f| Resolved::Files(vec![f])).unwrap_or(Resolved::Unknown);
        }
        let path = spec.split("::{").next().unwrap_or(spec).split(" as ").next().unwrap_or(spec).trim();
        let segs: Vec<&str> = path.split("::").map(str::trim).filter(|s| !s.is_empty()).collect();
        let Some(first) = segs.first().copied() else { return Resolved::Unknown };
        let crate_src = |f: &str| -> Option<String> {
            let mut d = dir_of(f).to_string();
            loop {
                if d.ends_with("src") || d == "src" {
                    return Some(d);
                }
                if d.is_empty() {
                    return None;
                }
                d = dir_of(&d).to_string();
            }
        };
        let (base, rest) = match first {
            "crate" => (crate_src(file), &segs[1..]),
            "self" => (Some(Self::rust_mod_dir(file)), &segs[1..]),
            "super" => {
                let mut dir = dir_of(&Self::rust_mod_dir(file)).to_string();
                let mut i = 1;
                while segs.get(i) == Some(&"super") {
                    dir = dir_of(&dir).to_string();
                    i += 1;
                }
                (Some(dir), &segs[i..])
            }
            "std" | "core" | "alloc" => return Resolved::Unknown,
            name => match self.crates.get(name) {
                Some(src) => (Some(src.clone()), &segs[1..]),
                None => return Resolved::External(name.to_string()),
            },
        };
        let Some(base) = base else { return Resolved::Unknown };
        match self.rust_find(&base, rest) {
            Some(f) => Resolved::Files(vec![f]),
            None => ["lib.rs", "main.rs", "mod.rs"]
                .iter()
                .map(|r| join(&base, r))
                .find(|p| self.has(p))
                .map(|p| Resolved::Files(vec![p]))
                .unwrap_or(Resolved::Unknown),
        }
    }

    fn python(&self, file: &str, spec: &str) -> Resolved {
        let dots = spec.chars().take_while(|c| *c == '.').count();
        let module = &spec[dots..];
        let rel_path = module.replace('.', "/");
        let mut bases: Vec<String> = vec![];
        if dots > 0 {
            let mut d = dir_of(file).to_string();
            for _ in 1..dots {
                d = dir_of(&d).to_string();
            }
            bases.push(d);
        } else {
            bases.extend(["".to_string(), "src".into(), "lib".into()]);
            let top = file.split('/').next().unwrap_or("");
            if file.contains('/') {
                bases.push(top.to_string());
            }
        }
        for base in &bases {
            let mut p = rel_path.clone();
            loop {
                for cand in [join(base, &format!("{p}.py")), join(base, &format!("{p}/__init__.py")), join(base, &format!("{p}.pyi"))] {
                    if self.has(&cand) {
                        return Resolved::Files(vec![cand]);
                    }
                }
                match p.rsplit_once('/') {
                    Some((head, _)) => p = head.to_string(),
                    None => break,
                }
            }
        }
        if dots == 0 && !module.is_empty() {
            Resolved::External(module.split('.').next().unwrap_or(module).to_string())
        } else {
            Resolved::Unknown
        }
    }

    fn js(&self, file: &str, spec: &str) -> Resolved {
        if spec.starts_with('.') || spec.starts_with('/') {
            let p = join(dir_of(file), spec);
            let stem = p.trim_end_matches(".js").trim_end_matches(".jsx").trim_end_matches(".mjs");
            let mut cands = vec![p.clone()];
            for base in [p.as_str(), stem] {
                for ext in ["ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts"] {
                    cands.push(format!("{base}.{ext}"));
                    cands.push(format!("{base}/index.{ext}"));
                }
            }
            return cands.into_iter().find(|c| self.has(c)).map(|c| Resolved::Files(vec![c])).unwrap_or(Resolved::Unknown);
        }
        if spec.starts_with("node:") {
            return Resolved::Unknown;
        }
        let mut parts = spec.split('/');
        let first = parts.next().unwrap_or(spec);
        let pkg = if first.starts_with('@') { format!("{first}/{}", parts.next().unwrap_or("")) } else { first.to_string() };
        Resolved::External(pkg)
    }

    fn go(&self, spec: &str) -> Resolved {
        if let Some((module, dir)) = &self.go_module {
            if let Some(rest) = spec.strip_prefix(module.as_str()) {
                let d = join(dir, rest.trim_start_matches('/'));
                let files: Vec<String> = self
                    .files
                    .iter()
                    .filter(|f| dir_of(f) == d && f.ends_with(".go") && !f.ends_with("_test.go"))
                    .map(|f| f.to_string())
                    .collect();
                return if files.is_empty() { Resolved::Unknown } else { Resolved::Files(files) };
            }
        }
        if spec.split('/').next().is_some_and(|s| s.contains('.')) {
            Resolved::External(spec.split('/').take(3).collect::<Vec<_>>().join("/"))
        } else {
            Resolved::Unknown // the standard library
        }
    }
}

// ── queries ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymRef {
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
    pub end: u32,
    pub sig: String,
    pub test: bool,
    /// The enclosing type, for methods.
    pub owner: Option<String>,
}

impl Index {
    fn sym_ref(&self, id: SymId) -> Option<SymRef> {
        if id.1 == TOP {
            let file = self.graph.paths.get(id.0 as usize)?.clone();
            let test = self.files().get(&file).is_some_and(|f| f.test);
            return Some(SymRef {
                name: "(top level)".into(),
                kind: "file".into(),
                line: 1,
                end: 1,
                sig: String::new(),
                test,
                owner: None,
                file,
            });
        }
        let (file, s) = self.sym(id)?;
        let owner =
            s.parent.and_then(|p| self.files().get(file)?.syms.get(p as usize)).filter(|p| p.kind != "module").map(|p| p.name.clone());
        Some(SymRef {
            name: s.name.clone(),
            kind: s.kind.clone(),
            file: file.to_string(),
            line: s.line,
            end: s.end,
            sig: s.sig.clone(),
            test: s.test,
            owner,
        })
    }

    pub fn stats(&self) -> Value {
        let mut langs: BTreeMap<&str, usize> = BTreeMap::new();
        let (mut syms, mut tests, mut funcs, mut reached) = (0, 0, 0, 0);
        for (n, f) in self.files().values().enumerate() {
            *langs.entry(f.lang.as_str()).or_default() += 1;
            syms += f.syms.len();
            for (k, s) in f.syms.iter().enumerate() {
                if s.test {
                    tests += 1;
                } else if !f.test && matches!(s.kind.as_str(), "function" | "method") {
                    funcs += 1;
                    if self.graph.tested_by.contains_key(&(n as u32, k as u32)) {
                        reached += 1;
                    }
                }
            }
        }
        let edges: usize = self.graph.callees.values().map(Vec::len).sum();
        let imports: usize = self.graph.deps.values().map(Vec::len).sum();
        json!({
            "root": self.root, "files": self.files().len(), "languages": langs, "symbols": syms, "calls": edges, "imports": imports,
            "tests": tests, "functions": funcs, "reached_by_tests": reached,
            "test_reach": if funcs > 0 { (reached as f64 / funcs as f64 * 100.0).round() } else { 0.0 },
            "lcov": !self.graph.coverage.is_empty(),
            "unsupported": self.stored.unsupported, "revision": self.stored.revision, "scope": SCOPE,
        })
    }

    /// Symbols matching a name: exact, then prefix, then substring.
    pub fn search(&self, query: &str, limit: usize) -> Vec<SymRef> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return vec![];
        }
        let mut hits: Vec<(u8, u8, SymId)> = vec![];
        for (n, f) in self.files().values().enumerate() {
            for (k, s) in f.syms.iter().enumerate().filter(|(_, s)| s.kind != "impl") {
                let name = s.name.to_lowercase();
                let rank = if name == q {
                    0
                } else if name.starts_with(&q) {
                    1
                } else if name.contains(&q) {
                    2
                } else {
                    continue;
                };
                let kind = match s.kind.as_str() {
                    "function" | "method" => 0,
                    "class" | "interface" | "type" => 1,
                    _ => 2,
                } + if s.test || f.test { 3 } else { 0 };
                hits.push((rank, kind, (n as u32, k as u32)));
            }
        }
        hits.sort();
        hits.into_iter().take(limit).filter_map(|h| self.sym_ref(h.2)).collect()
    }

    fn find(&self, target: &str) -> Vec<SymId> {
        // `path:line` picks the innermost symbol around that line.
        if let Some((file, line)) = target.rsplit_once(':').and_then(|(f, l)| Some((f, l.parse::<u32>().ok()?))) {
            let file = file.replace('\\', "/");
            if let (Some(n), Some(f)) = (self.graph.file_no.get(&file), self.files().get(&file)) {
                return f
                    .syms
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| s.line <= line && line <= s.end)
                    .max_by_key(|(_, s)| s.line)
                    .map(|(k, _)| vec![(*n, k as u32)])
                    .unwrap_or_default();
            }
        }
        // `Type::method` or `Type.method`.
        let (owner, name) = match target.rsplit_once("::").or_else(|| target.rsplit_once('.')) {
            Some((o, n)) if !o.is_empty() && !n.is_empty() => (Some(o.rsplit(['.', ':']).next().unwrap_or(o)), n),
            _ => (None, target),
        };
        let mut ids: Vec<SymId> = self.graph.by_name.get(name).cloned().unwrap_or_default();
        if let Some(o) = owner {
            let owned: Vec<SymId> =
                ids.iter().copied().filter(|id| self.sym_ref(*id).and_then(|r| r.owner).as_deref() == Some(o)).collect();
            if !owned.is_empty() {
                ids = owned;
            } else {
                // A module path (`util::text::helper`): keep definitions whose file
                // matches it, e.g. `util/src/text.rs` or `text/mod.rs`.
                let segs: Vec<&str> = target.split("::").flat_map(|s| s.split('.')).collect();
                let modules = &segs[..segs.len() - 1];
                let in_module: Vec<SymId> = ids
                    .iter()
                    .copied()
                    .filter(|id| {
                        let path = self.graph.paths.get(id.0 as usize).map(String::as_str).unwrap_or("");
                        let parts: Vec<&str> = path.trim_end_matches(".rs").trim_end_matches("/mod").split(['/', '.']).collect();
                        let last = modules.last().copied().unwrap_or("");
                        parts.last() == Some(&last) && modules.iter().all(|m| parts.contains(m) || *m == "crate")
                    })
                    .collect();
                if !in_module.is_empty() {
                    ids = in_module;
                }
            }
        }
        ids
    }

    fn body(&self, file: &str, from: u32, to: u32, max: u32) -> String {
        let Ok(text) = std::fs::read_to_string(self.root.join(file)) else { return String::new() };
        let lines: Vec<&str> = text.lines().collect();
        let end = to.min(from + max - 1);
        let mut out: Vec<&str> = lines.iter().skip(from as usize - 1).take((end + 1 - from) as usize).copied().collect();
        if end < to {
            out.push("    …");
        }
        out.join("\n")
    }

    fn tests_for(&self, id: SymId) -> Vec<SymRef> {
        let mut out: Vec<SymRef> = self.graph.tested_by.get(&id).into_iter().flatten().filter_map(|t| self.sym_ref(*t)).collect();
        // Test files importing the symbol's file count too.
        for imp in self.graph.importers.get(&id.0).into_iter().flatten() {
            if self.graph.paths.get(*imp as usize).and_then(|p| self.files().get(p)).is_some_and(|f| f.test)
                && !out.iter().any(|t| t.file == self.graph.paths[*imp as usize])
            {
                if let Some(r) = self.sym_ref((*imp, TOP)) {
                    out.push(r);
                }
            }
        }
        out.truncate(30);
        out
    }

    fn coverage_of(&self, id: SymId) -> Option<f64> {
        let cov = self.graph.coverage.get(&id.0)?;
        let (_, s) = self.sym(id)?;
        let lines: Vec<u64> = (s.line..=s.end).filter_map(|l| cov.get(&l).copied()).collect();
        (!lines.is_empty()).then(|| (lines.iter().filter(|h| **h > 0).count() as f64 / lines.len() as f64 * 100.0).round())
    }

    /// Everything known about a symbol (by name, `Type::name` or `path:line`).
    pub fn symbol(&self, target: &str) -> Value {
        let ids = self.find(target);
        let details: Vec<Value> = ids
            .iter()
            .take(6)
            .filter_map(|id| {
                let r = self.sym_ref(*id)?;
                let (_, s) = self.sym(*id)?;
                let callers: Vec<Value> = self
                    .graph
                    .callers
                    .get(id)
                    .into_iter()
                    .flatten()
                    .take(60)
                    .filter_map(|(c, line)| Some(json!({ "caller": self.sym_ref(*c)?, "line": line })))
                    .collect();
                let callees: Vec<SymRef> =
                    self.graph.callees.get(id).into_iter().flatten().take(60).filter_map(|(c, _)| self.sym_ref(*c)).collect();
                let unresolved: Vec<String> = {
                    let f = self.files().get(&r.file)?;
                    let known: HashSet<&str> = callees.iter().map(|c| c.name.as_str()).collect();
                    let mut v: Vec<String> = f
                        .refs
                        .iter()
                        .filter(|x| x.from == Some(id.1) && x.kind == "call" && !known.contains(x.name.as_str()))
                        .map(|x| x.name.clone())
                        .collect();
                    v.sort();
                    v.dedup();
                    v.truncate(40);
                    v
                };
                let importers: Vec<&String> = self
                    .graph
                    .importers
                    .get(&id.0)
                    .into_iter()
                    .flatten()
                    .filter_map(|i| self.graph.paths.get(*i as usize))
                    .take(40)
                    .collect();
                Some(json!({
                    "symbol": r, "doc": s.doc, "body": self.body(&r.file, s.line, s.end, 80),
                    "callers": callers, "callers_total": self.graph.callers.get(id).map(Vec::len).unwrap_or(0),
                    "callees": callees, "external_calls": unresolved,
                    "tests": self.tests_for(*id), "coverage": self.coverage_of(*id),
                    "file_importers": importers,
                }))
            })
            .collect();
        json!({ "query": target, "matches": ids.len(), "symbols": details, "scope": SCOPE })
    }

    /// A file's outline, imports and importers.
    pub fn file(&self, rel: &str) -> Value {
        let rel = rel.replace('\\', "/");
        let rel = rel.trim_start_matches("./");
        let Some((n, f)) = self.graph.file_no.get(rel).and_then(|n| Some((*n, self.files().get(rel)?))) else {
            return json!({ "error": format!("{rel} isn't in the index") });
        };
        let outline: Vec<Value> = f.syms.iter().map(|s| json!({ "name": s.name, "kind": s.kind, "line": s.line, "end": s.end, "parent": s.parent, "test": s.test, "sig": s.sig })).collect();
        let deps: Vec<&String> = self.graph.deps.get(&n).into_iter().flatten().filter_map(|i| self.graph.paths.get(*i as usize)).collect();
        let importers: Vec<&String> =
            self.graph.importers.get(&n).into_iter().flatten().filter_map(|i| self.graph.paths.get(*i as usize)).collect();
        json!({ "file": rel, "lang": f.lang, "test": f.test, "symbols": outline, "imports": deps, "external": self.graph.externals.get(&n), "imported_by": importers })
    }

    /// External dependencies from the manifests, and which files use them.
    pub fn deps(&self) -> Value {
        let mut declared: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (path, text) in &self.stored.manifests {
            for d in manifest_deps(path, text) {
                declared.entry(d).or_default().push(path.clone());
            }
        }
        let mut used: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (n, ext) in &self.graph.externals {
            for e in ext {
                used.entry(e.clone()).or_default().push(self.graph.paths[*n as usize].clone());
            }
        }
        let norm = |s: &str| s.replace('-', "_").to_lowercase();
        let mut rows: Vec<Value> = vec![];
        for (name, manifests) in &declared {
            let files = used.iter().find(|(u, _)| norm(u) == norm(name)).map(|x| x.1.clone()).unwrap_or_default();
            rows.push(
                json!({ "name": name, "manifests": manifests, "used_in": files.len(), "files": files.iter().take(20).collect::<Vec<_>>() }),
            );
        }
        let undeclared: Vec<&String> = used.keys().filter(|u| !declared.keys().any(|d| norm(d) == norm(u))).collect();
        json!({ "dependencies": rows, "imported_but_not_declared": undeclared })
    }

    /// What a change could break: the changed symbols, everything that calls
    /// or imports them (transitively), and the tests to run. `changes` maps
    /// files to changed line ranges (empty = the whole file).
    pub fn impact(&self, changes: &BTreeMap<String, Vec<(u32, u32)>>) -> Value {
        let mut changed: Vec<SymId> = vec![];
        let mut changed_files: Vec<u32> = vec![];
        for (file, ranges) in changes {
            let Some(n) = self.graph.file_no.get(file).copied() else { continue };
            changed_files.push(n);
            let f = &self.files()[file];
            let mut hit = false;
            for (k, s) in f.syms.iter().enumerate() {
                let overlaps = ranges.is_empty() || ranges.iter().any(|(a, b)| *a <= s.end && s.line <= *b);
                // Innermost only: skip a class when one of its methods changed.
                let inner_changed = f
                    .syms
                    .iter()
                    .any(|c| c.parent == Some(k as u32) && (ranges.is_empty() || ranges.iter().any(|(a, b)| *a <= c.end && c.line <= *b)));
                // A whole new file: its top-level symbols stand for it.
                let shown = if ranges.is_empty() { s.parent.is_none() } else { !inner_changed };
                if overlaps && shown && !matches!(s.kind.as_str(), "impl" | "module") {
                    changed.push((n, k as u32));
                    hit = true;
                }
            }
            if !hit {
                changed.push((n, TOP));
            }
        }
        // Upstream through callers.
        let mut depth: HashMap<SymId, (u32, SymId)> = HashMap::new();
        let mut q: VecDeque<(SymId, u32)> = changed.iter().map(|c| (*c, 0)).collect();
        for c in &changed {
            depth.insert(*c, (0, *c));
        }
        while let Some((s, d)) = q.pop_front() {
            if d >= 5 || depth.len() > 600 {
                continue;
            }
            for (caller, _) in self.graph.callers.get(&s).into_iter().flatten() {
                if !depth.contains_key(caller) {
                    depth.insert(*caller, (d + 1, s));
                    q.push_back((*caller, d + 1));
                }
            }
        }
        // Files importing the changed files (two levels).
        let mut files: HashMap<u32, u32> = changed_files.iter().map(|f| (*f, 0)).collect();
        let mut fq: VecDeque<(u32, u32)> = changed_files.iter().map(|f| (*f, 0)).collect();
        while let Some((f, d)) = fq.pop_front() {
            if d >= 2 {
                continue;
            }
            for i in self.graph.importers.get(&f).into_iter().flatten() {
                if !files.contains_key(i) {
                    files.insert(*i, d + 1);
                    fq.push_back((*i, d + 1));
                }
            }
        }
        for s in depth.keys() {
            files.entry(s.0).or_insert(9);
        }
        // Tests: the ones reaching changed code, test code among the callers, test files importing what changed.
        let mut tests: BTreeMap<String, SymRef> = BTreeMap::new();
        let mut add_test = |r: SymRef| {
            tests.entry(format!("{}:{}", r.file, r.line)).or_insert(r);
        };
        for c in &changed {
            for t in self.tests_for(*c) {
                add_test(t);
            }
        }
        for s in depth.keys() {
            if let Some(r) = self.sym_ref(*s).filter(|r| r.test) {
                add_test(r);
            }
        }
        for (f, _) in files.iter().filter(|(_, d)| **d <= 2) {
            if let Some(r) = self.sym_ref((*f, TOP)).filter(|r| r.test) {
                add_test(r);
            }
        }
        let untested: Vec<SymRef> = changed
            .iter()
            .filter(|c| c.1 != TOP && self.tests_for(**c).is_empty())
            .filter_map(|c| self.sym_ref(*c))
            .filter(|r| !r.test && matches!(r.kind.as_str(), "function" | "method"))
            .collect();
        let mut impacted: Vec<(u32, SymId, SymId)> =
            depth.iter().filter(|(_, (d, _))| *d > 0).map(|(s, (d, via))| (*d, *s, *via)).collect();
        impacted.sort();
        let impacted_json: Vec<Value> = impacted
            .iter()
            .take(200)
            .filter_map(|(d, s, via)| Some(json!({ "symbol": self.sym_ref(*s)?, "depth": d, "via": self.sym_ref(*via).map(|v| v.name) })))
            .collect();
        let mut file_list: Vec<(u32, &String)> = files.iter().map(|(f, d)| (*d, &self.graph.paths[*f as usize])).collect();
        file_list.sort();
        let risk = match (impacted.len(), untested.len()) {
            (n, u) if n > 40 || u > 3 => "high",
            (n, u) if n > 8 || u > 0 => "medium",
            _ => "low",
        };
        json!({
            "changed": changed.iter().filter_map(|c| self.sym_ref(*c)).collect::<Vec<_>>(),
            "impacted": impacted_json, "impacted_total": impacted.len(),
            "files": file_list.iter().map(|(d, f)| json!({ "file": f, "depth": if *d == 9 { None } else { Some(*d) } })).collect::<Vec<_>>(),
            "tests": tests.into_values().collect::<Vec<_>>(),
            "untested_changes": untested,
            "risk": risk,
            "scope": SCOPE,
        })
    }

    /// Changed line ranges from git: working tree against `base` (default HEAD), plus new files.
    pub fn git_changes(&self, base: Option<&str>) -> Result<BTreeMap<String, Vec<(u32, u32)>>> {
        let git = |args: &[&str]| -> Result<String> {
            let mut cmd = std::process::Command::new("git");
            cmd.arg("-C").arg(&self.root).args(args);
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(0x0800_0000);
            }
            let out = cmd.output().context("running git")?;
            if !out.status.success() {
                anyhow::bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
            }
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        };
        let top = git(&["rev-parse", "--show-toplevel"])?;
        let prefix = Path::new(top.trim())
            .canonicalize()
            .ok()
            .and_then(|t| self.root.canonicalize().ok()?.strip_prefix(t).ok().map(|p| p.to_string_lossy().replace('\\', "/")));
        let strip = |p: &str| -> Option<String> {
            match prefix.as_deref().filter(|x| !x.is_empty()) {
                Some(pre) => p.strip_prefix(pre).map(|r| r.trim_start_matches('/').to_string()),
                None => Some(p.to_string()),
            }
        };
        let mut out: BTreeMap<String, Vec<(u32, u32)>> = BTreeMap::new();
        let diff = git(&["diff", "-U0", "--no-color", "--no-ext-diff", base.unwrap_or("HEAD")])?;
        let mut cur: Option<String> = None;
        for l in diff.lines() {
            if let Some(p) = l.strip_prefix("+++ b/") {
                cur = strip(p);
                if let Some(c) = &cur {
                    out.entry(c.clone()).or_default();
                }
            } else if l.starts_with("+++ /dev/null") {
                cur = None;
            } else if let (Some(c), Some(h)) = (&cur, l.strip_prefix("@@ ")) {
                // @@ -a,b +c,d @@
                if let Some(plus) = h.split_whitespace().find(|x| x.starts_with('+')) {
                    let mut it = plus[1..].split(',');
                    let start: u32 = it.next().and_then(|x| x.parse().ok()).unwrap_or(1);
                    let len: u32 = it.next().and_then(|x| x.parse().ok()).unwrap_or(1);
                    out.entry(c.clone()).or_default().push((start.max(1), (start + len.max(1) - 1).max(1)));
                }
            }
        }
        for p in git(&["ls-files", "--others", "--exclude-standard"])?.lines() {
            if let Some(p) = strip(p) {
                out.insert(p, vec![]);
            }
        }
        out.retain(|f, _| self.graph.file_no.contains_key(f));
        Ok(out)
    }
}

/// Declared dependency names in a manifest.
fn manifest_deps(path: &str, text: &str) -> Vec<String> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let mut out = vec![];
    match name {
        "Cargo.toml" => {
            let mut in_deps = false;
            for l in text.lines() {
                let t = l.trim();
                if t.starts_with('[') {
                    in_deps = t.contains("dependencies") && !t.contains("dependencies.");
                    if let Some(d) = t.strip_prefix("[dependencies.").or_else(|| t.strip_prefix("[dev-dependencies.")) {
                        out.push(d.trim_end_matches(']').to_string());
                    }
                    continue;
                }
                if in_deps {
                    if let Some((k, _)) = t.split_once('=') {
                        let k = k.trim().trim_matches('"');
                        if !k.is_empty() && !k.starts_with('#') && !k.contains('.') {
                            out.push(k.to_string());
                        }
                    }
                }
            }
        }
        "package.json" => {
            if let Ok(v) = serde_json::from_str::<Value>(text) {
                for key in ["dependencies", "devDependencies", "peerDependencies"] {
                    if let Some(o) = v[key].as_object() {
                        out.extend(o.keys().cloned());
                    }
                }
            }
        }
        "go.mod" => {
            let mut block = false;
            for l in text.lines() {
                let t = l.trim();
                if t.starts_with("require (") {
                    block = true;
                } else if block && t == ")" {
                    block = false;
                } else if let Some(r) = t.strip_prefix("require ") {
                    out.extend(r.split_whitespace().next().map(String::from));
                } else if block && !t.is_empty() && !t.starts_with("//") {
                    out.extend(t.split_whitespace().next().map(String::from));
                }
            }
        }
        "requirements.txt" => {
            for l in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('-')) {
                let n: String = l.chars().take_while(|c| c.is_alphanumeric() || "-_.".contains(*c)).collect();
                if !n.is_empty() {
                    out.push(n);
                }
            }
        }
        "pyproject.toml" => {
            let mut in_list = false;
            for l in text.lines() {
                let t = l.trim();
                if t.starts_with("dependencies") && t.contains('[') {
                    in_list = !t.contains(']');
                    for item in t.split('"').skip(1).step_by(2) {
                        out.push(item.chars().take_while(|c| c.is_alphanumeric() || "-_.".contains(*c)).collect());
                    }
                } else if in_list {
                    if t.starts_with(']') {
                        in_list = false;
                    }
                    for item in t.split('"').skip(1).step_by(2) {
                        out.push(item.chars().take_while(|c| c.is_alphanumeric() || "-_.".contains(*c)).collect());
                    }
                }
            }
        }
        _ => {}
    }
    out.retain(|d| !d.is_empty());
    out.sort();
    out.dedup();
    out
}

/// The current commit of a folder's repository, if it is one.
fn git_head(dir: &Path) -> Option<String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(dir).args(["rev-parse", "HEAD"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd.output().ok().filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// A folder's full path, without Windows' `\\?\` prefix.
pub fn canonical(p: &Path) -> Result<PathBuf> {
    let c = std::fs::canonicalize(p).with_context(|| format!("{} not found", p.display()))?;
    Ok(PathBuf::from(c.to_string_lossy().trim_start_matches(r"\\?\")))
}

/// The repository a folder belongs to (its git top level), else the folder.
pub fn repo_root(dir: &Path) -> PathBuf {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(dir).args(["rev-parse", "--show-toplevel"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    match cmd.output() {
        Ok(o) if o.status.success() => PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()),
        _ => dir.to_path_buf(),
    }
}

// ── text for agents and the terminal ────────────────────────────────────────

fn loc(r: &SymRef) -> String {
    format!("{}:{}", r.file, r.line)
}

fn label(r: &SymRef) -> String {
    match &r.owner {
        Some(o) => format!("{o}::{}", r.name),
        None => r.name.clone(),
    }
}

/// Plain-text renderings of the queries, compact enough for an agent.
pub mod text {
    use super::*;

    pub fn search(hits: &[SymRef]) -> String {
        if hits.is_empty() {
            return "No symbols match.".into();
        }
        hits.iter().map(|h| format!("{} {} — {}  {}", h.kind, label(h), loc(h), h.sig)).collect::<Vec<_>>().join("\n")
    }

    pub fn symbol(v: &Value) -> String {
        let syms = v["symbols"].as_array().cloned().unwrap_or_default();
        if syms.is_empty() {
            return format!("No symbol named `{}` in the index. Try code_search.", v["query"].as_str().unwrap_or(""));
        }
        let mut out = String::new();
        if v["matches"].as_u64().unwrap_or(0) > syms.len() as u64 {
            out += &format!("{} definitions match; showing {}. Use Type::name or path:line to pick one.\n\n", v["matches"], syms.len());
        }
        for s in syms {
            let r: SymRef = serde_json::from_value(s["symbol"].clone()).unwrap_or_else(|_| SymRef {
                name: "?".into(),
                kind: "?".into(),
                file: "?".into(),
                line: 0,
                end: 0,
                sig: String::new(),
                test: false,
                owner: None,
            });
            out += &format!("## {} {} ({}–{})\n", r.kind, label(&r), loc(&r), r.end);
            if let Some(d) = s["doc"].as_str() {
                out += &format!("Doc: {d}\n");
            }
            let callers = s["callers"].as_array().cloned().unwrap_or_default();
            out += &format!("Called by ({}):", s["callers_total"]);
            if callers.is_empty() {
                out += " nothing found in the repo (may be an entry point, called dynamically, or unused)\n";
            } else {
                out += "\n";
                for c in callers.iter().take(25) {
                    out += &format!(
                        "  - {} {} at {}:{}\n",
                        c["caller"]["kind"].as_str().unwrap_or(""),
                        c["caller"]["name"].as_str().unwrap_or(""),
                        c["caller"]["file"].as_str().unwrap_or(""),
                        c["line"]
                    );
                }
            }
            let callees: Vec<String> = s["callees"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|c| format!("{} ({}:{})", c["name"].as_str().unwrap_or(""), c["file"].as_str().unwrap_or(""), c["line"]))
                .collect();
            if !callees.is_empty() {
                out += &format!("Calls: {}\n", callees.join(", "));
            }
            let ext: Vec<&str> = s["external_calls"].as_array().into_iter().flatten().filter_map(|x| x.as_str()).collect();
            if !ext.is_empty() {
                out += &format!("Also calls (outside the repo or unresolved): {}\n", ext.join(", "));
            }
            let tests: Vec<String> = s["tests"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|t| format!("{} ({}:{})", t["name"].as_str().unwrap_or(""), t["file"].as_str().unwrap_or(""), t["line"]))
                .collect();
            out +=
                &if tests.is_empty() { "Tests: none reach it\n".to_string() } else { format!("Tests reaching it: {}\n", tests.join(", ")) };
            if let Some(c) = s["coverage"].as_f64() {
                out += &format!("Line coverage (lcov): {c}%\n");
            }
            out += &format!("```\n{}\n```\n\n", s["body"].as_str().unwrap_or(""));
        }
        out
    }

    pub fn impact(v: &Value) -> String {
        let list = |k: &str| v[k].as_array().cloned().unwrap_or_default();
        let changed = list("changed");
        if changed.is_empty() {
            return "No indexed code changed.".into();
        }
        let mut out = format!(
            "Risk: {} ({} symbols changed, {} callers affected).\n\nChanged:\n",
            v["risk"].as_str().unwrap_or("?"),
            changed.len(),
            v["impacted_total"]
        );
        for c in &changed {
            out += &format!(
                "  - {} {} at {}:{}\n",
                c["kind"].as_str().unwrap_or(""),
                c["name"].as_str().unwrap_or(""),
                c["file"].as_str().unwrap_or(""),
                c["line"]
            );
        }
        let imp = list("impacted");
        if !imp.is_empty() {
            out += "\nCould break (callers, nearest first):\n";
            for i in imp.iter().take(40) {
                let s = &i["symbol"];
                out += &format!(
                    "  - {} {} at {}:{} (depth {}, via {})\n",
                    s["kind"].as_str().unwrap_or(""),
                    s["name"].as_str().unwrap_or(""),
                    s["file"].as_str().unwrap_or(""),
                    s["line"],
                    i["depth"],
                    i["via"].as_str().unwrap_or("")
                );
            }
        }
        let files: Vec<String> = list("files")
            .iter()
            .filter(|f| f["depth"].as_u64().is_some_and(|d| d > 0))
            .map(|f| f["file"].as_str().unwrap_or("").to_string())
            .collect();
        if !files.is_empty() {
            out += &format!("\nFiles importing what changed: {}\n", files.join(", "));
        }
        let tests = list("tests");
        out += &if tests.is_empty() {
            "\nTests to run: none found that reach this code.\n".to_string()
        } else {
            format!(
                "\nTests to run:\n{}\n",
                tests
                    .iter()
                    .map(|t| format!("  - {} ({}:{})", t["name"].as_str().unwrap_or(""), t["file"].as_str().unwrap_or(""), t["line"]))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        };
        let untested = list("untested_changes");
        if !untested.is_empty() {
            out += &format!(
                "\nChanged without any test reaching it: {}\n",
                untested
                    .iter()
                    .map(|u| format!("{} ({}:{})", u["name"].as_str().unwrap_or(""), u["file"].as_str().unwrap_or(""), u["line"]))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        out
    }

    pub fn file(v: &Value) -> String {
        if let Some(e) = v["error"].as_str() {
            return e.to_string();
        }
        let mut out = format!(
            "{} ({}{})\n",
            v["file"].as_str().unwrap_or(""),
            v["lang"].as_str().unwrap_or(""),
            if v["test"] == true { ", tests" } else { "" }
        );
        for s in v["symbols"].as_array().into_iter().flatten() {
            let indent = if s["parent"].is_null() { "  " } else { "    " };
            out += &format!(
                "{indent}{} {} :{}  {}\n",
                s["kind"].as_str().unwrap_or(""),
                s["name"].as_str().unwrap_or(""),
                s["line"],
                s["sig"].as_str().unwrap_or("")
            );
        }
        let join = |k: &str| v[k].as_array().into_iter().flatten().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ");
        out += &format!("Imports: {}\nExternal: {}\nImported by: {}\n", join("imports"), join("external"), join("imported_by"));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn every_query_compiles() {
        for (i, c) in configs().iter().enumerate() {
            assert!(c.is_some(), "tags query for {} failed to compile", LANGS[i].id);
        }
    }

    #[test]
    fn rust_project_graph() {
        let dir = std::env::temp_dir().join(format!("fermi-code-rs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write(&dir, "Cargo.toml", "[package]\nname = \"demo\"\n\n[dependencies]\nserde = \"1\"\n");
        write(&dir, "src/lib.rs", "pub mod math;\nuse crate::math::add;\n\n/// Sums twice.\npub fn twice(a: i32) -> i32 {\n    add(a, a)\n}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n    #[test]\n    fn twice_works() {\n        assert_eq!(twice(2), 4);\n    }\n}\n");
        write(&dir, "src/math.rs", "use serde::Serialize;\n\n/// Adds two numbers.\npub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\npub struct Calc;\nimpl Calc {\n    pub fn run(&self) -> i32 { add(1, 2) }\n}\n");
        let idx = Index::open(&dir).unwrap();
        let st = idx.stats();
        assert_eq!(st["files"], 2);
        let add = idx.symbol("add");
        let s = &add["symbols"][0];
        assert_eq!(s["doc"], "Adds two numbers.");
        let callers: Vec<&str> = s["callers"].as_array().unwrap().iter().map(|c| c["caller"]["name"].as_str().unwrap()).collect();
        assert!(callers.contains(&"twice") && callers.contains(&"run"), "{callers:?}");
        let tests: Vec<&str> = s["tests"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(tests.contains(&"twice_works"), "tests reach add through twice: {tests:?}");
        assert_eq!(idx.symbol("Calc::run")["symbols"][0]["symbol"]["owner"], "Calc");
        let f = idx.file("src/lib.rs");
        assert!(f["imports"].as_array().unwrap().iter().any(|x| x == "src/math.rs"));
        let math = idx.file("src/math.rs");
        assert_eq!(math["external"][0], "serde");
        // Changing `add` reaches `twice` and `run`, and the test.
        let mut changes = BTreeMap::new();
        changes.insert("src/math.rs".to_string(), vec![(5, 5)]);
        let imp = idx.impact(&changes);
        let names: Vec<&str> = imp["impacted"].as_array().unwrap().iter().map(|i| i["symbol"]["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"twice") && names.contains(&"run"), "{names:?}");
        assert!(imp["tests"].as_array().unwrap().iter().any(|t| t["name"] == "twice_works"));
        let deps = idx.deps();
        assert_eq!(deps["dependencies"][0]["name"], "serde");
        assert_eq!(deps["dependencies"][0]["used_in"], 1);
        // Incremental: nothing to parse the second time.
        let mut idx = idx;
        assert_eq!(idx.update().unwrap().parsed, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn python_and_typescript() {
        let dir = std::env::temp_dir().join(format!("fermi-code-mix-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write(&dir, "app/util.py", "def slug(s):\n    \"\"\"Makes a URL slug.\"\"\"\n    return s.lower()\n\nclass Store:\n    def save(self, x):\n        return slug(x)\n");
        write(&dir, "app/main.py", "from app.util import slug\nimport requests\n\ndef run():\n    return slug('A')\n");
        write(&dir, "tests/test_util.py", "from app.util import slug\n\ndef test_slug():\n    assert slug('A') == 'a'\n");
        write(
            &dir,
            "web/lib.ts",
            "export function fmt(n: number): string {\n  return n.toFixed(2);\n}\nexport interface Row { id: number }\n",
        );
        write(
            &dir,
            "web/view.tsx",
            "import { fmt } from './lib';\nimport React from 'react';\nexport function View() {\n  return <b>{fmt(1)}</b>;\n}\n",
        );
        let idx = Index::open(&dir).unwrap();
        let s = &idx.symbol("slug")["symbols"][0];
        assert_eq!(s["doc"], "Makes a URL slug.");
        let callers: Vec<&str> = s["callers"].as_array().unwrap().iter().map(|c| c["caller"]["name"].as_str().unwrap()).collect();
        assert!(callers.contains(&"run") && callers.contains(&"save") && callers.contains(&"test_slug"), "{callers:?}");
        assert_eq!(idx.symbol("Store.save")["symbols"][0]["symbol"]["kind"], "method");
        assert!(idx.file("app/main.py")["external"].as_array().unwrap().iter().any(|e| e == "requests"));
        let fmt = &idx.symbol("fmt")["symbols"][0];
        assert_eq!(fmt["callers"][0]["caller"]["name"], "View");
        assert!(idx.file("web/view.tsx")["imports"].as_array().unwrap().iter().any(|f| f == "web/lib.ts"));
        assert!(idx.file("web/view.tsx")["external"].as_array().unwrap().iter().any(|e| e == "react"));
        assert_eq!(idx.search("fm", 5)[0].name, "fmt");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
