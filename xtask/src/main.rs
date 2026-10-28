//! Acceptance runner for fermi.
//!
//!   cargo run --locked -p xtask -- acceptance --id MA-04
//!   cargo run --locked -p xtask -- acceptance --system MA
//!   cargo run --locked -p xtask -- acceptance --implemented      # cases that have a test (the PR gate)
//!   cargo run --locked -p xtask -- acceptance --all               # all 106 (the readiness gate)
//!   cargo run --locked -p xtask -- acceptance --all --profile security
//!   cargo run --locked -p xtask -- acceptance --all --profile live --allow-billable --live-accounts my-claude
//!   cargo run --locked -p xtask -- report --run-id <runId>
//!   cargo run --locked -p xtask -- dashboard-report --run-id <runId>   # after the dashboard tests: summary and frontend coverage
//!
//! The registry `tests/acceptance/systems.yaml` maps case ids to tests. Each
//! test writes its own result.json and artifacts; the runner keeps the
//! process logs, verifies the result and every artifact hash before it
//! counts a pass, and records cases without a test as `not_implemented`.
//! It exits nonzero unless every selected case passed.
//!
//! Tests learn the profile from `ACCEPTANCE_PROFILE`. In the live profile the
//! cases that compare models run on the accounts given with `--live-accounts`
//! (taken from the user's own fermi config); without any they report blocked.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

// ── the registry ────────────────────────────────────────────────────────────

/// Reads the registry: a list of cases with `key: value` fields (scalars,
/// JSON-quoted strings, `[a, b]` lists and null).
fn registry() -> Result<Vec<BTreeMap<String, Value>>, String> {
    let text = std::fs::read_to_string(root().join("tests/acceptance/systems.yaml")).map_err(|e| format!("reading the registry: {e}"))?;
    let mut cases: Vec<BTreeMap<String, Value>> = vec![];
    for (n, line) in text.lines().enumerate() {
        let t = line.trim_end();
        if t.trim_start().starts_with('#') || t.trim().is_empty() || !t.starts_with("  ") {
            continue;
        }
        let body = t.trim_start().trim_start_matches("- ");
        let Some((k, v)) = body.split_once(':') else { return Err(format!("line {}: expected key: value", n + 1)) };
        if t.trim_start().starts_with("- ") {
            cases.push(BTreeMap::new());
        }
        let v = v.trim();
        let val = if v == "null" {
            Value::Null
        } else if v == "true" || v == "false" {
            Value::Bool(v == "true")
        } else if v.starts_with('"') {
            serde_json::from_str(v).map_err(|e| format!("line {}: {e}", n + 1))?
        } else if let Some(list) = v.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
            Value::Array(list.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).map(|s| json!(s)).collect())
        } else if let Ok(num) = v.parse::<u64>() {
            json!(num)
        } else {
            json!(v)
        };
        cases.last_mut().ok_or(format!("line {}: field before the first case", n + 1))?.insert(k.trim().to_string(), val);
    }
    let mut seen = std::collections::HashSet::new();
    for c in &cases {
        let id = c.get("id").and_then(|v| v.as_str()).ok_or("a case without an id")?;
        if !seen.insert(id.to_string()) {
            return Err(format!("duplicate case id {id}"));
        }
    }
    Ok(cases)
}

fn s<'a>(c: &'a BTreeMap<String, Value>, k: &str) -> &'a str {
    c.get(k).and_then(|v| v.as_str()).unwrap_or("")
}

