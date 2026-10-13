//! Versioned workflow definition and the validation gate in front of execution.
//!
//! A [`WorkflowDef`] is data (JSON, `agentree.workflow/1`). Nothing runs from one until
//! [`WorkflowDef::validate`] has produced a [`ValidatedWorkflow`], which is the only type the
//! routing and review code accepts. Validation reports every violation at once.

use super::evidence::relative;
use super::registry::{ChunkPolicy, Permission, Registry, Requirement};
use super::{Code, Enforcement, Violation, SCHEMA, VERSION};
use crate::plugins;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Capabilities a definition may name. Anything else is rejected before execution.
pub const KNOWN_TOOLS: [&str; 6] = ["read", "search", "shell", "edit", "web", "mcp"];
/// Inputs the final review stage must receive, all required.
pub const REVIEW_INPUTS: [&str; 4] = ["diff", "objective", "acceptance_criteria", "verification"];
/// Bounds on the repair and retry loops a stage may request.
const MAX_ATTEMPTS: u8 = 3;
const MAX_REPAIRS: u8 = 3;
const MAX_INSPECTIONS: u8 = 8;
/// Claude Code supports one to four teammates of a role at once.
const MAX_CONCURRENT: u8 = 4;

/// Fast < Balanced < Thorough. A stage runs on every path at or above its `min_path`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPath {
    Fast,
    Balanced,
    Thorough,
}

pub const PATHS: [ExecutionPath; 3] = [
    ExecutionPath::Fast,
    ExecutionPath::Balanced,
    ExecutionPath::Thorough,
];

