//! Read-only, incremental observer for Claude Code's local JSONL transcripts.
//! This is not a billing API: figures are partial, approximate and may be absent
//! if Claude changes its private transcript schema or disables local recording.
use std::{collections::{BTreeMap, HashMap, HashSet, VecDeque}, fs, io::{BufRead, BufReader, Seek, SeekFrom}, path::{Path, PathBuf}, time::{Duration, SystemTime}};
use serde_json::Value;
use crate::team::TeamSnapshot;

const CONTEXT_WINDOW: u64 = 1_000_000; // Haiku/Opus 5.5 published context window
const MAX_EVENTS: usize = 36;

#[derive(Clone, Debug, Default)]
pub struct Totals {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub calls: u64,
    pub tools: u64,
    pub list_usd: f64,
    pub context_latest: u64,
    pub model: String,
    pub action: String,
    pub last_timestamp: String,
    pub first_timestamp: String,
    pub has_pricing_gap: bool,
    pub opus_calls: u64,
    pub last_activity: Option<SystemTime>,
}
impl Totals {
    pub fn tokens(&self) -> u64 { self.input + self.output + self.cache_read + self.cache_write }
    pub fn context_percent(&self) -> f64 { (self.context_latest as f64 * 100. / CONTEXT_WINDOW as f64).min(100.) }
    fn merge(&mut self, other: &Totals) {
        self.input += other.input; self.output += other.output;
        self.cache_read += other.cache_read; self.cache_write += other.cache_write;
        self.calls += other.calls; self.tools += other.tools;
        self.list_usd += other.list_usd;
        self.has_pricing_gap |= other.has_pricing_gap;
        self.opus_calls += other.opus_calls;
        if let Some(t)=other.last_activity {
            if self.last_activity.is_none_or(|old|t>old) {self.last_activity=Some(t);}
        }
    }
}
#[derive(Clone, Debug)]
pub struct Activity { pub who:String, pub what:String, pub time:String }
#[derive(Default)]
struct Trace {
    offset: u64,
    seen_responses: HashSet<String>,
    seen_tools: HashSet<String>,
    seen_advisor_blocks: HashSet<String>,
    seen_advisor_results: HashSet<String>,
    seen_advisor_iterations: HashSet<String>,
    advisor_invocations: u64,
    advisor_completed: u64,
    advisor: Totals,
    totals: Totals,
    role: String,
    review_started: bool,
    review_result: Option<String>,
    objective_complete: bool,
    advisor_completed_before_review: u64,
    advisor_usage_before_review: u64,
}
#[derive(Clone,Debug,Default)]
pub struct ReviewState {
    pub started: bool,
    pub result: Option<String>,
    pub objective_complete: bool,
    // Advisor evidence means that an Opus result was observed AFTER the
    // review-start marker. A textual claim alone is not a verified review.
    pub advisor_evidence: bool,
}
#[derive(Default)]
pub struct Telemetry {
    files: HashMap<PathBuf, Trace>,
    pub actors: BTreeMap<String, Totals>,
    pub combined: Totals,
    pub advisor: Totals,
    pub advisor_invocations: u64,
    pub advisor_completed: u64,
    pub events: VecDeque<Activity>,
    pub files_found: usize,
    pub unattributed_workers: usize,
    pub coverage: String,
    pub observed_opus: bool,
    pub review: ReviewState,
}

