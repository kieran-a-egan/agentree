use super::*;
use crate::roles::MODELS;
use crate::workflow::contract::{default_workflow, fixtures};
use crate::workflow::contract::{Input, OutputRef, StagePolicy};
use crate::workflow::registry::claude_default;
use uuid::Uuid;

const HAIKU: &str = MODELS[0];
const SONNET: &str = MODELS[1];
const OPUS: &str = MODELS[2];

struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("agentree-adapter-{}", Uuid::new_v4()));
        fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
    /// A copy of the built-in plugin, which is a distinct folder and so starts untrusted.
    fn plugin(&self) -> PathBuf {
        let to = self.0.join(BUILTIN_PLUGIN);
        copy_dir(&fixtures::builtin_plugin().root, &to);
        to
    }
    fn dirs(&self, candidates: Vec<PathBuf>) -> Dirs {
        Dirs {
            plugin_candidates: candidates,
            plugin_data: self.0.join("data"),
            trust_file: self.0.join("plugins.json"),
        }
    }
    fn trust(&self, dir: &Path) {
        let plugin = plugins::load(dir, &self.0.join("data")).unwrap();
        let mut store = Store::load(&self.0.join("plugins.json"));
        store.trust(&plugin).unwrap();
        store.save(&self.0.join("plugins.json")).unwrap();
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let target = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &target);
        } else {
            fs::copy(e.path(), target).unwrap();
        }
    }
}

fn trusted_env() -> Env {
    let p = fixtures::builtin_plugin();
    Env {
        trust: fixtures::trusted(&p),
        plugins: vec![p],
        notices: vec![],
    }
}

static NOTHING_VERIFIED: BTreeSet<String> = BTreeSet::new();

fn request<'a>(
    wf: &'a ValidatedWorkflow,
    settings: &'a Settings,
    registry: &'a Registry,
    env: &'a Env,
    path: Option<ExecutionPath>,
) -> Request<'a> {
    Request {
        workflow: wf,
        settings,
        registry,
        verified: &NOTHING_VERIFIED,
        plugins: &env.plugins,
        trust: &env.trust,
        task: task_for(path),
        preferred: None,
        context_tokens: 0,
        bulk_auth: None,
        review_auth: None,
    }
}

fn brief_with(
    settings: &Settings,
    registry: &Registry,
    path: Option<ExecutionPath>,
) -> Result<Brief, Vec<Violation>> {
    let env = trusted_env();
    let wf = validate(default_workflow(true), registry, &env).unwrap();
    adapt(&request(&wf, settings, registry, &env, path))
}

fn brief(path: Option<ExecutionPath>) -> Brief {
    brief_with(&Settings::default(), &claude_default(), path).unwrap()
}

fn codes(v: &[Violation]) -> Vec<Code> {
    v.iter().map(|x| x.code).collect()
}

fn role<'a>(b: &'a Brief, id: &str) -> &'a RoleBrief {
    b.roles.iter().find(|r| r.id == id).unwrap()
}

/// A backend that enforced every mandatory policy, so a strict workflow validates on it.
fn enforcing() -> BackendProfile {
    BackendProfile {
        role_model: Enforcement::Enforced,
        role_permission: Enforcement::Enforced,
        review_gate: Enforcement::Enforced,
        review_independence: Enforcement::Enforced,
        coordinator_bulk_reading: Enforcement::Enforced,
        ..BackendProfile::claude_native()
    }
}

#[test]
fn a_valid_workflow_feeds_the_adapter_and_routing_picks_the_stages() {
    let ids = |b: &Brief| b.roles.iter().map(|r| r.id.clone()).collect::<Vec<_>>();
    let b = brief(None);
    assert_eq!(b.decision.path, ExecutionPath::Balanced);
    assert_eq!(
        ids(&b),
        ["explorer", "researcher", "planner", "coder", "tester"]
    );
    assert!(b.conflicts.is_empty(), "{:?}", b.conflicts);
    // The workflow's own limit caps a saved concurrency, and says so.
    assert_eq!(role(&b, "planner").max_concurrent, 1);
    assert!(b
        .notices
        .iter()
        .any(|n| n.contains("Planner: saved concurrency 3 lowered")));
    assert_eq!(
        ids(&brief(Some(ExecutionPath::Fast))),
        ["explorer", "coder", "tester"]
    );
    assert_eq!(brief(Some(ExecutionPath::Thorough)).stages.len(), 8);
    // The plan is the existing launch plan: same lead, advisor and native teams.
    let default = RolePlan::claude_default();
    assert_eq!(b.plan.lead_model, default.lead_model);
    assert_eq!(b.plan.advisor, default.advisor);
    assert!(b.plan.native_teams);
    assert!(b
        .assignments()
        .contains("- Coder: claude-haiku-5-5, at most 3 at once"));
}