impl ExecutionPath {
    pub fn label(self) -> &'static str {
        match self {
            ExecutionPath::Fast => "Fast",
            ExecutionPath::Balanced => "Balanced",
            ExecutionPath::Thorough => "Thorough",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleClass {
    Planning,
    /// High-token reading: exploration, large files, docs, logs, evidence extraction.
    BulkRead,
    Implementation,
    Verification,
    /// The independent final reviewer.
    Review,
}

/// How a role's model is chosen. `LowestCost` and `StrongestReview` are mandatory for their
/// classes and are resolved from the registry at run time; `Configured` defers to the user's
/// role settings; `Pinned` names a registry model explicitly.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawPolicy", into = "RawPolicy")]
pub enum ModelPolicy {
    LowestCost,
    StrongestReview,
    Configured,
    Pinned { model: String },
}

/// Wire form of [`ModelPolicy`]: `{"policy": "...", "model": "..."}`. A struct, so unknown keys
/// are rejected (an internally tagged enum would ignore them on its unit variants).
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPolicy {
    policy: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
}

impl TryFrom<RawPolicy> for ModelPolicy {
    type Error = String;

    fn try_from(raw: RawPolicy) -> Result<Self, String> {
        match (raw.policy.as_str(), raw.model) {
            ("lowest_cost", None) => Ok(ModelPolicy::LowestCost),
            ("strongest_review", None) => Ok(ModelPolicy::StrongestReview),
            ("configured", None) => Ok(ModelPolicy::Configured),
            ("pinned", Some(model)) => Ok(ModelPolicy::Pinned { model }),
            ("pinned", None) => Err("a pinned policy needs a model".into()),
            (other, Some(_)) => Err(format!("policy '{other}' takes no model")),
            (other, None) => Err(format!("unknown model policy '{other}'")),
        }
    }
}

impl From<ModelPolicy> for RawPolicy {
    fn from(p: ModelPolicy) -> Self {
        let (policy, model) = match p {
            ModelPolicy::LowestCost => ("lowest_cost", None),
            ModelPolicy::StrongestReview => ("strongest_review", None),
            ModelPolicy::Configured => ("configured", None),
            ModelPolicy::Pinned { model } => ("pinned", Some(model)),
        };
        RawPolicy {
            policy: policy.into(),
            model,
        }
    }
}

/// A skill or MCP server by plugin and name. Agentree stores nothing else about plugins here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginRef {
    pub plugin: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleDef {
    pub id: String,
    pub class: RoleClass,
    pub model: ModelPolicy,
    #[serde(default)]
    pub skill: Option<PluginRef>,
    /// MCP servers from trusted plugins. Requires the `mcp` tool.
    #[serde(default)]
    pub mcp: Vec<PluginRef>,
    #[serde(default)]
    pub tools: BTreeSet<String>,
    pub permission: Permission,
    #[serde(default = "repository")]
    pub data_class: String,
    #[serde(default = "one")]
    pub max_concurrent: u8,
}

fn repository() -> String {
    "repository".into()
}
fn one() -> u8 {
    1
}
fn fast() -> ExecutionPath {
    ExecutionPath::Fast
}

impl RoleDef {
    /// What the model running this role must provide, for `context_tokens` of input.
    pub fn requirement(&self, context_tokens: u32) -> Requirement {
        Requirement {
            tools: self.tools.clone(),
            context_tokens,
            permission: self.permission,
            data_class: self.data_class.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Evidence,
    Plan,
    Diff,
    Verification,
    Verdict,
    Report,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputRef {
    pub stage: String,
    pub output: String,
}

/// A named input. `from: None` means the run supplies it (objective, acceptance criteria).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub name: String,
    #[serde(default)]
    pub from: Option<OutputRef>,
    #[serde(default)]
    pub required: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Output {
    pub name: String,
    pub kind: ArtifactKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnExhausted {
    /// Ask the human. The default, and the only way a failed stage is allowed to continue.
    Escalate,
    Abort,
}

/// Bounded retry and repair. There is no value that means "keep going silently".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StagePolicy {
    pub max_attempts: u8,
    pub max_repair_cycles: u8,
    pub on_exhausted: OnExhausted,
}

impl Default for StagePolicy {
    fn default() -> Self {
        Self {
            max_attempts: 2,
            max_repair_cycles: 2,
            on_exhausted: OnExhausted::Escalate,
        }
    }
}

/// How the reviewer may ask for more source. Each request is answered by the bulk reader.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inspection {
    pub max_requests: u8,
    pub reader_role: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stage {
    pub id: String,
    pub role: String,
    /// Stages this one always waits for. Each must run on every path this stage runs on
    /// (`min_path` no higher than this stage's), or validation fails.
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Stages this one waits for only when they are on the chosen path. Ordering only: a
    /// required input may not come from one, because it may be absent.
    #[serde(default)]
    pub optional_depends_on: Vec<String>,
    #[serde(default = "fast")]
    pub min_path: ExecutionPath,
    #[serde(default)]
    pub inputs: Vec<Input>,
    pub outputs: Vec<Output>,
    /// Workspace paths this stage may write. `.` means the whole workspace.
    #[serde(default)]
    pub owns: Vec<String>,
    #[serde(default)]
    pub policy: StagePolicy,
    #[serde(default)]
    pub inspection: Option<Inspection>,
}

/// User-configurable budgets and permitted capabilities.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkflowPolicy {
    /// Capabilities roles may use. A role asking for another is rejected.
    pub allowed_capabilities: BTreeSet<String>,
    /// Budget cap: routing never selects a path above this. Review is unaffected.
    pub max_path: Option<ExecutionPath>,
    /// Fail validation when the backend can only *instruct* a mandatory policy.
    pub strict_enforcement: bool,
    /// Bulk readers may chunk within these bounds. `None` forbids chunking.
    pub chunking: Option<ChunkPolicy>,
}

impl Default for WorkflowPolicy {
    fn default() -> Self {
        Self {
            allowed_capabilities: KNOWN_TOOLS.iter().map(|t| t.to_string()).collect(),
            max_path: None,
            strict_enforcement: false,
            chunking: Some(ChunkPolicy::default()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDef {
    pub schema: String,
    pub version: u32,
    pub name: String,
    pub roles: Vec<RoleDef>,
    pub stages: Vec<Stage>,
    #[serde(default)]
    pub policy: WorkflowPolicy,
}

/// What a backend can technically enforce, as opposed to ask for in a prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendProfile {
    pub name: String,
    pub tools: BTreeSet<String>,
    /// A teammate runs on the model chosen for its role.
    pub role_model: Enforcement,
    /// A teammate is held to its role's permission level and tool list.
    pub role_permission: Enforcement,
    /// Final review cannot be skipped by the lead.
    pub review_gate: Enforcement,
    /// The reviewer receives a fresh, separate packet (diff, objective, criteria, evidence)
    /// rather than the implementer's whole conversation.
    pub review_independence: Enforcement,
    /// The coordinating session is held to delegating bulk reading instead of doing it on
    /// its own model. The coordinator is not a workflow role, so a role policy cannot cover it.
    pub coordinator_bulk_reading: Enforcement,
}

impl BackendProfile {
    /// Claude Code native agent teams today (see `roles::Settings::notes`): only the lead's
    /// model, effort and permission mode are flags. Teammate settings are instructions in the
    /// lead's prompt, and Agentree observes the review markers but cannot force the review.
    pub fn claude_native() -> Self {
        Self {
            name: "Claude Code native teams".into(),
            tools: KNOWN_TOOLS.iter().map(|t| t.to_string()).collect(),
            role_model: Enforcement::Instructed,
            role_permission: Enforcement::Instructed,
            review_gate: Enforcement::Observed,
            // The advisor is shown the lead's transcript, not a separate review packet.
            review_independence: Enforcement::Instructed,
            coordinator_bulk_reading: Enforcement::Instructed,
        }
    }
}

/// Everything validation needs from outside the definition.
pub struct Context<'a> {
    pub registry: &'a Registry,
    pub backend: &'a BackendProfile,
    pub plugins: &'a [plugins::Plugin],
    pub trust: &'a plugins::Store,
}

/// A definition that passed every check. Cannot be built any other way.
#[derive(Clone, Debug)]
pub struct ValidatedWorkflow {
    def: WorkflowDef,
    /// Stage indices in dependency order.
    order: Vec<usize>,
    /// Policies the backend can only instruct or observe, shown before launch.
    limitations: Vec<String>,
}

impl ValidatedWorkflow {
    pub fn def(&self) -> &WorkflowDef {
        &self.def
    }

    pub fn limitations(&self) -> &[String] {
        &self.limitations
    }

    pub fn role(&self, id: &str) -> Option<&RoleDef> {
        self.def.roles.iter().find(|r| r.id == id)
    }

    /// The stages that run on `path`, in dependency order. The review stage is on every path.
    pub fn stages_for(&self, path: ExecutionPath) -> Vec<&Stage> {
        self.order
            .iter()
            .map(|&i| &self.def.stages[i])
            .filter(|s| s.min_path <= path)
            .collect()
    }

    /// What `stage` waits for on `path`: its `depends_on` plus those `optional_depends_on`
    /// stages that run there. Empty when `stage` is not on `path`. Every id is a stage in
    /// [`ValidatedWorkflow::stages_for`] for the same path, so the graph is self-contained.
    pub fn waits_for<'a>(&self, path: ExecutionPath, stage: &'a Stage) -> Vec<&'a str> {
        if stage.min_path > path {
            return vec![];
        }
        let on = |id: &str| {
            self.def
                .stages
                .iter()
                .any(|s| s.id == id && s.min_path <= path)
        };
        stage
            .depends_on
            .iter()
            .chain(&stage.optional_depends_on)
            .map(String::as_str)
            .filter(|id| on(id))
            .collect()
    }

    pub fn review_stage(&self) -> Option<&Stage> {
        self.def.stages.iter().find(|s| {
            self.role(&s.role)
                .is_some_and(|r| r.class == RoleClass::Review)
        })
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 48
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The canonical form of an owned path: `/`-separated, lowercase (the strictest common
/// filesystem rule), `.` and empty components dropped. `""` is the whole workspace. `None`
/// means the spelling is ambiguous or unsafe and cannot be compared reliably: it escapes the
/// workspace, is absolute or drive-qualified, or uses a Windows alias form (trailing dot or
/// space, `~` short names, wildcards, control characters).
fn normalize_owned(path: &str) -> Option<String> {
    // A trailing separator is the same directory; a bare `/` or an empty string is not relative.
    let path = path.trim_end_matches(['/', '\\']);
    if !relative(path) {
        return None;
    }
    let mut parts = Vec::new();
    for c in path.split(['/', '\\']) {
        match c {
            "" | "." => continue,
            ".." => return None,
            _ => {}
        }
        if c.ends_with(['.', ' '])
            || c.contains(['~', '*', '?', '"', '<', '>', '|'])
            || c.chars().any(char::is_control)
        {
            return None;
        }
        parts.push(c.to_lowercase());
    }
    Some(parts.join("/"))
}

/// Whether two owned paths may touch the same file. Anything that cannot be normalized
/// counts as overlapping, so an alias never slips past the parallel-write check.
fn overlaps(a: &str, b: &str) -> bool {
    let (Some(a), Some(b)) = (normalize_owned(a), normalize_owned(b)) else {
        return true;
    };
    a.is_empty()
        || b.is_empty()
        || a == b
        || b.starts_with(&format!("{a}/"))
        || a.starts_with(&format!("{b}/"))
}

/// Parses a definition. Format and version problems are violations, not panics.
pub fn parse(text: &str) -> Result<WorkflowDef, Violation> {
    let def: WorkflowDef = serde_json::from_str(text)
        .map_err(|e| Violation::new(Code::Schema, "workflow", format!("not valid: {e}")))?;
    if def.schema != SCHEMA {
        return Err(Violation::new(
            Code::Schema,
            "schema",
            format!("expected '{SCHEMA}', found '{}'", def.schema),
        ));
    }
    if def.version != VERSION {
        return Err(Violation::new(
            Code::Schema,
            "version",
            format!(
                "version {} is not supported; this build reads {VERSION}",
                def.version
            ),
        ));
    }
    Ok(def)
}

impl WorkflowDef {
    pub fn validate(self, ctx: &Context) -> Result<ValidatedWorkflow, Vec<Violation>> {
        let mut out = Vec::new();
        let mut limitations = Vec::new();
        out.extend(ctx.registry.validate());
        let order = check(&self, ctx, &mut out, &mut limitations);
        match (out.is_empty(), order) {
            (true, Some(order)) => Ok(ValidatedWorkflow {
                def: self,
                order,
                limitations,
            }),
            (_, _) => Err(out),
        }
    }
}

/// Every stage `s` may wait for, required or optional.
fn deps(s: &Stage) -> impl Iterator<Item = &String> {
    s.depends_on.iter().chain(&s.optional_depends_on)
}

/// Kahn's algorithm over known dependencies. `Err` lists the stages stuck in or behind a cycle.
fn topo(def: &WorkflowDef) -> Result<Vec<usize>, Vec<String>> {
    let index: BTreeMap<&str, usize> = def
        .stages
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.as_str(), i))
        .collect();
    let mut indegree = vec![0usize; def.stages.len()];
    let mut dependents: Vec<Vec<usize>> = vec![vec![]; def.stages.len()];
    for (i, s) in def.stages.iter().enumerate() {
        for dep in deps(s) {
            if let Some(&d) = index.get(dep.as_str()) {
                indegree[i] += 1;
                dependents[d].push(i);
            }
        }
    }
    let mut ready: Vec<usize> = (0..indegree.len()).filter(|&i| indegree[i] == 0).collect();
    let mut order = Vec::new();
    while let Some(i) = ready.pop() {
        order.push(i);
        for &n in &dependents[i] {
            indegree[n] -= 1;
            if indegree[n] == 0 {
                ready.push(n);
            }
        }
    }
    if order.len() == def.stages.len() {
        Ok(order)
    } else {
        Err((0..indegree.len())
            .filter(|i| !order.contains(i))
            .map(|i| def.stages[i].id.clone())
            .collect())
    }
}

/// One execution path's dependency graph. `anc[i]` is empty for a stage that is not `on`.
struct PathGraph {
    path: ExecutionPath,
    on: Vec<bool>,
    anc: Vec<BTreeSet<usize>>,
}

/// Transitive dependencies of each stage in `on`, following only edges whose ends are both in
/// `on`. An omitted stage therefore passes nothing on: a chain through it is broken.
fn ancestors(
    def: &WorkflowDef,
    index: &BTreeMap<&str, usize>,
    order: &[usize],
    on: &[bool],
) -> Vec<BTreeSet<usize>> {
    let mut anc: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); def.stages.len()];
    for &i in order.iter().filter(|&&i| on[i]) {
        let mut set = BTreeSet::new();
        for d in deps(&def.stages[i]) {
            if let Some(&di) = index.get(d.as_str()).filter(|&&di| on[di]) {
                set.insert(di);
                set.extend(anc[di].iter().copied());
            }
        }
        anc[i] = set;
    }
    anc
}

fn check(
    def: &WorkflowDef,
    ctx: &Context,
    out: &mut Vec<Violation>,
    limitations: &mut Vec<String>,
) -> Option<Vec<usize>> {
    if def.schema != SCHEMA || def.version != VERSION {
        out.push(Violation::new(
            Code::Schema,
            "schema",
            format!("expected '{SCHEMA}' version {VERSION}"),
        ));
        return None;
    }
    if def.name.trim().is_empty() {
        out.push(Violation::new(Code::Schema, "name", "needs a name"));
    }
    check_roles(def, ctx, out);

    // Stage identity and references.
    let mut stage_ids = BTreeSet::new();
    let roles: BTreeMap<&str, &RoleDef> = def.roles.iter().map(|r| (r.id.as_str(), r)).collect();
    for s in &def.stages {
        let at = format!("stages[{}]", s.id);
        if !valid_id(&s.id) {
            out.push(Violation::new(
                Code::Schema,
                &at,
                "ids use letters, digits, - and _",
            ));
        }
        if !stage_ids.insert(s.id.as_str()) {
            out.push(Violation::new(Code::Duplicate, &at, "duplicate stage id"));
        }
        if !roles.contains_key(s.role.as_str()) {
            out.push(Violation::new(
                Code::Unknown,
                &at,
                format!("role '{}' is not defined", s.role),
            ));
        }
        let mut waits = BTreeSet::new();
        for d in deps(s) {
            if d == &s.id {
                out.push(Violation::new(Code::Cycle, &at, "depends on itself"));
            } else if !def.stages.iter().any(|o| &o.id == d) {
                out.push(Violation::new(
                    Code::Unknown,
                    &at,
                    format!("depends on unknown stage '{d}'"),
                ));
            } else if !waits.insert(d.as_str()) {
                // Listed twice, or both required and optional: the contract must say which.
                out.push(Violation::new(
                    Code::Duplicate,
                    &at,
                    format!("'{d}' is listed more than once in its dependencies"),
                ));
            }
        }
        if s.outputs.is_empty() {
            out.push(Violation::new(Code::Schema, &at, "declares no outputs"));
        }
        let mut names = BTreeSet::new();
        for o in &s.outputs {
            if !names.insert(o.name.as_str()) {
                out.push(Violation::new(
                    Code::Duplicate,
                    &at,
                    format!("output '{}' is declared twice", o.name),
                ));
            }
        }
        let p = s.policy;
        if !(1..=MAX_ATTEMPTS).contains(&p.max_attempts) || p.max_repair_cycles > MAX_REPAIRS {
            out.push(Violation::new(
                Code::Schema,
                &at,
                format!(
                    "attempts must be 1 to {MAX_ATTEMPTS} and repair cycles at most {MAX_REPAIRS}"
                ),
            ));
        }
    }

    let order = match topo(def) {
        Ok(order) => order,
        Err(stuck) => {
            out.push(Violation::new(
                Code::Cycle,
                "stages",
                format!("dependency cycle involving: {}", stuck.join(", ")),
            ));
            return None;
        }
    };
    let index: BTreeMap<&str, usize> = def
        .stages
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.as_str(), i))
        .collect();
    // Transitive dependencies of each stage over the whole graph, by index.
    let all = vec![true; def.stages.len()];
    let anc = ancestors(def, &index, &order, &all);
    // The same, once per execution path: only stages on the path, and only edges between them.
    let graphs: Vec<PathGraph> = PATHS
        .iter()
        .map(|&path| {
            let on: Vec<bool> = def.stages.iter().map(|s| s.min_path <= path).collect();
            PathGraph {
                path,
                anc: ancestors(def, &index, &order, &on),
                on,
            }
        })
        .collect();

    check_data_flow(def, &roles, &index, &anc, out);
    check_ownership(def, &roles, &graphs, out);
    check_review(def, &roles, &index, &graphs, out);
    check_paths(def, &roles, &index, &graphs, out);
    check_enforcement(def, ctx, limitations, out);
    Some(order)
}

fn check_roles(def: &WorkflowDef, ctx: &Context, out: &mut Vec<Violation>) {
    let mut seen = BTreeSet::new();
    for r in &def.roles {
        let at = format!("roles[{}]", r.id);
        if !valid_id(&r.id) {
            out.push(Violation::new(
                Code::Schema,
                &at,
                "ids use letters, digits, - and _",
            ));
        }
        if !seen.insert(r.id.as_str()) {
            out.push(Violation::new(Code::Duplicate, &at, "duplicate role id"));
        }
        for t in &r.tools {
            if !KNOWN_TOOLS.contains(&t.as_str()) {
                out.push(Violation::new(
                    Code::Capability,
                    &at,
                    format!("'{t}' is not a capability Agentree defines"),
                ));
            } else if !ctx.backend.tools.contains(t) {
                out.push(Violation::new(
                    Code::Capability,
                    &at,
                    format!("{} does not support '{t}'", ctx.backend.name),
                ));
            } else if !def.policy.allowed_capabilities.contains(t) {
                out.push(Violation::new(
                    Code::Capability,
                    &at,
                    format!("'{t}' is not permitted by this workflow's policy"),
                ));
            }
        }
        if !(1..=MAX_CONCURRENT).contains(&r.max_concurrent) {
            out.push(Violation::new(
                Code::Schema,
                &at,
                format!("max_concurrent must be 1 to {MAX_CONCURRENT}"),
            ));
        }
        // Permission and tool coherence: only implementation may edit.
        let edits = r.tools.contains("edit");
        if r.class != RoleClass::Implementation && (r.permission == Permission::Edit || edits) {
            out.push(Violation::new(
                Code::Capability,
                &at,
                "only implementation roles may edit; this role must be read-only",
            ));
        }
        if r.permission == Permission::ReadOnly && edits {
            out.push(Violation::new(
                Code::Capability,
                &at,
                "a read-only role cannot hold the edit tool",
            ));
        }
        if r.class == RoleClass::Implementation && !edits {
            out.push(Violation::new(
                Code::Capability,
                &at,
                "an implementation role needs the edit tool",
            ));
        }
        if !r.mcp.is_empty() && !r.tools.contains("mcp") {
            out.push(Violation::new(
                Code::Capability,
                &at,
                "MCP servers need the 'mcp' tool on the role",
            ));
        }
        // Mandatory model policies.
        match (&r.class, &r.model) {
            (RoleClass::BulkRead, ModelPolicy::LowestCost) => {
                if !ctx.registry.any_capable(&r.requirement(0)) {
                    out.push(Violation::new(
                        Code::ModelPolicy,
                        &at,
                        "no configured model has the tools, permissions and data access \
                         this bulk-reading role needs",
                    ));
                }
            }
            (RoleClass::BulkRead, _) => out.push(Violation::new(
                Code::ModelPolicy,
                &at,
                "bulk-reading roles must use the lowest-cost capable model",
            )),
            (RoleClass::Review, ModelPolicy::StrongestReview) => {
                if !r.tools.contains("read") || !r.tools.contains("search") {
                    out.push(Violation::new(
                        Code::Review,
                        &at,
                        "the reviewer needs read and search to request targeted inspection",
                    ));
                }
                if !ctx.registry.any_reviewer(&r.requirement(0)) {
                    out.push(Violation::new(
                        Code::ModelPolicy,
                        &at,
                        "no configured model can act as the final reviewer",
                    ));
                }
            }
            (RoleClass::Review, _) => out.push(Violation::new(
                Code::ModelPolicy,
                &at,
                "the final reviewer must use the strongest eligible model",
            )),
            (_, ModelPolicy::LowestCost | ModelPolicy::StrongestReview) => {
                out.push(Violation::new(
                    Code::ModelPolicy,
                    &at,
                    "lowest_cost applies only to bulk-reading roles and strongest_review only to the reviewer",
                ))
            }
            (_, ModelPolicy::Pinned { model }) => match ctx.registry.get(model) {
                None => out.push(Violation::new(
                    Code::ModelPolicy,
                    &at,
                    format!("pinned model '{model}' is not in the registry"),
                )),
                Some(m) => {
                    let req = r.requirement(0);
                    if !req.tools.iter().all(|t| m.tools.contains(t))
                        || m.max_permission < req.permission
                        || !m.data_classes.contains(&req.data_class)
                    {
                        out.push(Violation::new(
                            Code::ModelPolicy,
                            &at,
                            format!("pinned model '{model}' cannot satisfy this role's requirements"),
                        ));
                    }
                }
            },
            (_, ModelPolicy::Configured) => {}
        }
        check_plugin_refs(r, ctx, &at, out);
    }
}

/// A plugin's skills and MCP servers reach a role only through a trusted plugin; trust never
/// widens the role's own tools or permission.
fn check_plugin_refs(r: &RoleDef, ctx: &Context, at: &str, out: &mut Vec<Violation>) {
    let refs = r
        .skill
        .iter()
        .map(|s| (s, true))
        .chain(r.mcp.iter().map(|m| (m, false)));
    for (reference, is_skill) in refs {
        let named: Vec<&plugins::Plugin> = ctx
            .plugins
            .iter()
            .filter(|p| p.manifest.name == reference.plugin)
            .collect();
        if named.len() > 1 {
            // Resolution is by name; two loaded plugins of one name could swap a trusted one
            // for an untrusted one after validation.
            out.push(Violation::new(
                Code::Plugin,
                at,
                format!(
                    "{} loaded plugins are named '{}'",
                    named.len(),
                    reference.plugin
                ),
            ));
            continue;
        }
        let Some(&plugin) = named.first() else {
            out.push(Violation::new(
                Code::Plugin,
                at,
                format!("plugin '{}' is not loaded", reference.plugin),
            ));
            continue;
        };
        let Some(active) = ctx.trust.activate(plugin) else {
            out.push(Violation::new(
                Code::Plugin,
                at,
                format!(
                    "plugin '{}' is {}; it cannot be used by a workflow",
                    reference.plugin,
                    ctx.trust.state(plugin).label()
                ),
            ));
            continue;
        };
        let found = if is_skill {
            active.skills.iter().any(|s| s.name == reference.name)
        } else {
            active.mcp.iter().any(|m| m.name == reference.name)
        };
        if !found {
            out.push(Violation::new(
                Code::Plugin,
                at,
                format!(
                    "trusted plugin '{}' has no {} '{}'",
                    reference.plugin,
                    if is_skill { "skill" } else { "MCP server" },
                    reference.name
                ),
            ));
        }
    }
}

fn check_data_flow(
    def: &WorkflowDef,
    roles: &BTreeMap<&str, &RoleDef>,
    index: &BTreeMap<&str, usize>,
    anc: &[BTreeSet<usize>],
    out: &mut Vec<Violation>,
) {
    for (i, s) in def.stages.iter().enumerate() {
        let at = format!("stages[{}]", s.id);
        let class = roles.get(s.role.as_str()).map(|r| r.class);
        for input in &s.inputs {
            let Some(from) = &input.from else { continue };
            match index.get(from.stage.as_str()) {
                None => out.push(Violation::new(
                    Code::Unknown,
                    &at,
                    format!(
                        "input '{}' comes from unknown stage '{}'",
                        input.name, from.stage
                    ),
                )),
                Some(&f) if !anc[i].contains(&f) => out.push(Violation::new(
                    Code::Unknown,
                    &at,
                    format!(
                        "input '{}' comes from '{}', which is not a dependency",
                        input.name, from.stage
                    ),
                )),
                Some(&f) => {
                    if !def.stages[f].outputs.iter().any(|o| o.name == from.output) {
                        out.push(Violation::new(
                            Code::Unknown,
                            &at,
                            format!("'{}' has no output '{}'", from.stage, from.output),
                        ));
                    }
                }
            }
        }
        // Bulk readers hand back compact, source-referenced evidence.
        if class == Some(RoleClass::BulkRead)
            && !s.outputs.iter().any(|o| o.kind == ArtifactKind::Evidence)
        {
            out.push(Violation::new(
                Code::Evidence,
                &at,
                "a bulk-reading stage must produce an evidence output",
            ));
        }
        if class == Some(RoleClass::Implementation)
            && !s.outputs.iter().any(|o| o.kind == ArtifactKind::Diff)
        {
            out.push(Violation::new(
                Code::Review,
                &at,
                "an implementation stage must produce a diff output",
            ));
        }
    }
}

fn check_ownership(
    def: &WorkflowDef,
    roles: &BTreeMap<&str, &RoleDef>,
    graphs: &[PathGraph],
    out: &mut Vec<Violation>,
) {
    let imp: Vec<usize> = def
        .stages
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            roles.get(s.role.as_str()).map(|r| r.class) == Some(RoleClass::Implementation)
        })
        .map(|(i, _)| i)
        .collect();
    for (i, s) in def.stages.iter().enumerate() {
        let at = format!("stages[{}]", s.id);
        let is_imp = imp.contains(&i);
        if is_imp && s.owns.is_empty() {
            out.push(Violation::new(
                Code::Ownership,
                &at,
                "an implementation stage must declare the paths it owns",
            ));
        }
        if !is_imp && !s.owns.is_empty() {
            out.push(Violation::new(
                Code::Ownership,
                &at,
                "only implementation stages may own paths",
            ));
        }
        for p in &s.owns {
            if normalize_owned(p).is_none() {
                out.push(Violation::new(
                    Code::Ownership,
                    &at,
                    format!("'{p}' is ambiguous or not a plain relative path inside the workspace"),
                ));
            }
        }
    }
    // Two writers that may run at the same time cannot own overlapping paths. "May run at the
    // same time" is decided per path: an optional stage that is omitted orders nothing.
    let mut reported = BTreeSet::new();
    for g in graphs {
        for (n, &a) in imp.iter().enumerate().filter(|(_, &a)| g.on[a]) {
            for &b in imp[n + 1..].iter().filter(|&&b| g.on[b]) {
                if g.anc[a].contains(&b) || g.anc[b].contains(&a) {
                    continue;
                }
                for pa in &def.stages[a].owns {
                    for pb in &def.stages[b].owns {
                        if overlaps(pa, pb) && reported.insert((a, b, pa, pb)) {
                            out.push(Violation::new(
                                Code::Ownership,
                                format!("stages[{}]", def.stages[a].id),
                                format!(
                                    "may run alongside '{}' on the {} path but both own \
                                     '{pa}'/'{pb}'",
                                    def.stages[b].id,
                                    g.path.label()
                                ),
                            ));
                        }
                    }
                }
            }
        }
    }
}

