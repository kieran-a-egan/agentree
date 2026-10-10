use serde::Deserialize;
use std::{collections::BTreeMap, fs, io::{BufRead, BufReader}, path::{Path, PathBuf}, time::{Duration, SystemTime}};

#[derive(Default, Clone, Debug)]
pub struct Agent {
    pub name: String,
    pub role: String,
    pub session_id: String,
    pub state: String,
    pub task: String,
}
#[derive(Default, Clone, Debug)]
pub struct TeamSnapshot {
    pub detected: bool,
    pub verified: bool,
    /// Linked by project and startup time, not by the CLI-requested UUID.
    pub project_linked: bool,
    pub claude_lead_session_id: String,
    pub agents: Vec<Agent>,
    pub team_name: String,
    pub discovery: String,
}
#[derive(Default, Deserialize)]
#[serde(default, rename_all="camelCase")]
struct TeamConfig { members: Vec<Member>, lead_session_id: String, cwd: String }
#[derive(Default, Deserialize)]
#[serde(default, rename_all="camelCase")]
struct Member { name: String, agent_type: String, session_id: String, cwd: String }
fn value_as_text(v: Option<&serde_json::Value>) -> String {
    v.and_then(serde_json::Value::as_str).unwrap_or("").to_owned()
}

// The GUID Agentree supplies via --session-id can differ from the
// leadSessionId Claude records for a newly created native team. Resolve a team
// conservatively: exact GUID first, otherwise unique project + launch-time
// evidence. Never choose the most recently modified arbitrary team.
fn same_project(a: &Path, b: &Path) -> bool {
    fn key(p: &Path) -> String {
        let resolved = p.canonicalize().unwrap_or_else(|_|p.to_path_buf());
        resolved.to_string_lossy().replace('\\', "/").trim_end_matches('/').to_ascii_lowercase()
    }
    key(a) == key(b)
}
fn belongs_to_project(config: &TeamConfig, project: &Path, claude_home: &Path) -> bool {
    if !config.cwd.is_empty() && same_project(Path::new(&config.cwd),project) { return true; }
    if config.members.iter().any(|m| !m.cwd.is_empty() &&
        m.agent_type == "team-lead" && same_project(Path::new(&m.cwd),project)) { return true; }
    // Team lead cwd may be absent from config. Link through the lead's session
    // transcript, which records cwd for the same leadSessionId. Reading the
    // first few JSONL records avoids loading a potentially enormous transcript.
    if config.lead_session_id.is_empty() { return false; }
    let Ok(dirs) = fs::read_dir(claude_home.join("projects")) else { return false; };
    for dir in dirs.flatten() {
        let transcript = dir.path().join(format!("{}.jsonl",config.lead_session_id));
        let Ok(file) = fs::File::open(transcript) else { continue; };
        for line in BufReader::new(file).lines().take(32).flatten() {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
                let cwd = value.get("cwd").and_then(|c|c.as_str()).unwrap_or("");
                if !cwd.is_empty() && same_project(Path::new(cwd),project) { return true; }
            }
        }
    }
    false
}
fn created_during_run(file: &Path, started_at: SystemTime) -> bool {
    let earliest=started_at.checked_sub(Duration::from_secs(10)).unwrap_or(started_at);
    // On Windows created() is supported. modified() is a fallback for other
    // file systems, but project matching is still required for association.
    file.metadata().ok().and_then(|m|m.created().or_else(|_|m.modified()).ok())
        .is_some_and(|t|t>=earliest)
}

