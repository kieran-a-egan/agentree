//! Global and per-project profiles, saved in `profiles.json`.
//!
//! A profile is a named set of per-role settings (`roles::Settings`). One is active globally and a
//! project may pin another and override single fields. The effective settings for a project are
//! resolved in this order, later steps winning: the active profile, the project's pinned profile,
//! the project's field overrides.
//!
//! Project choices live here, in the user's data folder, and not in the repository. A cloned
//! repository therefore cannot change models or permission modes without the user's say-so.
//! Repository-defined roles arrive with the v0.6 plugin trust rules.
//!
//! Credentials are only ever stored as references (`env:NAME` or `wincred:TARGET`), never as
//! values. A value that is not a valid reference is dropped on load and is never echoed.
//!
//! `roles.json` from earlier versions is migrated once into the `Migrated` profile and left in place.

use crate::roles::{self, Field, Role, Settings, FIELDS, ROLES};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

/// Bump only for a breaking change to the file format. Older readers refuse a newer file.
pub const VERSION: u32 = 1;
/// Built-in profile names, in the order they are offered.
pub const BUILT_IN: [&str; 4] = ["Budget", "Balanced", "Local", "Thorough"];
/// The profile a clean install starts on. Its settings are the launch Agentree has always used.
pub const DEFAULT_PROFILE: &str = "Balanced";
/// Where settings from a legacy `roles.json` are kept.
pub const MIGRATED: &str = "Migrated";

/// The settings a built-in profile starts from, or `None` for any other name.
pub fn built_in(name: &str) -> Option<Settings> {
    let mut s = Settings::default();
    let teammates = || ROLES.into_iter().filter(|r| r.is_teammate());
    match name {
        "Balanced" => {}
        "Budget" => {
            s.lead.effort = "low".into();
            teammates().for_each(|r| s.get_mut(r).concurrency = 1);
        }
        // Local models arrive in v0.7. Until then this is the least cloud use Claude Code allows.
        "Local" => {
            s.lead.effort = "low".into();
            s.advisor.model = "off".into();
            teammates().for_each(|r| s.get_mut(r).concurrency = 1);
        }
        "Thorough" => {
            s.lead.model = roles::MODELS[1].into();
            s.lead.effort = "high".into();
            for r in [Role::Planner, Role::Coder, Role::Tester] {
                s.get_mut(r).model = roles::MODELS[1].into();
            }
        }
        _ => return None,
    }
    Some(s)
}

/// One line saying what a built-in profile is for.
pub fn describe(name: &str) -> &'static str {
    match name {
        "Budget" => "Haiku throughout, effort low, one teammate per role.",
        "Balanced" => "The default: Haiku lead and teammates, Opus advisor and review.",
        "Local" => "Reserved for local models (v0.7). Now: Haiku only, no advisor.",
        "Thorough" => "Sonnet lead, planner, coder and tester, effort high.",
        MIGRATED => "Your settings from roles.json, as they were.",
        _ => "",
    }
}

/// A pointer to a secret held elsewhere. The secret itself is never stored by Agentree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CredentialRef(String);

/// Prefixes of well-known token formats, refused so a pasted key is not saved as a "reference".
const TOKEN_PREFIXES: [&str; 6] = ["sk-", "ghp_", "gho_", "github_pat_", "xox", "AIza"];

impl CredentialRef {
    /// Accepts `env:NAME` (an environment variable) or `wincred:TARGET` (a Windows Credential
    /// Manager entry). The error never repeats the input, which may be a secret.
    pub fn parse(text: &str) -> Result<Self, &'static str> {
        let (kind, name) = text
            .split_once(':')
            .ok_or("not a reference; use env:NAME or wincred:TARGET")?;
        if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
            return Err("the reference name is empty, too long or has control characters");
        }
        if TOKEN_PREFIXES.iter().any(|p| name.starts_with(p)) {
            return Err("this looks like a secret value, not a reference");
        }
        match kind {
            "env" => {
                let mut chars = name.chars();
                let head = chars
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
                if head && chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    Ok(Self(text.into()))
                } else {
                    Err("env: needs a variable name of letters, digits and underscores")
                }
            }
            "wincred" => Ok(Self(text.into())),
            _ => Err("not a reference; use env:NAME or wincred:TARGET"),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A named set of role settings and the credential references that go with it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub roles: Settings,
    /// Label (for example a provider name) to reference. Nothing in this version reads these.
    pub credentials: BTreeMap<String, CredentialRef>,
}