fn u64_field(v:&Value,k:&str)->u64 { v.get(k).and_then(Value::as_u64).unwrap_or(0) }
fn safe_action(v:&Value) -> String {
    let name=v.get("name").and_then(Value::as_str).unwrap_or("Tool");
    let arg=v.get("input").unwrap_or(&Value::Null);
    // Deliberately don't render arbitrary command lines or content, which
    // could expose secrets to the dashboard/scrollback.
    let target=["file_path","path","pattern","url"].iter()
        .find_map(|k|arg.get(*k).and_then(Value::as_str))
        .and_then(|p|p.rsplit(['/', '\\']).next()).unwrap_or("");
    let abbreviated: String=target.chars().take(38).collect();
    if abbreviated.is_empty(){name.to_owned()}else{format!("{} {}",name,abbreviated)}
}
fn pricing(model:&str, input:u64, output:u64, cache5:u64, cache1:u64, read:u64, ctx:u64)->Option<f64>{
    // October 2026 published USD API list rates / million tokens.
    // Haiku is long-context-priced when an individual prompt exceeds 100k.
    let (i,o,w5,w1,r)=if model.contains("haiku-5-5"){
        if ctx>100_000 {(0.50,2.50,0.625,1.00,0.05)}
        else {(0.10,0.50,0.125,0.20,0.01)}
    }else if model.contains("opus-5-5") {
        (4.0,20.0,5.0,8.0,0.20)
    }else if model.contains("sonnet-5-5"){
        (2.0,10.0,2.50,4.0,0.20)
    }else {return None};
    Some((input as f64*i + output as f64*o + cache5 as f64*w5
        + cache1 as f64*w1 + read as f64*r)/1_000_000.0)
}
fn is_fresh(path:&Path, started:SystemTime)->bool {
    let earliest=started.checked_sub(Duration::from_secs(15)).unwrap_or(started);
    path.metadata().ok().and_then(|m|m.modified().ok()).is_some_and(|t|t>=earliest)
}
fn recorded_name(meta:&Path)->Option<String>{
    let s=fs::read_to_string(meta).ok()?;
    let v:Value=serde_json::from_str(&s).ok()?;
    ["name","agentName"].iter().find_map(|k|v.get(*k).and_then(Value::as_str)).map(str::to_owned)
}
// Infer a role only from explicit identity fields in transcript records.
// If no such metadata exists, keep the work unattributed rather than making up
// precise per-teammate token counts.
fn labelled_agent(path:&Path, names:&[String])->Option<String>{
    let filename=path.file_name()?.to_string_lossy();
    let id=filename.strip_prefix("agent-")?.strip_suffix(".jsonl")?;
    let meta=path.with_file_name(format!("agent-{}.meta.json",id));
    if let Some(name)=recorded_name(&meta) {if names.contains(&name){return Some(name);}}
    let file=fs::File::open(path).ok()?;
    for line in BufReader::new(file).lines().take(12).flatten(){
        if let Ok(v)=serde_json::from_str::<Value>(&line){
            for key in ["agentName","agent_name","name"] {
                if let Some(name)=v.get(key).and_then(Value::as_str) {
                    if names.iter().any(|n|n==name){return Some(name.into());}
                }
            }
        }
    }
    None
}
impl Telemetry {
    pub fn reset(&mut self) { *self=Self::default(); }
    pub fn refresh(&mut self,home:&Path,requested_session:&str,team:&TeamSnapshot,started:SystemTime,run_token:&str){
        let lead=if !team.claude_lead_session_id.is_empty(){team.claude_lead_session_id.as_str()}else{requested_session};
        let names:Vec<String>=team.agents.iter().map(|x|x.name.clone()).collect();
        let mut paths=Vec::<(PathBuf,String)>::new();
        if let Ok(projects)=fs::read_dir(home.join("projects")) {
            for dir in projects.flatten() {
                let dir=dir.path();
                let mut ids = vec![(lead.to_owned(),"main".to_owned()),(requested_session.to_owned(),"main".to_owned())];
                ids.extend(team.agents.iter().filter(|a| !a.session_id.is_empty()).map(|a|(a.session_id.clone(),a.name.clone())));
                ids.sort();ids.dedup();
                for (id, actor) in ids {
                    let root=dir.join(format!("{}.jsonl",id));
                    if root.is_file() && is_fresh(&root,started) && !paths.iter().any(|(p,_)|p==&root){
                        paths.push((root,actor.clone()));
                    }
                    let sub=dir.join(&id).join("subagents");
                    if let Ok(entries)=fs::read_dir(sub){
                        for entry in entries.flatten() {
                            let p=entry.path();
                            if p.extension().and_then(|s|s.to_str())!=Some("jsonl") || !is_fresh(&p,started){continue;}
                            let role=labelled_agent(&p,&names).unwrap_or_else(||"unattributed".into());
                            if !paths.iter().any(|(prev,_)|prev==&p){paths.push((p,role));}
                        }
                    }
                }
            }
        }
        for (path, role) in paths {
            let trace=self.files.entry(path.clone()).or_default();
            trace.role=role.clone();
            let mut fresh_events=Vec::new();
            let prior_offset=trace.offset;
            let _=consume_trace(&path,trace,&role,&mut fresh_events,run_token);
            if trace.offset != prior_offset {
                trace.totals.last_activity=path.metadata().ok().and_then(|m|m.modified().ok());
            }
            for e in fresh_events {self.events.push_back(e);}
            while self.events.len()>MAX_EVENTS {self.events.pop_front();}
        }
        self.actors.clear();self.combined=Totals::default();self.advisor=Totals::default();
        self.advisor_invocations=0;self.advisor_completed=0;self.files_found=self.files.len();
        self.unattributed_workers=0;self.observed_opus=false;
        self.review=ReviewState::default();
        for trace in self.files.values() {
            let entry=self.actors.entry(trace.role.clone()).or_default();
            entry.merge(&trace.totals);
            // Prefer a trace with the newest action when multiple sources share a role.
            if !trace.totals.last_timestamp.is_empty() && trace.totals.last_timestamp>=entry.last_timestamp {
                entry.action=trace.totals.action.clone();
                entry.model=trace.totals.model.clone();
                entry.last_timestamp=trace.totals.last_timestamp.clone();
                entry.context_latest=trace.totals.context_latest;
            }
            self.combined.merge(&trace.totals);
            // The API's top-level usage excludes advisor inferences. Include
            // advisor iterations separately so the run total reflects both.
            self.combined.merge(&trace.advisor);
            self.advisor.merge(&trace.advisor);
            self.advisor_invocations+=trace.advisor_invocations;
            self.advisor_completed+=trace.advisor_completed;
            if trace.role=="main" && trace.review_started {
                self.review.started=true;
                self.review.objective_complete |= trace.objective_complete;
                if trace.review_result.is_some(){self.review.result=trace.review_result.clone();}
                self.review.advisor_evidence |= trace.advisor_completed>trace.advisor_completed_before_review
                    || trace.advisor.calls>trace.advisor_usage_before_review;
            }
            if trace.role=="unattributed"{self.unattributed_workers+=1;}
            if trace.totals.opus_calls>0 || trace.advisor_invocations>0 || trace.advisor.calls>0 {self.observed_opus=true;}
        }
        // Some files may be absent until Claude flushes its transcript.
        self.coverage=if self.files.is_empty(){"No local JSONL telemetry yet".into()}
            else if self.unattributed_workers>0 {format!("{} trace(s); {} worker file(s) not mapped to names",self.files_found,self.unattributed_workers)}
            else {format!("{} linked JSONL trace(s)",self.files_found)};
    }
}
fn consume_trace(path:&Path, trace:&mut Trace,role:&str,events:&mut Vec<Activity>,run_token:&str)->std::io::Result<()> {
    let mut f=fs::File::open(path)?;
    let len=f.metadata()?.len();
    if len<trace.offset {trace.offset=0; trace.seen_responses.clear(); trace.seen_tools.clear();
        trace.seen_advisor_blocks.clear();trace.seen_advisor_results.clear();
        trace.seen_advisor_iterations.clear();trace.advisor_invocations=0;
        trace.advisor_completed=0;trace.advisor=Totals::default();
        trace.review_started=false;trace.review_result=None;trace.objective_complete=false;
        trace.advisor_completed_before_review=0;trace.advisor_usage_before_review=0;
        trace.totals=Totals::default();}
    f.seek(SeekFrom::Start(trace.offset))?;
    let mut reader=BufReader::new(f);
    loop {
        let mut bytes=Vec::new();
        let n=reader.read_until(b'\n',&mut bytes)?;
        if n==0 {break;}
        // Don't advance over a partially written line.
        if !bytes.ends_with(b"\n"){break;}
        trace.offset+=n as u64;
        if bytes.len()>1_048_576 {continue;}
        let Ok(v)=serde_json::from_slice::<Value>(&bytes) else {continue;};
        if v.get("type").and_then(Value::as_str)!=Some("assistant") {continue;}
        let msg=&v["message"];
        let model=msg.get("model").and_then(Value::as_str).unwrap_or("");
        let timestamp=v.get("timestamp").and_then(Value::as_str).unwrap_or("");
        if !timestamp.is_empty(){
            if trace.totals.first_timestamp.is_empty(){trace.totals.first_timestamp=timestamp.into();}
            trace.totals.last_timestamp=timestamp.into();
        }
        if !model.is_empty(){trace.totals.model=model.into();}
        if let Some(usage)=msg.get("usage") {
            if let Some(id)=msg.get("id").and_then(Value::as_str){
                if trace.seen_responses.insert(id.to_owned()){
                    let input=u64_field(usage,"input_tokens");
                    let output=u64_field(usage,"output_tokens");
                    let cached=u64_field(usage,"cache_read_input_tokens");
                    let create=u64_field(usage,"cache_creation_input_tokens");
                    let cache1=u64_field(&usage["cache_creation"],"ephemeral_1h_input_tokens");
                    let cache5=u64_field(&usage["cache_creation"],"ephemeral_5m_input_tokens");
                    let write1=cache1.min(create);
                    let write5=if cache5==0 && cache1==0 {create}else{cache5.min(create.saturating_sub(write1))};
                    let ctx=input+cached+create;
                    trace.totals.context_latest=ctx;
                    trace.totals.input+=input;trace.totals.output+=output;
                    trace.totals.cache_read+=cached;trace.totals.cache_write+=create;
                    trace.totals.calls+=1;
                    if model.contains("opus-5-5") {trace.totals.opus_calls+=1;}
                    if let Some(usd)=pricing(model,input,output,write5,write1,cached,ctx) {
                        trace.totals.list_usd+=usd;
                    } else {trace.totals.has_pricing_gap=true;}
                }
            }
        }
        // Advisor calls are server-side and are nested *inside* Haiku's
        // response. The executor model therefore stays Haiku even when Opus
        // advises. "advisor_message" iterations report separate billable use.
        if let Some(iterations)=msg.get("usage").and_then(|u|u.get("iterations")).and_then(Value::as_array) {
            if let Some(id)=msg.get("id").and_then(Value::as_str) {
                for (index,iteration) in iterations.iter().enumerate() {
                    if iteration.get("type").and_then(Value::as_str)!=Some("advisor_message"){continue;}
                    if !trace.seen_advisor_iterations.insert(format!("{id}:{index}")){continue;}
                    let model=iteration.get("model").and_then(Value::as_str).unwrap_or("");
                    let input=u64_field(iteration,"input_tokens");
                    let output=u64_field(iteration,"output_tokens");
                    let read=u64_field(iteration,"cache_read_input_tokens");
                    let create=u64_field(iteration,"cache_creation_input_tokens");
                    let cache1=u64_field(&iteration["cache_creation"],"ephemeral_1h_input_tokens").min(create);
                    let cache5=if cache1==0 {create}else{create.saturating_sub(cache1)};
                    trace.advisor.calls+=1;
                    trace.advisor.input+=input;trace.advisor.output+=output;
                    trace.advisor.cache_read+=read;trace.advisor.cache_write+=create;
                    trace.advisor.context_latest=input+read+create;
                    if model.contains("opus"){trace.advisor.opus_calls+=1;}
                    if !model.is_empty(){trace.advisor.model=model.to_owned();}
                    trace.advisor.last_timestamp=timestamp.to_owned();
                    if let Some(price)=pricing(model,input,output,cache5,cache1,read,input+read+create){
                        trace.advisor.list_usd+=price;
                    } else {trace.advisor.has_pricing_gap=true;}
                }
            }
        }
        if let Some(blocks)=msg.get("content").and_then(Value::as_array){
            // The lead emits protocol markers as standalone assistant text.
            // This avoids treating an ordinary advisor consultation as a
            // successful final audit, or treating an idle terminal as done.
            if role=="main" && !run_token.is_empty() {
                for block in blocks {
                    if block.get("type").and_then(Value::as_str)!=Some("text"){continue;}
                    let body=block.get("text").and_then(Value::as_str).unwrap_or("");
                    for line in body.lines().map(str::trim) {
                        if line==format!("AGENTREE_REVIEW_START:{run_token}")
                            && (!trace.review_started || trace.review_result.is_some()) {
                            trace.review_started=true;
                            trace.review_result=None;
                            trace.objective_complete=false;
                            trace.advisor_completed_before_review=trace.advisor_completed;
                            trace.advisor_usage_before_review=trace.advisor.calls;
                            events.push(Activity{who:"Opus final review".into(),what:"Review stage requested by lead".into(),time:timestamp.chars().skip(11).take(8).collect()});
                        }
                        if trace.review_started {
                            let prefix=format!("AGENTREE_REVIEW_RESULT:{run_token}:");
                            if let Some(status)=line.strip_prefix(&prefix) {
                                if ["PASS","NEEDS_CHANGES","BLOCKED"].contains(&status) {
                                    trace.review_result=Some(status.to_owned());
                                }
                            }
                            if line==format!("AGENTREE_OBJECTIVE_COMPLETE:{run_token}") {
                                trace.objective_complete=true;
                                events.push(Activity{who:"main".into(),what:"Objective marked complete".into(),time:timestamp.chars().skip(11).take(8).collect()});
                            }
                        }
                    }
                }
            }
            for block in blocks {
                let kind=block.get("type").and_then(Value::as_str).unwrap_or("");
                let use_id=block.get("id").or_else(||block.get("tool_use_id"))
                    .and_then(Value::as_str).unwrap_or("");
                if kind=="server_tool_use" && block.get("name").and_then(Value::as_str)==Some("advisor") {
                    if !use_id.is_empty() && trace.seen_advisor_blocks.insert(use_id.to_owned()) {
                        trace.advisor_invocations+=1;
                        events.push(Activity{who:"Opus advisor".into(),what:"Consultation invoked".into(),time:timestamp.chars().skip(11).take(8).collect()});
                    }
                }
                if kind=="advisor_tool_result" && !use_id.is_empty() && trace.seen_advisor_results.insert(use_id.to_owned()) {
                    // Some local transcript revisions omit the preceding server_tool_use.
                    if trace.seen_advisor_blocks.insert(use_id.to_owned()){trace.advisor_invocations+=1;}
                    trace.advisor_completed+=1;
                    events.push(Activity{who:"Opus advisor".into(),what:"Consultation completed".into(),time:timestamp.chars().skip(11).take(8).collect()});
                }
                if kind=="tool_use" {
                    let id=block.get("id").and_then(Value::as_str).map(str::to_owned);
                    if let Some(id)=id {if !trace.seen_tools.insert(id) {continue;}}
                    trace.totals.tools+=1;
                    let action=safe_action(block);
                    trace.totals.action=action.clone();
                    events.push(Activity{who:role.into(),what:action,time:timestamp.chars().skip(11).take(8).collect()});
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]mod tests {
    use super::*;
    #[test]fn dedupes_usage_but_tracks_distinct_tool_calls(){
        let tmp=std::env::temp_dir().join(format!("telemetry-test-{}.jsonl",uuid::Uuid::new_v4()));
        let line=serde_json::json!({"type":"assistant","timestamp":"2026-10-10T01:00:00Z","message":{
            "id":"msg_A","model":"claude-haiku-5-5","usage":{"input_tokens":100,"output_tokens":10,
            "cache_read_input_tokens":500,"cache_creation_input_tokens":20},
            "content":[{"type":"tool_use","id":"tool_1","name":"Read","input":{"file_path":"C:/repo/src/main.rs"}}]}}).to_string();
        fs::write(&tmp,format!("{line}\n{line}\n")).unwrap();
        let mut trace=Trace::default();let mut events=Vec::new();
        consume_trace(&tmp,&mut trace,"main",&mut events,"run-123").unwrap();
        assert_eq!(trace.totals.calls,1);
        assert_eq!(trace.totals.tokens(),630);
        assert_eq!(trace.totals.context_latest,620);
        assert_eq!(trace.totals.tools,1); // same tool id must not count twice
        assert!(events[0].what.contains("main.rs"));
        fs::remove_file(tmp).unwrap();
    }
    #[test]fn uses_opus_pricing_and_long_context_haiku(){
        assert!((pricing("claude-opus-5-5",1_000_000,1_000_000,0,0,0,100).unwrap()-24.0).abs()<0.001);
        assert!((pricing("claude-haiku-5-5",1_000_000,0,0,0,0,150_000).unwrap()-0.5).abs()<0.001);
    }
    #[test]fn partial_line_is_retried(){
        let tmp=std::env::temp_dir().join(format!("telemetry-partial-{}.jsonl",uuid::Uuid::new_v4()));
        fs::write(&tmp,b"{\"type\":\"assistant\"").unwrap();
        let mut trace=Trace::default();let mut events=Vec::new();
        consume_trace(&tmp,&mut trace,"main",&mut events,"run-123").unwrap();assert_eq!(trace.offset,0);
        fs::write(&tmp,b"{\"type\":\"assistant\"}\n").unwrap();
        consume_trace(&tmp,&mut trace,"main",&mut events,"run-123").unwrap();
        assert!(trace.offset>0);fs::remove_file(tmp).unwrap();
    }
    #[test]fn links_named_teammate_session_and_counts_distinct_calls(){
        let root=std::env::temp_dir().join(format!("telemetry-native-{}",uuid::Uuid::new_v4()));
        let p=root.join("projects").join("-F-Forgeboard");
        fs::create_dir_all(&p).unwrap();
        let record=serde_json::json!({"type":"assistant","timestamp":"2026-10-10T01:00:00Z",
            "message":{"id":"m1","model":"claude-haiku-5-5","usage":{
                "input_tokens":100,"output_tokens":50,"cache_read_input_tokens":0,"cache_creation_input_tokens":0},
                "content":[{"type":"tool_use","id":"tool42","name":"Read","input":{"file_path":"src/main.rs"}}]}}).to_string();
        fs::write(p.join("lead-1.jsonl"),format!("{record}\n")).unwrap();
        let worker_record=record.replace("\"m1\"","\"m2\"");
        fs::write(p.join("worker-1.jsonl"),format!("{worker_record}\n")).unwrap();
        let mut team=TeamSnapshot::default();
        team.claude_lead_session_id="lead-1".into();
        team.agents.push(crate::team::Agent{name:"coder-frontend".into(),session_id:"worker-1".into(),..Default::default()});
        let mut obs=Telemetry::default();
        obs.refresh(&root,"requested-id",&team,SystemTime::now(),"run-123");
        assert_eq!(obs.combined.calls,2);
        assert_eq!(obs.actors["main"].tools,1);
        assert_eq!(obs.actors["coder-frontend"].tools,1);
        obs.refresh(&root,"requested-id",&team,SystemTime::now(),"run-123");
        assert_eq!(obs.combined.calls,2); // must not count the same file twice
        fs::remove_dir_all(root).unwrap();
    }
    #[test]fn advisor_is_counted_inside_haiku_response_without_double_billing(){
        let tmp=std::env::temp_dir().join(format!("advisor-test-{}.jsonl",uuid::Uuid::new_v4()));
        let line=serde_json::json!({"type":"assistant","timestamp":"2026-10-10T01:02:00Z",
          "message":{"id":"msg_advice_1","model":"claude-haiku-5-5",
          "usage":{"input_tokens":100,"output_tokens":20,"cache_read_input_tokens":50,"cache_creation_input_tokens":0,
            "iterations":[{"type":"message","input_tokens":100,"output_tokens":20},
              {"type":"advisor_message","model":"claude-opus-5-5","input_tokens":200,"output_tokens":80,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}]},
          "content":[{"type":"server_tool_use","id":"srv_1","name":"advisor","input":{}},
            {"type":"advisor_tool_result","tool_use_id":"srv_1","content":{"type":"advisor_redacted_result"}}]}}).to_string();
        fs::write(&tmp,format!("{line}\n{line}\n")).unwrap();
        let mut trace=Trace::default();let mut events=Vec::new();
        consume_trace(&tmp,&mut trace,"main",&mut events,"run-123").unwrap();
        assert_eq!(trace.totals.tokens(),170);
        assert_eq!(trace.advisor.tokens(),280);
        assert_eq!(trace.advisor.calls,1);
        assert_eq!(trace.advisor_invocations,1);
        assert_eq!(trace.advisor_completed,1);
        assert_eq!(trace.totals.opus_calls,0);
        assert_eq!(events.len(),2);
        fs::remove_file(tmp).unwrap();
    }
    #[test]fn advisor_usage_can_appear_after_first_record_of_same_response(){
        let tmp=std::env::temp_dir().join(format!("advisor-update-{}.jsonl",uuid::Uuid::new_v4()));
        let initial=serde_json::json!({"type":"assistant","message":{"id":"msg_same","model":"claude-haiku-5-5",
          "usage":{"input_tokens":7,"output_tokens":3},"content":[]}}).to_string();
        let final_record=serde_json::json!({"type":"assistant","message":{"id":"msg_same","model":"claude-haiku-5-5",
          "usage":{"input_tokens":7,"output_tokens":3,"iterations":[{"type":"advisor_message","model":"claude-opus-5-5","input_tokens":9,"output_tokens":2}]},"content":[]}}).to_string();
        fs::write(&tmp,format!("{initial}\n{final_record}\n")).unwrap();
        let mut trace=Trace::default();let mut events=Vec::new();
        consume_trace(&tmp,&mut trace,"main",&mut events,"run-123").unwrap();
        assert_eq!(trace.totals.tokens(),10);
        assert_eq!(trace.advisor.tokens(),11);
        fs::remove_file(tmp).unwrap();
    }

    #[test]fn review_requires_opus_after_marker_and_explicit_completion(){
        let tmp=std::env::temp_dir().join(format!("review-{}.jsonl",uuid::Uuid::new_v4()));
        let line=|id:&str,content:Value|serde_json::json!({"type":"assistant","timestamp":"2026-10-10T02:00:00Z","message":{
            "id":id,"model":"claude-haiku-5-5","usage":{"input_tokens":1,"output_tokens":1},"content":content}}).to_string();
        let earlier=line("advice",serde_json::json!([
            {"type":"server_tool_use","id":"srv_before","name":"advisor"},
            {"type":"advisor_tool_result","tool_use_id":"srv_before"}]));
        let start=line("start",serde_json::json!([{"type":"text","text":"AGENTREE_REVIEW_START:run-123"}]));
        let final_advice=line("review",serde_json::json!([
            {"type":"server_tool_use","id":"srv_after","name":"advisor"},
            {"type":"advisor_tool_result","tool_use_id":"srv_after"}]));
        let finished=line("finish",serde_json::json!([{"type":"text",
            "text":"AGENTREE_REVIEW_RESULT:run-123:PASS\nAGENTREE_OBJECTIVE_COMPLETE:run-123"}]));
        fs::write(&tmp,format!("{earlier}\n{start}\n{final_advice}\n{finished}\n")).unwrap();
        let mut trace=Trace::default(); let mut events=Vec::new();
        consume_trace(&tmp,&mut trace,"main",&mut events,"run-123").unwrap();
        assert!(trace.review_started);
        assert_eq!(trace.review_result.as_deref(),Some("PASS"));
        assert!(trace.objective_complete);
        assert_eq!(trace.advisor_completed-trace.advisor_completed_before_review,1);
        // A background agent must not be able to mark the lead objective done.
        let mut worker=Trace::default();
        consume_trace(&tmp,&mut worker,"coder",&mut events,"run-123").unwrap();
        assert!(!worker.objective_complete);
        fs::remove_file(tmp).unwrap();
    }
    #[test]fn markers_are_scoped_to_current_run(){
        let tmp=std::env::temp_dir().join(format!("marker-scope-{}.jsonl",uuid::Uuid::new_v4()));
        fs::write(&tmp,serde_json::json!({"type":"assistant","message":{
            "content":[{"type":"text","text":"AGENTREE_REVIEW_START:old-run\nAGENTREE_REVIEW_RESULT:old-run:PASS\nAGENTREE_OBJECTIVE_COMPLETE:old-run"}]}}).to_string()+"\n").unwrap();
        let mut trace=Trace::default();let mut events=Vec::new();
        consume_trace(&tmp,&mut trace,"main",&mut events,"run-123").unwrap();
        assert!(!trace.review_started);
        assert!(!trace.objective_complete);
        fs::remove_file(tmp).unwrap();
    }

}