fn list(c: &BTreeMap<String, Value>, k: &str) -> Vec<String> {
    c.get(k).and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

// ── evidence checks ─────────────────────────────────────────────────────────

fn sha256(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

/// A result counts only if it exists, is for this case, passed, made
/// assertions, and every listed artifact is present with its hash.
fn validate(dir: &Path, id: &str) -> Result<Value, String> {
    let r: Value =
        serde_json::from_slice(&std::fs::read(dir.join("result.json")).map_err(|_| "no result.json (missing evidence)".to_string())?)
            .map_err(|e| format!("result.json: {e}"))?;
    if r["testId"] != id {
        return Err(format!("result is for {} not {id}", r["testId"]));
    }
    if r["assertions"].as_array().is_none_or(|a| a.is_empty()) {
        return Err("no assertions recorded".into());
    }
    let manifest: Value = serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).map_err(|_| "no manifest.json".to_string())?)
        .map_err(|e| e.to_string())?;
    for f in manifest["files"].as_array().into_iter().flatten() {
        let p = dir.join(f["path"].as_str().unwrap_or(""));
        let bytes = std::fs::read(&p).map_err(|_| format!("artifact missing: {}", p.display()))?;
        if sha256(&bytes) != f["sha256"].as_str().unwrap_or("") {
            return Err(format!("artifact changed after the test: {}", p.display()));
        }
    }
    if r["status"] != "passed" {
        return Err(format!("failed: {}", r["reason"].as_str().unwrap_or("see result.json")));
    }
    Ok(r)
}

// ── commands ────────────────────────────────────────────────────────────────

struct Opts {
    ids: Vec<String>,
    systems: Vec<String>,
    all: bool,
    implemented: bool,
    profile: String,
    allow_billable: bool,
    /// Treat a `blocked` case (the platform can't exercise it) as acceptable, not a failure.
    allow_blocked: bool,
    repeat: Option<u32>,
    /// Accounts from the user's fermi config the live profile may use.
    live_accounts: Vec<String>,
}

fn utc_stamp() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    format!("run-{secs}")
}

