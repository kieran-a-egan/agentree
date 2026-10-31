//! Run provenance (Issue #12): a versioned, machine-readable record of which instruction sources,
//! trusted plugins and role skills were resolved and offered for a Claude-native run.
//!
//! The manifest is built from the decisions the launch actually used (the one
//! [`instructions::Resolved`], the one [`adapter::Brief`], the final policy text) and written once
//! per run as `run-manifest.json`. It starts and observes nothing.
//!
//! The typed [`Manifest`] *is* the allowlist: every field is a closed enum, an identifier, a count
//! or a SHA-256 digest. There is no field that can hold file or skill contents, prompts,
//! objectives, settings values, environment values, MCP arguments or error text, and no
//! absolute path. [`validate`] enforces that on every write and every read, and unknown fields
//! are rejected, so an exported manifest is re-emitted from the typed model, never from the raw
//! file.
//!
//! What a status means (see `docs/PROVENANCE.md`):
//! - `inspected`: Agentree read the file and hashed the bytes it read.
//! - `activated` / `offered`: the plugin passed `Store::activate` and the skill's path was written
//!   into the lead's instructions.
//! - `observed`: always `not_observed`. Agentree sees no Claude Code internals, so neither the
//!   loading of an instruction file nor the reading of a skill is ever claimed.

use crate::adapter::{Brief, ItemKind, Why};
use crate::instructions::{self, Delivery, Mode, Resolved, State, Text};
use crate::workflow::contract::ExecutionPath;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fmt, fs,
    io::{Read, Write},
    path::{Component, Path},
};
use uuid::Uuid;

pub const SCHEMA: &str = "agentree.run-manifest";
pub const VERSION: u32 = 1;
pub const FILE: &str = "run-manifest.json";
/// A larger file is refused on read; a manifest this size cannot come from a real run.
const MAX_BYTES: u64 = 256 * 1024;
const MAX_TEXT: usize = 96;
const MAX_ITEMS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: String,
    pub version: u32,
    pub run_id: String,
    pub agentree_version: String,
    pub backend: Backend,
    pub launch: LaunchState,
    /// `None` when no workflow was adapted (a simulated run).
    pub workflow: Option<Workflow>,
    pub lead_policy: LeadPolicy,
    pub instructions: Instructions,
    /// Only plugins that passed `Store::activate` for something offered to this run.
    pub plugins: Vec<PluginEntry>,
    pub skills: Vec<SkillEntry>,
    pub mcp: Vec<McpEntry>,
    /// Skills and MCP servers the workflow named that were not offered. Never activated.
    pub unavailable: Vec<UnavailableEntry>,
    pub limitations: Vec<Limitation>,
    pub warnings: Vec<Warning>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    ClaudeNativeTeams,
}

