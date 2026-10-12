//! Agent Plugins v1.0: manifest loading, skill and MCP discovery, path containment and trust.
//!
//! A plugin is a folder with a `plugin.json`, optional `skills/` and an optional `mcp.json`
//! (<https://agent-plugins.org>, schema 1.0.0). Loading only reads and validates; nothing here
//! starts a process. Whatever could run (skills, MCP servers) is handed out only by
//! [`Store::activate`], and only for a plugin the user trusted *at its current contents*.
//!
//! Trust is kept in the user's data folder (`plugins.json`), never in the repository, so a cloned
//! repository cannot enable its own plugins. A grant is bound to the plugin's folder and to a
//! SHA-256 of every file in it: any edit, from `mcp.json` to a bundled script, revokes it.
//! The check is made against the disk at the moment of use ([`Store::trust`], [`Store::state`],
//! [`Store::activate`] re-hash the folder), never from a stored [`Plugin`] alone. It is a
//! point-in-time check, not a lock: a file may still change between it and a later launch.
//!
//! Failures follow the specification's boundaries: a bad manifest rejects the plugin, a bad
//! skill or MCP entry is skipped and reported in [`Plugin::issues`], and valid siblings load.

use crate::profiles::project_key;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs, io,
    net::IpAddr,
    path::{Path, PathBuf},
};