#[test]
fn each_role_gets_its_trusted_skill_and_only_its_own_capabilities() {
    for path in contract::PATHS {
        let b = brief(Some(path));
        for r in &b.roles {
            let skill = r.skill.as_ref().unwrap();
            assert_eq!(
                Some(skill.name.as_str()),
                Role::from_key(&r.id).unwrap().skill()
            );
            assert_eq!(skill.plugin, BUILTIN_PLUGIN);
            assert!(skill.file.ends_with("SKILL.md") && skill.file.is_file());
            assert!(b.assignments().contains(&skill.fingerprint[..12]));
        }
        let explorer = role(&b, "explorer");
        assert_eq!(
            explorer.tools,
            BTreeSet::from(["read", "search"].map(String::from))
        );
        assert_eq!(explorer.access, Permission::ReadOnly);
        assert_eq!(role(&b, "coder").access, Permission::Edit);
        assert!(role(&b, "coder").tools.contains("edit"));
        assert!(!role(&b, "tester").tools.contains("edit"));
        let review = b.review.as_ref().unwrap();
        assert_eq!(review.skill.as_ref().unwrap().name, "review");
        assert_eq!(b.advisor_skill.as_ref().unwrap().name, "advise");
    }
    assert!(Role::Lead.skill().is_none());
}

#[test]
fn untrusted_revoked_stale_malformed_and_missing_plugins_provide_nothing() {
    let no_skills = |b: &Brief| {
        b.roles.iter().all(|r| r.skill.is_none())
            && b.advisor_skill.is_none()
            && b.review.as_ref().unwrap().skill.is_none()
    };
    let s = Settings::default();

    // Untrusted: the folder exists and is valid, but nobody trusted it.
    let t = Tmp::new();
    let dir = t.plugin();
    let b = prepare(&s, None, None, &t.dirs(vec![dir.clone()])).unwrap();
    assert!(no_skills(&b));
    assert!(b
        .notices
        .iter()
        .any(|n| n.contains("not trusted") && n.contains("--plugin-trust")));

    // Trusted, then revoked.
    t.trust(&dir);
    let b = prepare(&s, None, None, &t.dirs(vec![dir.clone()])).unwrap();
    assert!(!no_skills(&b));
    assert!(
        b.notices.iter().all(|n| n.contains("saved concurrency")),
        "{:?}",
        b.notices
    );
    let mut store = Store::load(&t.0.join("plugins.json"));
    assert!(store.revoke(&dir));
    store.save(&t.0.join("plugins.json")).unwrap();
    assert!(no_skills(
        &prepare(&s, None, None, &t.dirs(vec![dir.clone()])).unwrap()
    ));

    // Stale: trusted and validated, then a file changes before the launch uses it.
    t.trust(&dir);
    let reg = claude_default();
    let env = Env {
        trust: Store::load(&t.0.join("plugins.json")),
        plugins: vec![plugins::load(&dir, &t.0.join("data")).unwrap()],
        notices: vec![],
    };
    let wf = validate(default_workflow(true), &reg, &env).unwrap();
    fs::write(
        dir.join("skills/plan/SKILL.md"),
        "---\nname: plan\ndescription: edited.\n---\nBody\n",
    )
    .unwrap();
    let b = adapt(&request(&wf, &s, &reg, &env, None)).unwrap();
    assert!(no_skills(&b));
    assert!(
        b.notices.iter().any(|n| n.contains("changed")),
        "{:?}",
        b.notices
    );
    // ...and the workflow no longer validates against the changed plugin.
    let err = validate(default_workflow(true), &reg, &env).unwrap_err();
    assert!(codes(&err).iter().all(|c| *c == Code::Plugin));

    // Malformed manifest: rejected whole, the run goes on without skills.
    let bad = Tmp::new();
    fs::write(bad.0.join("plugin.json"), "{ not json").unwrap();
    let b = prepare(&s, None, None, &bad.dirs(vec![bad.0.clone()])).unwrap();
    assert!(no_skills(&b));
    assert!(b.notices.iter().any(|n| n.contains("rejected")));

    // Not installed next to the binary: a clear fix, never a claim that skills loaded.
    let none = Tmp::new();
    let missing = none.dirs(vec![none.0.join("missing")]);
    let b = prepare(&s, None, None, &missing).unwrap();
    assert!(no_skills(&b));
    assert!(b
        .notices
        .iter()
        .any(|n| n.contains("install.ps1") && n.contains("AGENTREE_PLUGIN_DIR")));
    assert!(!b.assignments().contains("trusted plugin"));

    // A workflow file the user wrote gets no silent fallback.
    let file = none.0.join("wf.json");
    fs::write(
        &file,
        serde_json::to_string(&default_workflow(true)).unwrap(),
    )
    .unwrap();
    let err = prepare(&s, Some(&file), None, &missing).unwrap_err();
    assert!(
        err.contains("not valid") && err.contains("not loaded"),
        "{err}"
    );
}

