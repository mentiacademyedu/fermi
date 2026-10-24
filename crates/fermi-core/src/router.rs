//! Task classification and model candidate ordering. Deliberately rule-based:
//! zero latency, zero tokens, predictable. Users who want control use
//! `pinned`/`single` mode or `-m`.

use crate::config::{Account, Mode, Profile};
use crate::models::{Cap, ModelSpec};
use anyhow::{anyhow, bail, Result};

#[derive(Debug, Clone)]
pub struct Need {
    pub cap: Cap,
    pub tier: u8,
    pub why: String,
}

const IMAGE: &[&str] = &[
    "image",
    "picture",
    "photo",
    "logo",
    "illustration",
    "icon",
    "drawing",
    "draw ",
    "wallpaper",
    "poster",
    "sprite",
    "artwork",
    "thumbnail",
    "imagen",
    "dibujo",
    "foto",
    "ilustraci",
    "portada",
];
const CODE: &[&str] = &[
    "bug",
    "fix",
    "function",
    "code",
    "compile",
    "error",
    "test",
    "refactor",
    "api",
    "endpoint",
    "upload",
    "css",
    "html",
    "component",
    "class ",
    "module",
    "crate",
    "repo",
    "script",
    "build",
    "deploy",
    "database",
    "query",
    ".rs",
    ".ts",
    ".tsx",
    ".py",
    ".js",
    ".go",
    "código",
    "codigo",
];
const HEAVY: &[&str] = &[
    "architect",
    "design a",
    "refactor",
    "migrate",
    "debug",
    "investigate",
    "root cause",
    "plan ",
    "security",
    "optimi",
    "performance",
    "from scratch",
    "implement",
    "rewrite",
];
const LIGHT: &[&str] =
    &["summarize", "summarise", "translate", "typo", "rename", "reformat", "one-liner", "quick question", "resume", "traduce"];

/// `w` occurs at a word start ("api" matches "api calls", not "capital").
/// Extensions like ".rs" match anywhere.
fn has_word(p: &str, w: &str) -> bool {
    p.match_indices(w).any(|(i, _)| w.starts_with('.') || p[..i].chars().next_back().is_none_or(|c| !c.is_alphanumeric()))
}

pub fn classify(prompt: &str) -> Need {
    let p = prompt.to_lowercase();
    let hit = |words: &[&str]| words.iter().find(|w| has_word(&p, w)).map(|w| w.trim().to_string());
    let code = hit(CODE);
    if let (Some(w), None) = (hit(IMAGE), &code) {
        return Need { cap: Cap::Image, tier: 2, why: format!("image request (\"{w}\")") };
    }
    let (tier, tier_why) = if let Some(w) = hit(HEAVY) {
        (3, format!("heavy (\"{w}\")"))
    } else if p.len() > 1500 {
        (3, "long prompt".into())
    } else if let (Some(w), true) = (hit(LIGHT), p.len() < 400) {
        (1, format!("light (\"{w}\")"))
    } else {
        (2, "standard".into())
    };
    let cap = match (&code, tier) {
        (Some(_), _) => Cap::Code,
        (None, 3) => Cap::Reasoning,
        (None, 1) => Cap::Fast,
        _ => Cap::General,
    };
    let why = match code {
        Some(w) => format!("code (\"{w}\"), {tier_why}"),
        None => tier_why,
    };
    Need { cap, tier, why }
}

/// Lower is better: matching capability first, then closest tier (prefer one
/// tier up over one tier down).
fn score(m: &ModelSpec, need: &Need) -> u32 {
    let cap = if m.has(need.cap) { 0 } else { 10 };
    let tier = if m.tier >= need.tier { (m.tier - need.tier) as u32 * 2 } else { (need.tier - m.tier) as u32 * 2 + 1 };
    cap + tier
}