pub const SCHEMA: &str = "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json";
pub const MCP_SCHEMA: &str = "https://agent-plugins.org/schemas/1.0.0/mcp.schema.json";
const SCHEMA_PREFIX: &str = "https://agent-plugins.org/schemas/";
/// Format version of `plugins.json`. Older readers refuse a newer file.
const VERSION: u32 = 1;
/// Largest JSON or SKILL.md file read. Larger ones are refused rather than loaded.
const MAX_TEXT: u64 = 1 << 20;
/// Bounds on what one plugin may contain, so fingerprinting cannot be made to run away.
const MAX_ENTRIES: usize = 10_000;
const MAX_BYTES: u64 = 64 << 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub name: String,
    pub version: Option<String>,
    pub description: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// The skill's folder, resolved and inside the plugin.
    pub dir: PathBuf,
    /// Regular files directly inside the skill's `references/`, resolved and inside the plugin.
    pub references: Vec<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transport {
    Stdio {
        /// A bare executable name, or the resolved path of a bundled `./` executable.
        command: PathBuf,
        /// Placeholders already expanded.
        args: Vec<String>,
        env: BTreeMap<String, String>,
        cwd: PathBuf,
    },
    Http {
        /// `true` for the deprecated HTTP+SSE transport.
        sse: bool,
        url: String,
        headers: BTreeMap<String, String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServer {
    pub name: String,
    pub transport: Transport,
}

#[derive(Clone, Debug)]
pub struct Plugin {
    /// The plugin folder, fully resolved.
    pub root: PathBuf,
    /// `PLUGIN_DATA` for this plugin. Not created by loading.
    pub data: PathBuf,
    pub manifest: Manifest,
    pub skills: Vec<Skill>,
    pub mcp: Vec<McpServer>,
    /// Everything that was skipped or ignored, for the user. Never fatal.
    pub issues: Vec<String>,
    /// SHA-256 over the names and contents of every file in the plugin.
    pub fingerprint: String,
}

/// Echoes untrusted text in a message without letting it carry terminal escapes or run on.
fn clip(s: &str) -> String {
    let mut out: String = s.chars().filter(|c| !c.is_control()).take(120).collect();
    if s.chars().count() > 120 {
        out.push_str("...");
    }
    out
}

/// `\\?\C:\x` becomes `C:\x`, which child processes accept as a working directory.
fn plain(p: PathBuf) -> PathBuf {
    match p.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => p,
    }
}

fn canon(p: &Path) -> io::Result<PathBuf> {
    fs::canonicalize(p).map(plain)
}

/// `p` resolved (symlinks and junctions followed), if it lands inside `root`.
fn contained(root: &Path, p: &Path) -> Option<PathBuf> {
    canon(p).ok().filter(|c| c.starts_with(root))
}

/// `base` plus the relative `rel`, which must not climb out: no `..`, no absolute or drive
/// parts, and, once it exists, a resolved location still inside `base`.
fn inside(base: &Path, rel: &str) -> Option<PathBuf> {
    let mut path = base.to_path_buf();
    for part in rel.split('/').filter(|p| !p.is_empty() && *p != ".") {
        if part == ".." || part.contains(['\\', ':']) || part.chars().any(char::is_control) {
            return None;
        }
        path.push(part);
    }
    // A missing target cannot be resolved, so resolve its nearest existing parent instead.
    // ponytail: a base that does not exist yet (PLUGIN_DATA) is checked lexically only.
    let Ok(base_real) = canon(base) else {
        return Some(path);
    };
    let mut probe = path.as_path();
    loop {
        match canon(probe) {
            Ok(real) => return real.starts_with(&base_real).then_some(path),
            Err(_) => probe = probe.parent()?,
        }
    }
}

fn read_text(path: &Path) -> Result<String, String> {
    let len = fs::metadata(path).map_err(|e| e.to_string())?.len();
    if len > MAX_TEXT {
        return Err(format!("larger than {} KiB", MAX_TEXT >> 10));
    }
    fs::read_to_string(path).map_err(|e| e.to_string())
}

/// Loads and validates the plugin in `dir`. `data_base` is the folder under which each plugin's
/// `PLUGIN_DATA` directory lives. `Err` means the plugin is rejected and none of it may be used.
pub fn load(dir: &Path, data_base: &Path) -> Result<Plugin, String> {
    let root = canon(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if !root.is_dir() {
        return Err(format!("{} is not a folder", dir.display()));
    }
    let manifest_path = contained(&root, &root.join("plugin.json"))
        .filter(|p| p.is_file())
        .ok_or("plugin.json is missing, or is not a file inside the plugin folder")?;
    // Hashed before and after parsing. This catches a change that is still in place after the
    // parse; it cannot catch one that is made and then reverted between the two hashes.
    let before = fingerprint(&root)?;
    let text = read_text(&manifest_path).map_err(|e| format!("plugin.json: {e}"))?;
    let mut issues = Vec::new();
    let manifest = parse_manifest(&text, &mut issues)?;
    let data = data_base.join(&manifest.name);
    let skills = discover_skills(&root, &mut issues);
    let mcp = load_mcp(&root, &data, &mut issues);
    let fingerprint = fingerprint(&root)?;
    if fingerprint != before {
        return Err("the plugin changed while it was being loaded; try again".into());
    }
    Ok(Plugin {
        root,
        data,
        manifest,
        skills,
        mcp,
        issues,
        fingerprint,
    })
}

fn valid_name(name: &str) -> bool {
    let edge = |c: Option<char>| c.is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    (1..=64).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
        && edge(name.chars().next())
        && edge(name.chars().last())
        && !name.contains("--")
        && !name.contains("..")
}

fn parse_manifest(text: &str, issues: &mut Vec<String>) -> Result<Manifest, String> {
    let value: Value =
        serde_json::from_str(text).map_err(|e| format!("plugin.json is not valid JSON: {e}"))?;
    let obj = value
        .as_object()
        .ok_or("plugin.json must be a JSON object")?;
    match obj.get("$schema").and_then(Value::as_str) {
        Some(SCHEMA) => {}
        Some(other) if other.starts_with(SCHEMA_PREFIX) => {
            return Err(format!(
                "plugin.json targets an unsupported Agent Plugins schema ({}); this version \
                 supports 1.0.0",
                clip(other)
            ))
        }
        _ => return Err(format!("plugin.json: $schema must be \"{SCHEMA}\"")),
    }
    let name = obj
        .get("name")
        .and_then(Value::as_str)
        .ok_or("plugin.json: name is required and must be a string")?;
    if !valid_name(name) {
        return Err(format!(
            "plugin.json: name '{}' is not valid (1-64 characters of a-z, 0-9, '-' and '.'; \
             starts and ends with a letter or digit; no '--' or '..')",
            clip(name)
        ));
    }
    for (key, v) in obj {
        let ok = match key.as_str() {
            "$schema" | "name" => true,
            "version" | "description" | "homepage" | "repository" | "license" => v.is_string(),
            "keywords" => v.as_array().is_some_and(|a| a.iter().all(Value::is_string)),
            "author" => v.as_object().is_some_and(|a| {
                a.iter()
                    .all(|(k, v)| ["name", "email", "url"].contains(&k.as_str()) && v.is_string())
            }),
            "extensions" => {
                if !v.is_object() {
                    issues.push("plugin.json: extensions is not an object; ignored.".into());
                }
                true
            }
            other => {
                issues.push(format!(
                    "plugin.json: unknown field '{}' ignored.",
                    clip(other)
                ));
                true
            }
        };
        if !ok {
            return Err(format!("plugin.json: '{}' has the wrong type", clip(key)));
        }
    }
    let text = |k: &str| obj.get(k).and_then(Value::as_str).map(str::to_owned);
    Ok(Manifest {
        name: name.into(),
        version: text("version"),
        description: text("description"),
    })
}

/// `name` and `description` from a SKILL.md's frontmatter. Only the top-level keys the Agent
/// Skills specification requires are read; block scalars (`>`, `|`) are joined onto one line.
fn frontmatter(text: &str) -> Option<BTreeMap<String, String>> {
    let mut lines = text.lines();
    if lines.next()?.trim_end() != "---" {
        return None;
    }
    let mut map = BTreeMap::new();
    let mut key: Option<String> = None;
    for line in lines {
        if line.trim_end() == "---" {
            return Some(map);
        }
        if line.starts_with([' ', '\t']) {
            if let Some(v) = key.as_ref().and_then(|k| map.get_mut(k)) {
                let v: &mut String = v;
                if !v.is_empty() {
                    v.push(' ');
                }
                v.push_str(line.trim());
            }
        } else if let Some((k, v)) = line.split_once(':') {
            let v = v.trim();
            let v = if v.starts_with(['>', '|']) { "" } else { v };
            let v = v.trim_matches(['"', '\'']);
            key = Some(k.trim().to_string());
            map.insert(k.trim().to_string(), v.to_string());
        }
    }
    None
}

fn discover_skills(root: &Path, issues: &mut Vec<String>) -> Vec<Skill> {
    let dir = root.join("skills");
    match fs::metadata(&dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return vec![],
        Err(e) => {
            issues.push(format!("skills: {e}; no skills loaded."));
            return vec![];
        }
        Ok(m) if !m.is_dir() => {
            issues.push("skills is not a folder; no skills loaded.".into());
            return vec![];
        }
        Ok(_) => {}
    }
    if contained(root, &dir).is_none() {
        issues.push("skills resolves outside the plugin; no skills loaded.".into());
        return vec![];
    }
    let mut children: Vec<_> = match fs::read_dir(&dir) {
        Ok(rd) => rd.filter_map(Result::ok).collect(),
        Err(e) => {
            issues.push(format!("skills: {e}; no skills loaded."));
            return vec![];
        }
    };
    children.sort_by_key(|e| e.file_name());
    let mut skills = Vec::new();
    for child in children {
        let Some(name) = child.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !child.path().is_dir() {
            continue;
        }
        // Resolved before it is listed: a link out of the plugin is never enumerated.
        if contained(root, &child.path()).is_none() {
            issues.push(format!(
                "skill '{}' skipped: it resolves outside the plugin.",
                clip(&name)
            ));
            continue;
        }
        // Only an exact SKILL.md makes a folder a skill (not skill.md on a case-blind disk).
        let has_skill_md = fs::read_dir(child.path())
            .map(|rd| {
                rd.filter_map(Result::ok)
                    .any(|e| e.file_name() == "SKILL.md")
            })
            .unwrap_or(false);
        if !has_skill_md {
            continue;
        }
        match load_skill(root, &name, &child.path(), issues) {
            Ok(skill) => skills.push(skill),
            Err(why) => issues.push(format!("skill '{}' skipped: {why}.", clip(&name))),
        }
    }
    skills
}

fn load_skill(
    root: &Path,
    name: &str,
    dir: &Path,
    issues: &mut Vec<String>,
) -> Result<Skill, String> {
    let real = contained(root, dir).ok_or("it resolves outside the plugin")?;
    let md = contained(root, &dir.join("SKILL.md"))
        .filter(|p| p.is_file())
        .ok_or("SKILL.md is not a file inside the plugin")?;
    let text = read_text(&md).map_err(|e| format!("SKILL.md: {e}"))?;
    let front = frontmatter(&text).ok_or("SKILL.md has no frontmatter")?;
    if front.get("name").map(String::as_str) != Some(name)
        || !valid_name(name)
        || name.contains('.')
    {
        return Err("frontmatter name must equal the folder name (a-z, 0-9, '-')".into());
    }
    let description = front.get("description").cloned().unwrap_or_default();
    if description.is_empty() || description.chars().count() > 1024 {
        return Err("description must be 1-1024 characters".into());
    }
    let mut references = Vec::new();
    let refs = real.join("references");
    // The directory itself is resolved and checked before it is listed, so a link (symlink or
    // junction) leaving the plugin is neither enumerated nor allowed to name its contents.
    // Listing then uses the resolved path.
    let refs = match fs::symlink_metadata(&refs) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => {
            issues.push(format!("skill '{}': references: {e}; skipped.", clip(name)));
            None
        }
        Ok(_) => match contained(root, &refs) {
            Some(r) if r.is_dir() => Some(r),
            Some(_) => None,
            None => {
                issues.push(format!(
                    "skill '{}': references resolves outside the plugin or cannot be resolved; \
                     skipped.",
                    clip(name)
                ));
                None
            }
        },
    };
    if let Some(refs) = refs {
        let mut files: Vec<_> = fs::read_dir(&refs)
            .map(|rd| rd.filter_map(Result::ok).collect())
            .unwrap_or_default();
        files.sort_by_key(|e| e.file_name());
        for f in files {
            match contained(root, &f.path()) {
                Some(p) if p.is_file() => references.push(p),
                Some(_) => {}
                None => issues.push(format!(
                    "skill '{}': reference '{}' resolves outside the plugin; skipped.",
                    clip(name),
                    clip(&f.file_name().to_string_lossy())
                )),
            }
        }
    }
    Ok(Skill {
        name: name.into(),
        description,
        dir: real,
        references,
    })
}

/// Replaces `${PLUGIN_ROOT}` and `${PLUGIN_DATA}` once, left to right. Inserted text is not
/// scanned again and any other `${...}` stays as written.
fn expand(s: &str, root: &str, data: &str) -> String {
    let (mut out, mut rest) = (String::new(), s);
    while let Some(i) = rest.find("${") {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        if let Some(t) = tail.strip_prefix("${PLUGIN_ROOT}") {
            out.push_str(root);
            rest = t;
        } else if let Some(t) = tail.strip_prefix("${PLUGIN_DATA}") {
            out.push_str(data);
            rest = t;
        } else {
            out.push_str("${");
            rest = &tail[2..];
        }
    }
    out.push_str(rest);
    out
}

fn load_mcp(root: &Path, data: &Path, issues: &mut Vec<String>) -> Vec<McpServer> {
    let path = root.join("mcp.json");
    match fs::metadata(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return vec![],
        Ok(m) if m.is_file() => {}
        _ => {
            issues.push("mcp.json is not a readable file; MCP disabled.".into());
            return vec![];
        }
    }
    let parsed = contained(root, &path)
        .ok_or_else(|| "it resolves outside the plugin".to_string())
        .and_then(|p| read_text(&p))
        .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| format!("invalid JSON: {e}")));
    let obj = match parsed {
        Ok(Value::Object(o)) => o,
        Ok(_) => {
            issues.push("mcp.json must be a JSON object; MCP disabled.".into());
            return vec![];
        }
        Err(e) => {
            issues.push(format!("mcp.json: {e}; MCP disabled."));
            return vec![];
        }
    };
    let servers = match (obj.len(), obj.get("$schema"), obj.get("mcpServers")) {
        (2, Some(Value::String(s)), Some(Value::Object(servers))) if s == MCP_SCHEMA => servers,
        _ => {
            issues.push(format!(
                "mcp.json must hold exactly \"$schema\": \"{MCP_SCHEMA}\" and \"mcpServers\"; \
                 MCP disabled."
            ));
            return vec![];
        }
    };
    let mut out = Vec::new();
    for (name, cfg) in servers {
        match parse_server(cfg, root, data) {
            Ok(transport) => out.push(McpServer {
                name: name.clone(),
                transport,
            }),
            Err(why) => issues.push(format!("MCP server '{}' skipped: {why}.", clip(name))),
        }
    }
    out
}

