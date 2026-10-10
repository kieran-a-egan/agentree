//! Immediate, qualitative observations from Claude's embedded terminal.
//! Local JSONL is still authoritative for token accounting; terminal text is
//! used only for UI lifecycle status so updates don't wait for a file flush.
use std::{collections::{BTreeMap,HashMap},time::{Duration,Instant}};
use crate::telemetry::Totals;

#[derive(Default,Debug)]
pub struct LiveSignals {
    pub review_started: bool,
    pub review_result: Option<String>,
    pub objective_complete: bool,
    pub advisor_seen: bool,
    pub advisor_in_flight: bool,
    pub advisor_after_review: bool,
    /// The previous visible screen included the start banner. Track edges:
    /// repeatedly redrawing the same line is not a new consultation.
    saw_advisor_banner: bool,
    saw_advisor_result: bool,
    prior_visible_results: usize,
}
impl LiveSignals {
    pub fn observe_screen(&mut self, screen:&str, token:&str) {
        if token.is_empty(){return;}
        let start_marker=format!("AGENTREE_REVIEW_START:{token}");
        let done_marker=format!("AGENTREE_OBJECTIVE_COMPLETE:{token}");
        // ConPTY may wrap protocol lines in narrow terminal panes.
        let compact:String=screen.chars().filter(|ch|!ch.is_whitespace()).collect();
        let review_position=screen.find(&start_marker);
        if compact.contains(&start_marker){ self.review_started=true; }
        for result in ["PASS","NEEDS_CHANGES","BLOCKED"] {
            if compact.contains(&format!("AGENTREE_REVIEW_RESULT:{token}:{result}")) {
                self.review_started=true;
                self.review_result=Some(result.to_owned());
            }
        }
        if compact.contains(&done_marker){self.objective_complete=true;}

        let lower=screen.to_ascii_lowercase();
        let banner_position=lower.rfind("advising using opus");
        let banner_now=banner_position.is_some();
        let result_count=lower.matches("advisor has reviewed").count()
            + lower.matches("advisor reviewed the conversation").count();
        let result_now=result_count>0;
        let after_review_banner=match (banner_position,review_position) {
            (Some(b),Some(r))=>b>r,
            (Some(_),None)=>self.review_started,
            _=>false,
        };
        if banner_now && (!self.saw_advisor_banner
            || (after_review_banner && !self.advisor_after_review)) {
            self.advisor_seen=true;
            self.advisor_in_flight=true;
            // An old preflight advisor line can remain on screen after the
            // review marker. Only attribute this call to review when the
            // banner appears after the marker in the current screen.
            if after_review_banner {self.advisor_after_review=true;}
        }
        if result_now && (!self.saw_advisor_result || result_count>self.prior_visible_results) {
            self.advisor_seen=true;
            self.advisor_in_flight=false;
        }
        self.saw_advisor_banner=banner_now;
        self.saw_advisor_result=result_now;
        self.prior_visible_results=result_count;
    }
}

/// An animation pulse is permitted only after fresh observable telemetry.
/// A file's last-write time and a teammate's mere presence on the roster do
/// not prove that agent is currently working. No stale 20-second pulses.
#[derive(Default,Debug)]
pub struct ActivityTracker {
    prior: HashMap<String,(u64,u64,u64)>,
    last_new_record: HashMap<String,Instant>,
}
impl ActivityTracker {
    pub fn observe(&mut self, actors:&BTreeMap<String,Totals>,now:Instant){
        for (role,t) in actors {
            let current=(t.calls,t.tools,t.tokens());
            let prior=self.prior.insert(role.clone(),current);
            if (current.0>0 || current.1>0) && prior != Some(current) {
                self.last_new_record.insert(role.clone(),now);
            }
        }
    }
    pub fn recent(&self,role:&str,window:Duration)->bool {
        self.last_new_record.get(role).is_some_and(|when|when.elapsed()<window)
    }
    pub fn any_recent(&self,window:Duration)->bool {
        self.last_new_record.values().any(|when|when.elapsed()<window)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detects_final_audit_while_terminal_is_still_running(){
        let mut s=LiveSignals::default();
        s.observe_screen("text\nAGENTREE_REVIEW_START:abc-123\n", "abc-123");
        assert!(s.review_started);
        assert!(!s.advisor_after_review);
        s.observe_screen("AGENTREE_REVIEW_START:abc-123\nAdvising using Opus 5.5\n", "abc-123");
        assert!(s.advisor_in_flight && s.advisor_after_review);
        s.observe_screen("AGENTREE_REVIEW_START:abc-123\nAdvising using Opus 5.5\nAdvisor has reviewed the conversation\n", "abc-123");
        assert!(!s.advisor_in_flight);
        s.observe_screen("AGENTREE_REVIEW_RESULT:abc-123:PASS\nAGENTREE_OBJECTIVE_COMPLETE:abc-123", "abc-123");
        assert_eq!(s.review_result.as_deref(),Some("PASS"));
        assert!(s.objective_complete);
    }
    #[test]
    fn wrapped_markers_are_detected(){
        let mut s=LiveSignals::default();
        s.observe_screen("AGENTREE_REVIEW_START:abc-\n123\n", "abc-123");
        assert!(s.review_started);
        s.observe_screen("AGENTREE_OBJECTIVE_COMPLETE:abc-\n123", "abc-123");
        assert!(s.objective_complete);
    }
    #[test]
    fn old_advice_on_screen_is_not_confused_with_final_review(){
        let mut s=LiveSignals::default();
        s.observe_screen("Advising using Opus 5.5\nAdvisor has reviewed the conversation", "run");
        s.observe_screen("Advising using Opus 5.5\nAdvisor has reviewed the conversation\nAGENTREE_REVIEW_START:run", "run");
        assert!(s.review_started);
        assert!(!s.advisor_after_review);
        assert!(!s.advisor_in_flight);
    }
    #[test]
    fn no_animation_without_new_model_or_tool_records(){
        let mut tracker=ActivityTracker::default();
        let mut actors=BTreeMap::new();
        actors.insert("coder".into(),Totals::default());
        tracker.observe(&actors,Instant::now());
        assert!(!tracker.any_recent(Duration::from_secs(1)));
        actors.get_mut("coder").unwrap().calls=1;
        tracker.observe(&actors,Instant::now());
        assert!(tracker.recent("coder",Duration::from_secs(1)));
        // Re-reading the same transcript cannot retrigger the animation.
        let before=tracker.last_new_record["coder"];
        tracker.observe(&actors,Instant::now());
        assert_eq!(tracker.last_new_record["coder"],before);
    }
}
