//! System 5: the integrated verification engine. Real checks (cargo, clippy,
//! the advisory scanner) on copies of F-CODE; where an agent is involved its
//! output comes from the fake CLI.

use acceptance_tests::{account, fixture, git, repo_root, sha256, wait_task, Case, Fermi};
use fermi_core::events::Event;
use fermi_core::hub::{TaskRec, TaskStatus};
use fermi_core::team::Teams;
use fermi_core::verify::{self, CheckResult, Report, RunOpts, Status, VerifyOpts};
use serde_json::{json, Value};
use std::path::Path;
use std::time::{Duration, Instant};

const T: Duration = Duration::from_secs(600);

fn read(dir: &Path, rel: &str) -> String {
    std::fs::read_to_string(dir.join(rel)).unwrap()
}

fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// Replaces `old` (which must occur once) in a file.
fn edit(dir: &Path, rel: &str, old: &str, new: &str) {
    let s = read(dir, rel);
    assert_eq!(s.matches(old).count(), 1, "{old:?} in {rel}");
    write(dir, rel, &s.replace(old, new));
}

fn commit(dir: &Path, msg: &str) {
    git(dir, &["add", "-A"]).unwrap();
    git(dir, &["commit", "-q", "-m", msg]).unwrap();
}

async fn checks(dir: &Path, only: &[&str]) -> Report {
    let only: Vec<String> = only.iter().map(|s| s.to_string()).collect();
    verify::run(dir, RunOpts { only: &only, ..Default::default() }).await.unwrap()
}

fn check<'a>(r: &'a Report, name: &str) -> &'a CheckResult {
    r.checks.iter().find(|c| c.name == name).unwrap_or_else(|| panic!("no check {name}"))
}

fn verified(max_repairs: Option<u32>) -> Option<VerifyOpts> {
    Some(VerifyOpts { max_repairs, checks: vec![] })
}

/// The task's verification events, in order.
fn verifies(rec: &TaskRec) -> Vec<(u32, bool, Value)> {
    rec.events
        .iter()
        .filter_map(|e| if let Event::Verify { attempt, passed, report } = e { Some((*attempt, *passed, report.clone())) } else { None })
        .collect()
}

fn repairs(rec: &TaskRec) -> Vec<(u32, u32, String)> {
    rec.events
        .iter()
        .filter_map(|e| if let Event::Repair { attempt, max, prompt } = e { Some((*attempt, *max, prompt.clone())) } else { None })
        .collect()
}

fn done(rec: &TaskRec) -> Option<(bool, String)> {
    rec.events.iter().rev().find_map(|e| if let Event::Done { ok, summary } = e { Some((*ok, summary.clone())) } else { None })
}

/// A test the agent must not change, and the bug it catches.
const PROTECTED_TEST: &str =
    "#[test]\nfn describe_formats_name_and_score() {\n    assert_eq!(engine::describe(\" Ada \"), \"ada:1\");\n}\n";

