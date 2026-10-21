use crate::events::{emit, Event, Tx};
use anyhow::{Context, Result};
use std::collections::VecDeque;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdout, Command};

/// A headless CLI child. The prompt goes through stdin (no argv length or
/// quoting limits); stdout is read as JSON lines; stderr is forwarded as
/// `Log` events and its tail kept for error messages.
pub struct Proc {
    child: Child,
    lines: Lines<BufReader<ChildStdout>>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
}

impl Proc {
    pub async fn spawn(
        bin: &Path,
        args: &[String],
        envs: &[(String, String)],
        cwd: &Path,
        stdin_text: &str,
        tx: &Tx,
    ) -> Result<Proc> {
        let mut cmd = Command::new(bin);
        cmd.args(args)
            .envs(envs.iter().map(|(k, v)| (k, v)))
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        let mut child = cmd.spawn().with_context(|| format!("spawning {}", bin.display()))?;

        let mut stdin = child.stdin.take().expect("piped stdin");
        let input = stdin_text.to_string();
        tokio::spawn(async move {
            let _ = stdin.write_all(input.as_bytes()).await;
            let _ = stdin.shutdown().await;
        });

        let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));
        let mut err_lines = BufReader::new(child.stderr.take().expect("piped stderr")).lines();
        let (tail, tx) = (stderr_tail.clone(), tx.clone());
        tokio::spawn(async move {
            while let Ok(Some(line)) = err_lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                {
                    let mut t = tail.lock().unwrap();
                    if t.len() == 20 {
                        t.pop_front();
                    }
                    t.push_back(line.clone());
                }
                emit(&tx, Event::Log { line });
            }
        });

        let lines = BufReader::new(child.stdout.take().expect("piped stdout")).lines();
        Ok(Proc { child, lines, stderr_tail })
    }

    pub async fn next_json(&mut self) -> Result<Option<serde_json::Value>> {
        while let Some(line) = self.lines.next_line().await? {
            if let Ok(v) = serde_json::from_str(&line) {
                return Ok(Some(v));
            }
        }
        Ok(None)
    }

    /// Waits for exit; returns (success, stderr tail).
    pub async fn finish(mut self) -> Result<(bool, String)> {
        let status = self.child.wait().await?;
        // Give the stderr reader a moment to drain.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let tail = self.stderr_tail.lock().unwrap().iter().cloned().collect::<Vec<_>>().join("\n");
        Ok((status.success(), tail))
    }
}