/// Models to try, in order, for this task in this profile.
pub fn candidates(
    catalog: &[ModelSpec],
    profile: &Profile,
    accounts: &[&Account],
    need: &Need,
    forced: Option<&str>,
) -> Result<Vec<ModelSpec>> {
    let find = |id: &str| catalog.iter().find(|m| m.id == id).cloned().ok_or_else(|| anyhow!("unknown model `{id}`"));
    let reachable = |m: &ModelSpec| accounts.iter().any(|a| Some(a.provider) == m.provider());

    if let Some(id) = forced {
        let m = find(id)?;
        if !reachable(&m) {
            bail!("profile `{}` has no {} account for `{id}`", profile.name, id.split(':').next().unwrap_or(""));
        }
        return Ok(vec![m]);
    }

    let mut list: Vec<ModelSpec> = match profile.mode {
        Mode::Single => vec![find(&profile.models[0])?],
        Mode::Pinned => profile.models.iter().map(|id| find(id)).collect::<Result<_>>()?,
        Mode::Auto => catalog.iter().filter(|m| m.auto).cloned().collect(),
    };
    list.retain(|m| reachable(m));
    if need.cap != Cap::Image {
        list.retain(|m| !m.image_only());
    } else if list.iter().any(|m| m.has(Cap::Image)) {
        // Image tasks go straight to an image model when one is reachable;
        // otherwise a text model takes them (and can delegate over MCP).
        list.retain(|m| m.has(Cap::Image));
    }
    // An economy or quality policy (see `policy`) judges every model itself.
    let classic = profile.routing == crate::config::Routing::Auto;
    if profile.mode == Mode::Auto && classic {
        let matching: Vec<_> = list.iter().filter(|m| m.has(need.cap)).cloned().collect();
        if !matching.is_empty() {
            list = matching;
        }
    }
    if profile.mode != Mode::Single && classic {
        list.sort_by_key(|m| score(m, need));
    }
    if list.is_empty() {
        bail!("profile `{}` has no account that can run any of its models", profile.name);
    }
    Ok(list)
}

/// Version of the policy rules below; recorded in every decision.
pub const POLICY_VERSION: u32 = 1;

/// A model's estimated cost for the task (micro-dollars) and declared quality, when known.
pub type ModelInfo<'a> = dyn Fn(&ModelSpec) -> (Option<i64>, Option<u32>) + 'a;

/// One model as a policy saw it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Candidate {
    pub model: String,
    pub eligible: bool,
    /// Why it's ineligible, or how it was ranked.
    pub notes: Vec<String>,
    /// Estimated cost of this task on it (micro-dollars), when it has a price.
    pub estimate: Option<i64>,
    /// Declared quality rank (price table), when set.
    pub quality: Option<u32>,
    pub tier: u8,
}

/// The record of an economy or quality routing decision.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Decision {
    pub policy: String,
    pub version: u32,
    pub need: String,
    pub rule: String,
    /// The profile's remaining budget (micro-dollars), when it has one.
    pub remaining: Option<i64>,
    pub candidates: Vec<Candidate>,
    /// Eligible models, best first.
    pub order: Vec<String>,
    pub selected: String,
}