#[tokio::test(flavor = "multi_thread")]
async fn ve_01_compiler_diagnostics() {
    let mut c = Case::start("VE-01", "T1");
    c.real(&["fermi verification service", "cargo check (rustc)", "git"]);
    let fx = fixture("F-CODE", &c.work.join("repo")).unwrap();
    // A syntax error at a known place: util/src/num.rs line 3, the `;` at column 13.
    edit(&fx.path, "util/src/num.rs", "    2\n", "    let x = ;\n    2\n");
    let r = checks(&fx.path, &["build"]).await;
    c.json("report.json", &serde_json::to_value(&r).unwrap());
    let b = check(&r, "build");
    c.check("notReportedAsPassing", !r.passed && b.status == Status::Failed, format!("gate passed={}, build {:?}", r.passed, b.status));
    c.check("nonzeroExit", b.exit_code.is_some_and(|e| e != 0), format!("exit {:?}", b.exit_code));
    let first = b.diagnostics.iter().find(|d| d.severity == "error");
    c.check(
        "correctLocation",
        first.is_some_and(|d| d.file == "util/src/num.rs" && d.line == 3 && d.column == 13),
        format!("first error {first:?}"),
    );
    c.check(
        "testedRevisionRecorded",
        r.revision.as_deref() == Some(fx.commit.as_str()) && r.dirty && r.fingerprint.starts_with(&fx.commit[..12]),
        format!("revision {:?}, dirty {}, fingerprint {}", r.revision, r.dirty, r.fingerprint),
    );
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ve_02_failing_test_feedback() {
    let mut c = Case::start("VE-02", "T1");
    c.real(&["fermi run_checks agent tool (MCP bridge handler)", "hub verification", "cargo test"]);
    c.mocked(&["model output (fake-cli)"]);
    let fx = fixture("F-CODE", &c.work.join("repo")).unwrap();
    edit(
        &fx.path,
        "engine/src/lib.rs",
        "assert_eq!(process(\" X \").1, 1);",
        "assert_eq!(process(\" X \").1, 7, \"VE02-DISTINCT score mismatch\");",
    );
    commit(&fx.path, "a failing test");
    // 1. What the agent sees when it calls the tool.
    let claim = json!({ "text": "All tests pass. Done." });
    let f = Fermi::new(&c, &[account("cl", "claude", vec![claim])], "").unwrap();
    let profile = f.engine.cfg.resolve_profile(Some("p"), &fx.path).unwrap();
    let tool = fermi_core::mcp::call_tool(&f.engine, &profile, &fx.path, "run_checks", &json!({})).await.unwrap();
    c.text("tool-response.txt", &tool);
    c.check("toolReportsFailure", tool.starts_with("FAILED") && !tool.contains("PASSED"), tool.lines().next().unwrap_or(""));
    c.check("failingTestIdRetained", tool.contains("failing test tests::process_scores"), "tests::process_scores in the tool response");
    c.check("failureOutputRetained", tool.contains("VE02-DISTINCT score mismatch"), "the assertion message reaches the agent");
    c.check("nonzeroExitRetained", tool.contains("(exit 101)"), "cargo test's exit code 101");
    // 2. A verified task whose agent claims success: the recorded verification event.
    let hub = f.hub();
    let mut task = f.task("Make the tests pass.", &fx.path);
    task.verify = verified(Some(0));
    let id = hub.submit(task);
    let rec = wait_task(&hub, id, T).await.unwrap();
    c.json("task.json", &serde_json::to_value(&rec).unwrap());
    let v = verifies(&rec);
    let failed: Vec<String> = v
        .first()
        .and_then(|(_, _, r)| r["checks"].as_array().cloned())
        .into_iter()
        .flatten()
        .flat_map(|ch| ch["failed_tests"].as_array().cloned().unwrap_or_default())
        .map(|t| format!("{} | {}", t["name"].as_str().unwrap_or(""), t["output"].as_str().unwrap_or("")))
        .collect();
    c.check(
        "verificationEventRecorded",
        v.len() == 1 && !v[0].1 && failed.iter().any(|t| t.starts_with("tests::process_scores |") && t.contains("VE02-DISTINCT")),
        format!("{} verify event(s), failed tests {failed:?}", v.len()),
    );
    c.check("noSuccessSubstitution", rec.status == TaskStatus::Failed, format!("task {:?} although the agent said it passed", rec.status));
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ve_03_repair_and_rerun() {
    let mut c = Case::start("VE-03", "T1");
    c.real(&["hub verified task loop", "fermi verification service", "cargo test", "git"]);
    c.mocked(&["model output and its edits (fake-cli)"]);
    let work = c.work.clone();
    let setup = |name: &str| {
        let fx = fixture("F-CODE", &work.join(name)).unwrap();
        write(&fx.path, "engine/tests/ve03.rs", PROTECTED_TEST);
        edit(&fx.path, ".fermi/checks.toml", "max_repairs = 3\n", "max_repairs = 3\nprotect = [\"engine/tests/**\"]\n");
        edit(&fx.path, "engine/src/lib.rs", "format!(\"{n}:{s}\")", "format!(\"{n}-{s}\")"); // the bug
        commit(&fx.path, "a bug and a protected test");
        fx
    };
    let lib = |fmt: &str| read(&repo_root().join("tests/fixtures/F-CODE/repo"), "engine/src/lib.rs").replace("format!(\"{n}:{s}\")", fmt);

    // 1. A wrong fix, then the right one after fermi reports the failure.
    let fx = setup("repo");
    let protected_before = sha256(read(&fx.path, "engine/tests/ve03.rs").as_bytes());
    let wrong = json!({ "match": "[[VE03]]", "text": "Fixed the format.", "edits": [{ "path": "engine/src/lib.rs", "content": lib("format!(\"{n} {s}\")") }] });
    let right = json!({ "match": "repair attempt 1", "text": "Fixed it properly.", "edits": [{ "path": "engine/src/lib.rs", "content": lib("format!(\"{n}:{s}\")") }] });
    let f = Fermi::at(&c, "repair", &[account("cl", "claude", vec![wrong, right])], "").unwrap();
    let hub = f.hub();
    let mut task = f.task("Fix the failing test [[VE03]].", &fx.path);
    task.verify = verified(None);
    let rec = wait_task(&hub, hub.submit(task), T).await.unwrap();
    let calls = f.calls("cl");
    c.json("repair-trace.json", &json!({ "task": rec, "calls": calls }));
    c.text("after.patch", &git(&fx.path, &["diff", "HEAD"]).unwrap());
    let v = verifies(&rec);
    let r = repairs(&rec);
    c.check(
        "correctionAttempted",
        calls.len() == 2 && calls[1]["prompt"].as_str().is_some_and(|p| p.contains("describe_formats_name_and_score")),
        format!("{} agent call(s); the second carries the failing test", calls.len()),
    );
    c.check(
        "eachOutcomeRecorded",
        v.iter().map(|x| (x.0, x.1)).collect::<Vec<_>>() == [(0, false), (1, true)] && r.iter().map(|x| x.0).collect::<Vec<_>>() == [1],
        format!("verify {:?}, repairs {:?}", v.iter().map(|x| (x.0, x.1)).collect::<Vec<_>>(), r.iter().map(|x| x.0).collect::<Vec<_>>()),
    );
    c.check("taskDone", rec.status == TaskStatus::Done, format!("{:?}", rec.status));
    // Independently: the unchanged test passes on the edited tree.
    let rerun = std::process::Command::new("cargo")
        .args(["test", "--offline", "--quiet", "--test", "ve03"])
        .current_dir(fx.path.join("engine"))
        .output()
        .unwrap();
    c.text("independent-rerun.txt", &format!("{}{}", String::from_utf8_lossy(&rerun.stdout), String::from_utf8_lossy(&rerun.stderr)));
    let protected_after = sha256(read(&fx.path, "engine/tests/ve03.rs").as_bytes());
    c.check(
        "unchangedTestPasses",
        rerun.status.success() && protected_before == protected_after,
        "cargo test --test ve03, protected file hash unchanged",
    );

    // 2. An agent that edits the protected test instead: no success.
    let fx2 = setup("repo-cheat");
    let cheat = json!({ "text": "Done, the test passes now.", "edits": [{ "path": "engine/tests/ve03.rs", "content": PROTECTED_TEST.replace("ada:1", "ada-1") }] });
    let f2 = Fermi::at(&c, "cheat", &[account("cl", "claude", vec![cheat])], "").unwrap();
    let hub2 = f2.hub();
    let mut task = f2.task("Fix the failing test.", &fx2.path);
    task.verify = verified(Some(0));
    let rec2 = wait_task(&hub2, hub2.submit(task), T).await.unwrap();
    c.json("protected-edit-trace.json", &serde_json::to_value(&rec2).unwrap());
    let v2 = verifies(&rec2);
    let prot =
        v2.first().and_then(|(_, _, r)| r["checks"].as_array().and_then(|a| a.iter().find(|x| x["name"] == "protected files").cloned()));
    c.check(
        "editingTheTestIsNotSuccess",
        rec2.status == TaskStatus::Failed
            && prot
                .as_ref()
                .is_some_and(|p| p["status"] == "failed" && p["reason"].as_str().unwrap_or("").contains("modified engine/tests/ve03.rs")),
        format!("task {:?}, protected check {prot:?}", rec2.status),
    );
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ve_04_lint_enforcement() {
    let mut c = Case::start("VE-04", "T1");
    c.real(&["fermi verification service", "cargo clippy (pinned rule)", "hub verified task"]);
    c.mocked(&["model output (fake-cli)"]);
    let fx = fixture("F-CODE", &c.work.join("repo")).unwrap();
    // One pinned rule, denied: clippy::needless_return.
    let lint = "\n[[check]]\nname = \"lint\"\nkind = \"lint\"\nrun = \"cargo clippy --workspace --all-targets --offline --message-format=json -- -A clippy::all -D clippy::needless_return\"\n";
    let cfg = format!("{}{lint}", read(&fx.path, ".fermi/checks.toml"));
    write(&fx.path, ".fermi/checks.toml", &cfg);
    // The violation: util/src/num.rs line 6, column 5.
    let num = format!("{}pub fn twice(n: u32) -> u32 {{\n    return n * 2;\n}}\n", read(&fx.path, "util/src/num.rs"));
    write(&fx.path, "util/src/num.rs", &num);
    commit(&fx.path, "a lint violation");
    c.text("lint-config.toml", &cfg);
    let r = checks(&fx.path, &["lint"]).await;
    c.json("report.json", &serde_json::to_value(&r).unwrap());
    let l = check(&r, "lint");
    let d = l.diagnostics.iter().find(|d| d.code.as_deref() == Some("clippy::needless_return"));
    c.check("expectedRuleAndLocation", d.is_some_and(|d| d.file == "util/src/num.rs" && d.line == 6 && d.column == 5), format!("{d:?}"));
    c.check(
        "lintFails",
        l.status == Status::Failed && l.exit_code.is_some_and(|e| e != 0) && !r.passed,
        format!("{:?}, exit {:?}", l.status, l.exit_code),
    );
    // The failure reaches the task.
    let f = Fermi::new(&c, &[account("cl", "claude", vec![json!({ "text": "Done." })])], "").unwrap();
    let hub = f.hub();
    let mut task = f.task("Tidy the number helpers.", &fx.path);
    task.verify = Some(VerifyOpts { max_repairs: Some(0), checks: vec!["lint".into()] });
    let rec = wait_task(&hub, hub.submit(task), T).await.unwrap();
    c.json("task.json", &serde_json::to_value(&rec).unwrap());
    let end = done(&rec);
    c.check(
        "failurePropagatesToTask",
        rec.status == TaskStatus::Failed && end.as_ref().is_some_and(|(ok, s)| !ok && s.contains("lint")),
        format!("{:?}: {end:?}", rec.status),
    );
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ve_05_dependency_advisory_detection() {
    let mut c = Case::start("VE-05", "T1");
    c.real(&["fermi advisory scanner (built in)", "pinned OSV dataset", "lockfile parser"]);
    let fx = fixture("F-CODE", &c.work.join("repo")).unwrap();
    let dataset = repo_root().join("tests/fixtures/F-CODE/advisories");
    let cfg = format!(
        "{}\n[[check]]\nname = \"advisories\"\nkind = \"audit\"\nadvisories = {}\n",
        read(&fx.path, ".fermi/checks.toml"),
        serde_json::to_string(&dataset.to_string_lossy().replace('\\', "/")).unwrap()
    );
    write(&fx.path, ".fermi/checks.toml", &cfg);
    let lock = read(&repo_root().join("tests/fixtures/F-CODE/expected"), "vulnerable-Cargo.lock");
    write(&fx.path, "Cargo.lock", &lock);
    c.text("Cargo.lock", &lock);
    let r = checks(&fx.path, &["advisories"]).await;
    c.json("scanner-report.json", &serde_json::to_value(&r).unwrap());
    let a = check(&r, "advisories");
    let ids: Vec<(String, String, String)> = a.findings.iter().map(|f| (f.id.clone(), f.package.clone(), f.version.clone())).collect();
    c.check(
        "advisoryIdentified",
        ids == [("RUSTSEC-2021-0003".to_string(), "smallvec".to_string(), "1.6.0".to_string())] && a.status == Status::Failed,
        format!("{ids:?} (time 0.3.36 is outside RUSTSEC-2020-0071's range)"),
    );
    let tool = a.tool.clone().unwrap_or_default();
    let sha = tool["dataset"]["sha256"].as_str().unwrap_or("");
    c.check(
        "scannerAndDatasetRecorded",
        tool["scanner"] == "fermi advisories"
            && tool["version"].as_str().is_some_and(|v| !v.is_empty())
            && sha.len() == 64
            && tool["dataset"]["records"] == 2
            && tool["dataset"]["modified"] == "2021-01-08T12:00:00Z",
        format!("{tool}"),
    );
    // Nothing installed or run: no command, no build output, only the inputs changed.
    let status = git(&fx.path, &["status", "--porcelain"]).unwrap();
    c.check(
        "nothingExecuted",
        a.command.as_deref().is_some_and(|x| x.starts_with("fermi advisories"))
            && !fx.path.join("target").exists()
            && status.lines().count() == 2,
        format!("command {:?}; git status:\n{status}", a.command),
    );
    // The same lockfile with the fixed version passes.
    write(&fx.path, "Cargo.lock", &lock.replace("version = \"1.6.0\"", "version = \"1.6.1\""));
    let fixed = checks(&fx.path, &["advisories"]).await;
    c.check(
        "fixedVersionPasses",
        check(&fixed, "advisories").status == Status::Passed,
        format!("{:?}", check(&fixed, "advisories").reason),
    );
    c.finish();
}

#[test]
fn ve_06_structured_edit_integrity() {
    let mut c = Case::start("VE-06", "T1");
    c.real(&["fermi code index", "tree-sitter parse", "filesystem"]);
    let fx = fixture("F-CODE", &c.work.join("repo")).unwrap();
    let mut idx = fermi_core::code::Index::open(&fx.path).unwrap();
    let before = read(&fx.path, "util/src/text.rs");
    let other = read(&fx.path, "util/src/num.rs");
    // An ambiguous name and a broken edit are refused, and nothing is written.
    let ambiguous = idx.replace_symbol("helper", "pub fn helper() -> u32 { 3 }");
    let broken = idx.replace_symbol("util::text::helper", "pub fn helper() -> u32 {\n    10\n");
    c.check(
        "unsafeEditsRefused",
        ambiguous.is_err() && broken.is_err() && read(&fx.path, "util/src/text.rs") == before,
        format!("ambiguous: {:?}; broken: {:?}", ambiguous.err().map(|e| e.to_string()), broken.err().map(|e| e.to_string())),
    );
    let res =
        idx.replace_symbol("util::text::helper", "pub fn helper() -> u32 {\n    // Rewritten by a structured edit.\n    10\n}").unwrap();
    c.json("edit.json", &res);
    c.text("edit.patch", res["patch"].as_str().unwrap_or(""));
    let after = read(&fx.path, "util/src/text.rs");
    let expected = read(&repo_root().join("tests/fixtures/F-CODE/expected"), "util-text-helper-edit.rs");
    c.check("matchesExpectedResult", after == expected, "util/src/text.rs equals expected/util-text-helper-edit.rs");
    let (start, end) = (res["before"]["bytes"][0].as_u64().unwrap() as usize, res["before"]["bytes"][1].as_u64().unwrap() as usize);
    let new_end = res["after"]["bytes"][1].as_u64().unwrap() as usize;
    c.check(
        "onlyTargetChanged",
        after.as_bytes()[..start] == before.as_bytes()[..start]
            && after.as_bytes()[new_end..] == before.as_bytes()[end..]
            && read(&fx.path, "util/src/num.rs") == other,
        format!("bytes {start}..{end} replaced; util::num::helper untouched"),
    );
    c.check("stillParses", fermi_core::code::syntax_error_in("util/src/text.rs", after.as_bytes()).is_none(), "no syntax error");
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ve_07_bounded_repair_attempts() {
    let mut c = Case::start("VE-07", "T1");
    c.real(&["hub verified task loop", "fermi verification service", "cargo test"]);
    c.mocked(&["model output (fake-cli)"]);
    let fx = fixture("F-CODE", &c.work.join("repo")).unwrap();
    // Stays failing whatever the agent does: normalize no longer trims.
    edit(&fx.path, "util/src/lib.rs", "s.trim().to_lowercase()", "s.to_lowercase()");
    commit(&fx.path, "a bug the agent won't fix");
    // Five scripted turns, so a fourth repair would be served if it were dispatched.
    let steps: Vec<Value> = (0..5)
        .map(|i| json!({ "text": format!("Attempt {i}."), "edits": [{ "path": "notes.txt", "content": format!("attempt {i}\n") }] }))
        .collect();
    let f = Fermi::new(&c, &[account("cl", "claude", steps)], "").unwrap();
    let hub = f.hub();
    let mut task = f.task("Fix the normalize tests.", &fx.path);
    task.verify = verified(None); // the project's max_repairs = 3
    let rec = wait_task(&hub, hub.submit(task), T).await.unwrap();
    let calls = f.calls("cl");
    c.json("dispatch-trace.json", &json!({ "task": rec, "calls": calls }));
    let r: Vec<(u32, u32)> = repairs(&rec).iter().map(|x| (x.0, x.1)).collect();
    let v: Vec<bool> = verifies(&rec).iter().map(|x| x.1).collect();
    c.check("exactlyThreeRepairs", r == [(1, 3), (2, 3), (3, 3)], format!("repairs {r:?}"));
    c.check("noFourthDispatch", calls.len() == 4, format!("{} agent calls (1 + 3 repairs)", calls.len()));
    let end = done(&rec);
    c.check(
        "unresolvedFailureExplicit",
        rec.status == TaskStatus::Failed
            && v == [false, false, false, false]
            && end.as_ref().is_some_and(|(ok, s)| !ok && s.contains("after 3 repair attempts")),
        format!("{:?}, verify {v:?}, {end:?}", rec.status),
    );
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ve_08_completion_enforcement() {
    let mut c = Case::start("VE-08", "T2");
    c.real(&["hub task service", "dashboard HTTP API (axum)", "team runs and their merge gate", "cargo test", "git"]);
    c.mocked(&["model output (fake-cli)"]);
    let fx = fixture("F-CODE", &c.work.join("repo")).unwrap();
    edit(&fx.path, "util/src/lib.rs", "s.trim().to_lowercase()", "s.to_lowercase()"); // a mandatory check fails
    commit(&fx.path, "a failing mandatory check");
    let rev = git(&fx.path, &["rev-parse", "HEAD"]).unwrap();
    let plan =
        json!({ "summary": "One change.", "tasks": [{ "id": "t1", "role": "developer", "title": "Tidy", "description": "Do [[T1]]." }] });
    let steps = vec![
        json!({ "match": "You are the architect", "text": format!("Plan:\n```json\n{plan}\n```") }),
        json!({ "match": "[[T1]]", "text": "Done, everything passes.", "edits": [{ "path": "notes.txt", "content": "tidied\n" }] }),
        json!({ "text": "All checks pass. Done." }),
    ];
    let f = Fermi::new(&c, &[account("cl", "claude", steps)], "").unwrap();

    // 1. The task service.
    let hub = f.hub();
    let mut task = f.task("Finish the work.", &fx.path);
    task.verify = verified(Some(0));
    let id = hub.submit(task);
    let rec = wait_task(&hub, id, T).await.unwrap();
    // The history file is written right after the in-memory state changes.
    let file = f.home.join("history").join(format!("{id}.json"));
    let mut saved = Value::Null;
    for _ in 0..50 {
        saved = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        if saved["status"] != "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    c.json("service-task.json", &saved);
    let fp = verifies(&rec).first().and_then(|(_, _, r)| r["fingerprint"].as_str().map(str::to_string)).unwrap_or_default();
    c.check(
        "serviceRejects",
        rec.status == TaskStatus::Failed && saved["status"] == "failed" && fp == rev[..12],
        format!("task {:?}, saved {}, tested revision {fp} (HEAD {})", rec.status, saved["status"], &rev[..12]),
    );

    // 2. The public API: a task, then a team run whose merge is the completion step.
    let server = fermi::server::start(f.engine.clone(), 0, fx.path.clone(), "acceptance").await.unwrap();
    let (base, token) = server.url.split_once("/#t=").map(|(b, t)| (b.to_string(), t.to_string())).unwrap();
    let api_hub = server.hub.clone();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let serving = tokio::spawn(server.serve(async {
        let _ = stop_rx.await;
    }));
    let http = reqwest::Client::new();
    let post = |path: &str, body: Value| http.post(format!("{base}{path}")).header("x-fermi-token", &token).json(&body).send();
    let get = |path: &str| http.get(format!("{base}{path}")).header("x-fermi-token", &token).send();
    let created: Value = post(
        "/api/tasks",
        json!({ "prompt": "Finish the work via the API.", "cwd": fx.path, "profile": "p", "verify": { "max_repairs": 0 } }),
    )
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let api_id = created["id"].as_u64().unwrap();
    let start = Instant::now();
    let api_task = loop {
        let t: Value = get(&format!("/api/tasks/{api_id}")).await.unwrap().json().await.unwrap();
        if t["status"] != "running" || start.elapsed() > T {
            break t;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    c.json("api-task.json", &api_task);
    c.check(
        "apiTaskRejected",
        api_task["status"] == "failed" && api_task["checks"] == false,
        format!("status {}, checks {}", api_task["status"], api_task["checks"]),
    );
    let run: Value = post(
        "/api/teams",
        json!({ "goal": "Tidy up", "repo": fx.path, "profile": "p", "parallel": 1, "checks": { "tests": false, "security": false } }),
    )
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let run_id = run["id"].as_u64().unwrap_or_else(|| panic!("no run: {run}"));
    let wait_status = |want: &'static [&'static str]| {
        let get = &get;
        async move {
            let start = Instant::now();
            loop {
                let r: Value = get(&format!("/api/teams/{run_id}")).await.unwrap().json().await.unwrap();
                if want.contains(&r["status"].as_str().unwrap_or("")) || start.elapsed() > T {
                    return r;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    };
    wait_status(&["review", "attention"]).await;
    post(&format!("/api/teams/{run_id}/approve"), json!({})).await.unwrap();
    let finished = wait_status(&["ready", "attention"]).await;
    c.json("team-run.json", &finished);
    let api_merge = post(&format!("/api/teams/{run_id}/merge"), json!({})).await.unwrap();
    let api_code = api_merge.status().as_u16();
    let api_msg: Value = api_merge.json().await.unwrap_or_default();
    let service_merge = Teams::new(api_hub).merge(run_id);
    let svc_msg = service_merge.as_ref().err().map(|e| format!("{e:#}")).unwrap_or_default();
    c.json("merge-attempts.json", &json!({ "api": { "status": api_code, "body": api_msg }, "service": svc_msg }));
    let tested = finished["verification"]["fingerprint"].as_str().unwrap_or("").to_string();
    let shows = |m: &str| m.contains("merge is blocked") && m.contains("tests") && !tested.is_empty() && m.contains(&tested);
    c.check(
        "mergeGateFailsClosed",
        finished["status"] == "attention" && finished["verification"]["passed"] == false && api_code == 400 && service_merge.is_err(),
        format!("run {}, API {api_code}, service {}", finished["status"], if service_merge.is_err() { "refused" } else { "merged" }),
    );
    c.check(
        "failedCheckAndRevisionShown",
        shows(api_msg["error"].as_str().unwrap_or("")) && shows(&svc_msg),
        format!("API: {api_msg}; service: {svc_msg}"),
    );
    let after: Value = get(&format!("/api/teams/{run_id}")).await.unwrap().json().await.unwrap();
    c.check(
        "nothingMarkedComplete",
        after["status"] != "merged" && git(&fx.path, &["rev-parse", "HEAD"]).unwrap() == rev,
        format!("run {}, base branch still at {}", after["status"], &rev[..12]),
    );
    let _ = stop_tx.send(());
    let _ = serving.await;
    c.finish();
}
