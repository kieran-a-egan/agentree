use crate::capability::RolePlan;
use std::path::Path;

/// The lead's standing instructions. Role assignments and the advisor paragraph come from `plan`,
/// so the text Claude receives matches the flags it was launched with.
pub fn system_policy(attention_file: &Path, run_token: &str, plan: &RolePlan) -> String {
    let path = attention_file.display().to_string().replace('\\', "/");
    let assignments = assignment_lines(plan);
    let advisor = advisor_paragraph(plan, run_token);
    format!(
        r#"You are the LEAD of a native Claude Code AGENT TEAM, running in an interactive session.
For genuinely parallel work, spawn named teammates using Claude Code's Agent tool with its `name` parameter (for example `name="coder-backend"`, `name="coder-frontend"`, and `name="explorer-tests"`). When agent teams are enabled in this interactive session, Claude Code creates the native team automatically when it spawns named agents. There is no separate team-creation tool to call or `team_name` parameter to rely on. Use shared tasks and teammate-to-teammate messaging when available. Delegate independent exploration, research, implementation, and tests to teammates with distinct file ownership; wait for results and integrate them. Do not confuse an unnamed, ordinary subagent with a named native teammate. If the Agent tool is unavailable or does not support named teammates, explain the limitation without claiming a native team exists and request human guidance.

{assignments}

{advisor}

Whenever YOU need human clarification or a decision before proceeding, write the following JSON file at {path:?} (outside the repo, provided for the UI):
{{"title":"Short decision title", "reason":"Concise explanation of the blocker and consequences", "question":"Exact question the human must answer", "options":["One option", "Another option"]}}
The application will display a prominent summary and provide an answer field. Await the human's answer in your interactive terminal before proceeding. Do not write the attention file for routine permission prompts; Claude Code already presents and enforces those in its native terminal. You may clear or overwrite the file after the answer. Never invent consent or bypass permissions.

Agentree observes your final-review lifecycle through three EXACT, STANDALONE lines in your assistant text (not through a file, a quoted code snippet, or a tool call). The ID makes the signal specific to this run.
(1) Immediately BEFORE calling the Opus advisor for the final audit, send this line as a separate assistant update:
AGENTREE_REVIEW_START:{run_token}
(2) AFTER receiving the Opus review and resolving actionable findings, include one of these lines in your concluding response:
AGENTREE_REVIEW_RESULT:{run_token}:PASS
AGENTREE_REVIEW_RESULT:{run_token}:NEEDS_CHANGES
AGENTREE_REVIEW_RESULT:{run_token}:BLOCKED
Use PASS only if you actually called Opus for the final review and addressed material findings. If Opus could not be consulted, say so and use BLOCKED. Do not issue a PASS solely because you checked the code yourself.
(3) Once you are done working on this objective, append this EXACT final line:
AGENTREE_OBJECTIVE_COMPLETE:{run_token}
Do not emit completion before work and verification are done, even if individual teammates finish. The terminal session may stay open for later follow-up; these markers let Agentree stop the objective clock without terminating Claude. Never print these markers in preliminary planning or by copying examples. They are not a security boundary: the UI independently looks for an Opus call after review start before marking the review verified.

When a review only finds issues you can fix autonomously, fix them: do NOT ask a human unless a genuine decision, missing information, credential, approval or unresolvable blocker is required. Provide a short end-of-run summary with tests and review findings. Respect project instructions."#
    )
}

/// The model and limits for each named-teammate role, as the lead must follow them.
fn assignment_lines(plan: &RolePlan) -> String {
    let mut text = format!(
        "The lead session runs on {}. Role assignments for named teammates (use the model, concurrency limit and permission mode given for each role):\n",
        plan.lead_model
    );
    for teammate in &plan.teammates {
        text.push_str(&format!(
            "- {}: {}, at most {} at once",
            teammate.role, teammate.model, teammate.max_concurrent
        ));
        if let Some(mode) = &teammate.permission {
            text.push_str(&format!(", permission mode {mode}"));
        }
        text.push('\n');
    }
    text.trim_end().to_string()
}

