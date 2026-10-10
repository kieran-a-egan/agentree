//! Read-only, incremental observer for Claude Code's local JSONL transcripts.
//! This is not a billing API: figures are partial, approximate and may be absent
//! if Claude changes its private transcript schema or disables local recording.
use std::{collections::{BTreeMap, HashMap, HashSet, VecDeque}, fs, io::{BufRead, BufReader, Seek, SeekFrom}, path::{Path, PathBuf}, time::{Duration, SystemTime}};
use serde_json::Value;
use crate::model::{self, EventKind, Measure, Timestamp};
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
    pub has_pricing_gap: bool,
    pub opus_calls: u64,
}
impl Totals {
    pub fn tokens(&self) -> u64 { self.input + self.output + self.cache_read + self.cache_write }
    /// Runtime-neutral view. With no observed calls every field is Unknown, not zero.
    /// A partial cost (an unpriced model was seen) is reported as Unknown.
    pub fn usage(&self) -> model::Usage {
        if self.calls == 0 {return model::Usage::default();}
        model::Usage {
            input_tokens: Measure::Observed(self.input),
            output_tokens: Measure::Observed(self.output),
            cache_read_tokens: Measure::Observed(self.cache_read),
            cache_write_tokens: Measure::Observed(self.cache_write),
            cost_usd: if self.has_pricing_gap {Measure::Unknown} else {Measure::Observed(self.list_usd)},
        }
    }
    pub fn context_percent(&self) -> f64 { (self.context_latest as f64 * 100. / CONTEXT_WINDOW as f64).min(100.) }
    fn merge(&mut self, other: &Totals) {
        self.input += other.input; self.output += other.output;
        self.cache_read += other.cache_read; self.cache_write += other.cache_write;
        self.calls += other.calls; self.tools += other.tools;
        self.list_usd += other.list_usd;
        self.has_pricing_gap |= other.has_pricing_gap;
        self.opus_calls += other.opus_calls;
    }
}
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
impl ReviewState {
    /// Final-review stage derived from run-scoped markers. Not started means Pending.
    #[allow(dead_code)] // Consumed by the v0.6 run snapshot; covered by tests.
    pub fn stage(&self, run: &model::Id) -> model::Stage {
        use model::StageStatus::*;
        let status = match self.result.as_deref() {
            Some("PASS") => Passed,
            Some("NEEDS_CHANGES") => NeedsChanges,
            Some("BLOCKED") => Blocked,
            _ if self.started => Running,
            _ => Pending,
        };
        model::Stage { id: model::Id::new(format!("{run}/final-review")), run_id: run.clone(),
            kind: model::StageKind::FinalReview, status }
    }
}
#[derive(Default)]
pub struct Telemetry {
    files: HashMap<PathBuf, Trace>,
    pub actors: BTreeMap<String, Totals>,
    pub combined: Totals,
    pub advisor: Totals,
    pub advisor_invocations: u64,
    pub advisor_completed: u64,
    pub events: VecDeque<model::Event>,
    pub files_found: usize,
    pub unattributed_workers: usize,
    pub coverage: String,
    pub observed_opus: bool,
    pub review: ReviewState,
}