fn acceptance(o: Opts) -> Result<bool, String> {
    if o.profile == "live" && !o.allow_billable {
        return Err("the live profile makes metered provider calls: add --allow-billable".into());
    }
    let cases = registry()?;
    let selected: Vec<&BTreeMap<String, Value>> = cases
        .iter()
        .filter(|c| {
            let id = s(c, "id");
            (o.all
                || o.ids.iter().any(|x| x == id)
                || o.systems.iter().any(|sys| id.starts_with(&format!("{sys}-")))
                || (o.implemented && !c["testName"].is_null()))
                && list(c, "profiles").contains(&o.profile)
        })
        .collect();
    for want in &o.ids {
        if !cases.iter().any(|c| s(c, "id") == want) {
            return Err(format!("unknown case id {want}"));
        }
    }
    if selected.is_empty() {
        return Err(format!("the filter selects no case with profile {}", o.profile));
    }
    let run_id = std::env::var("ACCEPTANCE_RUN_ID").unwrap_or_else(|_| utc_stamp());
    let reports = root().join("tests/reports").join(&run_id);
    std::fs::create_dir_all(&reports).map_err(|e| e.to_string())?;
    eprintln!("run {run_id}: {} case(s), profile {}", selected.len(), o.profile);
    // The revision under test is the one the tests are built from: record it now.
    let commit = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root())
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    let dirty = Command::new("git").args(["status", "--porcelain"]).current_dir(root()).output().ok().is_some_and(|o| !o.stdout.is_empty());

    // Build once; a build failure fails every case rather than passing none.
    let build = Command::new("cargo")
        .args(["test", "-p", "acceptance-tests", "--locked", "--no-run"])
        .current_dir(root())
        .status()
        .map_err(|e| e.to_string())?;
    let mut summary = vec![];
    for c in selected {
        let id = s(c, "id").to_string();
        let started = std::time::Instant::now();
        let mut attempts = vec![];
        let status;
        if c["testName"].is_null() {
            let dir = reports.join(&id).join("a1");
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let r = json!({ "schemaVersion": 1, "testId": id, "runId": run_id, "attemptId": "a1", "executionState": "not_run",
                "status": "not_implemented", "reason": format!("no executable test; capability {}: {}", s(c, "capability"), s(c, "auditNote")) });
            std::fs::write(dir.join("result.json"), serde_json::to_vec_pretty(&r).unwrap()).map_err(|e| e.to_string())?;
            status = "not_implemented".to_string();
            attempts.push(json!({ "attempt": "a1", "status": status, "reason": "no executable test" }));
        } else if !build.success() {
            status = "failed".into();
            attempts.push(json!({ "attempt": "a1", "status": "failed", "reason": "the acceptance tests did not build" }));
        } else {
            let reps = o.repeat.or(c.get("repetitions").and_then(|v| v.as_u64()).map(|n| n as u32)).unwrap_or(1).max(1);
            let file = Path::new(s(c, "testFile")).file_stem().and_then(|f| f.to_str()).unwrap_or("").to_string();
            let name = s(c, "testName").to_string();
            let mut all_ok = true;
            let mut blocked = false;
            for a in 1..=reps {
                let attempt = format!("a{a}");
                let dir = reports.join(&id).join(&attempt);
                let out = Command::new("cargo")
                    .args(["test", "-p", "acceptance-tests", "--locked", "--test", &file, "--", &name, "--exact", "--nocapture"])
                    .current_dir(root())
                    .env("ACCEPTANCE_RUN_ID", &run_id)
                    .env("ACCEPTANCE_ATTEMPT", &attempt)
                    .env("ACCEPTANCE_REPORTS", root().join("tests/reports"))
                    .env("ACCEPTANCE_PROFILE", &o.profile)
                    .env("ACCEPTANCE_LIVE_ACCOUNTS", o.live_accounts.join(","))
                    .output()
                    .map_err(|e| e.to_string())?;
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                let _ = std::fs::write(dir.join("stdout.log"), &stdout);
                let _ = std::fs::write(dir.join("stderr.log"), &out.stderr);
                // A test that can't run here (the platform lacks what it checks) says so.
                let result: Option<Value> = std::fs::read(dir.join("result.json")).ok().and_then(|b| serde_json::from_slice(&b).ok());
                if let Some(r) = result.filter(|r| r["status"] == "blocked" && r["testId"] == id.as_str() && out.status.success()) {
                    blocked = true;
                    attempts.push(json!({ "attempt": attempt, "status": "blocked", "reason": r["reason"] }));
                    eprintln!("  {id} {attempt}: blocked ({})", r["reason"].as_str().unwrap_or(""));
                    continue;
                }
                let ran_one = stdout.contains("test result: ok. 1 passed") || stdout.contains("1 failed");
                let verdict = if !ran_one {
                    Err("the filter ran no test".to_string())
                } else {
                    validate(&dir, &id).and_then(|r| if out.status.success() { Ok(r) } else { Err("test process failed".into()) })
                };
                let ok = verdict.is_ok();
                all_ok &= ok;
                attempts.push(json!({ "attempt": attempt, "status": if ok { "passed" } else { "failed" }, "exitCode": out.status.code(), "reason": verdict.err() }));
                eprintln!("  {id} {attempt}: {}", if ok { "passed" } else { "FAILED" });
            }
            status = if !all_ok {
                "failed".into()
            } else if blocked {
                "blocked".into()
            } else {
                "passed".into()
            };
        }
        eprintln!("{id}: {status}");
        summary.push(json!({ "id": id, "system": s(c, "system"), "status": status, "attempts": attempts, "durationMs": started.elapsed().as_millis() as u64 }));
    }
    let manifest = json!({ "runId": run_id, "profile": o.profile, "commitSha": commit, "dirtyTree": dirty, "os": std::env::consts::OS, "cases": summary });
    std::fs::write(reports.join("summary.json"), serde_json::to_vec_pretty(&manifest).unwrap()).map_err(|e| e.to_string())?;
    report(&run_id)?;
    let acceptable = |st: &str| st == "passed" || (o.allow_blocked && st == "blocked");
    Ok(summary.iter().all(|c| acceptable(c["status"].as_str().unwrap_or(""))))
}