fn advisor_paragraph(plan: &RolePlan, run_token: &str) -> String {
    match &plan.advisor {
        Some(advisor) => format!(
            "You have access to the built-in Opus advisor tool configured by --advisor {advisor}. ONLY call it if you are genuinely blocked, repeatedly failing to make progress, or face a consequential decision you cannot resolve with your own reasoning or your teammates' findings. Do not call Opus for preflight, initial planning, routine plan approval, or merely to decide teammate assignments. First inspect the project and delegate tasks to teammates. Only consult Opus after concrete evidence of a blocker or a consequential unresolved design choice. At the end, explicitly ask Opus for an independent final review of changed code and validation evidence (one final review only unless you need to re-review corrections). Opus must never edit production code. If final review identifies actionable defects, assign fixes back to teammates and re-review when warranted, with a bounded number of cycles. Don't describe work as complete without verifying it."
        ),
        // Agentree records a review only after its START line, so emit both markers here.
        None => format!(
            "No advisor is configured for this run. Do not call an advisor or describe one as consulted. Work from your own reasoning and your teammates' findings. Opus is not available for the final review, so when you finish, send the line AGENTREE_REVIEW_START:{run_token} as a standalone line, then AGENTREE_REVIEW_RESULT:{run_token}:BLOCKED as a standalone line in your concluding response. Don't describe work as complete without verifying it."
        ),
    }
}

pub fn initial_objective(objective: &str, plan: &RolePlan) -> String {
    let opus = if plan.advisor.is_some() {
        " Do not consult Opus before establishing context and identifying a concrete blocker. Use Opus only on genuine escalation and for a final review; ask me directly when human guidance is needed."
    } else {
        " No advisor is configured for this run. Ask me directly when human guidance is needed."
    };
    format!("Objective:\n{}\n\nFor genuinely parallel work, use the Agent tool to spawn named teammates (set the `name` argument), using the role assignments in your instructions. Claude Code automatically creates the native team in an enabled interactive session; no separate team-creation call is needed. Coordinate using native tasks and messaging, then integrate and test their results.{opus}", objective.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_contains_filepath_and_escalation() {
        let s = system_policy(
            Path::new("C:/scratch/attention.json"),
            "id987",
            &RolePlan::claude_default(),
        );
        assert!(s.contains("attention.json"));
        assert!(s.contains("ONLY call it"));
        assert!(s.contains("Agent tool"));
        assert!(s.contains("`name`"));
        assert!(!s.contains("TeamCreate"));
        assert!(s.contains("AGENTREE_OBJECTIVE_COMPLETE:id987"));
        assert!(s.contains("--advisor opus"));
    }

    #[test]
    fn policy_lists_each_teammate_role_from_the_plan() {
        let mut plan = RolePlan::claude_default();
        plan.teammates[3].model = "claude-sonnet-5-5".into();
        plan.teammates[3].max_concurrent = 2;
        plan.teammates[3].permission = Some("acceptEdits".into());
        let s = system_policy(Path::new("a.json"), "id", &plan);
        assert!(s.contains(
            "- Coder: claude-sonnet-5-5, at most 2 at once, permission mode acceptEdits"
        ));
        assert!(s.contains("- Planner: claude-haiku-5-5, at most 3 at once\n"));
    }

    #[test]
    fn policy_without_an_advisor_does_not_offer_one() {
        let plan = RolePlan {
            advisor: None,
            ..RolePlan::claude_default()
        };
        let s = system_policy(Path::new("a.json"), "id", &plan);
        assert!(!s.contains("ONLY call it"));
        assert!(!s.contains("--advisor"));
        assert!(s.contains("No advisor is configured"));
        assert!(s.contains("AGENTREE_REVIEW_START:id"));
    }

    #[test]
    fn initial_prompt_uses_named_agent_spawning() {
        let text = initial_objective("Build a project", &RolePlan::claude_default());
        assert!(text.contains("Build a project"));
        assert!(text.contains("Agent tool"));
        assert!(text.contains("`name`"));
        assert!(!text.contains("TeamCreate"));
    }

    #[test]
    fn first_message_mentions_opus_only_when_an_advisor_exists() {
        let advisor_off = RolePlan {
            advisor: None,
            ..RolePlan::claude_default()
        };
        let text = initial_objective("Build a project", &advisor_off);
        assert!(!text.contains("Opus"), "{text}");
        assert!(initial_objective("Build a project", &RolePlan::claude_default()).contains("Opus"));
    }

    #[test]
    fn policy_without_an_advisor_emits_the_review_markers_for_blocked() {
        let plan = RolePlan {
            advisor: None,
            ..RolePlan::claude_default()
        };
        let s = system_policy(Path::new("a.json"), "tok", &plan);
        assert!(s.contains("AGENTREE_REVIEW_START:tok as a standalone line"));
        assert!(s.contains("AGENTREE_REVIEW_RESULT:tok:BLOCKED"));
    }
}
