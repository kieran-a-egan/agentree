mod capability;
mod github;
mod live;
mod team;
mod telemetry;
// Runtime-neutral contract (v0.5). Adapters translate Claude files into these
// types; the dashboard should not parse Claude formats directly.
#[allow(dead_code)]
// Approval/Run/Stage have no runtime consumer until v0.6+; tests pin the wire format.
mod model;
// `Store::activate` and the skill and MCP details have no launch consumer until the v0.6 Claude
// adapter; the loader and trust rules are covered by tests and the `--plugin` command.
#[allow(dead_code)]
mod plugins;
mod profiles;
mod prompt;
mod pty;
mod roles;
mod scroll;
mod ui;
// Workflow contracts (Issue #9): no launch consumer until the Claude adapter (#11); covered by tests.
#[allow(dead_code)]
mod workflow;

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use portable_pty::PtySize;
use ratatui::{backend::CrosstermBackend, Terminal};
use scroll::{Move, Pane};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::mpsc::{self, TryRecvError},
    thread,
    time::{Duration, Instant, SystemTime},
};
use uuid::Uuid;

#[derive(Parser, Default)]
#[command(
    name = "agentree",
    version,
    about = "Native Claude teams, one Windows terminal"
)]
struct Args {
    #[arg(long)]
    project: Option<PathBuf>,
    #[arg(
        long,
        conflicts_with = "objective",
        requires = "project",
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    issue: Option<u64>,
    #[arg(long)]
    objective: Option<String>,
    #[arg(long)]
    demo: bool,
    /// Inspect an Agent Plugins v1.0 folder without starting a session.
    #[arg(
        long,
        value_name = "DIR",
        conflicts_with_all = ["project", "issue", "objective", "demo"]
    )]
    plugin: Option<PathBuf>,
    /// With --plugin: trust the plugin as it is now, letting runs use its skills and MCP servers.
    #[arg(long, requires = "plugin", conflicts_with = "plugin_revoke")]
    plugin_trust: bool,
    /// With --plugin: withdraw trust from the plugin.
    #[arg(long, requires = "plugin")]
    plugin_revoke: bool,
}
#[derive(Default, Serialize, Deserialize)]
struct Prefs {
    projects: Vec<String>,
    last_objective: String,
}

fn data_home() -> PathBuf {
    // Tests never read or write the user's saved settings, prefs or runs.
    #[cfg(test)]
    {
        static HOME: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        HOME.get_or_init(|| {
            std::env::temp_dir().join(format!("agentree-test-home-{}", Uuid::new_v4()))
        })
        .clone()
    }
    #[cfg(not(test))]
    {
        match std::env::var_os("LOCALAPPDATA") {
            Some(p) => PathBuf::from(p).join("ClaudeAgentTree"),
            None => std::env::temp_dir().join("ClaudeAgentTree"),
        }
    }
}
fn prefs_path() -> PathBuf {
    data_home().join("prefs.json")
}
/// Role settings as saved by versions before profiles. Read once to migrate, never written.
fn legacy_roles_path() -> PathBuf {
    data_home().join("roles.json")
}
fn profiles_path() -> PathBuf {
    data_home().join("profiles.json")
}
/// Which plugin folders the user trusted, and the contents trusted. See `plugins::Store`.
fn plugins_path() -> PathBuf {
    data_home().join("plugins.json")
}
/// Parent of each plugin's persistent `PLUGIN_DATA` folder.
fn plugin_data_path() -> PathBuf {
    data_home().join("plugin-data")
}
fn read_prefs() -> Prefs {
    fs::read_to_string(prefs_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}
fn save_prefs(preferences: &Prefs) {
    let _ = fs::create_dir_all(data_home());
    if let Ok(json) = serde_json::to_vec_pretty(preferences) {
        let _ = fs::write(prefs_path(), json);
    }
}
fn path_input(s: &str) -> PathBuf {
    let s = s.trim().trim_matches('"');
    if s == "~" {
        return std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(s));
    }
    if let Some(rest) = s.strip_prefix("~/").or_else(|| s.strip_prefix("~\\")) {
        if let Some(home) = std::env::var_os("USERPROFILE") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(s)
}
/// Lines of Claude terminal output kept for scrollback. Memory is bounded by this and the width.
const HISTORY_LINES: usize = 2_000;
/// How long the PTY queue must stay quiet after Claude exits before finishing.
const EXIT_DRAIN_GRACE: Duration = Duration::from_millis(300);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Setup,
    Browser,
    Issues,
    Running,
    Finished,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Project,
    Objective,
    Native,
    Response,
    /// Keys scroll and select dashboard panels; nothing is forwarded to Claude.
    Dashboard,
}
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Attention {
    pub title: String,
    pub reason: String,
    pub question: String,
    pub options: Vec<String>,
}

