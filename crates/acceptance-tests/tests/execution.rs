//! System 11: local and remote execution.

use acceptance_tests::{account, fixture, git, repo_root, wait_task, Case, Fermi};
use fermi_core::events::Event;
use fermi_core::hub::TaskStatus;
use serde_json::json;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread")]
async fn lr_01_local_execution() {
    let mut c = Case::start("LR-01", "T1");
    c.real(&["fermi hub (task API)", "engine", "local process", "cargo test on the fixture"]);
    c.mocked(&["model output (fake-cli applies the scripted edit)"]);
    let expected = std::fs::read_to_string(repo_root().join("tests/fixtures/F-CODE/expected/engine-lib-greet.rs")).unwrap();
    let step = json!({ "text": "Added greet.", "tools": [{ "name": "Edit", "input": { "file_path": "engine/src/lib.rs" } }], "edits": [{ "path": "engine/src/lib.rs", "content": expected }] });
    let f = Fermi::new(&c, &[account("cl", "claude", vec![step])], "").unwrap();
    let fx = fixture("F-CODE", &c.work.join("repo")).unwrap();
    let hub = f.hub();
    let id = hub.submit(f.task("Add a greet(name) function to the engine crate, with a test.", &fx.path));
    let rec = wait_task(&hub, id, Duration::from_secs(60)).await.unwrap();
    let patch = git(&fx.path, &["diff"]).unwrap();
    let test = std::process::Command::new("cargo")
        .args(["test", "--quiet", "--offline"])
        .current_dir(&fx.path)
        .env("CARGO_TARGET_DIR", c.work.join("target"))
        .output()
        .unwrap();
    c.text("task.patch", &patch);
    c.text("fixture-tests.txt", &format!("{}{}", String::from_utf8_lossy(&test.stdout), String::from_utf8_lossy(&test.stderr)));
    c.json("task-trace.json", &serde_json::to_value(&rec).unwrap());
    c.check("terminalState", rec.status == TaskStatus::Done, format!("{:?}", rec.status));
    c.check(
        "expectedPatch",
        std::fs::read_to_string(fx.path.join("engine/src/lib.rs")).unwrap() == expected
            && git(&fx.path, &["diff", "--name-only"]).unwrap() == "engine/src/lib.rs",
        "only engine/src/lib.rs changed, as expected",
    );
    c.check("fixtureTestsPass", test.status.success(), "cargo test on the fixture");
    let attributable = rec.events.iter().any(|e| matches!(e, Event::Routed { account, .. } if account == "cl"))
        && rec.events.iter().any(|e| matches!(e, Event::Tool { detail, .. } if detail.contains("engine/src/lib.rs")));
    c.check("attributable", attributable, "the trace names the account and the edited file");
    c.finish();
}
