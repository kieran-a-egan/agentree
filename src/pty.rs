use anyhow::{bail, Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::{
    ffi::OsString,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver},
    thread,
};

pub enum PtyEvent {
    Data(Vec<u8>),
    End(String),
}

pub struct Session {
    pub master: Box<dyn MasterPty + Send>,
    pub writer: Box<dyn Write + Send>,
    pub receiver: Receiver<PtyEvent>,
    pub child: Box<dyn portable_pty::Child + Send + Sync>,
    pub backend: String,
    pub handshake_responses: u32,
    query_tail: Vec<u8>,
}

// Keep the terminal handshake parser independent of the PTY and process.
// In CI it can be exercised without a Claude subscription or interactive terminal.
const TERMINAL_QUERIES: [(&[u8], &[u8]); 5] = [
    (b"\x1b[>0q", b"\x1bP>|Agentree 0.3.5\x1b\\"), // XTVERSION
    (b"\x1b[c", b"\x1b[?1;2c"),                    // Primary DA
    (b"\x1b[0c", b"\x1b[?1;2c"),                   // Primary DA variant
    (b"\x1b[6n", b"\x1b[1;1R"),                    // Cursor position
    (b"\x1b[5n", b"\x1b[0n"),                      // Device status
];

fn terminal_query_replies(tail: &mut Vec<u8>, bytes: &[u8]) -> Vec<&'static [u8]> {
    let previous = tail.len();
    let mut combined = std::mem::take(tail);
    combined.extend_from_slice(bytes);
    let mut replies = Vec::new();
    for (query, reply) in TERMINAL_QUERIES {
        for (index, window) in combined.windows(query.len()).enumerate() {
            // Don't answer queries already wholly contained in the previous tail.
            if index + query.len() > previous && window == query {
                replies.push(reply);
            }
        }
    }
    tail.extend_from_slice(&combined[combined.len().saturating_sub(16)..]);
    replies
}

impl Session {
    // ConPTY may intercept DA1 itself; this is a best-effort second path.
    pub fn answer_terminal_queries(&mut self, bytes: &[u8]) {
        for reply in terminal_query_replies(&mut self.query_tail, bytes) {
            if self
                .writer
                .write_all(reply)
                .and_then(|_| self.writer.flush())
                .is_ok()
            {
                self.handshake_responses += 1;
            }
        }
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

// Exactly one native lead CLI is launched; teammates are created by Claude's
// named Agent tool inside that interactive session, not by separate PTYs.
const CLAUDE_TEAM_ENV: [(&str, &str); 2] = [
    ("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS", "1"),
    ("CLAUDE_CODE_SUBAGENT_MODEL", "claude-haiku-5-5"),
];

fn claude_launch_args(session_id: &str, policy: &Path, first_message: &str) -> Vec<OsString> {
    vec![
        "--model".into(),
        "claude-haiku-5-5".into(),
        "--advisor".into(),
        "opus".into(),
        "--teammate-mode".into(),
        "in-process".into(),
        "--session-id".into(),
        session_id.into(),
        "--append-system-prompt-file".into(),
        policy.as_os_str().to_os_string(),
        first_message.into(),
    ]
}

pub fn spawn(
    cli: &Path,
    project: &Path,
    session_id: &str,
    policy: &Path,
    first_message: &str,
    rows: u16,
    cols: u16,
) -> Result<Session> {
    if !cli.exists() {
        bail!(
            "Claude CLI not found: {}. Re-run install.ps1.",
            cli.display()
        );
    }
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows: rows.max(4),
            cols: cols.max(20),
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("Opening ConPTY")?;
    let (exe, prefix) = real_cli_argv(cli)?;
    let backend = if prefix.is_empty() {
        "native"
    } else {
        "npm/node"
    }
    .to_string();
    let mut cmd = CommandBuilder::new(exe.as_os_str());
    cmd.args(prefix);
    cmd.args(claude_launch_args(session_id, policy, first_message));
    for (name, value) in CLAUDE_TEAM_ENV {
        cmd.env(name, value);
    }
    cmd.cwd(project.as_os_str());
    let child = pair
        .slave
        .spawn_command(cmd)
        .context("Launching interactive Claude Code in ConPTY")?;
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
                Ok(n) => {
                    if tx.send(PtyEvent::Data(buf[..n].to_vec())).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    let _ = tx.send(PtyEvent::End(e.to_string()));
                    return;
                }
            }
        }
        let _ = tx.send(PtyEvent::End("Claude terminal stream ended".into()));
    });
    Ok(Session {
        master: pair.master,
        writer,
        receiver,
        child,
        backend,
        handshake_responses: 0,
        query_tail: Vec::new(),
    })
}

