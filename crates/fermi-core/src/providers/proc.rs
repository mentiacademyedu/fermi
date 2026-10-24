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
/// `Log` events and its tail kept for error messages. In a replay it plays
/// a recording instead (see `trace`).
pub struct Proc {
    child: Option<Child>,
    lines: Option<Lines<BufReader<ChildStdout>>>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    /// Where the responses are recorded.
    pub tape: Option<crate::trace::TapeOut>,
    /// Applied to every line of output as it is read.
    pub redact: Option<Arc<crate::redact::Redactor>>,
    replay: Option<(crate::trace::Tape, Tx)>,
}

impl Proc {
    pub async fn spawn(
        bin: &Path,
        args: &[String],
        envs: &[(String, String)],
        cwd: &Path,
        stdin_text: &str,
        tx: &Tx,
        jail: Option<&crate::sandbox::Jail>,
    ) -> Result<Proc> {
        let mut cmd = Command::new(bin);
        // The sandbox first: it clears the environment, then fermi's variables go in.
        if let Some(j) = jail {
            crate::sandbox::apply(&mut cmd, j)?;
        }
        cmd.args(args)
            .envs(envs.iter().map(|(k, v)| (k, v)))
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
                                         // Its own process group, so cancelling can stop what it started too.
        #[cfg(unix)]
        cmd.process_group(0);
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
        Ok(Proc { child: Some(child), lines: Some(lines), stderr_tail, tape: None, redact: None, replay: None })
    }

    /// Plays a recording: nothing is started.
    pub fn replay(tape: crate::trace::Tape, tx: &Tx) -> Proc {
        Proc { child: None, lines: None, stderr_tail: Default::default(), tape: None, redact: None, replay: Some((tape, tx.clone())) }
    }

    pub async fn next_json(&mut self) -> Result<Option<serde_json::Value>> {
        if let Some((tape, tx)) = &mut self.replay {
            while let Some(line) = tape.lines.pop_front() {
                if let Ok(v) = serde_json::from_str(&line) {
                    return Ok(Some(v));
                }
            }
            if tape.end.is_none() {
                emit(tx, Event::Log { line: crate::trace::TAPE_CUT.into() });
            }
            return Ok(None);
        }
        let Some(lines) = self.lines.as_mut() else { return Ok(None) };
        while let Some(line) = lines.next_line().await? {
            let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
            // Secrets go before anything reads the line, so no preview or
            // shortened output can keep part of one.
            let changed = self.redact.as_ref().is_some_and(|r| r.value(&mut v));
            if let Some(t) = &mut self.tape {
                t.line(&if changed { v.to_string() } else { line });
            }
            return Ok(Some(v));
        }
        Ok(None)
    }

    /// Stops the CLI and every process it started (shell commands, servers,
    /// test runs). Killing only the CLI would orphan them.
    fn kill_tree(&mut self) {
        let Some(child) = self.child.as_mut() else { return };
        let Some(pid) = child.id() else { return }; // already reaped
        if !matches!(child.try_wait(), Ok(None)) {
            return; // it exited on its own
        }
        kill_tree(pid);
    }

    /// Waits for exit; returns (success, stderr tail).
    pub async fn finish(mut self) -> Result<(bool, String)> {
        if let Some((tape, _)) = self.replay.take() {
            return Ok(tape.end.unwrap_or_default());
        }
        let Some(child) = self.child.as_mut() else { return Ok((false, String::new())) };
        let status = child.wait().await?;
        // Give the stderr reader a moment to drain.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let tail = self.stderr_tail.lock().unwrap().iter().cloned().collect::<Vec<_>>().join("\n");
        if let Some(t) = &mut self.tape {
            t.end(status.success(), &tail);
        }
        Ok((status.success(), tail))
    }
}

impl Drop for Proc {
    /// A run that is cancelled or aborted drops its Proc while the CLI is
    /// still working: stop the whole tree, not just the CLI.
    fn drop(&mut self) {
        self.kill_tree();
    }
}

/// Stops a process and every process it started.
pub(crate) fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .creation_flags(0x0800_0000)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(unix)]
    {
        // It leads its own process group (see `Proc::spawn`).
        let _ = std::process::Command::new("kill")
            .args(["-KILL", "--", &format!("-{pid}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}