fn check_review(
    def: &WorkflowDef,
    roles: &BTreeMap<&str, &RoleDef>,
    index: &BTreeMap<&str, usize>,
    graphs: &[PathGraph],
    out: &mut Vec<Violation>,
) {
    let class_of = |s: &Stage| roles.get(s.role.as_str()).map(|r| r.class);
    let imps: Vec<usize> = (0..def.stages.len())
        .filter(|&i| class_of(&def.stages[i]) == Some(RoleClass::Implementation))
        .collect();
    let reviews: Vec<usize> = (0..def.stages.len())
        .filter(|&i| class_of(&def.stages[i]) == Some(RoleClass::Review))
        .collect();
    if imps.is_empty() && reviews.is_empty() {
        return;
    }
    if reviews.len() != 1 {
        out.push(Violation::new(
            Code::Review,
            "stages",
            format!(
                "a workflow that changes code needs exactly one final review stage, in a                  role of its own and independent of implementation; found {}",
                reviews.len()
            ),
        ));
        return;
    }
    let r = reviews[0];
    let rs = &def.stages[r];
    let at = format!("stages[{}]", rs.id);
    if rs.min_path != ExecutionPath::Fast {
        out.push(Violation::new(
            Code::Review,
            &at,
            "final review is mandatory on every path; min_path must be 'fast'",
        ));
    }
    // The review is last on every path: every other stage on the path is one of its
    // dependencies there. A link through an omitted optional stage does not count.
    let mut late = BTreeSet::new();
    for g in graphs.iter().filter(|g| g.on[r]) {
        for i in (0..def.stages.len()).filter(|&i| i != r && g.on[i]) {
            if !g.anc[r].contains(&i) && late.insert(i) {
                out.push(Violation::new(
                    Code::Review,
                    &at,
                    format!(
                        "stage '{}' is not a dependency of the final review on the {} path, \
                         so it could run after it",
                        def.stages[i].id,
                        g.path.label()
                    ),
                ));
            }
        }
    }
    if rs.outputs.iter().all(|o| o.kind != ArtifactKind::Verdict) {
        out.push(Violation::new(
            Code::Review,
            &at,
            "the review stage must produce a verdict output",
        ));
    }
    // The reviewer is given the actual diff and the verification evidence, structurally.
    for name in REVIEW_INPUTS {
        match rs.inputs.iter().find(|i| i.name == name) {
            Some(i) if i.required => {}
            _ => out.push(Violation::new(
                Code::Review,
                &at,
                format!("the review stage needs a required input '{name}'"),
            )),
        }
    }
    let kind_from = |name: &str| -> Option<(ArtifactKind, Option<RoleClass>)> {
        let from = rs.inputs.iter().find(|i| i.name == name)?.from.as_ref()?;
        let st = def.stages.get(*index.get(from.stage.as_str())?)?;
        let kind = st.outputs.iter().find(|o| o.name == from.output)?.kind;
        Some((kind, class_of(st)))
    };
    if kind_from("diff") != Some((ArtifactKind::Diff, Some(RoleClass::Implementation))) {
        out.push(Violation::new(
            Code::Review,
            &at,
            "input 'diff' must come from an implementation stage's diff output",
        ));
    }
    if kind_from("verification")
        != Some((ArtifactKind::Verification, Some(RoleClass::Verification)))
    {
        out.push(Violation::new(
            Code::Review,
            &at,
            "input 'verification' must come from a verification stage's output",
        ));
    }
    match &rs.inspection {
        Some(ins)
            if (1..=MAX_INSPECTIONS).contains(&ins.max_requests)
                && roles.get(ins.reader_role.as_str()).map(|r| r.class)
                    == Some(RoleClass::BulkRead) => {}
        _ => out.push(Violation::new(
            Code::Review,
            &at,
            format!(
                "the reviewer must be able to request targeted inspection: 1 to \
                 {MAX_INSPECTIONS} requests answered by a bulk-reading role"
            ),
        )),
    }
    for &i in imps.iter().filter(|i| late.contains(i)) {
        out.push(Violation::new(
            Code::Review,
            &at,
            format!("implementation '{}' is not reviewed", def.stages[i].id),
        ));
    }
}

