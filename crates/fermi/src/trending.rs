//! GitHub trending repositories. GitHub has no trending API, so this reads
//! github.com/trending; if that page changes shape, it falls back to the
//! search API (new repositories sorted by stars).

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::Value;
use std::sync::OnceLock;
use std::time::Duration;

#[derive(Debug, Clone, Serialize)]
pub struct Repo {
    pub owner: String,
    pub name: String,
    pub url: String,
    pub description: Option<String>,
    pub language: Option<String>,
    /// The language's color on GitHub, e.g. `#f1e05a`.
    pub color: Option<String>,
    pub stars: u64,
    pub forks: u64,
    /// Stars gained in the period (trending page only).
    pub gained: Option<u64>,
}

fn http() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(concat!("fermi/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(15))
            .build()
            .expect("http client")
    })
}

/// `daily`, `weekly` or `monthly`.
pub fn period(p: &str) -> &'static str {
    match p {
        "weekly" => "weekly",
        "monthly" => "monthly",
        _ => "daily",
    }
}

/// Language slugs GitHub uses in trending URLs (letters, digits, `+`, `#`, `-`).
fn slug(lang: &str) -> Option<String> {
    let s = lang.trim().to_lowercase().replace(' ', "-");
    if s.is_empty() || !s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '#' | '-' | '.')) {
        return None;
    }
    Some(s.replace('+', "%2B").replace('#', "%23"))
}

pub async fn fetch(period_: &str, language: Option<&str>) -> Result<(Vec<Repo>, &'static str)> {
    let since = period(period_);
    let lang = language.and_then(slug);
    match trending_page(since, lang.as_deref()).await {
        Ok(repos) if !repos.is_empty() => Ok((repos, "github.com/trending")),
        _ => Ok((search_api(since, language).await?, "GitHub search (new repositories by stars)")),
    }
}

async fn trending_page(since: &str, lang: Option<&str>) -> Result<Vec<Repo>> {
    let url = match lang {
        Some(l) => format!("https://github.com/trending/{l}?since={since}"),
        None => format!("https://github.com/trending?since={since}"),
    };
    let html = http().get(url).send().await?.error_for_status()?.text().await?;
    Ok(parse_trending(&html))
}

fn between<'a>(s: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let i = s.find(start)? + start.len();
    let j = s[i..].find(end)? + i;
    Some(&s[i..j])
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

fn unescape(s: &str) -> String {
    s.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&#x27;", "'")
}

fn number(s: &str) -> Option<u64> {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

pub fn parse_trending(html: &str) -> Vec<Repo> {
    html.split("<article class=\"Box-row\"")
        .skip(1)
        .filter_map(|a| {
            let heading = between(a, "<h2", "</h2>")?;
            let path = between(heading, "href=\"/", "\"")?;
            let (owner, name) = path.split_once('/')?;
            let ok = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
            if !ok(owner) || !ok(name) {
                return None;
            }
            let description = between(a, "<p class=\"col-9", "</p>")
                .and_then(|p| p.split_once('>').map(|(_, t)| unescape(strip_tags(t).trim())))
                .filter(|d| !d.is_empty());
            let language = between(a, "itemprop=\"programmingLanguage\">", "<").map(|l| unescape(l.trim()));
            let color = between(a, "repo-language-color\" style=\"background-color: ", "\"").map(|c| c.trim().to_string());
            // Skip the rest of the opening tag (its classes contain digits) before reading the number.
            let count = |link: &str| {
                between(a, &format!("href=\"/{path}/{link}\""), "</a>")
                    .and_then(|t| t.split_once('>').map(|(_, rest)| strip_tags(rest)))
                    .and_then(|t| number(&t))
            };
            let gained = ["stars today", "stars this week", "stars this month"].iter().find_map(|label| {
                let end = a.find(label)?;
                let start = a[..end].rfind('>')? + 1;
                number(&a[start..end])
            });
            Some(Repo {
                owner: owner.into(),
                name: name.into(),
                url: format!("https://github.com/{owner}/{name}"),
                description,
                language,
                color,
                stars: count("stargazers").unwrap_or(0),
                forks: count("forks").unwrap_or(0),
                gained,
            })
        })
        .collect()
}

async fn search_api(since: &str, language: Option<&str>) -> Result<Vec<Repo>> {
    let days = match since {
        "weekly" => 7,
        "monthly" => 30,
        _ => 2,
    };
    let from = chrono_like_date(fermi_core::util::now().saturating_sub(days * 86_400));
    let mut q = format!("created:>{from}");
    if let Some(l) = language.filter(|l| l.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '#' | '-' | ' '))) {
        q += &format!(" language:\"{l}\"");
    }
    let v: Value = http()
        .get("https://api.github.com/search/repositories")
        .query(&[("q", q.as_str()), ("sort", "stars"), ("order", "desc"), ("per_page", "25")])
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()
        .context("GitHub search")?
        .json()
        .await?;
    let Some(items) = v["items"].as_array() else { bail!("unexpected response from GitHub") };
    Ok(items
        .iter()
        .filter_map(|r| {
            let (owner, name) = r["full_name"].as_str()?.split_once('/')?;
            Some(Repo {
                owner: owner.into(),
                name: name.into(),
                url: r["html_url"].as_str()?.into(),
                description: r["description"].as_str().map(str::to_string),
                language: r["language"].as_str().map(str::to_string),
                color: None,
                stars: r["stargazers_count"].as_u64().unwrap_or(0),
                forks: r["forks_count"].as_u64().unwrap_or(0),
                gained: None,
            })
        })
        .collect())
}