pub fn real_cli_argv(cli: &Path) -> Result<(PathBuf, Vec<OsString>)> {
    let ext = cli.extension().and_then(|v| v.to_str()).unwrap_or("");
    if ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat") {
        // Known npm layout: %APPDATA%\npm\claude.cmd -> node_modules\...\cli.js
        // Replace the wrapper with direct node.exe execution. No shell ever
        // interprets the objective, even when it contains &, quotes or pipes.
        let script = cli
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("node_modules")
            .join("@anthropic-ai")
            .join("claude-code")
            .join("cli.js");
        if !script.is_file() {
            bail!("Not an npm Claude wrapper: {}. Set -ClaudeCli to the actual npm claude.cmd or native claude.exe.", cli.display());
        }
        for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
            let node = dir.join(if cfg!(windows) { "node.exe" } else { "node" });
            if node.is_file() {
                return Ok((node, vec![script.into_os_string()]));
            }
        }
        bail!("Found npm Claude but node.exe is not available on PATH");
    }
    Ok((cli.to_path_buf(), Vec::new()))
}

pub fn cli_path() -> Result<PathBuf> {
    if let Some(v) = std::env::var_os("CLAUDE_TREE_REAL_CLAUDE") {
        let p = PathBuf::from(v);
        if p.is_file() {
            return Ok(p);
        }
        bail!(
            "CLAUDE_TREE_REAL_CLAUDE points to missing file: {}",
            p.display()
        );
    }
    // The native agentree.exe launcher has no .cmd shim to set environment.
    // Read the original Claude selection saved by install.ps1 next to the exe.
    if let Ok(exe) = std::env::current_exe() {
        if let Ok(saved) = std::fs::read_to_string(exe.with_file_name("claude-original.txt")) {
            let p = PathBuf::from(saved.trim().trim_start_matches('\u{feff}').trim());
            if p.is_file() {
                return Ok(p);
            }
        }
    }
    let exe = std::env::current_exe().ok();
    for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        for filename in ["claude.exe", "claude.cmd", "claude.bat"] {
            let p = directory.join(filename);
            if !p.is_file() || exe.as_ref() == Some(&p) {
                continue;
            }
            // Never launch Agentree's own legacy claude.cmd override.
            if filename == "claude.cmd" {
                if let Ok(contents) = std::fs::read_to_string(&p) {
                    if contents.contains("CLAUDE_TREE_SHIM_DIR=")
                        && contents.contains("claude-agent-tree.exe")
                    {
                        continue;
                    }
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

    #[test]
    fn native_launcher_preserves_objective_and_team_settings() {
        let policy = Path::new("C:/project with spaces/agentree-policy.txt");
        let objective = "Implement issue #5: & | > \"quoted\"; don't escape shell metacharacters";
        let args = claude_launch_args("run-guid", policy, objective);
        let values: Vec<String> = args
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            values,
            vec![
                "--model",
                "claude-haiku-5-5",
                "--advisor",
                "opus",
                "--teammate-mode",
                "in-process",
                "--session-id",
                "run-guid",
                "--append-system-prompt-file",
                "C:/project with spaces/agentree-policy.txt",
                objective,
            ]
        );
        assert_eq!(
            CLAUDE_TEAM_ENV,
            [
                ("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS", "1"),
                ("CLAUDE_CODE_SUBAGENT_MODEL", "claude-haiku-5-5"),
            ]
        );
        // A direct executable path needs no shell or wrapper.
        let direct = Path::new("C:/tools/claude.exe");
        assert_eq!(
            real_cli_argv(direct).unwrap(),
            (direct.to_path_buf(), Vec::new())
        );
    }

    #[test]
    fn handshake_detects_split_queries_without_replying_twice() {
        let mut tail = Vec::new();
        assert!(terminal_query_replies(&mut tail, b"begin\x1b[>0").is_empty());
        assert_eq!(
            terminal_query_replies(&mut tail, b"q end"),
            vec![b"\x1bP>|Agentree 0.3.5\x1b\\".as_slice()]
        );
        assert!(terminal_query_replies(&mut tail, b"hello").is_empty());
        assert_eq!(
            terminal_query_replies(&mut tail, b"\x1b[6n\x1b[5n"),
            vec![b"\x1b[1;1R".as_slice(), b"\x1b[0n".as_slice()]
        );
        assert!(terminal_query_replies(&mut tail, b"unrelated text").is_empty());
    }

    #[test]
    fn handshake_handles_primary_device_attribute_variants() {
        let mut tail = Vec::new();
        assert_eq!(
            terminal_query_replies(&mut tail, b"\x1b[c\x1b[0c"),
            vec![b"\x1b[?1;2c".as_slice(), b"\x1b[?1;2c".as_slice()]
        );
        assert!(terminal_query_replies(&mut tail, b"").is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn windows_conpty_can_launch_and_kill_benign_child() {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 8,
                cols: 60,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("open Windows ConPTY");
        // An interactive shell intentionally waits for input; this tests the
        // process lifecycle without assuming cmd.exe exits on its own under
        // the non-interactive Windows Actions runner.
        let mut cmd = CommandBuilder::new("cmd.exe");
        cmd.args(["/D", "/Q"]);
        let mut child = pair
            .slave
            .spawn_command(cmd)
            .expect("start child under ConPTY");
        assert!(
            child.process_id().is_some(),
            "ConPTY child has no process ID"
        );
        drop(pair.slave);
        child.kill().expect("terminate ConPTY child");
    }
}
