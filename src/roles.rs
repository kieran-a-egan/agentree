//! Per-role model settings: what the Roles screen saves, which choices Claude Code enforces,
//! and the launch plan they become. The screen, `roles.json` and `capability::RolePlan` all
//! read from here, so a saved choice is the one that launches.
//!
//! Only the lead session's model, effort and permission mode, and the advisor, are passed to
//! Claude Code as flags. Teammate settings reach Claude as instructions in the lead's prompt;
//! `notes` says which is which, and the Roles screen shows it before launch.

use crate::capability::{RolePlan, Teammate};
use serde::{Deserialize, Serialize};

/// Models Agentree can hand to Claude Code, as full names.
pub const MODELS: [&str; 3] = ["claude-haiku-5-5", "claude-sonnet-5-5", "claude-opus-5-5"];
/// Values for `--effort`. The empty string leaves Claude Code's default in place.
pub const EFFORTS: [&str; 6] = ["", "low", "medium", "high", "xhigh", "max"];
/// `--permission-mode` values offered. `bypassPermissions` is never offered because it skips every
/// approval prompt. `auto`, `dontAsk` and `manual` are left out until a live session shows how they
/// behave. The empty string leaves Claude Code's default in place.
pub const PERMISSIONS: [&str; 3] = ["", "plan", "acceptEdits"];
/// Concurrency limits offered for each teammate role.
const CONCURRENCY: [&str; 4] = ["1", "2", "3", "4"];
/// Values for the advisor role. `off` runs without an advisor and therefore without Opus review.
const ADVISOR_MODELS: [&str; 2] = ["off", "opus"];
/// The final review runs through the Opus advisor, so this is the only value it can take.
const REVIEWER_MODELS: [&str; 1] = ["opus"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Lead,
    Planner,
    Explorer,
    Researcher,
    Coder,
    Tester,
    Advisor,
    FinalReviewer,
}

pub const ROLES: [Role; 8] = [
    Role::Lead,
    Role::Planner,
    Role::Explorer,
    Role::Researcher,
    Role::Coder,
    Role::Tester,
    Role::Advisor,
    Role::FinalReviewer,
];

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Role::Lead => "Lead",
            Role::Planner => "Planner",
            Role::Explorer => "Explorer",
            Role::Researcher => "Researcher",
            Role::Coder => "Coder",
            Role::Tester => "Tester",
            Role::Advisor => "Advisor",
            Role::FinalReviewer => "Final reviewer",
        }
    }

    /// Stable name used in saved files. Matches the `Settings` field names.
    pub fn key(self) -> &'static str {
        match self {
            Role::Lead => "lead",
            Role::Planner => "planner",
            Role::Explorer => "explorer",
            Role::Researcher => "researcher",
            Role::Coder => "coder",
            Role::Tester => "tester",
            Role::Advisor => "advisor",
            Role::FinalReviewer => "final_reviewer",
        }
    }

    pub fn from_key(key: &str) -> Option<Role> {
        ROLES.into_iter().find(|r| r.key() == key)
    }

    /// The skill this role uses from the built-in `software-engineering` plugin
    /// (`plugins/software-engineering`). The Lead coordinates and has none. Each skill belongs to
    /// one role and can be loaded without the others.
    // ponytail: a fixed table; the workflow definition names each role's skill and the adapter
    // uses this only for the Advisor, which has no workflow stage.
    pub fn skill(self) -> Option<&'static str> {
        match self {
            Role::Lead => None,
            Role::Planner => Some("plan"),
            Role::Explorer => Some("explore"),
            Role::Researcher => Some("research"),
            Role::Coder => Some("implement"),
            Role::Tester => Some("test"),
            Role::Advisor => Some("advise"),
            Role::FinalReviewer => Some("review"),
        }
    }

    /// Named teammates that Claude spawns inside the lead's session.
    pub fn is_teammate(self) -> bool {
        matches!(
            self,
            Role::Planner | Role::Explorer | Role::Researcher | Role::Coder | Role::Tester
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Model,
    Effort,
    Concurrency,
    Permission,
}

pub const FIELDS: [Field; 4] = [
    Field::Model,
    Field::Effort,
    Field::Concurrency,
    Field::Permission,
];

impl Field {
    pub fn label(self) -> &'static str {
        match self {
            Field::Model => "model",
            Field::Effort => "effort",
            Field::Concurrency => "concurrency",
            Field::Permission => "permission mode",
        }
    }
}

