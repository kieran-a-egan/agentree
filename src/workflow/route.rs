//! Automatic path routing and the bridge to the user's role settings.
//!
//! Routing chooses *how much* work a task gets (Fast, Balanced or Thorough). It never chooses a
//! model: [`RouteDecision`] has no model field, and model selection is [`resolve`], which asks
//! the registry. A higher-risk task therefore gets more stages, never a different bulk reader
//! or a different final reviewer.

use super::contract::{
    ExecutionPath, ModelPolicy, RoleClass, Stage, ValidatedWorkflow, WorkflowPolicy,
};
use super::registry::{
    Authorization, BulkSelection, Escalation, EscalationKind, Registry, ReviewSelection,
};
use super::{Code, Violation};
use crate::roles::{Role, Settings};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Complexity {
    Trivial,
    Small,
    Medium,
    Large,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Risk {
    Low,
    Medium,
    High,
    Critical,
}

/// What is known about a task before it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaskProfile {
    pub complexity: Complexity,
    pub risk: Risk,
    pub needs_research: bool,
    pub changes_code: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteDecision {
    pub path: ExecutionPath,
    pub reasons: Vec<String>,
    /// The budget cap lowered the path the task would otherwise have taken.
    pub capped: bool,
    /// Always true when code changes, on every path and at every risk level.
    pub review_required: bool,
}

/// Picks a path from task characteristics. `preferred` is a user floor, e.g. "always Thorough".
/// The budget cap in `policy` lowers the result but cannot remove review.
pub fn route(
    task: &TaskProfile,
    policy: &WorkflowPolicy,
    preferred: Option<ExecutionPath>,
) -> RouteDecision {
    use ExecutionPath::*;
    let mut reasons = Vec::new();
    let mut path = match task.complexity {
        Complexity::Trivial | Complexity::Small => Fast,
        Complexity::Medium => Balanced,
        Complexity::Large => Thorough,
    };
    reasons.push(format!(
        "{:?} complexity starts at {}",
        task.complexity,
        path.label()
    ));
    let floor = match task.risk {
        Risk::Low | Risk::Medium => Fast,
        Risk::High => Balanced,
        Risk::Critical => Thorough,
    };
    if floor > path {
        reasons.push(format!(
            "{:?} risk raises it to {}",
            task.risk,
            floor.label()
        ));
        path = floor;
    }
    if task.needs_research && path < Balanced {
        reasons.push("research is required, so it is at least Balanced".into());
        path = Balanced;
    }
    if let Some(p) = preferred.filter(|p| *p > path) {
        reasons.push(format!("the requested path is {}", p.label()));
        path = p;
    }
    let mut capped = false;
    if let Some(max) = policy.max_path.filter(|m| *m < path) {
        reasons.push(format!(
            "the budget caps it at {}; final review is unaffected",
            max.label()
        ));
        path = max;
        capped = true;
    }
    RouteDecision {
        path,
        reasons,
        capped,
        review_required: task.changes_code,
    }
}

/// The model decision for one stage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolved {
    Bulk(BulkSelection),
    Review(ReviewSelection),
    /// The user's role setting decides; the Claude adapter reads it from `roles::Settings`.
    Configured,
    Pinned(String),
}

/// Resolves the model for `stage` from the registry, for `context_tokens` of input. A bulk or
/// review stage is *always* resolved by its mandatory policy, whatever path or risk brought it
/// here. `Err` is an escalation: nothing may start until a human decides.
pub fn resolve(
    wf: &ValidatedWorkflow,
    stage: &Stage,
    registry: &Registry,
    context_tokens: u32,
    auth: Option<&Authorization>,
) -> Result<Resolved, Escalation> {
    let role = wf
        .role(&stage.role)
        .expect("validated stages name a defined role");
    let req = role.requirement(context_tokens);
    match (role.class, &role.model) {
        (RoleClass::BulkRead, _) => registry
            .select_bulk(&req, wf.def().policy.chunking, auth)
            .map(Resolved::Bulk),
        (RoleClass::Review, _) => registry.select_review(&req, auth).map(Resolved::Review),
        (_, ModelPolicy::Pinned { model }) => match registry.get(model) {
            Some(m) if m.available => Ok(Resolved::Pinned(m.id.clone())),
            _ => Err(Escalation {
                kind: EscalationKind::RetryLater,
                message: format!("pinned model '{model}' is unavailable"),
                options: vec![],
            }),
        },
        _ => Ok(Resolved::Configured),
    }
}