#[test]
fn bulk_readers_get_the_lowest_cost_model_whatever_is_saved() {
    let mut s = Settings::default();
    s.explorer.model = OPUS.into();
    s.researcher.model = SONNET.into();
    s.coder.model = SONNET.into();
    let b = brief_with(&s, &claude_default(), None).unwrap();
    assert_eq!(role(&b, "explorer").model, HAIKU);
    assert_eq!(role(&b, "researcher").model, HAIKU);
    // The Coder is not a bulk reader, so its saved model stands.
    assert_eq!(role(&b, "coder").model, SONNET);
    let planned = |name: &str| {
        b.plan
            .teammates
            .iter()
            .find(|t| t.role == name)
            .unwrap()
            .model
            .clone()
    };
    assert_eq!(planned("Explorer"), HAIKU);
    assert_eq!(planned("Coder"), SONNET);
    // Unnamed subagents follow the bulk model, not the Coder.
    assert_eq!(b.plan.subagent_model, HAIKU);
    // Saved settings that disagree are surfaced, and so is what Agentree did instead.
    let at: Vec<&str> = b.conflicts.iter().map(|c| c.at.as_str()).collect();
    assert!(at.contains(&"settings.explorer") && at.contains(&"settings.researcher"));
    // The unnamed-subagent conflict is resolved by the override, and the notice says so.
    assert!(!at.contains(&"settings.coder"), "{at:?}");
    assert!(b
        .notices
        .iter()
        .any(|n| n.contains("Unnamed subagents run on")));
    assert!(b
        .notices
        .iter()
        .any(|n| n.contains("saved model claude-opus-5-5 replaced")));
    let text = b.assignments();
    assert!(text.contains("- Explorer: claude-haiku-5-5"));
    assert!(text.contains("do not use unnamed subagents"));
    assert!(text.contains("Saved setting conflict"));
}

#[test]
fn a_pricier_bulk_reader_needs_a_named_human_authorization() {
    let env = trusted_env();
    let mut reg = claude_default();
    assert!(reg.set_available(HAIKU, false));
    let wf = validate(default_workflow(true), &reg, &env).unwrap();
    let s = Settings::default();
    let err = adapt(&request(&wf, &s, &reg, &env, None)).unwrap_err();
    assert_eq!(codes(&err), [Code::ModelPolicy, Code::ModelPolicy]);
    assert!(err[0]
        .message
        .contains("authorize one of: claude-sonnet-5-5, claude-opus-5-5"));
    assert!(err[0].message.contains("Nothing launches"));
    let auth = Authorization {
        model: SONNET.into(),
    };
    let mut r = request(&wf, &s, &reg, &env, None);
    r.bulk_auth = Some(&auth);
    let b = adapt(&r).unwrap();
    assert_eq!(role(&b, "explorer").model, SONNET);
    assert!(role(&b, "explorer").why.contains("explicitly authorized"));
}

#[test]
fn bulk_reading_is_chunked_within_bounds_when_the_cheap_model_is_small() {
    let env = trusted_env();
    let mut reg = claude_default();
    reg.models
        .iter_mut()
        .find(|m| m.id == HAIKU)
        .unwrap()
        .context_tokens = 50_000;
    let wf = validate(default_workflow(true), &reg, &env).unwrap();
    let s = Settings::default();
    let mut r = request(&wf, &s, &reg, &env, None);
    r.context_tokens = 120_000;
    let b = adapt(&r).unwrap();
    assert_eq!(role(&b, "explorer").model, HAIKU);
    assert!(role(&b, "explorer")
        .why
        .contains("3 windows of 42000 tokens"));
    assert!(b
        .assignments()
        .contains("at most 42000 tokens, at most 8 windows"));
    // Too much for any window count: an escalation, never a pricier model on its own.
    r.context_tokens = 5_000_000;
    assert!(adapt(&r).is_err());
}