fn strings(v: Option<&Value>, what: &str) -> Result<Vec<String>, String> {
    let Some(v) = v else { return Ok(vec![]) };
    v.as_array()
        .and_then(|a| a.iter().map(|x| x.as_str().map(str::to_owned)).collect())
        .ok_or_else(|| format!("{what} must be an array of strings"))
}

fn string_map(v: Option<&Value>, what: &str) -> Result<BTreeMap<String, String>, String> {
    let Some(v) = v else {
        return Ok(BTreeMap::new());
    };
    v.as_object()
        .and_then(|o| {
            o.iter()
                .map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                .collect()
        })
        .ok_or_else(|| format!("{what} must be an object of strings"))
}

fn parse_server(cfg: &Value, root: &Path, data: &Path) -> Result<Transport, String> {
    let obj = cfg.as_object().ok_or("it must be an object")?;
    let kind = obj
        .get("type")
        .and_then(Value::as_str)
        .ok_or("type is required")?;
    let allowed: &[&str] = match kind {
        "stdio" => &["type", "command", "args", "env", "cwd"],
        "streamable-http" | "sse" => &["type", "url", "headers"],
        other => return Err(format!("unsupported type '{}'", clip(other))),
    };
    if let Some(k) = obj.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(format!("field '{}' is not allowed for {kind}", clip(k)));
    }
    if kind == "stdio" {
        parse_stdio(obj, root, data)
    } else {
        parse_http(obj, kind == "sse")
    }
}

fn parse_stdio(obj: &Map<String, Value>, root: &Path, data: &Path) -> Result<Transport, String> {
    let command = obj
        .get("command")
        .and_then(Value::as_str)
        .ok_or("command is required")?;
    let command = match command.strip_prefix("./") {
        Some(rel) if !rel.trim_matches('/').is_empty() => {
            inside(root, rel).ok_or("command resolves outside the plugin")?
        }
        _ if !command.is_empty()
            && !command.starts_with('.')
            && command
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.+".contains(c)) =>
        {
            PathBuf::from(command)
        }
        _ => {
            return Err(
                "command must be one executable name or a './' path inside the plugin".into(),
            )
        }
    };
    let (root_text, data_text) = (root.to_string_lossy(), data.to_string_lossy());
    let args = strings(obj.get("args"), "args")?
        .iter()
        .map(|a| expand(a, &root_text, &data_text))
        .collect();
    let mut env = BTreeMap::new();
    for (k, v) in string_map(obj.get("env"), "env")? {
        if k.is_empty() || k.contains('=') || k.chars().any(char::is_control) {
            return Err("env has an invalid variable name".into());
        }
        if k == "PLUGIN_ROOT" || k == "PLUGIN_DATA" {
            return Err(format!("env must not set {k}"));
        }
        env.insert(k, expand(&v, &root_text, &data_text));
    }
    let cwd = match obj.get("cwd") {
        None => root.to_path_buf(),
        Some(Value::String(s)) => {
            let under = |marker: &str, base: &Path| match s.strip_prefix(marker) {
                Some("") => Some(inside(base, "")),
                Some(rest) => rest.strip_prefix('/').map(|rel| inside(base, rel)),
                None => None,
            };
            let hit = s
                .strip_prefix("./")
                .map(|rel| inside(root, rel))
                .or_else(|| under("${PLUGIN_ROOT}", root))
                .or_else(|| under("${PLUGIN_DATA}", data))
                .ok_or("cwd must start with './', ${PLUGIN_ROOT} or ${PLUGIN_DATA}")?;
            hit.ok_or("cwd resolves outside its folder")?
        }
        Some(_) => return Err("cwd must be a string".into()),
    };
    Ok(Transport::Stdio {
        command,
        args,
        env,
        cwd,
    })
}

fn is_loopback(host: &str) -> bool {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost") || bare.parse::<IpAddr>().is_ok_and(|a| a.is_loopback())
}

fn check_url(url: &str) -> Result<(), String> {
    if url
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
    {
        return Err("url has invalid characters".into());
    }
    let lower = url.to_ascii_lowercase();
    let (secure, rest) = if lower.starts_with("https://") {
        (true, &url[8..])
    } else if lower.starts_with("http://") {
        (false, &url[7..])
    } else {
        return Err("url must be an absolute http(s) URL".into());
    };
    if rest.contains('#') {
        return Err("url must not have a fragment".into());
    }
    let authority = rest.split(['/', '?']).next().unwrap_or_default();
    if authority.contains('@') {
        return Err("url must not contain user info".into());
    }
    let (host, port) = match authority.strip_prefix('[') {
        Some(v6) => match v6.split_once(']') {
            Some((h, p)) => (format!("[{h}]"), p.strip_prefix(':').unwrap_or(p)),
            None => return Err("url has an unclosed '['".into()),
        },
        None => match authority.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p),
            None => (authority.to_string(), ""),
        },
    };
    if host.is_empty() || (!port.is_empty() && port.parse::<u16>().is_err()) {
        return Err("url has no valid host or port".into());
    }
    if !secure && !is_loopback(&host) {
        return Err("url must use https unless the host is localhost or a loopback address".into());
    }
    Ok(())
}

fn parse_http(obj: &Map<String, Value>, sse: bool) -> Result<Transport, String> {
    let url = obj
        .get("url")
        .and_then(Value::as_str)
        .ok_or("url is required")?;
    check_url(url)?;
    let headers = string_map(obj.get("headers"), "headers")?;
    let mut seen = BTreeSet::new();
    for (name, value) in &headers {
        let token = |c: char| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c);
        if name.is_empty() || !name.chars().all(token) {
            return Err("headers has an invalid header name".into());
        }
        if value.chars().any(|c| c.is_control() && c != '\t') {
            return Err("headers has an invalid header value".into());
        }
        if !seen.insert(name.to_ascii_lowercase()) {
            return Err("headers has the same name twice".into());
        }
    }
    Ok(Transport::Http {
        sse,
        url: url.into(),
        headers,
    })
}

