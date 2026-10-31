use super::*;
use crate::adapter::{self, Brief, Dirs, Env, Request};
use crate::capability::RolePlan;
use crate::instructions::{resolve_with, resolve_with_excludes};
use crate::plugins::{self, Store};
use crate::prompt;
use crate::roles::{Role, Settings};
use crate::workflow::contract::{self, default_workflow, PluginRef};
use crate::workflow::registry::claude_default;
use std::collections::BTreeSet;
use std::path::PathBuf;

const RUN: &str = "123e4567-e89b-42d3-a456-426614174000";
const HELLO: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
const SE: &str = "software-engineering";

struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("agentree-prov-{}", Uuid::new_v4()));
        fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
    fn file(&self, rel: &str, bytes: impl AsRef<[u8]>) {
        let p = self.0.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, bytes).unwrap();
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

/// A private copy of the bundled plugin, with its own trust file. Nothing here reads the user's
/// Agentree data or Claude Code configuration.
struct World {
    t: Tmp,
    plugin: PathBuf,
}
impl World {
    fn new() -> World {
        let t = Tmp::new();
        let plugin = t.0.join(SE);
        copy_dir(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("plugins")
                .join(SE),
            &plugin,
        );
        World { t, plugin }
    }
    fn dirs(&self) -> Dirs {
        Dirs {
            plugin_candidates: vec![self.plugin.clone()],
            plugin_data: self.t.0.join("data"),
            trust_file: self.t.0.join("plugins.json"),
        }
    }
    fn trust(&self) {
        let plugin = plugins::load(&self.plugin, &self.t.0.join("data")).unwrap();
        let mut store = Store::load(&self.t.0.join("plugins.json"));
        store.trust(&plugin).unwrap();
        store.save(&self.t.0.join("plugins.json")).unwrap();
    }
    fn revoke(&self) {
        let mut store = Store::load(&self.t.0.join("plugins.json"));
        assert!(store.revoke(&self.plugin));
        store.save(&self.t.0.join("plugins.json")).unwrap();
    }
    fn brief(&self, path: Option<contract::ExecutionPath>) -> Brief {
        adapter::prepare(&Settings::default(), None, path, &self.dirs()).unwrap()
    }
    fn fingerprint(&self) -> String {
        plugins::load(&self.plugin, &self.t.0.join("data"))
            .unwrap()
            .fingerprint
    }
}

fn project(files: &[(&str, &str)]) -> Tmp {
    let t = Tmp::new();
    for (name, text) in files {
        t.file(name, text);
    }
    t
}

/// Every manifest a test builds must also pass the rules a real write enforces.
fn manifest(brief: Option<&Brief>, r: Option<&Resolved>, launch: LaunchState) -> Manifest {
    let note = r.and_then(|r| r.lead_note());
    let m = build(&Inputs {
        run_id: RUN,
        launch,
        brief,
        instructions: r,
        note: note.as_deref(),
        policy: "the effective policy",
    });
    validate(&m).unwrap_or_else(|e| panic!("build() produced an invalid manifest: {e}"));
    m
}

fn source<'a>(m: &'a Manifest, path: &str) -> &'a SourceEntry {
    m.instructions
        .sources
        .iter()
        .find(|s| s.path == path)
        .unwrap_or_else(|| panic!("no source {path}"))
}

fn runs_with(m: &Manifest) -> Tmp {
    let t = Tmp::new();
    let dir = t.0.join(&m.run_id);
    fs::create_dir_all(&dir).unwrap();
    write(&dir, m).unwrap();
    t
}

fn link_dir(target: &Path, link: &Path) -> bool {
    if cfg!(windows) {
        std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    } else {
        #[cfg(unix)]
        return std::os::unix::fs::symlink(target, link).is_ok();
        #[allow(unreachable_code)]
        false
    }
}

// 1. ---------------------------------------------------------------------------------------

#[test]
fn a_manifest_has_a_stable_versioned_schema() {
    let w = World::new();
    w.trust();
    let proj = project(&[("AGENTS.md", "hello")]);
    let r = resolve_with(&proj.0, Mode::Unset).unwrap();
    let m = manifest(Some(&w.brief(None)), Some(&r), LaunchState::Prepared);
    validate(&m).unwrap();
    let text = render(&m);
    assert_eq!(text, render(&m), "serialization is deterministic");
    assert!(text.ends_with("}\n"));
    assert_eq!(parse(text.as_bytes()).unwrap(), m, "round trip");
    let json: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["schema"], "agentree.run-manifest");
    assert_eq!(json["version"], 1);
    assert_eq!(json["run_id"], RUN);
    assert_eq!(json["backend"], "claude_native_teams");
    // Fixed field order, top level.
    let at: Vec<usize> = [
        "schema",
        "version",
        "run_id",
        "agentree_version",
        "backend",
        "launch",
        "workflow",
        "lead_policy",
        "instructions",
        "plugins",
        "skills",
        "mcp",
        "unavailable",
        "limitations",
        "warnings",
    ]
    .iter()
    .map(|k| text.find(&format!("\n  \"{k}\":")).unwrap())
    .collect();
    assert!(at.windows(2).all(|w| w[0] < w[1]), "{at:?}");
    assert_eq!(m.lead_policy.sha256, sha256(b"the effective policy"));
    assert_eq!(m.lead_policy.bytes, 20);
    assert_eq!(m.lead_policy.artifact, "instructions.txt");
    assert_eq!(m.workflow.as_ref().unwrap().path, PathKind::Balanced);
    // A second build from the same decisions is byte-identical.
    let again = manifest(Some(&w.brief(None)), Some(&r), LaunchState::Prepared);
    assert_eq!(render(&again), text);
}

// 2, 3. ------------------------------------------------------------------------------------

#[test]
fn agents_md_alone_and_beside_claude_md() {
    // Alone, no settings: whether Claude Code loads it depends on its build.
    let p = project(&[("AGENTS.md", "hello")]);
    let r = resolve_with(&p.0, Mode::Unset).unwrap();
    let m = manifest(None, Some(&r), LaunchState::Simulated);
    assert_eq!(m.instructions.agents_md, Some(AgentsDelivery::Unverified));
    assert_eq!(source(&m, "AGENTS.md").disposition, Disposition::Unverified);
    assert_eq!(source(&m, "CLAUDE.md").disposition, Disposition::Absent);
    assert_eq!(source(&m, "CLAUDE.md").file, FileState::Absent);
    assert_eq!(
        m.lead_policy.instruction_note.as_ref().unwrap().kind,
        NoteKind::ConditionalPointer
    );

    // Beside a different CLAUDE.md: Claude Code loads CLAUDE.md; the lead is asked to read AGENTS.md.
    let p = project(&[("AGENTS.md", "hello"), ("CLAUDE.md", "other")]);
    let r = resolve_with(&p.0, Mode::Unset).unwrap();
    let m = manifest(None, Some(&r), LaunchState::Simulated);
    assert_eq!(
        source(&m, "AGENTS.md").disposition,
        Disposition::LeadReadRequested
    );
    assert_eq!(
        source(&m, "CLAUDE.md").disposition,
        Disposition::ClaudeNativeExpected
    );
    assert_eq!(
        m.instructions.text_comparison,
        Some(TextComparison::Differs)
    );
    assert_eq!(
        m.lead_policy.instruction_note.as_ref().unwrap().kind,
        NoteKind::ReadRequestedUnlessLoaded
    );
    // Neither the loading nor the lead's read is ever claimed.
    assert_eq!(m.instructions.claude_ingestion, Observation::NotObserved);
    assert_eq!(m.instructions.lead_read, Observation::NotObserved);

    // Identical text in a live CLAUDE.md: no request, and not claimed as delivered either.
    let p = project(&[("AGENTS.md", "same\n"), ("CLAUDE.md", "same\n")]);
    let r = resolve_with(&p.0, Mode::Unset).unwrap();
    let m = manifest(None, Some(&r), LaunchState::Simulated);
    assert_eq!(
        source(&m, "AGENTS.md").disposition,
        Disposition::IdenticalToClaudeMd
    );
    assert!(m.lead_policy.instruction_note.is_none());
}