pub struct App {
    pub page: Page,
    pub focus: Focus,
    pub project: String,
    pub objective: String,
    pub message: String,
    pub recent: Vec<String>,
    pub browser_dir: PathBuf,
    pub browser_entries: Vec<PathBuf>,
    pub browser_index: usize,
    pub issues: github::IssueBrowser,
    /// Source of unique ids for GitHub lookups; a result must match the latest id to be applied.
    issue_seq: u64,
    pub team: team::TeamSnapshot,
    pub telemetry: telemetry::Telemetry,
    pub agent_seen: HashMap<String, Instant>,
    pub terminal_expanded: bool,
    pub dashboard_expanded: bool,
    pub session_id: String,
    pub attention: Option<Attention>,
    pub response: String,
    pub terminal: vt100::Parser,
    pub session: Option<pty::Session>,
    pub attention_file: PathBuf,
    pub run_dir: PathBuf,
    pub started: Instant,
    /// Frozen when the lead emits the run-scoped completion marker, or exits.
    pub finished_elapsed: Option<Duration>,
    pub clock_paused_manually: bool,
    pub agent_done_elapsed: HashMap<String, Duration>,
    pub session_started_at: SystemTime,
    pub demo: bool,
    pub output_end: bool,
    pub last_team_poll: Instant,
    pub terminal_size: (u16, u16),
    pub close_confirm: bool,
    /// Whether Claude has rendered any visible text, not just startup escape sequences.
    pub native_screen_visible: bool,
    /// Number of bytes received from Claude via the PTY. Never store raw content in diagnostics.
    pub native_received_bytes: u64,
    pub native_first_data_ms: Option<u128>,
    pub last_diagnostic: Instant,
    pub child_exit: Option<String>,
    child_exited_at: Option<Instant>,
    /// Observations from the live embedded terminal, independent of JSONL flushes.
    pub live: live::LiveSignals,
    /// New response/tool counters provide a bounded recent-activity pulse.
    pub activity: live::ActivityTracker,
    /// Scroll position of every pane. The live terminal keeps its own history in `terminal`.
    pub scrolls: scroll::Scrolls,
    /// Which Issues-page pane Ctrl+scroll moves: the issue list or the preview.
    pub issue_focus: Pane,
    /// The dashboard panel Ctrl+scroll moves while Focus::Dashboard is set (F5 cycles it).
    pub dash_focus: Pane,
    pub help_open: bool,
    /// Capability matrix from the last probe of the installed CLI. `None` until F7 or a launch.
    pub capabilities: Option<capability::Discovery>,
    /// F7 on Setup, and opened automatically when a launch is blocked.
    pub capabilities_open: bool,
    /// Proven problems with the launch plan against `capabilities`. Empty means it may launch.
    pub launch_problems: Vec<capability::Diagnostic>,
    /// The settings a launch uses: the project's profile with its overrides applied. Only changed
    /// on Setup, so a running session always matches them.
    pub role_settings: roles::Settings,
    /// Saved profiles and per-project choices from `profiles.json`.
    pub profiles: profiles::Store,
    /// Problems found while loading or sanitizing `profiles.json`, shown in the Roles overlay.
    pub role_notices: Vec<String>,
    /// Whether the Roles overlay edits this project's overrides (`true`) or the active profile.
    pub roles_project_scope: bool,
    /// The `project` text `role_settings` was resolved for, and the key it resolved under.
    settings_for: String,
    pub settings_key: String,
    /// F4 on Setup. Keys go to the overlay, not Claude, while it is open.
    pub roles_open: bool,
    /// Index into `roles::ROLES` for the overlay's selected row.
    pub roles_row: usize,
    pub roles_field: roles::Field,
    /// Where `profiles` is saved. Tests point this at a temporary file.
    profiles_file: PathBuf,
}
impl App {
    fn new(args: Args) -> Self {
        let p = read_prefs();
        let project = args
            .project
            .map(|v| v.to_string_lossy().to_string())
            .or_else(|| p.projects.first().cloned())
            .unwrap_or_else(|| {
                std::env::current_dir()
                    .unwrap_or_default()
                    .display()
                    .to_string()
            });
        // --issue replaces the saved objective, so a failed load cannot start stale text.
        let objective = match (args.issue, args.objective) {
            (Some(_), _) => String::new(),
            (None, objective) => objective.unwrap_or(p.last_objective),
        };
        let home = data_home();
        let session_id = Uuid::new_v4().to_string();
        let run_dir = home.join("runs").join(&session_id);
        let (profiles, role_notices) = profiles::load(&profiles_path(), &legacy_roles_path());
        let settings_key = profiles::project_key(&path_input(&project));
        let role_settings = profiles.effective(&settings_key);
        Self {
            page: Page::Setup,
            focus: Focus::Project,
            project: project.clone(),
            objective,
            message: role_notices.first().cloned().unwrap_or_default(),
            recent: p.projects,
            browser_dir: PathBuf::new(),
            browser_entries: vec![],
            browser_index: 0,
            issues: Default::default(),
            issue_seq: 0,
            team: Default::default(),
            telemetry: Default::default(),
            agent_seen: HashMap::new(),
            terminal_expanded: false,
            dashboard_expanded: false,
            session_id,
            attention: None,
            response: String::new(),
            terminal: vt100::Parser::new(24, 80, HISTORY_LINES),
            session: None,
            attention_file: run_dir.join("attention.json"),
            run_dir,
            started: Instant::now(),
            finished_elapsed: None,
            clock_paused_manually: false,
            agent_done_elapsed: HashMap::new(),
            session_started_at: SystemTime::now(),
            demo: args.demo,
            output_end: false,
            last_team_poll: Instant::now(),
            terminal_size: (24, 80),
            close_confirm: false,
            native_screen_visible: false,
            native_received_bytes: 0,
            native_first_data_ms: None,
            last_diagnostic: Instant::now(),
            child_exit: None,
            child_exited_at: None,
            live: Default::default(),
            activity: Default::default(),
            scrolls: Default::default(),
            issue_focus: Pane::Issues,
            dash_focus: Pane::Timeline,
            help_open: false,
            capabilities: None,
            capabilities_open: false,
            launch_problems: Vec::new(),
            settings_for: project,
            settings_key,
            role_settings,
            profiles,
            role_notices,
            roles_project_scope: false,
            roles_open: false,
            roles_row: 0,
            roles_field: roles::Field::Model,
            profiles_file: profiles_path(),
        }
    }
    fn refresh_capabilities(&mut self, discovery: capability::Discovery) {
        self.launch_problems = capability::preflight(&self.role_settings.plan(), &discovery);
        self.capabilities = Some(discovery);
    }
    /// The settings the Roles overlay shows: those of the layer it edits. In global scope that is
    /// the active profile even when the project pins another, so an edit is always visible;
    /// `role_settings` stays what launches.
    pub fn roles_view(&self) -> roles::Settings {
        if self.roles_project_scope {
            self.role_settings.clone()
        } else {
            self.profiles.active_settings()
        }
    }
    /// The key `profiles` stores this project's choices under.
    fn project_id(&self) -> String {
        profiles::project_key(&path_input(&self.project))
    }
    /// Re-resolves `role_settings` when the project text has changed. Only on Setup, so a
    /// running session keeps the settings it launched with.
    fn sync_roles(&mut self) {
        if self.page == Page::Setup && self.settings_for != self.project {
            self.resolve_roles();
        }
    }
    fn resolve_roles(&mut self) {
        self.settings_for = self.project.clone();
        self.settings_key = self.project_id();
        self.role_settings = self.profiles.effective(&self.settings_key);
    }
    /// Resolves settings for the current project and writes `profiles.json` after a change. A
    /// failed write is reported, not hidden, and the choice stays in memory.
    fn apply_profiles(&mut self) {
        self.resolve_roles();
        match self.profiles.save(&self.profiles_file) {
            Ok(()) => self.role_notices.clear(),
            Err(error) => self.message = format!("Role settings were not saved: {error}"),
        }
    }
    fn handle_roles(&mut self, key: KeyEvent) {
        let field_index = roles::FIELDS
            .iter()
            .position(|f| *f == self.roles_field)
            .unwrap_or(0);
        let project = self.project_id();
        let scoped = self.roles_project_scope;
        match key.code {
            KeyCode::Esc | KeyCode::F(4) => self.roles_open = false,
            KeyCode::Up => self.roles_row = self.roles_row.saturating_sub(1),
            KeyCode::Down => self.roles_row = (self.roles_row + 1).min(roles::ROLES.len() - 1),
            KeyCode::Tab => {
                self.roles_field = roles::FIELDS[(field_index + 1) % roles::FIELDS.len()]
            }
            KeyCode::BackTab => {
                self.roles_field =
                    roles::FIELDS[(field_index + roles::FIELDS.len() - 1) % roles::FIELDS.len()]
            }
            KeyCode::Char('s' | 'S') => self.roles_project_scope = !scoped,
            KeyCode::Char(c @ ('p' | 'P')) => {
                let step = if c == 'p' { 1 } else { -1 };
                if !scoped {
                    self.profiles.cycle_active(step);
                } else if project.is_empty() {
                    self.message = "Choose a project folder first.".into();
                    return;
                } else {
                    self.profiles.cycle_project_profile(&project, step);
                }
                self.apply_profiles();
            }
            KeyCode::Right | KeyCode::Left => {
                let step = if key.code == KeyCode::Right { 1 } else { -1 };
                let role = roles::ROLES[self.roles_row];
                if roles::options(role, self.roles_field).is_empty() {
                    self.message = format!(
                        "{} has no {} setting. {}",
                        role.label(),
                        self.roles_field.label(),
                        roles::not_applicable_reason(role, self.roles_field)
                    );
                    return;
                }
                // Step from the layer being edited, so the value shown is the value changed.
                let mut next = if scoped {
                    self.profiles.effective(&project)
                } else {
                    self.profiles.active_settings()
                };
                next.cycle(role, self.roles_field, step);
                let value = next.value(role, self.roles_field);
                let saved = if scoped {
                    self.profiles
                        .set_override(&project, role, self.roles_field, &value)
                } else {
                    self.profiles
                        .set_profile_value(role, self.roles_field, &value)
                };
                match saved {
                    Ok(()) => self.apply_profiles(),
                    Err(why) => self.message = why,
                }
            }
            KeyCode::Char('d' | 'D') => {
                let role = roles::ROLES[self.roles_row];
                if scoped {
                    self.profiles.clear_overrides(&project, role);
                } else {
                    self.profiles.reset_profile_role(role);
                }
                self.apply_profiles();
            }
            _ => {}
        }
    }
    /// Blocks the launch with the first problem and opens the matrix when the plan is invalid.
    fn check_launch(&mut self, discovery: capability::Discovery) -> Result<()> {
        self.refresh_capabilities(discovery);
        let Some(first) = self.launch_problems.first() else {
            return Ok(());
        };
        self.capabilities_open = true;
        let extra = match self.launch_problems.len() - 1 {
            0 => String::new(),
            more => format!(" (+{more} more)"),
        };
        anyhow::bail!(
            "Launch blocked: {} {}{extra} F7 shows the capability matrix.",
            first.problem,
            first.fix
        )
    }
    fn browser_open(&mut self) {
        self.browser_dir = if path_input(&self.project).is_dir() {
            path_input(&self.project)
        } else {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        };
        self.load_browser();
        self.page = Page::Browser;
    }
    fn load_browser(&mut self) {
        self.browser_entries = vec![self.browser_dir.join("..")];
        if let Ok(entries) = fs::read_dir(&self.browser_dir) {
            let mut sub: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            sub.sort_by_key(|p| {
                p.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_ascii_lowercase()
            });
            self.browser_entries.extend(sub);
        }
        self.browser_index = 0;
        self.scrolls.get(Pane::Folders).reset();
    }
    fn open_issues(&mut self) {
        if !path_input(&self.project).is_dir() {
            self.message = "Choose an existing project folder before browsing issues.".into();
            return;
        }
        self.issues = Default::default();
        self.issues.status = "Loading open issues from GitHub...".into();
        self.issue_focus = Pane::Issues;
        self.page = Page::Issues;
        self.start_load(github::browse);
    }
    /// The project folder a lookup is keyed to. Matches the path `start_load` hands to `gh` and `git`.
    fn project_key(&self) -> String {
        path_input(&self.project).display().to_string()
    }
    /// Starts a new lookup. Replacing `issues.rx` drops any earlier one, so its result can never arrive.
    fn begin_lookup(&mut self) -> (github::Ticket, mpsc::Sender<github::Lookup>) {
        self.issue_seq += 1;
        let ticket = github::Ticket {
            id: self.issue_seq,
            project: self.project_key(),
        };
        let (tx, rx) = mpsc::channel();
        self.issues.rx = Some(rx);
        self.issues.loading = true;
        self.issues.ticket = Some(ticket.clone());
        (ticket, tx)
    }
    /// Discards the in-flight lookup, if any. Its late result is never applied.
    fn cancel_lookup(&mut self) {
        self.issues.rx = None;
        self.issues.loading = false;
        self.issues.ticket = None;
    }
    /// A lookup for another project must not outlive the edit that changed the project.
    fn reconcile_lookup(&mut self) {
        let stale = self
            .issues
            .ticket
            .as_ref()
            .is_some_and(|t| t.project != self.project_key());
        if stale {
            self.cancel_lookup();
            self.message = "Project changed; the GitHub issue lookup was cancelled.".into();
        }
    }
    /// Runs `gh` and `git` off the UI thread.
    fn start_load<F>(&mut self, load: F)
    where
        F: FnOnce(&Path) -> Result<github::Loaded, github::Error> + Send + 'static,
    {
        let project = path_input(&self.project);
        let (ticket, tx) = self.begin_lookup();
        thread::spawn(move || {
            let _ = tx.send(github::Lookup {
                ticket,
                result: load(&project),
            });
        });
    }
    fn poll_issues(&mut self) {
        self.reconcile_lookup();
        let received = match self.issues.rx.as_ref() {
            Some(rx) => rx.try_recv(),
            None => return,
        };
        let lookup = match received {
            Err(TryRecvError::Empty) => return,
            Ok(lookup) => lookup,
            Err(TryRecvError::Disconnected) => {
                self.cancel_lookup();
                self.issues.status = "The GitHub lookup stopped unexpectedly.".into();
                return;
            }
        };
        // Accept only the current ticket. Project changes were reconciled above, so the project matches.
        if self.issues.ticket.as_ref() != Some(&lookup.ticket) {
            return;
        }
        self.cancel_lookup();
        match lookup.result {
            Ok(github::Loaded::Browse { repo, issues }) => {
                self.issues.status = if issues.is_empty() {
                    format!("No open issues in {}.", repo.slug())
                } else if issues.len() >= github::OPEN_ISSUE_LIMIT {
                    format!(
                        "Showing the first {} open issues in {}.",
                        github::OPEN_ISSUE_LIMIT,
                        repo.slug()
                    )
                } else {
                    format!("{} open issue(s) in {}.", issues.len(), repo.slug())
                };
                self.issues.repo = Some(repo);
                self.issues.issues = issues;
                self.issues.index = 0;
                self.scrolls.get(Pane::Issues).reset();
                self.scrolls.get(Pane::Preview).reset();
            }
            Ok(github::Loaded::Single { repo, issue }) => {
                if self.objective.trim().is_empty() {
                    self.adopt_issue(&repo, &issue);
                } else {
                    self.message = format!(
                        "Issue #{} loaded, but the objective already has text. Press F3 to pick it.",
                        issue.number
                    );
                }
            }
            Err(error) => match self.page {
                Page::Issues => self.issues.status = error.to_string(),
                _ => self.message = error.to_string(),
            },
        }
    }
    /// Startup path for `--issue`: loads in the background, then fills the objective for review.
    fn preselect_issue(&mut self, number: u64) {
        self.message = format!("Loading issue #{number} from GitHub...");
        self.start_load(move |project| github::single(project, number));
    }
    fn adopt_issue(&mut self, repo: &github::RepoRef, issue: &github::GithubIssue) {
        let replaced = !self.objective.trim().is_empty();
        self.objective = github::objective_from_issue(repo, issue);
        self.focus = Focus::Objective;
        // Open at the top so the whole GitHub-generated objective can be reviewed.
        self.scrolls.get(Pane::Objective).reset();
        self.message = format!(
            "Objective from {}#{}{}. Review or edit it, then press F5 to start.",
            repo.slug(),
            issue.number,
            if replaced {
                " (replaced previous text)"
            } else {
                ""
            }
        );
    }
    fn start(&mut self, area: ratatui::layout::Rect) -> Result<()> {
        let project = path_input(&self.project)
            .canonicalize()
            .with_context(|| format!("Project folder not accessible: {}", self.project))?;
        if !project.is_dir() {
            anyhow::bail!("Select a directory, not a file");
        }
        if self.objective.trim().is_empty() {
            anyhow::bail!("Enter an objective before starting");
        }
        let cli = if self.demo {
            None
        } else {
            Some(pty::cli_path()?)
        };
        // The launch uses this project's profile and overrides, resolved for the folder as it is
        // now, so a typed path that differs only in form still finds its saved choices.
        self.settings_key = profiles::project_key(&project);
        self.role_settings = self.profiles.effective(&self.settings_key);
        self.settings_for = self.project.clone();
        // Validate the plan before anything is saved or spawned. Demo mode launches nothing.
        if let Some(cli) = &cli {
            self.check_launch(capability::discover(Some(cli)))?;
        }
        self.project = project.display().to_string();
        let mut pref = read_prefs();
        pref.projects.retain(|x| x != &self.project);
        pref.projects.insert(0, self.project.clone());
        pref.projects.truncate(12);
        pref.last_objective = self.objective.clone();
        save_prefs(&pref);
        self.recent = pref.projects;
        self.session_id = Uuid::new_v4().to_string();
        self.run_dir = data_home().join("runs").join(&self.session_id);
        self.attention_file = self.run_dir.join("attention.json");
        fs::create_dir_all(&self.run_dir)?;
        let plan = self.role_settings.plan();
        let policy_file = self.run_dir.join("instructions.txt");
        fs::write(
            &policy_file,
            prompt::system_policy(&self.attention_file, &self.session_id, &plan),
        )?;
        let initial = prompt::initial_objective(&self.objective, &plan);
        self.terminal_expanded = false;
        self.dashboard_expanded = false;
        let (rows, cols) = ui::native_dimensions(area, self.terminal_expanded);
        self.terminal_size = (rows, cols);
        self.terminal = vt100::Parser::new(rows, cols, HISTORY_LINES);
        self.scrolls = Default::default();
        self.started = Instant::now();
        self.finished_elapsed = None;
        self.clock_paused_manually = false;
        self.agent_done_elapsed.clear();
        self.session_started_at = SystemTime::now();
        if let Some(cli) = cli {
            self.session = Some(pty::spawn(
                &cli,
                &plan,
                &project,
                &self.session_id,
                &policy_file,
                &initial,
                (rows, cols),
            )?);
            self.message = format!(
                "Claude process launched. Waiting for terminal startup | {}",
                self.session_id
            );
        } else {
            self.terminal.process(b"\x1b[32mDEMO - no Claude process launched\x1b[0m\r\nNative teammates and attention are simulated only in --demo mode.\r\n");
            self.message = "SIMULATED dashboard. No model calls, no edits.".into();
        }
        self.page = Page::Running;
        self.focus = Focus::Native;
        // A late --issue or picker result must not overwrite the running session's status.
        self.cancel_lookup();
        self.terminal_expanded = false;
        self.output_end = false;
        self.close_confirm = false;
        self.team = Default::default();
        self.telemetry.reset();
        self.agent_seen.clear();
        self.attention = None;
        self.native_screen_visible = false;
        self.native_received_bytes = 0;
        self.native_first_data_ms = None;
        self.last_diagnostic = Instant::now();
        self.child_exit = None;
        self.child_exited_at = None;
        self.live = Default::default();
        self.activity = Default::default();
        self.help_open = false;
        self.capabilities_open = false;
        Ok(())
    }
    fn poll(&mut self, area: ratatui::layout::Rect) {
        self.sync_roles();
        self.poll_issues();
        if self.page != Page::Running {
            return;
        }
        let (rows, cols) = ui::native_dimensions(area, self.terminal_expanded);
        if self.terminal_size != (rows, cols) {
            self.terminal_size = (rows, cols);
            self.terminal.screen_mut().set_size(rows, cols);
            if let Some(s) = &mut self.session {
                let _ = s.master.resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                });
            }
        }
        // `quiet` means the reader's queue was emptied without new output.
        let (mut got_data, mut quiet) = (false, false);
        if let Some(s) = &mut self.session {
            for _ in 0..128 {
                match s.receiver.try_recv() {
                    Ok(pty::PtyEvent::Data(data)) => {
                        if self.native_first_data_ms.is_none() {
                            self.native_first_data_ms = Some(self.started.elapsed().as_millis());
                        }
                        self.native_received_bytes += data.len() as u64;
                        s.answer_terminal_queries(&data);
                        self.terminal.process(&data);
                        got_data = true;
                    }
                    Ok(pty::PtyEvent::End(e)) => {
                        self.output_end = true;
                        self.message = e;
                        break;
                    }
                    Err(_) => {
                        quiet = !got_data;
                        break;
                    }
                }
            }
        }
        if got_data {
            // Claude's terminal renders status before its JSONL transcripts
            // are flushed. Observe *visible* signals on every poll that
            // brought PTY output, not only after a session ends.
            let screen = self.live_text();
            self.live.observe_screen(&screen, &self.session_id);
            self.native_screen_visible =
                self.native_screen_visible || screen.chars().any(|c| !c.is_whitespace());
        }
        // ConPTY can remain open after a child exits. Poll the child process
        // independently so a failed Claude launch is never shown as "waiting".
        if let Some(s) = &mut self.session {
            match s.child.try_wait() {
                Ok(Some(status)) => {
                    self.child_exit = Some(format!("{:?}", status));
                    self.message = format!("Claude session exited: {:?}", status);
                    // The reader thread may still hold Claude's last output
                    // (often the launch error). Finish only once the queue has
                    // been quiet for a moment; ConPTY may never signal EOF.
                    let exited = *self.child_exited_at.get_or_insert_with(Instant::now);
                    if quiet && exited.elapsed() >= EXIT_DRAIN_GRACE {
                        self.output_end = true;
                    }
                }
                Err(error) if !self.output_end => {
                    self.message = format!("Claude process status error: {}", error);
                }
                _ => {}
            }
        }
        if self.last_team_poll.elapsed() >= Duration::from_millis(850) {
            self.last_team_poll = Instant::now();
            self.team = team::snapshot(
                &team::claude_home(),
                &self.session_id,
                &path_input(&self.project),
                self.session_started_at,
            );
            for agent in &self.team.agents {
                self.agent_seen
                    .entry(agent.name.clone())
                    .or_insert_with(Instant::now);
            }
            self.telemetry.refresh(
                &team::claude_home(),
                &self.session_id,
                &self.team,
                self.session_started_at,
            );
            // Activity pulses are keyed to new *records*, not the last-write
            // time of an entire transcript. A teammate can remain assigned
            // while its animation has correctly returned to idle.
            self.activity
                .observe(&self.telemetry.actors, Instant::now());
            for agent in &self.team.agents {
                if agent.state == "TASK COMPLETE" {
                    let secs = self
                        .agent_seen
                        .get(&agent.name)
                        .map(|t| t.elapsed())
                        .unwrap_or_default();
                    self.agent_done_elapsed
                        .entry(agent.name.clone())
                        .or_insert(secs);
                }
            }
            if self.finished_elapsed.is_none()
                && (self.telemetry.review.objective_complete || self.live.objective_complete)
            {
                self.finished_elapsed = Some(self.started.elapsed());
                self.message =
                    "Objective complete; clock frozen. Claude remains interactive.".into();
            }
            if let Ok(raw) = fs::read_to_string(&self.attention_file) {
                if let Ok(a) = serde_json::from_str::<Attention>(&raw) {
                    if !a.question.is_empty()
                        && self.attention.as_ref().map(|x| x.question.as_str())
                            != Some(a.question.as_str())
                    {
                        self.focus = Focus::Response;
                        self.response = String::new();
                        self.scrolls.get(Pane::Attention).reset();
                    }
                    if !a.question.is_empty() {
                        self.attention = Some(a);
                    }
                }
            }
        }
        // ConPTY can swallow Claude's DA1 terminal handshake and delay first
        // rendered text for about a minute. Report the delay instead of falsely
        // describing it as a team-creation failure. Diagnostic metadata contains
        // no terminal content, user objective, or credentials.
        if !self.demo
            && (self.last_diagnostic.elapsed() >= Duration::from_secs(3) || self.output_end)
        {
            self.last_diagnostic = Instant::now();
            let state = serde_json::json!({
                "session_id": self.session_id,
                "elapsed_seconds": self.started.elapsed().as_secs(),
                "conpty_received_bytes": self.native_received_bytes,
                "first_output_after_ms": self.native_first_data_ms,
                "terminal_has_visible_text": self.native_screen_visible,
                "stream_ended": self.output_end,
                "team_detected": self.team.detected,
                "team_verified": self.team.verified,
                "team_project_linked": self.team.project_linked,
                "actual_claude_lead_session_id": self.team.claude_lead_session_id,
                "team_name": self.team.team_name,
                "team_discovery": self.team.discovery,
                "claude_backend": self.session.as_ref().map(|s|s.backend.as_str()),
                "child_pid": self.session.as_ref().and_then(|s|s.child.process_id()),
                "child_exit": self.child_exit,
                "terminal_queries_answered": self.session.as_ref().map(|s|s.handshake_responses).unwrap_or(0),
            });
            if let Ok(json) = serde_json::to_vec_pretty(&state) {
                let _ = fs::write(self.run_dir.join("startup-status.json"), json);
            }
        }
        if self.output_end {
            if self.finished_elapsed.is_none() {
                self.finished_elapsed = Some(self.started.elapsed());
            }
            self.page = Page::Finished;
            self.focus = Focus::Native;
        }
    }
    pub fn run_elapsed(&self) -> Duration {
        self.finished_elapsed
            .unwrap_or_else(|| self.started.elapsed())
    }
    pub fn teammate_elapsed(&self, name: &str) -> Option<Duration> {
        self.agent_done_elapsed
            .get(name)
            .copied()
            .or_else(|| self.agent_seen.get(name).map(Instant::elapsed))
    }
    /// Sends a user's keystrokes or answer to Claude. Typing is an explicit request for the live
    /// screen, so the reader returns to it. Output that arrives meanwhile is not lost.
    fn send(&mut self, input: &[u8]) -> bool {
        self.terminal.screen_mut().set_scrollback(0);
        self.write_input(input)
    }
    fn write_input(&mut self, input: &[u8]) -> bool {
        if let Some(s) = &mut self.session {
            if let Err(e) = s.writer.write_all(input).and_then(|_| s.writer.flush()) {
                self.message = format!("Claude terminal input failed: {}", e);
                return false;
            }
            return true;
        }
        self.message = "No running Claude terminal is available".into();
        false
    }
    /// Live terminal history is vt100's; every other pane's offset lives in `scrolls`.
    fn live_text(&mut self) -> String {
        let back = self.terminal.screen().scrollback();
        self.terminal.screen_mut().set_scrollback(0);
        let text = self.terminal.screen().contents();
        self.terminal.screen_mut().set_scrollback(back);
        text
    }
    /// Moves the Claude terminal's history view. Offset 0 is the live screen.
    fn scroll_terminal(&mut self, m: Move) {
        if self.terminal.screen().alternate_screen() {
            self.message =
                "Fullscreen Claude has no scrollback; use the wheel or Claude's keys.".into();
            return;
        }
        let back = self.terminal.screen().scrollback();
        self.terminal.screen_mut().set_scrollback(usize::MAX);
        let history = self.terminal.screen().scrollback();
        let top = history.saturating_sub(back);
        let next = scroll::target(m, top, history, self.terminal_size.0 as usize);
        self.terminal.screen_mut().set_scrollback(history - next);
    }
    /// The wheel over the terminal either scrolls its history, or, when fullscreen Claude asked
    /// for mouse input, passes the notch through so Claude scrolls its own view.
    fn terminal_wheel(&mut self, up: bool, column: u16, row: u16, pane: ratatui::layout::Rect) {
        let (alternate, mouse_on, sgr) = {
            let s = self.terminal.screen();
            (
                s.alternate_screen(),
                s.mouse_protocol_mode() != vt100::MouseProtocolMode::None,
                s.mouse_protocol_encoding() == vt100::MouseProtocolEncoding::Sgr,
            )
        };
        if !(alternate && mouse_on) {
            let lines = if up {
                -scroll::WHEEL_LINES
            } else {
                scroll::WHEEL_LINES
            };
            self.scroll_terminal(Move::Lines(lines));
            return;
        }
        let button: u8 = if up { 64 } else { 65 };
        let x = column.saturating_sub(pane.x) + 1;
        let y = row.saturating_sub(pane.y) + 1;
        let bytes = if sgr {
            format!("[<{button};{x};{y}M").into_bytes()
        } else if x <= 222 && y <= 222 {
            vec![0x1b, b'[', b'M', 32 + button, 32 + x as u8, 32 + y as u8]
        } else {
            return;
        };
        self.write_input(&bytes);
    }
    /// Mouse wheel scrolls the pane under the pointer. It never changes focus or selection.
    pub fn wheel(&mut self, up: bool, column: u16, row: u16, area: ratatui::layout::Rect) {
        let Some((pane, rect)) = ui::pane_at(area, self, column, row) else {
            return;
        };
        if pane == Pane::Terminal {
            self.terminal_wheel(up, column, row, rect);
            return;
        }
        let lines = if up {
            -scroll::WHEEL_LINES
        } else {
            scroll::WHEEL_LINES
        };
        self.scrolls.get(pane).apply(Move::Lines(lines));
    }
    /// The pane that Ctrl+scroll moves. It follows keyboard focus on each page.
    fn focused_pane(&self) -> Pane {
        match self.page {
            Page::Setup if self.focus == Focus::Objective => Pane::Objective,
            Page::Setup => Pane::Project,
            Page::Browser => Pane::Folders,
            Page::Issues => self.issue_focus,
            Page::Running | Page::Finished => match self.focus {
                Focus::Response => Pane::Attention,
                Focus::Dashboard => self.dash_focus,
                _ => Pane::Terminal,
            },
        }
    }
    fn scroll_focused(&mut self, m: Move) {
        let pane = if self.help_open {
            Pane::Help
        } else {
            self.focused_pane()
        };
        if pane == Pane::Terminal {
            self.scroll_terminal(m);
        } else {
            self.scrolls.get(pane).apply(m);
        }
    }
    /// Up/Down move the selection and keep it on screen. Otherwise the list's scroll position is kept.
    fn move_issue(&mut self, delta: isize) {
        self.issues.move_by(delta);
        self.scrolls.get(Pane::Issues).reveal(self.issues.index);
    }
    fn handle(&mut self, key: KeyEvent, area: ratatui::layout::Rect) -> bool {
        if key.kind == KeyEventKind::Release {
            return false;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // The capability matrix is modal, like the controls panel: keys never reach Claude.
        if self.capabilities_open {
            if matches!(key.code, KeyCode::Esc | KeyCode::F(7)) {
                self.capabilities_open = false;
            } else if let Some(m) = scroll::key_move(key) {
                self.scrolls.get(Pane::Capabilities).apply(m);
            }
            return false;
        }
        // The Roles overlay is modal too: it takes its own keys and nothing reaches Claude.
        if self.roles_open {
            self.handle_roles(key);
            return false;
        }
        // F1 opens the controls panel on every page. While it is open, keys never reach Claude.
        if key.code == KeyCode::F(1) {
            self.help_open = !self.help_open;
            return false;
        }
        if self.help_open {
            if key.code == KeyCode::Esc {
                self.help_open = false;
            } else if let Some(m) = scroll::key_move(key) {
                self.scrolls.get(Pane::Help).apply(m);
            }
            return false;
        }
        // Ctrl+scroll is handled before any page or Claude sees it.
        if let Some(m) = scroll::key_move(key) {
            self.scroll_focused(m);
            return false;
        }
        match self.page {
            Page::Setup => {
                // Ctrl+L clears the entire multiline objective from either setup field.
                // Also discard the saved objective so a restart does not restore it.
                if ctrl && matches!(key.code, KeyCode::Char('l' | 'L')) {
                    self.objective.clear();
                    self.focus = Focus::Objective;
                    self.scrolls.get(Pane::Objective).reset();
                    let mut prefs = read_prefs();
                    prefs.last_objective.clear();
                    save_prefs(&prefs);
                    self.message = "Objective cleared (including saved previous objective).".into();
                    return false;
                }
                match key.code {
                    KeyCode::Esc => return true,
                    KeyCode::F(2) => self.browser_open(),
                    KeyCode::F(3) => self.open_issues(),
                    KeyCode::F(5) => {
                        if let Err(e) = self.start(area) {
                            self.message = e.to_string();
                        }
                    }
                    KeyCode::F(4) => self.roles_open = true,
                    KeyCode::F(7) => {
                        let discovery = capability::discover(pty::cli_path().ok().as_deref());
                        self.refresh_capabilities(discovery);
                        self.capabilities_open = true;
                    }
                    KeyCode::Tab => {
                        self.focus = if self.focus == Focus::Project {
                            Focus::Objective
                        } else {
                            Focus::Project
                        }
                    }
                    KeyCode::Up if self.focus == Focus::Project && !self.recent.is_empty() => {
                        let pos = self
                            .recent
                            .iter()
                            .position(|x| x == &self.project)
                            .unwrap_or(self.recent.len() - 1);
                        self.project = self.recent[(pos + 1) % self.recent.len()].clone();
                    }
                    KeyCode::Down if self.focus == Focus::Project && !self.recent.is_empty() => {
                        let pos = self
                            .recent
                            .iter()
                            .position(|x| x == &self.project)
                            .unwrap_or(0);
                        self.project =
                            self.recent[(pos + self.recent.len() - 1) % self.recent.len()].clone();
                    }
                    KeyCode::Backspace => {
                        if self.focus == Focus::Project {
                            self.project.pop();
                        } else {
                            self.objective.pop();
                            self.scrolls.get(Pane::Objective).pin_end();
                        }
                    }
                    KeyCode::Enter if self.focus == Focus::Objective => {
                        self.objective.push('\n');
                        self.scrolls.get(Pane::Objective).pin_end();
                    }
                    KeyCode::Char(c) if !ctrl => {
                        if self.focus == Focus::Project {
                            self.project.push(c);
                        } else {
                            self.objective.push(c);
                            self.scrolls.get(Pane::Objective).pin_end();
                        }
                    }
                    _ => (),
                }
            }
            Page::Browser => match key.code {
                KeyCode::Esc => self.page = Page::Setup,
                KeyCode::Up => {
                    self.browser_index = self.browser_index.saturating_sub(1);
                    self.scrolls.get(Pane::Folders).reveal(self.browser_index);
                }
                KeyCode::Down => {
                    self.browser_index =
                        (self.browser_index + 1).min(self.browser_entries.len().saturating_sub(1));
                    self.scrolls.get(Pane::Folders).reveal(self.browser_index);
                }
                KeyCode::Enter => {
                    if let Some(p) = self.browser_entries.get(self.browser_index).cloned() {
                        if let Ok(p) = p.canonicalize() {
                            self.browser_dir = p;
                            self.load_browser();
                        }
                    }
                }
                KeyCode::Char('s') | KeyCode::F(5) => {
                    self.project = self.browser_dir.display().to_string();
                    self.page = Page::Setup;
                }
                _ => (),
            },
            Page::Issues => {
                let before = self.issues.selected().map(|i| i.number);
                match key.code {
                    KeyCode::Esc => {
                        self.cancel_lookup();
                        self.page = Page::Setup;
                        self.message =
                            "Issue selection cancelled. The objective is unchanged.".into();
                    }
                    KeyCode::Enter => {
                        if let (Some(repo), Some(issue)) =
                            (self.issues.repo.clone(), self.issues.selected().cloned())
                        {
                            self.adopt_issue(&repo, &issue);
                            self.page = Page::Setup;
                        }
                    }
                    KeyCode::Tab => {
                        self.issue_focus = if self.issue_focus == Pane::Issues {
                            Pane::Preview
                        } else {
                            Pane::Issues
                        };
                    }
                    KeyCode::Up => self.move_issue(-1),
                    KeyCode::Down => self.move_issue(1),
                    KeyCode::PageUp => self.move_issue(-10),
                    KeyCode::PageDown => self.move_issue(10),
                    KeyCode::Char('r' | 'R') if ctrl => self.open_issues(),
                    KeyCode::Backspace => {
                        let mut filter = self.issues.filter.clone();
                        filter.pop();
                        self.issues.set_filter(filter);
                        self.scrolls.get(Pane::Issues).reset();
                    }
                    KeyCode::Char(c) if !ctrl => {
                        let mut filter = self.issues.filter.clone();
                        filter.push(c);
                        self.issues.set_filter(filter);
                        self.scrolls.get(Pane::Issues).reset();
                    }
                    _ => (),
                }
                // A different issue starts its preview at the top. The same issue keeps its place.
                if self.issues.selected().map(|i| i.number) != before {
                    self.scrolls.get(Pane::Preview).reset();
                }
            }
            Page::Running => {
                match key.code {
                    KeyCode::F(6) => {
                        self.focus = Focus::Native;
                        self.terminal_expanded = false;
                        self.dashboard_expanded = false;
                        self.close_confirm = false;
                    }
                    KeyCode::F(7) if self.attention.is_some() => {
                        self.focus = Focus::Response;
                    }
                    KeyCode::F(8) => {
                        self.terminal_expanded = !self.terminal_expanded;
                        self.dashboard_expanded = false;
                        self.focus = Focus::Native;
                        self.message = if self.terminal_expanded {
                            "Full Claude terminal; F8 returns to split view".into()
                        } else {
                            "Split dashboard restored".into()
                        };
                    }
                    KeyCode::F(9) => {
                        self.dashboard_expanded = !self.dashboard_expanded;
                        self.terminal_expanded = false;
                        self.message = if self.dashboard_expanded {
                            "Full Agent Tree; F9 returns to split view".into()
                        } else {
                            "Split dashboard restored".into()
                        };
                    }
                    KeyCode::F(4) => {
                        if self.telemetry.review.objective_complete || self.live.objective_complete
                        {
                            self.message =
                                "Objective completion already recorded; the clock is frozen."
                                    .into();
                        } else if self.clock_paused_manually {
                            if let Some(previous) = self.finished_elapsed.take() {
                                self.started = Instant::now()
                                    .checked_sub(previous)
                                    .unwrap_or_else(Instant::now);
                            }
                            self.clock_paused_manually = false;
                            self.message =
                                "Run clock resumed manually (Claude was not stopped).".into();
                        } else {
                            self.finished_elapsed = Some(self.started.elapsed());
                            self.clock_paused_manually = true;
                            self.message =
                                "Run clock paused manually; Claude can still work.".into();
                        }
                    }
                    KeyCode::F(10) => {
                        self.message =
                            "Claude is still running. Finish or /exit before returning to setup."
                                .into();
                    }
                    // F5 moves dashboard focus. The first press focuses the current panel, then it cycles.
                    KeyCode::F(5) => {
                        if self.focus == Focus::Dashboard {
                            let at = Pane::DASHBOARD
                                .iter()
                                .position(|p| *p == self.dash_focus)
                                .unwrap_or(0);
                            self.dash_focus = Pane::DASHBOARD[(at + 1) % Pane::DASHBOARD.len()];
                        }
                        self.focus = Focus::Dashboard;
                        self.terminal_expanded = false;
                        self.message = format!(
                            "Dashboard panel {:?}. Ctrl+Up/Down scroll it; F6 returns to Claude.",
                            self.dash_focus
                        );
                    }
                    KeyCode::Char('q') if ctrl => {
                        if self.close_confirm {
                            self.session.take();
                            return true;
                        }
                        self.close_confirm = true;
                        self.message =
                            "Ctrl+Q again to terminate the Claude session and close.".into();
                    }
                    KeyCode::Enter if self.focus == Focus::Response => {
                        if !self.response.trim().is_empty() {
                            let answer = self.response.trim().to_owned();
                            if self.send(format!("{}\r", answer).as_bytes()) {
                                let _ = fs::remove_file(&self.attention_file);
                                self.attention = None;
                                self.response.clear();
                                self.focus = Focus::Native;
                                self.message =
                                    "Answer sent to Claude's interactive terminal.".into();
                            }
                        }
                    }
                    KeyCode::Backspace if self.focus == Focus::Response => {
                        self.response.pop();
                    }
                    KeyCode::Char(c) if self.focus == Focus::Response && !ctrl => {
                        self.response.push(c)
                    }
                    _ if self.focus == Focus::Native => {
                        if let Some(bytes) = key_bytes(key) {
                            // Clock remains frozen for the finished objective.
                            // A new objective starts a new run from setup.
                            self.send(&bytes);
                        }
                    }
                    _ => (),
                }
            }
            Page::Finished => match key.code {
                KeyCode::Enter | KeyCode::F(10) => {
                    self.page = Page::Setup;
                    self.session = None;
                    self.attention = None;
                    self.message.clear();
                }
                KeyCode::Esc => return true,
                _ => (),
            },
        }
        false
    }
}

