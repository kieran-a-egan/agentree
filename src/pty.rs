use anyhow::{bail, Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::{ffi::OsString, io::{Read, Write}, path::{Path, PathBuf}, sync::mpsc::{self, Receiver}, thread};

pub enum PtyEvent { Data(Vec<u8>), End(String) }

pub struct Session {
    pub master: Box<dyn MasterPty + Send>,
    pub writer: Box<dyn Write + Send>,
    pub receiver: Receiver<PtyEvent>,
    pub child: Box<dyn portable_pty::Child + Send + Sync>,
    pub backend: String,
    pub handshake_responses: u32,
    query_tail: Vec<u8>,
}

impl Session {
    // Forwarded terminal capability requests need real responses. vt100 renders
    // escape sequences but is not an interactive terminal emulator by itself.
    // ConPTY may intercept DA1 before it reaches this method, so this is only
    // one part of the startup mitigation, not a guaranteed Claude workaround.
    pub fn answer_terminal_queries(&mut self, bytes: &[u8]) {
        let previous = self.query_tail.len();
        let mut combined = std::mem::take(&mut self.query_tail);
        combined.extend_from_slice(bytes);
        let queries: &[(&[u8], &[u8])] = &[
            (b"\x1b[>0q", b"\x1bP>|Agentree 0.3.5\x1b\\"), // XTVERSION
            (b"\x1b[c", b"\x1b[?1;2c"),                 // Primary DA
            (b"\x1b[0c", b"\x1b[?1;2c"),                // Primary DA variant
            (b"\x1b[6n", b"\x1b[1;1R"),                 // Cursor position
            (b"\x1b[5n", b"\x1b[0n"),                   // Device status
        ];
        for (query, reply) in queries {
            for (index, window) in combined.windows(query.len()).enumerate() {
                // A request already wholly in the preceding tail was answered
                // in an earlier chunk and must not be answered again.
                if index + query.len() <= previous || window != *query { continue; }
                if self.writer.write_all(reply).and_then(|_| self.writer.flush()).is_ok() {
                    self.handshake_responses += 1;
                }
            }
        }
        self.query_tail = combined[combined.len().saturating_sub(16)..].to_vec();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // End the process BEFORE ConPTY's handles are closed. In native team
        // mode it is important not to leave Claude and its descendants behind.
        if matches!(self.child.try_wait(), Ok(None)) {
            #[cfg(windows)]
            if let Some(pid) = self.child.process_id() {
                let _ = std::process::Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/T", "/F"])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
            }
            let _ = self.child.kill();
        }
    }
}

pub fn spawn(cli: &Path, project: &Path, session_id: &str, policy: &Path,
             first_message: &str, rows: u16, cols: u16) -> Result<Session> {
    if !cli.exists() { bail!("Claude CLI not found: {}. Re-run install.ps1.", cli.display()); }
    let pty = native_pty_system();
    let pair = pty.openpty(PtySize { rows: rows.max(4), cols: cols.max(20), pixel_width: 0, pixel_height: 0 })
        .context("Opening ConPTY")?;
    let (exe, prefix) = real_cli_argv(cli)?;
    let backend = if prefix.is_empty() { "native" } else { "npm/node" }.to_string();
    let mut cmd = CommandBuilder::new(exe.as_os_str());
    cmd.args(prefix);
    cmd.arg("--model"); cmd.arg("claude-haiku-5-5");
    cmd.arg("--advisor"); cmd.arg("opus");
    cmd.arg("--teammate-mode"); cmd.arg("in-process");
    cmd.arg("--session-id"); cmd.arg(session_id);
    cmd.arg("--append-system-prompt-file"); cmd.arg(policy.as_os_str());
    cmd.arg(first_message);
    cmd.env("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS", "1");
    cmd.env("CLAUDE_CODE_SUBAGENT_MODEL", "claude-haiku-5-5");
    cmd.cwd(project.as_os_str());
    let child = pair.slave.spawn_command(cmd).context("Launching interactive Claude Code in ConPTY")?;
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;

    // Windows ConPTY sends an initial DA1 query and may wait for a response;
    // send the standard VT100 reply early. Some builds consume this internally.
    // Only attempt this on Windows to avoid unsolicited input on other PTYs.
    #[cfg(windows)]
    let _ = writer.write_all(b"\x1b[?1;2c").and_then(|_| writer.flush());

    let (tx, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => if tx.send(PtyEvent::Data(buf[..n].to_vec())).is_err() { return; },
                Err(e) => { let _ = tx.send(PtyEvent::End(e.to_string())); return; }
            }
        }
        let _ = tx.send(PtyEvent::End("Claude terminal stream ended".into()));
    });
    Ok(Session { master: pair.master, writer, receiver, child, backend,
        handshake_responses: 0, query_tail: Vec::new() })
}

pub fn real_cli_argv(cli: &Path) -> Result<(PathBuf, Vec<OsString>)> {
    let ext = cli.extension().and_then(|v| v.to_str()).unwrap_or("");
    if ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat") {
        // Known npm layout: %APPDATA%\npm\claude.cmd -> node_modules\...\cli.js
        // Replace the wrapper with direct node.exe execution. No shell ever
        // interprets the objective, even when it contains &, quotes or pipes.
        let script = cli.parent().unwrap_or_else(|| Path::new("."))
            .join("node_modules").join("@anthropic-ai").join("claude-code").join("cli.js");
        if !script.is_file() {
            bail!("Not an npm Claude wrapper: {}. Set -ClaudeCli to the actual npm claude.cmd or native claude.exe.", cli.display());
        }
        for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
            let node = dir.join(if cfg!(windows) { "node.exe" } else { "node" });
            if node.is_file() { return Ok((node, vec![script.into_os_string()])); }
        }
        bail!("Found npm Claude but node.exe is not available on PATH");
    }
    Ok((cli.to_path_buf(), Vec::new()))
}

pub fn cli_path() -> Result<PathBuf> {
    if let Some(v) = std::env::var_os("CLAUDE_TREE_REAL_CLAUDE") {
        let p = PathBuf::from(v);
        if p.is_file() { return Ok(p); }
        bail!("CLAUDE_TREE_REAL_CLAUDE points to missing file: {}", p.display());
    }
    // The native agentree.exe launcher has no .cmd shim to set environment.
    // Read the original Claude selection saved by install.ps1 next to the exe.
    if let Ok(exe) = std::env::current_exe() {
        if let Ok(saved) = std::fs::read_to_string(exe.with_file_name("claude-original.txt")) {
            let p = PathBuf::from(saved.trim().trim_start_matches('\u{feff}').trim());
            if p.is_file() { return Ok(p); }
        }
    }
    let exe = std::env::current_exe().ok();
    for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        for filename in ["claude.exe", "claude.cmd", "claude.bat"] {
            let p = directory.join(filename);
            if !p.is_file() || exe.as_ref() == Some(&p) { continue; }
            // Never launch Agentree's own legacy claude.cmd override.
            if filename == "claude.cmd" {
                if let Ok(contents) = std::fs::read_to_string(&p) {
                    if contents.contains("CLAUDE_TREE_SHIM_DIR=") && contents.contains("claude-agent-tree.exe") { continue; }
                }
            }
            return Ok(p);
        }
    }
    bail!("Claude Code CLI not found. Re-run install.ps1 -ClaudeCli 'C:\\path\\to\\claude.exe'.")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_cli_fails_without_spawn() {
        let bad = PathBuf::from("/this-file-should-not-exist/claude.cmd");
        assert!(real_cli_argv(&bad).is_err());
    }
}