/// Hashes the path and contents of everything in the plugin, in a fixed order. Symlinks and
/// junctions are not followed, but their targets are hashed, so retargeting one is a change.
/// Nothing is skipped, `.git` included: a bundled command may live there.
fn fingerprint(root: &Path) -> Result<String, String> {
    fn walk(dir: &Path, rel: &str, h: &mut Sha256, seen: &mut (usize, u64)) -> io::Result<()> {
        let mut entries: Vec<_> = fs::read_dir(dir)?.collect::<io::Result<_>>()?;
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            seen.0 += 1;
            if seen.0 > MAX_ENTRIES {
                return Err(io::Error::other(format!("more than {MAX_ENTRIES} entries")));
            }
            let path = format!("{rel}{name}");
            let kind = e.file_type()?;
            if kind.is_symlink() {
                let target = fs::read_link(e.path()).unwrap_or_default();
                h.update(format!("L {path} -> {}\n", target.display()));
            } else if kind.is_dir() {
                h.update(format!("D {path}\n"));
                walk(&e.path(), &format!("{path}/"), h, seen)?;
            } else {
                let len = e.metadata()?.len();
                seen.1 += len;
                if seen.1 > MAX_BYTES {
                    return Err(io::Error::other(format!(
                        "more than {} MiB",
                        MAX_BYTES >> 20
                    )));
                }
                h.update(format!("F {path} {len}\n"));
                h.update(fs::read(e.path())?);
            }
        }
        Ok(())
    }
    let mut hasher = Sha256::new();
    walk(root, "", &mut hasher, &mut (0, 0))
        .map_err(|e| format!("could not fingerprint the plugin: {e}"))?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// Ok only if `plugin.root` still resolves to itself and its contents hash to the fingerprint
/// taken at load. Any difference or read failure is an error, so callers fail closed.
fn unchanged(plugin: &Plugin) -> Result<(), String> {
    if canon(&plugin.root).ok().as_ref() != Some(&plugin.root) {
        return Err("the plugin folder is missing or now resolves elsewhere".into());
    }
    if fingerprint(&plugin.root)? != plugin.fingerprint {
        return Err("the plugin changed after it was loaded; load it again and review it".into());
    }
    Ok(())
}

/// Where a plugin stands with the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Never trusted from this folder.
    Untrusted,
    /// Trusted before, but its files differ now. Review and trust again.
    Changed,
    Trusted,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            State::Untrusted => "not trusted",
            State::Changed => "changed or unreadable since it was trusted or loaded; not trusted",
            State::Trusted => "trusted",
        }
    }
}

/// What a trusted plugin lets a run use. Obtained only through [`Store::activate`].
pub struct Activation<'a> {
    pub skills: &'a [Skill],
    pub mcp: &'a [McpServer],
}

#[derive(Debug, Default)]
pub struct Store {
    /// Folder key to the contents the user trusted there.
    grants: BTreeMap<String, Grant>,
    /// Set when the file could not be used; saving is refused so it is not overwritten.
    protected: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Grant {
    name: String,
    fingerprint: String,
}

#[derive(Deserialize)]
struct File {
    version: u32,
    #[serde(default)]
    grants: BTreeMap<String, Grant>,
}

impl Store {
    /// Reads `plugins.json`. A missing file means nothing is trusted. A file that cannot be used
    /// also trusts nothing, and is left as it is (see [`Store::warning`]).
    pub fn load(path: &Path) -> Store {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Store::default(),
            Err(e) => return Store::shielded(format!("could not be read ({e})")),
        };
        match serde_json::from_str::<File>(&text) {
            Ok(f) if f.version <= VERSION => Store {
                grants: f.grants,
                protected: None,
            },
            Ok(_) => Store::shielded("comes from a newer Agentree".into()),
            Err(e) => Store::shielded(format!("is not valid ({e})")),
        }
    }

    fn shielded(why: String) -> Store {
        Store {
            grants: BTreeMap::new(),
            protected: Some(why),
        }
    }

    /// Why the saved trust file is being ignored, if it is.
    pub fn warning(&self) -> Option<String> {
        self.protected.as_ref().map(|why| {
            format!("plugins.json {why}. No plugin is trusted and changes will not be saved.")
        })
    }

    /// `Trusted` only if the folder on disk *right now* still hashes to what was loaded and to
    /// what was granted. A changed, missing or unreadable file, or a relinked folder, is
    /// `Changed` (or `Untrusted` with no grant): the answer never rests on the loaded object alone.
    pub fn state(&self, plugin: &Plugin) -> State {
        let Some(g) = self.grants.get(&project_key(&plugin.root)) else {
            return State::Untrusted;
        };
        if g.fingerprint == plugin.fingerprint
            && g.name == plugin.manifest.name
            && unchanged(plugin).is_ok()
        {
            State::Trusted
        } else {
            State::Changed
        }
    }

    /// The skills and MCP servers a run may use, or `None` unless the plugin is trusted at its
    /// current contents. Anything that starts a plugin's tools must go through this.
    ///
    /// The contents are re-hashed on every call. Between this check and the launch that uses
    /// the result a file can still change; launching must therefore call this immediately
    /// before starting anything, and a launcher cannot make that window zero.
    pub fn activate<'a>(&self, plugin: &'a Plugin) -> Option<Activation<'a>> {
        (self.state(plugin) == State::Trusted).then_some(Activation {
            skills: &plugin.skills,
            mcp: &plugin.mcp,
        })
    }

    /// Grants trust to the plugin as it is on disk now. Refused, and nothing is recorded, if the
    /// folder no longer matches the `plugin` that was loaded and shown for review.
    pub fn trust(&mut self, plugin: &Plugin) -> Result<(), String> {
        unchanged(plugin)?;
        self.grants.insert(
            project_key(&plugin.root),
            Grant {
                name: plugin.manifest.name.clone(),
                fingerprint: plugin.fingerprint.clone(),
            },
        );
        Ok(())
    }

    /// Drops the grant for the plugin in `dir`, whether or not it still loads.
    pub fn revoke(&mut self, dir: &Path) -> bool {
        self.grants.remove(&project_key(dir)).is_some()
    }

    /// Writes through a temporary file and a rename, so a crash cannot leave half a file.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(why) = &self.protected {
            return Err(io::Error::other(format!("plugins.json {why}")));
        }
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let grants: BTreeMap<_, _> = self
            .grants
            .iter()
            .map(|(k, g)| (k, json!({ "name": g.name, "fingerprint": g.fingerprint })))
            .collect();
        let bytes = serde_json::to_vec_pretty(&json!({ "version": VERSION, "grants": grants }))
            .map_err(io::Error::other)?;
        let temp = path.with_extension("json.tmp");
        fs::write(&temp, bytes)?;
        fs::rename(&temp, path)
    }
}