/// One role's choices. Fields that do not apply to a role keep their stored value and are never read.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Setting {
    pub model: String,
    pub effort: String,
    pub concurrency: u8,
    pub permission: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub lead: Setting,
    pub planner: Setting,
    pub explorer: Setting,
    pub researcher: Setting,
    pub coder: Setting,
    pub tester: Setting,
    pub advisor: Setting,
    pub final_reviewer: Setting,
}

pub(crate) fn default_setting(role: Role) -> Setting {
    match role {
        Role::Lead => Setting {
            model: MODELS[0].into(),
            ..Default::default()
        },
        Role::Advisor => Setting {
            model: "opus".into(),
            ..Default::default()
        },
        Role::FinalReviewer => Setting {
            model: "opus".into(),
            ..Default::default()
        },
        // Teammates default to Haiku with up to three running at once. Previously Claude had no limit.
        _ => Setting {
            model: MODELS[0].into(),
            concurrency: 3,
            ..Default::default()
        },
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            lead: default_setting(Role::Lead),
            planner: default_setting(Role::Planner),
            explorer: default_setting(Role::Explorer),
            researcher: default_setting(Role::Researcher),
            coder: default_setting(Role::Coder),
            tester: default_setting(Role::Tester),
            advisor: default_setting(Role::Advisor),
            final_reviewer: default_setting(Role::FinalReviewer),
        }
    }
}

/// The values a field can take for a role, in cycle order. An empty slice means the field does
/// not apply to that role, which is how Claude Code's options are limited to the lead session.
pub fn options(role: Role, field: Field) -> &'static [&'static str] {
    match (role, field) {
        (Role::Advisor, Field::Model) => &ADVISOR_MODELS,
        (Role::FinalReviewer, Field::Model) => &REVIEWER_MODELS,
        (_, Field::Model) => &MODELS,
        (Role::Lead, Field::Effort) => &EFFORTS,
        (Role::Lead, Field::Permission) => &PERMISSIONS,
        (r, Field::Permission) if r.is_teammate() => &PERMISSIONS,
        (r, Field::Concurrency) if r.is_teammate() => &CONCURRENCY,
        _ => &[],
    }
}

/// How a setting reaches Claude: a launch flag, an instruction, or not at all.
pub fn applies(role: Role, field: Field) -> &'static str {
    if options(role, field).is_empty() {
        return "";
    }
    match (role, field) {
        (Role::Lead, Field::Model) => "enforced: --model",
        (Role::Lead, Field::Effort) => "enforced: --effort",
        (Role::Lead, Field::Permission) => "enforced: --permission-mode",
        (Role::Advisor, _) => "enforced: --advisor",
        (Role::FinalReviewer, _) => "via the Opus advisor",
        _ => "instruction to lead",
    }
}

/// Why a field is not offered for a role, for the explanation on the Roles screen.
pub fn not_applicable_reason(role: Role, field: Field) -> &'static str {
    match (role, field) {
        (_, Field::Effort) => "Claude Code sets effort per session, so only the Lead has one.",
        (_, Field::Concurrency) => "Concurrency limits apply to named teammates only.",
        (_, Field::Permission) => "The advisor and final review take no permission mode.",
        _ => "",
    }
}

pub(crate) fn raw(setting: &Setting, field: Field) -> String {
    match field {
        Field::Model => setting.model.clone(),
        Field::Effort => setting.effort.clone(),
        Field::Concurrency => setting.concurrency.to_string(),
        Field::Permission => setting.permission.clone(),
    }
}