/// Every execution path must stand on its own: a stage on the path never waits for a stage
/// that is not on it, whatever it requires is on the path and ordered before it, and code
/// changes are always verified before review.
fn check_paths(
    def: &WorkflowDef,
    roles: &BTreeMap<&str, &RoleDef>,
    index: &BTreeMap<&str, usize>,
    graphs: &[PathGraph],
    out: &mut Vec<Violation>,
) {
    let class_of = |s: &Stage| roles.get(s.role.as_str()).map(|r| r.class);
    let mut waits_off_path = BTreeSet::new();
    for g in graphs {
        let on = |i: usize| g.on[i];
        for i in (0..def.stages.len()).filter(|&i| on(i)) {
            let at = format!("stages[{}]", def.stages[i].id);
            // A required dependency that is omitted from this path would block it forever.
            for d in &def.stages[i].depends_on {
                if let Some(&f) = index.get(d.as_str()).filter(|&&f| !on(f)) {
                    if waits_off_path.insert((i, f)) {
                        out.push(Violation::new(
                            Code::Schema,
                            &at,
                            format!(
                                "depends_on '{d}', which does not run on the {} path; make it \
                                 optional_depends_on or raise this stage's min_path",
                                g.path.label()
                            ),
                        ));
                    }
                }
            }
            for input in def.stages[i].inputs.iter().filter(|i| i.required) {
                let Some(&f) = input
                    .from
                    .as_ref()
                    .and_then(|f| index.get(f.stage.as_str()))
                else {
                    continue;
                };
                let problem = if !on(f) {
                    "which does not run there"
                } else if !g.anc[i].contains(&f) {
                    "which is not ordered before it there"
                } else {
                    continue;
                };
                out.push(Violation::new(
                    Code::Schema,
                    &at,
                    format!(
                        "on the {} path it requires '{}' from '{}', {problem}",
                        g.path.label(),
                        input.name,
                        def.stages[f].id
                    ),
                ));
            }
            if class_of(&def.stages[i]) == Some(RoleClass::Implementation) {
                let verified = (0..def.stages.len()).any(|v| {
                    on(v)
                        && class_of(&def.stages[v]) == Some(RoleClass::Verification)
                        && g.anc[v].contains(&i)
                });
                if !verified {
                    out.push(Violation::new(
                        Code::Review,
                        &at,
                        format!("is not verified on the {} path", g.path.label()),
                    ));
                }
            }
        }
    }
}