/// What the user sees before deciding: what the plugin is, what it would run, and what was
/// skipped. MCP commands are shown in full because that is what trust grants.
pub fn report(plugin: &Plugin, state: State) -> String {
    let m = &plugin.manifest;
    let mut out = format!(
        "{} {}\n  folder: {}\n  state:  {}\n",
        m.name,
        clip(m.version.as_deref().unwrap_or("(no version)")),
        plugin.root.display(),
        state.label()
    );
    if let Some(d) = &m.description {
        let _ = writeln!(out, "  about:  {}", clip(d));
    }
    let _ = writeln!(out, "  skills ({}):", plugin.skills.len());
    for s in &plugin.skills {
        let _ = writeln!(
            out,
            "    {} ({} references) - {}",
            s.name,
            s.references.len(),
            clip(&s.description)
        );
    }
    let _ = writeln!(out, "  MCP servers ({}):", plugin.mcp.len());
    for s in &plugin.mcp {
        match &s.transport {
            Transport::Stdio {
                command,
                args,
                env,
                cwd,
            } => {
                // Debug formatting escapes control characters and keeps argument boundaries.
                let _ = writeln!(out, "    {}: runs {:?} {:?}", clip(&s.name), command, args);
                let _ = writeln!(out, "      in {:?}", cwd);
                for (k, v) in env {
                    let _ = writeln!(out, "      env {k:?} = {v:?}");
                }
            }
            Transport::Http { url, sse, headers } => {
                let kind = if *sse { "sse" } else { "streamable-http" };
                let _ = writeln!(out, "    {}: connects ({kind}) to {url}", clip(&s.name));
                if !headers.is_empty() {
                    let names: Vec<_> = headers.keys().collect();
                    let _ = writeln!(out, "      sends headers {names:?}");
                }
            }
        }
    }
    for issue in &plugin.issues {
        let _ = writeln!(out, "  note: {issue}");
    }
    out
}