/// `YYYY-MM-DD` (UTC) for a unix time, without a date library.
fn chrono_like_date(ts: u64) -> String {
    let days = (ts / 86_400) as i64;
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02}", if m <= 2 { y + 1 } else { y }, m, d)
}

/// Clones `owner/name` (shallow) into `parent/name`.
pub fn clone(owner: &str, name: &str, parent: &std::path::Path) -> Result<std::path::PathBuf> {
    let ok =
        |s: &str| !s.is_empty() && s != "." && s != ".." && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !ok(owner) || !ok(name) {
        bail!("not a GitHub repository name");
    }
    let dest = parent.join(name);
    if dest.join(".git").is_dir() {
        return Ok(dest); // already cloned there
    }
    if dest.exists() {
        bail!("{} already exists and isn't a clone", dest.display());
    }
    let mut cmd = std::process::Command::new("git");
    cmd.args(["clone", "--depth", "1", "--", &format!("https://github.com/{owner}/{name}.git")]).arg(&dest);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().context("running git (is it installed?)")?;
    if !out.status.success() {
        bail!("git clone failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(chrono_like_date(0), "1970-01-01");
        assert_eq!(chrono_like_date(1_792_540_800), "2026-10-21");
        assert_eq!(chrono_like_date(951_782_400), "2000-02-29");
    }

    #[test]
    fn parses_an_entry() {
        let html = r##"<article class="Box-row"><h2 class="h3"><a href="/acme/rocket" class="Link">acme / rocket</a></h2>
<p class="col-9 color-fg-muted my-1">Fast &amp; small</p>
<span class="repo-language-color" style="background-color: #dea584"></span><span itemprop="programmingLanguage">Rust</span>
<a href="/acme/rocket/stargazers" data-view-component="true" class="tmp-mr-3 Link"><svg height="16"></svg>
 12,345</a><a href="/acme/rocket/forks" class="tmp-mr-3 Link"><svg></svg>
 678</a><span class="d-inline-block float-sm-right"><svg></svg>
 1,234 stars today</span></article>"##;
        let r = &parse_trending(html)[0];
        assert_eq!((r.owner.as_str(), r.name.as_str()), ("acme", "rocket"));
        assert_eq!(r.description.as_deref(), Some("Fast & small"));
        assert_eq!(r.language.as_deref(), Some("Rust"));
        assert_eq!(r.color.as_deref(), Some("#dea584"));
        assert_eq!((r.stars, r.forks, r.gained), (12_345, 678, Some(1_234)));
    }
}
