//! System 2: multi-agent orchestration. Real hub and team runs, real git
//! worktrees and processes; model output comes from the fake CLI.

use acceptance_tests::{account, alive, fixture, git, kill, tree_hash, wait_file, wait_task, Case, Fermi};
use fermi_core::hub::TaskStatus;
use fermi_core::team::{NewRun, RunStatus, TaskState, TeamRun, Teams};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

const T: Duration = Duration::from_secs(90);

/// The architect's reply: a plan in a JSON block.
fn planner(tasks: Value) -> Value {
    let plan = json!({ "summary": "Plan for the acceptance case.", "tasks": tasks });
    json!({ "match": "You are the architect", "text": format!("Here is the plan.\n```json\n{plan}\n```") })
}

fn new_run(repo: &Path, parallel: usize) -> NewRun {
    serde_json::from_value(json!({
        "goal": "Acceptance case goal", "repo": repo, "profile": "p", "parallel": parallel,
        "checks": { "tests": false, "security": false },
    }))
    .unwrap()
}

async fn wait_run(teams: &Teams, id: u64, done: &[RunStatus]) -> TeamRun {
    let start = Instant::now();
    loop {
        let r = teams.get(id).unwrap();
        if done.contains(&r.status) {
            return r;
        }
        assert!(start.elapsed() < T, "run stuck in {:?}: {:?}", r.status, r.log.last());
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Plans (with the fake architect), approves and waits for a terminal state.
async fn team(f: &Fermi, repo: &Path, parallel: usize) -> (Arc<Teams>, u64) {
    let hub = f.hub();
    let teams = Teams::new(hub);
    let id = teams.create(new_run(repo, parallel)).unwrap();
    wait_run(&teams, id, &[RunStatus::Review, RunStatus::Attention]).await;
    teams.approve(id, None).unwrap();
    (teams, id)
}

fn ms(f: &Fermi, file: &str) -> u128 {
    std::fs::read_to_string(f.account_dir("cl").join(file)).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0)
}

#[tokio::test(flavor = "multi_thread")]
async fn ma_01_concurrent_workers() {
    let mut c = Case::start("MA-01", "T1");
    c.real(&["fermi hub", "engine", "OS processes"]);
    c.mocked(&["model output (fake-cli)"]);
    let step = |n: &str| json!({ "match": format!("[[{n}]]"), "touch": format!("{n}.start"), "wait_for": "release", "done_touch": format!("{n}.done"), "text": format!("result {n}") });
    let f = Fermi::new(&c, &[account("cl", "claude", vec![step("A"), step("B"), step("C")])], "").unwrap();
    let hub = f.hub();
    let mut ids = vec![];
    for n in ["A", "B", "C"] {
        let ws = c.work.join(format!("ws-{n}"));
        std::fs::create_dir_all(&ws).unwrap();
        ids.push(hub.submit(f.task(&format!("Job [[{n}]]"), &ws)));
    }
    // Each worker holds at its checkpoint until all three have started.
    let all_started = async {
        for n in ["A", "B", "C"] {
            wait_file(&f.account_dir("cl").join(format!("{n}.start")), Duration::from_secs(30)).await?;
        }
        anyhow::Ok(())
    }
    .await;
    c.check("allThreeStartedBeforeAnyFinished", all_started.is_ok(), "three workers reached their checkpoint together");
    f.release("cl", "release");
    let mut outs = vec![];
    for id in &ids {
        outs.push(wait_task(&hub, *id, T).await.unwrap());
    }
    let calls = f.calls("cl");
    c.json(
        "worker-timeline.json",
        &json!({ "calls": calls, "done_ms": { "A": ms(&f, "A.done"), "B": ms(&f, "B.done"), "C": ms(&f, "C.done") } }),
    );
    let starts: Vec<u128> = calls.iter().map(|x| x["started_ms"].as_u64().unwrap() as u128).collect();
    let first_done = ["A", "B", "C"].iter().map(|n| ms(&f, &format!("{n}.done"))).min().unwrap();
    c.check("intervalsOverlap", starts.iter().all(|s| *s <= first_done), format!("starts {starts:?}, first finish {first_done}"));
    for (n, rec) in ["A", "B", "C"].iter().zip(&outs) {
        let text = rec.events.iter().rev().find_map(|e| if let fermi_core::Event::Text { text } = e { Some(text.clone()) } else { None });
        c.check(
            &format!("ownResult:{n}"),
            rec.status == TaskStatus::Done && text.as_deref() == Some(&format!("result {n}")),
            format!("{:?} {text:?}", rec.status),
        );
    }
    let cwds: Vec<String> = calls.iter().map(|x| x["cwd"].as_str().unwrap_or("").to_string()).collect();
    c.check("separateWorkspaces", cwds.iter().collect::<std::collections::BTreeSet<_>>().len() == 3, format!("{cwds:?}"));
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ma_02_dependency_ordering() {
    let mut c = Case::start("MA-02", "T1");
    c.real(&["fermi team runs (scheduler)", "hub", "git worktrees"]);
    c.mocked(&["model output (fake-cli)"]);
    let plan = || {
        planner(json!([
            { "id": "a", "role": "developer", "title": "Step A", "description": "Do [[A]]." },
            { "id": "b", "role": "developer", "title": "Step B", "description": "Do [[B]].", "depends_on": ["a"] },
            { "id": "c", "role": "developer", "title": "Step C", "description": "Do [[C]].", "depends_on": ["b"] }
        ]))
    };
    let work = |n: &str| json!({ "match": format!("[[{n}]]"), "touch": format!("{n}.start"), "done_touch": format!("{n}.done"), "text": format!("{n} finished") });

    // 1. A, then B, then C.
    let f = Fermi::at(&c, "ok", &[account("cl", "claude", vec![plan(), work("A"), work("B"), work("C")])], "").unwrap();
    let fx = fixture("F-CODE", &c.work.join("repo-ok")).unwrap();
    let (teams, id) = team(&f, &fx.path, 3).await;
    let run = wait_run(&teams, id, &[RunStatus::Ready, RunStatus::Attention]).await;
    let calls = f.calls("cl");
    let start_of = |n: &str| {
        calls
            .iter()
            .find(|x| x["prompt"].as_str().unwrap_or("").contains(&format!("[[{n}]]")))
            .and_then(|x| x["started_ms"].as_u64())
            .unwrap_or(0) as u128
    };
    c.json("dag-success.json", &json!({ "run": run, "calls": calls }));
    c.check(
        "bAfterA",
        start_of("B") >= ms(&f, "A.done") && ms(&f, "A.done") > 0,
        format!("B started {} after A finished {}", start_of("B"), ms(&f, "A.done")),
    );
    c.check(
        "cAfterB",
        start_of("C") >= ms(&f, "B.done") && ms(&f, "B.done") > 0,
        format!("C started {} after B finished {}", start_of("C"), ms(&f, "B.done")),
    );
    c.check("allDone", run.status == RunStatus::Ready, format!("{:?}", run.status));

    // 2. A fails: B and C must never run.
    let fail = json!({ "match": "[[A]]", "error": "the build failed" });
    let f2 = Fermi::at(&c, "fail", &[account("cl", "claude", vec![plan(), fail, work("B"), work("C")])], "").unwrap();
    let fx2 = fixture("F-CODE", &c.work.join("repo-fail")).unwrap();
    let (teams2, id2) = team(&f2, &fx2.path, 3).await;
    let run2 = wait_run(&teams2, id2, &[RunStatus::Attention, RunStatus::Ready]).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let calls2 = f2.calls("cl");
    let ran = |n: &str| calls2.iter().any(|x| x["prompt"].as_str().unwrap_or("").contains(&format!("[[{n}]]")));
    c.json("dag-failure.json", &json!({ "run": teams2.get(id2), "calls": calls2 }));
    let st = |r: &TeamRun, k: &str| r.tasks.iter().find(|t| t.key == k).map(|t| t.state);
    c.check("failureBlocksDescendants", ran("A") && !ran("B") && !ran("C"), "A ran; B and C never started");
    c.check(
        "failureVisible",
        run2.status == RunStatus::Attention && st(&run2, "a") == Some(TaskState::Failed),
        format!("{:?} a={:?}", run2.status, st(&run2, "a")),
    );

    // 3. Cancel while B waits on A.
    let slow = json!({ "match": "[[A]]", "touch": "A.start", "wait_for": "A.go", "text": "A finished late" });
    let f3 = Fermi::at(&c, "cancel", &[account("cl", "claude", vec![plan(), slow, work("B"), work("C")])], "").unwrap();
    let fx3 = fixture("F-CODE", &c.work.join("repo-cancel")).unwrap();
    let (teams3, id3) = team(&f3, &fx3.path, 3).await;
    wait_file(&f3.account_dir("cl").join("A.start"), T).await.unwrap();
    teams3.cancel(id3).unwrap();
    f3.release("cl", "A.go");
    tokio::time::sleep(Duration::from_secs(2)).await;
    let calls3 = f3.calls("cl");
    let run3 = teams3.get(id3).unwrap();
    c.json("dag-cancel.json", &json!({ "run": run3, "calls": calls3 }));
    let ran3 = |n: &str| calls3.iter().any(|x| x["prompt"].as_str().unwrap_or("").contains(&format!("[[{n}]]")));
    c.check("cancelBlocksDescendants", !ran3("B") && !ran3("C") && run3.status == RunStatus::Cancelled, format!("{:?}", run3.status));
    c.finish();
}

/// Two developers in parallel; returns the run, teams and fixture.
async fn two_devs(
    c: &Case,
    name: &str,
    a: Value,
    b: Value,
    debugger: Option<Value>,
) -> (Fermi, Arc<Teams>, u64, acceptance_tests::Fixture) {
    let plan = planner(json!([
        { "id": "one", "role": "developer", "title": "Change one", "description": "Do [[ONE]]." },
        { "id": "two", "role": "developer", "title": "Change two", "description": "Do [[TWO]]." }
    ]));
    let mut steps = vec![plan, a, b];
    steps.extend(debugger);
    let f = Fermi::at(c, name, &[account("cl", "claude", steps)], "").unwrap();
    let fx = fixture("F-CODE", &c.work.join(format!("repo-{name}"))).unwrap();
    let (teams, id) = team(&f, &fx.path, 2).await;
    (f, teams, id, fx)
}

#[tokio::test(flavor = "multi_thread")]
async fn ma_03_independent_edits() {
    let mut c = Case::start("MA-03", "T1");
    c.real(&["fermi team runs", "git worktrees, merges and the final merge"]);
    c.mocked(&["model output (fake-cli applies scripted edits)"]);
    let num = "/// The number helper.\npub fn helper() -> u32 {\n    20\n}\n";
    let main =
        "use engine::describe;\nuse util::num::helper;\n\nfn main() {\n    println!(\"{} {}\", describe(\" Fermi \"), helper());\n}\n";
    let a = json!({ "match": "[[ONE]]", "edits": [{ "path": "util/src/num.rs", "content": num }], "text": "one done" });
    let b = json!({ "match": "[[TWO]]", "edits": [{ "path": "app/src/main.rs", "content": main }], "text": "two done" });
    let (_f, teams, id, fx) = two_devs(&c, "ind", a, b, None).await;
    let run = wait_run(&teams, id, &[RunStatus::Ready, RunStatus::Attention]).await;
    // The user's own uncommitted file must survive.
    std::fs::write(fx.path.join("USER_SENTINEL.txt"), "mine").unwrap();
    let merged = teams.merge(id);
    // Expected: the base plus exactly the two patches.
    let expect = c.work.join("expected");
    let fx_e = fixture("F-CODE", &expect).unwrap();
    std::fs::write(fx_e.path.join("util/src/num.rs"), num).unwrap();
    std::fs::write(fx_e.path.join("app/src/main.rs"), main).unwrap();
    std::fs::write(fx_e.path.join("USER_SENTINEL.txt"), "mine").unwrap();
    let (got, want) = (tree_hash(&fx.path), tree_hash(&fx_e.path));
    c.text("base-commit.txt", &fx.commit);
    c.text("integrated.diff", &git(&fx.path, &["diff", &fx.commit, "HEAD"]).unwrap_or_default());
    c.json("run.json", &serde_json::to_value(&run).unwrap());
    c.check(
        "integrated",
        run.status == RunStatus::Ready && merged.is_ok(),
        format!("{:?} merge {:?}", run.status, merged.as_ref().err().map(|e| e.to_string())),
    );
    c.check("bothEditsSurviveExactly", got == want, format!("tree {got} vs expected {want}"));
    c.check(
        "sentinelIntact",
        std::fs::read_to_string(fx.path.join("USER_SENTINEL.txt")).ok().as_deref() == Some("mine"),
        "user's file unchanged",
    );
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ma_04_conflicting_edits() {
    let mut c = Case::start("MA-04", "T1");
    c.real(&["fermi team runs", "git merge"]);
    c.mocked(&["model output (fake-cli); the debugger is given no resolution"]);
    let base = std::fs::read_to_string(acceptance_tests::repo_root().join("tests/fixtures/F-CODE/repo/engine/src/lib.rs")).unwrap();
    let v1 = base.replace("(normalize(name), helper())", "(normalize(name), helper() + 1)");
    let v2 = base.replace("(normalize(name), helper())", "(normalize(name).to_uppercase(), helper())");
    let a = json!({ "match": "[[ONE]]", "edits": [{ "path": "engine/src/lib.rs", "content": v1 }], "text": "one" });
    let b = json!({ "match": "[[TWO]]", "edits": [{ "path": "engine/src/lib.rs", "content": v2 }], "text": "two" });
    let dbg = json!({ "match": "Resolve merge conflict", "text": "I can't decide which change is right." });
    let (_f, teams, id, fx) = two_devs(&c, "conf", a, b, Some(dbg)).await;
    let run = wait_run(&teams, id, &[RunStatus::Attention, RunStatus::Ready]).await;
    let branch = |k: &str| run.tasks.iter().find(|t| t.key == k).and_then(|t| t.branch.clone()).unwrap_or_default();
    let show = |b: &str| git(&fx.path, &["show", &format!("{b}:engine/src/lib.rs")]).unwrap_or_default();
    let (b1, b2) = (branch("one"), branch("two"));
    c.text("diff-one.patch", &git(&fx.path, &["diff", &fx.commit, &b1]).unwrap_or_default());
    c.text("diff-two.patch", &git(&fx.path, &["diff", &fx.commit, &b2]).unwrap_or_default());
    c.json("run.json", &serde_json::to_value(&run).unwrap());
    let log = run.log.iter().map(|l| l.1.clone()).collect::<Vec<_>>().join("\n");
    c.check("conflictDetected", log.contains("conflicts in") && log.contains("engine/src/lib.rs"), "the log names the conflicting file");
    c.check("escalated", run.status == RunStatus::Attention && run.error.is_some(), format!("{:?}: {:?}", run.status, run.error));
    c.check(
        "bothPatchesKept",
        show(&b1).contains("helper() + 1") && show(&b2).contains("to_uppercase()"),
        "each branch still has its own change",
    );
    let integ = run.integration.as_ref().map(|i| i.worktree.clone()).unwrap();
    let unmerged = git(&integ, &["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default();
    c.check(
        "noSilentOverwrite",
        unmerged.contains("engine/src/lib.rs"),
        format!("still unresolved in the integration worktree: {unmerged:?}"),
    );
    c.check("userBranchUntouched", git(&fx.path, &["rev-parse", "HEAD"]).unwrap() == fx.commit, "main is still at the base commit");
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ma_06_worker_crash_isolation() {
    let mut c = Case::start("MA-06", "T1");
    c.real(&["fermi hub", "OS processes (one killed)"]);
    c.mocked(&["model output (fake-cli)"]);
    let step = |n: &str| json!({ "match": format!("[[{n}]]"), "touch": format!("{n}.pid"), "wait_for": format!("{n}.go"), "text": format!("result {n}") });
    let f = Fermi::new(&c, &[account("cl", "claude", vec![step("A"), step("B"), step("C")])], "").unwrap();
    let hub = f.hub();
    let ids: Vec<u64> = ["A", "B", "C"].iter().map(|n| hub.submit(f.task(&format!("Job [[{n}]]"), &c.work))).collect();
    for n in ["A", "B", "C"] {
        wait_file(&f.account_dir("cl").join(format!("{n}.pid")), T).await.unwrap();
    }
    let pid: u32 = std::fs::read_to_string(f.account_dir("cl").join("B.pid")).unwrap().trim().parse().unwrap();
    kill(pid);
    f.release("cl", "A.go");
    f.release("cl", "C.go");
    let recs: Vec<_> = futures_wait(&hub, &ids).await;
    c.json("results.json", &serde_json::to_value(&recs).unwrap());
    c.check("killedTaskFailed", recs[1].status == TaskStatus::Failed, format!("{:?}", recs[1].status));
    for (i, n) in [(0, "A"), (2, "C")] {
        let text =
            recs[i].events.iter().rev().find_map(|e| if let fermi_core::Event::Text { text } = e { Some(text.clone()) } else { None });
        c.check(
            &format!("survivor:{n}"),
            recs[i].status == TaskStatus::Done && text.as_deref() == Some(&format!("result {n}")),
            format!("{:?}", recs[i].status),
        );
    }
    c.check("killedProcessGone", !alive(pid), format!("pid {pid}"));
    c.finish();
}

async fn futures_wait(hub: &fermi_core::hub::Hub, ids: &[u64]) -> Vec<fermi_core::hub::TaskRec> {
    let mut out = vec![];
    for id in ids {
        out.push(wait_task(hub, *id, T).await.unwrap());
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn ma_07_parent_cancellation() {
    let mut c = Case::start("MA-07", "T1");
    c.real(&["fermi team runs", "hub cancellation", "OS processes and a grandchild process"]);
    c.mocked(&["model output (fake-cli)"]);
    let plan = planner(json!([
        { "id": "a", "role": "developer", "title": "Active", "description": "Do [[A]]." },
        { "id": "b", "role": "developer", "title": "Queued", "description": "Do [[B]].", "depends_on": ["a"] }
    ]));
    let active = json!({ "match": "[[A]]", "touch": "A.pid", "child": "A.child", "wait_for": "never", "text": "unreachable" });
    let queued = json!({ "match": "[[B]]", "text": "B ran" });
    let f = Fermi::new(&c, &[account("cl", "claude", vec![plan, active, queued])], "").unwrap();
    let fx = fixture("F-CODE", &c.work.join("repo")).unwrap();
    let (teams, id) = team(&f, &fx.path, 2).await;
    let dir = f.account_dir("cl");
    wait_file(&dir.join("A.child"), T).await.unwrap();
    let pid: u32 = std::fs::read_to_string(dir.join("A.pid")).unwrap().trim().parse().unwrap();
    let child: u32 = std::fs::read_to_string(dir.join("A.child")).unwrap().trim().parse().unwrap();
    let before = json!({ "cli": alive(pid), "grandchild": alive(child) });
    teams.cancel(id).unwrap();
    // Declared cancellation deadline: 5 seconds.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && (alive(pid) || alive(child)) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let after = json!({ "cli": alive(pid), "grandchild": alive(child) });
    let run = teams.get(id).unwrap();
    c.json("process-inventory.json", &json!({ "pids": { "cli": pid, "grandchild": child }, "before": before, "after": after }));
    c.json("run.json", &serde_json::to_value(&run).unwrap());
    c.check(
        "queuedNeverStarts",
        !f.calls("cl").iter().any(|x| x["prompt"].as_str().unwrap_or("").contains("[[B]]")),
        "B was never started",
    );
    c.check("activeStopped", !alive(pid), format!("agent CLI pid {pid} alive: {}", alive(pid)));
    c.check("noOrphan", !alive(child), format!("grandchild pid {child} alive: {}", alive(child)));
    let a = run.tasks.iter().find(|t| t.key == "a").map(|t| t.state);
    c.check(
        "noFalseCompletion",
        run.status == RunStatus::Cancelled && a == Some(TaskState::Interrupted),
        format!("{:?}, a = {a:?}", run.status),
    );
    if alive(child) {
        kill(child); // clean up after a failure
    }
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ma_08_verified_integration() {
    let mut c = Case::start("MA-08", "T1");
    c.real(&["fermi team runs", "verification gate (cargo test)", "git worktrees and merges"]);
    c.mocked(&["model output and edits (fake-cli)"]);
    let fx = fixture("F-CODE", &c.work.join("repo")).unwrap();
    let base = git(&fx.path, &["rev-parse", "HEAD"]).unwrap();
    let src = |rel: &str| std::fs::read_to_string(fx.path.join(rel)).unwrap();
    let shout = format!(
        "{}\n/// Shouts a name.\npub fn shout(name: &str) -> String {{\n    normalize(name).to_uppercase()\n}}\n",
        src("engine/src/lib.rs")
    );
    let util_ok = src("util/src/lib.rs");
    let util_bad = util_ok.replace("s.trim().to_lowercase()", "s.to_lowercase()");
    let steps = vec![
        planner(json!([
            { "id": "a", "role": "developer", "title": "Shout", "description": "Do [[A]].", "files": ["engine/src/lib.rs"] },
            { "id": "b", "role": "developer", "title": "Simplify normalize", "description": "Do [[B]].", "files": ["util/src/lib.rs"] }
        ])),
        json!({ "match": "[[A]]", "text": "Added shout.", "edits": [{ "path": "engine/src/lib.rs", "content": shout }] }),
        // Worker b breaks a mandatory test.
        json!({ "match": "[[B]]", "text": "Simplified normalize.", "edits": [{ "path": "util/src/lib.rs", "content": util_bad }] }),
        // The debugger's correction waits until the blocked merge has been tried.
        json!({ "match": "Fix failing checks", "wait_for": "release-fix", "text": "Restored the trim.", "edits": [{ "path": "util/src/lib.rs", "content": util_ok }] }),
    ];
    let f = Fermi::new(&c, &[account("cl", "claude", steps)], "").unwrap();
    let (teams, id) = team(&f, &fx.path, 2).await;
    // 1. The checks fail on the integrated branch: integration is blocked.
    let start = Instant::now();
    let failed = loop {
        let r = teams.get(id).unwrap();
        if r.verification.as_ref().is_some_and(|v| !v.passed) || matches!(r.status, RunStatus::Ready | RunStatus::Attention) {
            break r;
        }
        assert!(start.elapsed() < Duration::from_secs(300), "no verification: {:?}", r.log.last());
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let early = teams.merge(id);
    let early_msg = early.as_ref().err().map(|e| format!("{e:#}")).unwrap_or_default();
    c.json("gate-1-failed.json", &serde_json::to_value(&failed).unwrap());
    c.check(
        "failedChecksBlockIntegration",
        failed.verification.as_ref().is_some_and(|v| !v.passed)
            && early_msg.contains("merge is blocked")
            && early_msg.contains("tests")
            && git(&fx.path, &["rev-parse", "HEAD"]).unwrap() == base,
        format!("merge attempt: {early_msg}"),
    );
    // 2. The correction, then the checks pass and the merge goes through.
    f.release("cl", "release-fix");
    let ready = wait_run(&teams, id, &[RunStatus::Ready, RunStatus::Attention]).await;
    c.json("gate-2-passed.json", &serde_json::to_value(&ready).unwrap());
    let integ = ready.integration.clone().unwrap();
    let integ_head = git(&fx.path, &["rev-parse", &integ.branch]).unwrap();
    let v = ready.verification.clone().unwrap();
    let diff_a = teams.diff(id, Some("a"), Some("engine/src/lib.rs")).unwrap_or_default();
    let diff_b = teams.diff(id, Some("b"), Some("util/src/lib.rs")).unwrap_or_default();
    c.text("patch-a.diff", &diff_a);
    c.text("patch-b.diff", &diff_b);
    let merged = teams.merge(id);
    let parent = git(&fx.path, &["rev-parse", "HEAD^2"]).unwrap_or_default();
    c.text("integrated.diff", &git(&fx.path, &["diff", &base, "HEAD"]).unwrap_or_default());
    c.check(
        "passingResultAppliesToExactRevision",
        ready.status == RunStatus::Ready && v.passed && v.fingerprint == integ_head[..12] && merged.is_ok() && parent == integ_head,
        format!(
            "checks passed at {} (integration head {}), merged parent {}",
            v.fingerprint,
            &integ_head[..12],
            &parent[..parent.len().min(12)]
        ),
    );
    c.check(
        "bothPatchesReviewable",
        diff_a.contains("pub fn shout") && diff_b.contains("s.to_lowercase()"),
        "each worker's patch is available on its own",
    );
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ma_05_scoped_delegation() {
    use fermi_core::memory::{Audience, Memory, NewRecord, Provenance};
    let mut c = Case::start("MA-05", "T1");
    c.real(&[
        "fermi engine",
        "fermi's MCP bridge in its own process (delegate tool)",
        "claude CLI adapter and its permission mode",
        "project memory (audience filter)",
        "deny_read policy",
    ]);
    c.mocked(&["Claude Code CLI for the parent and child agents (fake-cli, scripted)"]);
    const PRIVATE: &str = "PRIVATE-MEMORY-SENTINEL-9c1";
    const DENIED: &str = "DENY-READ-SENTINEL-4e7";
    const PARENT_ONLY: &str = "PARENT-ONLY-SENTINEL-41";
    const SPEC: &str = "SPEC-PUBLIC-CONTENT-77";
    // The project: a spec the child may read, a file the policy keeps from agents, one outside it.
    let proj = c.work.join("proj");
    std::fs::create_dir_all(proj.join("notes")).unwrap();
    git(&proj, &["init", "-q"]).unwrap();
    std::fs::write(proj.join("notes/spec.md"), format!("# Names\n\nNames go through normalize() in util. {SPEC}\n")).unwrap();
    std::fs::write(proj.join("secret.env"), format!("TOKEN={DENIED}\n")).unwrap();
    std::fs::write(c.work.join("outside.txt"), "outside the project\n").unwrap();

    let delegate = |prompt: &str, files: &[&str], permission: &str| json!({ "mcp": { "name": "delegate", "arguments": { "prompt": prompt, "files": files, "permission": permission } } });
    let review = "Review notes/spec.md and say which function normalizes names.";
    let summary = "Summarize notes/spec.md in one line.";
    let steps = vec![
        // The parent: two delegations that must be refused, then two that run.
        json!({ "match": "Coordinate the names work", "text": "Parent done.", "ops": [
            delegate(review, &["notes/spec.md", "secret.env"], "plan"),
            delegate(review, &["../outside.txt"], "plan"),
            delegate(review, &["notes/spec.md"], "plan"),
            delegate(summary, &["notes/spec.md"], "bypassPermissions"),
        ] }),
        // Child 1 tries to write, which its plan mode must stop.
        json!({ "match": review, "text": "normalize() in util.", "ops": [
            { "tool": "Write", "input": { "file_path": "child.txt", "content": "from the child" }, "do": { "write": "child.txt", "content": "from the child" } }
        ] }),
        json!({ "match": summary, "text": "Names are normalized by util." }),
    ];
    let f = Fermi::new(&c, &[account("cl", "claude", steps)], "\n[security]\ndeny_read = [\"**/secret.env\"]\n").unwrap();
    let mem = Memory::open(&f.home, &proj).unwrap();
    let public = mem
        .record(
            NewRecord {
                topic: "names".into(),
                title: "Names are normalized by util normalize".into(),
                body: "Use normalize() for every name.".into(),
                ..Default::default()
            },
            "user",
            Provenance::User,
        )
        .unwrap();
    let private = mem
        .record(
            NewRecord {
                topic: "names".into(),
                title: "Names service credentials".into(),
                body: format!("{PRIVATE}: rotate monthly; normalizes names"),
                audience: Audience::Private,
                ..Default::default()
            },
            "user",
            Provenance::User,
        )
        .unwrap();

    let mut parent = f.task(&format!("Coordinate the names work. {PARENT_ONLY} is for you alone."), &proj);
    parent.id = Some(41);
    parent.permission = Some("acceptEdits".into());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let r = f.engine.run(&parent, &tx).await;
    drop(tx);
    let mut events = vec![];
    while let Some(e) = rx.recv().await {
        events.push(e);
    }
    let calls = f.calls("cl");
    let children: Vec<&Value> =
        calls.iter().filter(|x| !x["prompt"].as_str().unwrap_or("").contains("Coordinate the names work")).collect();
    let dir = f.home.join("delegations").join("41");
    let mut envelopes: Vec<Value> = std::fs::read_dir(&dir)
        .map(|d| d.filter_map(|e| std::fs::read(e.ok()?.path()).ok()).filter_map(|b| serde_json::from_slice(&b).ok()).collect())
        .unwrap_or_default();
    envelopes.sort_by_key(|e| e["n"].as_u64());
    let parent_text = r.as_ref().map(|o| o.text.clone()).unwrap_or_default();
    c.json("calls.json", &json!(calls));
    c.json("envelopes.json", &json!(envelopes));
    c.json("parent-events.json", &serde_json::to_value(&events).unwrap());
    c.text("parent-output.txt", &parent_text);

    let prompt_of = |i: usize| children.get(i).and_then(|x| x["prompt"].as_str()).unwrap_or("").to_string();
    let args_of = |i: usize| children.get(i).map(|x| x["args"].to_string()).unwrap_or_default();
    c.check(
        "outOfScopeFilesRefused",
        parent_text.contains("mcp delegate failed: secret.env is off limits")
            && parent_text.contains("mcp delegate failed: ../outside.txt is outside")
            && children.len() == 2
            && envelopes.len() == 2,
        format!("{} child run(s), {} envelope(s); parent saw: {parent_text}", children.len(), envelopes.len()),
    );
    let first = prompt_of(0);
    c.check(
        "boundedContextDelivered",
        first.contains(review) && first.contains(SPEC) && first.contains("Names are normalized by util normalize"),
        "child 1 received its instructions, the file it was given and the agent-visible memory record",
    );
    let everything: String = children.iter().map(|x| x.to_string()).chain(envelopes.iter().map(|e| e.to_string())).collect();
    c.check(
        "privateSentinelAbsent",
        ![PRIVATE, DENIED, PARENT_ONLY].iter().any(|s| everything.contains(s))
            && envelopes.first().is_some_and(|e| e["memory"].as_array().is_some_and(|m| m.contains(&json!(public.record.id))))
            && !envelopes.iter().any(|e| e["memory"].as_array().is_some_and(|m| m.contains(&json!(private.record.id)))),
        "no child input or delegation record holds the private memory record, the deny_read file or the parent's own prompt",
    );
    c.check(
        "narrowerScopeEnforced",
        args_of(0).contains(r#""--permission-mode","plan""#)
            && !proj.join("child.txt").exists()
            && envelopes[0]["permission"]["effective"] == "plan",
        format!("child 1 args {}; child.txt exists: {}", args_of(0), proj.join("child.txt").exists()),
    );
    c.check(
        "neverBroaderThanDelegator",
        envelopes.get(1).is_some_and(|e| {
            e["permission"] == json!({ "delegator": "acceptEdits", "requested": "bypassPermissions", "effective": "acceptEdits" })
        }) && args_of(1).contains(r#""--permission-mode","acceptEdits""#),
        format!("envelope 2 permission {}", envelopes.get(1).map(|e| e["permission"].clone()).unwrap_or_default()),
    );
    c.check(
        "envelopeRecorded",
        envelopes.iter().all(|e| e["parent"] == 41 && e["result"]["ok"] == true)
            && envelopes[0]["files"][0]["path"] == "notes/spec.md"
            && envelopes[0]["files"][0]["sha256"].as_str()
                == Some(acceptance_tests::sha256(&std::fs::read(proj.join("notes/spec.md")).unwrap()).as_str()),
        "delegations/41/<n>.json: instructions, files with hashes, memory ids, permission, result",
    );
    c.check(
        "answersReturned",
        r.is_ok() && parent_text.contains("normalize() in util.") && parent_text.contains("Names are normalized by util."),
        "the parent got both children's answers",
    );
    c.finish();
}