pub fn snapshot(claude_home: &Path, session_id: &str, project: &Path,
                started_at: SystemTime) -> TeamSnapshot {
    let expected_name = format!("session-{}", session_id.chars().take(8).collect::<String>());
    let team_root = claude_home.join("teams");
    let mut state = TeamSnapshot::default();
    let mut exact: Option<(String, TeamConfig)> = None;
    let mut project_candidates: Vec<(String, TeamConfig)> = Vec::new();
    let mut weak_candidates: Vec<(String, TeamConfig)> = Vec::new();
    let mut scanned=0usize;
    if let Ok(entries)=fs::read_dir(&team_root) {
        for entry in entries.flatten() {
            let file=entry.path().join("config.json");
            if !file.is_file() { continue; }
            scanned+=1;
            let name=entry.file_name().to_string_lossy().to_string();
            let Ok(raw)=fs::read_to_string(&file) else { continue; };
            let Ok(config)=serde_json::from_str::<TeamConfig>(&raw) else { continue; };
            if config.lead_session_id == session_id ||
                config.members.iter().any(|m|m.agent_type=="team-lead" && m.session_id==session_id) {
                exact=Some((name,config));
                break;
            }
            // A folder alone is insufficient if Claude explicitly records a
            // different leadSessionId (the failure in the Forgeboard run).
            if name==expected_name && config.lead_session_id.is_empty() {
                exact=Some((name,config));
                break;
            }
            if !created_during_run(&file,started_at) { continue; }
            if belongs_to_project(&config,project,claude_home) {
                project_candidates.push((name,config));
            } else if config.lead_session_id.is_empty() {
                weak_candidates.push((name,config));
            }
        }
    }
    let selected = if let Some(pair)=exact {
        state.verified=true;
        state.discovery="CLI session ID matches Claude team lead".into();
        Some(pair)
    } else if project_candidates.len()==1 {
        state.project_linked=true;
        state.discovery="unique team created this run; project/cwd matched (lead ID differs)".into();
        project_candidates.pop()
    } else if project_candidates.len()>1 {
        state.discovery=format!("{} new teams match project; ambiguous, not linked", project_candidates.len());
        None
    } else if weak_candidates.len()==1 && session_subagents(claude_home,session_id).is_none() {
        // An old-format config lacking a lead ID and cwd is only a hint.
        // Display members as unverified, never claim a confirmed native team.
        state.discovery="new team config found; session/project unverified".into();
        weak_candidates.pop()
    } else {
        state.discovery=format!("no linked native team; {} config(s) scanned",scanned);
        None
    };
    let Some((team_name,config))=selected else {
        if let Some(mut background)=session_subagents(claude_home,session_id) {
            background.discovery=format!("{} background subagent(s) under requested session; {} team config(s) scanned",
                background.agents.len(),scanned);
            return background;
        }
        return state;
    };
    state.claude_lead_session_id=config.lead_session_id.clone();
    state.detected = true;
    state.team_name = team_name.clone();
    for m in config.members {
        if m.agent_type == "team-lead" { continue; }
        if m.name.is_empty() { continue; }
        state.agents.push(Agent { name: m.name, role: m.agent_type, session_id: m.session_id,
            state: if state.verified || state.project_linked {"TEAM MEMBER"} else {"UNVERIFIED"}.into(),
            task: String::new() });
    }
    let task_dir = claude_home.join("tasks").join(&team_name);
    if let Ok(entries) = fs::read_dir(&task_dir) {
        // An agent can own several task files. Show the most live one
        // (in_progress > pending > completed) regardless of directory order;
        // sorted paths make ties deterministic.
        let mut files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        files.sort();
        let mut shown = BTreeMap::<String, u8>::new();
        for file in files {
            if file.extension().and_then(|s|s.to_str()) != Some("json") { continue; }
            if let Ok(text) = fs::read_to_string(&file) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                    if v.get("status").is_none() { continue; }
                    let status = value_as_text(v.get("status"));
                    let owner = value_as_text(v.get("owner"));
                    let (rank, label) = match status.as_str() {
                        "in_progress" => (3, "WORKING"), "completed" => (1, "TASK COMPLETE"), _ => (2, "WAITING")
                    };
                    if !owner.is_empty() && rank >= shown.get(&owner).copied().unwrap_or(0) {
                        if let Some(a) = state.agents.iter_mut().find(|a| a.name == owner) {
                            a.task = value_as_text(v.get("subject"));
                            a.state = label.into();
                            shown.insert(owner, rank);
                        }
                    }
                }
            }
        }
    }
    state
}