fn check_enforcement(
    def: &WorkflowDef,
    ctx: &Context,
    limitations: &mut Vec<String>,
    out: &mut Vec<Violation>,
) {
    let b = ctx.backend;
    let ids = |class: RoleClass| -> Vec<&str> {
        def.roles
            .iter()
            .filter(|r| r.class == class)
            .map(|r| r.id.as_str())
            .collect()
    };
    let bulk = ids(RoleClass::BulkRead);
    let review = ids(RoleClass::Review);
    let mut gaps: Vec<(String, Enforcement)> = Vec::new();
    if !bulk.is_empty() && b.role_model < Enforcement::Enforced {
        gaps.push((
            format!(
                "{} cannot make bulk-reading roles ({}) run on the lowest-cost model; it is {}",
                b.name,
                bulk.join(", "),
                b.role_model.label()
            ),
            b.role_model,
        ));
    }
    if !review.is_empty() && b.review_gate < Enforcement::Enforced {
        gaps.push((
            format!(
                "{} cannot prevent the final review ({}) from being skipped; it is {}",
                b.name,
                review.join(", "),
                b.review_gate.label()
            ),
            b.review_gate,
        ));
    }
    if !review.is_empty() && b.review_independence < Enforcement::Enforced {
        gaps.push((
            format!(
                "{} does not give the reviewer ({}) a separate packet of the actual diff and \
                 evidence, so reviewer independence is {}",
                b.name,
                review.join(", "),
                b.review_independence.label()
            ),
            b.review_independence,
        ));
    }
    if !bulk.is_empty() && b.coordinator_bulk_reading < Enforcement::Enforced {
        gaps.push((
            format!(
                "{} cannot stop the lead session from reading bulk context on its own model; \
                 delegation is {}",
                b.name,
                b.coordinator_bulk_reading.label()
            ),
            b.coordinator_bulk_reading,
        ));
    }
    if def
        .roles
        .iter()
        .any(|r| r.permission == Permission::ReadOnly)
        && b.role_permission < Enforcement::Enforced
    {
        gaps.push((
            format!(
                "{} does not apply per-role permissions or tool lists; they are {}",
                b.name,
                b.role_permission.label()
            ),
            b.role_permission,
        ));
    }
    for (message, _) in gaps {
        if def.policy.strict_enforcement {
            out.push(Violation::new(Code::Enforcement, "policy", message));
        } else {
            limitations.push(message);
        }
    }
}