#[test]
fn every_path_ends_in_the_strongest_review_even_when_capped() {
    for path in contract::PATHS {
        let b = brief(Some(path));
        let review = b.review.as_ref().unwrap();
        assert!(b.decision.review_required);
        assert_eq!(review.model, OPUS);
        assert_eq!(review.advisor.as_deref(), Some("opus"));
        assert!(!review.authorized_downgrade);
        assert!(b
            .assignments()
            .contains("every change to code, on every path"));
        assert_eq!(b.stages.last().unwrap().0, "review");
    }
    let env = trusted_env();
    let reg = claude_default();
    let mut def = default_workflow(true);
    def.policy.max_path = Some(ExecutionPath::Fast);
    let wf = validate(def, &reg, &env).unwrap();
    let s = Settings::default();
    let b = adapt(&request(&wf, &s, &reg, &env, Some(ExecutionPath::Thorough))).unwrap();
    assert!(b.decision.capped && b.decision.path == ExecutionPath::Fast);
    assert!(b.review.is_some());
}

#[test]
fn the_reviewer_comes_from_intelligence_rank_and_is_never_silently_downgraded() {
    let env = trusted_env();
    let s = Settings::default();
    // The strongest is whatever the registry ranks first, not a name: rank Sonnet above Opus.
    // Claude's advisor cannot run it, so the launch stops instead of using Opus.
    let mut reg = claude_default();
    reg.models
        .iter_mut()
        .find(|m| m.id == SONNET)
        .unwrap()
        .intelligence = 9;
    let wf = validate(default_workflow(true), &reg, &env).unwrap();
    let err = adapt(&request(&wf, &s, &reg, &env, None)).unwrap_err();
    assert!(
        err.iter()
            .any(|v| v.code == Code::Review && v.message.contains("accepts only opus")),
        "{err:?}"
    );

    // Strongest unavailable: blocked, with the weaker choices offered but not taken.
    let mut reg = claude_default();
    reg.set_available(OPUS, false);
    let wf = validate(default_workflow(true), &reg, &env).unwrap();
    let err = adapt(&request(&wf, &s, &reg, &env, None)).unwrap_err();
    assert_eq!(codes(&err), [Code::Review]);
    assert!(err[0].message.contains("claude-opus-5-5"));
    assert!(err[0].message.contains("authorize one of"));
    assert!(err[0].message.contains("Nothing launches"));
    // A human-authorized weaker reviewer still has to be one Claude's advisor can run.
    let auth = Authorization {
        model: SONNET.into(),
    };
    let mut r = request(&wf, &s, &reg, &env, None);
    r.review_auth = Some(&auth);
    let err = adapt(&r).unwrap_err();
    assert!(err[0].message.contains("accepts only opus"), "{err:?}");
}

#[test]
fn saved_settings_that_contradict_policy_are_surfaced_not_obeyed() {
    // Advisor off stays off, review can only end BLOCKED, and that is stated.
    let mut s = Settings::default();
    s.advisor.model = "off".into();
    let b = brief_with(&s, &claude_default(), None).unwrap();
    assert_eq!(b.plan.advisor, None);
    assert_eq!(b.review.as_ref().unwrap().advisor, None);
    assert!(b
        .conflicts
        .iter()
        .any(|c| c.code == Code::Review && c.at == "settings.advisor"));
    let policy =
        crate::prompt::system_policy_with(Path::new("a.json"), "tok", &b.plan, &b.assignments());
    assert!(policy.contains("the review ends BLOCKED"));
    assert!(policy.contains("AGENTREE_REVIEW_RESULT:tok:BLOCKED"));
    assert!(!policy.contains("never answer PASS yourself"));

    // A read-only role is not handed a write mode.
    let mut s = Settings::default();
    s.explorer.permission = "acceptEdits".into();
    let b = brief_with(&s, &claude_default(), None).unwrap();
    assert!(b.conflicts.iter().any(|c| c.code == Code::Capability));
    assert_eq!(role(&b, "explorer").mode, None);
    let explorer = b.plan.teammates.iter().find(|t| t.role == "Explorer");
    assert!(explorer.unwrap().permission.is_none());
}

#[test]
fn strict_enforcement_rejects_what_claude_native_teams_cannot_guarantee() {
    let reg = claude_default();
    let env = trusted_env();
    let mut def = default_workflow(true);
    def.policy.strict_enforcement = true;
    let err = validate(def.clone(), &reg, &env).unwrap_err();
    assert!(codes(&err).iter().all(|c| *c == Code::Enforcement));
    for needle in [
        "lowest-cost",
        "final review",
        "independence",
        "per-role permissions",
    ] {
        assert!(
            err.iter().any(|v| v.message.contains(needle)),
            "{needle}: {err:?}"
        );
    }
    // Non-strict: the same gaps are limitations the human is told about.
    let ok = validate(default_workflow(true), &reg, &env).unwrap();
    assert!(ok.limitations().len() >= 4);

    // A backend that did enforce everything would pass validation; the adapter then still
    // refuses a strict launch whose saved settings contradict policy.
    let backend = enforcing();
    let wf = def
        .validate(&contract::Context {
            registry: &reg,
            backend: &backend,
            plugins: &env.plugins,
            trust: &env.trust,
        })
        .unwrap();
    let mut s = Settings::default();
    assert!(adapt(&request(&wf, &s, &reg, &env, None)).is_ok());
    s.explorer.model = OPUS.into();
    let err = adapt(&request(&wf, &s, &reg, &env, None)).unwrap_err();
    assert!(err.iter().any(|v| v.at == "settings.explorer"));
}

