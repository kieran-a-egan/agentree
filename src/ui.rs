use crate::{App,Attention,Focus,Page};
use crate::model::EventKind;
use ratatui::{Frame, layout::{Constraint, Direction, Layout, Rect}, style::{Color,Modifier,Style},
    text::{Line,Span}, widgets::{Block,Borders,Paragraph,Wrap,Clear}};

const CYAN:Color=Color::Rgb(68,218,216);
const OPUS:Color=Color::Rgb(183,150,254);
const ORANGE:Color=Color::Rgb(249,179,109);
const MUTED:Color=Color::Rgb(119,137,157);

fn block<'a>(title:&'a str,color:Color)->Block<'a> {
    Block::default().title(format!(" {title} ")).borders(Borders::ALL)
        .border_style(Style::default().fg(color))
}
/// Terminal-friendly "glow": a moving graded highlight along the outline.
/// Ratatui has no bloom shader; the streak changes border cell colour only,
/// retaining the existing glyphs, contents and interaction.
fn perimeter_point(area:Rect,index:usize)->(u16,u16){
    let w=area.width as usize;
    let h=area.height as usize;
    let top=w;
    let right=h.saturating_sub(1);
    let bottom=w.saturating_sub(1);
    if index<top {return (area.x+index as u16,area.y);}
    let mut i=index-top;
    if i<right {return (area.x+w as u16-1,area.y+1+i as u16);}
    i-=right;
    if i<bottom {return (area.x+(w-2-i) as u16,area.y+(h-1) as u16);}
    i-=bottom;
    (area.x,area.y+(h-2-i) as u16)
}
/// One lap takes a fixed amount of time regardless of panel dimensions.
/// The old 76ms-per-cell animation needed 10-25 seconds per lap, while its
/// activity gate expired sooner. The result looked stuck or out of sync.
const BEAM_LAP_MS:u128=3_200;
fn beam_head(perimeter:usize,phase_ms:u128)->usize {
    if perimeter==0 {return 0;}
    ((phase_ms%BEAM_LAP_MS)*perimeter as u128 / BEAM_LAP_MS) as usize
}
fn glow_border(frame:&mut Frame,area:Rect,base:Color,phase_ms:u128){
    if area.width<4 || area.height<4{return;}
    let perimeter=(area.width as usize*2)+(area.height as usize*2)-4;
    let head=beam_head(perimeter,phase_ms);
    // Wider tail is visible at 80ms refresh without relying on GPU effects.
    let trail=(perimeter/12).clamp(7,22);
    let (bright, dim) = if base == CYAN {
        ((244, 255, 255), (32, 91, 106))
    } else if base == OPUS {
        ((251, 240, 255), (75, 46, 123))
    } else {
        ((255, 248, 215), (113, 70, 32))
    };
    for distance in (0..trail).rev(){
        let at=(head+perimeter-distance)%perimeter;
        let (x,y)=perimeter_point(area,at);
        if let Some(cell)=frame.buffer_mut().cell_mut((x,y)) {
            // Only illuminate actual border glyphs, not title text.
            if !matches!(cell.symbol(),"─"|"│"|"┌"|"┐"|"└"|"┘") {continue;}
            let numerator=(trail-distance) as u16;
            let d=trail as u16;
            let mix=|hi:u8,lo:u8| -> u8 {((hi as u16*numerator+lo as u16*(d-numerator))/d) as u8};
            cell.set_fg(Color::Rgb(mix(bright.0,dim.0),mix(bright.1,dim.1),mix(bright.2,dim.2)));
            if distance==0 {cell.set_bg(Color::Rgb(bright.0/10,bright.1/10,bright.2/10));}
        }
    }
}
fn paragraph(frame:&mut Frame,area:Rect, title:&str, content:Vec<Line<'static>>,colour:Color){
    frame.render_widget(Paragraph::new(content).block(block(title,colour)).wrap(Wrap{trim:false}),area);
}
fn divide(area:Rect,items:&[Constraint],horizontal:bool)->Vec<Rect>{
    Layout::default().direction(if horizontal{Direction::Horizontal}else{Direction::Vertical})
        .constraints(items.to_vec()).split(area).to_vec()
}
fn lines<'a>(s:&'a str) -> Vec<Line<'a>> { s.lines().map(|x|Line::raw(x.to_owned())).collect() }
fn accent(text:String,style:Color)->Line<'static>{Line::styled(text,Style::default().fg(style))}
fn small_frame(frame:&mut Frame, msg:&str){
    frame.render_widget(Paragraph::new(msg).block(block(" CLAUDE AGENT TREE ",ORANGE)),frame.area());
}
fn header(frame:&mut Frame,area:Rect,page:Page,demo:bool){
    let mode=if demo{"DEMO - OFFLINE"}else if page==Page::Running{"LIVE NATIVE TEAM"}else{"WINDOWS / CONPTY"};
    frame.render_widget(Paragraph::new(Line::from(vec![
        Span::styled("  CLAUDE CODE AGENT TREE  ",Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::styled("HAIKU 5.5 TEAM  ",Style::default().fg(CYAN)),
        Span::styled("OPUS ON CALL  ",Style::default().fg(OPUS)),
        Span::styled(mode,Style::default().fg(if demo{Color::Yellow}else{Color::Green})),
    ])).alignment(ratatui::layout::Alignment::Center).block(block("",MUTED)),area);
}
fn setup(frame:&mut Frame,area:Rect,app:&App){
    let parts=divide(area,&[Constraint::Length(5),Constraint::Min(10),Constraint::Length(8)],false);
    let project_label=if app.focus==Focus::Project {"PROJECT PATH  < EDITING >"} else {"PROJECT PATH"};
    paragraph(frame,parts[0],project_label,vec![Line::raw("Type full directory path. F2: browse folders, Up/Down: recent project.".to_owned()),
        accent(format!("{}{}",app.project,if app.focus==Focus::Project{"█"}else{""}),CYAN)],
        if app.focus==Focus::Project{CYAN}else{MUTED});
    let objective_label=if app.focus==Focus::Objective {"OBJECTIVE  < EDITING >  |  Ctrl+L clear all"} else {"OBJECTIVE  |  Ctrl+L clear all"};
    let mut content=lines(&app.objective);
    if content.is_empty(){content.push(accent("Explain exactly what the agents should accomplish...".into(),MUTED));}
    if app.focus==Focus::Objective{content.push(accent("█".into(),CYAN));}
    frame.render_widget(Paragraph::new(content).block(block(objective_label,if app.focus==Focus::Objective{CYAN}else{MUTED}))
        .wrap(Wrap{trim:false}),parts[1]);
    let mut history=vec![accent("RECENT PROJECTS  (Up/Down on project field)".to_owned(),OPUS)];
    history.extend(app.recent.iter().take(3).map(|x|Line::raw(format!("  {x}"))));
    paragraph(frame,parts[2],"LAUNCH",history,OPUS);
}
fn browser(frame:&mut Frame,area:Rect,app:&App){
    let parts=divide(area,&[Constraint::Length(4),Constraint::Min(5)],false);
    paragraph(frame,parts[0],"CURRENT DIRECTORY",vec![Line::raw(app.browser_dir.display().to_string()),
        accent("Up/Down select   Enter open folder   S select this folder   Esc back".into(),CYAN)],CYAN);
    let visible=parts[1].height.saturating_sub(2) as usize;
    let start=app.browser_index.saturating_sub(visible.saturating_sub(1));
    let items=app.browser_entries.iter().enumerate().skip(start).take(visible)
        .map(|(i,p)|{
            let name=if i==0 {"..  [PARENT DIRECTORY]".into()}else{p.file_name().unwrap_or_default().to_string_lossy().to_string()};
            Line::styled(format!(" {} {}",if i==app.browser_index {"▶"}else{" "},name),
                Style::default().fg(if i==app.browser_index{Color::Black}else{Color::Gray})
                    .bg(if i==app.browser_index{CYAN}else{Color::Reset}))
        }).collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(items).block(block("FOLDERS",MUTED)),parts[1]);
}
fn vt_color(c:vt100::Color)->Color {
    match c {
        vt100::Color::Default=>Color::Reset,
        vt100::Color::Idx(i)=>Color::Indexed(i),
        vt100::Color::Rgb(r,g,b)=>Color::Rgb(r,g,b),
    }
}
fn render_terminal(frame:&mut Frame,area:Rect,app:&App){
    if area.height<3 {return;}
    let (rows,cols)=app.terminal_size;
    let screen=app.terminal.screen();
    let (cursor_y,cursor_x)=screen.cursor_position();
    let mut output:Vec<Line>=Vec::with_capacity(rows as usize);
    for y in 0..rows {
        let mut spans:Vec<Span>=Vec::new();
        let mut text=String::new();
        let mut last_style:Option<Style>=None;
        for x in 0..cols {
            if let Some(cell)=screen.cell(y,x) {
                // vt100 0.15.x exposes is_wide() on the leading cell. Skip the
                // following column to avoid rendering wide glyphs twice.
                if x > 0 && screen.cell(y, x - 1).is_some_and(|prev| prev.is_wide()) {
                    continue;
                }
                let mut style=Style::default().fg(vt_color(cell.fgcolor())).bg(vt_color(cell.bgcolor()));
                if cell.bold() {style=style.add_modifier(Modifier::BOLD);}
                if cell.italic(){style=style.add_modifier(Modifier::ITALIC);}
                if cell.underline(){style=style.add_modifier(Modifier::UNDERLINED);}
                if cell.inverse() || (y==cursor_y && x==cursor_x && app.focus==Focus::Native){
                    style=style.add_modifier(Modifier::REVERSED);
                }
                let content = if cell.has_contents() { cell.contents() } else { " ".to_string() };
                if last_style!=Some(style) {
                    if let Some(prev)=last_style {spans.push(Span::styled(std::mem::take(&mut text),prev));}
                    last_style=Some(style);
                }
                text.push_str(&content);
            }
        }
        if let Some(style)=last_style {spans.push(Span::styled(text,style));}
        output.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(output).block(block("INTERACTIVE CLAUDE TERMINAL  |  F6 focus",ORANGE)),area);
}

