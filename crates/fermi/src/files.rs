//! File access for the dashboard's explorer and editor. Every path is
//! canonicalized and must sit inside one of the user's workspace folders, so
//! `..` or links can't reach anything else.

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Text files above this open read-only as "too large".
const MAX_TEXT: u64 = 2 * 1024 * 1024;
/// Raw downloads (images, …) above this are refused.
pub const MAX_RAW: u64 = 64 * 1024 * 1024;
/// Folders skipped by search (still listed in the tree).
const SKIP_DIRS: &[&str] = &[".git", "node_modules", "target", "dist", "build", ".next", "__pycache__", ".venv", "venv"];

/// `canonicalize` without Windows' `\\?\` prefix, so paths compare and display normally.
pub fn canon(p: &Path) -> Option<PathBuf> {
    let c = std::fs::canonicalize(p).ok()?;
    #[cfg(windows)]
    {
        let s = c.to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            if rest.as_bytes().get(1) == Some(&b':') {
                return Some(PathBuf::from(rest));
            }
        }
    }
    Some(c)
}

/// Resolves `path` (existing) and checks it is inside a root.
pub fn check(roots: &[PathBuf], path: &str) -> Result<PathBuf> {
    let c = canon(Path::new(path)).ok_or_else(|| anyhow!("not found: {path}"))?;
    if roots.iter().any(|r| c.starts_with(r)) {
        Ok(c)
    } else {
        bail!("that path is outside your workspaces")
    }
}

/// Like `check`, for a file that may not exist yet (its folder must).
pub fn check_new(roots: &[PathBuf], path: &str) -> Result<PathBuf> {
    let p = Path::new(path);
    if p.exists() {
        return check(roots, path);
    }
    let name = p.file_name().ok_or_else(|| anyhow!("not a file path"))?;
    let parent = p.parent().ok_or_else(|| anyhow!("not a file path"))?;
    let dir = check(roots, &parent.to_string_lossy())?;
    Ok(dir.join(name))
}

#[derive(Serialize)]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub dir: bool,
    pub size: u64,
}

pub fn list(dir: &Path) -> Result<Vec<Entry>> {
    let mut out: Vec<Entry> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            Some(Entry {
                name: e.file_name().to_string_lossy().into_owned(),
                path: e.path().to_string_lossy().into_owned(),
                dir: meta.is_dir(),
                size: if meta.is_dir() { 0 } else { meta.len() },
            })
        })
        .take(5000)
        .collect();
    out.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(out)
}

#[derive(Serialize)]
pub struct FileRead {
    pub path: String,
    pub size: u64,
    /// None for binary or too-large files.
    pub text: Option<String>,
    pub binary: bool,
    pub too_large: bool,
}

pub fn read(path: &Path) -> Result<FileRead> {
    let meta = std::fs::metadata(path)?;
    if meta.is_dir() {
        bail!("that's a folder");
    }
    let base = FileRead { path: path.to_string_lossy().into_owned(), size: meta.len(), text: None, binary: false, too_large: false };
    if meta.len() > MAX_TEXT {
        return Ok(FileRead { too_large: true, ..base });
    }
    let bytes = std::fs::read(path)?;
    let looks_binary = bytes.iter().take(8000).any(|b| *b == 0);
    match (looks_binary, String::from_utf8(bytes)) {
        (false, Ok(text)) => Ok(FileRead { text: Some(text), ..base }),
        _ => Ok(FileRead { binary: true, ..base }),
    }
}

pub fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("bmp") => "image/bmp",
        Some("avif") => "image/avif",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    }
}

#[derive(Serialize)]
pub struct Hit {
    pub path: String,
    /// Path relative to the searched root, `/`-separated.
    pub rel: String,
    pub line: Option<usize>,
    pub text: Option<String>,
}

/// Finds files under `root` whose relative path contains `q`, or (contents)
/// whose text contains it. Case-insensitive, bounded.
pub fn search(root: &Path, q: &str, contents: bool) -> Vec<Hit> {
    let q = q.to_lowercase();
    let (mut hits, mut seen) = (vec![], 0usize);
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.filter_map(|e| e.ok()) {
            let path = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                if !SKIP_DIRS.contains(&e.file_name().to_string_lossy().as_ref()) {
                    stack.push(path);
                }
                continue;
            }
            seen += 1;
            if seen > 30_000 || hits.len() >= 200 {
                return hits;
            }
            let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            if !contents {
                if rel.to_lowercase().contains(&q) {
                    hits.push(Hit { path: path.to_string_lossy().into_owned(), rel, line: None, text: None });
                }
                continue;
            }
            if e.metadata().map(|m| m.len() > 1024 * 1024).unwrap_or(true) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            if let Some((i, line)) = text.lines().enumerate().find(|(_, l)| l.to_lowercase().contains(&q)) {
                let snippet: String = line.trim().chars().take(160).collect();
                hits.push(Hit { path: path.to_string_lossy().into_owned(), rel, line: Some(i + 1), text: Some(snippet) });
            }
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_outside_roots_are_refused() {
        let root = canon(&std::env::temp_dir()).unwrap().join(format!("fermi-files-{}", std::process::id()));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub").join("a.txt"), "hello").unwrap();
        let roots = vec![canon(&root.join("sub")).unwrap()];
        assert!(check(&roots, &root.join("sub").join("a.txt").to_string_lossy()).is_ok());
        // `..` resolves outside the root.
        let escape = root.join("sub").join("..").join("sub").join("..");
        assert!(check(&roots, &escape.to_string_lossy()).is_err());
        assert!(check_new(&roots, &root.join("sub").join("new.txt").to_string_lossy()).is_ok());
        assert!(check_new(&roots, &root.join("new.txt").to_string_lossy()).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