#[test]
fn plugin_mcp_servers_are_reported_unsupported_and_fail_a_strict_launch() {
    let t = Tmp::new();
    let dir = t.plugin();
    fs::write(
        dir.join("mcp.json"),
        format!(
            r#"{{"$schema":"{}","mcpServers":{{"demo":{{"type":"stdio","command":"demo-server"}}}}}}"#,
            plugins::MCP_SCHEMA
        ),
    )
    .unwrap();
    let plugin = plugins::load(&dir, &t.0.join("data")).unwrap();
    assert_eq!(plugin.mcp.len(), 1, "{:?}", plugin.issues);
    let mut store = Store::default();
    store.trust(&plugin).unwrap();
    let env = Env {
        plugins: vec![plugin],
        trust: store,
        notices: vec![],
    };
    let reg = claude_default();
    let mut def = default_workflow(true);
    let explorer = def.roles.iter_mut().find(|r| r.id == "explorer").unwrap();
    explorer.tools.insert("mcp".into());
    explorer.mcp.push(PluginRef {
        plugin: BUILTIN_PLUGIN.into(),
        name: "demo".into(),
    });
    let wf = validate(def.clone(), &reg, &env).unwrap();
    let s = Settings::default();
    let b = adapt(&request(&wf, &s, &reg, &env, None)).unwrap();
    assert_eq!(role(&b, "explorer").mcp, ["software-engineering/demo"]);
    assert!(b
        .guarantees
        .iter()
        .any(|g| g.policy.starts_with("MCP") && g.level == Enforcement::Unsupported));
    assert!(b
        .assignments()
        .contains("MCP servers of trusted plugins: unsupported"));
    // Strict: unsupported is a refusal. (Validation passes only on a backend that enforces.)
    def.policy.strict_enforcement = true;
    let backend = enforcing();
    let wf = def
        .validate(&contract::Context {
            registry: &reg,
            backend: &backend,
            plugins: &env.plugins,
            trust: &env.trust,
        })
        .unwrap();
    let err = adapt(&request(&wf, &s, &reg, &env, None)).unwrap_err();
    assert_eq!(codes(&err), [Code::Enforcement]);
}

#[test]
fn the_native_launch_arguments_environment_and_safeguards_are_unchanged() {
    let policy = Path::new("C:/project with spaces/policy.txt");
    let args = |plan: &RolePlan| crate::pty::claude_launch_args(plan, "run", policy, "go");
    let b = brief(None);
    assert_eq!(args(&b.plan), args(&RolePlan::claude_default()));
    assert_eq!(
        crate::pty::claude_team_env(&b.plan),
        crate::pty::claude_team_env(&RolePlan::claude_default())
    );
    // A pricier Coder changes only the unnamed-subagent environment, back to the cheap model.
    let mut s = Settings::default();
    s.coder.model = SONNET.into();
    let thorough = brief_with(&s, &claude_default(), None).unwrap();
    assert_eq!(args(&thorough.plan), args(&RolePlan::claude_default()));
    assert_eq!(
        crate::pty::claude_team_env(&thorough.plan),
        [
            ("CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS", "1".to_string()),
            ("CLAUDE_CODE_SUBAGENT_MODEL", HAIKU.to_string()),
        ]
    );
    // Every operational rule of the existing instructions survives the new role section.
    let text = crate::prompt::system_policy_with(
        Path::new("C:/run/attention.json"),
        "tok",
        &b.plan,
        &b.assignments(),
    );
    for needle in [
        "Git writes require explicit human authorization",
        "must not stage files, create commits, push, merge, rebase, switch branches or rewrite Git history",
        "Read-only Git inspection",
        "Leave implementation changes uncommitted",
        "attention.json",
        "Await the human's answer in your interactive terminal",
        "AGENTREE_REVIEW_START:tok",
        "AGENTREE_REVIEW_RESULT:tok:PASS",
        "AGENTREE_REVIEW_RESULT:tok:NEEDS_CHANGES",
        "AGENTREE_REVIEW_RESULT:tok:BLOCKED",
        "AGENTREE_OBJECTIVE_COMPLETE:tok",
        "--advisor opus",
        "Agent tool",
        "Role assignments for named teammates",
    ] {
        assert!(text.contains(needle), "{needle}");
    }
    // The old fixed role list is gone when the adapter supplies the section.
    assert!(!text.contains("use the model, concurrency limit and permission mode given"));
    // The review packet instructions name the real diff, objective, criteria and evidence.
    let review = b.assignments();
    for needle in [
        "git diff output, not a summary",
        "the objective",
        "the acceptance criteria",
        "verification evidence",
        "never answer PASS yourself",
    ] {
        assert!(review.contains(needle), "{needle}");
    }
}