// The dashboard retains Claude as the only orchestrator. All boxes render
// observed task/session data; a missing cost or status is shown as unknown.
fn shorten(value:&str,max:usize)->String {
    let len=value.chars().count();
    if len<=max {return value.to_owned();}
    format!("{}…",value.chars().take(max.saturating_sub(1)).collect::<String>())
}
fn count(v:u64)->String {
    if v>=1_000_000 {format!("{:.2}m",v as f64/1_000_000.)}
    else if v>=1_000 {format!("{:.1}k",v as f64/1_000.)}
    else {v.to_string()}
}
fn uptime(seconds:u64)->String {format!("{:02}:{:02}:{:02}",seconds/3600,seconds/60%60,seconds%60)}
/// An estimate only when the normalised `Usage.cost_usd` is observed. Otherwise it is a lower bound.
fn cost_text(t:&crate::telemetry::Totals,basis:&str)->String {
    if t.calls==0 {return "Cost unknown".into();}
    match t.usage().cost_usd.get() {
        Some(usd)=>format!("Cost ~${usd:.4} {basis}"),
        None=>format!("Cost >=${:.4} + {}",t.list_usd,if t.has_pricing_gap {"unpriced"} else {"incomplete usage"}),
    }
}
fn stats_lines(t:Option<&crate::telemetry::Totals>,max:usize)->Vec<Line<'static>> {
    let Some(t)=t else {return vec![accent("Tokens / cost: not linked".into(),MUTED),
        accent("Context: unknown".into(),MUTED)];};
    let context=if t.calls>0 {format!("{:.1}%",t.context_percent())}else{"--".into()};
    vec![
        Line::raw(format!("Tokens {}  I:{}  O:{}",count(t.tokens()),count(t.input),count(t.output))),
        Line::raw(format!("Cache R:{}  W:{}",count(t.cache_read),count(t.cache_write))),
        accent(format!("Context ~{} / 1m",context),CYAN),
        accent(cost_text(t,"list"),ORANGE),
        Line::raw(shorten(&format!("{} | {} calls",if t.model.is_empty(){"Model unknown"}else{&t.model},t.calls),max)),
    ]
}
fn active_agent(app:&App,role:&str)->bool {
    !app.output_end && !app.live.objective_complete && !app.telemetry.review.objective_complete
        && app.activity.recent(role, std::time::Duration::from_secs(5))
}
fn review_started(app:&App)->bool { app.live.review_started || app.telemetry.review.started }
fn review_finished(app:&App)->bool { app.live.objective_complete || app.telemetry.review.objective_complete }
fn lead_and_opus(frame:&mut Frame,area:Rect,app:&App){
    let widths=divide(area,&[Constraint::Percentage(68),Constraint::Percentage(32)],true);
    let lead=app.telemetry.actors.get("main");
    let mut content=vec![accent(if active_agent(app,"main") {
            "HAIKU 5.5 // RECENT OBSERVED ACTIVITY".into()
        } else {"HAIKU 5.5 // PLANS + COORDINATES".into()},ORANGE),
        Line::raw(format!("Runtime: {} | tools: {}",uptime(app.run_elapsed().as_secs()),lead.map(|t|t.tools).unwrap_or(0)))];
    content.extend(stats_lines(lead,widths[0].width.saturating_sub(3) as usize));
    paragraph(frame,widths[0],"HAIKU 5.5 / MAIN",content,ORANGE);
    if active_agent(app,"main") {glow_border(frame,widths[0],ORANGE,app.started.elapsed().as_millis());}
    let mut advisor=vec![accent("OPUS 5.5  //  ON CALL".into(),OPUS)];
    let calls=app.telemetry.advisor_invocations.max(app.telemetry.advisor.calls);
    let observed=app.live.advisor_seen || calls>0 || app.telemetry.observed_opus;
    if calls>0 {
        advisor.push(Line::raw(format!("Advisor calls observed: {calls}")));
        if app.telemetry.advisor_completed>0 {
            advisor.push(Line::raw(format!("Results observed: {}",app.telemetry.advisor_completed)));
        }
    } else if app.telemetry.observed_opus {
        advisor.push(accent("Opus use recorded; type unknown".into(),OPUS));
    } else if observed {
        advisor.push(accent("Advisor seen in Claude UI".into(),OPUS));
        advisor.push(Line::raw("Count not available in transcript"));
    } else {
        advisor.push(accent("No advisor invocation observed".into(),MUTED));
    }
    if app.telemetry.advisor.calls>0 {
        let t=&app.telemetry.advisor;
        advisor.push(Line::raw(format!("Tokens {}  I:{}  O:{}",count(t.tokens()),count(t.input),count(t.output))));
        advisor.push(accent(cost_text(t,"API list"),ORANGE));
    } else if observed {
        advisor.push(Line::raw("Advisor tokens/cost: unavailable"));
    }
    if app.live.advisor_in_flight {advisor.push(accent("Currently advising (live terminal)".into(),OPUS));}
    advisor.push(accent("Advice does not confirm final review".into(),MUTED));
    paragraph(frame,widths[1],"OPUS ADVISOR",advisor,OPUS);
    let advisor_busy=!review_finished(app) && (app.live.advisor_in_flight
        || (calls>app.telemetry.advisor_completed && app.activity.recent("main",std::time::Duration::from_secs(5))));
    if advisor_busy {glow_border(frame,widths[1],OPUS,app.started.elapsed().as_millis());}
}
// Summary strip. This deliberately does not claim to perform routing or
// System One/JEV reasoning: native Claude Code owns orchestration.
fn run_totals(frame:&mut Frame,area:Rect,app:&App){
    let elapsed=uptime(app.run_elapsed().as_secs());
    let status=if review_finished(app) {
            match app.telemetry.review.result.as_deref().or(app.live.review_result.as_deref()) {
                Some("PASS") if app.telemetry.review.advisor_evidence || app.live.advisor_after_review=>"COMPLETE / OPUS OBSERVED",
                Some("PASS")=>"COMPLETE / REVIEW UNVERIFIED",
                Some("NEEDS_CHANGES")=>"ENDED / NEEDS CHANGES",
                Some("BLOCKED")=>"ENDED / BLOCKED",
                _=>"OBJECTIVE ENDED / REVIEW UNKNOWN",
            }
        }
        else if app.output_end {"CLAUDE SESSION ENDED"}
        else if review_started(app) {"FINAL REVIEW STAGE"}
        else if app.clock_paused_manually {"CLOCK PAUSED MANUALLY"}
        else if app.attention.is_some(){"NEEDS INPUT"}
        else if app.team.detected{"NATIVE TEAM ACTIVE"}
        else if !app.native_screen_visible && !app.demo{"CLAUDE STARTING"}
        else {"WAITING FOR TEAM"};
    let t=&app.telemetry.combined;
    let total=if app.telemetry.files_found==0 {
        "Observed tokens: --    Est. API cost: --".to_owned()
    }else{
        format!("Observed {} tokens  (I:{} O:{} Cache R:{} W:{})",
            count(t.tokens()),count(t.input),count(t.output),count(t.cache_read),count(t.cache_write))
    };
    let missing_advisor_cost=(app.telemetry.advisor_invocations>0 || app.live.advisor_seen)
        && app.telemetry.advisor.calls==0;
    let spend=if app.telemetry.files_found==0 || t.calls==0 {"Estimated API cost: --".to_owned()}
        else {match t.usage().cost_usd.get() {
            Some(usd) if !missing_advisor_cost => format!("Estimated API cost ~${usd:.4} USD list"),
            _ => format!("Estimated API cost >=${:.4} + unobserved usage",t.list_usd),
        }};
    paragraph(frame,area,"RUN TOTALS / OBSERVED",vec![
        accent(format!("{}  |  Runtime {}{}  |  {} teammates  |  {} tools",
            status,elapsed,if app.finished_elapsed.is_some(){" (stopped)"}else{""},app.team.agents.len(),t.tools),CYAN),
        Line::raw(total),
        accent(format!("{}  |  {}",spend,shorten(&app.telemetry.coverage,area.width.saturating_sub(40) as usize)),ORANGE),
    ],CYAN);
    if !app.output_end && !review_finished(app) && app.activity.any_recent(std::time::Duration::from_secs(5)){
        glow_border(frame,area,CYAN,app.started.elapsed().as_millis());
    }
}
fn worker_card(frame:&mut Frame,area:Rect,app:&App,idx:usize){
    let Some(worker)=app.team.agents.get(idx) else {
        paragraph(frame,area,"HAIKU / AVAILABLE",vec![accent("Waiting for Claude teammate".into(),MUTED)],CYAN);
        return;
    };
    let max=area.width.saturating_sub(3) as usize;
    let state_label=if active_agent(app,&worker.name) {format!("{} / RECENT I/O",worker.state)}
        else {worker.state.clone()};
    let mut content=vec![accent(shorten(&format!("{}  •  {}",worker.name,state_label),max),CYAN),
        Line::raw(shorten(if worker.task.is_empty(){if worker.role.is_empty(){"No task assigned"}else{&worker.role}}else{&worker.task},max))];
    let observed=app.teammate_elapsed(&worker.name).map(|i|uptime(i.as_secs())).unwrap_or_else(||"--".into());
    content.push(Line::raw(format!("Since first seen: {}",observed)));
    content.extend(stats_lines(app.telemetry.actors.get(&worker.name),max));
    if let Some(t)=app.telemetry.actors.get(&worker.name){
        if !t.action.is_empty(){content.push(Line::raw(shorten(&format!("Last: {}",t.action),max)));}
    }
    paragraph(frame,area,&format!("HAIKU / {}",shorten(&worker.name,max.saturating_sub(10))),content,CYAN);
    if worker.state!="TASK COMPLETE" && worker.state!="WAITING" && active_agent(app,&worker.name){
        glow_border(frame,area,CYAN,app.started.elapsed().as_millis()+idx as u128*470);
    }
}
fn workers(frame:&mut Frame,area:Rect,app:&App){
    let parts=divide(area,&[Constraint::Percentage(34),Constraint::Percentage(33),Constraint::Percentage(33)],area.width>=65);
    for (i,part) in parts.into_iter().enumerate(){worker_card(frame,part,app,i);}
}
fn review_strip(frame:&mut Frame,area:Rect,app:&App){
    let review=&app.telemetry.review;
    let started=review_started(app);
    let ended=review_finished(app);
    let result=review.result.as_ref().or(app.live.review_result.as_ref());
    let opus_seen=review.advisor_evidence || app.live.advisor_after_review;
    let label=if ended {"FINAL AUDIT RESULT REPORTED"}
        else if started {"FINAL AUDIT IN PROGRESS"}
        else {"FINAL AUDIT PENDING"};
    let detail=match result.map(String::as_str) {
        Some(status) if review.advisor_evidence=>format!("{} reported / Opus observed after audit started",status),
        Some(status) if opus_seen=>format!("{} reported / Opus call seen in terminal",status),
        Some(status)=>format!("{} reported / Opus review not independently confirmed",status),
        None if started && app.live.advisor_in_flight && app.live.advisor_after_review=>
            "Opus final consultation active (live terminal)".into(),
        None if started && opus_seen=>"Opus consultation observed; awaiting lead verdict".into(),
        None if started=>"Review requested; awaiting Opus consultation / result".into(),
        None=>"No final review stage observed yet".into(),
    };
    paragraph(frame,area,"OPUS 5.5 / FINAL REVIEW",vec![
        accent(label.into(),OPUS),
        Line::raw(shorten(&detail,area.width.saturating_sub(3) as usize)),
    ],OPUS);
    // A review announcement by itself must NOT animate an idle Opus panel.
    if started && !ended && app.live.advisor_in_flight && app.live.advisor_after_review {
        glow_border(frame,area,OPUS,app.started.elapsed().as_millis()+400);
    }
}
/// UTC clock time of an event, or a placeholder when the source had no valid timestamp.
fn clock(at:Option<u64>)->String {
    match at {
        Some(ms)=>{let s=(ms/1000)%86_400;format!("{:02}:{:02}:{:02}",s/3600,s/60%60,s%60)}
        None=>"--:--:--".into(),
    }
}
fn event_text(kind:&EventKind)->String {
    match kind {
        EventKind::ToolUsed{tool,target}=>if target.is_empty(){tool.clone()}else{format!("{tool} {target}")},
        EventKind::AdvisorInvoked=>"Consultation invoked".into(),
        EventKind::AdvisorCompleted=>"Consultation completed".into(),
        EventKind::ReviewStarted=>"Review stage requested by lead".into(),
        EventKind::ObjectiveCompleted=>"Objective marked complete".into(),
    }
}
fn timeline(frame:&mut Frame,area:Rect,app:&App){
    let cap=area.height.saturating_sub(2) as usize;
    let rows:Vec<Line<'static>>=if app.telemetry.events.is_empty(){
        vec![accent("Waiting for tool activity in local Claude JSONL transcripts".into(),MUTED)]
    }else{app.telemetry.events.iter().rev().take(cap).collect::<Vec<_>>()
        .into_iter().rev().map(|e|Line::from(vec![
            Span::styled(format!("{}  ",clock(e.at)),Style::default().fg(MUTED)),
            Span::styled(format!("{:<16}",shorten(&e.actor,15)),Style::default().fg(CYAN)),
            Span::raw(shorten(&event_text(&e.kind),70)),
        ])).collect()};
    paragraph(frame,area,"SESSION LOG / LIVE TOOL ACTIVITY",rows,MUTED);
}
fn agent_dashboard(frame:&mut Frame,area:Rect,app:&App){
    if area.height<24 {
        let p=divide(area,&[Constraint::Length(5),Constraint::Min(5)],false);
        run_totals(frame,p[0],app);timeline(frame,p[1],app);return;
    }
    let parts=divide(area,&[Constraint::Length(5),Constraint::Length(9),Constraint::Min(10),Constraint::Length(4),Constraint::Length(7)],false);
    run_totals(frame,parts[0],app);
    lead_and_opus(frame,parts[1],app);
    workers(frame,parts[2],app);
    review_strip(frame,parts[3],app);
    timeline(frame,parts[4],app);
}
fn attention_panel(frame:&mut Frame,area:Rect,att:&Attention,app:&App){
    if area.width<30||area.height<4{return;}
    let content=vec![
        accent(format!("ISSUE: {}",att.title),Color::Yellow),
        Line::raw(format!("WHY: {}",att.reason)),
        accent(format!("QUESTION: {}",att.question),CYAN),
        Line::raw(if att.options.is_empty(){"".into()}else{format!("OPTIONS: {}",att.options.join("  |  "))}),
        Line::raw(format!("ANSWER: {}{}",app.response,if app.focus==Focus::Response{"█"}else{""})),
        accent("F7 edit answer  ·  Enter send  ·  F6 native Claude controls".into(),MUTED),
    ];
    frame.render_widget(Clear,area);
    frame.render_widget(Paragraph::new(content).wrap(Wrap{trim:true})
        .block(block("NEEDS ATTENTION  //  HUMAN INPUT REQUIRED",Color::Yellow)),area);
}
fn running(frame:&mut Frame,area:Rect,app:&App){
    if app.dashboard_expanded {agent_dashboard(frame,area,app);} else if app.terminal_expanded {render_terminal(frame,area,app);} else {
        let segments=divide(area,&[Constraint::Percentage(58),Constraint::Percentage(42)],true);
        agent_dashboard(frame,segments[0],app);
        render_terminal(frame,segments[1],app);
    }
    if let Some(a)=app.attention.as_ref().filter(|_|app.focus==Focus::Response) {
        let width=area.width.saturating_sub(10).min(106);
        let height=13u16.min(area.height.saturating_sub(2)).max(4);
        let r=Rect{x:area.x+(area.width.saturating_sub(width))/2,
            y:area.y+(area.height.saturating_sub(height))/2,
            width,height};
        attention_panel(frame,r,a,app);
    }
}
fn footer(frame:&mut Frame,area:Rect,app:&App){
    let text=match app.page {
        Page::Setup=>"Tab switch fields | F2 browse | Ctrl+L clear objective | Enter newline | F5 start | Esc quit".to_owned(),
        Page::Browser=>"Up/Down browse | Enter folder | S select | Esc back".to_owned(),
        Page::Running=>if app.close_confirm{"CTRL+Q again to exit and terminate embedded Claude".into()}
            else{"F4 pause/resume clock | F6 Claude | F8 terminal | F9 tree | F7 attention | Ctrl+Q twice quit".into()},
        Page::Finished=>"Claude session ended | F10 or Enter new project/objective | Esc quit".into(),
    };
    frame.render_widget(Paragraph::new(vec![accent(text,CYAN),Line::raw(app.message.clone())])
        .block(block("CONTROLS / STATUS",MUTED)),area);
}