pub(crate) fn assign(setting: &mut Setting, field: Field, value: &str) {
    match field {
        Field::Model => setting.model = value.into(),
        Field::Effort => setting.effort = value.into(),
        Field::Concurrency => setting.concurrency = value.parse().unwrap_or(0),
        Field::Permission => setting.permission = value.into(),
    }
}

fn some_if_set(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

/// Model short names, as shown on the dashboard.
pub fn model_label(model: &str) -> &'static str {
    match model {
        "claude-haiku-5-5" => "HAIKU 5.5",
        "claude-sonnet-5-5" => "SONNET 5.5",
        "claude-opus-5-5" => "OPUS 5.5",
        _ => "UNKNOWN MODEL",
    }
}

impl Settings {
    /// The stored value of one field as text, whether or not the field applies to the role.
    pub fn value(&self, role: Role, field: Field) -> String {
        raw(self.get(role), field)
    }

    pub fn get(&self, role: Role) -> &Setting {
        match role {
            Role::Lead => &self.lead,
            Role::Planner => &self.planner,
            Role::Explorer => &self.explorer,
            Role::Researcher => &self.researcher,
            Role::Coder => &self.coder,
            Role::Tester => &self.tester,
            Role::Advisor => &self.advisor,
            Role::FinalReviewer => &self.final_reviewer,
        }
    }

    pub(crate) fn get_mut(&mut self, role: Role) -> &mut Setting {
        match role {
            Role::Lead => &mut self.lead,
            Role::Planner => &mut self.planner,
            Role::Explorer => &mut self.explorer,
            Role::Researcher => &mut self.researcher,
            Role::Coder => &mut self.coder,
            Role::Tester => &mut self.tester,
            Role::Advisor => &mut self.advisor,
            Role::FinalReviewer => &mut self.final_reviewer,
        }
    }

    /// The value as the screen shows it: `n/a` when the field does not apply, `default` for an
    /// empty effort or permission mode.
    pub fn display(&self, role: Role, field: Field) -> String {
        if options(role, field).is_empty() {
            return "n/a".into();
        }
        match raw(self.get(role), field) {
            v if v.is_empty() => "default".into(),
            v => v,
        }
    }

    /// Moves a field to the next (`step` = 1) or previous (`step` = -1) offered value, wrapping.
    pub fn cycle(&mut self, role: Role, field: Field, step: isize) {
        let values = options(role, field);
        if values.is_empty() {
            return;
        }
        let current = raw(self.get(role), field);
        let index = values.iter().position(|v| *v == current).unwrap_or(0) as isize;
        let next = (index + step).rem_euclid(values.len() as isize) as usize;
        assign(self.get_mut(role), field, values[next]);
    }

    /// Restores one role to its default choices.
    pub fn reset(&mut self, role: Role) {
        *self.get_mut(role) = default_setting(role);
    }

