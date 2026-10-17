//! Claude native-team adapter (Issue #11, v0.6).
//!
//! Turns a [`ValidatedWorkflow`], the saved role settings, the model registry and the trusted
//! Agent Plugins into what the existing launch already accepts: a [`RolePlan`] (launch flags and
//! environment) and the role section of the lead's instructions. It starts nothing and adds no
//! scheduler: one Claude coordinator and its named teammates, as before.
//!
//! Policy comes from the contracts, never from this file: `route::route` picks the path,
//! `route::resolve` and the registry pick models, `route::check_settings` finds saved settings that
//! contradict policy, and `Store::activate` is the only way a skill is offered. What Claude Code
//! can technically enforce is listed in [`Brief::guarantees`]; everything else is an instruction
//! and is described as one.

use crate::capability::{RolePlan, ADVISORS};
use crate::plugins::{self, Plugin, Store};
use crate::roles::{Role, Settings};
use crate::workflow::contract::{
    self, BackendProfile, ExecutionPath, PluginRef, RoleClass, Stage, ValidatedWorkflow,
    WorkflowDef,
};
use crate::workflow::registry::{
    self, Authorization, BulkSelection, Escalation, Permission, Registry,
};
use crate::workflow::route::{self, Complexity, Resolved, Risk, RouteDecision, TaskProfile};
use crate::workflow::{Code, Enforcement, Violation};
use std::{
    collections::BTreeSet,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

/// The plugin the built-in workflow draws its skills from.
const BUILTIN_PLUGIN: &str = "software-engineering";

/// Everything the adapter reads. Nothing here is mutated.
pub struct Request<'a> {
    pub workflow: &'a ValidatedWorkflow,
    pub settings: &'a Settings,
    pub registry: &'a Registry,
    pub plugins: &'a [Plugin],
    pub trust: &'a Store,
    pub task: TaskProfile,
    pub preferred: Option<ExecutionPath>,
    /// Tokens of bulk input to plan for. 0 when unknown.
    pub context_tokens: u32,
    /// A human's explicit choice of a pricier bulk reader. `None` unless a human named a model.
    pub bulk_auth: Option<&'a Authorization>,
    /// A human's explicit choice of a weaker final reviewer.
    pub review_auth: Option<&'a Authorization>,
}

