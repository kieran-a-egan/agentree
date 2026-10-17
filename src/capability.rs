//! Backend capability discovery and launch preflight for Claude Code.
//!
//! The launch arguments in `pty` and the matrix shown in the UI are built from the same
//! flags, so a plan that passes `preflight` is the plan that gets launched. Discovery only
//! reports what it can prove without starting a session: each flag is probed by asking the
//! CLI to parse it with no value. Everything else is `Unverified`, and `Unverified` never
//! blocks a launch. Only a proven `Unsupported` flag or an invalid model name does.

use crate::{pty, roles};
use std::{
    ffi::OsString,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

/// A probe still running after this long is killed and treated as inconclusive.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Values accepted by `--advisor`. The CLI takes a model alias here, not a full model name.
pub(crate) const ADVISORS: [&str; 1] = ["opus"];

/// Flags probed in the CLI, as (capability label, flag). Each one takes a value.
/// A value-less probe is safe only for flags that fail with "argument missing"; all of these were checked on Claude Code 2.1.296.
const PROBED: [(&str, &str); 8] = [
    ("Model selection", "--model"),
    ("Advisor", "--advisor"),
    ("Native teammates", "--teammate-mode"),
    ("Session id", "--session-id"),
    ("System prompt file", "--append-system-prompt-file"),
    ("Tool allowlists", "--allowedTools"),
    ("Permission modes", "--permission-mode"),
    ("Effort level", "--effort"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Supported,
    Unsupported,
    Unverified,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Supported => "supported",
            Status::Unsupported => "NOT SUPPORTED",
            Status::Unverified => "unverified",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Row {
    pub capability: String,
    /// The CLI flag this row probes. `None` for rows that cannot be probed without a session.
    pub flag: Option<&'static str>,
    pub status: Status,
    pub note: String,
}

#[derive(Clone, Debug)]
pub struct Discovery {
    /// The CLI that was probed, or `None` when none was found.
    pub cli: Option<String>,
    pub rows: Vec<Row>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// The setting or capability at fault, e.g. "Advisor".
    pub capability: String,
    pub problem: String,
    pub fix: String,
}

/// What Agentree asks Claude Code to run. Mirrors the launch arguments built in `pty`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RolePlan {
    pub lead_model: String,
    /// The model for unnamed subagents, via CLAUDE_CODE_SUBAGENT_MODEL.
    pub subagent_model: String,
    pub advisor: Option<String>,
    pub native_teams: bool,
    /// `--effort` for the lead session. `None` leaves Claude Code's default.
    pub effort: Option<String>,
    /// `--permission-mode` for the lead session. `None` leaves Claude Code's default.
    pub permission_mode: Option<String>,
    /// Named teammates. Their settings reach Claude as instructions, not flags.
    pub teammates: Vec<Teammate>,
}

/// One named-teammate role and the limits Claude is asked to follow for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Teammate {
    pub role: &'static str,
    pub model: String,
    pub max_concurrent: u8,
    pub permission: Option<String>,
}

impl RolePlan {
    /// The configuration Agentree has always launched: Haiku lead and workers, Opus advisor, native teams.
    pub fn claude_default() -> Self {
        roles::Settings::default().plan()
    }
}

/// Probes the CLI in parallel and returns the capability matrix.
pub fn discover(cli: Option<&Path>) -> Discovery {
    let statuses: Vec<Status> = match cli {
        Some(cli) => thread::scope(|s| {
            let probes: Vec<_> = PROBED
                .iter()
                .map(|(_, flag)| s.spawn(move || probe(cli, flag)))
                .collect();
            probes
                .into_iter()
                .map(|p| p.join().unwrap_or(Status::Unverified))
                .collect()
        }),
        None => vec![Status::Unverified; PROBED.len()],
    };
    let mut rows: Vec<Row> = PROBED
        .iter()
        .zip(statuses)
        .map(|((label, flag), status)| Row {
            capability: format!("{label} ({flag})"),
            flag: Some(flag),
            status,
            note: match (status, cli) {
                (Status::Supported, _) => "accepted by the CLI".into(),
                (Status::Unsupported, _) => "the CLI rejects this flag".into(),
                (Status::Unverified, None) => "no Claude Code CLI found".into(),
                (Status::Unverified, Some(_)) => "probe was inconclusive".into(),
            },
        })
        .collect();
    rows.extend(static_rows());
    Discovery {
        cli: cli.map(|p| p.display().to_string()),
        rows,
    }
}

/// Capabilities that only a live session can confirm, plus Agentree's built-in figures.
fn static_rows() -> Vec<Row> {
    let row = |capability: &str, status, note: &str| Row {
        capability: capability.into(),
        flag: None,
        status,
        note: note.into(),
    };
    vec![
        row(
            "Agent teams (CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS)",
            Status::Unverified,
            "set by Agentree at launch; only a live session shows Claude honouring it",
        ),
        row(
            "Models (haiku, sonnet, opus 5.5)",
            Status::Unverified,
            "names known to Agentree; your plan's access is not probed",
        ),
        row(
            "Interactive prompts and permissions",
            Status::Unverified,
            "Claude's permission and question prompts should appear in the embedded terminal",
        ),
        row(
            "Context window",
            Status::Unverified,
            "Agentree's built-in figure of 1,000,000 tokens, used for context %; not probed",
        ),
    ]
}

/// Runs `cli <flag>` with no value. A known flag fails with "argument missing"; an unknown
/// one fails with "unknown option". Neither starts a session.
fn probe(cli: &Path, flag: &str) -> Status {
    let Ok((exe, prefix)) = pty::real_cli_argv(cli) else {
        return Status::Unverified;
    };
    let args: Vec<OsString> = prefix.into_iter().chain([OsString::from(flag)]).collect();
    run_probe(&exe, &args, PROBE_TIMEOUT)
}

/// Runs a probe and classifies its output. A child still running at `timeout` is killed.
fn run_probe(program: &Path, args: &[OsString], timeout: Duration) -> Status {
    let Ok(mut child) = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    else {
        return Status::Unverified;
    };
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Status::Unverified;
            }
        }
    }
    match child.wait_with_output() {
        Ok(out) => classify(&format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )),
        Err(_) => Status::Unverified,
    }
}

