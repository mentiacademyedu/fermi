//! Writes PowerPoint decks from a short description (`DeckSpec`), so agents
//! can make real `.pptx` slides without running a script (and without
//! needing approval to run one). The layouts are designed here: a title
//! slide, section breaks, bullets, two columns, numbered steps, an image
//! with text, a quote, a table and a big number, in a few color themes.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::fmt::Write as _;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, Default)]
pub struct DeckSpec {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub theme: Option<String>,
    pub slides: Vec<SlideSpec>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Step {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub text: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct SlideSpec {
    #[serde(default)]
    pub layout: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub subtitle: Option<String>,
    /// A paragraph of body text.
    #[serde(default)]
    pub text: Option<String>,
    /// Bullet points; start one with two spaces for a sub-point. `**bold**` works.
    #[serde(default)]
    pub bullets: Vec<String>,
    #[serde(default)]
    pub left_title: Option<String>,
    #[serde(default)]
    pub left: Vec<String>,
    #[serde(default)]
    pub right_title: Option<String>,
    #[serde(default)]
    pub right: Vec<String>,
    #[serde(default)]
    pub steps: Vec<Step>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub caption: Option<String>,
    #[serde(default)]
    pub quote: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    /// Table rows; the first row is the header.
    #[serde(default)]
    pub rows: Vec<Vec<String>>,
    #[serde(default)]
    pub stat: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

pub const LAYOUTS: [&str; 9] = ["title", "section", "bullets", "two_columns", "steps", "image", "quote", "table", "stat"];
pub const THEMES: [&str; 6] = ["light", "dark", "ocean", "sunset", "forest", "mono"];

struct Palette {
    bg: &'static str,
    text: &'static str,
    muted: &'static str,
    accent: &'static str,
    accent2: &'static str,
    surface: &'static str,
    on_accent: &'static str,
}

fn palette(name: &str) -> Palette {
    let p = |bg, text, muted, accent, accent2, surface, on_accent| Palette { bg, text, muted, accent, accent2, surface, on_accent };
    match name {
        "dark" => p("0F172A", "F1F5F9", "94A3B8", "38BDF8", "A78BFA", "1E293B", "0F172A"),
        "ocean" => p("F0F7FF", "0B2545", "5C7A99", "1363DF", "06B6D4", "DCEBFF", "FFFFFF"),
        "sunset" => p("FFF7ED", "431407", "9A6B4F", "EA580C", "DB2777", "FFEDD5", "FFFFFF"),
        "forest" => p("F3FAF5", "14281D", "5B7565", "15803D", "0D9488", "DCEFE3", "FFFFFF"),
        "mono" => p("FAFAFA", "111111", "666666", "111111", "555555", "EEEEEE", "FFFFFF"),
        _ => p("FFFFFF", "1F2937", "6B7280", "2563EB", "0EA5E9", "F1F5F9", "FFFFFF"),
    }
}

const FONT: &str = "Segoe UI";
const NS: &str = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main""#;
const DECL: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n";
const SLIDE_W: i64 = 12_192_000;
const SLIDE_H: i64 = 6_858_000;

/// Inches to EMU.
fn i(x: f64) -> i64 {
    (x * 914_400.0).round() as i64
}

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            // Control characters aren't allowed in XML.
            c if (c as u32) < 0x20 && c != '\t' => {}
            c => o.push(c),
        }
    }
    o
}

// ── text ────────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct Run {
    text: String,
    size: f64,
    bold: bool,
    italic: bool,
    color: String,
}