/// A skill offered to a role. Exists only if its plugin passed `Store::activate` just now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillUse {
    pub plugin: String,
    pub version: Option<String>,
    pub name: String,
    pub file: PathBuf,
    /// First characters of the plugin fingerprint that was trusted.
    pub fingerprint: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleBrief {
    /// The workflow role id.
    pub id: String,
    pub label: String,
    pub stages: Vec<String>,
    pub model: String,
    /// Why this model: policy, saved setting or pin.
    pub why: String,
    pub max_concurrent: u8,
    pub access: Permission,
    pub tools: BTreeSet<String>,
    /// Saved permission mode handed to the lead as an instruction.
    pub mode: Option<String>,
    pub skill: Option<SkillUse>,
    /// Trusted MCP servers the role names. Agentree cannot pass them to Claude Code.
    pub mcp: Vec<String>,
    pub bulk: bool,
    pub attempts: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewBrief {
    /// Registry id of the reviewer.
    pub model: String,
    pub strongest: String,
    pub authorized_downgrade: bool,
    /// The `--advisor` value that reaches it. `None`: the advisor is off and review ends BLOCKED.
    pub advisor: Option<String>,
    pub max_inspections: u8,
    pub inspector: String,
    pub skill: Option<SkillUse>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Guarantee {
    pub policy: &'static str,
    pub level: Enforcement,
    pub note: String,
}

#[derive(Clone, Debug)]
pub struct Brief {
    pub workflow: String,
    pub plan: RolePlan,
    pub decision: RouteDecision,
    /// (stage, role) in dependency order for the chosen path.
    pub stages: Vec<(String, String)>,
    pub roles: Vec<RoleBrief>,
    pub review: Option<ReviewBrief>,
    pub advisor_skill: Option<SkillUse>,
    /// Saved settings that contradict a mandatory policy.
    pub conflicts: Vec<Violation>,
    /// What the adapter changed or could not load, for the human.
    pub notices: Vec<String>,
    pub guarantees: Vec<Guarantee>,
    chunking: String,
}

impl Brief {
    pub fn limits(&self) -> usize {
        self.guarantees
            .iter()
            .filter(|g| g.level < Enforcement::Enforced)
            .count()
    }

    /// One line for the status bar.
    pub fn status_line(&self) -> String {
        format!(
            "Workflow: {} path, {} policy conflict(s), {} limit(s) Claude Code cannot enforce (adapter-report.txt)",
            self.decision.path.label(),
            self.conflicts.len(),
            self.limits()
        )
    }

    /// For the human: what is enforced, what is only asked, what conflicts, what changed.
    pub fn report(&self) -> String {
        let mut t = format!(
            "Workflow '{}', {} path: {}\n\nEnforcement by Claude Code native teams:\n",
            self.workflow,
            self.decision.path.label(),
            self.decision.reasons.join("; ")
        );
        for g in &self.guarantees {
            let _ = writeln!(t, "  [{}] {}: {}", g.level.label(), g.policy, g.note);
        }
        if !self.conflicts.is_empty() {
            t.push_str("\nSaved settings that conflict with workflow policy:\n");
            for c in &self.conflicts {
                let _ = writeln!(t, "  {c}");
            }
        }
        if !self.notices.is_empty() {
            t.push_str("\nNotices:\n");
            for n in &self.notices {
                let _ = writeln!(t, "  {n}");
            }
        }
        t
    }

    /// The role section of the lead's instructions. It replaces the fixed role list.
    pub fn assignments(&self) -> String {
        let mut t = format!(
            "The lead session runs on {}. Workflow '{}', {} path ({}). Stages in order: {}.\n",
            self.plan.lead_model,
            self.workflow,
            self.decision.path.label(),
            self.decision.reasons.join("; "),
            self.stages
                .iter()
                .map(|(s, r)| format!("{s} ({r})"))
                .collect::<Vec<_>>()
                .join(" > ")
        );
        t.push_str("Role assignments for named teammates. Claude Code does not apply a teammate's model, tools or permission mode, so state them in each teammate's prompt; Agentree cannot confirm they were followed:\n");
        for r in &self.roles {
            let _ = write!(
                t,
                "- {}: {}, at most {} at once",
                r.label, r.model, r.max_concurrent
            );
            if let Some(mode) = &r.mode {
                let _ = write!(t, ", permission mode {mode}");
            }
            let tools: Vec<&str> = r.tools.iter().map(|c| claude_tools(c)).collect();
            let _ = writeln!(
                t,
                "\n  Stages: {}. Model: {}. Access: {}; capabilities {} (Claude tools: {}).",
                r.stages.join(", "),
                r.why,
                if r.access == Permission::Edit {
                    "may edit"
                } else {
                    "read-only, must not edit files"
                },
                r.tools.iter().cloned().collect::<Vec<_>>().join(", "),
                tools.join(", ")
            );
            if let Some(s) = &r.skill {
                let _ = writeln!(t, "  {}", skill_line("Skill", s, true));
            }
        }
        if self.roles.iter().any(|r| r.bulk) {
            let bulk: Vec<&str> = self
                .roles
                .iter()
                .filter(|r| r.bulk)
                .map(|r| r.label.as_str())
                .collect();
            let attempts = self.roles.iter().filter(|r| r.bulk).map(|r| r.attempts);
            let _ = write!(
                t,
                "\nBulk reading: all high-token work (repository exploration, large files, documentation research, log examination, scanning, evidence extraction) goes to {} on the lowest-cost capable model listed above. Do not do it yourself on the lead model and do not use unnamed subagents for it. When you spawn a bulk reader, use exactly the listed model and pass no other model override. Teammates return compact evidence (claim, file, line range, source hash), not file contents. {} A failed read may be retried at most {} more time(s), then ask the human. Never move bulk reading to a more expensive model on your own: if the listed model cannot do the work, write the attention file and ask the human to authorize a named model. Claude Code cannot enforce this delegation.\n",
                bulk.join(" and "),
                self.chunking,
                attempts.min().unwrap_or(1).saturating_sub(1)
            );
        }
        match &self.review {
            Some(r) => t.push_str(&self.review_text(r)),
            None => t.push_str("\nThis workflow has no review stage.\n"),
        }
        if let Some(s) = &self.advisor_skill {
            let _ = writeln!(
                t,
                "\n{}",
                skill_line(
                    "Advisor skill (for blockers, not the final review)",
                    s,
                    false
                )
            );
        }
        if !self.conflicts.is_empty() || !self.notices.is_empty() {
            t.push_str("\nAgentree reported these to the human at launch; do not hide them:\n");
            for c in &self.conflicts {
                let _ = writeln!(t, "- Saved setting conflict, {c}");
            }
            for n in &self.notices {
                let _ = writeln!(t, "- {n}");
            }
        }
        t.push_str("\nWhat Claude Code does not enforce. Never tell the human any of these is technically guaranteed:\n");
        for g in self
            .guarantees
            .iter()
            .filter(|g| g.level < Enforcement::Enforced)
        {
            let _ = writeln!(t, "- {}: {} ({})", g.policy, g.level.label(), g.note);
        }
        t.trim_end().to_string()
    }

    fn review_text(&self, r: &ReviewBrief) -> String {
        let mut t = format!(
            "\nFinal review: every change to code, on every path, ends with an independent review by the strongest eligible model in the model registry, {}. ",
            r.model
        );
        if r.authorized_downgrade {
            let _ = write!(
                t,
                "A human authorized this weaker reviewer because {} is unavailable. ",
                r.strongest
            );
        }
        match &r.advisor {
            Some(alias) => {
                let _ = write!(
                    t,
                    "Consult it through the advisor (--advisor {alias}). Before you do, read the review skill and put the review packet into the conversation as visible text: the real diff (git diff output, not a summary), the objective, the acceptance criteria, and the verification evidence (each command, its exit code and result). If it asks for more, answer at most {} targeted inspection request(s), each a specific file and line range read by {}. Report its verdict as it gave it; never soften or invent one, and never answer PASS yourself. The advisor sees your whole conversation, not an isolated packet, so reviewer independence is not guaranteed. ",
                    r.max_inspections, r.inspector
                );
            }
            None => {
                t.push_str("The advisor is off, so that model cannot be consulted: the review ends BLOCKED. Do not substitute another model or review it yourself and call it independent. ");
            }
        }
        if let Some(s) = &r.skill {
            let _ = write!(t, "{}", skill_line("Review skill", s, false));
        }
        t.push('\n');
        t
    }
}

fn skill_line(what: &str, s: &SkillUse, teammate: bool) -> String {
    let file = s.file.display().to_string().replace('\\', "/");
    let action = if teammate {
        format!("Include in this teammate's prompt: \"Read {file} before starting and follow it.\"")
    } else {
        format!("Read {file} before you start and follow it.")
    };
    format!(
        "{what}: {}/{} (trusted plugin {} version {}, contents {}). {action} Claude Code does not load plugin skills for named teammates or the advisor.",
        s.plugin,
        s.name,
        s.plugin,
        s.version.as_deref().unwrap_or("unversioned"),
        s.fingerprint,
    )
}

/// The Claude Code tools a workflow capability stands for.
fn claude_tools(capability: &str) -> &'static str {
    match capability {
        "read" => "Read",
        "search" => "Grep, Glob",
        "shell" => "Bash, PowerShell",
        "edit" => "Edit, Write",
        "web" => "WebFetch, WebSearch",
        "mcp" => "MCP tools",
        _ => "none",
    }
}

fn escalated(code: Code, at: &str, e: &Escalation) -> Violation {
    let mut message = e.message.clone();
    if !e.options.is_empty() {
        let _ = write!(
            message,
            " A human may authorize one of: {}.",
            e.options.join(", ")
        );
    }
    message.push_str(
        " Nothing launches until then: Agentree does not pick a different model for you.",
    );
    Violation::new(code, at, message)
}

/// The plugin for `r`, only if it is loaded exactly once and passes the trust gate right now.
fn activate<'a>(
    r: &PluginRef,
    all: &'a [Plugin],
    trust: &Store,
) -> Result<(&'a Plugin, plugins::Activation<'a>), String> {
    let named: Vec<&Plugin> = all.iter().filter(|p| p.manifest.name == r.plugin).collect();
    let plugin = match named.as_slice() {
        [p] => *p,
        [] => return Err(format!("plugin '{}' is not loaded", r.plugin)),
        many => {
            return Err(format!(
                "{} loaded plugins are named '{}'",
                many.len(),
                r.plugin
            ))
        }
    };
    trust
        .activate(plugin)
        .map(|a| (plugin, a))
        .ok_or_else(|| {
            format!(
                "plugin '{}' is {}. Review it, then trust it with: agentree --plugin \"{}\" --plugin-trust",
                r.plugin,
                trust.state(plugin).label(),
                plugin.root.display()
            )
        })
}