#[test]
fn native_conditional_imported_excluded_dropped_and_unverified_states() {
    let both = project(&[("AGENTS.md", "a"), ("CLAUDE.md", "b")]);
    let m = |r: &Resolved| manifest(None, Some(r), LaunchState::Simulated);

    let r = resolve_with(&both.0, Mode::Both).unwrap();
    let native = m(&r);
    assert_eq!(
        native.instructions.agents_md,
        Some(AgentsDelivery::NativeConfigured)
    );
    assert_eq!(
        source(&native, "AGENTS.md").disposition,
        Disposition::NativeLoadConfigured
    );
    // Configured to load natively, yet the lead still gets a conditional pointer.
    assert_eq!(
        native.lead_policy.instruction_note.as_ref().unwrap().kind,
        NoteKind::ConditionalPointer
    );
    assert_eq!(
        native.instructions.claude_ingestion,
        Observation::NotObserved
    );

    let imp = project(&[("AGENTS.md", "a"), ("CLAUDE.md", "See @AGENTS.md\n")]);
    let r = resolve_with(&imp.0, Mode::Unset).unwrap();
    let imported = m(&r);
    assert_eq!(
        imported.instructions.agents_md,
        Some(AgentsDelivery::ImportSyntax)
    );
    assert_eq!(
        source(&imported, "AGENTS.md").disposition,
        Disposition::ImportSyntaxDetected
    );
    assert!(imported.lead_policy.instruction_note.is_none());
    assert!(imported.instructions.import_search_complete);

    let r = resolve_with_excludes(&both.0, Mode::Unset, &["**/AGENTS.md".to_string()]).unwrap();
    let excluded = m(&r);
    assert_eq!(
        source(&excluded, "AGENTS.md").disposition,
        Disposition::Excluded
    );
    assert!(source(&excluded, "AGENTS.md").excluded);
    assert!(excluded.lead_policy.instruction_note.is_none());
    assert!(
        excluded
            .warnings
            .iter()
            .any(|w| w.code == WarningCode::SourceOmitted
                && w.subject.as_deref() == Some("AGENTS.md"))
    );

    let r = resolve_with(&both.0, Mode::Dropped).unwrap();
    let dropped = m(&r);
    assert_eq!(
        source(&dropped, "AGENTS.md").disposition,
        Disposition::Dropped
    );
    assert_eq!(
        source(&dropped, "CLAUDE.md").disposition,
        Disposition::Dropped
    );
    assert!(dropped.lead_policy.instruction_note.is_none());
    assert_eq!(dropped.instructions.settings, Some(SettingsMode::Dropped));

    // A pattern Agentree cannot evaluate is "possibly excluded": no request, and a warning.
    let r = resolve_with_excludes(&both.0, Mode::Unset, &["{a,b}/x".to_string()]).unwrap();
    let unchecked = m(&r);
    assert!(unchecked
        .warnings
        .iter()
        .any(|w| w.code == WarningCode::ExclusionsUnchecked));
    assert!(unchecked.lead_policy.instruction_note.is_none());

    // An import chain that cannot be followed is unverified, not "imported".
    let big = format!("@AGENTS.md\n{}", "x".repeat(300 * 1024));
    let gap = project(&[("AGENTS.md", "a"), ("CLAUDE.md", &big)]);
    let r = resolve_with(&gap.0, Mode::Unset).unwrap();
    let gapped = m(&r);
    assert!(!gapped.instructions.import_search_complete);
    assert_ne!(
        source(&gapped, "AGENTS.md").disposition,
        Disposition::ImportSyntaxDetected
    );
    assert!(gapped
        .warnings
        .iter()
        .any(|w| w.code == WarningCode::ImportSearchIncomplete));
}

// 4, 5. ------------------------------------------------------------------------------------

#[test]
fn unreadable_invalid_oversized_and_outside_sources_are_never_used() {
    let p = project(&[("CLAUDE.md", "fine")]);
    p.file("AGENTS.md", [0xff, 0xfe, 0x00, 0x41]);
    let r = resolve_with(&p.0, Mode::Unset).unwrap();
    let m = manifest(None, Some(&r), LaunchState::Simulated);
    let a = source(&m, "AGENTS.md");
    assert_eq!(a.file, FileState::InvalidUtf8);
    assert_eq!(a.disposition, Disposition::Unusable);
    assert_eq!(
        a.sha256.as_deref(),
        Some(sha256(&[0xff, 0xfe, 0x00, 0x41]).as_str())
    );
    assert_eq!(a.bytes, Some(4));
    assert!(m.lead_policy.instruction_note.is_none());
    assert!(m.warnings.iter().any(
        |w| w.code == WarningCode::SourceUnusable && w.subject.as_deref() == Some("AGENTS.md")
    ));

    // Oversized: reported, never read, so there is no digest.
    let p = project(&[]);
    p.file("AGENTS.md", vec![b'a'; 300 * 1024]);
    let r = resolve_with(&p.0, Mode::Unset).unwrap();
    let a = manifest(None, Some(&r), LaunchState::Simulated);
    assert_eq!(source(&a, "AGENTS.md").file, FileState::TooLarge);
    assert_eq!(source(&a, "AGENTS.md").sha256, None);
    assert_eq!(source(&a, "AGENTS.md").bytes, None);

    // A directory where a file belongs.
    let p = project(&[("CLAUDE.md", "x")]);
    fs::create_dir_all(p.0.join("AGENTS.md")).unwrap();
    let r = resolve_with(&p.0, Mode::Unset).unwrap();
    let m = manifest(None, Some(&r), LaunchState::Simulated);
    assert_eq!(source(&m, "AGENTS.md").file, FileState::NotAFile);
    assert_eq!(source(&m, "AGENTS.md").disposition, Disposition::Unusable);

    // Outside the project: a symlink is reported and the target is never read.
    let outside = project(&[("AGENTS.md", "secret instructions")]);
    let p = project(&[]);
    let outside_file = outside.0.join("AGENTS.md");
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_file(&outside_file, p.0.join("AGENTS.md")).is_ok();
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&outside_file, p.0.join("AGENTS.md")).is_ok();
    if made {
        let r = resolve_with(&p.0, Mode::Unset).unwrap();
        let m = manifest(None, Some(&r), LaunchState::Simulated);
        let a = source(&m, "AGENTS.md");
        assert_eq!(a.file, FileState::OutsideProject);
        assert_eq!((a.sha256.as_deref(), a.bytes), (None, None));
        assert_eq!(a.disposition, Disposition::Unusable);
        assert!(!render(&m).contains("secret instructions"));
    } else {
        eprintln!("SKIPPED: cannot create a symlink here");
    }

    // A junction out of the project (no privilege needed on Windows) is contained too.
    let outside_claude = project(&[("CLAUDE.md", "secret claude")]);
    let p = project(&[]);
    if link_dir(&outside_claude.0, &p.0.join(".claude")) {
        let r = resolve_with(&p.0, Mode::Unset).unwrap();
        let m = manifest(None, Some(&r), LaunchState::Simulated);
        let c = source(&m, ".claude/CLAUDE.md");
        assert_eq!(c.file, FileState::OutsideProject);
        assert_eq!(c.disposition, Disposition::Unusable);
        assert_eq!((c.sha256.as_deref(), c.bytes), (None, None));
        assert!(!render(&m).contains("secret claude"));
    } else {
        eprintln!("SKIPPED: cannot create a junction or symlink here");
    }

    // Resolution failure is recorded as such, with no invented sources.
    let m = manifest(None, None, LaunchState::Simulated);
    assert_eq!(m.instructions.resolution, Resolution::Failed);
    assert!(m.instructions.sources.is_empty());
    assert!(m
        .warnings
        .iter()
        .any(|w| w.code == WarningCode::InstructionsUnresolved));
    validate(&m).unwrap();
}