fn u64_field(v:&Value,k:&str)->u64 { v.get(k).and_then(Value::as_u64).unwrap_or(0) }
/// Tool name and a sanitised target label. Deliberately never renders arbitrary
/// command lines or content, which could expose secrets to the dashboard.
fn safe_tool(v:&Value) -> (String,String) {
    let name=v.get("name").and_then(Value::as_str).unwrap_or("Tool");
    let arg=v.get("input").unwrap_or(&Value::Null);
    let target=["file_path","path","pattern","url"].iter()
        .find_map(|k|arg.get(*k).and_then(Value::as_str))
        .and_then(|p|p.rsplit(['/', '\\']).next()).unwrap_or("");
    (name.to_owned(), target.chars().take(38).collect())
}
/// Parses the `YYYY-MM-DDTHH:MM:SS[.fff]Z` form Claude writes into transcripts.
/// Anything else (offsets, missing Z, out-of-range fields) is None, not a guess.
fn parse_timestamp(text:&str) -> Option<Timestamp> {
    let b=text.as_bytes();
    if b.len()<20 || b[4]!=b'-' || b[7]!=b'-' || b[10]!=b'T' || b[13]!=b':' || b[16]!=b':' || b[b.len()-1]!=b'Z' {return None;}
    let num=|r:std::ops::Range<usize>| text.get(r)?.parse::<i64>().ok();
    let (y,mo,d,h,mi,s)=(num(0..4)?,num(5..7)?,num(8..10)?,num(11..13)?,num(14..16)?,num(17..19)?);
    let leap=(y%4==0 && y%100!=0) || y%400==0;
    let month_days=match mo {2=>if leap {29} else {28}, 4|6|9|11=>30, 1..=12=>31, _=>return None};
    if !(1..=month_days).contains(&d) || h>23 || mi>59 || s>59 {return None;}
    let fraction=text.get(19..text.len()-1)?;
    let millis=if fraction.is_empty() {0} else {
        let digits=fraction.strip_prefix('.')?;
        if digits.is_empty() || !digits.bytes().all(|c|c.is_ascii_digit()) {return None;}
        digits.chars().chain("000".chars()).take(3).collect::<String>().parse::<u64>().ok()?
    };
    // Howard Hinnant's days-from-civil, proleptic Gregorian, epoch 1970-01-01.
    let yy=if mo<=2 {y-1} else {y};
    let era=if yy>=0 {yy} else {yy-399}/400;
    let yoe=yy-era*400;
    let doy=(153*((mo+9)%12)+2)/5+d-1;
    let doe=yoe*365+yoe/4-yoe/100+doy;
    let days=era*146_097+doe-719_468;
    let seconds=days*86_400+h*3_600+mi*60+s;
    u64::try_from(seconds).ok().map(|s|s*1_000+millis)
}
/// Appends one core event. `line` is the transcript path and byte offset of the
/// record. The ID is `line#index`, where index counts this line's earlier events.
/// Events from one line are always appended consecutively, so this is stable
/// across refreshes and unaffected by how much else was read in the same batch.
fn emit(events:&mut Vec<model::Event>, line:&str, run:&str, actor:&str, agent:Option<&str>, at:Option<Timestamp>, kind:EventKind) {
    let index=events.iter().rev()
        .take_while(|e|e.id.as_str().strip_prefix(line).is_some_and(|rest|rest.starts_with('#')))
        .count();
    let id=model::Id::new(format!("{line}#{index}"));
    let agent=agent.map(|role|model::Id::new(format!("{run}/{role}")));
    events.push(model::Event::new(id,model::Id::new(run),agent,actor,at,kind));
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
fn add_usage(t:&mut Totals,usage:&Value,model:&str){
    let input=u64_field(usage,"input_tokens");
    let output=u64_field(usage,"output_tokens");
    let read=u64_field(usage,"cache_read_input_tokens");
    let create=u64_field(usage,"cache_creation_input_tokens");
    // Whatever cache write is not itemised as 1h is priced at the 5m rate.
    let write1=u64_field(&usage["cache_creation"],"ephemeral_1h_input_tokens").min(create);
    let ctx=input+read+create;
    t.context_latest=ctx;
    t.input+=input;t.output+=output;t.cache_read+=read;t.cache_write+=create;
    t.calls+=1;
    if model.contains("opus-5-5") {t.opus_calls+=1;}
    match pricing(model,input,output,create-write1,write1,read,ctx) {
        Some(usd)=>t.list_usd+=usd,
        None=>t.has_pricing_gap=true,
    }
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
    if let Some(name)=recorded_name(&meta).filter(|name|names.contains(name)) {return Some(name);}
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
    pub fn refresh(&mut self,home:&Path,requested_session:&str,team:&TeamSnapshot,started:SystemTime){
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
            let _=consume_trace(&path,trace,&role,&mut fresh_events,requested_session);
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
fn consume_trace(path:&Path, trace:&mut Trace,role:&str,events:&mut Vec<model::Event>,run_token:&str)->std::io::Result<()> {
    let mut f=fs::File::open(path)?;
    let len=f.metadata()?.len();
    if len<trace.offset {*trace=Trace{role:std::mem::take(&mut trace.role),..Default::default()};}
    f.seek(SeekFrom::Start(trace.offset))?;
    let mut reader=BufReader::new(f);
    loop {
        let mut bytes=Vec::new();
        let n=reader.read_until(b'\n',&mut bytes)?;
        if n==0 {break;}
        // Don't advance over a partially written line.
        if !bytes.ends_with(b"\n"){break;}
        let base=format!("{}@{}",path.display(),trace.offset);
        trace.offset+=n as u64;
        if bytes.len()>1_048_576 {continue;}
        let Ok(v)=serde_json::from_slice::<Value>(&bytes) else {continue;};
        if v.get("type").and_then(Value::as_str)!=Some("assistant") {continue;}
        let msg=&v["message"];
        let model=msg.get("model").and_then(Value::as_str).unwrap_or("");
        let timestamp=v.get("timestamp").and_then(Value::as_str).unwrap_or("");
        let at=parse_timestamp(timestamp);
        if !timestamp.is_empty(){trace.totals.last_timestamp=timestamp.into();}
        if !model.is_empty(){trace.totals.model=model.into();}
        if let Some(usage)=msg.get("usage") {
            if let Some(id)=msg.get("id").and_then(Value::as_str){
                if trace.seen_responses.insert(id.to_owned()){add_usage(&mut trace.totals,usage,model);}
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
                    add_usage(&mut trace.advisor,iteration,model);
                    if !model.is_empty(){trace.advisor.model=model.to_owned();}
                    trace.advisor.last_timestamp=timestamp.to_owned();
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
                            emit(events,&base,run_token,"Opus final review",None,at,EventKind::ReviewStarted);
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
                                emit(events,&base,run_token,"main",Some("main"),at,EventKind::ObjectiveCompleted);
                            }
                        }
                    }
                }
            }
            for block in blocks {
                let kind=block.get("type").and_then(Value::as_str).unwrap_or("");
                let use_id=block.get("id").or_else(||block.get("tool_use_id"))
                    .and_then(Value::as_str).unwrap_or("");
                if kind=="server_tool_use" && block.get("name").and_then(Value::as_str)==Some("advisor")
                    && !use_id.is_empty() && trace.seen_advisor_blocks.insert(use_id.to_owned()) {
                    trace.advisor_invocations+=1;
                    emit(events,&base,run_token,"Opus advisor",None,at,EventKind::AdvisorInvoked);
                }
                if kind=="advisor_tool_result" && !use_id.is_empty() && trace.seen_advisor_results.insert(use_id.to_owned()) {
                    // Some local transcript revisions omit the preceding server_tool_use.
                    if trace.seen_advisor_blocks.insert(use_id.to_owned()){trace.advisor_invocations+=1;}
                    trace.advisor_completed+=1;
                    emit(events,&base,run_token,"Opus advisor",None,at,EventKind::AdvisorCompleted);
                }
                if kind=="tool_use" {
                    let id=block.get("id").and_then(Value::as_str).map(str::to_owned);
                    if let Some(id)=id {if !trace.seen_tools.insert(id) {continue;}}
                    trace.totals.tools+=1;
                    let (tool,target)=safe_tool(block);
                    trace.totals.action=if target.is_empty(){tool.clone()}else{format!("{tool} {target}")};
                    // Unmapped workers stay unattributed: no invented agent ID that could merge them.
                    let agent=(role!="unattributed").then_some(role);
                    emit(events,&base,run_token,role,agent,at,EventKind::ToolUsed{tool,target});
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
        assert_eq!(events.len(),1);
        assert!(matches!(&events[0].kind,EventKind::ToolUsed{tool,target} if tool=="Read" && target=="main.rs"));
        assert_eq!(events[0].at,Some(1_791_594_000_000));
        assert_eq!(events[0].agent_id.as_ref().map(|id|id.as_str()),Some("run-123/main"));
        // Adapter output must survive the versioned wire format unchanged.
        let wire=serde_json::to_value(&events[0]).unwrap();
        assert_eq!(wire["schema_version"],1);
        assert_eq!(serde_json::from_value::<model::Event>(wire).unwrap(),events[0]);
        fs::remove_file(tmp).unwrap();
    }
    #[test]fn event_ids_are_stable_per_line_and_unattributed_has_no_agent(){
        let tmp=std::env::temp_dir().join(format!("event-ids-{}.jsonl",uuid::Uuid::new_v4()));
        let record=|id:&str|serde_json::json!({"type":"assistant","message":{"id":id,"model":"claude-haiku-5-5",
            "content":[{"type":"tool_use","id":format!("tool-{id}"),"name":"Read","input":{"file_path":"a.rs"}}]}}).to_string();
        fs::write(&tmp,format!("{}\n{}\n",record("m1"),record("m2"))).unwrap();
        let mut trace=Trace::default();let mut events=Vec::new();
        consume_trace(&tmp,&mut trace,"main",&mut events,"run-123").unwrap();
        let ids:Vec<&str>=events.iter().map(|e|e.id.as_str()).collect();
        assert_eq!(events.len(),2);
        assert!(ids[0].ends_with("#0") && ids[1].ends_with("#0"),"{ids:?}");
        assert_ne!(ids[0],ids[1]);
        // The same record read as part of a larger batch keeps its per-line index.
        let mut batch=Vec::new();
        let mut other=Trace::default();
        consume_trace(&tmp,&mut other,"main",&mut batch,"run-123").unwrap();
        assert_eq!(batch.iter().map(|e|e.id.as_str()).collect::<Vec<_>>(),ids);
        let mut worker=Trace::default();let mut unattributed=Vec::new();
        fs::write(&tmp,format!("{}\n",record("m3"))).unwrap();
        consume_trace(&tmp,&mut worker,"unattributed",&mut unattributed,"run-123").unwrap();
        assert_eq!(unattributed[0].actor,"unattributed");
        assert_eq!(unattributed[0].agent_id,None);
        fs::remove_file(tmp).unwrap();
    }
    #[test]fn parses_transcript_timestamps_strictly(){
        assert_eq!(parse_timestamp("2000-01-01T00:00:00Z"),Some(946_684_800_000));
        assert_eq!(parse_timestamp("2000-01-01T00:00:00.123Z"),Some(946_684_800_123));
        // 2000 is a leap year: 1 March is two days after 28 February.
        assert_eq!(parse_timestamp("2000-03-01T00:00:00Z").unwrap()-parse_timestamp("2000-02-28T00:00:00Z").unwrap(),2*86_400_000);
        // Offsets, missing Z, bad fields and garbage are unknown rather than guessed.
        for bad in ["2000-01-01T00:00:00+01:00","2000-01-01T00:00:00","2000-13-01T00:00:00Z","2000-01-01T25:00:00Z",
            "2000-01-01T00:00:00.Z","2001-02-29T00:00:00Z","2000-04-31T00:00:00Z","not a time"] {
            assert_eq!(parse_timestamp(bad),None,"{bad}");
        }
        assert!(parse_timestamp("2000-02-29T00:00:00Z").is_some());
    }
    #[test]fn unknown_usage_is_explicit_not_zero(){
        let empty=Totals::default();
        assert_eq!(empty.usage(),model::Usage::default());
        let mut priced=Totals{calls:1,input:10,output:2,list_usd:0.5,..Default::default()};
        assert_eq!(priced.usage().input_tokens,Measure::Observed(10));
        assert_eq!(priced.usage().cost_usd,Measure::Observed(0.5));
        priced.has_pricing_gap=true;
        assert_eq!(priced.usage().cost_usd,Measure::Unknown);
        assert_eq!(priced.usage().input_tokens,Measure::Observed(10));
    }
    #[test]fn final_review_stage_follows_markers(){
        let run=model::Id::new("run-123");
        let mut review=ReviewState::default();
        assert_eq!(review.stage(&run).status,model::StageStatus::Pending);
        review.started=true;
        assert_eq!(review.stage(&run).status,model::StageStatus::Running);
        review.result=Some("NEEDS_CHANGES".into());
        let stage=review.stage(&run);
        assert_eq!(stage.status,model::StageStatus::NeedsChanges);
        assert_eq!(stage.id.as_str(),"run-123/final-review");
    }
    #[test]fn uses_opus_pricing_and_long_context_haiku(){
        assert!((pricing("claude-opus-5-5",1_000_000,1_000_000,0,0,0,100).unwrap()-24.0).abs()<0.001);
        assert!((pricing("claude-haiku-5-5",1_000_000,0,0,0,0,150_000).unwrap()-0.5).abs()<0.001);
    }
    #[test]fn names_worker_from_transcript_when_meta_is_missing(){
        let dir=std::env::temp_dir().join(format!("telemetry-label-{}",uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let p=dir.join("agent-xyz.jsonl");
        fs::write(&p,"{\"type\":\"user\",\"agentName\":\"coder-frontend\"}\n").unwrap();
        let names=vec!["coder-frontend".to_string()];
        assert_eq!(labelled_agent(&p,&names).as_deref(),Some("coder-frontend"));
        assert_eq!(labelled_agent(&p,&["someone-else".to_string()]),None);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]fn older_opus_teammate_is_not_counted_as_advisor_evidence(){
        let mut t=Totals::default();
        add_usage(&mut t,&serde_json::json!({"input_tokens":1,"output_tokens":1}),"claude-opus-4-1");
        assert_eq!(t.opus_calls,0);
        add_usage(&mut t,&serde_json::json!({"input_tokens":1,"output_tokens":1}),"claude-opus-5-5");
        assert_eq!(t.opus_calls,1);
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
        let mut team=TeamSnapshot{claude_lead_session_id:"lead-1".into(),..Default::default()};
        team.agents.push(crate::team::Agent{name:"coder-frontend".into(),session_id:"worker-1".into(),..Default::default()});
        let mut obs=Telemetry::default();
        obs.refresh(&root,"requested-id",&team,SystemTime::now());
        assert_eq!(obs.combined.calls,2);
        assert_eq!(obs.actors["main"].tools,1);
        assert_eq!(obs.actors["coder-frontend"].tools,1);
        obs.refresh(&root,"requested-id",&team,SystemTime::now());
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