    /// Replaces any value that is not offered for its role with the default, and says so.
    /// Saved files are hand-editable, so a stored value is never trusted as it is.
    pub fn sanitized(mut self) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        for role in ROLES {
            for field in FIELDS {
                let values = options(role, field);
                let current = raw(self.get(role), field);
                if values.is_empty() || values.contains(&current.as_str()) {
                    continue;
                }
                let fallback = raw(&default_setting(role), field);
                warnings.push(format!(
                    "{} {} '{}' is not supported; using {}.",
                    role.label(),
                    field.label(),
                    current,
                    fallback
                ));
                assign(self.get_mut(role), field, &fallback);
            }
        }
        (self, warnings)
    }

    /// The launch plan for these settings. Its flags and instructions are built from this alone.
    pub fn plan(&self) -> RolePlan {
        let advisor = self.advisor.model.as_str();
        RolePlan {
            lead_model: self.lead.model.clone(),
            // Claude Code has one model for unnamed subagents. Agentree uses the Coder's model for it.
            subagent_model: self.coder.model.clone(),
            advisor: (advisor != "off").then(|| advisor.to_string()),
            native_teams: true,
            effort: some_if_set(&self.lead.effort),
            permission_mode: some_if_set(&self.lead.permission),
            teammates: ROLES
                .iter()
                .filter(|r| r.is_teammate())
                .map(|&role| {
                    let s = self.get(role);
                    Teammate {
                        role: role.label(),
                        model: s.model.clone(),
                        max_concurrent: s.concurrency,
                        permission: some_if_set(&s.permission),
                    }
                })
                .collect(),
        }
    }

    /// Plain-language limits that apply before launch. Each one says what Claude Code cannot enforce.
    pub fn notes(&self) -> Vec<String> {
        let mut notes = Vec::new();
        if ROLES.iter().any(|r| r.is_teammate()) {
            notes.push(
                "Teammate models, concurrency limits and permission modes are instructions in \
                 the lead's prompt. Claude Code does not apply them per teammate, so Agentree \
                 cannot confirm them. At launch the workflow adapter replaces a pricier                  Explorer or Researcher model with the lowest-cost capable one, and unnamed                  subagents use that model instead of the Coder's."
                    .into(),
            );
        }
        if self.advisor.model == "off" {
            notes.push(
                "No advisor: Opus will not be consulted, so the final review can only end BLOCKED."
                    .into(),
            );
        }
        if self.lead.permission == "plan" {
            notes.push(
                "Lead permission mode 'plan' is read-only: Claude plans but does not edit files."
                    .into(),
            );
        }
        notes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_plan_matches_the_launch_agentree_has_always_used() {
        let plan = Settings::default().plan();
        assert_eq!(plan.lead_model, MODELS[0]);
        assert_eq!(plan.subagent_model, MODELS[0]);
        assert_eq!(plan.advisor.as_deref(), Some("opus"));
        assert!(plan.native_teams);
        assert_eq!(plan.effort, None);
        assert_eq!(plan.permission_mode, None);
        assert_eq!(plan.teammates.len(), 5);
    }

    #[test]
    fn options_are_limited_to_what_claude_code_can_apply() {
        assert!(options(Role::Lead, Field::Effort).len() > 1);
        assert!(options(Role::Coder, Field::Effort).is_empty());
        assert!(options(Role::Advisor, Field::Concurrency).is_empty());
        assert_eq!(options(Role::Advisor, Field::Model), &["off", "opus"]);
        assert_eq!(options(Role::FinalReviewer, Field::Model), &["opus"]);
        assert!(!options(Role::Lead, Field::Permission).contains(&"bypassPermissions"));
    }

    #[test]
    fn cycling_wraps_and_ignores_fields_that_do_not_apply() {
        let mut s = Settings::default();
        s.cycle(Role::Lead, Field::Effort, -1);
        assert_eq!(s.display(Role::Lead, Field::Effort), "max");
        s.cycle(Role::Lead, Field::Effort, 1);
        assert_eq!(s.display(Role::Lead, Field::Effort), "default");

        s.cycle(Role::Coder, Field::Effort, 1);
        assert_eq!(s.display(Role::Coder, Field::Effort), "n/a");
        assert_eq!(s.coder, default_setting(Role::Coder));

        s.cycle(Role::Advisor, Field::Model, 1);
        assert_eq!(s.plan().advisor, None);
        assert!(s.notes().iter().any(|n| n.contains("BLOCKED")));
    }

    #[test]
    fn notes_say_which_settings_are_only_instructions() {
        let mut s = Settings::default();
        let notes = s.notes();
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("instructions"));
        s.cycle(Role::Lead, Field::Permission, 1);
        assert!(s.notes().iter().any(|n| n.contains("read-only")));
    }

    #[test]
    fn unsupported_stored_values_fall_back_with_a_warning() {
        let mut s = Settings::default();
        s.lead.model = "gpt-4o".into();
        s.coder.concurrency = 9;
        let (fixed, warnings) = s.sanitized();
        assert_eq!(fixed.lead.model, MODELS[0]);
        assert_eq!(fixed.coder.concurrency, 3);
        assert_eq!(warnings.len(), 2);
        assert!(warnings[0].contains("gpt-4o"));
    }

    #[test]
    fn teammate_settings_become_plan_entries_and_the_coder_is_the_subagent_default() {
        let mut s = Settings::default();
        s.cycle(Role::Coder, Field::Model, 1);
        s.cycle(Role::Planner, Field::Permission, 1);
        let plan = s.plan();
        assert_eq!(plan.subagent_model, MODELS[1]);
        let planner = plan.teammates.iter().find(|t| t.role == "Planner").unwrap();
        assert_eq!(planner.permission.as_deref(), Some("plan"));
        assert_eq!(planner.max_concurrent, 3);
    }

    #[test]
    fn partial_files_keep_defaults_for_what_they_omit() {
        let partial: Settings =
            serde_json::from_str(r#"{"lead":{"model":"claude-opus-5-5"}}"#).unwrap();
        let (partial, warnings) = partial.sanitized();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(partial.lead.model, MODELS[2]);
        assert_eq!(partial.coder, default_setting(Role::Coder));
    }

    #[test]
    fn role_keys_match_the_saved_field_names() {
        let value = serde_json::to_value(Settings::default()).unwrap();
        for role in ROLES {
            assert!(value.get(role.key()).is_some(), "{}", role.key());
            assert_eq!(Role::from_key(role.key()), Some(role));
        }
        assert_eq!(Role::from_key("nobody"), None);
    }

    fn builtin() -> (std::path::PathBuf, crate::plugins::Plugin) {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/software-engineering");
        let plugin = crate::plugins::load(&dir, &std::env::temp_dir()).unwrap();
        (dir, plugin)
    }

    #[test]
    fn the_builtin_plugin_loads_cleanly_with_one_skill_per_role() {
        let (_, plugin) = builtin();
        assert!(plugin.issues.is_empty(), "{:?}", plugin.issues);
        assert_eq!(plugin.manifest.name, "software-engineering");
        let mut names: Vec<_> = plugin.skills.iter().map(|s| s.name.as_str()).collect();
        names.sort_unstable();
        let mut wanted: Vec<_> = ROLES.iter().filter_map(|r| r.skill()).collect();
        wanted.sort_unstable();
        assert_eq!(names, wanted);
        assert_eq!(wanted.len(), 7);
        assert_eq!(Role::Lead.skill(), None);
    }

    #[test]
    fn each_skill_documents_inputs_and_outputs() {
        let (dir, _) = builtin();
        for role in ROLES {
            let Some(skill) = role.skill() else { continue };
            let text =
                std::fs::read_to_string(dir.join("skills").join(skill).join("SKILL.md")).unwrap();
            for heading in ["## Inputs", "## Outputs", "## Limits"] {
                assert!(text.contains(heading), "{skill} lacks {heading}");
            }
            assert!(
                text.contains("(required)"),
                "{skill} names no required input"
            );
        }
    }

    #[test]
    fn a_role_selects_its_own_skill_and_only_trust_activates_it() {
        use crate::plugins::{State, Store};
        let (_, plugin) = builtin();
        let mut store = Store::default();
        assert!(store.activate(&plugin).is_none());
        store.trust(&plugin).unwrap();
        assert_eq!(store.state(&plugin), State::Trusted);
        let active = store.activate(&plugin).unwrap();
        assert!(active.mcp.is_empty());
        for role in ROLES {
            let picked: Vec<_> = active
                .skills
                .iter()
                .filter(|s| Some(s.name.as_str()) == role.skill())
                .collect();
            assert_eq!(picked.len(), role.skill().iter().count(), "{}", role.key());
        }
    }
}
