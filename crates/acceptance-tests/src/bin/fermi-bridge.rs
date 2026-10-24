//! fermi's MCP bridge (`fermi mcp --profile <name>`) as a program of its own,
//! for tests that need the real bridge in a real process: the fake CLI starts
//! it from the MCP config fermi writes, as Claude Code would.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let profile = args.iter().position(|a| a == "--profile").and_then(|i| args.get(i + 1)).cloned().unwrap_or_default();
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let res = rt.block_on(async { fermi_core::mcp::serve(fermi_core::Engine::load()?, profile).await });
    if let Err(e) = res {
        eprintln!("fermi-bridge: {e:#}");
        std::process::exit(1);
    }
}
