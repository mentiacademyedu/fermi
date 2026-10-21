//! Integrated terminal: the user's shell in a pseudo-terminal (ConPTY on
//! Windows), bridged to a WebSocket by the server.

use anyhow::{anyhow, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::path::Path;

pub struct Pty {
    pub master: Box<dyn MasterPty + Send>,
    pub child: Box<dyn Child + Send + Sync>,
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
}

/// PowerShell 7 if installed, else Windows PowerShell; `$SHELL` elsewhere.
fn shell() -> CommandBuilder {
    if cfg!(windows) {
        let pwsh = fermi_core::util::resolve_bin("pwsh", None).is_ok();
        let mut cmd = CommandBuilder::new(if pwsh { "pwsh.exe" } else { "powershell.exe" });
        cmd.arg("-NoLogo");
        cmd
    } else {
        let mut cmd = CommandBuilder::new(std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into()));
        cmd.arg("-l");
        cmd.env("TERM", "xterm-256color");
        cmd
    }
}

pub fn size(cols: u16, rows: u16) -> PtySize {
    PtySize { rows: rows.clamp(2, 500), cols: cols.clamp(10, 1000), pixel_width: 0, pixel_height: 0 }
}

pub fn spawn(cwd: &Path, cols: u16, rows: u16) -> Result<Pty> {
    let pair = native_pty_system().openpty(size(cols, rows)).map_err(|e| anyhow!("opening a terminal: {e}"))?;
    let mut cmd = shell();
    cmd.cwd(cwd);
    let child = pair.slave.spawn_command(cmd).map_err(|e| anyhow!("starting the shell: {e}"))?;
    drop(pair.slave); // the shell holds its end; ours would keep the pty open after it exits
    let reader = pair.master.try_clone_reader().map_err(|e| anyhow!("{e}"))?;
    let writer = pair.master.take_writer().map_err(|e| anyhow!("{e}"))?;
    Ok(Pty { master: pair.master, child, reader, writer })
}