#[test]
fn hashes_are_of_the_bytes_inspected_and_file_states_are_exact() {
    let p = project(&[
        ("AGENTS.md", "hello"),
        ("CLAUDE.md", "hello"),
        (".claude/CLAUDE.md", "nested claude"),
        ("GEMINI.md", "other tool"),
    ]);
    let r = resolve_with(&p.0, Mode::Unset).unwrap();
    let m = manifest(None, Some(&r), LaunchState::Simulated);
    assert_eq!(source(&m, "AGENTS.md").sha256.as_deref(), Some(HELLO));
    assert_eq!(source(&m, "CLAUDE.md").sha256.as_deref(), Some(HELLO));
    assert_eq!(source(&m, "AGENTS.md").bytes, Some(5));
    assert_eq!(
        source(&m, ".claude/CLAUDE.md").sha256.as_deref(),
        Some(sha256(b"nested claude").as_str())
    );
    assert_eq!(source(&m, "CLAUDE.local.md").file, FileState::Absent);
    assert_eq!(source(&m, "CLAUDE.local.md").sha256, None);
    assert_eq!(
        source(&m, "CLAUDE.local.md").disposition,
        Disposition::Absent
    );
    let other = source(&m, "GEMINI.md");
    assert_eq!(other.family, Family::Other);
    assert_eq!(other.disposition, Disposition::NotUsedByBackend);
    // Every known project file is listed, found or absent; nothing is inferred from existence.
    let paths: Vec<&str> = m
        .instructions
        .sources
        .iter()
        .map(|s| s.path.as_str())
        .collect();
    assert_eq!(
        paths,
        [
            ".claude/CLAUDE.md",
            "AGENTS.md",
            "CLAUDE.local.md",
            "CLAUDE.md",
            "GEMINI.md"
        ]
    );
    // Changed content, changed digest.
    fs::write(p.0.join("AGENTS.md"), "hello!").unwrap();
    let r = resolve_with(&p.0, Mode::Unset).unwrap();
    let m2 = manifest(None, Some(&r), LaunchState::Simulated);
    assert_ne!(
        source(&m2, "AGENTS.md").sha256,
        source(&m, "AGENTS.md").sha256
    );
    // Nested, user-level and managed instructions are declared as not inspected.
    for l in [
        Limitation::NestedInstructionsNotInspected,
        Limitation::UserInstructionsNotInspected,
        Limitation::ManagedInstructionsNotInspected,
    ] {
        assert!(m.limitations.contains(&l));
    }
}

// 6, 7. ------------------------------------------------------------------------------------

#[test]
fn a_trusted_plugin_is_recorded_with_its_version_and_full_fingerprint() {
    let w = World::new();
    w.trust();
    let b = w.brief(None);
    let m = manifest(Some(&b), None, LaunchState::Prepared);
    assert_eq!(m.plugins.len(), 1);
    let p = &m.plugins[0];
    assert_eq!(p.name, SE);
    assert_eq!(p.version.as_deref(), Some("1.0.0"));
    assert_eq!(p.fingerprint, w.fingerprint());
    assert_eq!(p.fingerprint.len(), 64);
    assert_eq!(p.status, PluginStatus::Activated);
    assert!(m.unavailable.is_empty());
    for s in &m.skills {
        assert_eq!(s.plugin, SE);
        assert_eq!(s.fingerprint, p.fingerprint);
        assert_eq!(s.version.as_deref(), Some("1.0.0"));
        assert_eq!(s.source, format!("skills/{}/SKILL.md", s.skill));
        assert_eq!(s.status, SkillStatus::Offered);
    }
}

#[test]
fn missing_untrusted_revoked_and_modified_plugins_are_unavailable_not_activated() {
    let check = |m: &Manifest, reason: Reason| {
        validate(m).unwrap();
        assert!(
            m.plugins.is_empty() && m.skills.is_empty() && m.mcp.is_empty(),
            "{m:?}"
        );
        assert!(!m.unavailable.is_empty());
        assert!(
            m.unavailable
                .iter()
                .all(|u| u.reason == reason && u.plugin == SE),
            "{:?}",
            m.unavailable
        );
        assert!(m
            .warnings
            .iter()
            .any(|w| w.code == WarningCode::SkillUnavailable));
        // Role skills, the review skill and the advisor skill are all accounted for.
        let roles: BTreeSet<&str> = m.unavailable.iter().map(|u| u.role.as_str()).collect();
        assert!(
            roles.contains("advisor") && roles.contains("coder"),
            "{roles:?}"
        );
    };

    // Untrusted: a valid folder nobody trusted.
    let w = World::new();
    check(
        &manifest(Some(&w.brief(None)), None, LaunchState::Prepared),
        Reason::Untrusted,
    );

    // Trusted, then revoked.
    w.trust();
    assert!(!manifest(Some(&w.brief(None)), None, LaunchState::Prepared)
        .skills
        .is_empty());
    w.revoke();
    check(
        &manifest(Some(&w.brief(None)), None, LaunchState::Prepared),
        Reason::Untrusted,
    );

    // Modified after it was trusted.
    w.trust();
    fs::write(
        w.plugin.join("skills/plan/SKILL.md"),
        "---\nname: plan\ndescription: edited.\n---\nBody\n",
    )
    .unwrap();
    check(
        &manifest(Some(&w.brief(None)), None, LaunchState::Prepared),
        Reason::Changed,
    );

    // Missing (not installed) and rejected (malformed) are both "not loaded".
    let none = World::new();
    let dirs = Dirs {
        plugin_candidates: vec![none.t.0.join("missing")],
        ..none.dirs()
    };
    let b = adapter::prepare(&Settings::default(), None, None, &dirs).unwrap();
    check(
        &manifest(Some(&b), None, LaunchState::Prepared),
        Reason::NotLoaded,
    );
    fs::write(none.plugin.join("plugin.json"), "{ not json").unwrap();
    let b = adapter::prepare(&Settings::default(), None, None, &none.dirs()).unwrap();
    check(
        &manifest(Some(&b), None, LaunchState::Prepared),
        Reason::NotLoaded,
    );
}

// 8, 9. ------------------------------------------------------------------------------------