/// Field values a project sets over its profile. An absent field is inherited.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Override {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission: Option<String>,
}

impl Override {
    fn get(&self, field: Field) -> Option<String> {
        match field {
            Field::Model => self.model.clone(),
            Field::Effort => self.effort.clone(),
            Field::Concurrency => self.concurrency.map(|n| n.to_string()),
            Field::Permission => self.permission.clone(),
        }
    }

    fn set(&mut self, field: Field, value: Option<&str>) {
        let text = value.map(String::from);
        match field {
            Field::Model => self.model = text,
            Field::Effort => self.effort = text,
            Field::Concurrency => self.concurrency = value.and_then(|v| v.parse().ok()),
            Field::Permission => self.permission = text,
        }
    }

    fn is_empty(&self) -> bool {
        *self == Override::default()
    }
}

/// What one project chooses. Keyed in `Store::projects` by `project_key`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectEntry {
    /// A profile pinned for this project. `None` follows the global active profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Field overrides, keyed by `Role::key`.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub roles: BTreeMap<String, Override>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Store {
    pub version: u32,
    pub active: String,
    pub profiles: BTreeMap<String, Profile>,
    pub projects: BTreeMap<String, ProjectEntry>,
}

impl Default for Store {
    fn default() -> Self {
        let profiles = BUILT_IN
            .iter()
            .map(|n| {
                let roles = built_in(n).unwrap_or_default();
                (
                    n.to_string(),
                    Profile {
                        roles,
                        ..Default::default()
                    },
                )
            })
            .collect();
        Self {
            version: VERSION,
            active: DEFAULT_PROFILE.into(),
            profiles,
            projects: BTreeMap::new(),
        }
    }
}

/// A stable key for a project folder: resolved when it exists, lower case, forward slashes.
pub fn project_key(path: &Path) -> String {
    let resolved = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let text = resolved.display().to_string();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    text.replace('\\', "/").trim_end_matches('/').to_lowercase()
}

fn supported(role: Role, field: Field, value: &str) -> bool {
    roles::options(role, field).contains(&value)
}