/// Writes REPORT.md for a run: every one of the registry's cases is counted,
/// whether it ran or not, so missing work stays visible.
fn report(run_id: &str) -> Result<(), String> {
    let dir = root().join("tests/reports").join(run_id);
    let sum: Value = serde_json::from_slice(&std::fs::read(dir.join("summary.json")).map_err(|_| format!("no summary for {run_id}"))?)
        .map_err(|e| e.to_string())?;
    let ran: BTreeMap<String, Value> =
        sum["cases"].as_array().into_iter().flatten().map(|c| (c["id"].as_str().unwrap_or("").to_string(), c.clone())).collect();
    let cases = registry()?;
    let mut by_sys: BTreeMap<String, BTreeMap<&str, usize>> = BTreeMap::new();
    let mut rows = vec![];
    for c in &cases {
        let id = s(c, "id");
        // Outside this run: a case without a test is not_implemented, one with a test simply didn't run.
        let absent = if c["testName"].is_null() { "not_implemented" } else { "not_run" };
        let st = ran.get(id).and_then(|r| r["status"].as_str()).unwrap_or(absent);
        *by_sys.entry(s(c, "system").to_string()).or_default().entry(st).or_default() += 1;
        *by_sys.entry(s(c, "system").to_string()).or_default().entry("total").or_default() += 1;
        rows.push(format!("| {id} | {} | {st} | {} |", s(c, "title"), s(c, "capability")));
    }
    let total = cases.len();
    let passed: usize = by_sys.values().map(|m| m.get("passed").copied().unwrap_or(0)).sum();
    let mut md = format!(
        "# Acceptance report: {run_id}\n\nRevision `{}`{} · profile {} · {}\n\n**Overall acceptance: {passed} / {total} passed.** Cases that did not run, or have no test, stay in the denominator.\n\n",
        sum["commitSha"].as_str().unwrap_or("?"),
        if sum["dirtyTree"] == true { " (dirty tree)" } else { "" },
        sum["profile"].as_str().unwrap_or("?"),
        sum["os"].as_str().unwrap_or("?")
    );
    md += "| System | Cases | Passed | Failed | Blocked | Not implemented | Not run |\n| --- | --- | --- | --- | --- | --- | --- |\n";
    for (sys, m) in &by_sys {
        let g = |k: &str| m.get(k).copied().unwrap_or(0);
        md += &format!(
            "| {sys} | {} | {} | {} | {} | {} | {} |\n",
            g("total"),
            g("passed"),
            g("failed"),
            g("blocked"),
            g("not_implemented"),
            g("not_run")
        );
    }
    md += "\n| ID | Case | Result | Capability (audit) |\n| --- | --- | --- | --- |\n";
    md += &rows.join("\n");
    md += "\n\nEvidence: `tests/reports/<runId>/<caseId>/<attempt>/` (result.json, manifest.json, artifacts, stdout.log, stderr.log).\n";
    std::fs::write(dir.join("REPORT.md"), md).map_err(|e| e.to_string())?;
    eprintln!("report: {}", dir.join("REPORT.md").display());
    Ok(())
}

// ── the dashboard tests ─────────────────────────────────────────────────────

const PAGE: &str = "crates/fermi/web/index.html";

/// The page's inline script as the browser sees it, and the line of index.html it starts on.
fn page_script() -> Result<(String, usize), String> {
    let html = std::fs::read_to_string(root().join(PAGE)).map_err(|e| format!("reading {PAGE}: {e}"))?;
    let open = html.find("<script>").ok_or("the page has no inline script")? + "<script>".len();
    let close = open + html[open..].find("</script>").ok_or("the page's inline script isn't closed")?;
    // HTML parsing turns CRLF and lone CR into LF (a Windows checkout has CRLF).
    let src = html[open..close].replace("\r\n", "\n").replace('\r', "\n");
    Ok((src, html[..open].matches('\n').count() + 1))
}

fn percent(hit: usize, found: usize) -> Value {
    json!({ "found": found, "hit": hit, "percent": if found == 0 { 0.0 } else { (hit as f64 * 1000.0 / found as f64).round() / 10.0 } })
}