#[test]
fn every_active_path_maps_each_skill_to_its_role() {
    let w = World::new();
    w.trust();
    for path in contract::PATHS {
        let b = w.brief(Some(path));
        let m = manifest(Some(&b), None, LaunchState::Prepared);
        validate(&m).unwrap();
        let wf = m.workflow.as_ref().unwrap();
        assert_eq!(
            wf.path,
            match path {
                contract::ExecutionPath::Fast => PathKind::Fast,
                contract::ExecutionPath::Balanced => PathKind::Balanced,
                contract::ExecutionPath::Thorough => PathKind::Thorough,
            }
        );
        assert_eq!(wf.stages.len(), b.stages.len());
        assert!(wf
            .stages
            .iter()
            .all(|s| !s.stage.is_empty() && !s.role.is_empty()));
        // One skill for each teammate role the path uses, plus the reviewer and the advisor.
        let mut want: Vec<(String, String)> = b
            .roles
            .iter()
            .map(|r| {
                (
                    r.id.clone(),
                    Role::from_key(&r.id).unwrap().skill().unwrap().into(),
                )
            })
            .collect();
        want.push((b.review.as_ref().unwrap().role.clone(), "review".into()));
        want.push(("advisor".into(), "advise".into()));
        want.sort();
        let got: Vec<(String, String)> = m
            .skills
            .iter()
            .map(|s| (s.role.clone(), s.skill.clone()))
            .collect();
        assert_eq!(got, want, "{path:?}");
        for s in &m.skills {
            let teammate = b.roles.iter().any(|r| r.id == s.role);
            assert_eq!(
                s.audience,
                if teammate {
                    Audience::TeammatePrompt
                } else {
                    Audience::Lead
                }
            );
        }
    }
}

#[test]
fn a_skill_is_offered_never_observed() {
    let w = World::new();
    w.trust();
    let m = manifest(Some(&w.brief(None)), None, LaunchState::Prepared);
    assert!(!m.skills.is_empty());
    assert!(m
        .skills
        .iter()
        .all(|s| s.status == SkillStatus::Offered && s.observed == Observation::NotObserved));
    assert!(m.limitations.contains(&Limitation::SkillReadNotConfirmed));
    assert!(m
        .limitations
        .contains(&Limitation::TeammateSkillUseUnverified));
    assert!(m
        .limitations
        .contains(&Limitation::PluginMayChangeAfterActivation));
    // The file format has no way to say "observed": a hand-edited claim is refused.
    let forged = render(&m).replace(
        "\"observed\": \"not_observed\"",
        "\"observed\": \"observed\"",
    );
    assert!(matches!(parse(forged.as_bytes()), Err(Error::Corrupt(_))));
    let forged = render(&m).replace(
        "\"claude_ingestion\": \"not_observed\"",
        "\"claude_ingestion\": \"loaded\"",
    );
    assert!(matches!(parse(forged.as_bytes()), Err(Error::Corrupt(_))));
    // The lead was really told about the skill the manifest says was offered.
    let assignments = w.brief(None).assignments();
    for s in &m.skills {
        assert!(assignments.contains(&format!("{}/{}", s.plugin, s.skill)));
        assert!(assignments.contains(&s.fingerprint[..12]));
    }
}

// 10. --------------------------------------------------------------------------------------

#[test]
fn a_declared_mcp_server_is_never_marked_launched() {
    let w = World::new();
    fs::write(
        w.plugin.join("mcp.json"),
        format!(
            r#"{{"$schema":"{}","mcpServers":{{"demo":{{"type":"stdio","command":"demo-server","args":["--token","sk-live-SECRET"],"env":{{"API_KEY":"sk-live-SECRET"}}}}}}}}"#,
            plugins::MCP_SCHEMA
        ),
    )
    .unwrap();
    let plugin = plugins::load(&w.plugin, &w.t.0.join("data")).unwrap();
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
        plugin: SE.into(),
        name: "demo".into(),
    });
    let wf = adapter::validate(def, &reg, &env).unwrap();
    let verified = BTreeSet::new();
    let settings = Settings::default();
    let b = adapter::adapt(&Request {
        workflow: &wf,
        settings: &settings,
        registry: &reg,
        verified: &verified,
        plugins: &env.plugins,
        trust: &env.trust,
        task: adapter_task(),
        preferred: None,
        context_tokens: 0,
        bulk_auth: None,
        review_auth: None,
    })
    .unwrap();
    let m = manifest(Some(&b), None, LaunchState::Prepared);
    validate(&m).unwrap();
    assert_eq!(m.mcp.len(), 1);
    assert_eq!(m.mcp[0].server, opaque("demo"));
    assert_eq!(m.mcp[0].plugin, SE);
    assert_eq!(m.mcp[0].role, "explorer");
    assert_eq!(m.mcp[0].status, McpStatus::DeclaredNotLaunched);
    assert!(m.limitations.contains(&Limitation::McpServersNotLaunched));
    let text = render(&m);
    assert!(
        !text.contains("sk-live-SECRET")
            && !text.contains("demo-server")
            && !text.contains("API_KEY")
    );
    assert!(!text.contains("\"launched\"") && !text.contains("\"started\""));
}

fn adapter_task() -> crate::workflow::route::TaskProfile {
    use crate::workflow::route::{Complexity, Risk, TaskProfile};
    TaskProfile {
        complexity: Complexity::Medium,
        risk: Risk::Medium,
        needs_research: false,
        changes_code: true,
    }
}

// 11. --------------------------------------------------------------------------------------

#[test]
fn the_export_carries_no_secrets_contents_or_personal_paths() {
    let w = World::new();
    // Secrets in a plugin's description and a skill body, in instruction files and in the workflow.
    fs::write(
        w.plugin.join("plugin.json"),
        r#"{"$schema":"https://agent-plugins.org/schemas/1.0.0/plugin.schema.json","name":"software-engineering","version":"1.0.0\u001b[31m","description":"Authorization: Bearer sk-live-DESC"}"#,
    )
    .unwrap();
    let skill = fs::read_to_string(w.plugin.join("skills/plan/SKILL.md")).unwrap();
    fs::write(
        w.plugin.join("skills/plan/SKILL.md"),
        format!("{skill}\nANTHROPIC_API_KEY=sk-live-SKILLBODY\n"),
    )
    .unwrap();
    w.trust();
    let p = project(&[
        (
            "AGENTS.md",
            "ANTHROPIC_API_KEY=sk-live-AGENTS\nsession token abc",
        ),
        ("CLAUDE.md", "Authorization: Bearer sk-live-CLAUDE"),
    ]);
    let r = resolve_with(&p.0, Mode::Unset).unwrap();
    let b = w.brief(None);
    let note = r.lead_note();
    let policy = format!(
        "objective: rotate sk-live-OBJECTIVE\n{}",
        note.clone().unwrap()
    );
    let m = build(&Inputs {
        run_id: RUN,
        launch: LaunchState::Prepared,
        brief: Some(&b),
        instructions: Some(&r),
        note: note.as_deref(),
        policy: &policy,
    });
    let runs = runs_with(&m);
    let text = export(&runs.0, RUN).unwrap();
    for banned in [
        "sk-live",
        "Bearer",
        "Authorization",
        "ANTHROPIC_API_KEY",
        "session token",
        "objective",
        "rotate",
        "\u{1b}",
    ] {
        assert!(!text.contains(banned), "{banned} leaked:\n{text}");
    }
    // No absolute or personal path of any kind: not the project, the plugin copy, the temp
    // folder, a drive letter, a backslash, nor the user's name or profile.
    let mut private = vec![
        p.0.display().to_string(),
        w.t.0.display().to_string(),
        std::env::temp_dir().display().to_string(),
        p.0.file_name().unwrap().to_string_lossy().into_owned(),
        "\\".to_string(),
        ":/".to_string(),
        ":\\".to_string(),
    ];
    for var in ["USERNAME", "USER", "USERPROFILE", "HOME"] {
        if let Some(v) = std::env::var_os(var).map(|v| v.to_string_lossy().into_owned()) {
            if v.len() > 3 {
                private.push(v.replace('\\', "/"));
                private.push(v);
            }
        }
    }
    for banned in private {
        assert!(!text.contains(&banned), "{banned} leaked:\n{text}");
    }
    // A hostile version string is exported only as an opaque fingerprint.
    assert_eq!(
        m.plugins[0].version.as_deref(),
        Some(opaque("1.0.0\u{1b}[31m").as_str())
    );
    // The shape is the allowlist: an extra field carrying a secret is refused on read.
    let tampered = text.replacen("\"schema\"", "\"api_key\": \"sk-live-X\",\n  \"schema\"", 1);
    assert!(matches!(parse(tampered.as_bytes()), Err(Error::Corrupt(_))));
    // ...and so is a control character or an absolute path smuggled into an allowed field.
    let hostile = opaque("1.0.0\u{1b}[31m");
    for bad in [
        (hostile.as_str(), "1.0.0\\u001b[31m"),
        ("\"AGENTS.md\"", "\"C:/Users/me/AGENTS.md\""),
        ("\"AGENTS.md\"", "\"/etc/passwd\""),
    ] {
        let t = text.replacen(bad.0, bad.1, 1);
        assert!(parse(t.as_bytes()).is_err(), "{}", bad.1);
    }
    // Error text never echoes arbitrary input.
    let e = parse(b"{\"schema\": \"agentree.run-manifest\", \"version\": 1, \"\x1b[31mevil\": 1}")
        .unwrap_err();
    assert!(!e.to_string().contains('\u{1b}'));
}