/// Read Claude's background-subagent manifests, not the native Team roster.
/// The exact `<session_id>/subagents/` path prevents cross-session leakage.
/// We deliberately do not infer a RUNNING or DONE state from file mtime:
/// a transcript recently written to disk does not prove the process is alive.
fn session_subagents(claude_home: &Path, session_id: &str) -> Option<TeamSnapshot> {
    let projects = fs::read_dir(claude_home.join("projects")).ok()?;
    let mut agents = BTreeMap::<String, Agent>::new();
    for project in projects.flatten() {
        let dir = project.path().join(session_id).join("subagents");
        if !dir.is_dir() { continue; }
        let Ok(entries) = fs::read_dir(dir) else { continue; };
        for entry in entries.flatten() {
            let file = entry.path();
            let Some(filename_os) = file.file_name() else { continue; };
            let Some(filename) = filename_os.to_str() else { continue; };
            let id = if let Some(id) = filename.strip_prefix("agent-").and_then(|s| s.strip_suffix(".meta.json")) {
                id
            } else if let Some(id) = filename.strip_prefix("agent-").and_then(|s| s.strip_suffix(".jsonl")) {
                id
            } else { continue; };
            if id.is_empty() { continue; }
            let meta = file.with_file_name(format!("agent-{id}.meta.json"));
            let trace = file.with_file_name(format!("agent-{id}.jsonl"));
            let details: serde_json::Value = fs::read_to_string(&meta).ok()
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or(serde_json::Value::Null);
            let display_name = value_as_text(details.get("name"));
            let description = value_as_text(details.get("description"));
            let role = value_as_text(details.get("agentType"));
            let name = if display_name.is_empty() {
                format!("agent-{}", id.chars().take(8).collect::<String>())
            } else { display_name };
            // RECENT OUTPUT means only that the trace file was updated recently.
            let recent = trace.metadata().ok().and_then(|m|m.modified().ok())
                .and_then(|modified|SystemTime::now().duration_since(modified).ok())
                .is_some_and(|age|age < Duration::from_secs(30));
            let task = if description.is_empty() { "Delegated subagent task".into() } else { description };
            agents.insert(id.to_string(), Agent { name, role, session_id: String::new(),
                state: if recent { "RECENT OUTPUT" } else { "RECORDED" }.into(), task });
        }
    }
    if agents.is_empty() { return None; }
    Some(TeamSnapshot {
        detected: true, verified: false,
        agents: agents.into_values().collect(),
        team_name: String::new(),
        ..TeamSnapshot::default()
    })
}

pub fn claude_home() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))
                .map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
            home.join(".claude")
        })
}