/// The software-engineering workflow Agentree ships. Roles use the keys from `roles::Role`
/// and, when `with_skills` is set, the skills of the built-in `software-engineering` plugin.
pub fn default_workflow(with_skills: bool) -> WorkflowDef {
    let skill = |name: &str| {
        with_skills.then(|| PluginRef {
            plugin: "software-engineering".into(),
            name: name.into(),
        })
    };
    let tools = |t: &[&str]| t.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>();
    let role = |id: &str,
                class: RoleClass,
                model: ModelPolicy,
                skill_name: &str,
                t: &[&str],
                permission: Permission,
                concurrent: u8| RoleDef {
        id: id.into(),
        class,
        model,
        skill: skill(skill_name),
        mcp: vec![],
        tools: tools(t),
        permission,
        data_class: repository(),
        max_concurrent: concurrent,
    };
    let out = |name: &str, kind: ArtifactKind| Output {
        name: name.into(),
        kind,
    };
    let from = |stage: &str, output: &str| {
        Some(OutputRef {
            stage: stage.into(),
            output: output.into(),
        })
    };
    let input = |name: &str, f: Option<OutputRef>, required: bool| Input {
        name: name.into(),
        from: f,
        required,
    };
    let deps = |d: &[&str]| d.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let stage = |id: &str,
                 role: &str,
                 depends: &[&str],
                 min_path: ExecutionPath,
                 inputs: Vec<Input>,
                 outputs: Vec<Output>| Stage {
        id: id.into(),
        role: role.into(),
        depends_on: deps(depends),
        optional_depends_on: vec![],
        min_path,
        inputs,
        outputs,
        owns: vec![],
        policy: StagePolicy::default(),
        inspection: None,
    };
    use ArtifactKind::*;
    use ExecutionPath::*;
    let mut implement = stage(
        "implement",
        "coder",
        &["explore"],
        Fast,
        vec![
            input("evidence", from("explore", "evidence"), true),
            input("plan", from("plan", "plan"), false),
            input("objective", None, true),
        ],
        vec![out("diff", Diff)],
    );
    // `plan` runs from Balanced up; on Fast the implementer starts without waiting for it.
    implement.optional_depends_on = deps(&["plan"]);
    implement.owns = vec![".".into()];
    let mut review = stage(
        "review",
        "final_reviewer",
        &["implement", "test"],
        Fast,
        vec![
            input("diff", from("implement", "diff"), true),
            input("objective", None, true),
            input("acceptance_criteria", None, true),
            input("verification", from("test", "results"), true),
            input("evidence", from("prepare-review", "evidence"), false),
            input("risks", from("risk-scan", "evidence"), false),
        ],
        vec![out("verdict", Verdict)],
    );
    // Extra evidence exists only on the paths that run its stage; review waits only for those.
    review.optional_depends_on = deps(&["prepare-review", "risk-scan"]);
    review.inspection = Some(Inspection {
        max_requests: 5,
        reader_role: "explorer".into(),
    });
    WorkflowDef {
        schema: SCHEMA.into(),
        version: VERSION,
        name: "software-engineering".into(),
        roles: vec![
            role(
                "explorer",
                RoleClass::BulkRead,
                ModelPolicy::LowestCost,
                "explore",
                &["read", "search"],
                Permission::ReadOnly,
                3,
            ),
            role(
                "researcher",
                RoleClass::BulkRead,
                ModelPolicy::LowestCost,
                "research",
                &["read", "search", "web"],
                Permission::ReadOnly,
                3,
            ),
            role(
                "planner",
                RoleClass::Planning,
                ModelPolicy::Configured,
                "plan",
                &["read", "search"],
                Permission::ReadOnly,
                1,
            ),
            role(
                "coder",
                RoleClass::Implementation,
                ModelPolicy::Configured,
                "implement",
                &["read", "search", "shell", "edit"],
                Permission::Edit,
                3,
            ),
            role(
                "tester",
                RoleClass::Verification,
                ModelPolicy::Configured,
                "test",
                &["read", "search", "shell"],
                Permission::ReadOnly,
                2,
            ),
            role(
                "final_reviewer",
                RoleClass::Review,
                ModelPolicy::StrongestReview,
                "review",
                &["read", "search"],
                Permission::ReadOnly,
                1,
            ),
        ],
        stages: vec![
            stage(
                "explore",
                "explorer",
                &[],
                Fast,
                vec![input("objective", None, true)],
                vec![out("evidence", Evidence)],
            ),
            stage(
                "research",
                "researcher",
                &[],
                Balanced,
                vec![input("objective", None, true)],
                vec![out("evidence", Evidence)],
            ),
            stage(
                "plan",
                "planner",
                &["explore", "research"],
                Balanced,
                vec![
                    input("evidence", from("explore", "evidence"), true),
                    input("research", from("research", "evidence"), false),
                ],
                vec![out("plan", Plan)],
            ),
            implement,
            stage(
                "test",
                "tester",
                &["implement"],
                Fast,
                vec![input("diff", from("implement", "diff"), true)],
                vec![out("results", Verification)],
            ),
            stage(
                "prepare-review",
                "explorer",
                &["implement", "test"],
                Balanced,
                vec![
                    input("diff", from("implement", "diff"), true),
                    input("verification", from("test", "results"), true),
                ],
                vec![out("evidence", Evidence)],
            ),
            stage(
                "risk-scan",
                "explorer",
                &["implement", "test"],
                Thorough,
                vec![input("diff", from("implement", "diff"), true)],
                vec![out("evidence", Evidence)],
            ),
            review,
        ],
        policy: WorkflowPolicy::default(),
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;
    use crate::plugins::{self, Plugin, Store};
    use crate::workflow::registry::fixtures::registry;

    pub fn builtin_plugin() -> Plugin {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/software-engineering");
        plugins::load(&dir, &std::env::temp_dir()).unwrap()
    }

    pub fn trusted(plugin: &Plugin) -> Store {
        let mut s = Store::default();
        s.trust(plugin).unwrap();
        s
    }

    /// A registry for the default workflow: every model has the tools the roles hold.
    pub fn full_registry() -> Registry {
        let mut r = registry();
        for m in &mut r.models {
            m.tools.insert("web".into());
        }
        r
    }

    pub fn validate(def: WorkflowDef) -> Result<ValidatedWorkflow, Vec<Violation>> {
        let plugin = builtin_plugin();
        let store = trusted(&plugin);
        let reg = full_registry();
        let backend = BackendProfile::claude_native();
        def.validate(&Context {
            registry: &reg,
            backend: &backend,
            plugins: std::slice::from_ref(&plugin),
            trust: &store,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use crate::plugins::Store;

    fn codes(r: Result<ValidatedWorkflow, Vec<Violation>>) -> Vec<Code> {
        r.err().unwrap().iter().map(|v| v.code).collect()
    }

    #[test]
    fn the_default_workflow_validates_and_reports_claude_limitations() {
        let ok = validate(default_workflow(true)).unwrap();
        assert!(ok.limitations().iter().any(|l| l.contains("lowest-cost")));
        assert!(ok.limitations().iter().any(|l| l.contains("final review")));
        assert!(ok.limitations().iter().any(|l| l.contains("independence")));
        assert!(ok.limitations().iter().any(|l| l.contains("lead session")));
        assert!(ok.limitations().iter().all(|l| !l.contains("enforced:")));
    }

    #[test]
    fn paths_add_stages_and_every_path_ends_in_review() {
        let ok = validate(default_workflow(true)).unwrap();
        let ids = |p| -> Vec<String> { ok.stages_for(p).iter().map(|s| s.id.clone()).collect() };
        assert_eq!(
            ids(ExecutionPath::Fast),
            ["explore", "implement", "test", "review"]
        );
        assert!(ids(ExecutionPath::Balanced).contains(&"prepare-review".to_string()));
        assert!(!ids(ExecutionPath::Balanced).contains(&"risk-scan".to_string()));
        assert!(ids(ExecutionPath::Thorough).contains(&"risk-scan".to_string()));
        for p in PATHS {
            assert_eq!(ok.stages_for(p).last().unwrap().id, "review");
        }
    }

    fn stage<'a>(d: &'a mut WorkflowDef, id: &str) -> &'a mut Stage {
        d.stages.iter_mut().find(|s| s.id == id).unwrap()
    }

    #[test]
    fn every_path_is_a_self_contained_dependency_graph() {
        let ok = validate(default_workflow(true)).unwrap();
        for p in PATHS {
            let ids: Vec<&str> = ok.stages_for(p).iter().map(|s| s.id.as_str()).collect();
            for (n, s) in ok.stages_for(p).into_iter().enumerate() {
                for w in ok.waits_for(p, s) {
                    // Waits only for stages on this path, and those are scheduled earlier.
                    assert!(
                        ids[..n].contains(&w),
                        "{p:?}: '{}' waits for '{w}' which is not before it",
                        s.id
                    );
                }
            }
        }
        let waits = |p, id: &str| {
            let s = ok.def().stages.iter().find(|s| s.id == id).unwrap();
            ok.waits_for(p, s)
        };
        // `plan` and the extra evidence exist only on higher paths.
        assert_eq!(waits(ExecutionPath::Fast, "implement"), ["explore"]);
        assert_eq!(
            waits(ExecutionPath::Balanced, "implement"),
            ["explore", "plan"]
        );
        assert_eq!(waits(ExecutionPath::Fast, "review"), ["implement", "test"]);
        assert_eq!(
            waits(ExecutionPath::Balanced, "review"),
            ["implement", "test", "prepare-review"]
        );
        assert_eq!(
            waits(ExecutionPath::Thorough, "review"),
            ["implement", "test", "prepare-review", "risk-scan"]
        );
        // A stage that is not on the path waits for nothing.
        assert!(waits(ExecutionPath::Fast, "plan").is_empty());
    }

    #[test]
    fn a_required_dependency_that_only_a_higher_path_runs_is_rejected() {
        // The old shape: the Fast implementer waits for the Balanced-only planner.
        let mut d = default_workflow(false);
        stage(&mut d, "implement").depends_on.push("plan".into());
        stage(&mut d, "implement").optional_depends_on.clear();
        let v = validate(d).unwrap_err();
        assert!(v.iter().any(|v| v.code == Code::Schema
            && v.at == "stages[implement]"
            && v.message.contains("Fast")));
        // Same for the review waiting on an extra evidence stage.
        let mut d = default_workflow(false);
        let rv = stage(&mut d, "review");
        rv.depends_on.push("risk-scan".into());
        rv.optional_depends_on.retain(|s| s != "risk-scan");
        assert!(codes(validate(d)).contains(&Code::Schema));
        // A required *input* from a higher-path stage is rejected even when it is ordered.
        let mut d = default_workflow(false);
        stage(&mut d, "review")
            .inputs
            .iter_mut()
            .find(|i| i.name == "risks")
            .unwrap()
            .required = true;
        assert!(codes(validate(d)).contains(&Code::Schema));
        // Optional ordering is the supported way to depend on a higher-path stage.
        assert!(validate(default_workflow(false)).is_ok());
    }

    #[test]
    fn optional_dependencies_are_unambiguous_and_still_ordered() {
        // Listing a stage as both required and optional is ambiguous.
        let mut d = default_workflow(false);
        stage(&mut d, "review")
            .optional_depends_on
            .push("implement".into());
        assert!(codes(validate(d)).contains(&Code::Duplicate));
        // Optional edges take part in cycle detection.
        let mut d = default_workflow(false);
        stage(&mut d, "explore").optional_depends_on = vec!["review".into()];
        assert!(codes(validate(d)).contains(&Code::Cycle));
        // And in reference checks.
        let mut d = default_workflow(false);
        stage(&mut d, "review").optional_depends_on = vec!["ghost".into()];
        assert!(codes(validate(d)).contains(&Code::Unknown));
        // A stage on a path that is not reachable from review there is flagged, per path:
        // dropping the optional edge makes `risk-scan` unordered with the review on Thorough.
        let mut d = default_workflow(false);
        stage(&mut d, "review")
            .optional_depends_on
            .retain(|s| s != "risk-scan");
        let v = validate(d).unwrap_err();
        assert!(v
            .iter()
            .any(|v| v.code == Code::Review && v.message.contains("Thorough")));
    }

    #[test]
    fn dependency_cycles_are_rejected() {
        let mut d = default_workflow(false);
        d.stages[0].depends_on = vec!["review".into()];
        assert!(codes(validate(d)).contains(&Code::Cycle));
        let mut d = default_workflow(false);
        d.stages[0].depends_on = vec!["explore".into()];
        assert!(codes(validate(d)).contains(&Code::Cycle));
    }

    #[test]
    fn invalid_roles_and_unsupported_capabilities_are_rejected() {
        let mut d = default_workflow(false);
        d.roles[0].tools.insert("teleport".into());
        d.roles[1].id = "explorer".into();
        d.stages[1].role = "ghost".into();
        let c = codes(validate(d));
        assert!(c.contains(&Code::Capability));
        assert!(c.contains(&Code::Duplicate));
        assert!(c.contains(&Code::Unknown));
        // A capability the user's policy does not permit is rejected too.
        let mut d = default_workflow(false);
        d.policy.allowed_capabilities.remove("web");
        assert!(codes(validate(d)).contains(&Code::Capability));
        // And one the backend does not have.
        let plugin = builtin_plugin();
        let store = trusted(&plugin);
        let reg = full_registry();
        let mut backend = BackendProfile::claude_native();
        backend.tools.remove("shell");
        let r = default_workflow(false).validate(&Context {
            registry: &reg,
            backend: &backend,
            plugins: std::slice::from_ref(&plugin),
            trust: &store,
        });
        assert!(r
            .unwrap_err()
            .iter()
            .any(|v| v.message.contains("does not support 'shell'")));
    }

    #[test]
    fn bulk_roles_cannot_be_pinned_to_a_pricier_model() {
        let mut d = default_workflow(false);
        d.roles[0].model = ModelPolicy::Pinned {
            model: "mid".into(),
        };
        assert!(codes(validate(d)).contains(&Code::ModelPolicy));
        let mut d = default_workflow(false);
        d.roles[0].model = ModelPolicy::Configured;
        assert!(codes(validate(d)).contains(&Code::ModelPolicy));
    }

    #[test]
    fn the_reviewer_must_use_the_strongest_policy_on_every_path() {
        let mut d = default_workflow(false);
        let r = d
            .roles
            .iter_mut()
            .find(|r| r.id == "final_reviewer")
            .unwrap();
        r.model = ModelPolicy::Pinned {
            model: "cheap".into(),
        };
        assert!(codes(validate(d)).contains(&Code::ModelPolicy));
        let mut d = default_workflow(false);
        d.stages
            .iter_mut()
            .find(|s| s.id == "review")
            .unwrap()
            .min_path = ExecutionPath::Thorough;
        assert!(codes(validate(d)).contains(&Code::Review));
    }

    #[test]
    fn code_changes_without_a_review_or_verification_are_rejected() {
        let mut d = default_workflow(false);
        d.stages.retain(|s| s.id != "review");
        assert!(codes(validate(d)).contains(&Code::Review));

        let mut d = default_workflow(false);
        let t = d.stages.iter_mut().find(|s| s.id == "test").unwrap();
        t.min_path = ExecutionPath::Thorough;
        assert!(codes(validate(d)).contains(&Code::Schema));
    }

    #[test]
    fn the_reviewer_must_receive_the_diff_and_be_independent() {
        let mut d = default_workflow(false);
        let rv = d.stages.iter_mut().find(|s| s.id == "review").unwrap();
        rv.inputs.retain(|i| i.name != "diff");
        rv.inspection = None;
        let c = validate(d).unwrap_err();
        assert!(c.iter().any(|v| v.message.contains("'diff'")));
        assert!(c.iter().any(|v| v.message.contains("targeted inspection")));

        let mut d = default_workflow(false);
        d.stages
            .iter_mut()
            .find(|s| s.id == "implement")
            .unwrap()
            .role = "final_reviewer".into();
        let c = validate(d).unwrap_err();
        assert!(c
            .iter()
            .any(|v| v.code == Code::Review && v.message.contains("independent")));
    }

    #[test]
    fn writers_with_overlapping_ownership_are_rejected() {
        let mut d = default_workflow(false);
        let mut second = d
            .stages
            .iter()
            .find(|s| s.id == "implement")
            .unwrap()
            .clone();
        second.id = "implement-b".into();
        second.depends_on = vec!["explore".into()];
        second.owns = vec!["src".into()];
        d.stages.push(second);
        d.stages
            .iter_mut()
            .find(|s| s.id == "review")
            .unwrap()
            .depends_on
            .push("implement-b".into());
        assert!(codes(validate(d)).contains(&Code::Ownership));
    }

    #[test]
    fn equivalent_paths_normalize_the_same_and_aliases_overlap() {
        let root = ["."];
        let same_as_root = [".", "./", ".\\", "", "./."];
        // Whole workspace overlaps everything, in any spelling.
        for r in root.iter().chain(&same_as_root[..3]) {
            for p in ["src", "src/", "./src", "src/./module.rs", "SRC\\Module.rs"] {
                assert!(overlaps(r, p), "{r:?} vs {p:?}");
                assert!(overlaps(p, r), "{p:?} vs {r:?}");
            }
        }
        assert_eq!(normalize_owned("./"), Some(String::new()));
        for (a, b) in [
            ("src", "src/"),
            ("src", "./src"),
            ("src", "SRC"),
            ("src", "src\\"),
            ("src/./module.rs", "src/module.rs"),
            ("src/module.rs", "SRC\\Module.rs"),
            ("src", "src/./module.rs"),
            ("src/a", "src\\a\\b\\c.rs"),
            ("src//a", "src/a"),
            ("./src/.", "src"),
        ] {
            assert!(overlaps(a, b) && overlaps(b, a), "{a:?} vs {b:?}");
        }
        // Distinct siblings and look-alike prefixes do not overlap.
        for (a, b) in [
            ("src", "srcs"),
            ("src/a", "src/b"),
            ("src/a.rs", "src/a"),
            ("docs", "src/docs"),
        ] {
            assert!(!overlaps(a, b), "{a:?} vs {b:?}");
        }
    }

    #[test]
    fn ambiguous_ownership_spellings_fail_validation_and_always_overlap() {
        for bad in [
            "/src",
            "\\src",
            "C:\\src",
            "C:src",
            "../src",
            "src/../other",
            "src.",
            "src /a",
            "src ",
            "PROGRA~1",
            "src/*.rs",
            "a\u{0}b",
            "",
            "  ",
        ] {
            assert_eq!(normalize_owned(bad), None, "{bad:?}");
            assert!(overlaps(bad, "docs"), "{bad:?} must not bypass the check");
            let mut d = default_workflow(false);
            stage(&mut d, "implement").owns = vec![bad.into()];
            let v = validate(d).unwrap_err();
            assert!(
                v.iter().any(|v| v.code == Code::Ownership),
                "{bad:?} was accepted"
            );
        }
    }

    /// A second implementer that is unordered with `implement`, owning `path`.
    fn with_parallel_writer(path: &str, ordered: bool) -> WorkflowDef {
        let mut d = default_workflow(false);
        let mut second = stage(&mut d, "implement").clone();
        second.id = "implement-b".into();
        second.owns = vec![path.into()];
        if ordered {
            second.depends_on.push("implement".into());
        }
        d.stages.push(second);
        let rv = stage(&mut d, "review");
        rv.depends_on.push("implement-b".into());
        // Keep verification out of the picture: the new stage is tested by `test` too.
        stage(&mut d, "test").depends_on.push("implement-b".into());
        d
    }

    #[test]
    fn path_aliases_cannot_bypass_the_parallel_write_check() {
        // `implement` owns the whole workspace; every spelling below is a part of it.
        for alias in ["src", "./src/", "SRC\\module.rs", "src/./module.rs", "."] {
            let c = codes(validate(with_parallel_writer(alias, false)));
            assert!(c.contains(&Code::Ownership), "{alias:?} slipped through");
            // Ordering the writers removes the conflict.
            assert!(
                validate(with_parallel_writer(alias, true)).is_ok(),
                "{alias:?}"
            );
        }
        // Disjoint ownership of parallel writers is fine, whatever the separator or case.
        let mut d = with_parallel_writer("Tests\\unit", false);
        stage(&mut d, "implement").owns = vec!["./src/".into()];
        assert!(validate(d).is_ok());
        let mut d = with_parallel_writer("SRC\\module.rs", false);
        stage(&mut d, "implement").owns = vec!["src/./MODULE.rs".into()];
        assert!(codes(validate(d)).contains(&Code::Ownership));
    }

    #[test]
    fn parallel_writers_are_judged_on_each_path() {
        // On Balanced, `late` follows `implement`, and `early` follows `late`. Fast omits
        // `late`, which is the only thing ordering `early` after `implement`.
        let mut d = default_workflow(false);
        let mut early = stage(&mut d, "implement").clone();
        early.id = "early".into();
        early.owns = vec!["SRC\\".into()];
        early.optional_depends_on = vec!["late".into()];
        let mut late = early.clone();
        late.id = "late".into();
        late.min_path = ExecutionPath::Balanced;
        late.depends_on = vec!["implement".into()];
        late.optional_depends_on.clear();
        d.stages.push(early);
        d.stages.push(late);
        let rv = stage(&mut d, "review");
        rv.depends_on.push("early".into());
        rv.optional_depends_on.push("late".into());
        let t = stage(&mut d, "test");
        t.depends_on.push("early".into());
        t.optional_depends_on.push("late".into());
        let v = validate(d).unwrap_err();
        let own: Vec<_> = v.iter().filter(|v| v.code == Code::Ownership).collect();
        assert!(own.iter().any(|v| v.message.contains("Fast")), "{v:?}");
        assert!(own.iter().all(|v| !v.message.contains("Balanced")), "{v:?}");
    }

    #[test]
    fn untrusted_or_missing_plugins_cannot_supply_skills() {
        let plugin = builtin_plugin();
        let reg = full_registry();
        let backend = BackendProfile::claude_native();
        let untrusted = Store::default();
        let r = default_workflow(true).validate(&Context {
            registry: &reg,
            backend: &backend,
            plugins: std::slice::from_ref(&plugin),
            trust: &untrusted,
        });
        let v = r.unwrap_err();
        assert!(v
            .iter()
            .any(|v| v.code == Code::Plugin && v.message.contains("not trusted")));
        // Not loaded at all.
        let r = default_workflow(true).validate(&Context {
            registry: &reg,
            backend: &backend,
            plugins: &[],
            trust: &untrusted,
        });
        assert!(r
            .unwrap_err()
            .iter()
            .any(|v| v.message.contains("not loaded")));
        // Trusted, but the skill does not exist.
        let mut d = default_workflow(true);
        d.roles[0].skill.as_mut().unwrap().name = "teleport".into();
        assert!(codes(validate(d)).contains(&Code::Plugin));
    }

    #[test]
    fn a_trusted_plugin_does_not_widen_a_roles_permissions() {
        // Trust activates a skill; it cannot give a read-only role the edit tool.
        let mut d = default_workflow(true);
        d.roles[0].tools.insert("edit".into());
        assert!(codes(validate(d)).contains(&Code::Capability));
        // MCP servers are reachable only through the 'mcp' tool on a trusted plugin.
        let mut d = default_workflow(true);
        d.roles[0].mcp.push(PluginRef {
            plugin: "software-engineering".into(),
            name: "anything".into(),
        });
        let v = validate(d).unwrap_err();
        assert!(v.iter().any(|v| v.message.contains("'mcp' tool")));
    }

    #[test]
    fn strict_enforcement_fails_where_claude_can_only_instruct() {
        let mut d = default_workflow(false);
        d.policy.strict_enforcement = true;
        let v = validate(d).unwrap_err();
        assert!(v.iter().all(|v| v.code == Code::Enforcement));
        assert!(v.iter().any(|v| v.message.contains("lowest-cost")));
        assert!(v.iter().any(|v| v.message.contains("final review")));
    }

    #[test]
    fn a_backend_that_enforces_passes_strict_validation() {
        let plugin = builtin_plugin();
        let store = trusted(&plugin);
        let reg = full_registry();
        let mut backend = BackendProfile::claude_native();
        backend.role_model = Enforcement::Enforced;
        backend.role_permission = Enforcement::Enforced;
        backend.review_gate = Enforcement::Enforced;
        backend.review_independence = Enforcement::Enforced;
        backend.coordinator_bulk_reading = Enforcement::Enforced;
        let mut d = default_workflow(false);
        d.policy.strict_enforcement = true;
        let ok = d
            .validate(&Context {
                registry: &reg,
                backend: &backend,
                plugins: std::slice::from_ref(&plugin),
                trust: &store,
            })
            .unwrap();
        assert!(ok.limitations().is_empty());
    }

    #[test]
    fn parse_rejects_wrong_schema_and_newer_versions() {
        let good = serde_json::to_string(&default_workflow(false)).unwrap();
        assert!(parse(&good).is_ok());
        assert_eq!(parse("{").unwrap_err().code, Code::Schema);
        let newer = good.replace("\"version\":1", "\"version\":2");
        assert!(parse(&newer).unwrap_err().message.contains("version 2"));
        let other = good.replace("agentree.workflow/1", "other/1");
        assert_eq!(parse(&other).unwrap_err().at, "schema");
    }

    #[test]
    fn misspelled_keys_are_rejected_instead_of_falling_back_to_defaults() {
        let good = serde_json::to_string(&default_workflow(false)).unwrap();
        for (from, to) in [
            ("allowed_capabilities", "allowed_capabilites"),
            ("strict_enforcement", "strict_enforcment"),
            ("max_path", "max_pth"),
            ("depends_on", "depend_on"),
            ("lowest_cost", "lowest_cost\",\"model\":\"x"),
        ] {
            let bad = good.replacen(from, to, 1);
            assert!(parse(&bad).is_err(), "{from} -> {to} was accepted");
        }
    }

    #[test]
    fn two_loaded_plugins_with_one_name_cannot_be_resolved() {
        let trusted_copy = builtin_plugin();
        let store = trusted(&trusted_copy);
        let reg = full_registry();
        let backend = BackendProfile::claude_native();
        let both = [trusted_copy.clone(), trusted_copy];
        let r = default_workflow(true).validate(&Context {
            registry: &reg,
            backend: &backend,
            plugins: &both,
            trust: &store,
        });
        assert!(r
            .unwrap_err()
            .iter()
            .any(|v| v.code == Code::Plugin && v.message.contains("2 loaded plugins")));
    }

    #[test]
    fn the_example_file_is_the_builtin_definition() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("examples/workflows/software-engineering.workflow.json");
        let built = default_workflow(true);
        if std::env::var_os("AGENTREE_WRITE_EXAMPLES").is_some() {
            let text = serde_json::to_string_pretty(&built).unwrap() + "\n";
            std::fs::write(&path, text).unwrap();
        }
        let on_disk = parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(on_disk, built);
        assert!(validate(on_disk).is_ok());
    }

    #[test]
    fn workflow_metadata_stays_out_of_the_plugin_manifest() {
        let dir = std::env::temp_dir().join(format!("agentree-wf-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("plugin.json"),
            r#"{"$schema":"https://agent-plugins.org/schemas/1.0.0/plugin.schema.json","name":"x","roles":[{"id":"r"}],"workflow":{"stages":[]}}"#,
        )
        .unwrap();
        let plugin = plugins::load(&dir, &std::env::temp_dir()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(plugin
            .issues
            .iter()
            .any(|i| i.contains("unknown field 'workflow'")));
        assert!(plugin
            .issues
            .iter()
            .any(|i| i.contains("unknown field 'roles'")));
        assert_eq!(plugin.manifest.name, "x");
    }
}