// 12. --------------------------------------------------------------------------------------

const SECRETS: [&str; 4] = [
    "sk-live-WFNAME-0123456789abcdef",
    "ghp_VERSIONSECRET0123456789abcdefghij",
    "Bearer-SCHEMASECRET",
    "AKIAFIELDNAME0123456789ABCDEF",
];

#[test]
fn credentials_in_workflow_names_and_plugin_versions_are_not_exported() {
    let w = World::new();
    fs::write(
        w.plugin.join("plugin.json"),
        format!(
            r#"{{"$schema":"https://agent-plugins.org/schemas/1.0.0/plugin.schema.json","name":"software-engineering","version":"1.0.0-{}"}}"#,
            SECRETS[1]
        ),
    )
    .unwrap();
    w.trust();
    let mut b = w.brief(None);
    b.workflow = SECRETS[0].into();
    let m = manifest(Some(&b), None, LaunchState::Prepared);
    let runs = runs_with(&m);
    let text = export(&runs.0, RUN).unwrap();
    for s in SECRETS {
        assert!(!text.contains(s), "{s} leaked:\n{text}");
    }
    assert_eq!(m.workflow.as_ref().unwrap().name, opaque(SECRETS[0]));
    assert_eq!(
        m.plugins[0].version.as_deref(),
        Some(opaque(&format!("1.0.0-{}", SECRETS[1])).as_str())
    );
    assert!(!m.skills.is_empty());
    assert!(m
        .skills
        .iter()
        .all(|s| s.version.as_deref() == m.plugins[0].version.as_deref()));

    // Ordinary names and versions keep their identity.
    assert_eq!(version("1.0.0"), "1.0.0");
    assert_eq!(version("2.1.0-rc.3"), "2.1.0-rc.3");
    assert_eq!(workflow_name(SE), SE);
    assert_eq!(ident("planner"), "planner");
    // Anything else is a stable fingerprint, not a copy.
    for odd in ["1.0.0-sk-live", "v1", "1.0.0+ghp_x", "latest", "", "1..0"] {
        assert!(version(odd).starts_with("sha256-"), "{odd}");
        assert_eq!(version(odd), version(odd));
    }
    assert!(ident(SECRETS[3]).starts_with("sha256-"));
    // No shape check: short, ordinary-looking and credential-shaped values are all opaque.
    for odd in [
        "AKIAIOSFODNN7EXAMPLE",
        "test-runner_2",
        "demo",
        "x",
        "sha256-0123456789ab",
    ] {
        assert_eq!(ident(odd), opaque(odd), "{odd}");
        assert_eq!(ident(odd), ident(odd));
    }

    // A manifest edited by hand to carry such text is refused, never re-exported.
    let plugin_version = m.plugins[0].version.clone().unwrap();
    for (from, to) in [
        (opaque(SECRETS[0]), SECRETS[0].to_string()),
        (plugin_version, "1.0.0-ghp_VERSIONSECRET".to_string()),
    ] {
        let t = text.replacen(&from, &to, 1);
        assert!(parse(t.as_bytes()).is_err(), "{to}");
    }
}

// 12b. -------------------------------------------------------------------------------------

#[test]
fn credential_shaped_identifiers_are_opaque_and_keep_their_references() {
    const STAGE: &str = "sk-live-9f8e7d6c";
    const ROLE: &str = "akia1234exampl";
    const PLUGIN: &str = "ghp-a1b2c3d4e5f6";
    const SKILL: &str = "tok-9z8y7x6w5v";
    const SERVER: &str = "xoxb-123456789";
    let all = [STAGE, ROLE, PLUGIN, SKILL, SERVER];

    let w = World::new();
    let manifest_path = w.plugin.join("plugin.json");
    let text = fs::read_to_string(&manifest_path)
        .unwrap()
        .replace("\"software-engineering\"", &format!("\"{PLUGIN}\""));
    fs::write(&manifest_path, text).unwrap();
    fs::rename(
        w.plugin.join("skills/explore"),
        w.plugin.join("skills").join(SKILL),
    )
    .unwrap();
    let skill_md = w.plugin.join("skills").join(SKILL).join("SKILL.md");
    let text = fs::read_to_string(&skill_md).unwrap();
    fs::write(
        &skill_md,
        text.replacen("name: explore", &format!("name: {SKILL}"), 1),
    )
    .unwrap();
    fs::write(
        w.plugin.join("mcp.json"),
        format!(
            r#"{{"$schema":"{}","mcpServers":{{"{SERVER}":{{"type":"stdio","command":"demo-server"}}}}}}"#,
            plugins::MCP_SCHEMA
        ),
    )
    .unwrap();
    w.trust();

    // A valid custom workflow: credential-shaped stage and role ids, plugin, skill and MCP server.
    let example = include_str!("../../examples/workflows/software-engineering.workflow.json");
    let mut wf: serde_json::Value = serde_json::from_str(example).unwrap();
    fn rename(v: &mut serde_json::Value, from: &str, to: &str) {
        match v {
            serde_json::Value::String(s) if s == from => *s = to.into(),
            serde_json::Value::Array(a) => a.iter_mut().for_each(|x| rename(x, from, to)),
            serde_json::Value::Object(o) => o.values_mut().for_each(|x| rename(x, from, to)),
            _ => {}
        }
    }
    rename(&mut wf, "risk-scan", STAGE);
    rename(&mut wf, "explorer", ROLE);
    rename(&mut wf, SE, PLUGIN);
    wf["name"] = SE.into(); // the workflow's own name is a separate field
    let explorer = wf["roles"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|r| r["id"] == ROLE)
        .unwrap();
    explorer["skill"]["name"] = SKILL.into();
    explorer["tools"].as_array_mut().unwrap().push("mcp".into());
    explorer["mcp"] = serde_json::json!([{ "plugin": PLUGIN, "name": SERVER }]);
    let file = w.t.0.join("custom.workflow.json");
    fs::write(&file, serde_json::to_string_pretty(&wf).unwrap()).unwrap();
    let b = adapter::prepare(
        &Settings::default(),
        Some(&file),
        Some(contract::ExecutionPath::Thorough),
        &w.dirs(),
    )
    .unwrap();
    let m = manifest(Some(&b), None, LaunchState::Prepared);
    let runs = runs_with(&m);
    let json = export(&runs.0, RUN).unwrap();
    for s in all {
        assert!(
            !json.contains(s),
            "{s} leaked:
{json}"
        );
    }

    // Each value is replaced by the same opaque form wherever it appears.
    let wfm = m.workflow.as_ref().unwrap();
    assert!(wfm.stages.iter().any(|s| s.stage == opaque(STAGE)));
    assert!(wfm.stages.iter().any(|s| s.role == opaque(ROLE)));
    assert!(wfm
        .stages
        .iter()
        .any(|s| s.stage == "review" && s.role == "final_reviewer"));
    assert_eq!(m.plugins[0].name, opaque(PLUGIN));
    let skill = m.skills.iter().find(|s| s.skill == opaque(SKILL)).unwrap();
    assert_eq!(skill.role, opaque(ROLE));
    assert_eq!(skill.plugin, m.plugins[0].name);
    assert_eq!(skill.source, format!("skills/{}/SKILL.md", opaque(SKILL)));
    assert_eq!(m.mcp.len(), 1);
    assert_eq!(
        (
            m.mcp[0].role.as_str(),
            m.mcp[0].plugin.as_str(),
            m.mcp[0].server.as_str()
        ),
        (
            opaque(ROLE).as_str(),
            m.plugins[0].name.as_str(),
            opaque(SERVER).as_str()
        )
    );
    // Fixed instruction paths and built-in names stay readable.
    assert!(wfm
        .stages
        .iter()
        .any(|s| s.stage == "plan" && s.role == "planner"));
    for p in [
        "AGENTS.md",
        "CLAUDE.md",
        ".claude/CLAUDE.md",
        ".github/copilot-instructions.md",
    ] {
        assert_eq!(rel_path(p), p);
    }

    // The same values in an imported or hand-edited manifest are refused, and the refusal does
    // not repeat them.
    for s in all {
        let from = format!("\"{}\"", opaque(s));
        assert!(json.contains(&from), "{s}");
        let t = json.replace(&from, &format!("\"{s}\""));
        let e = parse(t.as_bytes()).unwrap_err();
        let shown = format!(
            "{e}
{e:?}"
        );
        assert!(matches!(e, Error::Invalid(_)), "{shown}");
        for v in all {
            assert!(!shown.contains(v), "{v} in diagnostic: {shown}");
        }
    }
    // A short, ordinary name is not trusted either.
    let t = json.replace(&format!("\"{}\"", opaque(SERVER)), "\"demo\"");
    assert!(parse(t.as_bytes()).is_err());
    assert_eq!(parse(json.as_bytes()).unwrap(), m);
}