#[test]
fn the_path_flag_parses_and_the_report_is_honest_about_limits() {
    assert_eq!(parse_path("Fast"), Ok(ExecutionPath::Fast));
    assert_eq!(parse_path("thorough"), Ok(ExecutionPath::Thorough));
    assert!(parse_path("slow").is_err());
    let b = brief(None);
    // Prompt text is never reported as enforced.
    for g in &b.guarantees {
        if g.policy.starts_with("Teammate") || g.policy.contains("independence") {
            assert!(g.level < Enforcement::Enforced, "{}", g.policy);
        }
    }
    let report = b.report();
    assert!(report.contains("[instruction only] Teammate models"));
    assert!(report.contains("[observed, not prevented] Final review"));
    let subagent = b
        .guarantees
        .iter()
        .find(|g| g.policy.starts_with("Unnamed"));
    assert!(subagent.unwrap().level < Enforcement::Enforced);
    assert!(b.status_line().contains("Balanced path"));
    assert!(b
        .assignments()
        .contains("What Claude Code does not enforce"));
}

#[test]
fn the_checkout_plugin_is_found_without_an_install() {
    let t = Tmp::new();
    let dirs = Dirs::standard(&t.0);
    assert_eq!(dirs.trust_file, t.0.join("plugins.json"));
    let env = environment(&dirs);
    assert_eq!(env.plugins.len(), 1, "{:?}", env.notices);
    assert!(env.notices.is_empty());
    // Nothing is trusted yet, so nothing is active.
    assert!(env.trust.activate(&env.plugins[0]).is_none());
}

fn stage<'a>(b: &'a Brief, id: &str) -> &'a StageBrief {
    b.flow.iter().find(|s| s.id == id).unwrap()
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn the_flow_comes_from_the_workflow_and_differs_by_path() {
    let thorough = brief(Some(ExecutionPath::Thorough));
    assert_eq!(thorough.flow.len(), 8);
    // Independent branches are parallel; dependants are sequential.
    assert_eq!(stage(&thorough, "explore").waits_for, Vec::<String>::new());
    assert_eq!(stage(&thorough, "explore").parallel_with, ["research"]);
    assert_eq!(stage(&thorough, "plan").waits_for, ["explore", "research"]);
    assert!(stage(&thorough, "plan").parallel_with.is_empty());
    assert_eq!(
        stage(&thorough, "prepare-review").parallel_with,
        ["risk-scan"]
    );
    assert_eq!(
        stage(&thorough, "review").waits_for,
        ["implement", "test", "prepare-review", "risk-scan"]
    );
    // Contracts and ownership are the stage's own.
    let implement = stage(&thorough, "implement");
    assert_eq!(implement.waits_for, ["explore", "plan"]);
    assert_eq!(
        implement.inputs,
        [
            (
                "evidence".to_string(),
                Some("explore.evidence".to_string()),
                true
            ),
            ("plan".to_string(), Some("plan.plan".to_string()), false),
            ("objective".to_string(), None, true),
        ]
    );
    assert_eq!(
        implement.outputs,
        [("diff".to_string(), "diff".to_string())]
    );
    assert_eq!(implement.owns, ["."]);
    assert!(stage(&thorough, "test").owns.is_empty());
    assert_eq!(
        (implement.max_attempts, implement.max_repair_cycles),
        (2, 2)
    );
    // On Fast the optional plan does not exist: no wait and no input from it.
    let fast = brief(Some(ExecutionPath::Fast));
    let implement = stage(&fast, "implement");
    assert_eq!(implement.waits_for, ["explore"]);
    assert!(implement.inputs.iter().all(|i| i.0 != "plan"));
    assert_eq!(stage(&fast, "review").waits_for, ["implement", "test"]);
    assert!(fast.flow.iter().all(|s| s.id != "research"));
    // The lead is told, and told it is only an instruction.
    let text = thorough.assignments();
    for needle in [
        "- explore (Explorer): starts immediately; may run in parallel with research.",
        "- plan (Planner): waits for explore, research; nothing may run alongside it.",
        "Inputs: evidence (from explore.evidence, required); research (from research.evidence, optional)",
        "Outputs: diff (diff). Owns the whole workspace",
        "Owns no workspace paths: it must not write files.",
        "at most 2 attempt(s) and 2 repair cycle(s); then stop and ask the human through the attention file",
        "Claude Code does not schedule, gate or sandbox these",
    ] {
        assert!(text.contains(needle), "{needle}\n{text}");
    }
}

