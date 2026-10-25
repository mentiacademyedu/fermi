//! `fermi worker` as a program of its own, for the execution tests: started
//! with a fermi home (`FERMI_HOME`) whose accounts use the fake CLI. Prints
//! `listening <addr>` once it accepts connections.
//!
//! Usage: fermi-worker --listen <addr> --token <t> --name <n> --dir <jobs dir>

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned().unwrap_or_default();
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let res = rt.block_on(async {
        let engine = fermi_core::Engine::load()?;
        let dir = std::path::PathBuf::from(arg("--dir"));
        fermi::worker_http::serve(engine, &arg("--listen"), &arg("--token"), &arg("--name"), &dir, |addr| {
            println!("listening {addr}");
            use std::io::Write;
            let _ = std::io::stdout().flush();
        })
        .await
    });
    if let Err(e) = res {
        eprintln!("fermi-worker: {e:#}");
        std::process::exit(1);
    }
}
