//! .pptx decks for the slides viewer. A deck is a zip of XML parts; the
//! dashboard renders and edits the XML itself, so this only unzips and
//! rezips: the text parts out, images by name, edited parts back in.

use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Read, Write};
use std::path::Path;

/// Biggest deck opened.
pub const MAX_DECK: u64 = 300 * 1024 * 1024;
/// Most XML handed to the browser.
const MAX_XML: u64 = 40 * 1024 * 1024;

#[derive(Serialize)]
pub struct Deck {
    /// XML and relationship parts by name (`ppt/slides/slide1.xml`).
    pub parts: BTreeMap<String, String>,
    /// Every other part (images, fonts…), served on request.
    pub media: Vec<String>,
}

fn is_text_part(name: &str) -> bool {
    name.ends_with(".xml") || name.ends_with(".rels")
}

fn open(path: &Path) -> Result<zip::ZipArchive<std::fs::File>> {
    let f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    if f.metadata()?.len() > MAX_DECK {
        bail!("this deck is too large to open here");
    }
    zip::ZipArchive::new(f).context("this isn't a valid .pptx file")
}

pub fn read(path: &Path) -> Result<Deck> {
    let mut zip = open(path)?;
    let mut parts = BTreeMap::new();
    let mut media = vec![];
    let mut total = 0;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i)?;
        let name = f.name()?.into_owned();
        if f.is_dir() {
            continue;
        }
        if is_text_part(&name) {
            total += f.size();
            if total > MAX_XML {
                bail!("this deck has too much XML to open here");
            }
            let mut s = String::new();
            f.read_to_string(&mut s).with_context(|| format!("reading {name}"))?;
            parts.insert(name, s);
        } else {
            media.push(name);
        }
    }
    if !parts.contains_key("ppt/presentation.xml") {
        bail!("this isn't a PowerPoint deck (no ppt/presentation.xml)");
    }
    Ok(Deck { parts, media })
}

/// One non-XML part (an image) and its content type.
pub fn media(path: &Path, name: &str) -> Result<(Vec<u8>, &'static str)> {
    if is_text_part(name) || !name.starts_with("ppt/") {
        bail!("not a media part");
    }
    let mut zip = open(path)?;
    let mut f = zip.by_name(name).context("no such part in the deck")?;
    let mut bytes = Vec::with_capacity(f.size() as usize);
    f.read_to_end(&mut bytes)?;
    let mime = match Path::new(name).extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("emf") => "image/emf",
        Some("wmf") => "image/wmf",
        Some("tif") | Some("tiff") => "image/tiff",
        _ => crate::files::content_type(Path::new(name)),
    };
    Ok((bytes, mime))
}

/// A part a save may add: a new slide (a duplicate) and its relationships.
fn new_part_allowed(name: &str) -> bool {
    let num = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    let slide = |s: &str| s.strip_prefix("slide").and_then(|r| r.strip_suffix(".xml")).is_some_and(num);
    match (name.strip_prefix("ppt/slides/_rels/"), name.strip_prefix("ppt/slides/")) {
        (Some(rels), _) => rels.strip_suffix(".rels").is_some_and(slide),
        (None, Some(part)) => slide(part),
        _ => false,
    }
}