impl Store {
    /// Built-in names first, in their own order, then any others alphabetically.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = BUILT_IN
            .iter()
            .filter(|n| self.profiles.contains_key(**n))
            .map(|n| n.to_string())
            .collect();
        names.extend(
            self.profiles
                .keys()
                .filter(|n| !BUILT_IN.contains(&n.as_str()))
                .cloned(),
        );
        names
    }

    /// The profile in force for a project: its pinned one, or the global active one.
    pub fn profile_name(&self, project: &str) -> &str {
        self.projects
            .get(project)
            .and_then(|e| e.profile.as_deref())
            .unwrap_or(&self.active)
    }

    pub fn pinned(&self, project: &str) -> bool {
        self.projects
            .get(project)
            .is_some_and(|e| e.profile.is_some())
    }

    /// The globally active profile's settings, without any project choices.
    pub fn active_settings(&self) -> Settings {
        self.profiles
            .get(&self.active)
            .map(|p| p.roles.clone())
            .unwrap_or_default()
    }

    /// Whether the project overrides any field of this role.
    pub fn has_overrides(&self, project: &str, role: Role) -> bool {
        self.projects
            .get(project)
            .is_some_and(|e| e.roles.get(role.key()).is_some_and(|o| !o.is_empty()))
    }

    /// The settings a run for this project uses.
    pub fn effective(&self, project: &str) -> Settings {
        let mut settings = self
            .profiles
            .get(self.profile_name(project))
            .map(|p| p.roles.clone())
            .unwrap_or_default();
        let Some(entry) = self.projects.get(project) else {
            return settings;
        };
        for (key, over) in &entry.roles {
            let Some(role) = Role::from_key(key) else {
                continue;
            };
            for field in FIELDS {
                if let Some(value) = over.get(field) {
                    roles::assign(settings.get_mut(role), field, &value);
                }
            }
        }
        settings
    }

    /// Makes another profile the global one, `step` places along `names`.
    pub fn cycle_active(&mut self, step: isize) {
        let names = self.names();
        let at = names.iter().position(|n| *n == self.active).unwrap_or(0) as isize;
        self.active = names[(at + step).rem_euclid(names.len() as isize) as usize].clone();
    }

    /// Cycles a project's pinned profile through "follow global" and every profile.
    pub fn cycle_project_profile(&mut self, project: &str, step: isize) {
        let mut choices = vec![None];
        choices.extend(self.names().into_iter().map(Some));
        let current = self.projects.get(project).and_then(|e| e.profile.clone());
        let at = choices.iter().position(|c| *c == current).unwrap_or(0) as isize;
        let next = choices[(at + step).rem_euclid(choices.len() as isize) as usize].clone();
        self.projects.entry(project.into()).or_default().profile = next;
        self.prune(project);
    }

    /// Writes one field of the globally active profile.
    pub fn set_profile_value(
        &mut self,
        role: Role,
        field: Field,
        value: &str,
    ) -> Result<(), String> {
        if !supported(role, field, value) {
            return Err(format!(
                "{} {} does not take that value.",
                role.label(),
                field.label()
            ));
        }
        let active = self.active.clone();
        let profile = self.profiles.entry(active).or_default();
        roles::assign(profile.roles.get_mut(role), field, value);
        Ok(())
    }

    /// Restores one role of the active profile to its built-in values.
    pub fn reset_profile_role(&mut self, role: Role) {
        let fresh = built_in(&self.active).unwrap_or_default();
        let profile = self.profiles.entry(self.active.clone()).or_default();
        *profile.roles.get_mut(role) = fresh.get(role).clone();
    }

    /// Sets one field for a project, over whatever its profile says.
    pub fn set_override(
        &mut self,
        project: &str,
        role: Role,
        field: Field,
        value: &str,
    ) -> Result<(), String> {
        if project.is_empty() {
            return Err("Choose a project folder first.".into());
        }
        if !supported(role, field, value) {
            return Err(format!(
                "{} {} does not take that value.",
                role.label(),
                field.label()
            ));
        }
        let entry = self.projects.entry(project.into()).or_default();
        entry
            .roles
            .entry(role.key().into())
            .or_default()
            .set(field, Some(value));
        Ok(())
    }

    /// Removes a project's overrides for one role.
    pub fn clear_overrides(&mut self, project: &str, role: Role) {
        if let Some(entry) = self.projects.get_mut(project) {
            entry.roles.remove(role.key());
        }
        self.prune(project);
    }

    fn prune(&mut self, project: &str) {
        let empty = self
            .projects
            .get(project)
            .is_some_and(|e| e.profile.is_none() && e.roles.is_empty());
        if empty {
            self.projects.remove(project);
        }
    }

    /// Stores a credential reference for a profile. A value that is not a reference is refused.
    #[allow(dead_code)] // No runtime consumer until backends that need credentials arrive in v0.7.
    pub fn set_credential(
        &mut self,
        profile: &str,
        label: &str,
        reference: &str,
    ) -> Result<(), String> {
        let reference = CredentialRef::parse(reference).map_err(String::from)?;
        let profile = self
            .profiles
            .get_mut(profile)
            .ok_or_else(|| format!("No profile named {profile}."))?;
        profile.credentials.insert(label.into(), reference);
        Ok(())
    }

    /// Repairs anything a hand edit or an older build left unsupported, and says what changed.
    /// Credential warnings name the label only, never the value.
    pub fn sanitized(mut self) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        for name in BUILT_IN {
            self.profiles.entry(name.into()).or_insert_with(|| Profile {
                roles: built_in(name).unwrap_or_default(),
                ..Default::default()
            });
        }
        if !self.profiles.contains_key(&self.active) {
            warnings.push(format!(
                "Active profile '{}' does not exist; using {DEFAULT_PROFILE}.",
                self.active
            ));
            self.active = DEFAULT_PROFILE.into();
        }
        for (name, profile) in std::mem::take(&mut self.profiles) {
            let (fixed, notes) = profile.roles.sanitized();
            warnings.extend(notes.into_iter().map(|n| format!("Profile {name}: {n}")));
            // The value is dropped and not named, since a rejected value may be a pasted secret.
            let credentials = profile
                .credentials
                .into_iter()
                .filter(|(label, value)| {
                    let valid = CredentialRef::parse(value.as_str()).is_ok();
                    if !valid {
                        warnings.push(format!(
                            "Profile {name}: credential '{label}' is not a reference (env:NAME or wincred:TARGET) and was removed."
                        ));
                    }
                    valid
                })
                .collect();
            self.profiles.insert(
                name,
                Profile {
                    roles: fixed,
                    credentials,
                },
            );
        }
        self.projects = std::mem::take(&mut self.projects)
            .into_iter()
            .filter_map(|(project, entry)| self.repair_project(project, entry, &mut warnings))
            .collect();
        self.version = VERSION;
        (self, warnings)
    }

    fn repair_project(
        &self,
        project: String,
        mut entry: ProjectEntry,
        warnings: &mut Vec<String>,
    ) -> Option<(String, ProjectEntry)> {
        if let Some(name) = &entry.profile {
            if !self.profiles.contains_key(name) {
                warnings.push(format!(
                    "Project {project} pins profile '{name}', which does not exist; using the active profile."
                ));
                entry.profile = None;
            }
        }
        entry.roles = std::mem::take(&mut entry.roles)
            .into_iter()
            .filter_map(|(key, mut over)| {
                let Some(role) = Role::from_key(&key) else {
                    warnings.push(format!(
                        "Project {project}: '{key}' is not a role; ignored."
                    ));
                    return None;
                };
                for field in FIELDS {
                    let Some(value) = over.get(field) else {
                        continue;
                    };
                    if !supported(role, field, &value) {
                        warnings.push(format!(
                            "Project {project}: {} {} '{value}' is not supported; ignored.",
                            role.label(),
                            field.label()
                        ));
                        over.set(field, None);
                    }
                }
                (!over.is_empty()).then_some((key, over))
            })
            .collect();
        (entry.profile.is_some() || !entry.roles.is_empty()).then_some((project, entry))
    }

    /// Writes the file through a temporary one, so a crash cannot leave half a file behind.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        let temp = path.with_extension("json.tmp");
        fs::write(&temp, json)?;
        fs::rename(&temp, path)
    }
}