/// Checks the saved role settings against the mandatory policies. The Roles screen lets a user
/// pick any model for any teammate; the workflow contract says bulk readers use the cheapest
/// capable model and the reviewer the strongest. Each mismatch is a violation, so a saved
/// profile cannot quietly override policy.
pub fn check_settings(
    settings: &Settings,
    wf: &ValidatedWorkflow,
    registry: &Registry,
) -> Vec<Violation> {
    let mut out = Vec::new();
    // Claude Code has one model for unnamed subagents and Agentree sets it to the Coder's
    // (`Settings::plan`). Exploration subagents are bulk readers, so a Coder on a pricier
    // model silently moves them off the lowest-cost tier.
    if let Some(bulk) = wf
        .def()
        .roles
        .iter()
        .find(|r| r.class == RoleClass::BulkRead)
    {
        let sub = settings.plan().subagent_model;
        if !registry.is_lowest_cost(&sub, &bulk.requirement(0)) {
            out.push(Violation::new(
                Code::ModelPolicy,
                "settings.coder",
                format!(
                    "Claude runs unnamed subagents on the Coder's model '{sub}', which is not                      the lowest-cost capable model; their bulk reading would be upgraded"
                ),
            ));
        }
    }
    for role_def in &wf.def().roles {
        let Some(role) = Role::from_key(&role_def.id) else {
            continue;
        };
        let at = format!("settings.{}", role.key());
        let configured = settings.get(role).model.as_str();
        let req = role_def.requirement(0);
        match role_def.class {
            RoleClass::BulkRead if !registry.is_lowest_cost(configured, &req) => {
                out.push(Violation::new(
                    Code::ModelPolicy,
                    at,
                    format!(
                        "'{configured}' is not the lowest-cost capable model; bulk-reading roles \
                         must not use a more expensive one"
                    ),
                ));
            }
            RoleClass::Review => {
                let strongest = registry.strongest_reviewer(&req).map(|m| m.id.as_str());
                let chosen = registry.get(configured).map(|m| m.id.as_str());
                if strongest.is_none() || chosen != strongest {
                    out.push(Violation::new(
                        Code::ModelPolicy,
                        at,
                        format!(
                            "'{configured}' is not the strongest eligible reviewer ({})",
                            strongest.unwrap_or("none configured")
                        ),
                    ));
                }
                // On Claude the review runs through the advisor, so its model counts too.
                let advisor = settings.advisor.model.as_str();
                if advisor == "off" {
                    out.push(Violation::new(
                        Code::Review,
                        "settings.advisor",
                        "the advisor is off, so the final review cannot run; review would end BLOCKED",
                    ));
                } else if registry.get(advisor).map(|m| m.id.as_str()) != strongest {
                    out.push(Violation::new(
                        Code::ModelPolicy,
                        "settings.advisor",
                        format!("the advisor '{advisor}' is not the strongest eligible reviewer"),
                    ));
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::contract::{
        default_workflow,
        fixtures::{full_registry as registry, validate},
    };
    use crate::workflow::registry::claude_default;

    fn task(complexity: Complexity, risk: Risk) -> TaskProfile {
        TaskProfile {
            complexity,
            risk,
            needs_research: false,
            changes_code: true,
        }
    }

    #[test]
    fn complexity_risk_and_research_pick_the_path() {
        let p = WorkflowPolicy::default();
        let go = |t| route(&t, &p, None).path;
        assert_eq!(
            go(task(Complexity::Trivial, Risk::Low)),
            ExecutionPath::Fast
        );
        assert_eq!(
            go(task(Complexity::Medium, Risk::Low)),
            ExecutionPath::Balanced
        );
        assert_eq!(
            go(task(Complexity::Large, Risk::Low)),
            ExecutionPath::Thorough
        );
        assert_eq!(
            go(task(Complexity::Small, Risk::High)),
            ExecutionPath::Balanced
        );
        assert_eq!(
            go(task(Complexity::Trivial, Risk::Critical)),
            ExecutionPath::Thorough
        );
        let mut t = task(Complexity::Trivial, Risk::Low);
        t.needs_research = true;
        assert_eq!(go(t), ExecutionPath::Balanced);
        assert_eq!(
            route(&t, &p, Some(ExecutionPath::Thorough)).path,
            ExecutionPath::Thorough
        );
    }

    #[test]
    fn a_budget_cap_lowers_the_path_but_never_removes_review() {
        let p = WorkflowPolicy {
            max_path: Some(ExecutionPath::Fast),
            ..Default::default()
        };
        let d = route(&task(Complexity::Large, Risk::Critical), &p, None);
        assert_eq!(d.path, ExecutionPath::Fast);
        assert!(d.capped);
        assert!(d.review_required);
    }

    #[test]
    fn risk_and_path_never_change_which_models_are_selected() {
        let wf = validate(default_workflow(false)).unwrap();
        let reg = registry();
        let p = WorkflowPolicy::default();
        let mut picked = Vec::new();
        for (c, r) in [
            (Complexity::Trivial, Risk::Low),
            (Complexity::Medium, Risk::High),
            (Complexity::Large, Risk::Critical),
        ] {
            let d = route(&task(c, r), &p, None);
            let mut models = Vec::new();
            for s in wf.stages_for(d.path) {
                match resolve(&wf, s, &reg, 10_000, None).unwrap() {
                    Resolved::Bulk(b) => models.push((s.id.clone(), b.model().to_string())),
                    Resolved::Review(r) => models.push((s.id.clone(), r.model)),
                    _ => {}
                }
            }
            // The reviewer is "top" on every path, every bulk reader "cheap".
            assert!(models.iter().all(|(id, m)| if id == "review" {
                m == "top"
            } else {
                m == "cheap"
            }));
            assert!(models.iter().any(|(id, _)| id == "review"));
            picked.push(models.len());
        }
        // More stages on harder paths, same policy.
        assert!(picked[0] < picked[1] && picked[1] < picked[2]);
    }

    #[test]
    fn trivial_changes_still_get_the_strongest_reviewer() {
        let wf = validate(default_workflow(false)).unwrap();
        let reg = registry();
        let d = route(
            &task(Complexity::Trivial, Risk::Low),
            &WorkflowPolicy::default(),
            None,
        );
        let review = wf
            .stages_for(d.path)
            .into_iter()
            .find(|s| s.id == "review")
            .unwrap();
        match resolve(&wf, review, &reg, 500, None).unwrap() {
            Resolved::Review(r) => assert_eq!(r.model, "top"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_unavailable_strongest_reviewer_blocks_the_stage() {
        let wf = validate(default_workflow(false)).unwrap();
        let mut reg = registry();
        reg.set_available("top", false);
        let review = wf.review_stage().unwrap();
        let err = resolve(&wf, review, &reg, 500, None).unwrap_err();
        assert_eq!(err.kind, EscalationKind::NeedsAuthorization);
    }

    #[test]
    fn bulk_stages_escalate_instead_of_upgrading() {
        let wf = validate(default_workflow(false)).unwrap();
        let mut reg = registry();
        reg.set_available("cheap", false);
        let explore = wf.stages_for(ExecutionPath::Fast)[0];
        let err = resolve(&wf, explore, &reg, 1_000, None).unwrap_err();
        assert_eq!(err.kind, EscalationKind::NeedsAuthorization);
        assert!(resolve(
            &wf,
            explore,
            &reg,
            1_000,
            Some(&Authorization {
                model: "top".into()
            })
        )
        .is_ok());
    }

    #[test]
    fn settings_that_override_the_mandatory_policies_are_flagged() {
        let wf = validate(default_workflow(false)).unwrap();
        let reg = claude_default();
        let mut s = Settings::default();
        assert!(check_settings(&s, &wf, &reg).is_empty());

        s.explorer.model = crate::roles::MODELS[2].into();
        let v = check_settings(&s, &wf, &reg);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].at, "settings.explorer");

        let mut s = Settings::default();
        s.final_reviewer.model = "sonnet".into();
        s.advisor.model = "off".into();
        let v = check_settings(&s, &wf, &reg);
        assert_eq!(v.len(), 2);
        assert!(v.iter().any(|v| v.code == Code::Review));

        // The Coder's model is what unnamed subagents (bulk readers) run on.
        let mut s = Settings::default();
        s.coder.model = crate::roles::MODELS[2].into();
        let v = check_settings(&s, &wf, &reg);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].at, "settings.coder");

        // A weaker advisor than the strongest reviewer is flagged too.
        let mut s = Settings::default();
        let mut weak = claude_default();
        weak.models[2].intelligence = 0;
        s.advisor.model = "opus".into();
        assert!(check_settings(&s, &wf, &weak)
            .iter()
            .any(|v| v.at == "settings.advisor"));
    }

    #[test]
    fn built_in_profiles_that_conflict_with_the_policies_are_reported_not_hidden() {
        let wf = validate(default_workflow(false)).unwrap();
        let reg = claude_default();
        let check = |name| check_settings(&crate::profiles::built_in(name).unwrap(), &wf, &reg);
        assert!(check("Balanced").is_empty());
        // Thorough runs the Coder (and so unnamed subagents) on a pricier model.
        assert!(check("Thorough").iter().any(|v| v.at == "settings.coder"));
        // Local turns the advisor off, so there is no final review.
        assert!(check("Local").iter().any(|v| v.code == Code::Review));
    }

    #[test]
    fn native_team_launch_plan_is_unchanged_by_workflow_validation() {
        use crate::capability::{preflight, Discovery, RolePlan};
        let wf = validate(default_workflow(false)).unwrap();
        let settings = Settings::default();
        let before = settings.plan();
        assert!(check_settings(&settings, &wf, &claude_default()).is_empty());
        assert_eq!(settings.plan(), before);
        assert_eq!(before, RolePlan::claude_default());
        let discovery = Discovery {
            cli: Some("claude".into()),
            rows: vec![],
        };
        assert!(preflight(&before, &discovery).is_empty());
    }

    #[test]
    fn the_claude_registry_ranks_opus_strongest_without_naming_it_in_logic() {
        let reg = claude_default();
        assert!(reg.validate().is_empty());
        let req = crate::workflow::registry::fixtures::req(1000);
        assert_eq!(reg.strongest_reviewer(&req).unwrap().aliases, ["opus"]);
        assert_eq!(
            reg.select_bulk(&req, None, None).unwrap().model(),
            crate::roles::MODELS[0]
        );
    }
}