#[test]
fn a_branched_workflow_keeps_its_own_dependencies_contracts_and_ownership() {
    let mut def = default_workflow(true);
    // A second implementer on a disjoint folder, in parallel with the first, aborting on failure.
    let mut ui = def
        .stages
        .iter()
        .find(|s| s.id == "implement")
        .unwrap()
        .clone();
    ui.id = "implement-ui".into();
    ui.owns = strs(&["src/ui"]);
    ui.outputs[0].name = "ui-diff".into();
    ui.policy = StagePolicy {
        max_attempts: 1,
        max_repair_cycles: 0,
        on_exhausted: OnExhausted::Abort,
    };
    for s in &mut def.stages {
        match s.id.as_str() {
            "implement" => s.owns = strs(&["src/core"]),
            "test" => {
                s.depends_on = strs(&["implement", "implement-ui"]);
                s.inputs.push(Input {
                    name: "ui-diff".into(),
                    from: Some(OutputRef {
                        stage: "implement-ui".into(),
                        output: "ui-diff".into(),
                    }),
                    required: true,
                });
            }
            "review" => s.depends_on.push("implement-ui".into()),
            _ => {}
        }
    }
    def.stages.push(ui);
    let env = trusted_env();
    let reg = claude_default();
    let wf = validate(def, &reg, &env).unwrap_or_else(|v| panic!("{v:?}"));
    let s = Settings::default();
    let b = adapt(&request(&wf, &s, &reg, &env, Some(ExecutionPath::Fast))).unwrap();
    let (core, ui, test) = (
        stage(&b, "implement"),
        stage(&b, "implement-ui"),
        stage(&b, "test"),
    );
    assert_eq!(core.parallel_with, ["implement-ui"]);
    assert_eq!(ui.parallel_with, ["implement"]);
    assert_eq!(core.owns, ["src/core"]);
    assert_eq!(ui.owns, ["src/ui"]);
    assert_eq!(test.waits_for, ["implement", "implement-ui"]);
    assert!(test.parallel_with.is_empty());
    assert_eq!(ui.on_exhausted, OnExhausted::Abort);
    assert_eq!(
        (core.on_exhausted, core.max_attempts),
        (OnExhausted::Escalate, 2)
    );
    let text = b.assignments();
    for needle in [
        "- implement (Coder): waits for explore; may run in parallel with implement-ui.",
        "- implement-ui (Coder): waits for explore; may run in parallel with implement.",
        "Owns only src/core: it must not write elsewhere.",
        "Owns only src/ui: it must not write elsewhere.",
        "ui-diff (from implement-ui.ui-diff, required)",
        "at most 1 attempt(s) and 0 repair cycle(s); then abort the stage and report it",
        "- test (Tester): waits for implement, implement-ui; nothing may run alongside it.",
    ] {
        assert!(text.contains(needle), "{needle}\n{text}");
    }
}