#[derive(Clone)]
struct Para {
    runs: Vec<Run>,
    align: &'static str,
    /// Bullet character and its color.
    bullet: Option<(&'static str, String)>,
    level: u32,
    space_before: f64,
    line: f64,
}

/// One styled paragraph from text with `**bold**` spans.
fn para(text: &str, size: f64, color: &str, bold: bool) -> Para {
    let mut runs = vec![];
    for (k, piece) in text.split("**").enumerate() {
        if !piece.is_empty() {
            runs.push(Run { text: piece.to_string(), size, bold: bold || k % 2 == 1, italic: false, color: color.to_string() });
        }
    }
    if runs.is_empty() {
        runs.push(Run { text: String::new(), size, bold, italic: false, color: color.to_string() });
    }
    Para { runs, align: "l", bullet: None, level: 0, space_before: 0.0, line: 1.1 }
}

fn bullet_paras(items: &[String], size: f64, pal: &Palette) -> Vec<Para> {
    items
        .iter()
        .map(|b| {
            let sub = b.starts_with("  ") || b.starts_with('\t');
            let t = b.trim().trim_start_matches(['-', '*', '•']).trim();
            let mut p = para(t, if sub { size * 0.82 } else { size }, if sub { pal.muted } else { pal.text }, false);
            p.bullet = Some((if sub { "–" } else { "•" }, pal.accent.to_string()));
            p.level = sub as u32;
            p.space_before = if sub { size * 0.25 } else { size * 0.55 };
            p
        })
        .collect()
}

/// A font size that lets this much text fit a box of `area` square inches.
fn fit(chars: usize, lines: usize, area: f64, max: f64, min: f64) -> f64 {
    // Roughly: a character at size s pt covers (0.55 s/72)·(1.3 s/72) in².
    let need = |s: f64| chars as f64 * 0.55 * 1.35 * (s / 72.0).powi(2) + lines as f64 * 0.6 * (s / 72.0) * 4.0;
    let mut s = max;
    while s > min && need(s) > area * 0.55 {
        s -= 1.0;
    }
    s
}

fn para_xml(p: &Para) -> String {
    let mut x = String::from("<a:p>");
    let (mar, ind) = match (&p.bullet, p.level) {
        (Some(_), 0) => (i(0.36), -i(0.3)),
        (Some(_), _) => (i(0.8), -i(0.28)),
        _ => (0, 0),
    };
    let _ = write!(
        x,
        r#"<a:pPr marL="{mar}" indent="{ind}" algn="{}"><a:lnSpc><a:spcPct val="{}"/></a:lnSpc><a:spcBef><a:spcPts val="{}"/></a:spcBef>"#,
        p.align,
        (p.line * 100_000.0) as i64,
        (p.space_before * 100.0) as i64
    );
    match &p.bullet {
        Some((ch, color)) => {
            let _ = write!(x, r#"<a:buClr><a:srgbClr val="{color}"/></a:buClr><a:buFont typeface="Arial"/><a:buChar char="{ch}"/>"#);
        }
        None => x.push_str("<a:buNone/>"),
    }
    x.push_str("</a:pPr>");
    for r in &p.runs {
        let _ = write!(
            x,
            r#"<a:r><a:rPr lang="en-US" sz="{}" b="{}" i="{}" dirty="0"><a:solidFill><a:srgbClr val="{}"/></a:solidFill><a:latin typeface="{FONT}"/><a:cs typeface="{FONT}"/></a:rPr><a:t>{}</a:t></a:r>"#,
            (r.size * 100.0) as i64,
            r.bold as u8,
            r.italic as u8,
            r.color,
            esc(&r.text)
        );
    }
    let size = p.runs.first().map(|r| r.size).unwrap_or(18.0);
    let _ = write!(x, r#"<a:endParaRPr lang="en-US" sz="{}" dirty="0"/></a:p>"#, (size * 100.0) as i64);
    x
}

// ── a slide ─────────────────────────────────────────────────────────────────

struct Slide {
    shapes: String,
    next_id: u32,
    /// Images: (relationship id, media part name).
    images: Vec<(String, String)>,
}

#[derive(Clone, Copy)]
struct BoxIn {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

fn b(x: f64, y: f64, w: f64, h: f64) -> BoxIn {
    BoxIn { x, y, w, h }
}

impl Slide {
    fn new() -> Self {
        Slide { shapes: String::new(), next_id: 2, images: vec![] }
    }
    fn id(&mut self) -> u32 {
        self.next_id += 1;
        self.next_id - 1
    }
    fn xfrm(r: BoxIn) -> String {
        format!(r#"<a:xfrm><a:off x="{}" y="{}"/><a:ext cx="{}" cy="{}"/></a:xfrm>"#, i(r.x), i(r.y), i(r.w).max(1), i(r.h).max(1))
    }
    fn fill(color: Option<&str>, alpha: Option<u32>) -> String {
        match (color, alpha) {
            (Some(c), Some(a)) => format!(r#"<a:solidFill><a:srgbClr val="{c}"><a:alpha val="{a}"/></a:srgbClr></a:solidFill>"#),
            (Some(c), None) => format!(r#"<a:solidFill><a:srgbClr val="{c}"/></a:solidFill>"#),
            _ => "<a:noFill/>".into(),
        }
    }
    /// A plain shape (rectangle, rounded rectangle, ellipse).
    fn shape(&mut self, r: BoxIn, geom: &str, color: &str, alpha: Option<u32>) {
        let id = self.id();
        let av = if geom == "roundRect" { r#"<a:gd name="adj" fmla="val 7000"/>"# } else { "" };
        let _ = write!(
            self.shapes,
            r#"<p:sp><p:nvSpPr><p:cNvPr id="{id}" name="Shape {id}"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr>{}<a:prstGeom prst="{geom}"><a:avLst>{av}</a:avLst></a:prstGeom>{}<a:ln><a:noFill/></a:ln></p:spPr><p:txBody><a:bodyPr rtlCol="0" anchor="ctr"/><a:lstStyle/><a:p><a:endParaRPr lang="en-US" dirty="0"/></a:p></p:txBody></p:sp>"#,
            Self::xfrm(r),
            Self::fill(Some(color), alpha)
        );
    }
    /// A text box, optionally on a filled shape.
    fn text(&mut self, r: BoxIn, paras: &[Para], anchor: &str, fill: Option<&str>, geom: &str, inset: f64) {
        let id = self.id();
        let av = if geom == "roundRect" { r#"<a:gd name="adj" fmla="val 7000"/>"# } else { "" };
        let ins = i(inset);
        let body: String = paras.iter().map(para_xml).collect();
        let _ = write!(
            self.shapes,
            r#"<p:sp><p:nvSpPr><p:cNvPr id="{id}" name="Text {id}"/><p:cNvSpPr txBox="1"/><p:nvPr/></p:nvSpPr><p:spPr>{}<a:prstGeom prst="{geom}"><a:avLst>{av}</a:avLst></a:prstGeom>{}<a:ln><a:noFill/></a:ln></p:spPr><p:txBody><a:bodyPr wrap="square" lIns="{ins}" tIns="{ins}" rIns="{ins}" bIns="{ins}" rtlCol="0" anchor="{anchor}"><a:normAutofit/></a:bodyPr><a:lstStyle/>{body}</p:txBody></p:sp>"#,
            Self::xfrm(r),
            Self::fill(fill, None)
        );
    }
    fn picture(&mut self, r: BoxIn, rid: &str) {
        let id = self.id();
        let _ = write!(
            self.shapes,
            r#"<p:pic><p:nvPicPr><p:cNvPr id="{id}" name="Picture {id}"/><p:cNvPicPr><a:picLocks noChangeAspect="1"/></p:cNvPicPr><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="{rid}"/><a:stretch><a:fillRect/></a:stretch></p:blipFill><p:spPr>{}<a:prstGeom prst="rect"><a:avLst/></a:prstGeom></p:spPr></p:pic>"#,
            Self::xfrm(r)
        );
    }
    fn table(&mut self, r: BoxIn, rows: &[Vec<String>], pal: &Palette) {
        let id = self.id();
        let cols = rows.iter().map(|r| r.len()).max().unwrap_or(1).max(1);
        let col_w = i(r.w) / cols as i64;
        let row_h = i(r.h) / rows.len().max(1) as i64;
        let size = if cols > 5 || rows.len() > 8 {
            13.0
        } else if cols > 3 || rows.len() > 6 {
            15.0
        } else {
            17.0
        };
        let mut x = format!(
            r#"<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="{id}" name="Table {id}"/><p:cNvGraphicFramePr><a:graphicFrameLocks noGrp="1"/></p:cNvGraphicFramePr><p:nvPr/></p:nvGraphicFramePr><p:xfrm><a:off x="{}" y="{}"/><a:ext cx="{}" cy="{}"/></p:xfrm><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/table"><a:tbl><a:tblPr firstRow="1" bandRow="1"/><a:tblGrid>"#,
            i(r.x),
            i(r.y),
            col_w * cols as i64,
            row_h * rows.len() as i64
        );
        for _ in 0..cols {
            let _ = write!(x, r#"<a:gridCol w="{col_w}"/>"#);
        }
        x.push_str("</a:tblGrid>");
        for (ri, row) in rows.iter().enumerate() {
            let head = ri == 0;
            let _ = write!(x, r#"<a:tr h="{row_h}">"#);
            for ci in 0..cols {
                let cell = row.get(ci).map(String::as_str).unwrap_or("");
                let mut p = para(cell, size, if head { pal.on_accent } else { pal.text }, head);
                p.line = 1.0;
                let fill = if head {
                    pal.accent
                } else if ri % 2 == 0 {
                    pal.surface
                } else {
                    pal.bg
                };
                let _ = write!(
                    x,
                    r#"<a:tc><a:txBody><a:bodyPr/><a:lstStyle/>{}</a:txBody><a:tcPr marL="{m}" marR="{m}" marT="{t}" marB="{t}" anchor="ctr"><a:solidFill><a:srgbClr val="{fill}"/></a:solidFill></a:tcPr></a:tc>"#,
                    para_xml(&p),
                    m = i(0.12),
                    t = i(0.05)
                );
            }
            x.push_str("</a:tr>");
        }
        x.push_str("</a:tbl></a:graphicData></a:graphic></p:graphicFrame>");
        self.shapes.push_str(&x);
    }
    fn xml(&self, bg: &str) -> String {
        format!(
            r#"{DECL}<p:sld {NS}><p:cSld><p:bg><p:bgPr><a:solidFill><a:srgbClr val="{bg}"/></a:solidFill><a:effectLst/></p:bgPr></p:bg><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/><a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr>{}</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>"#,
            self.shapes
        )
    }
}

/// Pixel size of a PNG, JPEG or GIF.
fn image_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.starts_with(b"\x89PNG") && bytes.len() >= 24 {
        let n = |o: usize| u32::from_be_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
        return Some((n(16), n(20)));
    }
    if bytes.starts_with(b"GIF") && bytes.len() >= 10 {
        return Some((u16::from_le_bytes([bytes[6], bytes[7]]) as u32, u16::from_le_bytes([bytes[8], bytes[9]]) as u32));
    }
    if bytes.starts_with(&[0xFF, 0xD8]) {
        let mut k = 2;
        while k + 9 < bytes.len() {
            if bytes[k] != 0xFF {
                k += 1;
                continue;
            }
            let marker = bytes[k + 1];
            let len = u16::from_be_bytes([bytes[k + 2], bytes[k + 3]]) as usize;
            if (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xC8 && marker != 0xCC {
                let h = u16::from_be_bytes([bytes[k + 5], bytes[k + 6]]) as u32;
                let w = u16::from_be_bytes([bytes[k + 7], bytes[k + 8]]) as u32;
                return Some((w, h));
            }
            k += 2 + len;
        }
    }
    None
}

struct Media {
    name: String,
    bytes: Vec<u8>,
}

struct Ctx<'a> {
    pal: Palette,
    deck_title: String,
    base: &'a Path,
    media: Vec<Media>,
}

impl Ctx<'_> {
    /// Adds an image file to the deck; returns its relationship id and pixel size.
    fn image(&mut self, slide: &mut Slide, path: &str) -> Result<(String, (u32, u32))> {
        let p = if Path::new(path).is_absolute() { PathBuf::from(path) } else { self.base.join(path) };
        let bytes = std::fs::read(&p).with_context(|| format!("reading image {}", p.display()))?;
        let ext = match () {
            _ if bytes.starts_with(b"\x89PNG") => "png",
            _ if bytes.starts_with(&[0xFF, 0xD8]) => "jpeg",
            _ if bytes.starts_with(b"GIF") => "gif",
            _ => bail!("{} isn't a PNG, JPEG or GIF image", p.display()),
        };
        let size = image_size(&bytes).unwrap_or((1600, 1000));
        let name = format!("ppt/media/image{}.{ext}", self.media.len() + 1);
        self.media.push(Media { name: name.clone(), bytes });
        let rid = format!("rId{}", slide.images.len() + 3);
        slide.images.push((rid.clone(), name));
        Ok((rid, size))
    }

    /// Title, accent rule, footer and slide number shared by content slides.
    fn header(&self, s: &mut Slide, title: &str, n: usize, total: usize) {
        let p = &self.pal;
        let size = match title.chars().count() {
            0..=40 => 36.0,
            41..=70 => 30.0,
            _ => 24.0,
        };
        let mut t = para(title, size, p.text, true);
        t.line = 1.0;
        s.text(b(0.7, 0.35, 11.93, 1.1), &[t], "b", None, "rect", 0.0);
        s.shape(b(0.7, 1.55, 1.1, 0.07), "rect", p.accent, None);
        self.footer(s, n, total);
    }

    fn footer(&self, s: &mut Slide, n: usize, total: usize) {
        let p = &self.pal;
        if !self.deck_title.is_empty() {
            s.text(b(0.7, 6.95, 8.0, 0.35), &[para(&self.deck_title, 11.0, p.muted, false)], "ctr", None, "rect", 0.0);
        }
        let mut num = para(&format!("{n} / {total}"), 11.0, p.muted, false);
        num.align = "r";
        s.text(b(10.6, 6.95, 2.03, 0.35), &[num], "ctr", None, "rect", 0.0);
    }

    fn bullets_box(&self, s: &mut Slide, r: BoxIn, intro: Option<&str>, items: &[String], max: f64) {
        let p = &self.pal;
        let chars: usize = items.iter().map(|b| b.len()).sum::<usize>() + intro.map(str::len).unwrap_or(0);
        let size = fit(chars, items.len() + intro.is_some() as usize, r.w * r.h, max, 14.0);
        let mut paras = vec![];
        if let Some(t) = intro.filter(|t| !t.trim().is_empty()) {
            for line in t.lines().filter(|l| !l.trim().is_empty()) {
                let mut q = para(line.trim(), size, p.muted, false);
                q.space_before = size * 0.4;
                paras.push(q);
            }
        }
        paras.extend(bullet_paras(items, size, p));
        if !paras.is_empty() {
            s.text(r, &paras, "t", None, "rect", 0.05);
        }
    }

    fn render(&mut self, spec: &SlideSpec, n: usize, total: usize) -> Result<Slide> {
        let mut s = Slide::new();
        let title = spec.title.clone().unwrap_or_default();
        let layout = spec.layout.clone().unwrap_or_else(|| guess_layout(spec, n));
        let pal = palette_ref(&self.pal);
        match layout.as_str() {
            "title" => {
                s.shape(b(0.0, 0.0, 0.32, 7.5), "rect", pal.accent, None);
                s.shape(b(9.6, -1.6, 5.4, 5.4), "ellipse", pal.accent2, Some(16_000));
                s.shape(b(11.2, 5.0, 3.0, 3.0), "ellipse", pal.accent, Some(12_000));
                let size = if title.chars().count() > 45 { 40.0 } else { 52.0 };
                let mut t = para(&title, size, pal.text, true);
                t.line = 1.0;
                s.text(b(1.1, 1.2, 10.5, 2.75), &[t], "b", None, "rect", 0.0);
                s.shape(b(1.1, 4.12, 1.4, 0.08), "rect", pal.accent, None);
                let sub = spec.subtitle.clone().or(spec.text.clone()).unwrap_or_default();
                if !sub.is_empty() {
                    s.text(b(1.1, 4.4, 10.0, 1.6), &[para(&sub, 22.0, pal.muted, false)], "t", None, "rect", 0.0);
                }
                return Ok(s);
            }
            "section" => {
                let mut t = para(&title, 46.0, pal.on_accent, true);
                t.line = 1.0;
                s.text(b(0.9, 2.2, 11.5, 1.8), &[t], "b", None, "rect", 0.0);
                if let Some(sub) = spec.subtitle.as_ref().or(spec.text.as_ref()) {
                    s.text(b(0.9, 4.15, 11.5, 1.2), &[para(sub, 22.0, pal.on_accent, false)], "t", None, "rect", 0.0);
                }
                s.shapes = format!(
                    r#"<p:sp><p:nvSpPr><p:cNvPr id="2" name="Band"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr>{}<a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:solidFill><a:srgbClr val="{}"/></a:solidFill><a:ln><a:noFill/></a:ln></p:spPr><p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:endParaRPr lang="en-US" dirty="0"/></a:p></p:txBody></p:sp>{}"#,
                    Slide::xfrm(b(0.0, 0.0, 13.333, 7.5)),
                    pal.accent,
                    s.shapes
                );
                return Ok(s);
            }
            "two_columns" => {
                self.header(&mut s, &title, n, total);
                for (k, (head, items)) in [(&spec.left_title, &spec.left), (&spec.right_title, &spec.right)].into_iter().enumerate() {
                    let x = 0.7 + k as f64 * 6.13;
                    s.shape(b(x, 1.95, 5.8, 4.8), "roundRect", pal.surface, None);
                    let mut top = 2.2;
                    if let Some(h) = head.as_ref().filter(|h| !h.is_empty()) {
                        s.text(b(x + 0.3, top, 5.2, 0.6), &[para(h, 22.0, pal.accent, true)], "t", None, "rect", 0.0);
                        top += 0.7;
                    }
                    self.bullets_box(&mut s, b(x + 0.3, top, 5.2, 6.5 - top), None, items, 22.0);
                }
            }
            "steps" => {
                self.header(&mut s, &title, n, total);
                let steps = &spec.steps;
                let k = steps.len().max(1);
                if k <= 4 {
                    let gap = 0.3;
                    let w = (11.93 - gap * (k as f64 - 1.0)) / k as f64;
                    let chars = steps.iter().map(|x| x.title.len() + x.text.len()).max().unwrap_or(0);
                    let size = fit(chars, 3, (w - 0.5) * 3.0, 18.0, 12.0);
                    for (j, st) in steps.iter().enumerate() {
                        let x = 0.7 + j as f64 * (w + gap);
                        s.shape(b(x, 2.0, w, 4.65), "roundRect", pal.surface, None);
                        let mut num = para(&(j + 1).to_string(), 20.0, pal.on_accent, true);
                        num.align = "ctr";
                        s.text(b(x + 0.3, 2.3, 0.7, 0.7), &[num], "ctr", Some(pal.accent), "ellipse", 0.0);
                        let mut paras = vec![para(&st.title, size + 3.0, pal.text, true)];
                        if !st.text.is_empty() {
                            let mut body = para(&st.text, size, pal.muted, false);
                            body.space_before = size * 0.6;
                            paras.push(body);
                        }
                        s.text(b(x + 0.3, 3.2, w - 0.55, 3.3), &paras, "t", None, "rect", 0.0);
                    }
                } else {
                    let row = (4.85 / k as f64).min(1.1);
                    let size = fit(steps.iter().map(|x| x.title.len() + x.text.len()).max().unwrap_or(0), 1, 10.5 * row, 18.0, 11.0);
                    for (j, st) in steps.iter().enumerate() {
                        let y = 1.95 + j as f64 * row;
                        let d = (row * 0.62).min(0.6);
                        let mut num = para(&(j + 1).to_string(), (d * 26.0).max(11.0), pal.on_accent, true);
                        num.align = "ctr";
                        s.text(b(0.7, y + (row - d) / 2.0, d, d), &[num], "ctr", Some(pal.accent), "ellipse", 0.0);
                        let mut line = para(&st.title, size, pal.text, true);
                        if !st.text.is_empty() {
                            line.runs.push(Run {
                                text: format!("  {}", st.text),
                                size,
                                bold: false,
                                italic: false,
                                color: pal.muted.to_string(),
                            });
                        }
                        s.text(b(0.7 + d + 0.25, y, 11.9 - d - 0.25, row), &[line], "ctr", None, "rect", 0.0);
                    }
                }
            }
            "image" => {
                self.header(&mut s, &title, n, total);
                let has_text = !spec.bullets.is_empty() || spec.text.as_ref().is_some_and(|t| !t.is_empty());
                let area = if has_text { b(0.7, 1.95, 6.6, 4.5) } else { b(0.7, 1.95, 11.93, 4.5) };
                if let Some(path) = spec.image.as_deref() {
                    let (rid, (pw, ph)) = self.image(&mut s, path)?;
                    let ratio = pw.max(1) as f64 / ph.max(1) as f64;
                    let (w, h) = if area.w / area.h > ratio { (area.h * ratio, area.h) } else { (area.w, area.w / ratio) };
                    s.picture(b(area.x + (area.w - w) / 2.0, area.y + (area.h - h) / 2.0, w, h), &rid);
                }
                if let Some(c) = spec.caption.as_ref() {
                    let mut cp = para(c, 13.0, pal.muted, false);
                    cp.align = "ctr";
                    cp.runs.iter_mut().for_each(|r| r.italic = true);
                    s.text(b(area.x, 6.5, area.w, 0.4), &[cp], "t", None, "rect", 0.0);
                }
                if has_text {
                    self.bullets_box(&mut s, b(7.65, 1.95, 4.98, 4.8), spec.text.as_deref(), &spec.bullets, 22.0);
                }
            }
            "quote" => {
                let mut mark = para("\u{201C}", 140.0, pal.accent, true);
                mark.line = 0.8;
                s.text(b(0.9, 0.5, 2.0, 2.0), &[mark], "t", None, "rect", 0.0);
                let q = spec.quote.clone().or(spec.text.clone()).unwrap_or(title.clone());
                let size = fit(q.len(), 1, 10.5 * 3.2, 34.0, 18.0);
                let mut qp = para(&q, size, pal.text, false);
                qp.runs.iter_mut().for_each(|r| r.italic = true);
                qp.line = 1.15;
                s.text(b(1.6, 1.8, 10.3, 3.4), &[qp], "ctr", None, "rect", 0.0);
                if let Some(a) = spec.author.as_ref() {
                    s.text(b(1.6, 5.35, 10.3, 0.6), &[para(&format!("— {a}"), 20.0, pal.muted, false)], "t", None, "rect", 0.0);
                }
                self.footer(&mut s, n, total);
            }
            "table" => {
                self.header(&mut s, &title, n, total);
                if !spec.rows.is_empty() {
                    let h = (spec.rows.len() as f64 * 0.55).min(4.7);
                    s.table(b(0.7, 1.95, 11.93, h), &spec.rows, &pal);
                }
                if let Some(t) = spec.text.as_ref() {
                    s.text(b(0.7, 6.3, 11.93, 0.5), &[para(t, 14.0, pal.muted, false)], "t", None, "rect", 0.0);
                }
            }
            "stat" => {
                self.header(&mut s, &title, n, total);
                let stat = spec.stat.clone().unwrap_or_default();
                let size = if stat.chars().count() > 6 { 72.0 } else { 110.0 };
                s.text(b(0.7, 1.9, 6.0, 2.6), &[para(&stat, size, pal.accent, true)], "b", None, "rect", 0.0);
                if let Some(t) = spec.text.as_ref().or(spec.subtitle.as_ref()) {
                    s.text(b(0.7, 4.6, 6.0, 1.9), &[para(t, 22.0, pal.muted, false)], "t", None, "rect", 0.0);
                }
                if !spec.bullets.is_empty() {
                    self.bullets_box(&mut s, b(7.2, 2.1, 5.4, 4.6), None, &spec.bullets, 22.0);
                }
            }
            _ => {
                // bullets
                self.header(&mut s, &title, n, total);
                self.bullets_box(&mut s, b(0.7, 1.9, 11.93, 4.85), spec.text.as_deref(), &spec.bullets, 26.0);
            }
        }
        Ok(s)
    }
}

fn palette_ref(p: &Palette) -> Palette {
    Palette { bg: p.bg, text: p.text, muted: p.muted, accent: p.accent, accent2: p.accent2, surface: p.surface, on_accent: p.on_accent }
}

fn guess_layout(s: &SlideSpec, n: usize) -> String {
    let l = if !s.steps.is_empty() {
        "steps"
    } else if !s.rows.is_empty() {
        "table"
    } else if s.image.is_some() {
        "image"
    } else if s.quote.is_some() {
        "quote"
    } else if s.stat.is_some() {
        "stat"
    } else if !s.left.is_empty() || !s.right.is_empty() {
        "two_columns"
    } else if n == 1 && s.bullets.is_empty() {
        "title"
    } else {
        "bullets"
    };
    l.into()
}

// ── the package ─────────────────────────────────────────────────────────────

fn theme_xml(pal: &Palette, name: &str) -> String {
    let c = |n: &str, v: &str| format!(r#"<a:{n}><a:srgbClr val="{v}"/></a:{n}>"#);
    let ph = r#"<a:solidFill><a:schemeClr val="phClr"/></a:solidFill>"#;
    let ln = format!(r#"<a:ln w="9525">{ph}</a:ln>"#);
    format!(
        r#"{DECL}<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="{name}"><a:themeElements><a:clrScheme name="{name}">{}{}{}{}{}{}{}{}{}{}{}{}</a:clrScheme><a:fontScheme name="{name}"><a:majorFont><a:latin typeface="{FONT}"/><a:ea typeface=""/><a:cs typeface=""/></a:majorFont><a:minorFont><a:latin typeface="{FONT}"/><a:ea typeface=""/><a:cs typeface=""/></a:minorFont></a:fontScheme><a:fmtScheme name="{name}"><a:fillStyleLst>{ph}{ph}{ph}</a:fillStyleLst><a:lnStyleLst>{ln}{ln}{ln}</a:lnStyleLst><a:effectStyleLst><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle></a:effectStyleLst><a:bgFillStyleLst>{ph}{ph}{ph}</a:bgFillStyleLst></a:fmtScheme></a:themeElements><a:objectDefaults/><a:extraClrSchemeLst/></a:theme>"#,
        c("dk1", pal.text),
        c("lt1", pal.bg),
        c("dk2", pal.text),
        c("lt2", pal.surface),
        c("accent1", pal.accent),
        c("accent2", pal.accent2),
        c("accent3", pal.muted),
        c("accent4", pal.accent),
        c("accent5", pal.accent2),
        c("accent6", pal.muted),
        c("hlink", pal.accent),
        c("folHlink", pal.accent2),
    )
}

const EMPTY_TREE: &str = r#"<p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/><a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr>"#;
const CLR_MAP: &str = r#"bg1="lt1" tx1="dk1" bg2="lt2" tx2="dk2" accent1="accent1" accent2="accent2" accent3="accent3" accent4="accent4" accent5="accent5" accent6="accent6" hlink="hlink" folHlink="folHlink""#;

fn rels(items: &[(String, &str, String)]) -> String {
    let mut x = format!(r#"{DECL}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#);
    for (id, ty, target) in items {
        let _ = write!(
            x,
            r#"<Relationship Id="{id}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/{ty}" Target="{target}"/>"#
        );
    }
    x.push_str("</Relationships>");
    x
}

fn notes_xml(slide_no: usize, text: &str) -> String {
    let paras: String =
        text.lines().map(|l| format!(r#"<a:p><a:r><a:rPr lang="en-US" dirty="0"/><a:t>{}</a:t></a:r></a:p>"#, esc(l))).collect();
    format!(
        r#"{DECL}<p:notes {NS}><p:cSld><p:spTree>{EMPTY_TREE}<p:sp><p:nvSpPr><p:cNvPr id="2" name="Slide Image Placeholder {slide_no}"/><p:cNvSpPr><a:spLocks noGrp="1" noRot="1" noChangeAspect="1"/></p:cNvSpPr><p:nvPr><p:ph type="sldImg"/></p:nvPr></p:nvSpPr><p:spPr/></p:sp><p:sp><p:nvSpPr><p:cNvPr id="3" name="Notes Placeholder {slide_no}"/><p:cNvSpPr><a:spLocks noGrp="1"/></p:cNvSpPr><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:lstStyle/>{}</p:txBody></p:sp></p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:notes>"#,
        if paras.is_empty() { "<a:p/>".to_string() } else { paras }
    )
}

/// Writes the deck to `out`. Relative image paths are read from `base`.
pub fn write(spec: &DeckSpec, out: &Path, base: &Path) -> Result<usize> {
    if spec.slides.is_empty() {
        bail!("a deck needs at least one slide");
    }
    if spec.slides.len() > 200 {
        bail!("that's too many slides (200 at most)");
    }
    for (k, s) in spec.slides.iter().enumerate() {
        if let Some(l) = s.layout.as_deref().filter(|l| !LAYOUTS.contains(l)) {
            bail!("slide {}: unknown layout `{l}` (use {})", k + 1, LAYOUTS.join(", "));
        }
    }
    let theme = spec.theme.as_deref().unwrap_or("light");
    let deck_title = spec.title.clone().or_else(|| spec.slides[0].title.clone()).unwrap_or_default();
    let mut ctx = Ctx { pal: palette(theme), deck_title, base, media: vec![] };
    let total = spec.slides.len();
    let slides: Vec<Slide> = spec.slides.iter().enumerate().map(|(k, s)| ctx.render(s, k + 1, total)).collect::<Result<_>>()?;

    let mut parts: Vec<(String, Vec<u8>)> = vec![];
    let mut put = |name: &str, body: String| parts.push((name.to_string(), body.into_bytes()));

    let mut ct = format!(
        r#"{DECL}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Default Extension="jpeg" ContentType="image/jpeg"/><Default Extension="gif" ContentType="image/gif"/><Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/><Override PartName="/ppt/slideMasters/slideMaster1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml"/><Override PartName="/ppt/slideLayouts/slideLayout1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml"/><Override PartName="/ppt/notesMasters/notesMaster1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.notesMaster+xml"/><Override PartName="/ppt/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/><Override PartName="/ppt/theme/theme2.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/><Override PartName="/ppt/presProps.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presProps+xml"/><Override PartName="/ppt/viewProps.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.viewProps+xml"/><Override PartName="/ppt/tableStyles.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.tableStyles+xml"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/><Override PartName="/docProps/app.xml" ContentType="application/vnd.openxmlformats-officedocument.extended-properties+xml"/>"#
    );
    for k in 1..=total {
        let _ = write!(
            ct,
            r#"<Override PartName="/ppt/slides/slide{k}.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>"#
        );
        if spec.slides[k - 1].notes.as_ref().is_some_and(|n| !n.trim().is_empty()) {
            let _ = write!(
                ct,
                r#"<Override PartName="/ppt/notesSlides/notesSlide{k}.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.notesSlide+xml"/>"#
            );
        }
    }
    ct.push_str("</Types>");
    put("[Content_Types].xml", ct);
    put(
        "_rels/.rels",
        format!(
            r#"{DECL}<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties" Target="docProps/app.xml"/></Relationships>"#
        ),
    );
    put(
        "docProps/core.xml",
        format!(
            r#"{DECL}<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><dc:title>{}</dc:title><dc:creator>fermi</dc:creator></cp:coreProperties>"#,
            esc(&ctx.deck_title)
        ),
    );
    put(
        "docProps/app.xml",
        format!(
            r#"{DECL}<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties"><Application>fermi</Application><Slides>{total}</Slides></Properties>"#
        ),
    );

    let mut sld_ids = String::new();
    let mut pres_rels = vec![
        ("rId1".to_string(), "slideMaster", "slideMasters/slideMaster1.xml".to_string()),
        ("rId2".to_string(), "notesMaster", "notesMasters/notesMaster1.xml".to_string()),
        ("rId3".to_string(), "presProps", "presProps.xml".to_string()),
        ("rId4".to_string(), "viewProps", "viewProps.xml".to_string()),
        ("rId5".to_string(), "theme", "theme/theme1.xml".to_string()),
        ("rId6".to_string(), "tableStyles", "tableStyles.xml".to_string()),
    ];
    for k in 1..=total {
        let _ = write!(sld_ids, r#"<p:sldId id="{}" r:id="rId{}"/>"#, 255 + k, 9 + k);
        pres_rels.push((format!("rId{}", 9 + k), "slide", format!("slides/slide{k}.xml")));
    }
    put(
        "ppt/presentation.xml",
        format!(
            r#"{DECL}<p:presentation {NS} saveSubsetFonts="1"><p:sldMasterIdLst><p:sldMasterId id="2147483648" r:id="rId1"/></p:sldMasterIdLst><p:notesMasterIdLst><p:notesMasterId r:id="rId2"/></p:notesMasterIdLst><p:sldIdLst>{sld_ids}</p:sldIdLst><p:sldSz cx="{SLIDE_W}" cy="{SLIDE_H}"/><p:notesSz cx="6858000" cy="9144000"/><p:defaultTextStyle><a:lvl1pPr marL="0" algn="l"><a:defRPr sz="1800"><a:solidFill><a:schemeClr val="tx1"/></a:solidFill><a:latin typeface="+mn-lt"/></a:defRPr></a:lvl1pPr></p:defaultTextStyle></p:presentation>"#
        ),
    );
    put("ppt/_rels/presentation.xml.rels", rels(&pres_rels));
    put("ppt/presProps.xml", format!(r#"{DECL}<p:presentationPr {NS}/>"#));
    put(
        "ppt/viewProps.xml",
        format!(
            r#"{DECL}<p:viewPr {NS}><p:normalViewPr><p:restoredLeft sz="15620"/><p:restoredTop sz="94660"/></p:normalViewPr><p:gridSpacing cx="76200" cy="76200"/></p:viewPr>"#
        ),
    );
    put(
        "ppt/tableStyles.xml",
        format!(
            r#"{DECL}<a:tblStyleLst xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" def="{{5C22544A-7EE6-4342-B048-85BDC9FD1C3A}}"/>"#
        ),
    );
    put("ppt/theme/theme1.xml", theme_xml(&ctx.pal, theme));
    put("ppt/theme/theme2.xml", theme_xml(&palette("light"), "notes"));
    put(
        "ppt/slideMasters/slideMaster1.xml",
        format!(
            r#"{DECL}<p:sldMaster {NS}><p:cSld><p:bg><p:bgRef idx="1001"><a:schemeClr val="bg1"/></p:bgRef></p:bg><p:spTree>{EMPTY_TREE}</p:spTree></p:cSld><p:clrMap {CLR_MAP}/><p:sldLayoutIdLst><p:sldLayoutId id="2147483649" r:id="rId1"/></p:sldLayoutIdLst><p:txStyles><p:titleStyle><a:lvl1pPr><a:defRPr sz="4400"><a:solidFill><a:schemeClr val="tx1"/></a:solidFill><a:latin typeface="+mj-lt"/></a:defRPr></a:lvl1pPr></p:titleStyle><p:bodyStyle><a:lvl1pPr><a:defRPr sz="2400"><a:solidFill><a:schemeClr val="tx1"/></a:solidFill><a:latin typeface="+mn-lt"/></a:defRPr></a:lvl1pPr></p:bodyStyle><p:otherStyle><a:lvl1pPr><a:defRPr sz="1800"><a:solidFill><a:schemeClr val="tx1"/></a:solidFill><a:latin typeface="+mn-lt"/></a:defRPr></a:lvl1pPr></p:otherStyle></p:txStyles></p:sldMaster>"#
        ),
    );
    put(
        "ppt/slideMasters/_rels/slideMaster1.xml.rels",
        rels(&[
            ("rId1".into(), "slideLayout", "../slideLayouts/slideLayout1.xml".into()),
            ("rId2".into(), "theme", "../theme/theme1.xml".into()),
        ]),
    );
    put(
        "ppt/slideLayouts/slideLayout1.xml",
        format!(
            r#"{DECL}<p:sldLayout {NS} type="blank" preserve="1"><p:cSld name="Blank"><p:spTree>{EMPTY_TREE}</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>"#
        ),
    );
    put("ppt/slideLayouts/_rels/slideLayout1.xml.rels", rels(&[("rId1".into(), "slideMaster", "../slideMasters/slideMaster1.xml".into())]));
    put(
        "ppt/notesMasters/notesMaster1.xml",
        format!(
            r#"{DECL}<p:notesMaster {NS}><p:cSld><p:bg><p:bgRef idx="1001"><a:schemeClr val="bg1"/></p:bgRef></p:bg><p:spTree>{EMPTY_TREE}<p:sp><p:nvSpPr><p:cNvPr id="2" name="Slide Image Placeholder"/><p:cNvSpPr><a:spLocks noGrp="1" noRot="1" noChangeAspect="1"/></p:cNvSpPr><p:nvPr><p:ph type="sldImg" idx="2"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="685800" y="1143000"/><a:ext cx="5486400" cy="3086100"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:noFill/><a:ln w="12700"><a:solidFill><a:prstClr val="black"/></a:solidFill></a:ln></p:spPr></p:sp><p:sp><p:nvSpPr><p:cNvPr id="3" name="Notes Placeholder"/><p:cNvSpPr><a:spLocks noGrp="1"/></p:cNvSpPr><p:nvPr><p:ph type="body" sz="quarter" idx="3"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="685800" y="4400550"/><a:ext cx="5486400" cy="3600450"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></p:spPr><p:txBody><a:bodyPr vert="horz" lIns="91440" tIns="45720" rIns="91440" bIns="45720" rtlCol="0"/><a:lstStyle/><a:p><a:endParaRPr lang="en-US"/></a:p></p:txBody></p:sp></p:spTree></p:cSld><p:clrMap {CLR_MAP}/><p:notesStyle><a:lvl1pPr marL="0" algn="l"><a:defRPr sz="1200"><a:solidFill><a:schemeClr val="tx1"/></a:solidFill><a:latin typeface="+mn-lt"/></a:defRPr></a:lvl1pPr></p:notesStyle></p:notesMaster>"#
        ),
    );
    put("ppt/notesMasters/_rels/notesMaster1.xml.rels", rels(&[("rId1".into(), "theme", "../theme/theme2.xml".into())]));

    for (k, s) in slides.iter().enumerate() {
        let n = k + 1;
        let spec_s = &spec.slides[k];
        put(
            &format!("ppt/slides/slide{n}.xml"),
            s.xml(if spec_s.layout.as_deref() == Some("section") { ctx.pal.accent } else { ctx.pal.bg }),
        );
        let mut r = vec![("rId1".to_string(), "slideLayout", "../slideLayouts/slideLayout1.xml".to_string())];
        let has_notes = spec_s.notes.as_ref().is_some_and(|t| !t.trim().is_empty());
        if has_notes {
            r.push(("rId2".into(), "notesSlide", format!("../notesSlides/notesSlide{n}.xml")));
            put(&format!("ppt/notesSlides/notesSlide{n}.xml"), notes_xml(n, spec_s.notes.as_deref().unwrap_or("")));
            put(
                &format!("ppt/notesSlides/_rels/notesSlide{n}.xml.rels"),
                rels(&[
                    ("rId1".into(), "notesMaster", "../notesMasters/notesMaster1.xml".into()),
                    ("rId2".into(), "slide", format!("../slides/slide{n}.xml")),
                ]),
            );
        }
        for (rid, media) in &s.images {
            r.push((rid.clone(), "image", format!("../media/{}", media.trim_start_matches("ppt/media/"))));
        }
        put(&format!("ppt/slides/_rels/slide{n}.xml.rels"), rels(&r));
    }

    // Write next to the target, then swap in.
    if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = out.with_extension("pptx.fermi-tmp");
    let res = (|| -> Result<()> {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&tmp)?);
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, body) in &parts {
            zip.start_file(name.as_str(), opts)?;
            zip.write_all(body)?;
        }
        for m in &ctx.media {
            zip.start_file(m.name.as_str(), zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored))?;
            zip.write_all(&m.bytes)?;
        }
        zip.finish()?;
        Ok(())
    })();
    if let Err(e) = res {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, out).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        anyhow::anyhow!("couldn't write {} ({e}); is it open in PowerPoint?", out.display())
    })?;
    Ok(total)
}

/// The JSON schema of `DeckSpec`, for the agent tool.
pub fn schema() -> serde_json::Value {
    use serde_json::json;
    let strings = json!({ "type": "array", "items": { "type": "string" } });
    json!({
        "type": "object",
        "properties": {
            "path": { "type": "string", "description": "Output .pptx path, relative to the working directory" },
            "title": { "type": "string", "description": "Deck title (shown in the footer)" },
            "theme": { "type": "string", "enum": THEMES, "description": "Color theme; default light" },
            "slides": { "type": "array", "items": { "type": "object", "properties": {
                "layout": { "type": "string", "enum": LAYOUTS, "description": "title: big title + subtitle. section: a break between parts. bullets: title + points. two_columns: left/right lists (e.g. pros/cons, before/after). steps: numbered steps (a process, a method, a timeline). image: a picture with optional bullets beside it. quote. table: rows of cells. stat: one big number with context. Guessed from the fields when omitted." },
                "title": { "type": "string" },
                "subtitle": { "type": "string" },
                "text": { "type": "string", "description": "A short paragraph" },
                "bullets": { "type": "array", "items": { "type": "string" }, "description": "Points, a few words to one line each; start with two spaces for a sub-point; **bold** works" },
                "left_title": { "type": "string" }, "left": strings,
                "right_title": { "type": "string" }, "right": strings,
                "steps": { "type": "array", "items": { "type": "object", "properties": { "title": { "type": "string" }, "text": { "type": "string" } } } },
                "image": { "type": "string", "description": "PNG/JPEG/GIF path (e.g. one made with generate_image)" },
                "caption": { "type": "string" },
                "quote": { "type": "string" }, "author": { "type": "string" },
                "rows": { "type": "array", "items": strings, "description": "Table rows; the first is the header" },
                "stat": { "type": "string", "description": "The big number, e.g. 42% or $1.2M" },
                "notes": { "type": "string", "description": "Speaker notes" }
            } } }
        },
        "required": ["path", "slides"]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_every_layout() {
        let dir = std::env::temp_dir().join(format!("fermi-deck-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A 2×1 PNG.
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x02\0\0\0\x01\x08\x02\0\0\0\0\0\0\0\0\0\0\0IEND\xaeB`\x82";
        std::fs::write(dir.join("pic.png"), png).unwrap();
        let spec: DeckSpec = serde_json::from_value(serde_json::json!({
            "title": "Derivatives", "theme": "ocean",
            "slides": [
                { "title": "How to find a derivative", "subtitle": "Step by step", "notes": "Hello\nSecond line" },
                { "layout": "section", "title": "The power rule" },
                { "title": "Rules", "bullets": ["**Power**: n·xⁿ⁻¹", "  constants vanish", "Sum rule"] },
                { "title": "Pros and cons", "left_title": "Pros", "left": ["fast"], "right": ["tricky"] },
                { "title": "Method", "steps": [{ "title": "Split", "text": "into terms" }, { "title": "Apply" }, { "title": "Add" }] },
                { "title": "Picture", "image": "pic.png", "caption": "A chart", "bullets": ["a < b & c"] },
                { "quote": "Imagination is more important", "author": "Einstein" },
                { "title": "Table", "rows": [["a", "b"], ["1", "2"]] },
                { "title": "Growth", "stat": "42%", "text": "year over year" }
            ]
        }))
        .unwrap();
        let out = dir.join("d.pptx");
        assert_eq!(write(&spec, &out, &dir).unwrap(), 9);
        let mut z = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
        let names: Vec<String> = z.file_names().map(|n| n.unwrap().into_owned()).collect();
        assert_eq!(names[0], "[Content_Types].xml");
        assert!(names.contains(&"ppt/media/image1.png".to_string()));
        assert!(names.contains(&"ppt/notesSlides/notesSlide1.xml".to_string()));
        let mut s = String::new();
        std::io::Read::read_to_string(&mut z.by_name("ppt/slides/slide6.xml").unwrap(), &mut s).unwrap();
        assert!(s.contains("a &lt; b &amp; c") && s.contains(r#"r:embed="rId3""#));
        assert!(write(
            &DeckSpec { slides: vec![SlideSpec { layout: Some("nope".into()), ..Default::default() }], ..Default::default() },
            &out,
            &dir
        )
        .is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sizes_images() {
        assert_eq!(image_size(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\x01\x00\0\0\0\x80"), Some((256, 128)));
        assert_eq!(image_size(b"GIF89a\x10\0\x08\0"), Some((16, 8)));
    }
}