pub fn native_dimensions(whole:Rect,expanded:bool)->(u16,u16){
    let central=whole.height.saturating_sub(3+4); // header + footer
    let width=if expanded {whole.width}else{whole.width.saturating_mul(42)/100};
    (central.saturating_sub(2).max(4),width.saturating_sub(2).max(20))
}
pub fn draw(frame:&mut Frame,app:&App){
    let area=frame.area();
    if area.width<78||area.height<21{small_frame(frame,"Increase terminal to at least 78 columns x 21 rows.");return;}
    let shell=divide(area,&[Constraint::Length(3),Constraint::Min(13),Constraint::Length(4)],false);
    header(frame,shell[0],app.page,app.demo);
    match app.page {
        Page::Setup=>setup(frame,shell[1],app),
        Page::Browser=>browser(frame,shell[1],app),
        Page::Running|Page::Finished=>running(frame,shell[1],app),
    }
    footer(frame,shell[2],app);
}

#[cfg(test)] mod animation_tests {
    use super::*;
    #[test] fn beam_speed_does_not_depend_on_panel_perimeter() {
        for length in [32, 96, 340] {
            assert_eq!(beam_head(length,0),0);
            assert_eq!(beam_head(length,BEAM_LAP_MS/2),length/2);
            assert_eq!(beam_head(length,BEAM_LAP_MS),0);
        }
    }
    #[test] fn glow_path_covers_each_edge_without_leaving_rect() {
        let area=Rect::new(10,20,30,12);
        let len=2*(area.width as usize+area.height as usize)-4;
        let mut seen=std::collections::HashSet::new();
        for i in 0..len {
            let (x,y)=perimeter_point(area,i);
            assert!(x>=area.x && x<area.x+area.width);
            assert!(y>=area.y && y<area.y+area.height);
            assert!(x==area.x || x==area.x+area.width-1 || y==area.y || y==area.y+area.height-1);
            seen.insert((x,y));
        }
        assert_eq!(seen.len(),len);
    }
    #[test] fn timeline_wording_and_clock_match_the_event_contract() {
        assert_eq!(clock(Some(1_791_594_000_000)),"01:00:00");
        assert_eq!(clock(None),"--:--:--");
        let tool=|target:&str|EventKind::ToolUsed{tool:"Read".into(),target:target.into()};
        assert_eq!(event_text(&tool("main.rs")),"Read main.rs");
        assert_eq!(event_text(&tool("")),"Read");
        assert_eq!(event_text(&EventKind::AdvisorCompleted),"Consultation completed");
    }
    #[test] fn cost_is_an_estimate_only_when_usage_cost_is_observed() {
        use crate::telemetry::{Missing,Totals};
        let observed=Totals{calls:1,list_usd:0.5,..Default::default()};
        assert_eq!(cost_text(&observed,"list"),"Cost ~$0.5000 list");
        let incomplete=Totals{missing:Missing{cache_read:true,..Default::default()},..observed.clone()};
        assert_eq!(cost_text(&incomplete,"list"),"Cost >=$0.5000 + incomplete usage");
        let unpriced=Totals{has_pricing_gap:true,..observed.clone()};
        assert_eq!(cost_text(&unpriced,"list"),"Cost >=$0.5000 + unpriced");
        assert_eq!(cost_text(&Totals::default(),"list"),"Cost unknown");
    }
}