#[test]
fn every_built_in_role_stage_and_skill_stays_readable() {
    for with_review in [true, false] {
        let def = default_workflow(with_review);
        for id in def
            .roles
            .iter()
            .map(|r| &r.id)
            .chain(def.stages.iter().map(|s| &s.id))
        {
            assert_eq!(ident(id), *id, "{id} should be allowlisted");
        }
    }
    let skills = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("plugins")
        .join(SE)
        .join("skills");
    for e in fs::read_dir(skills).unwrap() {
        let name = e.unwrap().file_name().to_string_lossy().into_owned();
        assert_eq!(ident(&name), name, "{name} should be allowlisted");
    }
    assert_eq!(ident("advisor"), "advisor");
    assert_eq!(ident(SE), SE);
}

#[test]
fn malformed_input_is_never_reflected_in_errors() {
    let good = render(&manifest(None, None, LaunchState::Simulated));
    let leak = |what: &str, bytes: &[u8]| {
        let e = parse(bytes).unwrap_err();
        for s in format!("{e}\n{e:?}").lines() {
            for secret in SECRETS {
                assert!(!s.contains(secret), "{what}: {s}");
            }
        }
        assert!(e.to_string().len() < 160, "{what}: {e}");
    };
    // Unsupported schema, as a string and as a very long string.
    leak(
        "schema",
        good.replacen("agentree.run-manifest", SECRETS[2], 1)
            .as_bytes(),
    );
    leak(
        "long schema",
        good.replacen("agentree.run-manifest", &SECRETS[2].repeat(500), 1)
            .as_bytes(),
    );
    // Unknown field names, top-level and nested.
    leak(
        "field",
        good.replacen(
            "\"schema\"",
            &format!("\"{}\": 1,\n  \"schema\"", SECRETS[3]),
            1,
        )
        .as_bytes(),
    );
    leak(
        "nested field",
        good.replacen(
            "\"artifact\"",
            &format!("\"{}\": 1,\n    \"artifact\"", SECRETS[3]),
            1,
        )
        .as_bytes(),
    );
    // A bad enum value, a bad type and malformed JSON that contain a secret.
    leak(
        "enum value",
        good.replacen("\"simulated\"", &format!("\"{}\"", SECRETS[0]), 1)
            .as_bytes(),
    );
    leak(
        "type",
        good.replacen("\"simulated\"", &format!("[\"{}\"]", SECRETS[0]), 1)
            .as_bytes(),
    );
    leak(
        "syntax",
        format!("{{\"schema\": \"{}\" oops", SECRETS[2]).as_bytes(),
    );
    // A rule break names the rule, not the value.
    leak(
        "rule",
        good.replacen(
            "\"run_id\": \"",
            &format!("\"run_id\": \"{}", SECRETS[0]),
            1,
        )
        .as_bytes(),
    );
    // The same through the file entry point: the export fails and prints nothing of the file.
    let runs = Tmp::new();
    fs::create_dir_all(runs.0.join(RUN)).unwrap();
    fs::write(
        runs.0.join(RUN).join(FILE),
        good.replacen(
            "\"schema\"",
            &format!("\"{}\": 1,\n  \"schema\"", SECRETS[3]),
            1,
        ),
    )
    .unwrap();
    let e = export(&runs.0, RUN).unwrap_err();
    assert!(matches!(e, Error::Corrupt(_)));
    assert!(!format!("{e} {e:?}").contains(SECRETS[3]));
    // A forward version is reported by number only.
    let e = parse(
        good.replacen("\"version\": 1", "\"version\": 7", 1)
            .as_bytes(),
    )
    .unwrap_err();
    assert!(e.to_string().contains("version 7"));
}

#[test]
fn skills_omitted_by_the_fallback_are_each_accounted_for() {
    let w = World::new();
    // A valid, trusted plugin that lacks one of the skills the built-in workflow asks for.
    fs::remove_dir_all(w.plugin.join("skills/plan")).unwrap();
    w.trust();
    let b = w.brief(Some(contract::ExecutionPath::Thorough));
    // The whole workflow fell back, so no role or reviewer skill is offered. The advisor skill is
    // resolved on its own and is available, so it is still offered and its plugin activated.
    assert!(b.roles.iter().all(|r| r.skill.is_none()));
    assert!(b.review.as_ref().unwrap().skill.is_none());
    let m = manifest(Some(&b), None, LaunchState::Prepared);
    assert_eq!(m.plugins.len(), 1);
    assert_eq!(m.skills.len(), 1);
    assert_eq!(m.skills[0].role, "advisor");
    assert!(m.unavailable.iter().all(|u| u.role != "advisor"));

    let asked = default_workflow(true).roles;
    let review = b.review.as_ref().map(|r| r.role.clone());
    let mut missing = 0;
    for rd in asked.iter().filter(|r| r.skill.is_some()) {
        let on_path = b.roles.iter().any(|r| r.id == rd.id) || review.as_deref() == Some(&rd.id);
        let entries: Vec<_> = m
            .unavailable
            .iter()
            .filter(|u| u.role == rd.id && u.kind == Kind::Skill)
            .collect();
        if !on_path {
            continue;
        }
        let want = if rd.skill.as_ref().unwrap().name == "plan" {
            missing += 1;
            Reason::NoSuchItem
        } else {
            Reason::SuppressedByFallback
        };
        assert!(
            entries.len() == 1 && entries[0].reason == want && entries[0].plugin == SE,
            "{}: {entries:?}",
            rd.id
        );
        assert_eq!(entries[0].name, rd.skill.as_ref().unwrap().name);
    }
    assert_eq!(missing, 1, "the planner must be on the thorough path");
    // Suppressed skills are available, so they are never reported as untrusted or changed.
    assert!(m
        .unavailable
        .iter()
        .all(|u| matches!(u.reason, Reason::NoSuchItem | Reason::SuppressedByFallback)));
    assert!(m
        .unavailable
        .iter()
        .any(|u| u.reason == Reason::SuppressedByFallback));
}