/// Orders `list` by a cost or quality policy. A model is eligible when it has
/// the task's capability (never traded for price or rank) and its estimate
/// fits the remaining budget. Economy: lowest estimate, then higher declared
/// quality, then id; models without a price come last. Quality: highest
/// declared rank (from the price table, never what a model says about
/// itself), then lower estimate, then id; models without a rank come last.
pub fn policy(
    routing: crate::config::Routing,
    list: Vec<ModelSpec>,
    need: &Need,
    info: &ModelInfo<'_>,
    remaining: Option<i64>,
) -> Result<(Vec<ModelSpec>, Decision)> {
    use crate::config::Routing;
    let economy = routing == Routing::Economy;
    let mut cands = vec![];
    let mut ok: Vec<(ModelSpec, Option<i64>, Option<u32>)> = vec![];
    for m in list {
        let (est, quality) = info(&m);
        let mut notes = vec![];
        if !m.has(need.cap) {
            notes.push(format!("lacks the {:?} capability this task needs", need.cap).to_lowercase());
        }
        if let (Some(r), Some(e)) = (remaining, est) {
            if e > r {
                notes.push(format!("estimate {} is over the remaining budget {}", crate::billing::fmt_usd(e), crate::billing::fmt_usd(r)));
            }
        }
        let eligible = notes.is_empty();
        if eligible {
            if economy && est.is_none() {
                notes.push("no price: ranked after priced models".into());
            }
            if !economy && quality.is_none() {
                notes.push("no declared quality: ranked last".into());
            }
            ok.push((m.clone(), est, quality));
        }
        cands.push(Candidate { model: m.id.clone(), eligible, notes, estimate: est, quality, tier: m.tier });
    }
    if economy {
        ok.sort_by(|a, b| {
            (a.1.is_none(), a.1.unwrap_or(0), std::cmp::Reverse(a.2.unwrap_or(0)), &a.0.id).cmp(&(
                b.1.is_none(),
                b.1.unwrap_or(0),
                std::cmp::Reverse(b.2.unwrap_or(0)),
                &b.0.id,
            ))
        });
    } else {
        ok.sort_by(|a, b| {
            (a.2.is_none(), std::cmp::Reverse(a.2.unwrap_or(0)), a.1.is_none(), a.1.unwrap_or(0), &a.0.id).cmp(&(
                b.2.is_none(),
                std::cmp::Reverse(b.2.unwrap_or(0)),
                b.1.is_none(),
                b.1.unwrap_or(0),
                &b.0.id,
            ))
        });
    }
    let order: Vec<ModelSpec> = ok.into_iter().map(|x| x.0).collect();
    let Some(first) = order.first() else {
        bail!(
            "no model fits this task under the {} policy: {}",
            if economy { "economy" } else { "quality" },
            cands.iter().map(|c| format!("{} ({})", c.model, c.notes.join("; "))).collect::<Vec<_>>().join(", ")
        );
    };
    let decision = Decision {
        policy: if economy { "economy".into() } else { "quality".into() },
        version: POLICY_VERSION,
        need: format!("{:?} — {}", need.cap, need.why).to_lowercase(),
        rule: if economy {
            "eligible = has the capability and fits the budget; lowest estimate, then higher declared quality, then id".into()
        } else {
            "eligible = has the capability and fits the budget; highest declared quality, then lower estimate, then id".into()
        },
        remaining,
        selected: first.id.clone(),
        order: order.iter().map(|m| m.id.clone()).collect(),
        candidates: cands,
    };
    Ok((order, decision))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::path::Path;

    const CFG: &str = r#"
        default_profile = "personal"
        [[accounts]]
        id = "c-home"
        provider = "claude"
        [[accounts]]
        id = "o-home"
        provider = "openai"
        [[accounts]]
        id = "c-work"
        provider = "claude"
        [[profiles]]
        name = "personal"
        accounts = ["c-home", "o-home"]
        [[profiles]]
        name = "work"
        accounts = ["c-work"]
        paths = ["/work"]
        mode = "pinned"
        models = ["claude:opus", "claude:sonnet"]
    "#;

    fn ids(v: &[ModelSpec]) -> Vec<&str> {
        v.iter().map(|m| m.id.as_str()).collect()
    }

    fn plan(cfg: &Config, profile: &str, prompt: &str) -> Vec<ModelSpec> {
        let p = cfg.resolve_profile(Some(profile), Path::new("/")).unwrap();
        let accts: Vec<_> = cfg.profile_accounts(&p).collect();
        candidates(&cfg.catalog(), &p, &accts, &classify(prompt), None).unwrap()
    }

    #[test]
    fn classifies() {
        assert_eq!(classify("generate a logo for my bakery").cap, Cap::Image);
        assert_eq!(classify("fix the image upload bug").cap, Cap::Code);
        assert_eq!(classify("refactor the auth module").tier, 3);
        assert_eq!(classify("translate this to french").tier, 1);
        assert_eq!(classify("haz una imagen de un gato").cap, Cap::Image);
        // word-start matching: no "api" in "capital", no "test" in "latest"
        assert_ne!(classify("what is the capital of the latest country").cap, Cap::Code);
    }

    #[test]
    fn profiles_resolve_by_path() {
        let cfg = Config::parse(CFG).unwrap();
        assert_eq!(cfg.resolve_profile(None, Path::new("/work/acme")).unwrap().name, "work");
        assert_eq!(cfg.resolve_profile(None, Path::new("/workshop")).unwrap().name, "personal");
        assert_eq!(cfg.resolve_profile(None, Path::new("/home/me")).unwrap().name, "personal");
    }

    #[test]
    fn auto_routes_by_need() {
        let cfg = Config::parse(CFG).unwrap();
        assert_eq!(ids(&plan(&cfg, "personal", "draw a logo for a bakery")), ["openai:image"]);
        assert_eq!(plan(&cfg, "personal", "refactor the auth module")[0].id, "claude:opus");
        assert_eq!(plan(&cfg, "personal", "fix the login bug")[0].id, "claude:sonnet");
        assert!(!ids(&plan(&cfg, "personal", "fix the login bug")).contains(&"claude:fable"));
    }

    #[test]
    fn pinned_stays_inside_profile() {
        let cfg = Config::parse(CFG).unwrap();
        // work has no openai account: an image task goes to a pinned Claude model
        let v = plan(&cfg, "work", "draw a logo");
        assert!(v.iter().all(|m| m.id.starts_with("claude:")));
        // pinned models are reordered by fit, never extended
        assert_eq!(ids(&plan(&cfg, "work", "fix the login bug")), ["claude:sonnet", "claude:opus"]);
        assert_eq!(ids(&plan(&cfg, "work", "refactor the login flow")), ["claude:opus", "claude:sonnet"]);
    }

    #[test]
    fn rejects_unknown_refs() {
        assert!(Config::parse(&CFG.replace(r#"["c-work"]"#, r#"["nope"]"#)).is_err());
        assert!(Config::parse(&CFG.replace("claude:sonnet\"]", "claude:nope\"]")).is_err());
    }
}
