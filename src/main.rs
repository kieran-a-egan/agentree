mod team;
mod telemetry;
mod live;
mod prompt;
mod pty;
mod ui;

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::{event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers}, execute,
                terminal::{enable_raw_mode,disable_raw_mode,EnterAlternateScreen,LeaveAlternateScreen}};
use portable_pty::PtySize;
use serde::{Deserialize,Serialize};
use std::{collections::HashMap, fs, io::{self, Write}, path::PathBuf, time::{Duration,Instant,SystemTime}};
use ratatui::{backend::CrosstermBackend, Terminal};
use uuid::Uuid;

#[derive(Parser)]
#[command(name="agentree", version, about="Native Claude teams, one Windows terminal")]
struct Args {
    #[arg(long)] project: Option<PathBuf>,
    #[arg(long)] objective: Option<String>,
    #[arg(long)] demo: bool,
}
#[derive(Default,Serialize,Deserialize)]
struct Prefs { projects: Vec<String>, last_objective: String }

fn data_home() -> PathBuf {
    if let Some(p) = std::env::var_os("LOCALAPPDATA") { PathBuf::from(p).join("ClaudeAgentTree") }
    else { std::env::temp_dir().join("ClaudeAgentTree") }
}
fn prefs_path() -> PathBuf { data_home().join("prefs.json") }
fn read_prefs() -> Prefs {
    fs::read_to_string(prefs_path()).ok().and_then(|s|serde_json::from_str(&s).ok()).unwrap_or_default()
}
fn save_prefs(preferences: &Prefs) {
    let _ = fs::create_dir_all(data_home());
    if let Ok(json) = serde_json::to_vec_pretty(preferences) { let _ = fs::write(prefs_path(),json); }
}
fn path_input(s: &str) -> PathBuf {
    let s = s.trim().trim_matches('"');
    if s == "~" { return std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_else(||PathBuf::from(s)); }
    if let Some(rest) = s.strip_prefix("~/").or_else(||s.strip_prefix("~\\")) {
        if let Some(home) = std::env::var_os("USERPROFILE") { return PathBuf::from(home).join(rest); }
    }
    PathBuf::from(s)
}
/// How long the PTY queue must stay quiet after Claude exits before finishing.
const EXIT_DRAIN_GRACE: Duration = Duration::from_millis(300);
#[derive(Clone,Copy,PartialEq,Eq)]
pub enum Page { Setup, Browser, Running, Finished }
#[derive(Clone,Copy,PartialEq,Eq)]
pub enum Focus { Project, Objective, Native, Response }
#[derive(Debug,Default,Deserialize)]
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
    pub terminal_size: (u16,u16),
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
}
impl App {
    fn new(args: Args) -> Self {
        let p = read_prefs();
        let project = args.project.map(|v|v.to_string_lossy().to_string())
            .or_else(|| p.projects.first().cloned())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default().display().to_string());
        let objective = args.objective.unwrap_or(p.last_objective);
        let home = data_home();
        let session_id = Uuid::new_v4().to_string();
        let run_dir = home.join("runs").join(&session_id);
        Self { page:Page::Setup, focus:Focus::Project, project, objective, message:String::new(),
            recent:p.projects, browser_dir:PathBuf::new(),browser_entries:vec![],browser_index:0,
            team:Default::default(), telemetry:Default::default(), agent_seen:HashMap::new(), terminal_expanded:false, dashboard_expanded:false, session_id,attention:None,response:String::new(),
            terminal:vt100::Parser::new(24,80,300), session:None,attention_file:run_dir.join("attention.json"),
            run_dir, started:Instant::now(), finished_elapsed:None,clock_paused_manually:false, agent_done_elapsed:HashMap::new(), session_started_at:SystemTime::now(), demo:args.demo, output_end:false,
            last_team_poll:Instant::now(),terminal_size:(24,80),close_confirm:false,
            native_screen_visible:false,native_received_bytes:0,native_first_data_ms:None,last_diagnostic:Instant::now(),child_exit:None,child_exited_at:None, live:Default::default(), activity:Default::default() }
    }
    fn browser_open(&mut self) {
        self.browser_dir = if path_input(&self.project).is_dir() {path_input(&self.project)}
            else { std::env::current_dir().unwrap_or_else(|_|PathBuf::from(".")) };
        self.load_browser(); self.page=Page::Browser;
    }
    fn load_browser(&mut self) {
        self.browser_entries = vec![self.browser_dir.join("..")];
        if let Ok(entries)=fs::read_dir(&self.browser_dir) {
            let mut sub:Vec<PathBuf>=entries.flatten().map(|e|e.path()).filter(|p|p.is_dir()).collect();
            sub.sort_by_key(|p|p.file_name().unwrap_or_default().to_string_lossy().to_ascii_lowercase());
            self.browser_entries.extend(sub);
        }
        self.browser_index=0;
    }
    fn start(&mut self, area: ratatui::layout::Rect) -> Result<()> {
        let project=path_input(&self.project).canonicalize()
            .with_context(||format!("Project folder not accessible: {}",self.project))?;
        if !project.is_dir() { anyhow::bail!("Select a directory, not a file"); }
        if self.objective.trim().is_empty() { anyhow::bail!("Enter an objective before starting"); }
        let cli=if self.demo {None} else {Some(pty::cli_path()?)};
        self.project=project.display().to_string();
        let mut pref = read_prefs();
        pref.projects.retain(|x|x!=&self.project);
        pref.projects.insert(0,self.project.clone()); pref.projects.truncate(12);
        pref.last_objective=self.objective.clone(); save_prefs(&pref);
        self.recent=pref.projects;
        self.session_id=Uuid::new_v4().to_string();
        self.run_dir=data_home().join("runs").join(&self.session_id);
        self.attention_file=self.run_dir.join("attention.json");
        fs::create_dir_all(&self.run_dir)?;
        let policy_file = self.run_dir.join("instructions.txt");
        fs::write(&policy_file,prompt::system_policy(&self.attention_file,&self.session_id))?;
        let initial = prompt::initial_objective(&self.objective);
        self.terminal_expanded=false;self.dashboard_expanded=false;
        let (rows,cols)=ui::native_dimensions(area,self.terminal_expanded);
        self.terminal_size=(rows,cols);
        self.terminal=vt100::Parser::new(rows,cols,400);
        self.started=Instant::now();
        self.finished_elapsed=None; self.clock_paused_manually=false; self.agent_done_elapsed.clear();
        self.session_started_at=SystemTime::now();
        if let Some(cli)=cli {
            self.session=Some(pty::spawn(&cli,&project,&self.session_id,&policy_file,&initial,rows,cols)?);
            self.message=format!("Claude process launched. Waiting for terminal startup | {}",self.session_id);
        } else {
            self.terminal.process(b"\x1b[32mDEMO - no Claude process launched\x1b[0m\r\nNative teammates and attention are simulated only in --demo mode.\r\n");
            self.message="SIMULATED dashboard. No model calls, no edits.".into();
        }
        self.page=Page::Running;self.focus=Focus::Native;
        self.terminal_expanded=false;self.output_end=false;self.close_confirm=false;self.team=Default::default();self.telemetry.reset();self.agent_seen.clear();self.attention=None;
        self.native_screen_visible=false;self.native_received_bytes=0;self.native_first_data_ms=None;self.last_diagnostic=Instant::now();self.child_exit=None;self.child_exited_at=None;self.live=Default::default();self.activity=Default::default();
        Ok(())
    }
    fn poll(&mut self,area:ratatui::layout::Rect) {
        if self.page!=Page::Running {return;}
        let (rows,cols)=ui::native_dimensions(area,self.terminal_expanded);
        if self.terminal_size!=(rows,cols) {
            self.terminal_size=(rows,cols);
            self.terminal.set_size(rows,cols);
            if let Some(s)=&mut self.session {
                let _=s.master.resize(PtySize{rows,cols,pixel_width:0,pixel_height:0});
            }
        }
        // `quiet` means the reader's queue was emptied without new output.
        let (mut got_data,mut quiet)=(false,false);
        if let Some(s)=&mut self.session {
            for _ in 0..128 {
                match s.receiver.try_recv() {
                    Ok(pty::PtyEvent::Data(data))=>{
                        if self.native_first_data_ms.is_none() {
                            self.native_first_data_ms=Some(self.started.elapsed().as_millis());
                        }
                        self.native_received_bytes+=data.len() as u64;
                        s.answer_terminal_queries(&data);
                        self.terminal.process(&data);
                        got_data=true;
                    },
                    Ok(pty::PtyEvent::End(e))=>{self.output_end=true;self.message=e;break;},
                    Err(_)=>{quiet=!got_data;break;},
                }
            }
        }
        if got_data {
            // Claude's terminal renders status before its JSONL transcripts
            // are flushed. Observe *visible* signals on every poll that
            // brought PTY output, not only after a session ends.
            let screen=self.terminal.screen().contents();
            self.live.observe_screen(&screen,&self.session_id);
            self.native_screen_visible = self.native_screen_visible || screen.chars().any(|c| !c.is_whitespace());
        }
        // ConPTY can remain open after a child exits. Poll the child process
        // independently so a failed Claude launch is never shown as "waiting".
        if let Some(s)=&mut self.session {
            match s.child.try_wait() {
                Ok(Some(status)) => {
                    self.child_exit=Some(format!("{:?}",status));
                    self.message=format!("Claude session exited: {:?}",status);
                    // The reader thread may still hold Claude's last output
                    // (often the launch error). Finish only once the queue has
                    // been quiet for a moment; ConPTY may never signal EOF.
                    let exited=*self.child_exited_at.get_or_insert_with(Instant::now);
                    if quiet && exited.elapsed()>=EXIT_DRAIN_GRACE {self.output_end=true;}
                },
                Err(error) if !self.output_end => { self.message=format!("Claude process status error: {}",error); },
                _ => {},
            }
        }
        if self.last_team_poll.elapsed()>=Duration::from_millis(850) {
            self.last_team_poll=Instant::now();
            self.team=team::snapshot(&team::claude_home(),&self.session_id,&path_input(&self.project),self.session_started_at);
            for agent in &self.team.agents {
                self.agent_seen.entry(agent.name.clone()).or_insert_with(Instant::now);
            }
            self.telemetry.refresh(&team::claude_home(), &self.session_id, &self.team, self.session_started_at);
            // Activity pulses are keyed to new *records*, not the last-write
            // time of an entire transcript. A teammate can remain assigned
            // while its animation has correctly returned to idle.
            self.activity.observe(&self.telemetry.actors,Instant::now());
            for agent in &self.team.agents {
                if agent.state=="TASK COMPLETE" {
                    let secs=self.agent_seen.get(&agent.name).map(|t|t.elapsed()).unwrap_or_default();
                    self.agent_done_elapsed.entry(agent.name.clone()).or_insert(secs);
                }
            }
            if self.finished_elapsed.is_none() && (self.telemetry.review.objective_complete || self.live.objective_complete) {
                self.finished_elapsed=Some(self.started.elapsed());
                self.message="Objective complete; clock frozen. Claude remains interactive.".into();
            }
            if let Ok(raw)=fs::read_to_string(&self.attention_file) {
                if let Ok(a)=serde_json::from_str::<Attention>(&raw) {
                    if !a.question.is_empty() && self.attention.as_ref().map(|x|x.question.as_str())!=Some(a.question.as_str()) {
                        self.focus=Focus::Response;
                        self.response=String::new();
                    }
                    if !a.question.is_empty(){self.attention=Some(a);}
                }
            }
        }
        // ConPTY can swallow Claude's DA1 terminal handshake and delay first
        // rendered text for about a minute. Report the delay instead of falsely
        // describing it as a team-creation failure. Diagnostic metadata contains
        // no terminal content, user objective, or credentials.
        if !self.demo && (self.last_diagnostic.elapsed()>=Duration::from_secs(3) || self.output_end) {
            self.last_diagnostic=Instant::now();
            let state=serde_json::json!({
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
            if let Ok(json)=serde_json::to_vec_pretty(&state) {
                let _=fs::write(self.run_dir.join("startup-status.json"),json);
            }
        }
        if self.output_end {
            if self.finished_elapsed.is_none(){self.finished_elapsed=Some(self.started.elapsed());}
            self.page=Page::Finished; self.focus=Focus::Native;
        }
    }
    pub fn run_elapsed(&self)->Duration {
        self.finished_elapsed.unwrap_or_else(||self.started.elapsed())
    }
    pub fn teammate_elapsed(&self,name:&str)->Option<Duration> {
        self.agent_done_elapsed.get(name).copied().or_else(||self.agent_seen.get(name).map(Instant::elapsed))
    }
    fn send(&mut self, input: &[u8]) -> bool {
        if let Some(s)=&mut self.session {
            if let Err(e)=s.writer.write_all(input).and_then(|_|s.writer.flush()) {
                self.message=format!("Claude terminal input failed: {}",e);
                return false;
            }
            return true;
        }
        self.message="No running Claude terminal is available".into();
        false
    }
    fn handle(&mut self,key:KeyEvent,area:ratatui::layout::Rect)->bool {
        if key.kind==KeyEventKind::Release {return false;}
        let ctrl=key.modifiers.contains(KeyModifiers::CONTROL);
        match self.page {
            Page::Setup => {
                // Ctrl+L clears the entire multiline objective from either setup field.
                // Also discard the saved objective so a restart does not restore it.
                if ctrl && matches!(key.code,KeyCode::Char('l'|'L')) {
                    self.objective.clear();
                    self.focus=Focus::Objective;
                    let mut prefs=read_prefs();
                    prefs.last_objective.clear();
                    save_prefs(&prefs);
                    self.message="Objective cleared (including saved previous objective).".into();
                    return false;
                }
                match key.code {
                    KeyCode::Esc=>return true,
                    KeyCode::F(2)=>self.browser_open(),
                    KeyCode::F(5)=>if let Err(e)=self.start(area) {self.message=e.to_string();},
                    KeyCode::Tab=>self.focus=if self.focus==Focus::Project {Focus::Objective}else{Focus::Project},
                    KeyCode::Up if self.focus==Focus::Project && !self.recent.is_empty()=>{
                        let pos=self.recent.iter().position(|x|x==&self.project).unwrap_or(self.recent.len()-1);
                        self.project=self.recent[(pos+1)%self.recent.len()].clone();
                    },
                    KeyCode::Down if self.focus==Focus::Project && !self.recent.is_empty()=>{
                        let pos=self.recent.iter().position(|x|x==&self.project).unwrap_or(0);
                        self.project=self.recent[(pos+self.recent.len()-1)%self.recent.len()].clone();
                    },
                    KeyCode::Backspace=>{
                        if self.focus==Focus::Project {self.project.pop();}else{self.objective.pop();}
                    },
                    KeyCode::Enter if self.focus==Focus::Objective => self.objective.push('\n'),
                    KeyCode::Char(c) if !ctrl => {
                        if self.focus==Focus::Project {self.project.push(c);}else{self.objective.push(c);}
                    },
                    _=>(),
                }
            },
            Page::Browser=>match key.code {
                KeyCode::Esc=>self.page=Page::Setup,
                KeyCode::Up=>self.browser_index=self.browser_index.saturating_sub(1),
                KeyCode::Down=>self.browser_index=(self.browser_index+1).min(self.browser_entries.len().saturating_sub(1)),
                KeyCode::Enter=>{
                    if let Some(p)=self.browser_entries.get(self.browser_index).cloned(){
                        if let Ok(p)=p.canonicalize() {self.browser_dir=p;self.load_browser();}
                    }
                },
                KeyCode::Char('s') | KeyCode::F(5)=>{
                    self.project=self.browser_dir.display().to_string();self.page=Page::Setup;
                },
                _=>(),
            },
            Page::Running=>{
                match key.code {
                    KeyCode::F(6)=>{self.focus=Focus::Native;self.terminal_expanded=false;self.dashboard_expanded=false;self.close_confirm=false;},
                    KeyCode::F(7) if self.attention.is_some()=>{self.focus=Focus::Response;},
                    KeyCode::F(8)=>{self.terminal_expanded=!self.terminal_expanded; self.dashboard_expanded=false; self.focus=Focus::Native; self.message=if self.terminal_expanded {"Full Claude terminal; F8 returns to split view".into()}else{"Split dashboard restored".into()};},
                    KeyCode::F(9)=>{self.dashboard_expanded=!self.dashboard_expanded; self.terminal_expanded=false; self.message=if self.dashboard_expanded {"Full Agent Tree; F9 returns to split view".into()}else{"Split dashboard restored".into()};},
                    KeyCode::F(4)=>{
                        if self.telemetry.review.objective_complete || self.live.objective_complete {
                            self.message="Objective completion already recorded; the clock is frozen.".into();
                        } else if self.clock_paused_manually {
                            if let Some(previous)=self.finished_elapsed.take(){
                                self.started=Instant::now().checked_sub(previous).unwrap_or_else(Instant::now);
                            }
                            self.clock_paused_manually=false;
                            self.message="Run clock resumed manually (Claude was not stopped).".into();
                        } else {
                            self.finished_elapsed=Some(self.started.elapsed());
                            self.clock_paused_manually=true;
                            self.message="Run clock paused manually; Claude can still work.".into();
                        }
                    },
                    KeyCode::F(10)=>{self.message="Claude is still running. Finish or /exit before returning to setup.".into();},
                    KeyCode::Char('q') if ctrl =>{
                        if self.close_confirm { self.session.take(); return true; }
                        self.close_confirm=true;self.message="Ctrl+Q again to terminate the Claude session and close.".into();
                    },
                    KeyCode::Enter if self.focus==Focus::Response => {
                        if !self.response.trim().is_empty() {
                            let answer=self.response.trim().to_owned();
                            if self.send(format!("{}\r",answer).as_bytes()) {
                                let _=fs::remove_file(&self.attention_file);
                                self.attention=None;self.response.clear();self.focus=Focus::Native;
                                self.message="Answer sent to Claude's interactive terminal.".into();
                            }
                        }
                    },
                    KeyCode::Backspace if self.focus==Focus::Response=>{self.response.pop();},
                    KeyCode::Char(c) if self.focus==Focus::Response && !ctrl=>self.response.push(c),
                    _ if self.focus==Focus::Native=> {
                        if let Some(bytes)=key_bytes(key){
                            // Clock remains frozen for the finished objective.
                            // A new objective starts a new run from setup.
                            self.send(&bytes);
                        }
                    },
                    _=>(),
                }
            },
            Page::Finished=>match key.code {
                KeyCode::Enter|KeyCode::F(10)=>{self.page=Page::Setup;self.session=None;self.attention=None;self.message.clear();},
                KeyCode::Esc=>return true,
                _=>(),
            },
        }
        false
    }
}

// Correct VT input sequences; normal characters go straight to the native Claude terminal.
fn key_bytes(event:KeyEvent)->Option<Vec<u8>> {
    let c=event.code;
    let ctrl=event.modifiers.contains(KeyModifiers::CONTROL);
    let alt=event.modifiers.contains(KeyModifiers::ALT);
    let shift=event.modifiers.contains(KeyModifiers::SHIFT);
    let mut bytes=match c {
        KeyCode::Char(ch) if ctrl && ch.is_ascii_alphabetic()=>vec![(ch.to_ascii_lowercase() as u8)-b'a'+1],
        KeyCode::Char(ch)=>ch.to_string().into_bytes(),
        KeyCode::Enter=>b"\r".to_vec(),
        KeyCode::Backspace=>vec![0x7f],
        KeyCode::Tab=>b"\t".to_vec(),
        KeyCode::BackTab=>b"\x1b[Z".to_vec(),
        KeyCode::Esc=>vec![0x1b],
        KeyCode::Up=>b"\x1b[A".to_vec(), KeyCode::Down=>b"\x1b[B".to_vec(),
        KeyCode::Right=>b"\x1b[C".to_vec(),KeyCode::Left=>b"\x1b[D".to_vec(),
        KeyCode::Home=>b"\x1b[H".to_vec(),KeyCode::End=>b"\x1b[F".to_vec(),
        KeyCode::Delete=>b"\x1b[3~".to_vec(),
        KeyCode::PageUp=>b"\x1b[5~".to_vec(), KeyCode::PageDown=>b"\x1b[6~".to_vec(),
        _=>return None,
    };
    if alt && !matches!(c,KeyCode::Esc) {bytes.insert(0,0x1b);}
    if shift && matches!(c,KeyCode::Enter) { bytes=b"\x1b[13;2u".to_vec(); }
    Some(bytes)
}

struct Reset;
impl Drop for Reset {
    fn drop(&mut self) {
        let _=disable_raw_mode();
        let _=execute!(io::stdout(),LeaveAlternateScreen);
    }
}
fn dashboard(args:Args)->Result<()> {
    let mut app=App::new(args);
    enable_raw_mode().context("Switching terminal to raw input")?;
    let _reset=Reset;
    let mut stdout=io::stdout();
    execute!(stdout,EnterAlternateScreen)?;
    let mut terminal=Terminal::new(CrosstermBackend::new(stdout))?;
    loop {
        let dim=terminal.size()?;
        let rect=ratatui::layout::Rect::new(0,0,dim.width,dim.height);
        app.poll(rect);
        terminal.draw(|f|ui::draw(f,&app))?;
        if event::poll(Duration::from_millis(80))? {
            match event::read()? {
                Event::Key(k) if app.handle(k,rect)=>break,
                Event::Resize(_,_)=>{terminal.clear()?;},
                _=>(),
            }
        }
    }
    Ok(())
}
fn main()->Result<()> {
    dashboard(Args::parse())
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn key_maps_normal_and_ctrl_c() {
        assert_eq!(key_bytes(KeyEvent::new(KeyCode::Char('c'),KeyModifiers::CONTROL)),Some(vec![3]));
        assert_eq!(key_bytes(KeyEvent::new(KeyCode::Enter,KeyModifiers::NONE)),Some(vec![13]));
    }
    #[test] fn run_clock_can_be_frozen_without_stopping_claude() {
        let mut a=App::new(Args{project:None,objective:None,demo:true});
        a.finished_elapsed=Some(Duration::from_secs(42));
        assert_eq!(a.run_elapsed(),Duration::from_secs(42));
    }
    #[test] fn empty_objective_does_not_start() {
        let args=Args{project:Some(std::env::current_dir().unwrap()),objective:Some(String::new()),demo:true};
        let mut a=App::new(args);
        assert!(a.start(ratatui::layout::Rect::new(0,0,140,40)).is_err());
    }
}