fn skill_use(r: &PluginRef, all: &[Plugin], trust: &Store) -> Result<SkillUse, String> {
    let (plugin, active) = activate(r, all, trust)?;
    let skill = active
        .skills
        .iter()
        .find(|s| s.name == r.name)
        .ok_or_else(|| format!("trusted plugin '{}' has no skill '{}'", r.plugin, r.name))?;
    Ok(SkillUse {
        plugin: plugin.manifest.name.clone(),
        version: plugin.manifest.version.clone(),
        name: skill.name.clone(),
        file: skill.dir.join("SKILL.md"),
        fingerprint: plugin.fingerprint.chars().take(12).collect(),
    })
}

fn mcp_names(r: &PluginRef, all: &[Plugin], trust: &Store) -> Result<String, String> {
    let (_, active) = activate(r, all, trust)?;
    active
        .mcp
        .iter()
        .find(|m| m.name == r.name)
        .map(|m| format!("{}/{}", r.plugin, m.name))
        .ok_or_else(|| {
            format!(
                "trusted plugin '{}' has no MCP server '{}'",
                r.plugin, r.name
            )
        })
}

/// The `--advisor` value that runs `model`, if Claude Code's advisor can run it at all.
fn advisor_alias(registry: &Registry, model: &str) -> Option<String> {
    let entry = registry.get(model)?;
    entry
        .aliases
        .iter()
        .chain(std::iter::once(&entry.id))
        .find(|a| ADVISORS.contains(&a.as_str()))
        .cloned()
}