/// Moves an unusable file aside so the next save cannot destroy it.
fn set_aside(path: &Path) -> PathBuf {
    let mut aside = path.with_extension("json.invalid");
    for n in 1..100 {
        if !aside.exists() {
            break;
        }
        aside = path.with_extension(format!("json.invalid.{n}"));
    }
    let _ = fs::rename(path, &aside);
    aside
}

/// Loads `profiles.json`, migrating `legacy_roles` (the old `roles.json`) the first time.
///
/// A file that cannot be used is moved to `profiles.json.invalid` and the built-in profiles are
/// used, so nothing the user wrote is overwritten. `legacy_roles` is never modified.
pub fn load(path: &Path, legacy_roles: &Path) -> (Store, Vec<String>) {
    match fs::read_to_string(path) {
        Ok(text) => match parse(&text) {
            Ok(store) => store.sanitized(),
            Err(why) => {
                let aside = set_aside(path);
                (
                    Store::default(),
                    vec![format!(
                        "profiles.json could not be used ({why}); using the built-in profiles. \
                         The file was kept as {}.",
                        aside.display()
                    )],
                )
            }
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => migrate(path, legacy_roles),
        Err(error) => (
            Store::default(),
            vec![format!(
                "profiles.json could not be read ({error}); using the built-in profiles."
            )],
        ),
    }
}

/// Describes a parse failure without repeating file content. serde's data errors quote the
/// offending value, which in this file could be a pasted secret, so only the position is kept.
fn describe_error(error: &serde_json::Error) -> String {
    match error.classify() {
        serde_json::error::Category::Data => format!(
            "a value has the wrong type at line {} column {}",
            error.line(),
            error.column()
        ),
        _ => error.to_string(),
    }
}

fn parse(text: &str) -> Result<Store, String> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|e| describe_error(&e))?;
    let version = value
        .get("version")
        .and_then(|v| v.as_u64())
        .unwrap_or(VERSION as u64);
    if version > VERSION as u64 {
        return Err(format!(
            "it was written by a newer Agentree (format {version}; this one reads {VERSION})"
        ));
    }
    serde_json::from_str(text).map_err(|e| describe_error(&e))
}