/// Reads a clap-style parse error. Anything unrecognised is inconclusive, not a rejection.
pub fn classify(output: &str) -> Status {
    if output.contains("argument missing") {
        Status::Supported
    } else if output.contains("unknown option") {
        Status::Unsupported
    } else {
        Status::Unverified
    }
}

fn label_for(flag: &str) -> &'static str {
    PROBED
        .iter()
        .find(|(_, f)| *f == flag)
        .map(|(label, _)| *label)
        .unwrap_or("Launch flag")
}

/// Diagnostics for every proven incompatibility. An empty list means the plan may launch.
pub fn preflight(plan: &RolePlan, discovery: &Discovery) -> Vec<Diagnostic> {
    // Without a CLI nothing can be probed, so the plan cannot launch.
    let Some(cli) = discovery.cli.as_deref() else {
        return vec![Diagnostic {
            capability: "Claude Code CLI".into(),
            problem: "No Claude Code CLI was found.".into(),
            fix: "Re-run install.ps1 -ClaudeCli 'C:\\path\\to\\claude.exe', or set CLAUDE_TREE_REAL_CLAUDE to its path.".into(),
        }];
    };
    let mut out = Vec::new();
    let status = |flag: &str| {
        discovery
            .rows
            .iter()
            .find(|r| r.flag == Some(flag))
            .map(|r| r.status)
            .unwrap_or(Status::Unverified)
    };

    let mut models = vec![
        ("Lead model", plan.lead_model.as_str()),
        ("Subagent model", plan.subagent_model.as_str()),
    ];
    for teammate in &plan.teammates {
        models.push((teammate.role, teammate.model.as_str()));
        if !(1..=4).contains(&teammate.max_concurrent) {
            out.push(Diagnostic {
                capability: format!("{} concurrency", teammate.role),
                problem: format!(
                    "{} teammates cannot run {} at once.",
                    teammate.role, teammate.max_concurrent
                ),
                fix: "Choose a limit from 1 to 4.".into(),
            });
        }
    }
    for (name, model) in models {
        if !roles::MODELS.contains(&model) {
            out.push(Diagnostic {
                capability: name.into(),
                problem: format!("'{model}' is not a model Agentree can run on Claude Code."),
                fix: format!("Use one of: {}.", roles::MODELS.join(", ")),
            });
        }
    }
    if let Some(advisor) = &plan.advisor {
        if !ADVISORS.contains(&advisor.as_str()) {
            out.push(Diagnostic {
                capability: "Advisor".into(),
                problem: format!(
                    "'{advisor}' cannot advise; only {} can.",
                    ADVISORS.join(", ")
                ),
                fix: "Choose the Opus advisor or run without one.".into(),
            });
        }
    }
    if let Some(effort) = &plan.effort {
        if !roles::EFFORTS[1..].contains(&effort.as_str()) {
            out.push(Diagnostic {
                capability: "Effort level".into(),
                problem: format!("'{effort}' is not an effort level Claude Code accepts."),
                fix: format!("Use one of: {}.", roles::EFFORTS[1..].join(", ")),
            });
        }
    }
    if let Some(mode) = &plan.permission_mode {
        if !roles::PERMISSIONS[1..].contains(&mode.as_str()) {
            out.push(Diagnostic {
                capability: "Permission mode".into(),
                problem: format!("'{mode}' is not a permission mode Agentree offers."),
                fix: format!("Use one of: {}.", roles::PERMISSIONS[1..].join(", ")),
            });
        }
    }

    // Every launch passes these flags, so the CLI must accept them. The advisor, team, effort
    // and permission flags are checked only when the plan asks for them.
    let mut needed = vec![
        ("--model", "the lead model"),
        ("--session-id", "the run id"),
        ("--append-system-prompt-file", "the role instructions"),
    ];
    if plan.advisor.is_some() {
        needed.push(("--advisor", "the Opus advisor"));
    }
    if plan.native_teams {
        needed.push(("--teammate-mode", "native teammates"));
    }
    if plan.effort.is_some() {
        needed.push(("--effort", "the lead effort level"));
    }
    if plan.permission_mode.is_some() {
        needed.push(("--permission-mode", "the lead permission mode"));
    }
    for (flag, purpose) in needed {
        if status(flag) == Status::Unsupported {
            out.push(Diagnostic {
                capability: label_for(flag).into(),
                problem: format!(
                    "{cli} does not accept {flag}, which Agentree needs for {purpose}."
                ),
                fix: "Update Claude Code (`claude update`), then press F5 again.".into(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    const HAIKU: &str = roles::MODELS[0];
    const SONNET: &str = roles::MODELS[1];

    /// A matrix in which every probed flag has the given status.
    fn matrix(status: Status) -> Discovery {
        Discovery {
            cli: Some("claude.exe".into()),
            rows: PROBED
                .iter()
                .map(|(label, flag)| Row {
                    capability: format!("{label} ({flag})"),
                    flag: Some(flag),
                    status,
                    note: String::new(),
                })
                .collect(),
        }
    }

    fn with_unsupported(flag: &str) -> Discovery {
        let mut d = matrix(Status::Supported);
        for row in d.rows.iter_mut().filter(|r| r.flag == Some(flag)) {
            row.status = Status::Unsupported;
        }
        d
    }

    /// v0.5 release gate: a model chosen for a stage is saved, survives a restart, reaches the
    /// launch arguments and instructions, and an unsupported setting is refused before launch.
    #[test]
    fn release_gate_model_change_persists_and_launches() {
        use crate::profiles::{self, Store};
        use crate::roles::{Field, Role};
        let dir = std::env::temp_dir().join(format!("agentree-gate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (file, legacy) = (dir.join("profiles.json"), dir.join("roles.json"));

        let mut store = Store::default();
        store
            .set_profile_value(Role::Lead, Field::Model, "claude-sonnet-5-5")
            .unwrap();
        store
            .set_profile_value(Role::Coder, Field::Model, "claude-sonnet-5-5")
            .unwrap();
        // A value outside the offered list is rejected at the setting, before it can be saved.
        assert!(store
            .set_profile_value(Role::Lead, Field::Model, "gpt-4")
            .is_err());
        store.save(&file).unwrap();

        // Restart: a fresh load resolves the same choices for any project.
        let (reloaded, notices) = profiles::load(&file, &legacy);
        assert!(notices.is_empty(), "{notices:?}");
        let plan = reloaded.effective("any-project").plan();
        assert_eq!(plan.lead_model, "claude-sonnet-5-5");
        assert!(preflight(&plan, &matrix(Status::Supported)).is_empty());

        // The saved choice is what runs: launch flag and the teammate instructions.
        let args: Vec<String> = crate::pty::claude_launch_args(&plan, "run", Path::new("p"), "go")
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(&args[..2], ["--model", "claude-sonnet-5-5"]);
        let policy = crate::prompt::system_policy(Path::new("a.json"), "run", &plan);
        assert!(policy.contains("- Coder: claude-sonnet-5-5"), "{policy}");

        // Unsupported settings fail preflight, so nothing is spawned.
        let mut bad = plan.clone();
        bad.lead_model = "gpt-4".into();
        assert!(!preflight(&bad, &matrix(Status::Supported)).is_empty());
        assert!(!preflight(&plan, &with_unsupported("--model")).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn parse_errors_classify_flags() {
        assert_eq!(
            classify("error: option '--advisor <model>' argument missing"),
            Status::Supported
        );
        assert_eq!(
            classify("error: unknown option '--advisor'"),
            Status::Unsupported
        );
        assert_eq!(classify("Something unexpected"), Status::Unverified);
    }

    #[test]
    fn valid_claude_configuration_passes_preflight() {
        assert!(preflight(&RolePlan::claude_default(), &matrix(Status::Supported)).is_empty());
    }

    #[test]
    fn inconclusive_probes_never_block_launch() {
        assert!(preflight(&RolePlan::claude_default(), &matrix(Status::Unverified)).is_empty());
    }

    #[test]
    fn cli_without_advisor_blocks_only_plans_that_use_one() {
        let old = with_unsupported("--advisor");
        let diagnostics = preflight(&RolePlan::claude_default(), &old);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].capability, "Advisor");
        assert!(diagnostics[0].problem.contains("--advisor"));
        assert!(diagnostics[0].fix.contains("claude update"));

        let no_advisor = RolePlan {
            advisor: None,
            ..RolePlan::claude_default()
        };
        assert!(preflight(&no_advisor, &old).is_empty());
    }

    #[test]
    fn cli_without_teammate_mode_blocks_native_teams_only() {
        let old = with_unsupported("--teammate-mode");
        assert_eq!(preflight(&RolePlan::claude_default(), &old).len(), 1);
        let solo = RolePlan {
            native_teams: false,
            ..RolePlan::claude_default()
        };
        assert!(preflight(&solo, &old).is_empty());
    }

    #[test]
    fn required_launch_flags_always_block_when_rejected() {
        for flag in ["--model", "--session-id", "--append-system-prompt-file"] {
            let d = preflight(&RolePlan::claude_default(), &with_unsupported(flag));
            assert_eq!(d.len(), 1, "{flag} should block");
        }
    }

    #[test]
    fn invalid_models_and_advisors_are_rejected_with_choices() {
        let plan = RolePlan {
            lead_model: "gpt-4o".into(),
            subagent_model: HAIKU.into(),
            advisor: Some(SONNET.into()),
            ..RolePlan::claude_default()
        };
        let d = preflight(&plan, &matrix(Status::Supported));
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].capability, "Lead model");
        assert!(d[0].fix.contains(HAIKU));
        assert_eq!(d[1].capability, "Advisor");
        assert!(d[1].problem.contains(SONNET));
    }

    #[test]
    fn invalid_effort_permission_and_teammate_limits_are_blocked() {
        let mut plan = RolePlan {
            effort: Some("ultra".into()),
            permission_mode: Some("bypassPermissions".into()),
            ..RolePlan::claude_default()
        };
        plan.teammates[0].max_concurrent = 9;
        plan.teammates[1].model = "gpt-4o".into();
        let d = preflight(&plan, &matrix(Status::Supported));
        let capabilities: Vec<&str> = d.iter().map(|x| x.capability.as_str()).collect();
        assert_eq!(
            capabilities,
            [
                "Planner concurrency",
                "Explorer",
                "Effort level",
                "Permission mode"
            ]
        );
    }

    #[test]
    fn effort_and_permission_flags_block_only_when_the_plan_sets_them() {
        let set = RolePlan {
            effort: Some("high".into()),
            permission_mode: Some("plan".into()),
            ..RolePlan::claude_default()
        };
        assert!(preflight(&set, &matrix(Status::Supported)).is_empty());
        let old = with_unsupported("--effort");
        assert_eq!(preflight(&set, &old).len(), 1);
        assert!(preflight(&RolePlan::claude_default(), &old).is_empty());
        let old_modes = with_unsupported("--permission-mode");
        assert_eq!(preflight(&set, &old_modes).len(), 1);
        assert!(preflight(&RolePlan::claude_default(), &old_modes).is_empty());
    }

    #[test]
    fn missing_cli_blocks_launch_with_an_install_hint() {
        let d = discover(None);
        assert!(d.cli.is_none());
        assert!(d
            .rows
            .iter()
            .filter(|r| r.flag.is_some())
            .all(|r| r.status == Status::Unverified));
        let problems = preflight(&RolePlan::claude_default(), &d);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].fix.contains("install.ps1"));
    }

    #[cfg(windows)]
    #[test]
    fn slow_probe_is_killed_and_treated_as_inconclusive() {
        let started = Instant::now();
        let args = [
            OsString::from("/C"),
            OsString::from("ping -n 30 127.0.0.1 >nul"),
        ];
        let status = run_probe(Path::new("cmd.exe"), &args, Duration::from_millis(300));
        assert_eq!(status, Status::Unverified);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn matrix_lists_probed_flags_and_static_facts() {
        let d = discover(None);
        assert_eq!(d.rows.len(), PROBED.len() + 4);
        assert!(d
            .rows
            .iter()
            .any(|r| r.capability.starts_with("Context window")));
    }

    /// Needs an installed Claude Code CLI: `cargo test -- --ignored --nocapture installed_cli`.
    #[test]
    #[ignore]
    fn installed_cli_accepts_default_plan() {
        let cli = pty::cli_path().expect("Claude Code CLI on PATH");
        let d = discover(Some(&cli));
        for row in &d.rows {
            println!(
                "{:<55} {:<14} {}",
                row.capability,
                row.status.label(),
                row.note
            );
        }
        let problems = preflight(&RolePlan::claude_default(), &d);
        assert!(problems.is_empty(), "{problems:?}");
    }
}
