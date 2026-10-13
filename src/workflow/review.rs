//! Review packet, verification results, verdict and bounded repair.
//!
//! The packet, verdict and verification types are versioned together with the workflow
//! definition under `agentree.workflow/1`; a change to them is a new schema version.
//!
//! The reviewer is handed the real diff (by hash and content), the objective, the acceptance
//! criteria and the verification evidence. A verdict is accepted only if it came from the
//! reviewer that was assigned, covers the diff that was reviewed, and its claims are consistent
//! with the evidence: a `Pass` cannot sit on top of a failed test.

use super::contract::StagePolicy;
use super::evidence::{source_hash, Handoff};
use super::registry::ReviewSelection;
use super::{Code, Violation};
use serde::{Deserialize, Serialize};

/// The review outcomes Agentree already recognises as `AGENTREE_REVIEW_RESULT:<id>:<marker>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    NeedsChanges,
    Blocked,
}

impl Verdict {
    pub fn marker(self) -> &'static str {
        match self {
            Verdict::Pass => "PASS",
            Verdict::NeedsChanges => "NEEDS_CHANGES",
            Verdict::Blocked => "BLOCKED",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    Passed,
    Failed,
    NotRun,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationResult {
    pub command: String,
    pub status: VerificationStatus,
    pub exit_code: Option<i32>,
    /// Hash of the captured output, so the reviewer can ask for it by reference.
    #[serde(default)]
    pub output_hash: Option<String>,
}

impl VerificationResult {
    /// `Passed` is credible only without a nonzero process exit code.
    fn contradictory(&self) -> bool {
        self.status == VerificationStatus::Passed && self.exit_code.is_some_and(|c| c != 0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRef {
    pub id: String,
    pub model: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewPacket {
    pub objective: String,
    pub acceptance_criteria: Vec<String>,
    /// The actual patch under review, not a summary of it.
    pub diff: String,
    pub verification: Vec<VerificationResult>,
    /// Compact evidence prepared by the bulk reader. Optional help, never a substitute.
    #[serde(default)]
    pub evidence: Vec<Handoff>,
    /// Every agent that produced part of the change.
    pub implementers: Vec<AgentRef>,
}

impl ReviewPacket {
    pub fn diff_hash(&self) -> String {
        source_hash(self.diff.as_bytes())
    }

    pub fn validate(&self) -> Vec<Violation> {
        let mut out = Vec::new();
        let mut bad = |m: &str| out.push(Violation::new(Code::Review, "review_packet", m));
        if self.objective.trim().is_empty() {
            bad("has no objective");
        }
        if self.acceptance_criteria.iter().all(|c| c.trim().is_empty()) {
            bad("has no acceptance criteria");
        }
        if self.diff.trim().is_empty() {
            bad("has no diff; the reviewer must see the actual change");
        }
        if self.verification.is_empty() {
            bad("has no verification results");
        }
        for r in self.verification.iter().filter(|r| r.contradictory()) {
            bad(&format!(
                "verification '{}' is marked passed but reports exit code {}",
                r.command,
                r.exit_code.unwrap_or_default()
            ));
        }
        if self.implementers.is_empty() {
            bad("does not say who implemented the change");
        }
        for h in &self.evidence {
            out.extend(h.validate());
        }
        out
    }
}

/// A reviewer's request for more source. Answered by the bulk reader, with evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InspectionRequest {
    pub path: String,
    pub start_line: Option<u32>,
    pub end_line: Option<u32>,
    pub reason: String,
}

impl InspectionRequest {
    /// `made` requests were already used out of `max`.
    pub fn validate(&self, made: u8, max: u8) -> Vec<Violation> {
        let mut out = Vec::new();
        let mut bad = |m: &str| out.push(Violation::new(Code::Review, "inspection", m));
        if made >= max {
            bad("the inspection budget is used up");
        }
        if self.path.is_empty()
            || self.path.starts_with(['/', '\\'])
            || self.path.contains(':')
            || self.path.split(['/', '\\']).any(|c| c == "..")
        {
            bad("the path must be inside the workspace");
        }
        if self.reason.trim().is_empty() {
            bad("a request must say what it needs to confirm");
        }
        if matches!((self.start_line, self.end_line), (Some(s), Some(e)) if s == 0 || s > e) {
            bad("the line range is invalid");
        }
        out
    }
}

/// The reviewer selected for a packet. Built only by [`assign_reviewer`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewAssignment {
    pub reviewer: AgentRef,
    pub diff_hash: String,
    pub authorized_downgrade: bool,
}

/// Binds a selected review model to a fresh reviewer agent. The reviewer must not be any agent
/// that implemented the change. It may run the same *model*: independence is a separate agent
/// with no shared context, and requiring a different model would force a weaker reviewer.
pub fn assign_reviewer(
    packet: &ReviewPacket,
    selection: &ReviewSelection,
    reviewer_agent: &str,
) -> Result<ReviewAssignment, Vec<Violation>> {
    let mut out = packet.validate();
    if packet.implementers.iter().any(|a| a.id == reviewer_agent) {
        out.push(Violation::new(
            Code::Review,
            "reviewer",
            format!("'{reviewer_agent}' implemented the change and cannot review it"),
        ));
    }
    if !out.is_empty() {
        return Err(out);
    }
    Ok(ReviewAssignment {
        reviewer: AgentRef {
            id: reviewer_agent.into(),
            model: selection.model.clone(),
        },
        diff_hash: packet.diff_hash(),
        authorized_downgrade: selection.authorized_downgrade,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub severity: Severity,
    pub file: String,
    pub summary: String,
    pub failure_scenario: String,
}

/// What the reviewer says it independently assessed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assessed {
    pub correctness: bool,
    pub security: bool,
    pub regressions: bool,
    pub unresolved_risks: bool,
}

impl Assessed {
    fn all(self) -> bool {
        self.correctness && self.security && self.regressions && self.unresolved_risks
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewVerdict {
    pub verdict: Verdict,
    pub reviewer: AgentRef,
    pub reviewed_diff_hash: String,
    pub assessed: Assessed,
    #[serde(default)]
    pub findings: Vec<Finding>,
    #[serde(default)]
    pub evidence_gaps: Vec<String>,
    #[serde(default)]
    pub blocked_reason: Option<String>,
}

/// Checks a verdict against the assignment and packet it answers.
pub fn validate_verdict(
    v: &ReviewVerdict,
    assignment: &ReviewAssignment,
    packet: &ReviewPacket,
) -> Vec<Violation> {
    let mut out = Vec::new();
    let mut bad = |m: &str| out.push(Violation::new(Code::Verdict, "verdict", m));
    if v.reviewer != assignment.reviewer {
        bad("was not produced by the assigned reviewer and model");
    }
    if v.reviewed_diff_hash != assignment.diff_hash || assignment.diff_hash != packet.diff_hash() {
        bad("does not cover the diff in the review packet");
    }
    match v.verdict {
        Verdict::Pass => {
            if !v.assessed.all() {
                bad(
                    "a pass needs correctness, security, regressions and unresolved risks assessed",
                );
            }
            if v.findings.iter().any(|f| f.severity >= Severity::Medium) {
                bad("a pass cannot carry medium or higher findings");
            }
            if !v.evidence_gaps.is_empty() {
                bad("a pass cannot carry evidence gaps");
            }
            if packet
                .verification
                .iter()
                .any(|r| r.status != VerificationStatus::Passed || r.contradictory())
            {
                bad("a pass cannot rest on failed, missing or contradictory verification");
            }
        }
        Verdict::NeedsChanges => {
            if v.findings.is_empty() && v.evidence_gaps.is_empty() {
                bad("needs_changes must name a finding or an evidence gap");
            }
        }
        Verdict::Blocked => {
            if v.blocked_reason.as_deref().unwrap_or("").trim().is_empty() {
                bad("blocked must say why review could not be performed");
            }
        }
    }
    out
}

/// Why a stage did not succeed, as far as the contract distinguishes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// A tool or session error that may not repeat.
    Transient,
    VerificationFailed,
    ReviewNeedsChanges,
    /// The mandatory model is not available. Never retried on another model.
    ModelUnavailable,
    /// A permission, ownership or trust rule was hit.
    PolicyViolation,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub attempts: u8,
    pub repair_cycles: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Retry,
    /// Run another bounded repair cycle; a review failure is re-reviewed by the same policy.
    Repair,
    /// Stop and ask the human.
    Escalate(String),
    Abort,
}

/// The next step after a failure. Loops are bounded by the stage policy and always end in a
/// human escalation or an abort; none ends in silently continuing.
pub fn next_action(policy: &StagePolicy, progress: Progress, failure: Failure) -> Action {
    use super::contract::OnExhausted;
    let exhausted = || match policy.on_exhausted {
        OnExhausted::Escalate => Action::Escalate(
            "retry and repair limits are used up; a human decision is needed".into(),
        ),
        OnExhausted::Abort => Action::Abort,
    };
    match failure {
        Failure::ModelUnavailable => Action::Escalate(
            "the required model is unavailable; authorize an alternative or wait".into(),
        ),
        Failure::PolicyViolation => {
            Action::Escalate("a permission, ownership or trust rule was violated".into())
        }
        Failure::Transient if progress.attempts.saturating_add(1) < policy.max_attempts => {
            Action::Retry
        }
        Failure::VerificationFailed | Failure::ReviewNeedsChanges
            if progress.repair_cycles < policy.max_repair_cycles =>
        {
            Action::Repair
        }
        _ => exhausted(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::contract::OnExhausted;

    fn packet() -> ReviewPacket {
        ReviewPacket {
            objective: "fix the parser".into(),
            acceptance_criteria: vec!["parser rejects empty input".into()],
            diff: "--- a/x\n+++ b/x\n@@\n+fix\n".into(),
            verification: vec![VerificationResult {
                command: "cargo test".into(),
                status: VerificationStatus::Passed,
                exit_code: Some(0),
                output_hash: None,
            }],
            evidence: vec![],
            implementers: vec![AgentRef {
                id: "coder-1".into(),
                model: "cheap".into(),
            }],
        }
    }

    fn selection() -> ReviewSelection {
        ReviewSelection {
            model: "top".into(),
            strongest: "top".into(),
            authorized_downgrade: false,
        }
    }

    fn pass(a: &ReviewAssignment) -> ReviewVerdict {
        ReviewVerdict {
            verdict: Verdict::Pass,
            reviewer: a.reviewer.clone(),
            reviewed_diff_hash: a.diff_hash.clone(),
            assessed: Assessed {
                correctness: true,
                security: true,
                regressions: true,
                unresolved_risks: true,
            },
            findings: vec![],
            evidence_gaps: vec![],
            blocked_reason: None,
        }
    }

    #[test]
    fn a_packet_without_the_diff_or_evidence_is_rejected() {
        let mut p = packet();
        p.diff = "  ".into();
        p.verification.clear();
        p.objective.clear();
        p.acceptance_criteria.clear();
        assert_eq!(p.validate().len(), 4);
        assert!(packet().validate().is_empty());
    }

    #[test]
    fn the_implementer_cannot_review_but_may_share_a_model_with_the_reviewer() {
        let p = packet();
        assert!(assign_reviewer(&p, &selection(), "coder-1").is_err());
        let mut p2 = packet();
        p2.implementers[0].model = "top".into();
        let a = assign_reviewer(&p2, &selection(), "reviewer-1").unwrap();
        assert_eq!(a.reviewer.model, "top");
        assert_eq!(a.diff_hash, p2.diff_hash());
    }

    #[test]
    fn verdicts_must_match_the_assignment_diff_and_evidence() {
        let p = packet();
        let a = assign_reviewer(&p, &selection(), "reviewer-1").unwrap();
        assert!(validate_verdict(&pass(&a), &a, &p).is_empty());

        let mut other_model = pass(&a);
        other_model.reviewer.model = "cheap".into();
        assert!(!validate_verdict(&other_model, &a, &p).is_empty());

        let mut stale = pass(&a);
        stale.reviewed_diff_hash = source_hash(b"another diff");
        assert!(!validate_verdict(&stale, &a, &p).is_empty());

        let mut failed = packet();
        failed.verification[0].status = VerificationStatus::Failed;
        let fa = assign_reviewer(&failed, &selection(), "reviewer-1").unwrap();
        assert!(!validate_verdict(&pass(&fa), &fa, &failed).is_empty());

        let mut partial = pass(&a);
        partial.assessed.security = false;
        partial.findings.push(Finding {
            severity: Severity::High,
            file: "x".into(),
            summary: "s".into(),
            failure_scenario: "f".into(),
        });
        assert_eq!(validate_verdict(&partial, &a, &p).len(), 2);
    }

    #[test]
    fn a_passed_result_with_a_nonzero_exit_code_cannot_support_a_pass() {
        let mut p = packet();
        p.verification[0].exit_code = Some(1);
        // The packet itself is rejected, so no reviewer can be assigned to it.
        assert!(p
            .validate()
            .iter()
            .any(|v| v.message.contains("exit code 1")));
        assert!(assign_reviewer(&p, &selection(), "reviewer-1").is_err());
        // Even with an assignment built around it, the pass is refused.
        let good = packet();
        let a = assign_reviewer(&good, &selection(), "reviewer-1").unwrap();
        let v = validate_verdict(&pass(&a), &a, &p);
        assert!(v.iter().any(|v| v.message.contains("contradictory")));
        // A failed result with a nonzero code is consistent, and a clean zero stays valid.
        p.verification[0].status = VerificationStatus::Failed;
        assert!(p.validate().is_empty());
        assert!(packet().validate().is_empty());
    }

    #[test]
    fn non_pass_verdicts_need_their_reasons() {
        let p = packet();
        let a = assign_reviewer(&p, &selection(), "reviewer-1").unwrap();
        let mut v = pass(&a);
        v.verdict = Verdict::NeedsChanges;
        assert_eq!(validate_verdict(&v, &a, &p).len(), 1);
        v.verdict = Verdict::Blocked;
        assert_eq!(validate_verdict(&v, &a, &p).len(), 1);
        v.blocked_reason = Some("strongest reviewer unavailable".into());
        assert!(validate_verdict(&v, &a, &p).is_empty());
        assert_eq!(Verdict::NeedsChanges.marker(), "NEEDS_CHANGES");
    }

    #[test]
    fn inspection_requests_are_bounded_and_workspace_relative() {
        let ok = InspectionRequest {
            path: "src/a.rs".into(),
            start_line: Some(10),
            end_line: Some(20),
            reason: "confirm the guard".into(),
        };
        assert!(ok.validate(0, 3).is_empty());
        assert!(!ok.validate(3, 3).is_empty());
        let escape = InspectionRequest {
            path: "../x".into(),
            ..ok
        };
        assert!(!escape.validate(0, 3).is_empty());
    }

    #[test]
    fn repair_is_bounded_and_always_ends_in_a_human_or_an_abort() {
        let policy = StagePolicy {
            max_attempts: 2,
            max_repair_cycles: 1,
            on_exhausted: OnExhausted::Escalate,
        };
        let at = |attempts, repair_cycles| Progress {
            attempts,
            repair_cycles,
        };
        assert_eq!(
            next_action(&policy, at(0, 0), Failure::Transient),
            Action::Retry
        );
        assert!(matches!(
            next_action(&policy, at(1, 0), Failure::Transient),
            Action::Escalate(_)
        ));
        assert_eq!(
            next_action(&policy, at(0, 0), Failure::ReviewNeedsChanges),
            Action::Repair
        );
        assert!(matches!(
            next_action(&policy, at(0, 1), Failure::VerificationFailed),
            Action::Escalate(_)
        ));
        // A missing mandatory model is never retried or repaired around.
        assert!(matches!(
            next_action(&policy, at(0, 0), Failure::ModelUnavailable),
            Action::Escalate(_)
        ));
        let abort = StagePolicy {
            on_exhausted: OnExhausted::Abort,
            ..policy
        };
        assert_eq!(
            next_action(&abort, at(0, 1), Failure::VerificationFailed),
            Action::Abort
        );
    }
}