fn label_of(id: &str) -> String {
    Role::from_key(id).map_or_else(|| id.to_string(), |r| r.label().to_string())
}

fn describe_bulk(sel: &BulkSelection) -> String {
    match sel {
        BulkSelection::Whole { authorized: true, .. } => {
            "a pricier model a human explicitly authorized for bulk reading".into()
        }
        BulkSelection::Whole { .. } => "lowest-cost capable model in the model registry".into(),
        BulkSelection::Chunked { chunks, chunk_tokens, .. } => format!(
            "lowest-cost capable model in the model registry, reading in {chunks} windows of {chunk_tokens} tokens"
        ),
    }
}

/// Builds the launch brief, or every reason the launch must not happen.
pub fn adapt(req: &Request) -> Result<Brief, Vec<Violation>> {
    let wf = req.workflow;
    let def = wf.def();
    let decision = route::route(&req.task, &def.policy, req.preferred);
    let on_path = wf.stages_for(decision.path);
    let mut plan = req.settings.plan();
    let (mut errors, mut conflicts, mut notices) = (Vec::new(), Vec::new(), Vec::new());
    let (mut roles, mut bulk_models): (Vec<RoleBrief>, Vec<String>) = (Vec::new(), Vec::new());
    let mut chunking =
        "Chunking is not permitted: if the input does not fit, ask the human.".to_string();
    let skill_for = |label: &str, r: &Option<PluginRef>, notices: &mut Vec<String>| {
        let r = r.as_ref()?;
        skill_use(r, req.plugins, req.trust)
            .map_err(|why| {
                notices.push(format!(
                    "{label}: skill {}/{} not loaded: {why}",
                    r.plugin, r.name
                ))
            })
            .ok()
    };

    for rd in def.roles.iter().filter(|r| r.class != RoleClass::Review) {
        let stages: Vec<&Stage> = on_path
            .iter()
            .copied()
            .filter(|s| s.role == rd.id)
            .collect();
        let Some(first) = stages.first() else {
            continue;
        };
        let at = format!("roles[{}]", rd.id);
        let setting_role = Role::from_key(&rd.id).filter(|r| r.is_teammate());
        let resolved =
            match route::resolve(wf, first, req.registry, req.context_tokens, req.bulk_auth) {
                Ok(r) => r,
                Err(e) => {
                    errors.push(escalated(Code::ModelPolicy, &at, &e));
                    continue;
                }
            };
        let (model, why) = match (resolved, setting_role) {
            (Resolved::Bulk(sel), _) => {
                if let (BulkSelection::Chunked { chunk_tokens, .. }, Some(p)) =
                    (&sel, def.policy.chunking)
                {
                    chunking = format!(
                        "If an input exceeds {chunk_tokens} tokens, split it into windows of at most {chunk_tokens} tokens, at most {} windows in all.",
                        p.max_chunks
                    );
                } else if let Some(p) = def.policy.chunking {
                    let context = req
                        .registry
                        .get(sel.model())
                        .map_or(0, |m| m.context_tokens);
                    chunking = format!(
                        "If an input exceeds {} tokens, split it into windows of at most that size, at most {} windows in all.",
                        context.saturating_sub(p.reserve_tokens),
                        p.max_chunks
                    );
                }
                bulk_models.push(sel.model().to_string());
                (sel.model().to_string(), describe_bulk(&sel))
            }
            (Resolved::Pinned(m), _) => (m, "pinned by the workflow".into()),
            (Resolved::Configured, Some(r)) => (
                req.settings.get(r).model.clone(),
                "saved role setting".into(),
            ),
            _ => {
                errors.push(Violation::new(
                    Code::ModelPolicy,
                    &at,
                    "no saved model setting exists for this role",
                ));
                continue;
            }
        };
        let saved = setting_role.map(|r| req.settings.get(r));
        if let Some(s) = saved.filter(|s| {
            req.registry.get(&s.model).map(|m| &m.id) != req.registry.get(&model).map(|m| &m.id)
        }) {
            notices.push(format!(
                "{}: saved model {} replaced by {model} (workflow policy)",
                label_of(&rd.id),
                s.model
            ));
        }
        let concurrent = saved.map_or(rd.max_concurrent, |s| s.concurrency.min(rd.max_concurrent));
        if let Some(s) = saved.filter(|s| s.concurrency > concurrent) {
            notices.push(format!(
                "{}: saved concurrency {} lowered to the workflow's limit of {concurrent}",
                label_of(&rd.id),
                s.concurrency
            ));
        }
        let mut mode = saved
            .map(|s| s.permission.clone())
            .filter(|m| !m.is_empty());
        if rd.permission == Permission::ReadOnly && mode.as_deref() == Some("acceptEdits") {
            conflicts.push(Violation::new(
                Code::Capability,
                format!("settings.{}", rd.id),
                "permission mode acceptEdits lets edits through but this role is read-only; it is not passed on",
            ));
            mode = None;
        }
        if let Some(t) =
            setting_role.and_then(|r| plan.teammates.iter_mut().find(|t| t.role == r.label()))
        {
            t.model = model.clone();
            t.max_concurrent = concurrent;
            t.permission = mode.clone();
        }
        let mut mcp = Vec::new();
        for m in &rd.mcp {
            match mcp_names(m, req.plugins, req.trust) {
                Ok(n) => mcp.push(n),
                Err(why) => notices.push(format!(
                    "{}: MCP server {}/{} not available: {why}",
                    label_of(&rd.id),
                    m.plugin,
                    m.name
                )),
            }
        }
        roles.push(RoleBrief {
            id: rd.id.clone(),
            label: label_of(&rd.id),
            stages: stages.iter().map(|s| s.id.clone()).collect(),
            model,
            why,
            max_concurrent: concurrent,
            access: rd.permission,
            tools: rd.tools.clone(),
            mode,
            skill: skill_for(&label_of(&rd.id), &rd.skill, &mut notices),
            mcp,
            bulk: rd.class == RoleClass::BulkRead,
            attempts: first.policy.max_attempts,
        });
    }
    // Unnamed subagents have one model in Claude Code. Bulk reading can land there, so it takes
    // the dearest bulk model (capable of every bulk role) instead of the Coder's.
    let cost = |m: &String| req.registry.get(m).map_or(0, |e| e.cost);
    if let Some(model) = bulk_models.iter().max_by_key(|m| cost(m)) {
        if &plan.subagent_model != model {
            notices.push(format!(
                "Unnamed subagents run on {model} (lowest-cost capable model), not on the Coder's {}",
                plan.subagent_model
            ));
        }
        plan.subagent_model = model.clone();
    }

    // The final review: strongest eligible model, never silently weaker.
    let mut review = None;
    if let Some(stage) = wf.review_stage() {
        let rd = wf
            .role(&stage.role)
            .expect("validated stages name a defined role");
        let at = format!("roles[{}]", rd.id);
        match route::resolve(wf, stage, req.registry, req.context_tokens, req.review_auth) {
            Ok(Resolved::Review(sel)) => match advisor_alias(req.registry, &sel.model) {
                None => errors.push(Violation::new(
                    Code::Review,
                    &at,
                    format!(
                        "Claude Code's advisor accepts only {}; it cannot run '{}'. A weaker reviewer needs a human's explicit authorization. Nothing launches until then.",
                        ADVISORS.join(", "),
                        sel.model
                    ),
                )),
                Some(alias) => {
                    // An advisor the user turned off stays off: review then ends BLOCKED.
                    if plan.advisor.is_some() {
                        plan.advisor = Some(alias);
                    }
                    review = Some(ReviewBrief {
                        model: sel.model,
                        strongest: sel.strongest,
                        authorized_downgrade: sel.authorized_downgrade,
                        advisor: plan.advisor.clone(),
                        max_inspections: stage.inspection.as_ref().map_or(0, |i| i.max_requests),
                        inspector: stage
                            .inspection
                            .as_ref()
                            .map_or("a bulk reader".into(), |i| label_of(&i.reader_role)),
                        skill: skill_for("Final reviewer", &rd.skill, &mut notices),
                    });
                }
            },
            Ok(_) => errors.push(Violation::new(Code::Review, &at, "the review role did not resolve to a reviewer")),
            Err(e) => errors.push(escalated(Code::Review, &at, &e)),
        }
    } else if decision.review_required {
        errors.push(Violation::new(
            Code::Review,
            "stages",
            "the task changes code but the workflow has no final review stage",
        ));
    }

    // The advisor role has no stage; it uses the review skill's plugin.
    let advisor_skill = Role::Advisor.skill().and_then(|name| {
        let plugin = wf
            .review_stage()
            .and_then(|s| wf.role(&s.role))
            .and_then(|r| r.skill.as_ref())
            .map_or(BUILTIN_PLUGIN, |s| s.plugin.as_str());
        let r = PluginRef {
            plugin: plugin.into(),
            name: name.into(),
        };
        skill_use(&r, req.plugins, req.trust)
            .map_err(|why| {
                notices.push(format!("Advisor: skill {plugin}/{name} not loaded: {why}"))
            })
            .ok()
    });

    // The Coder's model sets the unnamed-subagent model unless a bulk model replaced it above;
    // then that conflict is resolved and the notice already says so.
    let resolved = !bulk_models.is_empty();
    conflicts.extend(
        route::check_settings(req.settings, wf, req.registry)
            .into_iter()
            .filter(|v| !(resolved && v.at == "settings.coder")),
    );
    let guarantees = guarantees(&BackendProfile::claude_native(), &plan, &roles);
    if def.policy.strict_enforcement {
        errors.extend(conflicts.iter().cloned());
        errors.extend(
            guarantees
                .iter()
                .filter(|g| g.level == Enforcement::Unsupported)
                .map(|g| {
                    Violation::new(
                        Code::Enforcement,
                        "policy",
                        format!("{}: {}", g.policy, g.note),
                    )
                }),
        );
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(Brief {
        workflow: def.name.clone(),
        plan,
        decision,
        stages: on_path
            .iter()
            .map(|s| (s.id.clone(), label_of(&s.role)))
            .collect(),
        roles,
        review,
        advisor_skill,
        conflicts,
        notices,
        guarantees,
        chunking,
    })
}

/// What Claude Code native teams enforce for this launch. Prompt text is never `Enforced`.
fn guarantees(b: &BackendProfile, plan: &RolePlan, roles: &[RoleBrief]) -> Vec<Guarantee> {
    let g = |policy, level, note: &str| Guarantee {
        policy,
        level,
        note: note.into(),
    };
    let mut out = vec![
        g(
            "Lead model, effort, permission mode and advisor",
            Enforcement::Enforced,
            "launch flags --model, --effort, --permission-mode and --advisor",
        ),
        Guarantee {
            policy: "Unnamed-subagent model",
            level: Enforcement::Instructed,
            note: format!(
                "CLAUDE_CODE_SUBAGENT_MODEL={} is set at launch as the default; a per-call model or an agent definition's model overrides it, it does not cover named teammates, and Agentree has not observed it in a live session",
                plan.subagent_model
            ),
        },
        g("Teammate models (cheapest bulk reader)", b.role_model, "written into the lead's instructions; Claude Code does not apply a model per named teammate"),
        g("Teammate tools and permission modes", b.role_permission, "written into the lead's instructions; not applied per teammate"),
        g("Bulk reading delegated away from the lead", b.coordinator_bulk_reading, "the lead is asked to delegate; it can still read on its own model"),
        g("Final review cannot be skipped", b.review_gate, "the review markers are observed after the fact; the lead can skip the review"),
        g("Reviewer independence and packet isolation", b.review_independence, "the advisor sees the lead's whole conversation, not a separate packet"),
        g("Skill loading", Enforcement::Instructed, "Agentree checks plugin trust at launch (Store::activate) and gives the lead the SKILL.md path; Claude Code does not load skills for named teammates"),
        g("Git writes need human authorization", Enforcement::Instructed, "stated in the lead's instructions; Claude Code's own permission prompts still apply"),
    ];
    if roles.iter().any(|r| !r.mcp.is_empty()) {
        out.push(g(
            "MCP servers of trusted plugins",
            Enforcement::Unsupported,
            "Agentree does not pass plugin MCP servers to Claude Code in this release",
        ));
    }
    out
}

/// Where the built-in plugin and the saved trust live.
#[derive(Clone, Debug)]
pub struct Dirs {
    /// Folders to try for the bundled plugin, best first.
    pub plugin_candidates: Vec<PathBuf>,
    pub plugin_data: PathBuf,
    pub trust_file: PathBuf,
}

impl Dirs {
    /// `home` is Agentree's data folder. `install.ps1` copies the plugin to `home\plugins`.
    pub fn standard(home: &Path) -> Dirs {
        let mut c: Vec<PathBuf> = std::env::var_os("AGENTREE_PLUGIN_DIR")
            .map(PathBuf::from)
            .into_iter()
            .collect();
        c.push(home.join("plugins").join(BUILTIN_PLUGIN));
        if let Some(exe) = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(Path::to_path_buf))
        {
            c.push(exe.join("plugins").join(BUILTIN_PLUGIN));
            // target\release or target\debug inside a checkout.
            c.push(
                exe.join("..")
                    .join("..")
                    .join("plugins")
                    .join(BUILTIN_PLUGIN),
            );
        }
        c.push(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("plugins")
                .join(BUILTIN_PLUGIN),
        );
        Dirs {
            plugin_candidates: c,
            plugin_data: home.join("plugin-data"),
            trust_file: home.join("plugins.json"),
        }
    }
}