#[test]
fn run_ids_are_validated_and_cannot_reach_other_paths() {
    let m = manifest(None, None, LaunchState::Simulated);
    let runs = runs_with(&m);
    assert!(load(&runs.0, RUN).is_ok());
    for bad in [
        "",
        ".",
        "..",
        "../x",
        "..\\x",
        "..\\..\\Windows",
        "/etc/passwd",
        "C:\\Windows",
        "C:/Windows/win.ini",
        "\\\\server\\share\\x",
        "123e4567-e89b-42d3-a456-426614174000/..",
        "123e4567-e89b-42d3-a456-426614174000/../x",
        "123e4567-e89b-42d3-a456-426614174000\\",
        "123E4567-E89B-42D3-A456-426614174000",
        "{123e4567-e89b-42d3-a456-426614174000}",
        "urn:uuid:123e4567-e89b-42d3-a456-426614174000",
        "123e4567e89b42d3a456426614174000",
        "123e4567-e89b-42d3-a456-426614174000\0",
        "123e4567-e89b-42d3-a456-426614174000 ",
        &"a".repeat(5000),
    ] {
        assert_eq!(load(&runs.0, bad).unwrap_err(), Error::BadRunId, "{bad:?}");
    }
    // A well-formed id with no run is simply not found.
    assert_eq!(
        load(&runs.0, "00000000-0000-4000-8000-000000000000").unwrap_err(),
        Error::NotFound
    );
}

#[test]
fn a_linked_run_folder_or_file_is_refused() {
    let m = manifest(None, None, LaunchState::Simulated);
    // A valid manifest somewhere else, which a link tries to present as a run.
    let elsewhere = runs_with(&m);
    let runs = Tmp::new();
    if !link_dir(&elsewhere.0.join(RUN), &runs.0.join(RUN)) {
        eprintln!("SKIPPED: cannot create a junction or symlink here");
    } else {
        assert_eq!(load(&runs.0, RUN).unwrap_err(), Error::Unsafe);
        assert_eq!(export(&runs.0, RUN).unwrap_err(), Error::Unsafe);
    }
    // The runs folder itself reached through a link is the user's own choice and still safe: the
    // run folder inside it must be a real folder.
    let real = runs_with(&m);
    let via = Tmp::new();
    if link_dir(&real.0, &via.0.join("runs")) {
        assert!(load(&via.0.join("runs"), RUN).is_ok());
    }
    // A manifest file that is a link to another file.
    let runs = Tmp::new();
    let dir = runs.0.join(RUN);
    fs::create_dir_all(&dir).unwrap();
    fs::write(runs.0.join("real.json"), render(&m)).unwrap();
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_file(runs.0.join("real.json"), dir.join(FILE)).is_ok();
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(runs.0.join("real.json"), dir.join(FILE)).is_ok();
    if made {
        assert_eq!(load(&runs.0, RUN).unwrap_err(), Error::Unsafe);
    } else {
        eprintln!("SKIPPED: cannot create a file symlink here");
    }
    // A run folder that is really a file.
    let runs = Tmp::new();
    fs::write(runs.0.join(RUN), "x").unwrap();
    assert_eq!(load(&runs.0, RUN).unwrap_err(), Error::Unsafe);
    // A run id that names another run's manifest content is refused.
    let other = "00000000-0000-4000-8000-000000000001";
    let runs = Tmp::new();
    fs::create_dir_all(runs.0.join(other)).unwrap();
    fs::write(runs.0.join(other).join(FILE), render(&m)).unwrap();
    assert!(matches!(
        load(&runs.0, other).unwrap_err(),
        Error::Invalid(_)
    ));
}

// 13. --------------------------------------------------------------------------------------

#[test]
fn truncated_corrupt_and_unsupported_manifests_are_refused_clearly() {
    let m = manifest(None, None, LaunchState::Simulated);
    let good = render(&m);
    assert!(parse(good.as_bytes()).is_ok());
    for cut in [0, 1, good.len() / 3, good.len() / 2, good.len() - 3] {
        let e = parse(&good.as_bytes()[..cut]).unwrap_err();
        assert!(matches!(e, Error::Corrupt(_)), "{cut}: {e:?}");
        assert!(e.to_string().contains("corrupt or truncated"));
    }
    assert!(matches!(
        parse(b"\xff\xfe not json"),
        Err(Error::Corrupt(_))
    ));
    assert!(matches!(parse(b"[]"), Err(Error::Unsupported(_))));
    assert!(matches!(parse(b"null"), Err(Error::Unsupported(_))));

    let newer = good.replacen("\"version\": 1", "\"version\": 2", 1);
    let e = parse(newer.as_bytes()).unwrap_err();
    assert!(matches!(e, Error::Unsupported(_)));
    assert!(e.to_string().contains("version 2"));
    let other = good.replacen("agentree.run-manifest", "somebody.else", 1);
    assert!(matches!(
        parse(other.as_bytes()),
        Err(Error::Unsupported(_))
    ));
    let nover = good.replacen("\"version\": 1,", "", 1);
    assert!(matches!(parse(nover.as_bytes()), Err(Error::Corrupt(_))));

    // Right schema and version, wrong shape or a broken rule.
    let wrong_type = good.replacen("\"launch\": \"simulated\"", "\"launch\": 7", 1);
    assert!(matches!(
        parse(wrong_type.as_bytes()),
        Err(Error::Corrupt(_))
    ));
    let bad_hash = good.replacen(&m.lead_policy.sha256, "nothex", 1);
    assert!(matches!(parse(bad_hash.as_bytes()), Err(Error::Invalid(_))));
    let fewer = good.replacen("\"claude_ingestion_not_observed\",", "", 1);
    assert!(matches!(parse(fewer.as_bytes()), Err(Error::Invalid(_))));

    // Bounded: a huge file is refused unread by the parser, and by the file reader.
    let huge = vec![b' '; MAX_BYTES as usize + 1];
    assert_eq!(parse(&huge).unwrap_err(), Error::TooLarge);
    let runs = Tmp::new();
    fs::create_dir_all(runs.0.join(RUN)).unwrap();
    fs::write(runs.0.join(RUN).join(FILE), &huge).unwrap();
    assert_eq!(load(&runs.0, RUN).unwrap_err(), Error::TooLarge);
    // Truncated on disk, through the real entry point.
    fs::write(
        runs.0.join(RUN).join(FILE),
        &good.as_bytes()[..good.len() / 2],
    )
    .unwrap();
    assert!(matches!(
        export(&runs.0, RUN).unwrap_err(),
        Error::Corrupt(_)
    ));
}

// 14. --------------------------------------------------------------------------------------

