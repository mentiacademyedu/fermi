//! Runs one task through fermi's engine in a process of its own (the home is
//! `FERMI_HOME`), for tests that control the process's environment. Prints
//! each event as a JSON line, then `{"outcome": …}` or `{"error": …}`.
//!
//! Usage: fermi-run --profile <p> --cwd <dir> <prompt>

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned().unwrap_or_default();
    let prompt = args.last().cloned().unwrap_or_default();
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let code = rt.block_on(async {
        let engine = match fermi_core::Engine::load() {
            Ok(e) => e,
            Err(e) => {
                println!("{}", serde_json::json!({ "error": format!("{e:#}") }));
                return 1;
            }
        };
        let mut task = fermi_core::engine::Task::new(prompt, arg("--cwd"));
        task.profile = Some(arg("--profile"));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let printer = tokio::spawn(async move {
            while let Some(e) = rx.recv().await {
                println!("{}", serde_json::to_string(&e).unwrap_or_default());
            }
        });
        let r = engine.run(&task, &tx).await;
        drop(tx);
        let _ = printer.await;
        match r {
            Ok(o) => {
                let u = o.usage;
                println!(
                    "{}",
                    serde_json::json!({ "outcome": { "text": o.text, "model": o.model, "account": o.account, "input": u.input, "output": u.output } })
                );
                0
            }
            Err(e) => {
                println!("{}", serde_json::json!({ "error": format!("{e:#}") }));
                1
            }
        }
    });
    std::process::exit(code);
}