// Correct VT input sequences; normal characters go straight to the native Claude terminal.
fn key_bytes(event: KeyEvent) -> Option<Vec<u8>> {
    let c = event.code;
    let ctrl = event.modifiers.contains(KeyModifiers::CONTROL);
    let alt = event.modifiers.contains(KeyModifiers::ALT);
    let shift = event.modifiers.contains(KeyModifiers::SHIFT);
    let mut bytes = match c {
        KeyCode::Char(ch) if ctrl && ch.is_ascii_alphabetic() => {
            vec![(ch.to_ascii_lowercase() as u8) - b'a' + 1]
        }
        KeyCode::Char(ch) => ch.to_string().into_bytes(),
        KeyCode::Enter => b"\r".to_vec(),
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        _ => return None,
    };
    if alt && !matches!(c, KeyCode::Esc) {
        bytes.insert(0, 0x1b);
    }
    if shift && matches!(c, KeyCode::Enter) {
        bytes = b"\x1b[13;2u".to_vec();
    }
    Some(bytes)
}

struct Reset;
impl Drop for Reset {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
    }
}
fn dashboard(args: Args) -> Result<()> {
    let issue = args.issue;
    let mut app = App::new(args);
    if let Some(number) = issue {
        app.preselect_issue(number);
    }
    enable_raw_mode().context("Switching terminal to raw input")?;
    let _reset = Reset;
    let mut stdout = io::stdout();
    // Mouse capture is enabled after the alternate screen and disabled by `Reset` before leaving it.
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    'frames: loop {
        let dim = terminal.size()?;
        let rect = ratatui::layout::Rect::new(0, 0, dim.width, dim.height);
        app.poll(rect);
        terminal.draw(|f| ui::draw(f, &app))?;
        // Wait up to one frame for input, then take what queued meanwhile. Mouse movement
        // is dense on Windows, so this stops it delaying keystrokes by a frame each.
        let mut wait = Duration::from_millis(80);
        for _ in 0..256 {
            if !event::poll(wait)? {
                break;
            }
            wait = Duration::ZERO;
            match event::read()? {
                Event::Key(k) if app.handle(k, rect) => break 'frames,
                Event::Mouse(m) => match m.kind {
                    MouseEventKind::ScrollUp => app.wheel(true, m.column, m.row, rect),
                    MouseEventKind::ScrollDown => app.wheel(false, m.column, m.row, rect),
                    _ => (),
                },
                Event::Resize(_, _) => {
                    terminal.clear()?;
                }
                _ => (),
            }
        }
    }
    Ok(())
}
fn main() -> Result<()> {
    let args = Args::parse();
    if let Some(dir) = &args.plugin {
        let (data, store) = (plugin_data_path(), plugins_path());
        let report = plugins::command(dir, &data, &store, args.plugin_trust, args.plugin_revoke)
            .map_err(anyhow::Error::msg)?;
        print!("{report}");
        return Ok(());
    }
    dashboard(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn key_maps_normal_and_ctrl_c() {
        assert_eq!(
            key_bytes(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(vec![3])
        );
        assert_eq!(
            key_bytes(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Some(vec![13])
        );
    }
    #[test]
    fn run_clock_can_be_frozen_without_stopping_claude() {
        let mut a = App::new(Args {
            project: None,
            issue: None,
            objective: None,
            demo: true,
            ..Args::default()
        });
        a.finished_elapsed = Some(Duration::from_secs(42));
        assert_eq!(a.run_elapsed(), Duration::from_secs(42));
    }
    #[test]
    fn issue_flag_conflicts_with_objective_and_requires_project() {
        let parse = |args: &[&str]| {
            Args::try_parse_from(std::iter::once("agentree").chain(args.iter().copied()))
        };
        assert!(parse(&["--project", "C:/r", "--issue", "38", "--objective", "x"]).is_err());
        assert!(parse(&["--issue", "38"]).is_err());
        assert!(parse(&["--project", "C:/r", "--issue", "0"]).is_err());
        let ok = parse(&["--project", "C:/r", "--issue", "38"]).unwrap();
        assert_eq!(ok.issue, Some(38));
        assert!(parse(&["--project", "C:/r", "--objective", "x"])
            .unwrap()
            .issue
            .is_none());
    }
    #[test]
    fn plugin_flags_leave_the_normal_launch_alone() {
        let parse = |args: &[&str]| {
            Args::try_parse_from(std::iter::once("agentree").chain(args.iter().copied()))
        };
        let plain = parse(&[]).unwrap();
        assert!(plain.plugin.is_none() && !plain.plugin_trust && !plain.plugin_revoke);
        assert!(parse(&["--project", "C:/r"]).unwrap().plugin.is_none());
        assert!(parse(&["--plugin", "C:/p"]).is_ok());
        assert!(parse(&["--plugin", "C:/p", "--plugin-trust"]).is_ok());
        assert!(parse(&["--plugin", "C:/p", "--project", "C:/r"]).is_err());
        assert!(parse(&["--plugin-trust"]).is_err());
        assert!(parse(&["--plugin", "C:/p", "--plugin-trust", "--plugin-revoke"]).is_err());
    }
    #[test]
    fn issue_startup_never_restores_saved_objective() {
        let a = App::new(Args {
            project: Some(std::env::current_dir().unwrap()),
            issue: Some(38),
            objective: None,
            demo: true,
            ..Args::default()
        });
        assert!(a.objective.is_empty());
        assert_eq!(a.page, Page::Setup);
    }
    #[test]
    fn explicit_objective_still_prefills_setup() {
        let a = App::new(Args {
            project: Some(std::env::current_dir().unwrap()),
            issue: None,
            objective: Some("Ship the picker".into()),
            demo: true,
            ..Args::default()
        });
        assert_eq!(a.objective, "Ship the picker");
        assert_eq!(a.page, Page::Setup);
        assert_eq!(a.focus, Focus::Project);
    }
    #[test]
    fn adopting_an_issue_fills_objective_for_review_without_starting() {
        let mut a = App::new(Args {
            project: Some(std::env::current_dir().unwrap()),
            issue: None,
            objective: Some("old text".into()),
            demo: true,
            ..Args::default()
        });
        let repo = github::RepoRef {
            owner: "acme".into(),
            name: "tool".into(),
        };
        let issue = github::GithubIssue {
            number: 9,
            title: "Title".into(),
            body: Some("Body".into()),
            state: "OPEN".into(),
            url: "https://github.com/acme/tool/issues/9".into(),
            milestone: None,
            labels: vec![],
        };
        a.adopt_issue(&repo, &issue);
        assert!(a.objective.starts_with("Work on GitHub issue acme/tool#9."));
        assert!(a.objective.contains("Title: Title\n"));
        assert_eq!(a.focus, Focus::Objective);
        assert!(a.message.contains("replaced previous text"));
        assert!(a.session.is_none());
        assert_eq!(a.page, Page::Setup);
    }
    #[test]
    fn empty_objective_does_not_start() {
        let args = Args {
            project: Some(std::env::current_dir().unwrap()),
            issue: None,
            objective: Some(String::new()),
            demo: true,
            ..Args::default()
        };
        let mut a = App::new(args);
        assert!(a.start(ratatui::layout::Rect::new(0, 0, 140, 40)).is_err());
    }
    #[test]
    fn launch_is_blocked_by_an_invalid_plan_and_opens_the_matrix() {
        use capability::{Discovery, Row, Status};
        let row = |flag: &'static str, status| Row {
            capability: flag.into(),
            flag: Some(flag),
            status,
            note: String::new(),
        };
        let old_cli = Discovery {
            cli: Some("claude.exe".into()),
            rows: vec![
                row("--model", Status::Supported),
                row("--advisor", Status::Unsupported),
            ],
        };
        let mut a = App::new(Args {
            project: Some(std::env::current_dir().unwrap()),
            issue: None,
            objective: Some("Do the thing".into()),
            demo: true,
            ..Args::default()
        });
        let err = a.check_launch(old_cli).unwrap_err().to_string();
        assert!(err.starts_with("Launch blocked:"), "{err}");
        assert!(
            err.contains("--advisor") && err.contains("claude update"),
            "{err}"
        );
        assert!(a.capabilities_open);
        assert_eq!(a.launch_problems.len(), 1);

        let ok = Discovery {
            cli: Some("claude.exe".into()),
            rows: vec![
                row("--model", Status::Supported),
                row("--advisor", Status::Supported),
            ],
        };
        a.capabilities_open = false;
        assert!(a.check_launch(ok).is_ok());
        assert!(a.launch_problems.is_empty());
        assert!(!a.capabilities_open);
    }
    fn lookup_app(project: &str) -> App {
        let mut a = App::new(Args {
            project: Some(PathBuf::from(project)),
            issue: None,
            objective: None,
            demo: true,
            ..Args::default()
        });
        a.objective.clear();
        a
    }
    fn single_issue(number: u64) -> github::Loaded {
        github::Loaded::Single {
            repo: github::RepoRef {
                owner: "acme".into(),
                name: "tool".into(),
            },
            issue: github::GithubIssue {
                number,
                title: "Title".into(),
                body: Some("Body".into()),
                state: "OPEN".into(),
                url: format!("https://github.com/acme/tool/issues/{number}"),
                milestone: None,
                labels: vec![],
            },
        }
    }
    #[test]
    fn lookup_for_previous_project_is_never_adopted() {
        let mut a = lookup_app("F:/repo-a");
        let (ticket, tx) = a.begin_lookup();
        a.project = "F:/repo-b".into();
        tx.send(github::Lookup {
            ticket,
            result: Ok(single_issue(7)),
        })
        .unwrap();
        a.poll_issues();
        assert!(a.objective.is_empty());
        assert!(a.issues.ticket.is_none());
        assert!(!a.issues.loading);
        assert!(a.message.contains("Project changed"));
    }
    #[test]
    fn superseded_lookup_is_dropped_and_current_one_still_applies() {
        let mut a = lookup_app("F:/repo-a");
        let (old, old_tx) = a.begin_lookup();
        let (current, current_tx) = a.begin_lookup();
        // The old receiver was replaced, so its late send fails and nothing can apply it.
        assert!(old_tx
            .send(github::Lookup {
                ticket: old,
                result: Ok(single_issue(1)),
            })
            .is_err());
        current_tx
            .send(github::Lookup {
                ticket: current.clone(),
                result: Ok(single_issue(2)),
            })
            .unwrap();
        a.poll_issues();
        assert_eq!(
            a.objective.lines().next(),
            Some("Work on GitHub issue acme/tool#2.")
        );
        assert!(a.issues.ticket.is_none());
    }
    #[test]
    fn non_current_ticket_is_ignored_without_cancelling_the_current_lookup() {
        let mut a = lookup_app("F:/repo-a");
        let (current, tx) = a.begin_lookup();
        let forged = github::Ticket {
            id: current.id + 99,
            project: current.project.clone(),
        };
        tx.send(github::Lookup {
            ticket: forged,
            result: Ok(single_issue(3)),
        })
        .unwrap();
        a.poll_issues();
        assert!(a.objective.is_empty());
        assert_eq!(a.issues.ticket, Some(current));
        assert!(a.issues.loading);
    }
    #[test]
    fn escape_cancels_the_lookup_and_its_late_result_is_discarded() {
        let mut a = lookup_app("F:/repo-a");
        a.page = Page::Issues;
        let (ticket, tx) = a.begin_lookup();
        let area = ratatui::layout::Rect::new(0, 0, 140, 40);
        a.handle(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), area);
        assert_eq!(a.page, Page::Setup);
        assert!(!a.issues.loading);
        assert!(tx
            .send(github::Lookup {
                ticket,
                result: Ok(single_issue(4)),
            })
            .is_err());
        a.poll_issues();
        assert!(a.objective.is_empty());
    }
    #[test]
    fn matching_project_lookup_still_preselects_issue() {
        let mut a = lookup_app(&std::env::current_dir().unwrap().display().to_string());
        let (ticket, tx) = a.begin_lookup();
        tx.send(github::Lookup {
            ticket,
            result: Ok(single_issue(5)),
        })
        .unwrap();
        a.poll_issues();
        assert!(a.objective.starts_with("Work on GitHub issue acme/tool#5."));
        assert_eq!(a.focus, Focus::Objective);
    }

    use ratatui::layout::Rect;

    fn draw_text(app: &App, width: u16, height: u16) -> String {
        use ratatui::{backend::TestBackend, Terminal};
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| ui::draw(f, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }
    fn region(app: &App, width: u16, height: u16, pane: Pane) -> ratatui::layout::Rect {
        ui::regions(ratatui::layout::Rect::new(0, 0, width, height), app)
            .into_iter()
            .find(|(p, _)| *p == pane)
            .map(|(_, r)| r)
            .expect("pane is on screen")
    }
    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }
    const AREA: Rect = Rect::new(0, 0, 140, 40);
    /// A running page with a small terminal whose history can be inspected.
    fn running_app() -> App {
        let mut a = App::new(Args {
            project: Some(std::env::current_dir().unwrap()),
            issue: None,
            objective: None,
            demo: true,
            ..Args::default()
        });
        a.page = Page::Running;
        a.focus = Focus::Native;
        a.terminal = vt100::Parser::new(10, 40, HISTORY_LINES);
        a.terminal_size = (10, 40);
        a
    }
    fn feed_lines(a: &mut App, range: std::ops::Range<usize>) {
        for i in range {
            a.terminal.process(format!("line {i}\r\n").as_bytes());
        }
    }
    fn screen_row(a: &App, y: u16) -> String {
        (0..40)
            .map(|x| {
                a.terminal
                    .screen()
                    .cell(y, x)
                    .map(|c| c.contents().to_owned())
                    .unwrap_or_default()
            })
            .collect::<String>()
            .trim_end()
            .to_owned()
    }
    /// Setup with the built-in profiles saved to a temporary file, never the user's `profiles.json`.
    fn roles_app() -> (App, PathBuf) {
        let dir = std::env::temp_dir().join(format!("agentree-app-roles-{}", Uuid::new_v4()));
        let file = dir.join("profiles.json");
        let mut a = App::new(Args {
            project: Some(std::env::current_dir().unwrap()),
            issue: None,
            objective: None,
            demo: true,
            ..Args::default()
        });
        a.profiles_file = file;
        a.profiles = profiles::Store::default();
        a.role_settings = roles::Settings::default();
        a.settings_for = a.project.clone();
        a.settings_key = a.project_id();
        a.role_notices.clear();
        (a, dir)
    }
    #[test]
    fn roles_overlay_saves_each_change_and_the_next_plan_uses_it() {
        let (mut a, dir) = roles_app();
        assert!(!a.handle(key(KeyCode::F(4), KeyModifiers::NONE), AREA));
        assert!(a.roles_open);
        // Lead row, effort field: default -> low.
        a.handle(key(KeyCode::Tab, KeyModifiers::NONE), AREA);
        assert_eq!(a.roles_field, roles::Field::Effort);
        a.handle(key(KeyCode::Right, KeyModifiers::NONE), AREA);
        assert_eq!(a.role_settings.plan().effort.as_deref(), Some("low"));
        let saved = profiles::load(&a.profiles_file, &dir.join("roles.json")).0;
        assert_eq!(saved.active_settings().lead.effort, "low");
        // Keys stay in the overlay: the Setup objective is untouched.
        let objective = a.objective.clone();
        a.handle(key(KeyCode::Char('x'), KeyModifiers::NONE), AREA);
        assert_eq!(a.objective, objective);
        a.handle(key(KeyCode::Esc, KeyModifiers::NONE), AREA);
        assert!(!a.roles_open);
        let _ = fs::remove_dir_all(dir);
    }

    /// v0.5 release gate, UI half: the model is changed in the overlay, survives a restart and is
    /// the plan that launches; a plan the CLI cannot run is refused with nothing spawned.
    #[test]
    fn release_gate_overlay_model_change_survives_restart_and_is_checked_before_launch() {
        use capability::{Discovery, Row, Status};
        let (mut a, dir) = roles_app();
        let default_lead = roles::Settings::default().lead.model;
        a.handle(key(KeyCode::F(4), KeyModifiers::NONE), AREA);
        assert_eq!(a.roles_row, 0);
        assert_eq!(a.roles_field, roles::Field::Model);
        a.handle(key(KeyCode::Right, KeyModifiers::NONE), AREA);
        let chosen = a.role_settings.lead.model.clone();
        assert_ne!(chosen, default_lead);

        // Restart: a fresh load of the saved file launches the same plan.
        let (back, notices) = profiles::load(&a.profiles_file, &dir.join("roles.json"));
        assert!(notices.is_empty(), "{notices:?}");
        assert_eq!(
            back.effective(&a.project_id()).plan(),
            a.role_settings.plan()
        );
        assert_eq!(a.role_settings.plan().lead_model, chosen);

        // The installed CLI rejecting --model blocks the launch before any session exists.
        let no_model = Discovery {
            cli: Some("claude.exe".into()),
            rows: vec![Row {
                capability: "--model".into(),
                flag: Some("--model"),
                status: Status::Unsupported,
                note: String::new(),
            }],
        };
        let err = a.check_launch(no_model).unwrap_err().to_string();
        assert!(err.contains("--model"), "{err}");
        assert!(a.capabilities_open);
        assert!(a.session.is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn roles_overlay_explains_a_field_that_does_not_apply() {
        let (mut a, dir) = roles_app();
        a.roles_open = true;
        // Lead row has no concurrency setting.
        a.roles_field = roles::Field::Concurrency;
        a.handle(key(KeyCode::Right, KeyModifiers::NONE), AREA);
        assert!(
            a.message.contains("Lead has no concurrency setting"),
            "{}",
            a.message
        );
        assert!(!a.profiles_file.exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn roles_overlay_reset_restores_the_selected_role() {
        let (mut a, dir) = roles_app();
        a.roles_open = true;
        a.roles_row = 4; // Coder
        a.roles_field = roles::Field::Concurrency;
        a.handle(key(KeyCode::Right, KeyModifiers::NONE), AREA);
        assert_eq!(a.role_settings.coder.concurrency, 4);
        a.handle(key(KeyCode::Char('d'), KeyModifiers::NONE), AREA);
        assert_eq!(a.role_settings.coder, roles::Settings::default().coder);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn roles_overlay_switches_profile_and_edits_project_overrides() {
        let (mut a, dir) = roles_app();
        let id = a.project_id();
        a.roles_open = true;
        let press = |a: &mut App, code| {
            a.handle(key(code, KeyModifiers::NONE), AREA);
        };
        // Global: p moves the active profile, and the settings follow it.
        press(&mut a, KeyCode::Char('p'));
        assert_eq!(a.profiles.active, "Local");
        assert_eq!(a.role_settings.plan().advisor, None);
        press(&mut a, KeyCode::Char('P'));
        assert_eq!(a.profiles.active, "Balanced");

        // Project scope: an override changes this project and leaves the profile alone.
        press(&mut a, KeyCode::Char('s'));
        assert!(a.roles_project_scope);
        a.roles_field = roles::Field::Effort;
        press(&mut a, KeyCode::Right);
        assert_eq!(a.role_settings.lead.effort, "low");
        assert_eq!(a.profiles.active_settings().lead.effort, "");
        assert!(a.profiles.has_overrides(&id, roles::Role::Lead));

        // Pin a profile for the project; the override still wins over it.
        press(&mut a, KeyCode::Char('p'));
        assert_eq!(a.profiles.profile_name(&id), "Budget");
        assert_eq!(a.profiles.active, "Balanced");

        // Everything survives a restart.
        let (back, _) = profiles::load(&a.profiles_file, &dir.join("roles.json"));
        assert_eq!(back.effective(&id), a.role_settings);

        // d clears the role's overrides only; leaving scope edits the profile again.
        press(&mut a, KeyCode::Char('d'));
        assert!(!a.profiles.has_overrides(&id, roles::Role::Lead));
        press(&mut a, KeyCode::Char('s'));
        assert!(!a.roles_project_scope);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn global_edits_stay_visible_when_the_project_pins_another_profile() {
        use roles::{Field, Role};
        let (mut a, dir) = roles_app();
        let id = a.project_id();
        a.profiles.cycle_project_profile(&id, 1); // pin Budget (lead effort low)
        a.apply_profiles();
        a.roles_open = true;
        a.roles_field = Field::Effort;
        assert!(!a.roles_project_scope);
        // The table shows the profile being edited, and the pinned one is what launches.
        assert_eq!(a.roles_view().display(Role::Lead, Field::Effort), "default");
        assert_eq!(a.role_settings.lead.effort, "low");
        a.handle(key(KeyCode::Right, KeyModifiers::NONE), AREA);
        assert_eq!(a.roles_view().display(Role::Lead, Field::Effort), "low");
        assert_eq!(a.profiles.active_settings().lead.effort, "low");
        a.handle(key(KeyCode::Right, KeyModifiers::NONE), AREA);
        assert_eq!(a.roles_view().display(Role::Lead, Field::Effort), "medium");
        // What launches is still the project's pin.
        assert_eq!(a.role_settings.lead.effort, "low");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn changing_the_project_re_resolves_the_settings_on_setup_only() {
        let (mut a, dir) = roles_app();
        let other = dir.join("other");
        fs::create_dir_all(&other).unwrap();
        let id = profiles::project_key(&other);
        a.profiles.cycle_project_profile(&id, 1); // Budget
        a.sync_roles();
        assert_eq!(a.role_settings, roles::Settings::default());
        a.project = other.display().to_string();
        a.sync_roles();
        assert_eq!(a.role_settings.lead.effort, "low");
        // A running session keeps what it launched with.
        a.page = Page::Running;
        a.project = std::env::current_dir().unwrap().display().to_string();
        a.sync_roles();
        assert_eq!(a.role_settings.lead.effort, "low");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn ctrl_scroll_keys_are_never_forwarded_to_claude() {
        let mut a = running_app();
        feed_lines(&mut a, 0..100);
        // With no session, forwarding would replace the message. Scrolling must leave it alone.
        a.message = "untouched".into();
        assert!(!a.handle(key(KeyCode::Up, KeyModifiers::CONTROL), AREA));
        assert!(!a.handle(key(KeyCode::PageUp, KeyModifiers::CONTROL), AREA));
        assert!(!a.handle(key(KeyCode::Home, KeyModifiers::CONTROL), AREA));
        assert_eq!(a.message, "untouched");
        assert!(a.terminal.screen().scrollback() > 0);
        assert!(!a.handle(key(KeyCode::End, KeyModifiers::CONTROL), AREA));
        assert_eq!(a.terminal.screen().scrollback(), 0);
    }
    #[test]
    fn plain_keys_still_reach_claude_and_typing_returns_to_live() {
        let mut a = running_app();
        feed_lines(&mut a, 0..100);
        a.handle(key(KeyCode::PageUp, KeyModifiers::CONTROL), AREA);
        assert!(a.terminal.screen().scrollback() > 0);
        a.message.clear();
        // Typing is sent to Claude (here: fails because no session), and returns the reader to live.
        a.handle(key(KeyCode::Char('x'), KeyModifiers::NONE), AREA);
        assert_eq!(a.message, "No running Claude terminal is available");
        assert_eq!(a.terminal.screen().scrollback(), 0);
        // Plain PageUp is Claude's key, so it is forwarded and does not scroll the history.
        a.message.clear();
        a.handle(key(KeyCode::PageUp, KeyModifiers::NONE), AREA);
        assert_eq!(a.message, "No running Claude terminal is available");
        assert_eq!(a.terminal.screen().scrollback(), 0);
    }
    #[test]
    fn history_stays_put_while_output_arrives_and_end_returns_to_live() {
        let mut a = running_app();
        feed_lines(&mut a, 0..100);
        // Live top row is "line 91"; one page back is "line 82".
        a.handle(key(KeyCode::PageUp, KeyModifiers::CONTROL), AREA);
        assert_eq!(screen_row(&a, 0), "line 82");
        feed_lines(&mut a, 100..150);
        assert_eq!(
            screen_row(&a, 0),
            "line 82",
            "reader must not be pulled to the bottom"
        );
        a.handle(key(KeyCode::End, KeyModifiers::CONTROL), AREA);
        assert_eq!(a.terminal.screen().scrollback(), 0);
        assert_eq!(screen_row(&a, 8), "line 149");
    }
    #[test]
    fn live_observation_reads_the_live_screen_even_while_scrolled() {
        let mut a = running_app();
        feed_lines(&mut a, 0..100);
        a.handle(key(KeyCode::PageUp, KeyModifiers::CONTROL), AREA);
        let scrolled = a.terminal.screen().scrollback();
        let text = a.live_text();
        assert!(text.contains("line 99"), "{text}");
        assert!(!text.contains("line 82"), "{text}");
        assert_eq!(
            a.terminal.screen().scrollback(),
            scrolled,
            "reading live text must not move the reader"
        );
    }
    #[test]
    fn wheel_over_terminal_scrolls_its_history_and_wheel_elsewhere_does_not_focus() {
        let mut a = running_app();
        feed_lines(&mut a, 0..100);
        let term = region(&a, 140, 40, Pane::Terminal);
        a.wheel(true, term.x + 3, term.y + 2, AREA);
        assert_eq!(a.terminal.screen().scrollback(), 3);
        assert_eq!(a.focus, Focus::Native);
    }
    #[test]
    fn fullscreen_claude_gets_the_wheel_only_when_it_asked_for_mouse() {
        let mut a = running_app();
        feed_lines(&mut a, 0..100);
        a.terminal.process(b"\x1b[?1049h");
        let term = region(&a, 140, 40, Pane::Terminal);
        // Alternate screen without mouse reporting: nothing to scroll, and the user is told so.
        a.wheel(true, term.x + 3, term.y + 2, AREA);
        assert!(a.message.contains("no scrollback"), "{}", a.message);
        // Mouse reporting on: the wheel is passed to Claude, which fails here with no session.
        a.message.clear();
        a.terminal.process(b"\x1b[?1002h\x1b[?1006h");
        a.wheel(true, term.x + 3, term.y + 2, AREA);
        assert_eq!(a.message, "No running Claude terminal is available");
        assert_eq!(a.terminal.screen().scrollback(), 0);
    }
    #[test]
    fn f5_focuses_dashboard_and_its_keys_never_reach_claude() {
        let mut a = running_app();
        a.message.clear();
        a.handle(key(KeyCode::F(5), KeyModifiers::NONE), AREA);
        assert_eq!(a.focus, Focus::Dashboard);
        assert_eq!(a.dash_focus, Pane::Timeline);
        a.handle(key(KeyCode::F(5), KeyModifiers::NONE), AREA);
        assert_eq!(
            a.dash_focus,
            Pane::Totals,
            "second press cycles to the next panel"
        );
        a.message.clear();
        a.handle(key(KeyCode::Char('x'), KeyModifiers::NONE), AREA);
        assert!(
            a.message.is_empty(),
            "typing on the dashboard is not forwarded"
        );
        a.handle(key(KeyCode::F(6), KeyModifiers::NONE), AREA);
        assert_eq!(a.focus, Focus::Native);
    }
    #[test]
    fn f1_opens_controls_and_keeps_keys_away_from_claude() {
        let mut a = running_app();
        a.message.clear();
        a.handle(key(KeyCode::F(1), KeyModifiers::NONE), AREA);
        assert!(a.help_open);
        a.handle(key(KeyCode::Char('x'), KeyModifiers::NONE), AREA);
        assert!(a.message.is_empty());
        assert!(!a.handle(key(KeyCode::Esc, KeyModifiers::NONE), AREA));
        assert!(!a.help_open, "Esc closes controls without leaving the page");
    }
    fn issues_app(count: u64, body_lines: usize) -> App {
        let mut a = App::new(Args {
            project: Some(std::env::current_dir().unwrap()),
            issue: None,
            objective: None,
            demo: true,
            ..Args::default()
        });
        a.page = Page::Issues;
        let body = (0..body_lines)
            .map(|i| format!("body line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        a.issues.issues = (1..=count)
            .map(|n| github::GithubIssue {
                number: n,
                title: format!("Issue {n}"),
                body: Some(body.clone()),
                state: "OPEN".into(),
                url: String::new(),
                milestone: None,
                labels: vec![],
            })
            .collect();
        a
    }
    #[test]
    fn issue_preview_scrolls_independently_and_resets_for_another_issue() {
        let mut a = issues_app(3, 80);
        draw_text(&a, 120, 30);
        let preview = region(&a, 120, 30, Pane::Preview);
        a.wheel(
            false,
            preview.x + 2,
            preview.y + 2,
            Rect::new(0, 0, 120, 30),
        );
        assert_eq!(a.scrolls.get(Pane::Preview).top(), 3);
        assert_eq!(a.scrolls.get(Pane::Issues).top(), 0);
        assert_eq!(a.issues.index, 0, "wheel never changes the selected issue");
        draw_text(&a, 120, 30);
        assert!(a.scrolls.get(Pane::Preview).top() > 0);
        a.handle(
            key(KeyCode::Down, KeyModifiers::NONE),
            Rect::new(0, 0, 120, 30),
        );
        draw_text(&a, 120, 30);
        assert_eq!(a.scrolls.get(Pane::Preview).top(), 0);
    }
    #[test]
    fn issue_list_keeps_selection_visible_and_wheel_keeps_selection() {
        let mut a = issues_app(30, 1);
        let area = Rect::new(0, 0, 78, 21);
        draw_text(&a, 78, 21);
        for _ in 0..20 {
            a.handle(key(KeyCode::Down, KeyModifiers::NONE), area);
        }
        draw_text(&a, 78, 21);
        let view_top = a.scrolls.get(Pane::Issues).top();
        let viewport = region(&a, 78, 21, Pane::Issues).height as usize - 2;
        assert!(view_top <= 20 && 20 < view_top + viewport, "top {view_top}");
        let top_before = a.scrolls.get(Pane::Issues).top();
        let list = region(&a, 78, 21, Pane::Issues);
        a.wheel(true, list.x + 2, list.y + 2, area);
        assert_eq!(
            a.issues.index, 20,
            "scrolling the list keeps the selected row"
        );
        assert_eq!(a.scrolls.get(Pane::Issues).top(), top_before - 3);
    }
    #[test]
    fn long_objective_is_reviewable_from_the_top_and_follows_the_end_while_editing() {
        let mut a = App::new(Args {
            project: Some(std::env::current_dir().unwrap()),
            issue: None,
            objective: Some(
                (0..200)
                    .map(|i| format!("line {i}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            demo: true,
            ..Args::default()
        });
        a.focus = Focus::Objective;
        let at_top = draw_text(&a, 120, 30);
        assert!(at_top.contains("line 0"));
        assert!(!at_top.contains("line 199"));
        // Editing pins the view to the end, so the newest text and the cursor are visible.
        a.handle(
            key(KeyCode::Char('x'), KeyModifiers::NONE),
            Rect::new(0, 0, 120, 30),
        );
        let edited = draw_text(&a, 120, 30);
        assert!(edited.contains("line 199x"), "newest text must be visible");
        // Scrolling up leaves the end, and the position is shown in the title.
        a.handle(
            key(KeyCode::Up, KeyModifiers::CONTROL),
            Rect::new(0, 0, 120, 30),
        );
        assert!(draw_text(&a, 120, 30).contains("↑"));
    }
    #[test]
    fn every_page_and_overlay_renders_at_small_and_large_sizes() {
        let mut a = running_app();
        feed_lines(&mut a, 0..300);
        let attention = || Attention {
            title: "Pick one".into(),
            reason: "Two options".into(),
            question: "Which?".into(),
            options: vec!["a".into(), "b".into()],
        };
        let mut states = vec![];
        for page in [
            Page::Setup,
            Page::Browser,
            Page::Issues,
            Page::Running,
            Page::Finished,
        ] {
            for help in [false, true] {
                for dashboard in [false, true] {
                    let mut s = App::new(Args {
                        project: Some(std::env::current_dir().unwrap()),
                        issue: None,
                        objective: Some(
                            (0..80)
                                .map(|i| format!("objective {i}"))
                                .collect::<Vec<_>>()
                                .join(
                                    "
",
                                ),
                        ),
                        demo: true,
                        ..Args::default()
                    });
                    s.page = page;
                    s.help_open = help;
                    s.dashboard_expanded = dashboard;
                    s.attention = Some(attention());
                    s.focus = Focus::Response;
                    s.terminal = std::mem::replace(&mut a.terminal, vt100::Parser::new(1, 1, 0));
                    s.terminal_size = (10, 40);
                    states.push(s);
                }
            }
        }
        for s in &states {
            for (w, h) in [(78u16, 21u16), (100, 30), (200, 60), (30, 10)] {
                draw_text(s, w, h);
                // Overlays must never receive wheel events outside the screen.
                let _ = ui::pane_at(
                    Rect::new(0, 0, w, h),
                    s,
                    w.saturating_sub(1),
                    h.saturating_sub(1),
                );
            }
        }
    }
    #[test]
    fn long_issue_body_wraps_and_reports_its_overflow() {
        let mut a = issues_app(1, 0);
        a.issues.issues[0].body = Some("word ".repeat(200));
        let text = draw_text(&a, 78, 21);
        // The preview is short in a narrow terminal, so the body must wrap and show a position.
        assert!(text.contains("↓ 1-"), "{text}");
        a.wheel(
            false,
            region(&a, 78, 21, Pane::Preview).x + 2,
            region(&a, 78, 21, Pane::Preview).y + 2,
            Rect::new(0, 0, 78, 21),
        );
        assert!(draw_text(&a, 78, 21).contains("↑"));
    }
    #[test]
    fn status_messages_and_key_hints_are_visible_on_the_main_pages() {
        for (width, height) in [(78u16, 21u16), (100, 30)] {
            for page in [Page::Setup, Page::Issues, Page::Running] {
                let mut a = running_app();
                a.page = page;
                a.focus = Focus::Native;
                a.message = "Project changed; the GitHub issue lookup was cancelled.".into();
                let text = draw_text(&a, width, height);
                assert!(
                    text.contains("Project changed"),
                    "{page:?} {width}x{height}: {text}"
                );
                // The hint line is one line and starts with F1, so the controls panel is always reachable.
                assert!(text.contains("F1 controls"), "{page:?} {width}x{height}");
            }
        }
    }
}