#[test]
fn models_are_configured_and_eligible_not_verified_unless_proven() {
    let b = brief(None);
    assert!(b.roles.iter().all(|r| !r.verified));
    assert!(!b.review.as_ref().unwrap().verified);
    let text = b.assignments();
    assert!(text.contains("runtime availability not verified"));
    assert!(text.contains("is not a verified runtime model"));
    assert!(b.report().contains("Final reviewer: claude-opus-5-5 (configured and eligible; runtime availability not verified)"));
    assert!(b
        .guarantees
        .iter()
        .any(|g| g.policy.starts_with("Stage order") && g.level == Enforcement::Instructed));
    assert!(b
        .report()
        .contains("Stage order, input and output contracts"));
    assert!(text.contains("Never substitute another model for a bulk reader or the final reviewer"));
    assert!(!text.contains("(verified available)"));
    let g = b
        .guarantees
        .iter()
        .find(|g| g.policy == "Model runtime availability")
        .unwrap();
    assert!(g.level < Enforcement::Enforced);
    assert!(b.report().contains("Model runtime availability"));
    // The production entry point has nothing to verify with, so it never claims availability.
    let tmp = Tmp::new();
    let prod = prepare(&Settings::default(), None, None, &tmp.dirs(vec![])).unwrap();
    assert!(prod.roles.iter().all(|r| !r.verified) && !prod.review.unwrap().verified);
    // A model proven available is reported as such, and only that model.
    let env = trusted_env();
    let reg = claude_default();
    let wf = validate(default_workflow(true), &reg, &env).unwrap();
    let mut s = Settings::default();
    s.coder.model = SONNET.into();
    let proven: BTreeSet<String> = [HAIKU.to_string(), OPUS.to_string()].into();
    let mut r = request(&wf, &s, &reg, &env, None);
    r.verified = &proven;
    let b = adapt(&r).unwrap();
    assert!(role(&b, "explorer").verified && b.review.as_ref().unwrap().verified);
    assert!(b.roles.iter().all(|r| r.verified == (r.model == HAIKU)));
    let text = b.assignments();
    assert!(text.contains("(verified available)"));
    // Partly verified: the caveat names only the unverified ones.
    assert!(text.contains("not marked verified available above"));
    assert!(!text.contains("none is a verified"));
    assert!(b
        .report()
        .contains("Explorer: claude-haiku-5-5 (verified available)"));
}

#[test]
fn an_unavailable_cheapest_reader_or_strongest_reviewer_is_never_swapped() {
    let env = trusted_env();
    let s = Settings::default();
    for (model, code) in [(HAIKU, Code::ModelPolicy), (OPUS, Code::Review)] {
        let mut reg = claude_default();
        reg.set_available(model, false);
        let wf = validate(default_workflow(true), &reg, &env).unwrap();
        let err = adapt(&request(&wf, &s, &reg, &env, None)).unwrap_err();
        assert!(codes(&err).contains(&code), "{model}: {err:?}");
    }
    // A model that is unavailable is not verified even if something claims it is.
    let mut reg = claude_default();
    reg.set_available(HAIKU, false);
    let wf = validate(default_workflow(true), &reg, &env).unwrap();
    let proven: BTreeSet<String> = [HAIKU.to_string(), SONNET.to_string()].into();
    let auth = Authorization {
        model: SONNET.into(),
    };
    let mut r = request(&wf, &s, &reg, &env, None);
    r.verified = &proven;
    r.bulk_auth = Some(&auth);
    let b = adapt(&r).unwrap();
    // Only the explicit authorization moved the readers, and it is still labelled so.
    assert_eq!(role(&b, "explorer").model, SONNET);
    assert!(role(&b, "explorer").why.contains("explicitly authorized"));
    assert!(!is_verified(&r, HAIKU));
}

#[test]
fn bulk_roles_with_different_cheapest_models_keep_the_cheaper_unnamed_default() {
    // Haiku cannot browse, so the Researcher's cheapest capable model is Sonnet; the Explorer's is Haiku.
    let mut reg = claude_default();
    reg.models[0].tools.remove("web");
    let b = brief_with(&Settings::default(), &reg, None).unwrap();
    assert_eq!(role(&b, "explorer").model, HAIKU);
    assert_eq!(role(&b, "researcher").model, SONNET);
    // Unnamed work is not moved to the dearer model.
    assert_eq!(b.plan.subagent_model, HAIKU);
    assert!(b
        .notices
        .iter()
        .any(|n| n.contains("cannot match them all")));
    let text = b.assignments();
    assert!(text.contains("Unnamed subagents run on claude-haiku-5-5"));
    assert!(text.contains(
        "cannot match every bulk role: Researcher (claude-sonnet-5-5) use a different model"
    ));
    assert!(text.contains("only as named teammates with exactly that model"));
    assert!(!text.contains("Explorer (claude-haiku-5-5) need"));
    let g = b
        .guarantees
        .iter()
        .find(|g| g.policy.starts_with("Unnamed"))
        .unwrap();
    assert!(g
        .note
        .contains("cannot match bulk roles that resolve to different models"));
    assert_eq!(
        crate::pty::claude_team_env(&b.plan)[1],
        ("CLAUDE_CODE_SUBAGENT_MODEL", HAIKU.to_string())
    );
    // The dearer model becomes the default only when every bulk role resolves to it.
    let mut reg = claude_default();
    reg.models[0].tools.remove("web");
    reg.models[0].tools.remove("search");
    let b = brief_with(&Settings::default(), &reg, None).unwrap();
    assert!(b.roles.iter().filter(|r| r.bulk).all(|r| r.model == SONNET));
    assert_eq!(b.plan.subagent_model, SONNET);
    assert!(!b.assignments().contains("cannot match every bulk role"));
}