/// Plugins loaded for a run and the saved trust. Loading starts nothing.
pub struct Env {
    pub plugins: Vec<Plugin>,
    pub trust: Store,
    pub notices: Vec<String>,
}

/// Locates and loads the bundled plugin. A plugin that cannot be found or loaded is reported
/// with the supported fix and simply provides nothing.
pub fn environment(dirs: &Dirs) -> Env {
    let mut notices = Vec::new();
    let trust = Store::load(&dirs.trust_file);
    notices.extend(trust.warning());
    let mut loaded = Vec::new();
    match dirs.plugin_candidates.iter().find(|d| d.join("plugin.json").is_file()) {
        Some(dir) => match plugins::load(dir, &dirs.plugin_data) {
            Ok(p) => loaded.push(p),
            Err(e) => notices.push(format!(
                "The bundled plugin at {} was rejected ({e}); no skills are loaded. Reinstall it with install.ps1.",
                dir.display()
            )),
        },
        None => notices.push(format!(
            "The bundled '{BUILTIN_PLUGIN}' plugin was not found (looked in: {}); no skills are loaded. Re-run install.ps1, which copies it to {}, or set AGENTREE_PLUGIN_DIR to its folder.",
            dirs.plugin_candidates
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join("; "),
            dirs.plugin_candidates.first().map_or("the Agentree data folder".into(), |p| p.display().to_string()),
        )),
    }
    Env {
        plugins: loaded,
        trust,
        notices,
    }
}