#[test]
fn writes_are_atomic_and_never_claim_success_that_did_not_happen() {
    let t = Tmp::new();
    let w = World::new();
    w.trust();
    let m = manifest(Some(&w.brief(None)), None, LaunchState::Prepared);
    write(&t.0, &m).unwrap();
    assert!(!t.0.join(format!("{FILE}.tmp")).exists());
    assert_eq!(fs::read_to_string(t.0.join(FILE)).unwrap(), render(&m));
    // Rewriting replaces it whole.
    mark(&t.0, LaunchState::Spawned).unwrap();
    assert_eq!(
        parse(&fs::read(t.0.join(FILE)).unwrap()).unwrap().launch,
        LaunchState::Spawned
    );
    mark(&t.0, LaunchState::SpawnFailed).unwrap();
    let failed = parse(&fs::read(t.0.join(FILE)).unwrap()).unwrap();
    assert_eq!(failed.launch, LaunchState::SpawnFailed);
    assert_eq!(
        failed.skills, m.skills,
        "a failed spawn rewrites only the launch state"
    );

    // An invalid manifest is never written, and an earlier one is left alone.
    let before = fs::read(t.0.join(FILE)).unwrap();
    let mut bad = m.clone();
    bad.lead_policy.sha256 = "zz".into();
    assert!(matches!(write(&t.0, &bad), Err(Error::Invalid(_))));
    assert_eq!(fs::read(t.0.join(FILE)).unwrap(), before);
    assert!(!t.0.join(format!("{FILE}.tmp")).exists());

    // A failing rename leaves no temporary file and no half manifest.
    let blocked = Tmp::new();
    fs::create_dir_all(blocked.0.join(FILE)).unwrap();
    assert!(matches!(write(&blocked.0, &m), Err(Error::Io(_))));
    assert!(!blocked.0.join(format!("{FILE}.tmp")).exists());
    assert!(blocked.0.join(FILE).is_dir());
    let missing = t.0.join("no-such-folder");
    assert!(matches!(write(&missing, &m), Err(Error::Io(_))));

    // `mark` refuses to build on a corrupt manifest and leaves it untouched.
    let corrupt = Tmp::new();
    fs::write(corrupt.0.join(FILE), "{ \"schema\": ").unwrap();
    assert!(matches!(
        mark(&corrupt.0, LaunchState::Spawned),
        Err(Error::Corrupt(_))
    ));
    assert_eq!(
        fs::read_to_string(corrupt.0.join(FILE)).unwrap(),
        "{ \"schema\": "
    );

    // No false success: a simulated run cannot carry a workflow, plugins or skills...
    let mut sim = m.clone();
    sim.launch = LaunchState::Simulated;
    assert!(validate(&sim).is_err());
    // ...a missing or unusable file cannot be "requested" or "imported"...
    let p = project(&[("AGENTS.md", "a")]);
    let r = resolve_with(&p.0, Mode::Unset).unwrap();
    let mut lie = manifest(None, Some(&r), LaunchState::Simulated);
    for s in &mut lie.instructions.sources {
        if s.path == "CLAUDE.md" {
            s.disposition = Disposition::ClaudeNativeExpected;
        }
    }
    assert!(validate(&lie).is_err());
    // ...and a skill cannot be both offered and unavailable.
    let mut both = m.clone();
    let s = both.skills[0].clone();
    both.unavailable.push(UnavailableEntry {
        role: s.role,
        kind: Kind::Skill,
        plugin: s.plugin,
        name: s.skill,
        reason: Reason::Untrusted,
    });
    assert!(validate(&both).is_err());
    // A skill whose plugin was not activated cannot appear.
    let mut orphan = m.clone();
    orphan.plugins.clear();
    assert!(validate(&orphan).is_err());
}

// 15. --------------------------------------------------------------------------------------

#[test]
fn the_launch_policy_and_existing_reports_are_unchanged_by_provenance() {
    let w = World::new();
    w.trust();
    let b = w.brief(None);
    let p = project(&[("AGENTS.md", "a"), ("CLAUDE.md", "b")]);
    let r = resolve_with(&p.0, Mode::Unset).unwrap();
    let mut policy = prompt::system_policy_with(
        Path::new("C:/scratch/attention.json"),
        RUN,
        &b.plan,
        &b.assignments(),
    );
    policy.push_str(&r.lead_note().unwrap());
    // Attention requests, review markers and the Git-write rule are all still in the prompt.
    assert!(policy.contains("attention.json"));
    for marker in [
        format!("AGENTREE_REVIEW_START:{RUN}"),
        format!("AGENTREE_REVIEW_RESULT:{RUN}:PASS"),
        format!("AGENTREE_OBJECTIVE_COMPLETE:{RUN}"),
    ] {
        assert!(policy.contains(&marker), "{marker}");
    }
    assert!(policy.contains("Git writes require explicit human authorization"));
    assert!(policy.contains("must not stage files, create commits, push, merge, rebase, switch branches or rewrite Git history"));
    assert!(policy.contains("Project instructions: shared project instructions are in"));
    // The human-readable reports keep their form.
    assert!(r
        .render()
        .contains("No instruction file is modified by Agentree."));
    assert!(b
        .report()
        .contains("Enforcement by Claude Code native teams:"));
    assert_eq!(b.plan.lead_model, RolePlan::claude_default().lead_model);
    // The manifest identifies exactly this text.
    let m = build(&Inputs {
        run_id: RUN,
        launch: LaunchState::Prepared,
        brief: Some(&b),
        instructions: Some(&r),
        note: r.lead_note().as_deref(),
        policy: &policy,
    });
    assert_eq!(m.lead_policy.sha256, sha256(policy.as_bytes()));
    assert_eq!(m.lead_policy.bytes as usize, policy.len());
    assert_eq!(
        m.lead_policy.instruction_note.unwrap().sha256,
        sha256(r.lead_note().unwrap().as_bytes())
    );
}

#[test]
fn the_instruction_files_are_only_read_never_changed() {
    let p = project(&[("AGENTS.md", "keep me"), ("CLAUDE.md", "and me")]);
    let before: Vec<_> = ["AGENTS.md", "CLAUDE.md"]
        .iter()
        .map(|n| fs::metadata(p.0.join(n)).unwrap().modified().unwrap())
        .collect();
    let r = resolve_with(&p.0, Mode::Unset).unwrap();
    let _ = manifest(None, Some(&r), LaunchState::Simulated);
    assert_eq!(
        fs::read_to_string(p.0.join("AGENTS.md")).unwrap(),
        "keep me"
    );
    assert_eq!(fs::read_to_string(p.0.join("CLAUDE.md")).unwrap(), "and me");
    let after: Vec<_> = ["AGENTS.md", "CLAUDE.md"]
        .iter()
        .map(|n| fs::metadata(p.0.join(n)).unwrap().modified().unwrap())
        .collect();
    assert_eq!(before, after);
    assert_eq!(fs::read_dir(&p.0).unwrap().count(), 2, "no file was added");
}

#[test]
fn the_documented_sample_is_a_valid_manifest() {
    let doc = include_str!("../../docs/PROVENANCE.md").replace("\r\n", "\n");
    let fence = "```";
    let json = doc.split(&format!("{fence}json\n")).nth(1).unwrap();
    let json = json.split(fence).next().unwrap();
    let m = parse(json.as_bytes()).expect("the sample in docs/PROVENANCE.md must stay valid");
    assert_eq!(m.launch, LaunchState::Spawned);
    // Re-rendering is stable, apart from the sample's compact stage objects.
    assert_eq!(parse(render(&m).as_bytes()).unwrap(), m);
}