#[cfg(test)] mod tests {
    use super::*;
    use std::time::Duration;
    fn temp_fixture() -> PathBuf {
        let p = std::env::temp_dir().join(format!("agentree-team-test-{}",uuid::Uuid::new_v4()));
        fs::create_dir_all(p.join("teams")).unwrap();
        fs::create_dir_all(p.join("tasks")).unwrap();
        p
    }
    #[test] fn missing_team_is_not_invented() {
        let s = snapshot(Path::new("a/nonexistent/test/folder"), "12345678-0000-0000-0000-000000000000", Path::new("/some-project"), SystemTime::now());
        assert!(!s.detected); assert!(s.agents.is_empty());
    }
    #[test] fn discovers_nonstandard_team_by_lead_session() {
        let home=temp_fixture();
        let p=home.join("teams").join("my-feature"); fs::create_dir_all(&p).unwrap();
        fs::write(p.join("config.json"),r#"{"leadSessionId":"12345678-abcd","members":[{"name":"team-lead","agentType":"team-lead"},{"name":"coder","agentType":"general-purpose","model":"claude-haiku-5-5"}]}"#).unwrap();
        let s=snapshot(&home,"12345678-abcd",Path::new("/some-project"),SystemTime::now());
        assert!(s.detected && s.verified); assert_eq!(s.team_name,"my-feature");
        assert_eq!(s.agents[0].name,"coder");
        fs::remove_dir_all(home).unwrap();
    }
    #[test] fn unrelated_team_not_attributed_to_active_session() {
        let home=temp_fixture();
        let p=home.join("teams").join("someone-else"); fs::create_dir_all(&p).unwrap();
        fs::write(p.join("config.json"),r#"{"leadSessionId":"unrelated","members":[{"name":"other","agentType":"general-purpose"}]}"#).unwrap();
        let s=snapshot(&home,"our-session",Path::new("/some-project"),SystemTime::now()+Duration::from_secs(60));
        assert!(!s.detected);
        fs::remove_dir_all(home).unwrap();
    }
    #[test] fn recent_unverified_team_is_labelled_as_such() {
        let home=temp_fixture();
        let p=home.join("teams").join("custom-team"); fs::create_dir_all(&p).unwrap();
        fs::write(p.join("config.json"),r#"{"members":[{"name":"explorer","agentType":"general-purpose"}]}"#).unwrap();
        let s=snapshot(&home,"not-a-match",Path::new("/some-project"),SystemTime::now()-Duration::from_secs(60));
        assert!(s.detected && !s.verified);
        assert_eq!(s.agents[0].state,"UNVERIFIED");
        fs::remove_dir_all(home).unwrap();
    }
    #[test] fn session_named_folder_without_lead_id_is_linked() {
        let home=temp_fixture();
        let p=home.join("teams").join("session-ab12cd34"); fs::create_dir_all(&p).unwrap();
        fs::write(p.join("config.json"),r#"{"members":[{"name":"coder"}]}"#).unwrap();
        let s=snapshot(&home,"ab12cd34-0000-0000-0000-000000000000",Path::new("/some-project"),SystemTime::now()+Duration::from_secs(60));
        assert!(s.detected && s.verified);
        assert_eq!(s.agents[0].name,"coder");
        fs::remove_dir_all(home).unwrap();
    }
    #[test] fn in_progress_task_wins_over_completed_in_any_file_order() {
        let home=temp_fixture();
        let p=home.join("teams").join("native"); fs::create_dir_all(&p).unwrap();
        fs::write(p.join("config.json"),r#"{"leadSessionId":"lead","members":[{"name":"coder"}]}"#).unwrap();
        let tasks=home.join("tasks").join("native"); fs::create_dir_all(&tasks).unwrap();
        // The live task sorts both before and after a completed one.
        fs::write(tasks.join("1.json"),r#"{"status":"completed","owner":"coder","subject":"old"}"#).unwrap();
        fs::write(tasks.join("2.json"),r#"{"status":"in_progress","owner":"coder","subject":"current"}"#).unwrap();
        fs::write(tasks.join("3.json"),r#"{"status":"completed","owner":"coder","subject":"older"}"#).unwrap();
        let s=snapshot(&home,"lead",Path::new("/some-project"),SystemTime::now());
        assert_eq!(s.agents[0].state,"WORKING");
        assert_eq!(s.agents[0].task,"current");
        fs::remove_dir_all(home).unwrap();
    }
    #[test] fn background_subagents_are_visible_but_not_native_team() {
        let home=temp_fixture();
        let session="session-with-workers";
        let dir=home.join("projects").join("-F-Projects-Forgeboard").join(session).join("subagents");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("agent-abc123.meta.json"), r#"{"agentType":"general-purpose","name":"coder-frontend","description":"Build React UI"}"#).unwrap();
        fs::write(dir.join("agent-abc123.jsonl"), "{\"type\":\"assistant\"}\n").unwrap();
        let result=snapshot(&home,session,Path::new("/some-project"),SystemTime::now());
        assert!(result.detected && !result.verified);
        assert_eq!(result.agents.len(),1);
        assert_eq!(result.agents[0].name,"coder-frontend");
        assert_eq!(result.agents[0].task,"Build React UI");
        assert_eq!(result.agents[0].state,"RECENT OUTPUT");
        let other=snapshot(&home,"other-session",Path::new("/some-project"),SystemTime::now());
        assert!(!other.detected);
        fs::remove_dir_all(home).unwrap();
    }
    #[test] fn a_verified_team_takes_precedence_over_subagents() {
        let home=temp_fixture();
        let session="abc-def";
        let p=home.join("teams").join("native");fs::create_dir_all(&p).unwrap();
        fs::write(p.join("config.json"),r#"{"leadSessionId":"abc-def","members":[{"name":"coder","model":"claude-haiku-5-5"}]}"#).unwrap();
        let dir=home.join("projects").join("foo").join(session).join("subagents");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("agent-xyz.meta.json"),r#"{"description":"Extra"}"#).unwrap();
        let result=snapshot(&home,session,Path::new("/some-project"),SystemTime::now());
        assert!(result.verified);
        assert_eq!(result.agents[0].state,"TEAM MEMBER");
        fs::remove_dir_all(home).unwrap();
    }
    #[test] fn associates_forgeboard_native_team_by_cwd_and_start_time() {
        let home=temp_fixture();
        let project=home.join("Forgeboard"); fs::create_dir_all(&project).unwrap();
        let p=home.join("teams").join("session-a080794e"); fs::create_dir_all(&p).unwrap();
        let json=serde_json::json!({
            "leadSessionId":"a080794e-2ec6-4bbf-bb69-1f66fc5540b5",
            "members":[
                {"name":"team-lead","agentType":"team-lead","cwd":project.display().to_string()},
                {"name":"coder-backend","agentId":"coder-backend@session-a080794e"},
                {"name":"explorer-tests","agentId":"explorer-tests@session-a080794e"},
                {"name":"coder-frontend","agentId":"coder-frontend@session-a080794e"},
            ]
        });
        fs::write(p.join("config.json"),json.to_string()).unwrap();
        let result=snapshot(&home,"2234c448-3556-461a-81cd-d89101c4136c",&project,
            SystemTime::now()-Duration::from_secs(30));
        assert!(result.detected && result.project_linked && !result.verified);
        assert_eq!(result.team_name,"session-a080794e");
        assert_eq!(result.claude_lead_session_id,"a080794e-2ec6-4bbf-bb69-1f66fc5540b5");
        assert_eq!(result.agents.len(),3);
        fs::remove_dir_all(home).unwrap();
    }
    #[test] fn transcript_can_link_team_when_config_lacks_cwd() {
        let home=temp_fixture();
        let project=home.join("Forgeboard"); fs::create_dir_all(&project).unwrap();
        let p=home.join("teams").join("session-other"); fs::create_dir_all(&p).unwrap();
        fs::write(p.join("config.json"),r#"{"leadSessionId":"other-lead","members":[{"name":"coder"}]}"#).unwrap();
        let t=home.join("projects").join("-mock-F-Projects-Forgeboard"); fs::create_dir_all(&t).unwrap();
        fs::write(t.join("other-lead.jsonl"),serde_json::json!({"type":"user","cwd":project.display().to_string()}).to_string()).unwrap();
        let result=snapshot(&home,"app-generated-id",&project,SystemTime::now()-Duration::from_secs(30));
        assert!(result.project_linked);
        fs::remove_dir_all(home).unwrap();
    }
    #[test] fn unrelated_new_team_in_other_project_is_not_linked() {
        let home=temp_fixture();
        let p=home.join("teams").join("other-team"); fs::create_dir_all(&p).unwrap();
        fs::write(p.join("config.json"),r#"{"leadSessionId":"different-lead","members":[{"name":"team-lead","agentType":"team-lead","cwd":"/unrelated-project"},{"name":"coder"}]}"#).unwrap();
        let result=snapshot(&home,"our-session",Path::new("/our-project"),SystemTime::now()-Duration::from_secs(30));
        assert!(!result.detected);
        fs::remove_dir_all(home).unwrap();
    }
    #[test] fn two_new_teams_in_same_project_are_ambiguous() {
        let home=temp_fixture();
        let project=home.join("same-project");fs::create_dir_all(&project).unwrap();
        for i in 0..2 {
            let p=home.join("teams").join(format!("team-{i}"));fs::create_dir_all(&p).unwrap();
            fs::write(p.join("config.json"),serde_json::json!({
                "leadSessionId":format!("lead-{i}"),
                "members":[{"name":"team-lead","agentType":"team-lead","cwd":project.display().to_string()}]
            }).to_string()).unwrap();
        }
        let result=snapshot(&home,"unmatched",&project,SystemTime::now()-Duration::from_secs(30));
        assert!(!result.detected);
        assert!(result.discovery.contains("ambiguous"));
        fs::remove_dir_all(home).unwrap();
    }

}