/// Validates against Claude Code native teams and the plugins in `env`.
pub fn validate(
    def: WorkflowDef,
    registry: &Registry,
    env: &Env,
) -> Result<ValidatedWorkflow, Vec<Violation>> {
    def.validate(&contract::Context {
        registry,
        backend: &BackendProfile::claude_native(),
        plugins: &env.plugins,
        trust: &env.trust,
    })
}

fn task_for(path: Option<ExecutionPath>) -> TaskProfile {
    TaskProfile {
        complexity: match path {
            Some(ExecutionPath::Fast) => Complexity::Small,
            Some(ExecutionPath::Thorough) => Complexity::Large,
            _ => Complexity::Medium,
        },
        risk: Risk::Medium,
        needs_research: false,
        // Any objective may change code, so review is always required.
        changes_code: true,
    }
}

pub fn parse_path(s: &str) -> Result<ExecutionPath, String> {
    contract::PATHS
        .into_iter()
        .find(|p| p.label().eq_ignore_ascii_case(s))
        .ok_or_else(|| "expected fast, balanced or thorough".into())
}

fn join(v: &[Violation]) -> String {
    v.iter()
        .map(|x| x.to_string())
        .collect::<Vec<_>>()
        .join(" | ")
}

/// The launch entry point: the workflow in `file` (or the built-in one), the saved settings and
/// the claude registry become a [`Brief`]. `Err` is the text that blocks the launch.
///
/// The built-in workflow asks for its skills. If their plugin is missing, untrusted or changed,
/// it runs without them and says so; a workflow file the user supplied gets no such fallback.
pub fn prepare(
    settings: &Settings,
    file: Option<&Path>,
    path: Option<ExecutionPath>,
    dirs: &Dirs,
) -> Result<Brief, String> {
    let registry = registry::claude_default();
    let env = environment(dirs);
    let mut notices = env.notices.clone();
    let workflow = match file {
        Some(file) => {
            let text = fs::read_to_string(file)
                .map_err(|e| format!("Workflow {} could not be read: {e}", file.display()))?;
            let def =
                contract::parse(&text).map_err(|v| format!("Workflow {}: {v}", file.display()))?;
            validate(def, &registry, &env)
                .map_err(|v| format!("Workflow {} is not valid: {}", file.display(), join(&v)))?
        }
        None => match validate(contract::default_workflow(true), &registry, &env) {
            Ok(w) => w,
            Err(v) if v.iter().all(|x| x.code == Code::Plugin) => {
                notices.extend(v.iter().map(|x| format!("Skills not loaded: {x}")));
                validate(contract::default_workflow(false), &registry, &env)
                    .map_err(|v| format!("The built-in workflow is not valid: {}", join(&v)))?
            }
            Err(v) => return Err(format!("The built-in workflow is not valid: {}", join(&v))),
        },
    };
    let mut brief = adapt(&Request {
        workflow: &workflow,
        settings,
        registry: &registry,
        plugins: &env.plugins,
        trust: &env.trust,
        task: task_for(path),
        preferred: None,
        context_tokens: 0,
        bulk_auth: None,
        review_auth: None,
    })
    .map_err(|v| format!("Launch blocked by workflow policy: {}", join(&v)))?;
    notices.extend(std::mem::take(&mut brief.notices));
    brief.notices = notices;
    Ok(brief)
}

#[cfg(test)]
mod tests;