/// The `--plugin` command: inspect the plugin in `dir`, and with `trust` or `revoke` record that
/// decision. Returns the text to print.
pub fn command(
    dir: &Path,
    data_base: &Path,
    store_path: &Path,
    trust: bool,
    revoke: bool,
) -> Result<String, String> {
    let mut store = Store::load(store_path);
    let save = |store: &Store| {
        store
            .save(store_path)
            .map_err(|e| format!("could not save {}: {e}", store_path.display()))
    };
    if revoke {
        let had = store.revoke(dir);
        save(&store)?;
        return Ok(if had {
            "Trust revoked.\n".into()
        } else {
            "That plugin was not trusted.\n".into()
        });
    }
    let plugin = load(dir, data_base)?;
    if trust {
        store.trust(&plugin)?;
        save(&store)?;
    }
    let mut out = report(&plugin, store.state(&plugin));
    if let Some(w) = store.warning() {
        let _ = writeln!(out, "  warning: {w}");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    struct Dir(PathBuf);
    impl Dir {
        fn new() -> Dir {
            let p = std::env::temp_dir().join(format!("agentree-plugins-{}", Uuid::new_v4()));
            fs::create_dir_all(&p).unwrap();
            Dir(p)
        }
        fn write(&self, rel: &str, text: &str) {
            let p = self.0.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn manifest(extra: &str) -> String {
        format!(r#"{{"$schema":"{SCHEMA}","name":"demo","version":"1.0.0"{extra}}}"#)
    }
    fn skill(d: &Dir, name: &str) {
        d.write(
            &format!("skills/{name}/SKILL.md"),
            &format!("---\nname: {name}\ndescription: Does {name}.\n---\nBody\n"),
        );
    }
    fn mcp(servers: &str) -> String {
        format!(r#"{{"$schema":"{MCP_SCHEMA}","mcpServers":{servers}}}"#)
    }
    fn plugin(d: &Dir) -> Plugin {
        load(&d.0, &d.0.join("..").join("agentree-plugin-data")).unwrap()
    }
    fn reject(manifest_text: &str) -> String {
        let d = Dir::new();
        d.write("plugin.json", manifest_text);
        load(&d.0, &d.0).unwrap_err()
    }
    fn mcp_issues(servers: &str) -> (Vec<McpServer>, Vec<String>) {
        let d = Dir::new();
        d.write("plugin.json", &manifest(""));
        d.write("mcp.json", &mcp(servers));
        let p = plugin(&d);
        (p.mcp, p.issues)
    }

    #[test]
    fn compliant_plugin_loads_with_skills_references_and_mcp() {
        let d = Dir::new();
        d.write(
            "plugin.json",
            &manifest(r#","description":"Demo","author":{"name":"A"},"keywords":["x"],"extensions":{"com.example":{}}"#),
        );
        skill(&d, "review");
        skill(&d, "plan");
        d.write("skills/review/references/RULES.md", "rules");
        d.write("skills/review/scripts/run.sh", "echo");
        d.write("bin/tool.cmd", "echo");
        d.write(
            "mcp.json",
            &mcp(
                r#"{"local":{"type":"stdio","command":"./bin/tool.cmd","args":["--root","${PLUGIN_ROOT}"],"env":{"D":"${PLUGIN_DATA}/x"},"cwd":"${PLUGIN_ROOT}/bin"},
                    "remote":{"type":"streamable-http","url":"https://example.com/mcp","headers":{"X-Team":"a"}},
                    "dev":{"type":"sse","url":"http://localhost:8080/sse"}}"#,
            ),
        );
        let p = plugin(&d);
        assert!(p.issues.is_empty(), "{:?}", p.issues);
        assert_eq!(p.manifest.name, "demo");
        let names: Vec<_> = p.skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["plan", "review"]);
        let review = &p.skills[1];
        assert_eq!(review.references.len(), 1);
        assert!(review.references[0].starts_with(&p.root));
        assert_eq!(p.mcp.len(), 3);
        let local = p.mcp.iter().find(|s| s.name == "local").unwrap();
        let Transport::Stdio {
            command,
            args,
            env,
            cwd,
        } = &local.transport
        else {
            panic!("stdio expected");
        };
        assert_eq!(command, &p.root.join("bin").join("tool.cmd"));
        assert_eq!(args[1], p.root.to_string_lossy());
        assert!(env["D"].starts_with(&*p.data.to_string_lossy()));
        assert_eq!(cwd, &p.root.join("bin"));
    }

    #[test]
    fn invalid_manifests_are_rejected() {
        let ok = |name: &str| format!(r#"{{"$schema":"{SCHEMA}","name":"{name}"}}"#);
        assert!(reject("not json").contains("not valid JSON"));
        assert!(reject("[]").contains("JSON object"));
        assert!(reject(r#"{"name":"demo"}"#).contains("$schema"));
        assert!(reject(&format!(r#"{{"$schema":"{SCHEMA}"}}"#)).contains("name is required"));
        let v2 = SCHEMA.replace("1.0.0", "2.0.0");
        assert!(reject(&format!(r#"{{"$schema":"{v2}","name":"demo"}}"#)).contains("unsupported"));
        for bad in [
            "Demo", "-demo", "demo-", "de--mo", "de..mo", "de mo", "de_mo", "",
        ] {
            assert!(reject(&ok(bad)).contains("name"), "{bad:?} accepted");
        }
        assert!(reject(&ok(&"a".repeat(65))).contains("name"));
        for bad in [
            r#","version":1"#,
            r#","keywords":["a",2]"#,
            r#","author":{"name":"a","phone":"1"}"#,
            r#","author":{"name":1}"#,
            r#","author":"a""#,
        ] {
            assert!(reject(&manifest(bad)).contains("wrong type"), "{bad}");
        }
        let d = Dir::new();
        assert!(load(&d.0, &d.0).unwrap_err().contains("plugin.json"));
    }

    #[test]
    fn unknown_fields_and_odd_extensions_are_reported_not_fatal() {
        let d = Dir::new();
        d.write("plugin.json", &manifest(r#","surprise":1,"extensions":3"#));
        let p = plugin(&d);
        assert!(p.issues.iter().any(|i| i.contains("surprise")));
        assert!(p.issues.iter().any(|i| i.contains("extensions")));
    }

    #[test]
    fn bad_skills_are_skipped_and_siblings_load() {
        let d = Dir::new();
        d.write("plugin.json", &manifest(""));
        skill(&d, "good");
        d.write(
            "skills/wrong/SKILL.md",
            "---\nname: other\ndescription: x\n---\n",
        );
        d.write("skills/nodesc/SKILL.md", "---\nname: nodesc\n---\n");
        d.write("skills/nofront/SKILL.md", "just text");
        d.write(
            "skills/lower/skill.md",
            "---\nname: lower\ndescription: x\n---\n",
        );
        d.write(
            "skills/group/inner/SKILL.md",
            "---\nname: inner\ndescription: x\n---\n",
        );
        d.write(
            "skills/folded/SKILL.md",
            "---\nname: folded\ndescription: >\n  A long\n  description.\n---\n",
        );
        let p = plugin(&d);
        let names: Vec<_> = p.skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["folded", "good"], "{:?}", p.issues);
        assert_eq!(p.skills[0].description, "A long description.");
        assert_eq!(p.issues.len(), 3, "{:?}", p.issues);
    }

    #[test]
    fn skills_that_is_a_file_loads_nothing_but_the_plugin_still_loads() {
        let d = Dir::new();
        d.write("plugin.json", &manifest(""));
        d.write("skills", "oops");
        let p = plugin(&d);
        assert!(p.skills.is_empty());
        assert!(p.issues.iter().any(|i| i.contains("not a folder")));
    }

    /// A directory link: a symlink where allowed, else a junction, which needs no privilege.
    fn link_dir(link: &Path, target: &Path) -> bool {
        #[cfg(unix)]
        return std::os::unix::fs::symlink(target, link).is_ok();
        #[cfg(windows)]
        return std::os::windows::fs::symlink_dir(target, link).is_ok()
            || std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link.to_string_lossy().replace('/', "\\"))
                .arg(target)
                .output()
                .is_ok_and(|o| o.status.success());
    }

    /// Removes a directory link itself (never its target) on every platform.
    fn unlink(link: &Path) {
        fs::remove_dir(link)
            .or_else(|_| fs::remove_file(link))
            .unwrap();
    }

    #[test]
    fn links_out_of_the_plugin_are_skipped_or_invalid() {
        let outside = Dir::new();
        outside.write(
            "SKILL.md",
            "---
name: evil
description: x
---
",
        );
        outside.write("secret.txt", "s");
        let d = Dir::new();
        d.write("plugin.json", &manifest(""));
        skill(&d, "good");
        d.write(
            "mcp.json",
            &mcp(r#"{"s":{"type":"stdio","command":"./bin/tool"},
                     "t":{"type":"stdio","command":"npx","cwd":"./bin"}}"#),
        );
        assert!(
            link_dir(&d.0.join("skills").join("evil"), &outside.0)
                && link_dir(&d.0.join("skills/good/references"), &outside.0)
                && link_dir(&d.0.join("bin"), &outside.0),
            "could not create a directory link"
        );
        let p = plugin(&d);
        let names: Vec<_> = p.skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["good"], "{:?}", p.issues);
        assert!(p.skills[0].references.is_empty());
        assert!(p.mcp.is_empty());
        for needle in ["evil", "references", "'s' skipped", "'t' skipped"] {
            assert!(p.issues.iter().any(|i| i.contains(needle)), "{needle}");
        }
        // The linked-out folder is never listed, so none of its names can surface.
        assert!(
            p.issues.iter().all(|i| !i.contains("secret.txt")),
            "{:?}",
            p.issues
        );
    }

    #[test]
    fn references_link_out_is_not_enumerated_and_internal_links_still_work() {
        let outside = Dir::new();
        outside.write("secret.txt", "s");
        let d = Dir::new();
        d.write("plugin.json", &manifest(""));
        skill(&d, "out");
        skill(&d, "inside");
        skill(&d, "mixed");
        d.write("shared/a.md", "a");
        d.write("skills/mixed/references/own.md", "own");
        d.write("skills/mixed/references/ext.md", "x");
        assert!(
            link_dir(&d.0.join("skills/out/references"), &outside.0)
                && link_dir(&d.0.join("skills/inside/references"), &d.0.join("shared"))
        );
        // A file link out of an otherwise internal folder is still skipped and reported.
        fs::remove_file(d.0.join("skills/mixed/references/ext.md")).unwrap();
        outside.write("ext.md", "x");
        let file_link = {
            #[cfg(unix)]
            let r = std::os::unix::fs::symlink(
                outside.0.join("ext.md"),
                d.0.join("skills/mixed/references/ext.md"),
            );
            #[cfg(windows)]
            let r = std::os::windows::fs::symlink_file(
                outside.0.join("ext.md"),
                d.0.join("skills/mixed/references/ext.md"),
            );
            r.is_ok()
        };
        let p = plugin(&d);
        let by = |n: &str| p.skills.iter().find(|s| s.name == n).unwrap();
        assert!(by("out").references.is_empty());
        assert_eq!(by("inside").references.len(), 1);
        assert!(by("inside").references[0].starts_with(&p.root));
        assert!(by("mixed").references.iter().any(|r| r.ends_with("own.md")));
        assert!(
            p.issues.iter().all(|i| !i.contains("secret.txt")),
            "{:?}",
            p.issues
        );
        assert!(p
            .issues
            .iter()
            .any(|i| i.contains("'out'") && i.contains("references")));
        if file_link {
            assert_eq!(by("mixed").references.len(), 1);
            assert!(p.issues.iter().any(|i| i.contains("ext.md")));
        }
    }

    #[test]
    fn skill_folder_link_out_is_not_listed_even_without_a_skill_md() {
        let outside = Dir::new();
        outside.write("secret.txt", "s");
        let d = Dir::new();
        d.write("plugin.json", &manifest(""));
        fs::create_dir_all(d.0.join("skills")).unwrap();
        assert!(link_dir(&d.0.join("skills/ext"), &outside.0));
        let p = plugin(&d);
        assert!(p.skills.is_empty());
        assert!(
            p.issues.iter().any(|i| i.contains("'ext'")),
            "{:?}",
            p.issues
        );
        assert!(p.issues.iter().all(|i| !i.contains("secret.txt")));
    }

    #[test]
    fn invalid_mcp_servers_are_skipped_and_valid_ones_load() {
        let (servers, issues) = mcp_issues(
            r#"{"ok":{"type":"stdio","command":"npx","args":["-y","x"]},
                "web":{"type":"streamable-http","url":"https://example.com"},
                "notype":{"command":"npx"},
                "unknown":{"type":"websocket","url":"wss://x"},
                "mixed":{"type":"stdio","command":"npx","url":"https://x"},
                "extra":{"type":"stdio","command":"npx","bogus":1},
                "shell":{"type":"stdio","command":"npx -y x"},
                "parent":{"type":"stdio","command":"../x"},
                "abs":{"type":"stdio","command":"/usr/bin/x"},
                "windrive":{"type":"stdio","command":"C:\\x.exe"},
                "placeholder":{"type":"stdio","command":"${PLUGIN_ROOT}/x"},
                "dotdot":{"type":"stdio","command":"./a/../../x"},
                "cwdout":{"type":"stdio","command":"npx","cwd":"./../x"},
                "cwdabs":{"type":"stdio","command":"npx","cwd":"/tmp"},
                "cwdroot":{"type":"stdio","command":"npx","cwd":"${PLUGIN_ROOT}/../x"},
                "cwddata":{"type":"stdio","command":"npx","cwd":"${PLUGIN_DATA}/../x"},
                "envroot":{"type":"stdio","command":"npx","env":{"PLUGIN_ROOT":"x"}},
                "argtype":{"type":"stdio","command":"npx","args":[1]},
                "http":{"type":"streamable-http","url":"http://example.com/mcp"},
                "userinfo":{"type":"streamable-http","url":"https://u:p@example.com"},
                "fragment":{"type":"streamable-http","url":"https://example.com/#x"},
                "relative":{"type":"streamable-http","url":"/mcp"},
                "localhostish":{"type":"streamable-http","url":"http://localhost.evil.com/"},
                "dupheader":{"type":"streamable-http","url":"https://x.com","headers":{"A":"1","a":"2"}},
                "badheader":{"type":"streamable-http","url":"https://x.com","headers":{"A B":"1"}}}"#,
        );
        let names: Vec<_> = servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["ok", "web"], "{issues:#?}");
        assert_eq!(issues.len(), 23, "{issues:#?}");
    }

    #[test]
    fn loopback_http_is_allowed() {
        let (servers, issues) = mcp_issues(
            r#"{"a":{"type":"streamable-http","url":"http://localhost:3000/mcp"},
                "b":{"type":"streamable-http","url":"http://127.0.0.1/mcp"},
                "c":{"type":"streamable-http","url":"http://[::1]:9/mcp"},
                "d":{"type":"sse","url":"HTTP://LOCALHOST/sse"}}"#,
        );
        assert_eq!(servers.len(), 4, "{issues:#?}");
    }

    #[test]
    fn bad_mcp_json_disables_mcp_only() {
        for text in [
            "{",
            "[]",
            r#"{"mcpServers":{}}"#,
            &format!(
                r#"{{"$schema":"{}","mcpServers":{{}}}}"#,
                MCP_SCHEMA.replace("1.0.0", "1.1.0")
            ),
            &format!(r#"{{"$schema":"{MCP_SCHEMA}","mcpServers":{{}},"extra":1}}"#),
            &format!(r#"{{"$schema":"{MCP_SCHEMA}","mcpServers":[]}}"#),
        ] {
            let d = Dir::new();
            d.write("plugin.json", &manifest(""));
            skill(&d, "good");
            d.write("mcp.json", text);
            let p = plugin(&d);
            assert!(p.mcp.is_empty());
            assert_eq!(p.skills.len(), 1);
            assert!(
                p.issues.iter().any(|i| i.contains("MCP disabled")),
                "{text}"
            );
        }
        let d = Dir::new();
        d.write("plugin.json", &manifest(""));
        d.write("mcp.json/inner", "a directory, not a file");
        assert!(plugin(&d).issues.iter().any(|i| i.contains("MCP disabled")));
        // An empty server table is valid and not a problem.
        let (servers, issues) = mcp_issues("{}");
        assert!(servers.is_empty() && issues.is_empty());
    }

    #[test]
    fn placeholders_expand_once_and_only_known_ones() {
        assert_eq!(expand("${PLUGIN_ROOT}/a", "R", "D"), "R/a");
        assert_eq!(expand("${PLUGIN_DATA}${PLUGIN_ROOT}", "R", "D"), "DR");
        assert_eq!(
            expand("${PLUGIN_ROOT}", "${PLUGIN_DATA}", "D"),
            "${PLUGIN_DATA}"
        );
        assert_eq!(
            expand("${HOME} ${PLUGIN_ROOTX} ${", "R", "D"),
            "${HOME} ${PLUGIN_ROOTX} ${"
        );
    }

    #[test]
    fn untrusted_plugins_activate_nothing() {
        let d = Dir::new();
        d.write("plugin.json", &manifest(""));
        skill(&d, "review");
        d.write(
            "mcp.json",
            &mcp(r#"{"s":{"type":"stdio","command":"npx"}}"#),
        );
        let p = plugin(&d);
        let store = Store::default();
        assert_eq!(store.state(&p), State::Untrusted);
        assert!(store.activate(&p).is_none());
        assert!(!p.mcp.is_empty(), "loading alone still lists it for review");
    }

    #[test]
    fn trust_activates_and_any_change_revokes_it() {
        let d = Dir::new();
        d.write("plugin.json", &manifest(""));
        skill(&d, "review");
        d.write(
            "mcp.json",
            &mcp(r#"{"s":{"type":"stdio","command":"./run.cmd"}}"#),
        );
        d.write("run.cmd", "echo safe");
        let mut store = Store::default();
        store.trust(&plugin(&d)).unwrap();
        let loaded = plugin(&d);
        let a = store.activate(&loaded).expect("trusted plugin activates");
        assert_eq!((a.skills.len(), a.mcp.len()), (1, 1));
        d.write("run.cmd", "echo changed");
        let p = plugin(&d);
        assert_eq!(store.state(&p), State::Changed);
        assert!(store.activate(&p).is_none());
        store.trust(&p).unwrap();
        assert_eq!(store.state(&p), State::Trusted);
        d.write("skills/review/references/new.md", "more");
        assert_eq!(store.state(&plugin(&d)), State::Changed);
        assert!(store.revoke(&d.0));
        assert!(!store.revoke(&d.0));
        assert_eq!(store.state(&plugin(&d)), State::Untrusted);
    }

    fn bundled() -> (Dir, Plugin) {
        let d = Dir::new();
        d.write("plugin.json", &manifest(""));
        skill(&d, "review");
        d.write("run.cmd", "echo safe");
        d.write(
            "mcp.json",
            &mcp(r#"{"s":{"type":"stdio","command":"./run.cmd"}}"#),
        );
        let p = plugin(&d);
        (d, p)
    }

    /// Each edit is made *after* the `Plugin` was loaded, so only a fresh look at the disk can
    /// notice it. The stale object must neither be trusted nor stay activated.
    #[test]
    fn a_plugin_edited_after_loading_cannot_be_trusted_or_activated() {
        type Edit<'a> = (&'a str, &'a dyn Fn(&Dir));
        let edits: [Edit; 5] = [
            ("executable", &|d| d.write("run.cmd", "echo evil")),
            ("mcp.json", &|d| {
                d.write(
                    "mcp.json",
                    &mcp(r#"{"s":{"type":"stdio","command":"npx"}}"#),
                );
            }),
            ("skill", &|d| {
                d.write(
                    "skills/review/SKILL.md",
                    "---\nname: review\ndescription: Does it.\n---\nNew\n",
                );
            }),
            ("deleted file", &|d| {
                fs::remove_file(d.0.join("run.cmd")).unwrap()
            }),
            ("new file", &|d| {
                d.write("skills/review/references/x.md", "x")
            }),
        ];
        for (what, edit) in edits {
            // Edited between load and trust: refused, and nothing is recorded.
            let (d, p) = bundled();
            edit(&d);
            let mut store = Store::default();
            assert!(store.trust(&p).is_err(), "{what}");
            assert_eq!(store.state(&p), State::Untrusted, "{what}");
            assert!(store.activate(&p).is_none(), "{what}");

            // Edited between trust and activation: the stale object no longer activates, and
            // neither does the object loaded afterwards until it is trusted again.
            let (d, p) = bundled();
            let mut store = Store::default();
            store.trust(&p).unwrap();
            assert!(store.activate(&p).is_some(), "{what}");
            edit(&d);
            assert_eq!(store.state(&p), State::Changed, "{what}");
            assert!(store.activate(&p).is_none(), "{what}");
            let fresh = plugin(&d);
            assert!(store.activate(&fresh).is_none(), "{what}");

            // Trusting the new contents does not revive the stale object, which still holds the
            // old skills and servers.
            store.trust(&fresh).unwrap();
            assert!(store.activate(&fresh).is_some(), "{what}");
            assert!(store.activate(&p).is_none(), "{what}");
            assert!(store.trust(&p).is_err(), "{what}");
            assert!(
                store.activate(&fresh).is_some(),
                "trust of fresh survives: {what}"
            );
        }
    }

    /// A file another process holds open exclusively cannot be hashed: fail closed, then recover.
    #[cfg(windows)]
    #[test]
    fn an_unreadable_file_fails_closed() {
        use std::os::windows::fs::OpenOptionsExt;
        let (d, p) = bundled();
        let mut store = Store::default();
        store.trust(&p).unwrap();
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(d.0.join("run.cmd"))
            .unwrap();
        assert_eq!(store.state(&p), State::Changed);
        assert!(store.activate(&p).is_none());
        assert!(store.trust(&p).is_err());
        drop(held);
        assert!(store.activate(&p).is_some());
    }

    #[test]
    fn a_trusted_plugin_that_vanishes_or_is_relinked_is_not_activated() {
        let (d, p) = bundled();
        let mut store = Store::default();
        store.trust(&p).unwrap();
        // An empty copy is not a match for the trusted folder.
        let moved = d.0.with_file_name(format!("moved-{}", Uuid::new_v4()));
        fs::rename(&d.0, &moved).unwrap();
        assert_eq!(store.state(&p), State::Changed);
        assert!(store.activate(&p).is_none());
        assert!(store.trust(&p).is_err());
        // The folder replaced by a link to identical content is not the same folder.
        let copy = Dir::new();
        for f in [
            "plugin.json",
            "mcp.json",
            "run.cmd",
            "skills/review/SKILL.md",
        ] {
            copy.write(f, &fs::read_to_string(moved.join(f)).unwrap());
        }
        assert!(link_dir(&d.0, &copy.0), "could not create a directory link");
        assert!(store.activate(&p).is_none());
        assert!(store.trust(&p).is_err());
        unlink(&d.0);
        fs::rename(&moved, &d.0).unwrap();
        assert!(
            store.activate(&p).is_some(),
            "moving it back restores trust"
        );
    }

    #[test]
    fn a_link_retargeted_after_loading_is_not_activated() {
        let d = Dir::new();
        d.write("plugin.json", &manifest(""));
        d.write("impl-a/tool.cmd", "a");
        d.write("impl-b/tool.cmd", "b");
        assert!(link_dir(&d.0.join("bin"), &d.0.join("impl-a")));
        let p = plugin(&d);
        let mut store = Store::default();
        store.trust(&p).unwrap();
        unlink(&d.0.join("bin"));
        assert!(link_dir(&d.0.join("bin"), &d.0.join("impl-b")));
        assert!(store.activate(&p).is_none());
        assert!(Store::default().trust(&p).is_err());
    }

    #[test]
    fn a_copy_of_a_trusted_plugin_is_not_trusted() {
        let (a, b) = (Dir::new(), Dir::new());
        for d in [&a, &b] {
            d.write("plugin.json", &manifest(""));
        }
        let mut store = Store::default();
        store.trust(&plugin(&a)).unwrap();
        assert_eq!(store.state(&plugin(&a)), State::Trusted);
        assert_eq!(store.state(&plugin(&b)), State::Untrusted);
    }

    #[test]
    fn trust_survives_a_restart_and_a_bad_file_trusts_nothing() {
        let d = Dir::new();
        d.write("plugin.json", &manifest(""));
        let file =
            d.0.join("..")
                .join(format!("plugins-{}.json", Uuid::new_v4()));
        let mut store = Store::load(&file);
        assert!(store.warning().is_none());
        store.trust(&plugin(&d)).unwrap();
        store.save(&file).unwrap();
        assert_eq!(Store::load(&file).state(&plugin(&d)), State::Trusted);

        fs::write(&file, "{ nope").unwrap();
        let broken = Store::load(&file);
        assert_eq!(broken.state(&plugin(&d)), State::Untrusted);
        assert!(broken.warning().is_some());
        assert!(broken.save(&file).is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), "{ nope");

        fs::write(&file, r#"{"version":99,"grants":{}}"#).unwrap();
        assert!(Store::load(&file).warning().unwrap().contains("newer"));
        let _ = fs::remove_file(&file);
    }

    #[test]
    fn command_inspects_trusts_and_revokes() {
        let d = Dir::new();
        d.write("plugin.json", &manifest(""));
        skill(&d, "review");
        let file =
            d.0.join("..")
                .join(format!("plugins-{}.json", Uuid::new_v4()));
        let text = command(&d.0, &d.0, &file, false, false).unwrap();
        assert!(text.contains("not trusted") && text.contains("review"));
        let text = command(&d.0, &d.0, &file, true, false).unwrap();
        assert!(text.contains("state:  trusted"));
        assert!(command(&d.0, &d.0, &file, false, true)
            .unwrap()
            .contains("revoked"));
        let text = command(&d.0, &d.0, &file, false, false).unwrap();
        assert!(text.contains("not trusted"));
        fs::remove_file(d.0.join("plugin.json")).unwrap();
        assert!(command(&d.0, &d.0, &file, true, false).is_err());
        let _ = fs::remove_file(&file);
    }

    #[test]
    fn the_example_plugin_loads_cleanly() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/plugins/code-review");
        let p = load(&dir, &std::env::temp_dir()).unwrap();
        assert!(p.issues.is_empty(), "{:?}", p.issues);
        assert_eq!(p.manifest.name, "code-review");
        assert_eq!((p.skills.len(), p.mcp.len()), (1, 1));
        assert_eq!(p.skills[0].references.len(), 1);
    }

    #[test]
    fn git_folder_and_link_targets_are_part_of_the_fingerprint() {
        let a = Dir::new();
        a.write("plugin.json", &manifest(""));
        a.write(".git/x.cmd", "one");
        let before = plugin(&a).fingerprint;
        a.write(".git/x.cmd", "two");
        assert_ne!(plugin(&a).fingerprint, before);

        let d = Dir::new();
        d.write("plugin.json", &manifest(""));
        d.write("impl-a/tool.cmd", "a");
        d.write("impl-b/tool.cmd", "b");
        assert!(link_dir(&d.0.join("bin"), &d.0.join("impl-a")));
        let mut store = Store::default();
        store.trust(&plugin(&d)).unwrap();
        assert_eq!(store.state(&plugin(&d)), State::Trusted);
        unlink(&d.0.join("bin"));
        assert!(link_dir(&d.0.join("bin"), &d.0.join("impl-b")));
        assert_eq!(store.state(&plugin(&d)), State::Changed);
    }

    #[test]
    fn report_shows_what_trust_grants_without_letting_it_hide_anything() {
        let d = Dir::new();
        d.write("plugin.json", &manifest(r#","version":"1\u001b[2K""#));
        d.write(
            "mcp.json",
            &mcp(
                r#"{"s":{"type":"stdio","command":"npx","args":["a b","\u001b[2Khidden","c"],
                    "env":{"PATH":"${PLUGIN_ROOT}/bin","NODE_OPTIONS":"--require x"},"cwd":"./"},
                    "h":{"type":"streamable-http","url":"https://x.com","headers":{"X-Key":"v"}}}"#,
            ),
        );
        let p = plugin(&d);
        let text = report(&p, State::Untrusted);
        assert!(!text.contains('\u{1b}'), "{text:?}");
        assert!(text.contains(r#""a b", "\u{1b}[2Khidden", "c""#), "{text}");
        assert!(text.contains("NODE_OPTIONS") && text.contains("--require x"));
        assert!(text.contains("PATH") && text.contains("X-Key"));
    }

    #[test]
    fn terminal_escapes_in_plugin_text_are_not_echoed() {
        let d = Dir::new();
        d.write("plugin.json", &manifest(r#","hostile\u001b[2J":1"#));
        let p = plugin(&d);
        assert!(p.issues.iter().all(|i| !i.contains('\u{1b}')));
        assert!(!report(&p, State::Untrusted).contains('\u{1b}'));
    }
}