/// How far the launch got. None of these says Claude Code loaded anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchState {
    /// Written before the Claude process was started; the start is not recorded yet.
    Prepared,
    /// The operating system accepted the process start. Nothing past that is observed.
    Spawned,
    /// Starting the process failed. No instruction or skill reached a Claude session.
    SpawnFailed,
    /// Demo mode: no Claude process, no workflow, no skills.
    Simulated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Observation {
    NotObserved,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workflow {
    pub name: String,
    pub path: PathKind,
    /// (stage id, role id) in dependency order for the chosen path.
    pub stages: Vec<StageEntry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathKind {
    Fast,
    Balanced,
    Thorough,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageEntry {
    pub stage: String,
    pub role: String,
}

/// The standing instructions Agentree generated for the lead (`instructions.txt`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeadPolicy {
    pub artifact: String,
    pub sha256: String,
    pub bytes: u64,
    /// The project-instruction text Agentree appended to the policy, if it did. This is Agentree's
    /// prompt text; it is not Claude Code's own loading of any file.
    pub instruction_note: Option<InstructionNote>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstructionNote {
    pub kind: NoteKind,
    pub sha256: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteKind {
    /// "Claude Code is not expected to load AGENTS.md; read it unless it is already loaded."
    ReadRequestedUnlessLoaded,
    /// "Claude Code may already load AGENTS.md; read it unless it is already loaded."
    ConditionalPointer,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Instructions {
    pub resolution: Resolution,
    /// What the Claude Code user and managed settings say about loading; `None` if unresolved.
    pub settings: Option<SettingsMode>,
    /// How `AGENTS.md` reaches Claude, as the resolver decided; `None` if unresolved.
    pub agents_md: Option<AgentsDelivery>,
    pub text_comparison: Option<TextComparison>,
    /// False when the `@import` search stopped at a read limit or an unreadable file.
    pub import_search_complete: bool,
    /// A `CLAUDE.md`-family file exists in a folder above the project. Existence only.
    pub claude_file_above_project: bool,
    /// Claude Code's own loading of any file listed here is never observed.
    pub claude_ingestion: Observation,
    /// Whether the lead did the conditional read Agentree asked for is never observed.
    pub lead_read: Observation,
    pub sources: Vec<SourceEntry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    Resolved,
    /// The resolver could not run (for example the project folder was unusable).
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingsMode {
    Unset,
    ClaudeOnly,
    AgentsFallback,
    Both,
    Dropped,
    Unusable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentsDelivery {
    Absent,
    SameFile,
    ImportSyntax,
    NativeConfigured,
    Dropped,
    Excluded,
    Requested,
    Unverified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextComparison {
    Identical,
    SameLines,
    Differs,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceEntry {
    /// Relative to the project folder, `/`-separated.
    pub path: String,
    pub family: Family,
    pub file: FileState,
    /// Length and SHA-256 of the bytes Agentree read. Present only for `ok` and `invalid_utf8`.
    pub bytes: Option<u64>,
    pub sha256: Option<String>,
    /// A `claudeMdExcludes` pattern matched (for `AGENTS.md`, or could not be evaluated).
    pub excluded: bool,
    pub disposition: Disposition,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Family {
    Shared,
    Claude,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileState {
    Absent,
    Ok,
    OutsideProject,
    BrokenLink,
    NotAFile,
    Unreadable,
    InvalidUtf8,
    TooLarge,
}

/// What Agentree knows about a file's part in this run. None of these is proof of ingestion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    Absent,
    /// Found but outside the project, unreadable, invalid, too large or not a regular file.
    Unusable,
    /// Identified for provenance only; the Claude backend does not use it.
    NotUsedByBackend,
    /// Claude Code is expected to load it itself. Not observed.
    ClaudeNativeExpected,
    Dropped,
    Excluded,
    SameFileAsClaudeMd,
    /// Same text as a live `CLAUDE.md`; Agentree added no request. Text identity, not delivery.
    IdenticalToClaudeMd,
    /// A `CLAUDE.md` `@import` chain reaches it. Syntax only; ingestion not observed.
    ImportSyntaxDetected,
    /// Settings make Claude Code load it. Not confirmed against the running build.
    NativeLoadConfigured,
    /// The lead's instructions ask it to read the file unless already loaded.
    LeadReadRequested,
    Unverified,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginEntry {
    pub name: String,
    pub version: Option<String>,
    /// SHA-256 over every file of the plugin, as trusted and activated.
    pub fingerprint: String,
    pub status: PluginStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginStatus {
    /// Trusted at this fingerprint and activated through `Store::activate` just before launch.
    Activated,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillEntry {
    /// The workflow role id, or `advisor`.
    pub role: String,
    pub plugin: String,
    pub skill: String,
    pub version: Option<String>,
    pub fingerprint: String,
    /// `SKILL.md` inside the plugin, `/`-separated.
    pub source: String,
    pub audience: Audience,
    pub status: SkillStatus,
    /// Whether anyone read the skill is never observed.
    pub observed: Observation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Audience {
    /// The lead is told to read the file.
    Lead,
    /// The lead is told to put "read this file" into the named teammate's prompt. Claude Code
    /// does not load skills for named teammates.
    TeammatePrompt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillStatus {
    /// Its path and plugin identity are in the lead's instructions. Not proof of use.
    Offered,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpEntry {
    pub role: String,
    pub plugin: String,
    pub server: String,
    pub status: McpStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpStatus {
    /// The trusted plugin declares it. Agentree starts no MCP server and cannot hand it to
    /// Claude Code in this release.
    DeclaredNotLaunched,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnavailableEntry {
    pub role: String,
    pub kind: Kind,
    pub plugin: String,
    pub name: String,
    pub reason: Reason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Skill,
    Mcp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// Missing, or rejected when loaded.
    NotLoaded,
    Ambiguous,
    /// Never trusted, or trust revoked.
    Untrusted,
    /// Trusted before; its files changed or cannot be read now.
    Changed,
    NoSuchItem,
    /// Available from a trusted plugin, but not offered because the whole workflow fell back to
    /// its skill-free form. Not a statement that the skill is missing or untrusted.
    SuppressedByFallback,
}

/// A fixed statement of what this manifest cannot establish. `docs/PROVENANCE.md` explains each.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Limitation {
    ClaudeIngestionNotObserved,
    ImportIngestionNotObserved,
    LeadReadNotConfirmed,
    SkillReadNotConfirmed,
    TeammateSkillUseUnverified,
    McpServersNotLaunched,
    CommandLineSettingsNotInspected,
    NestedInstructionsNotInspected,
    UserInstructionsNotInspected,
    ManagedInstructionsNotInspected,
    FilesMayChangeAfterInspection,
    PluginMayChangeAfterActivation,
}

const LIMITATIONS: [Limitation; 12] = [
    Limitation::ClaudeIngestionNotObserved,
    Limitation::ImportIngestionNotObserved,
    Limitation::LeadReadNotConfirmed,
    Limitation::SkillReadNotConfirmed,
    Limitation::TeammateSkillUseUnverified,
    Limitation::McpServersNotLaunched,
    Limitation::CommandLineSettingsNotInspected,
    Limitation::NestedInstructionsNotInspected,
    Limitation::UserInstructionsNotInspected,
    Limitation::ManagedInstructionsNotInspected,
    Limitation::FilesMayChangeAfterInspection,
    Limitation::PluginMayChangeAfterActivation,
];

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Warning {
    pub code: WarningCode,
    /// A project-relative instruction path, when the warning is about one.
    pub subject: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarningCode {
    InstructionsUnresolved,
    SourceUnusable,
    /// `AGENTS.md` is usable but Agentree adds no request because settings drop or exclude it.
    SourceOmitted,
    ImportSearchIncomplete,
    ExclusionsUnchecked,
    SettingsUnusable,
    ClaudeFileAboveProject,
    SkillUnavailable,
    McpUnavailable,
}

/// What `build` needs, all of it already decided by the launch.
pub struct Inputs<'a> {
    pub run_id: &'a str,
    pub launch: LaunchState,
    /// `None` for a simulated run, which adapts no workflow.
    pub brief: Option<&'a Brief>,
    /// `None` when the resolver failed.
    pub instructions: Option<&'a Resolved>,
    /// The note `Resolved::lead_note` produced, exactly as appended to the policy.
    pub note: Option<&'a str>,
    /// The complete policy text, exactly as written to `instructions.txt`.
    pub policy: &'a str,
}

pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// The one workflow name Agentree ships. Any other name is free text from a workflow file.
const BUILTIN_WORKFLOW: &str = "software-engineering";
/// The export form of a value that cannot be exported as written: `sha256-` and the first 12 hex
/// digits of its SHA-256. It names the value for comparison without containing any of it.
fn opaque(s: &str) -> String {
    format!("sha256-{}", &sha256(s.as_bytes())[..12])
}

fn is_opaque(s: &str) -> bool {
    s.strip_prefix("sha256-")
        .is_some_and(|h| h.len() == 12 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

/// Fixed identifiers Agentree itself ships: the built-in plugin, its skills, roles and stages,
/// and the instruction files Agentree looks for. Only these are exported as written.
const BUILTIN: [&str; 27] = [
    "software-engineering",
    "advisor",
    "explorer",
    "researcher",
    "planner",
    "coder",
    "tester",
    "final_reviewer",
    "explore",
    "research",
    "plan",
    "implement",
    "test",
    "prepare-review",
    "risk-scan",
    "review",
    "advise",
    "skills",
    "SKILL.md",
    "AGENTS.md",
    "CLAUDE.md",
    "CLAUDE.local.md",
    ".claude",
    "GEMINI.md",
    ".github",
    "copilot-instructions.md",
    ".cursorrules",
];

/// An identifier for export: a [`BUILTIN`] name as written, everything else its [`opaque`] form,
/// however short or ordinary it looks. The same input always gives the same output, so references
/// between entries still match.
fn ident(s: &str) -> String {
    if BUILTIN.contains(&s) {
        s.into()
    } else {
        opaque(s)
    }
}

/// True when `s` is what [`ident`] could have exported: a built-in name or an opaque form.
fn ident_ok(s: &str) -> bool {
    BUILTIN.contains(&s) || is_opaque(s)
}

/// A `/`-separated relative path for export, each component through [`ident`].
fn rel_path(s: &str) -> String {
    s.split('/').map(ident).collect::<Vec<_>>().join("/")
}

/// Workflow names are free text, so only the built-in name is exported as written.
fn workflow_name(s: &str) -> String {
    if s == BUILTIN_WORKFLOW || is_opaque(s) {
        s.into()
    } else {
        opaque(s)
    }
}

/// Plugin versions are free text. Only `N`, `N.N`, `N.N.N` or `N.N.N.N` (up to nine digits each),
/// optionally followed by `-alpha`, `-beta`, `-rc`, `-pre` or `-dev` and an optional `.N`, are
/// exported as written.
fn version(s: &str) -> String {
    let (core, pre) = s.split_once('-').map_or((s, None), |(c, p)| (c, Some(p)));
    let num = |n: &str| (1..=9).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_digit());
    let core_ok = (1..=4).contains(&core.split('.').count()) && core.split('.').all(num);
    let pre_ok = pre.is_none_or(|p| {
        let (label, n) = p.split_once('.').map_or((p, None), |(l, n)| (l, Some(n)));
        matches!(label, "alpha" | "beta" | "rc" | "pre" | "dev") && n.is_none_or(num)
    });
    if (core_ok && pre_ok) || is_opaque(s) {
        s.into()
    } else {
        opaque(s)
    }
}

pub fn build(i: &Inputs) -> Manifest {
    let mut warnings = Vec::new();
    let mut warn = |code, subject: Option<String>| warnings.push(Warning { code, subject });
    let mut instructions = match i.instructions {
        Some(r) => instruction_section(r, i.note.is_some()),
        None => {
            warn(WarningCode::InstructionsUnresolved, None);
            Instructions {
                resolution: Resolution::Failed,
                settings: None,
                agents_md: None,
                text_comparison: None,
                import_search_complete: false,
                claude_file_above_project: false,
                claude_ingestion: Observation::NotObserved,
                lead_read: Observation::NotObserved,
                sources: Vec::new(),
            }
        }
    };
    if let Some(r) = i.instructions {
        for s in &instructions.sources {
            if !matches!(s.file, FileState::Absent | FileState::Ok) {
                warn(WarningCode::SourceUnusable, Some(s.path.clone()));
            }
            if s.family == Family::Shared
                && s.file == FileState::Ok
                && matches!(r.delivery, Delivery::Dropped | Delivery::Excluded)
            {
                warn(WarningCode::SourceOmitted, Some(s.path.clone()));
            }
        }
        if r.import_gap.is_some() {
            warn(WarningCode::ImportSearchIncomplete, None);
        }
        if r.exclusions_unchecked {
            warn(WarningCode::ExclusionsUnchecked, None);
        }
        if matches!(r.mode, Mode::Unreadable(_)) {
            warn(WarningCode::SettingsUnusable, None);
        }
        if r.ancestor.is_some() {
            warn(WarningCode::ClaudeFileAboveProject, None);
        }
    }
    instructions.sources.sort_by(|a, b| a.path.cmp(&b.path));

    let (mut workflow, mut plugins, mut skills, mut mcp, mut unavailable) =
        (None, Vec::new(), Vec::new(), Vec::new(), Vec::new());
    if let Some(b) = i.brief {
        workflow = Some(Workflow {
            name: workflow_name(&b.workflow),
            path: match b.decision.path {
                ExecutionPath::Fast => PathKind::Fast,
                ExecutionPath::Balanced => PathKind::Balanced,
                ExecutionPath::Thorough => PathKind::Thorough,
            },
            stages: b
                .stages
                .iter()
                .zip(&b.flow)
                .map(|((stage, _), f)| StageEntry {
                    stage: ident(stage),
                    role: ident(&f.role_id),
                })
                .collect(),
        });
        plugins = b
            .activated
            .iter()
            .map(|p| PluginEntry {
                name: ident(&p.name),
                version: p.version.as_deref().map(version),
                fingerprint: p.fingerprint.clone(),
                status: PluginStatus::Activated,
            })
            .collect();
        let offer = |role: &str, s: &crate::adapter::SkillUse, audience| SkillEntry {
            role: ident(role),
            plugin: ident(&s.plugin),
            skill: ident(&s.name),
            version: s.version.as_deref().map(version),
            fingerprint: s.fingerprint.clone(),
            source: rel_path(&s.source),
            audience,
            status: SkillStatus::Offered,
            observed: Observation::NotObserved,
        };
        for r in &b.roles {
            if let Some(s) = &r.skill {
                skills.push(offer(&r.id, s, Audience::TeammatePrompt));
            }
            for m in &r.mcp {
                let (plugin, server) = m.split_once('/').unwrap_or(("", m));
                mcp.push(McpEntry {
                    role: ident(&r.id),
                    plugin: ident(plugin),
                    server: ident(server),
                    status: McpStatus::DeclaredNotLaunched,
                });
            }
        }
        if let Some(r) = &b.review {
            if let Some(s) = &r.skill {
                skills.push(offer(&r.role, s, Audience::Lead));
            }
        }
        if let Some(s) = &b.advisor_skill {
            skills.push(offer("advisor", s, Audience::Lead));
        }
        unavailable = b
            .unavailable
            .iter()
            .map(|u| UnavailableEntry {
                role: ident(&u.role),
                kind: match u.kind {
                    ItemKind::Skill => Kind::Skill,
                    ItemKind::Mcp => Kind::Mcp,
                },
                plugin: ident(&u.plugin),
                name: ident(&u.name),
                reason: match u.why {
                    Why::NotLoaded => Reason::NotLoaded,
                    Why::Ambiguous => Reason::Ambiguous,
                    Why::Untrusted => Reason::Untrusted,
                    Why::Changed => Reason::Changed,
                    Why::NoSuchItem => Reason::NoSuchItem,
                    Why::Suppressed => Reason::SuppressedByFallback,
                },
            })
            .collect();
        for u in &unavailable {
            let code = match u.kind {
                Kind::Skill => WarningCode::SkillUnavailable,
                Kind::Mcp => WarningCode::McpUnavailable,
            };
            warn(code, None);
        }
    }
    skills.sort();
    mcp.sort();
    unavailable.sort();
    unavailable.dedup();
    plugins.sort_by(|a: &PluginEntry, b| a.name.cmp(&b.name));
    warnings.sort();
    warnings.dedup();

    Manifest {
        schema: SCHEMA.into(),
        version: VERSION,
        run_id: i.run_id.into(),
        agentree_version: version(env!("CARGO_PKG_VERSION")),
        backend: Backend::ClaudeNativeTeams,
        launch: i.launch,
        workflow,
        lead_policy: LeadPolicy {
            artifact: "instructions.txt".into(),
            sha256: sha256(i.policy.as_bytes()),
            bytes: i.policy.len() as u64,
            instruction_note: i.note.map(|n| InstructionNote {
                kind: match i.instructions.map(|r| r.delivery) {
                    Some(Delivery::Requested) => NoteKind::ReadRequestedUnlessLoaded,
                    _ => NoteKind::ConditionalPointer,
                },
                sha256: sha256(n.as_bytes()),
            }),
        },
        instructions,
        plugins,
        skills,
        mcp,
        unavailable,
        limitations: LIMITATIONS.to_vec(),
        warnings,
    }
}

fn instruction_section(r: &Resolved, noted: bool) -> Instructions {
    let rel = |p: &Path| {
        p.strip_prefix(&r.dir)
            .map(|q| {
                q.components()
                    .filter_map(|c| match c {
                        Component::Normal(n) => Some(n.to_string_lossy().into_owned()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("/")
            })
            .unwrap_or_default()
    };
    let entry = |s: &instructions::Source, family| {
        let file = match &s.state {
            State::Absent => FileState::Absent,
            State::Ok => FileState::Ok,
            State::Outside(_) => FileState::OutsideProject,
            State::Broken(_) => FileState::BrokenLink,
            State::NotFile => FileState::NotAFile,
            State::Unreadable(_) => FileState::Unreadable,
            State::InvalidUtf8 => FileState::InvalidUtf8,
            State::TooLarge => FileState::TooLarge,
        };
        let disposition = if file == FileState::Absent {
            Disposition::Absent
        } else if file != FileState::Ok {
            Disposition::Unusable
        } else if family == Family::Other {
            Disposition::NotUsedByBackend
        } else if r.mode == Mode::Dropped {
            Disposition::Dropped
        } else if s.excluded {
            Disposition::Excluded
        } else if family == Family::Claude {
            Disposition::ClaudeNativeExpected
        } else {
            match r.delivery {
                Delivery::Absent => Disposition::Unusable,
                Delivery::SameFile => Disposition::SameFileAsClaudeMd,
                Delivery::Import => Disposition::ImportSyntaxDetected,
                Delivery::Native => Disposition::NativeLoadConfigured,
                Delivery::Dropped => Disposition::Dropped,
                Delivery::Excluded => Disposition::Excluded,
                Delivery::Requested if !noted => Disposition::IdenticalToClaudeMd,
                Delivery::Requested => Disposition::LeadReadRequested,
                Delivery::Unverified => Disposition::Unverified,
            }
        };
        SourceEntry {
            path: rel_path(&rel(&s.path)),
            family,
            file,
            bytes: s.digest.as_ref().map(|d| d.1),
            sha256: s.digest.as_ref().map(|d| d.0.clone()),
            excluded: s.excluded,
            disposition,
        }
    };
    let mut sources = vec![entry(&r.shared, Family::Shared)];
    sources.extend(r.claude.iter().map(|s| entry(s, Family::Claude)));
    sources.extend(r.others.iter().map(|s| entry(s, Family::Other)));
    Instructions {
        resolution: Resolution::Resolved,
        settings: Some(match r.mode {
            Mode::Unset => SettingsMode::Unset,
            Mode::Claude => SettingsMode::ClaudeOnly,
            Mode::Fallback => SettingsMode::AgentsFallback,
            Mode::Both => SettingsMode::Both,
            Mode::Dropped => SettingsMode::Dropped,
            Mode::Unreadable(_) => SettingsMode::Unusable,
        }),
        agents_md: Some(match r.delivery {
            Delivery::Absent => AgentsDelivery::Absent,
            Delivery::SameFile => AgentsDelivery::SameFile,
            Delivery::Import => AgentsDelivery::ImportSyntax,
            Delivery::Native => AgentsDelivery::NativeConfigured,
            Delivery::Dropped => AgentsDelivery::Dropped,
            Delivery::Excluded => AgentsDelivery::Excluded,
            Delivery::Requested => AgentsDelivery::Requested,
            Delivery::Unverified => AgentsDelivery::Unverified,
        }),
        text_comparison: r.text.as_ref().map(|t| match t {
            Text::Identical => TextComparison::Identical,
            Text::SameLines => TextComparison::SameLines,
            Text::Differ { .. } => TextComparison::Differs,
        }),
        import_search_complete: r.import_gap.is_none(),
        claude_file_above_project: r.ancestor.is_some(),
        claude_ingestion: Observation::NotObserved,
        lead_read: Observation::NotObserved,
        sources,
    }
}

/// Why a manifest cannot be shown or written. Messages never repeat file content.
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// The run id is not a canonical lowercase UUID.
    BadRunId,
    /// No run folder, or no manifest in it (older run, or the write failed).
    NotFound,
    /// The run folder or manifest is a link, a junction or something that is not a plain file
    /// inside the runs folder.
    Unsafe,
    TooLarge,
    /// Not JSON, truncated, or does not match the schema. A fixed category, never parser text:
    /// serde messages quote the offending field names and values.
    Corrupt(&'static str),
    /// A different schema or a version this Agentree does not read. Holds a fixed phrase and at
    /// most a version number.
    Unsupported(String),
    /// Valid JSON of the right version that breaks a manifest rule. A fixed rule name.
    Invalid(&'static str),
    Io(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::BadRunId => f.write_str("run id must be a lowercase UUID such as 123e4567-e89b-42d3-a456-426614174000"),
            Error::NotFound => write!(f, "no {FILE} for this run (runs started before provenance, or its write failed)"),
            Error::Unsafe => write!(f, "refusing to read {FILE}: the run folder or file is a link or lies outside the runs folder"),
            Error::TooLarge => write!(f, "{FILE} is larger than {} KiB; not read", MAX_BYTES / 1024),
            Error::Corrupt(why) => write!(f, "{FILE} is corrupt or truncated ({why})"),
            Error::Unsupported(why) => write!(f, "{FILE} is not supported by this Agentree ({why})"),
            Error::Invalid(why) => write!(f, "{FILE} breaks the manifest rules ({why})"),
            Error::Io(why) => write!(f, "{FILE} could not be read or written ({why})"),
        }
    }
}

/// An OS error kind in a message: printable ASCII only, short. Never used for file content.
fn clip(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                '?'
            }
        })
        .take(80)
        .collect()
}

fn hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn rel_ok(s: &str) -> bool {
    s.len() <= MAX_TEXT && s.split('/').all(ident_ok)
}

/// The rules every manifest obeys, on write and on read. Returns the first one broken.
pub fn validate(m: &Manifest) -> Result<(), &'static str> {
    let bad = |what: &'static str| Err(what);
    if m.schema != SCHEMA || m.version != VERSION {
        return bad("schema or version");
    }
    if parse_run_id(&m.run_id).is_err() {
        return bad("run_id");
    }
    if version(&m.agentree_version) != m.agentree_version {
        return bad("agentree_version");
    }
    if m.lead_policy.artifact != "instructions.txt" || !hex64(&m.lead_policy.sha256) {
        return bad("lead_policy");
    }
    if let Some(n) = &m.lead_policy.instruction_note {
        if !hex64(&n.sha256) {
            return bad("lead_policy.instruction_note");
        }
    }
    if m.plugins.len() > MAX_ITEMS
        || m.skills.len() > MAX_ITEMS
        || m.mcp.len() > MAX_ITEMS
        || m.unavailable.len() > MAX_ITEMS
        || m.warnings.len() > MAX_ITEMS
        || m.limitations.len() > 32
        || m.instructions.sources.len() > 32
    {
        return bad("too many entries");
    }
    if let Some(w) = &m.workflow {
        if workflow_name(&w.name) != w.name
            || w.stages.len() > MAX_ITEMS
            || w.stages
                .iter()
                .any(|s| !ident_ok(&s.stage) || !ident_ok(&s.role))
        {
            return bad("workflow");
        }
    }
    let opt = |v: &Option<String>| v.as_deref().is_none_or(|v| version(v) == v);
    for p in &m.plugins {
        if !ident_ok(&p.name) || !opt(&p.version) || !hex64(&p.fingerprint) {
            return bad("plugins");
        }
    }
    for s in &m.skills {
        let known = m.plugins.iter().any(|p| {
            p.name == s.plugin && p.fingerprint == s.fingerprint && p.version == s.version
        });
        if !ident_ok(&s.role) || !ident_ok(&s.skill) || !rel_ok(&s.source) || !known {
            return bad("skills");
        }
        // A skill cannot be both offered and unavailable.
        if m.unavailable.iter().any(|u| {
            u.kind == Kind::Skill && u.role == s.role && u.plugin == s.plugin && u.name == s.skill
        }) {
            return bad("skill is both offered and unavailable");
        }
    }
    for s in &m.mcp {
        let known = m.plugins.iter().any(|p| p.name == s.plugin);
        if !ident_ok(&s.role) || !ident_ok(&s.server) || !known {
            return bad("mcp");
        }
    }
    for u in &m.unavailable {
        if !ident_ok(&u.role) || !ident_ok(&u.plugin) || !ident_ok(&u.name) {
            return bad("unavailable");
        }
    }
    for w in &m.warnings {
        if !w.subject.as_deref().is_none_or(rel_ok) {
            return bad("warnings");
        }
    }
    let ins = &m.instructions;
    let resolved = ins.resolution == Resolution::Resolved;
    if resolved == ins.sources.is_empty()
        || (!resolved && (ins.settings.is_some() || ins.agents_md.is_some()))
    {
        return bad("instructions.resolution");
    }
    for s in &ins.sources {
        // Other tools' files are located, never read, so they carry no digest.
        let read =
            s.family != Family::Other && matches!(s.file, FileState::Ok | FileState::InvalidUtf8);
        if !rel_ok(&s.path)
            || read != s.bytes.is_some()
            || read != s.sha256.is_some()
            || !s.sha256.as_deref().is_none_or(hex64)
        {
            return bad("instructions.sources");
        }
        // A missing or unusable file can never be reported as loaded, requested or imported.
        let usable = s.file == FileState::Ok;
        let claims_use = !matches!(
            s.disposition,
            Disposition::Absent | Disposition::Unusable | Disposition::NotUsedByBackend
        );
        if (s.file == FileState::Absent) != (s.disposition == Disposition::Absent)
            || (!usable
                && s.disposition != Disposition::Absent
                && s.disposition != Disposition::Unusable)
            || (claims_use && !usable)
        {
            return bad("instructions.sources state");
        }
    }
    // No process, workflow or skill can be claimed for a run that never started one.
    if matches!(m.launch, LaunchState::Simulated)
        && (m.workflow.is_some()
            || !m.plugins.is_empty()
            || !m.skills.is_empty()
            || !m.mcp.is_empty())
    {
        return bad("simulated run claims workflow, plugins or skills");
    }
    if m.limitations != LIMITATIONS {
        return bad("limitations");
    }
    Ok(())
}

fn parse_run_id(id: &str) -> Result<Uuid, Error> {
    match Uuid::parse_str(id) {
        Ok(u) if u.hyphenated().to_string() == id => Ok(u),
        _ => Err(Error::BadRunId),
    }
}

/// Pretty JSON, fixed field order, trailing newline. Same manifest, same bytes.
pub fn render(m: &Manifest) -> String {
    let mut s = serde_json::to_string_pretty(m).expect("a manifest always serializes");
    s.push('\n');
    s
}

/// Parses and validates manifest bytes. Strict: unknown fields, wrong types, other schemas or
/// versions, and broken rules are all refused with a bounded message.
pub fn parse(bytes: &[u8]) -> Result<Manifest, Error> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err(Error::TooLarge);
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|e| {
        Error::Corrupt(match e.classify() {
            serde_json::error::Category::Eof => "truncated JSON",
            serde_json::error::Category::Syntax => "not valid JSON",
            _ => "unreadable JSON",
        })
    })?;
    if value.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(Error::Unsupported("unrecognized schema".into()));
    }
    match value.get("version").and_then(Value::as_u64) {
        Some(v) if v == u64::from(VERSION) => {}
        Some(v) => {
            return Err(Error::Unsupported(format!(
                "version {v}; this Agentree reads version {VERSION}"
            )))
        }
        None => return Err(Error::Corrupt("version missing")),
    }
    let m: Manifest = serde_json::from_value(value)
        .map_err(|_| Error::Corrupt("does not match the manifest schema"))?;
    validate(&m).map_err(Error::Invalid)?;
    Ok(m)
}

fn read_file(path: &Path) -> Result<Vec<u8>, Error> {
    let mut buf = Vec::new();
    fs::File::open(path)
        .and_then(|f| f.take(MAX_BYTES + 1).read_to_end(&mut buf))
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::NotFound
            } else {
                Error::Io(clip(&e.kind().to_string()))
            }
        })?;
    Ok(buf)
}

/// Reads the manifest of run `id` under `runs`. The id must be a canonical UUID, the run folder
/// and the file must be plain (no link, no junction), and the real location of the file must be
/// exactly `<runs>/<id>/run-manifest.json`.
pub fn load(runs: &Path, id: &str) -> Result<Manifest, Error> {
    parse_run_id(id)?;
    let plain = |p: &Path, dir: bool| match fs::symlink_metadata(p) {
        Ok(m) if m.file_type().is_symlink() => Err(Error::Unsafe),
        Ok(m) if m.is_dir() == dir => Ok(()),
        Ok(_) => Err(Error::Unsafe),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::NotFound),
        Err(e) => Err(Error::Io(clip(&e.kind().to_string()))),
    };
    let dir = runs.join(id);
    let file = dir.join(FILE);
    plain(&dir, true)?;
    plain(&file, false)?;
    let real = |p: &Path| fs::canonicalize(p).map_err(|e| Error::Io(clip(&e.kind().to_string())));
    let (found, wanted) = (real(&file)?, real(runs)?.join(id).join(FILE));
    let same = if cfg!(windows) {
        found.to_string_lossy().to_lowercase() == wanted.to_string_lossy().to_lowercase()
    } else {
        found == wanted
    };
    if !same {
        return Err(Error::Unsafe);
    }
    let m = parse(&read_file(&file)?)?;
    if m.run_id != id {
        return Err(Error::Invalid("run_id does not match the folder"));
    }
    Ok(m)
}

/// The sanitized export for run `id`: the validated manifest re-emitted from the typed model.
pub fn export(runs: &Path, id: &str) -> Result<String, Error> {
    load(runs, id).map(|m| render(&m))
}

/// Validates, then writes through a temporary file and a rename so a crash or a full disk never
/// leaves half a manifest. On failure the temporary file is removed and any earlier manifest is
/// left as it was.
pub fn write(run_dir: &Path, m: &Manifest) -> Result<(), Error> {
    validate(m).map_err(Error::Invalid)?;
    let temp = run_dir.join(format!("{FILE}.tmp"));
    let put = || -> std::io::Result<()> {
        let mut f = fs::File::create(&temp)?;
        f.write_all(render(m).as_bytes())?;
        f.sync_all()?;
        drop(f);
        fs::rename(&temp, run_dir.join(FILE))
    };
    put().map_err(|e| {
        let _ = fs::remove_file(&temp);
        Error::Io(clip(&e.kind().to_string()))
    })
}

/// Records how far the launch got, by rewriting the manifest in this run folder.
pub fn mark(run_dir: &Path, launch: LaunchState) -> Result<(), Error> {
    let mut m = parse(&read_file(&run_dir.join(FILE))?)?;
    m.launch = launch;
    write(run_dir, &m)
}

#[cfg(test)]
mod tests;