/// Summarizes a run of the dashboard tests (UI-xx, which the registry doesn't
/// list): its summary.json, and the frontend coverage merged over every case
/// (frontend-coverage.json, frontend.lcov). The page script's coverage counts
/// a line as run when any of its code ran, and a function when it was called.
fn dashboard_report(run_id: &str) -> Result<bool, String> {
    let dir = root().join("tests/reports").join(run_id);
    let (src, first_line) = page_script()?;
    // Each UTF-16 code unit of the script (the browser's offsets): its line and whether it's code.
    let mut units: Vec<(usize, bool)> = vec![];
    let mut line = first_line;
    for ch in src.chars() {
        units.extend(std::iter::repeat_n((line, !ch.is_whitespace()), ch.len_utf16()));
        if ch == '\n' {
            line += 1;
        }
    }
    let mut ids: Vec<String> = std::fs::read_dir(&dir)
        .map_err(|_| format!("no reports for {run_id}"))?
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
        .filter(|n| n.starts_with("UI-"))
        .collect();
    ids.sort();
    let (mut ran, mut functions) = (vec![false; units.len()], BTreeMap::<(usize, usize), (String, bool)>::new());
    let (mut cases, mut covered, mut all_passed, mut first) = (vec![], vec![], true, Value::Null);
    for id in &ids {
        let mut attempts: Vec<PathBuf> =
            std::fs::read_dir(dir.join(id)).into_iter().flatten().filter_map(|e| e.ok().map(|e| e.path())).collect();
        attempts.sort();
        let Some(at) = attempts.pop() else { continue };
        let r: Value = std::fs::read(at.join("result.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null);
        let status = match validate(&at, id) {
            Ok(_) => "passed".to_string(),
            Err(e) => {
                eprintln!("{id}: {e}");
                all_passed = false;
                r["status"].as_str().filter(|s| *s != "passed").unwrap_or("failed").to_string()
            }
        };
        if first.is_null() {
            first = r.clone();
        }
        let checks: Vec<Value> = r["assertions"].as_array().into_iter().flatten().map(|a| a["name"].clone()).collect();
        cases.push(json!({ "id": id, "status": status, "durationMs": r["durationMs"], "checks": checks }));
        let Ok(bytes) = std::fs::read(at.join("frontend-coverage.json")) else { continue };
        let cov: Value = serde_json::from_slice(&bytes).map_err(|e| format!("{id}: frontend-coverage.json: {e}"))?;
        if cov["loads"].as_u64().unwrap_or(0) == 0 {
            eprintln!("{id}: recorded no frontend coverage");
            continue;
        }
        if cov["length"].as_u64() != Some(units.len() as u64) {
            return Err(format!(
                "{id} ran a page script of {} code units, but {PAGE} now has {}: rebuild and rerun the dashboard tests",
                cov["length"],
                units.len()
            ));
        }
        for span in cov["ran"].as_array().into_iter().flatten() {
            let (s, e) = (span[0].as_u64().unwrap_or(0) as usize, span[1].as_u64().unwrap_or(0) as usize);
            ran[s.min(units.len())..e.min(units.len())].fill(true);
        }
        for f in cov["functions"].as_array().into_iter().flatten() {
            let key = (f["start"].as_u64().unwrap_or(0) as usize, f["end"].as_u64().unwrap_or(0) as usize);
            functions.entry(key).or_insert_with(|| (f["name"].as_str().unwrap_or_default().to_string(), false)).1 |= f["ran"] == true;
        }
        covered.push(id.clone());
    }
    if covered.is_empty() {
        return Err(format!("no dashboard test in {run_id} recorded frontend coverage"));
    }

    let mut lines: BTreeMap<usize, bool> = BTreeMap::new();
    for (&(line, code), &hit) in units.iter().zip(&ran).filter(|(u, _)| u.1) {
        *lines.entry(line).or_default() |= code && hit;
    }
    let code_units = units.iter().filter(|u| u.1).count();
    let code_ran = units.iter().zip(&ran).filter(|(u, &hit)| u.1 && hit).count();
    let line_of = |offset: usize| units.get(offset).map_or(first_line, |u| u.0);
    let name = |n: &str, offset: usize| format!("{}@{}", if n.is_empty() { "(anonymous)" } else { n }, line_of(offset));
    let totals = json!({
        "lines": percent(lines.values().filter(|&&h| h).count(), lines.len()),
        "functions": percent(functions.values().filter(|f| f.1).count(), functions.len()),
        "codeCharacters": percent(code_ran, code_units),
    });
    let not_run: Vec<Value> = functions.iter().filter(|(_, f)| !f.1).map(|(&(s, _), f)| json!(name(&f.0, s))).collect();
    let report = json!({ "runId": run_id, "page": PAGE, "script": "the inline <script>", "cases": covered, "totals": totals, "functionsNotRun": not_run });
    std::fs::write(dir.join("frontend-coverage.json"), serde_json::to_string_pretty(&report).unwrap()).map_err(|e| e.to_string())?;

    let mut lcov = format!("TN:\nSF:{PAGE}\n");
    for (&(s, _), (n, _)) in &functions {
        lcov += &format!("FN:{},{}\n", line_of(s), name(n, s));
    }
    for (&(s, _), (n, hit)) in &functions {
        lcov += &format!("FNDA:{},{}\n", u8::from(*hit), name(n, s));
    }
    lcov += &format!("FNF:{}\nFNH:{}\n", functions.len(), functions.values().filter(|f| f.1).count());
    for (line, hit) in &lines {
        lcov += &format!("DA:{line},{}\n", u8::from(*hit));
    }
    lcov += &format!("LF:{}\nLH:{}\nend_of_record\n", lines.len(), lines.values().filter(|&&h| h).count());
    std::fs::write(dir.join("frontend.lcov"), lcov).map_err(|e| e.to_string())?;

    let summary = json!({
        "runId": run_id, "commitSha": first["commitSha"], "dirtyTree": first["dirtyTree"], "os": std::env::consts::OS,
        "suite": "dashboard (crates/acceptance-tests/tests/dashboard.rs)", "cases": cases, "frontendCoverage": totals,
    });
    std::fs::write(dir.join("summary.json"), serde_json::to_string_pretty(&summary).unwrap()).map_err(|e| e.to_string())?;
    let p = |k: &str| format!("{} / {} ({}%)", totals[k]["hit"], totals[k]["found"], totals[k]["percent"]);
    eprintln!(
        "dashboard tests: {} of {} passed\nfrontend coverage ({PAGE}, merged over {}): lines {}, functions {}, code characters {}\nreport: {}",
        cases.iter().filter(|c| c["status"] == "passed").count(),
        cases.len(),
        covered.join(", "),
        p("lines"),
        p("functions"),
        p("codeCharacters"),
        dir.join("frontend-coverage.json").display()
    );
    Ok(all_passed)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().any(|a| a == name);
    let values = |name: &str| -> Vec<String> { args.windows(2).filter(|w| w[0] == name).map(|w| w[1].clone()).collect() };
    let res = match args.first().map(String::as_str) {
        Some("acceptance") => acceptance(Opts {
            ids: values("--id"),
            systems: values("--system"),
            all: flag("--all"),
            implemented: flag("--implemented"),
            profile: values("--profile").pop().unwrap_or_else(|| "deterministic".into()),
            allow_billable: flag("--allow-billable"),
            allow_blocked: flag("--allow-blocked"),
            repeat: values("--repeat").pop().and_then(|r| r.parse().ok()),
            live_accounts: values("--live-accounts").iter().flat_map(|v| v.split(',')).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
        }),
        Some("report") => match values("--run-id").pop() {
            Some(r) => report(&r).map(|_| true),
            None => Err("report needs --run-id".into()),
        },
        Some("dashboard-report") => match values("--run-id").pop() {
            Some(r) => dashboard_report(&r),
            None => Err("dashboard-report needs --run-id".into()),
        },
        _ => Err("usage: xtask acceptance [--id X]… [--system AI]… [--implemented | --all] [--allow-blocked] [--profile deterministic|security|live] [--allow-billable] [--live-accounts a,b] [--repeat N]\n       xtask report --run-id <runId>\n       xtask dashboard-report --run-id <runId>".into()),
    };
    match res {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => {
            eprintln!("not every selected case passed");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}