/// Writes the deck again with `changed` parts replaced (or added, for new
/// slides) and `remove`d parts left out. Everything else is copied as is.
pub fn save(path: &Path, changed: &HashMap<String, String>, remove: &[String]) -> Result<()> {
    let mut zip = open(path)?;
    let existing: HashSet<String> = zip.file_names().map(|n| n.map(|n| n.into_owned())).collect::<Result<_, _>>()?;
    for name in changed.keys() {
        if !is_text_part(name) {
            bail!("only XML parts can be edited ({name})");
        }
        if !existing.contains(name) && !new_part_allowed(name) {
            bail!("unexpected new part {name}");
        }
    }
    let remove: HashSet<&str> = remove.iter().map(String::as_str).collect();
    for name in &remove {
        if !is_text_part(name) || name.starts_with('[') || *name == "ppt/presentation.xml" {
            bail!("{name} can't be removed");
        }
    }
    let dir = path.parent().context("deck has no folder")?;
    let tmp = dir.join(format!(".{}.fermi-{}.tmp", path.file_name().and_then(|n| n.to_str()).unwrap_or("deck"), std::process::id()));
    let res = (|| -> Result<()> {
        let mut out = zip::ZipWriter::new(std::fs::File::create(&tmp)?);
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        // Same order as before: [Content_Types].xml stays first.
        for i in 0..zip.len() {
            let f = zip.by_index_raw(i)?;
            let name = f.name()?.into_owned();
            if remove.contains(name.as_str()) {
                continue;
            }
            match changed.get(&name) {
                Some(text) => {
                    drop(f);
                    out.start_file(name.as_str(), opts)?;
                    out.write_all(text.as_bytes())?;
                }
                None => out.raw_copy_file(f)?,
            }
        }
        let mut added: Vec<&String> = changed.keys().filter(|n| !existing.contains(*n)).collect();
        added.sort();
        for name in added {
            out.start_file(name.as_str(), opts)?;
            out.write_all(changed[name].as_bytes())?;
        }
        out.finish()?;
        Ok(())
    })();
    if let Err(e) = res {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    drop(zip);
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        anyhow::anyhow!("couldn't replace {} ({e}); is it open in PowerPoint?", path.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deck(dir: &Path) -> std::path::PathBuf {
        let p = dir.join("t.pptx");
        let mut w = zip::ZipWriter::new(std::fs::File::create(&p).unwrap());
        let o = zip::write::SimpleFileOptions::default();
        for (n, body) in [
            ("[Content_Types].xml", "<Types/>"),
            ("ppt/presentation.xml", "<p:presentation/>"),
            ("ppt/slides/slide1.xml", "<p:sld>one</p:sld>"),
            ("ppt/slides/slide2.xml", "<p:sld>two</p:sld>"),
        ] {
            w.start_file(n, o).unwrap();
            w.write_all(body.as_bytes()).unwrap();
        }
        w.start_file("ppt/media/image1.png", o).unwrap();
        w.write_all(&[137, 80, 78, 71]).unwrap();
        w.finish().unwrap();
        p
    }

    #[test]
    fn edits_round_trip() {
        let dir = std::env::temp_dir().join(format!("fermi-slides-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = deck(&dir);
        let d = read(&p).unwrap();
        assert_eq!(d.parts.len(), 4);
        assert_eq!(d.media, ["ppt/media/image1.png"]);
        assert_eq!(media(&p, "ppt/media/image1.png").unwrap().1, "image/png");
        assert!(media(&p, "ppt/presentation.xml").is_err());

        let mut changed = HashMap::new();
        changed.insert("ppt/slides/slide1.xml".to_string(), "<p:sld>ONE</p:sld>".to_string());
        changed.insert("ppt/slides/slide3.xml".to_string(), "<p:sld>three</p:sld>".to_string());
        save(&p, &changed, &["ppt/slides/slide2.xml".to_string()]).unwrap();
        let d = read(&p).unwrap();
        assert_eq!(d.parts["ppt/slides/slide1.xml"], "<p:sld>ONE</p:sld>");
        assert_eq!(d.parts["ppt/slides/slide3.xml"], "<p:sld>three</p:sld>");
        assert!(!d.parts.contains_key("ppt/slides/slide2.xml"));
        assert_eq!(d.parts.keys().next().unwrap(), "[Content_Types].xml");
        assert_eq!(d.media.len(), 1, "media copied unchanged");

        let mut bad = HashMap::new();
        bad.insert("ppt/evil.xml".to_string(), String::new());
        assert!(save(&p, &bad, &[]).is_err());
        assert!(save(&p, &HashMap::new(), &["[Content_Types].xml".to_string()]).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn new_parts() {
        assert!(new_part_allowed("ppt/slides/slide12.xml"));
        assert!(new_part_allowed("ppt/slides/_rels/slide12.xml.rels"));
        assert!(!new_part_allowed("ppt/slides/slide.xml"));
        assert!(!new_part_allowed("ppt/slides/../x.xml"));
        assert!(!new_part_allowed("ppt/theme/theme2.xml"));
    }
}