/// First run on this version: carry the old `roles.json` settings into the `Migrated` profile.
fn migrate(path: &Path, legacy_roles: &Path) -> (Store, Vec<String>) {
    let text = match fs::read_to_string(legacy_roles) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return (Store::default(), vec![]);
        }
        Err(error) => {
            return (
                Store::default(),
                vec![format!(
                    "roles.json could not be read ({error}); using the built-in profiles. It was left untouched."
                )],
            );
        }
    };
    let legacy: Settings = match serde_json::from_str(&text) {
        Ok(settings) => settings,
        Err(error) => {
            return (
                Store::default(),
                vec![format!(
                    "roles.json could not be read ({}); using the built-in profiles. It was left untouched.",
                    describe_error(&error)
                )],
            );
        }
    };
    let mut store = Store::default();
    store.profiles.insert(
        MIGRATED.into(),
        Profile {
            roles: legacy,
            ..Default::default()
        },
    );
    store.active = MIGRATED.into();
    let (store, mut warnings) = store.sanitized();
    warnings.push(match store.save(path) {
        Ok(()) => "Your role settings from roles.json are now the Migrated profile. roles.json was left in place."
            .to_string(),
        Err(error) => format!("Role settings were migrated for this session but not saved: {error}"),
    });
    (store, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agentree-profiles-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn built_ins_are_valid_and_balanced_is_todays_launch() {
        let (store, warnings) = Store::default().sanitized();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(store.names(), BUILT_IN);
        assert_eq!(built_in(DEFAULT_PROFILE), Some(Settings::default()));
        assert_eq!(store.active_settings(), Settings::default());
        for name in BUILT_IN {
            assert!(!describe(name).is_empty());
            let (_, notes) = built_in(name).unwrap().sanitized();
            assert!(notes.is_empty(), "{name}: {notes:?}");
        }
        assert_ne!(built_in("Thorough"), built_in("Budget"));
        assert_eq!(built_in("Local").unwrap().plan().advisor, None);
    }

    #[test]
    fn project_overrides_take_precedence_over_profile_and_pin() {
        let mut store = Store::default();
        store
            .set_profile_value(Role::Coder, Field::Model, roles::MODELS[1])
            .unwrap();
        assert_eq!(store.effective("a").coder.model, roles::MODELS[1]);

        // Pinning a profile replaces the global one for that project only.
        store.projects.entry("a".into()).or_default().profile = Some("Thorough".into());
        assert_eq!(store.effective("a").lead.effort, "high");
        assert_eq!(store.effective("b").lead.effort, "");

        // A field override wins over the pinned profile; other fields stay inherited.
        store
            .set_override("a", Role::Lead, Field::Effort, "max")
            .unwrap();
        let eff = store.effective("a");
        assert_eq!(eff.lead.effort, "max");
        assert_eq!(eff.lead.model, roles::MODELS[1]);
        assert!(store.has_overrides("a", Role::Lead));
        assert!(!store.has_overrides("a", Role::Coder));

        store.clear_overrides("a", Role::Lead);
        assert_eq!(store.effective("a").lead.effort, "high");
    }

    #[test]
    fn profiles_and_project_choices_survive_a_restart() {
        let dir = temp();
        let (path, legacy) = (dir.join("profiles.json"), dir.join("roles.json"));
        let mut store = Store::default();
        store.cycle_active(1); // Balanced -> Local
        store
            .set_profile_value(Role::Lead, Field::Effort, "medium")
            .unwrap();
        store
            .set_override("c:/repo", Role::Tester, Field::Concurrency, "2")
            .unwrap();
        store.cycle_project_profile("c:/repo", 1);
        store.save(&path).unwrap();
        assert!(!path.with_extension("json.tmp").exists());

        let (back, warnings) = load(&path, &legacy);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(back, store);
        assert_eq!(back.active, "Local");
        assert_eq!(back.effective("c:/repo").tester.concurrency, 2);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn project_with_no_choices_is_not_kept() {
        let mut store = Store::default();
        store.cycle_project_profile("p", 1);
        assert!(store.pinned("p"));
        store.cycle_project_profile("p", -1);
        assert!(store.projects.is_empty());
        store
            .set_override("p", Role::Lead, Field::Effort, "low")
            .unwrap();
        store.clear_overrides("p", Role::Lead);
        assert!(store.projects.is_empty());
    }

    #[test]
    fn unsupported_values_are_refused_when_set_and_repaired_when_loaded() {
        let mut store = Store::default();
        assert!(store
            .set_override("p", Role::Lead, Field::Model, "gpt-4o")
            .is_err());
        assert!(store
            .set_override("p", Role::Coder, Field::Effort, "low")
            .is_err());
        assert!(store
            .set_override("", Role::Lead, Field::Effort, "low")
            .is_err());
        assert!(store.projects.is_empty());

        let text = r#"{
            "active": "Nope",
            "profiles": {"Budget": {"roles": {"lead": {"model": "gpt-4o"}}}},
            "projects": {"p": {"profile": "Gone", "roles": {
                "coder": {"model": "gpt-4o", "concurrency": 2},
                "wizard": {"model": "claude-opus-5-5"}}}}
        }"#;
        let (store, warnings) = parse(text).unwrap().sanitized();
        assert_eq!(store.active, DEFAULT_PROFILE);
        assert_eq!(store.profiles["Budget"].roles.lead.model, roles::MODELS[0]);
        let entry = &store.projects["p"];
        assert_eq!(entry.profile, None);
        assert_eq!(entry.roles["coder"].model, None);
        assert_eq!(entry.roles["coder"].concurrency, Some(2));
        assert!(!entry.roles.contains_key("wizard"));
        assert_eq!(warnings.len(), 5, "{warnings:?}");
        // Built-ins omitted from the file are restored.
        assert_eq!(store.names(), BUILT_IN);
    }

    #[test]
    fn only_references_are_accepted_as_credentials() {
        for ok in [
            "env:ANTHROPIC_API_KEY",
            "env:_x1",
            "wincred:Agentree/anthropic",
        ] {
            assert!(CredentialRef::parse(ok).is_ok(), "{ok}");
        }
        for bad in [
            "sk-ant-api03-abcdef",
            "env:sk-ant-api03-abcdef",
            "env:has space",
            "env:1ABC",
            "wincred:ghp_abcdef",
            "env:",
            "http://example.com",
            "plaintext",
        ] {
            let error = CredentialRef::parse(bad).unwrap_err();
            assert!(!error.contains(bad), "error repeats the input: {error}");
        }
        let mut store = Store::default();
        assert!(store
            .set_credential("Budget", "anthropic", "env:ANTHROPIC_API_KEY")
            .is_ok());
        assert!(store
            .set_credential("Budget", "anthropic", "sk-live-123")
            .is_err());
        assert!(store
            .set_credential("Missing", "anthropic", "env:KEY")
            .is_err());
    }

    #[test]
    fn plaintext_credentials_in_a_file_are_dropped_and_never_echoed_or_rewritten() {
        let dir = temp();
        let (path, legacy) = (dir.join("profiles.json"), dir.join("roles.json"));
        let secret = "sk-ant-SECRET-1234567890";
        let ok = "env:ANTHROPIC_API_KEY";
        fs::write(
            &path,
            format!(
                r#"{{"profiles": {{"Budget": {{"credentials": {{"bad": "{secret}", "good": "{ok}"}}}}}}}}"#
            ),
        )
        .unwrap();
        let (store, warnings) = load(&path, &legacy);
        assert_eq!(store.profiles["Budget"].credentials.len(), 1);
        assert_eq!(store.profiles["Budget"].credentials["good"].as_str(), ok);
        assert!(warnings.iter().any(|w| w.contains("bad")), "{warnings:?}");
        assert!(warnings.iter().all(|w| !w.contains(secret)), "{warnings:?}");
        store.save(&path).unwrap();
        assert!(!fs::read_to_string(&path).unwrap().contains(secret));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_wrongly_typed_secret_is_not_quoted_in_the_warning() {
        let dir = temp();
        let (path, legacy) = (dir.join("profiles.json"), dir.join("roles.json"));
        let secret = "sk-ant-SECRET-1234567890";
        for body in [
            format!(r#"{{"profiles": {{"Budget": {{"credentials": "{secret}"}}}}}}"#),
            format!(r#"{{"active": 1, "projects": "{secret}"}}"#),
        ] {
            fs::write(&path, body).unwrap();
            let (store, warnings) = load(&path, &legacy);
            assert_eq!(store, Store::default());
            assert!(warnings[0].contains("wrong type at line"), "{warnings:?}");
            assert!(warnings.iter().all(|w| !w.contains(secret)), "{warnings:?}");
            let aside = path.with_extension("json.invalid");
            assert!(fs::read_to_string(&aside).unwrap().contains(secret));
            fs::remove_file(aside).unwrap();
        }
        // A second bad file does not overwrite an earlier one that was set aside.
        fs::write(&path, "{ one").unwrap();
        load(&path, &legacy);
        fs::write(&path, "{ two").unwrap();
        load(&path, &legacy);
        let first = path.with_extension("json.invalid");
        assert_eq!(fs::read_to_string(first).unwrap(), "{ one");
        let second = path.with_extension("json.invalid.1");
        assert_eq!(fs::read_to_string(second).unwrap(), "{ two");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn legacy_roles_json_migrates_once_without_being_touched() {
        let dir = temp();
        let (path, legacy) = (dir.join("profiles.json"), dir.join("roles.json"));
        let old = r#"{"lead":{"model":"claude-opus-5-5","effort":"high"},"coder":{"model":"claude-sonnet-5-5","concurrency":2}}"#;
        fs::write(&legacy, old).unwrap();

        let (store, warnings) = load(&path, &legacy);
        assert_eq!(store.active, MIGRATED);
        assert_eq!(store.effective("").lead.model, roles::MODELS[2]);
        assert_eq!(store.effective("").coder.concurrency, 2);
        // Unmentioned roles keep today's defaults, and the built-ins are still there.
        assert_eq!(store.effective("").planner, Settings::default().planner);
        assert!(store.profiles.contains_key("Budget"));
        assert!(
            warnings.iter().any(|w| w.contains("Migrated")),
            "{warnings:?}"
        );
        assert_eq!(fs::read_to_string(&legacy).unwrap(), old);
        assert!(path.exists());

        // Second start: loaded from profiles.json, no migration notice, even if roles.json changed.
        fs::write(&legacy, "{}").unwrap();
        let (again, warnings) = load(&path, &legacy);
        assert_eq!(again, store);
        assert!(warnings.is_empty(), "{warnings:?}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn migration_repairs_unsupported_old_values_and_survives_a_corrupt_file() {
        let dir = temp();
        let (path, legacy) = (dir.join("profiles.json"), dir.join("roles.json"));
        fs::write(&legacy, r#"{"lead":{"model":"gpt-4o"}}"#).unwrap();
        let (store, warnings) = load(&path, &legacy);
        assert_eq!(store.effective("").lead.model, roles::MODELS[0]);
        assert!(
            warnings.iter().any(|w| w.contains("gpt-4o")),
            "{warnings:?}"
        );
        fs::remove_file(&path).unwrap();

        fs::write(&legacy, "{ not json").unwrap();
        let (store, warnings) = load(&path, &legacy);
        assert_eq!(store, Store::default());
        assert!(warnings[0].contains("left untouched"));
        assert!(
            !path.exists(),
            "nothing is written for a file that could not be read"
        );
        assert_eq!(fs::read_to_string(&legacy).unwrap(), "{ not json");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_file_means_built_ins_and_a_bad_file_is_kept_aside() {
        let dir = temp();
        let (path, legacy) = (dir.join("profiles.json"), dir.join("roles.json"));
        assert_eq!(load(&path, &legacy), (Store::default(), vec![]));
        assert!(!path.exists());

        for bad in ["{ not json", r#"{"version": 2, "active": "Budget"}"#] {
            fs::write(&path, bad).unwrap();
            let (store, warnings) = load(&path, &legacy);
            assert_eq!(store, Store::default());
            assert!(warnings[0].contains("could not be used"), "{warnings:?}");
            assert!(!path.exists());
            let aside = path.with_extension("json.invalid");
            assert_eq!(fs::read_to_string(&aside).unwrap(), bad);
            fs::remove_file(aside).unwrap();
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn reset_restores_the_built_in_values_of_the_active_profile() {
        let mut store = Store::default();
        store.cycle_active(-1); // Balanced -> Budget
        assert_eq!(store.active, "Budget");
        store
            .set_profile_value(Role::Coder, Field::Concurrency, "4")
            .unwrap();
        store.reset_profile_role(Role::Coder);
        assert_eq!(store.active_settings().coder.concurrency, 1);
    }

    #[test]
    fn project_keys_ignore_case_slashes_and_trailing_separators() {
        let a = project_key(Path::new(r"C:\Not\There\Repo\"));
        assert_eq!(a, "c:/not/there/repo");
        assert_eq!(a, project_key(Path::new("c:/NOT/there/repo")));
        let here = std::env::current_dir().unwrap();
        assert_eq!(project_key(&here), project_key(&here.join(".")));
    }
}
