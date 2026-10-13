//! Resolves the project instruction files a run sees: shared `AGENTS.md`, the backend's native
//! file (`CLAUDE.md` for Claude Code), and other tools' files. Read-only: nothing is written.
//!
//! Precedence for a Claude run, lowest to highest: `AGENTS.md`, then `CLAUDE.md`. Claude Code
//! loads `CLAUDE.md` itself and never reads `AGENTS.md`, so the lead is told about `AGENTS.md`.
//! Only the project folder is resolved; nested or user-level files are Claude Code's own.

use std::fs;
use std::path::{Path, PathBuf};

const SHARED: &str = "AGENTS.md";
const NATIVE: &str = "CLAUDE.md";
/// Other tools' instruction files. Listed for provenance, never merged or edited.
const OTHERS: [&str; 3] = [
    "GEMINI.md",
    ".github/copilot-instructions.md",
    ".cursorrules",
];

#[derive(Debug, PartialEq)]
pub enum Relation {
    Neither,
    SharedOnly,
    NativeOnly,
    /// Same file through a symlink, so there is nothing to reconcile.
    SameFile,
    Identical,
    /// `CLAUDE.md` pulls `AGENTS.md` in with an `@AGENTS.md` import.
    Imports,
    /// Both exist and differ: number of lines found in only one of them.
    Conflict {
        only_shared: usize,
        only_native: usize,
    },
    /// One of the two exists but could not be read, so no comparison was made.
    Unreadable,
}

#[derive(Debug)]
pub struct Resolved {
    pub dir: PathBuf,
    pub shared: Option<PathBuf>,
    pub native: Option<PathBuf>,
    pub others: Vec<PathBuf>,
    pub relation: Relation,
    /// Files that exist but could not be read, with the reason.
    pub problems: Vec<String>,
}

pub fn resolve(dir: &Path) -> Resolved {
    let found = |name: &str| Some(dir.join(name)).filter(|p| p.is_file());
    let (shared, native) = (found(SHARED), found(NATIVE));
    let others = OTHERS.iter().filter_map(|n| found(n)).collect();
    let mut problems = Vec::new();
    let relation = match (&shared, &native) {
        (None, None) => Relation::Neither,
        (Some(_), None) => Relation::SharedOnly,
        (None, Some(_)) => Relation::NativeOnly,
        (Some(s), Some(n)) => compare(s, n, &mut problems),
    };
    Resolved {
        dir: dir.to_path_buf(),
        shared,
        native,
        others,
        relation,
        problems,
    }
}

fn compare(shared: &Path, native: &Path, problems: &mut Vec<String>) -> Relation {
    if let (Ok(a), Ok(b)) = (fs::canonicalize(shared), fs::canonicalize(native)) {
        if a == b {
            return Relation::SameFile;
        }
    }
    let mut read = |p: &Path| match fs::read_to_string(p) {
        Ok(text) => text.replace("\r\n", "\n"),
        Err(e) => {
            problems.push(format!("{}: {e}", p.display()));
            String::new()
        }
    };
    let (s, n) = (read(shared), read(native));
    if !problems.is_empty() {
        return Relation::Unreadable;
    }
    if s.trim() == n.trim() {
        return Relation::Identical;
    }
    // Only an import on a line of its own counts; prose or a longer name does not.
    if n.lines()
        .any(|l| matches!(l.trim(), "@AGENTS.md" | "@./AGENTS.md"))
    {
        return Relation::Imports;
    }
    let lines = |t: &str| -> Vec<String> {
        t.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect()
    };
    let (ls, ln) = (lines(&s), lines(&n));
    Relation::Conflict {
        only_shared: ls.iter().filter(|l| !ln.contains(l)).count(),
        only_native: ln.iter().filter(|l| !ls.contains(l)).count(),
    }
}

impl Resolved {
    /// Human-readable provenance and, when the files disagree, why and which one wins.
    pub fn render(&self) -> String {
        let name = |p: &Option<PathBuf>| match p {
            Some(p) => p.display().to_string(),
            None => "(none)".into(),
        };
        let mut out = format!(
            "Project instructions for {}\n  shared  {SHARED}: {}\n  claude  {NATIVE}: {}\n",
            self.dir.display(),
            name(&self.shared),
            name(&self.native)
        );
        for other in &self.others {
            out.push_str(&format!(
                "  other   {} (present; not used by the Claude backend)\n",
                other.strip_prefix(&self.dir).unwrap_or(other).display()
            ));
        }
        out.push_str(&format!("Result: {}\n", self.explain()));
        for problem in &self.problems {
            out.push_str(&format!("Problem: {problem}\n"));
        }
        out.push_str("No instruction file is modified by Agentree.\n");
        out
    }

    fn explain(&self) -> String {
        match &self.relation {
            Relation::Neither => "no AGENTS.md or CLAUDE.md; the run starts without project instructions.".into(),
            Relation::SharedOnly => "only AGENTS.md exists. Agentree does not rely on Claude Code loading it, so the lead is told to read it first.".into(),
            Relation::NativeOnly => "only CLAUDE.md exists; Claude Code loads it natively.".into(),
            Relation::SameFile => "AGENTS.md and CLAUDE.md are the same file; nothing to reconcile.".into(),
            Relation::Identical => "AGENTS.md and CLAUDE.md have identical content; Claude Code loads CLAUDE.md.".into(),
            Relation::Imports => "CLAUDE.md imports AGENTS.md (@AGENTS.md); Claude Code loads both.".into(),
            Relation::Unreadable => "could not compare the files because one is unreadable.".into(),
            Relation::Conflict { only_shared, only_native } => format!(
                "AGENTS.md and CLAUDE.md differ ({only_shared} line(s) only in AGENTS.md, {only_native} only in CLAUDE.md). \
                 Claude Code loads CLAUDE.md; the lead is told to read AGENTS.md too and to follow CLAUDE.md where they conflict. \
                 To remove the ambiguity, add `@AGENTS.md` to CLAUDE.md or make one the single source."
            ),
        }
    }

    /// One line for the launch status bar; `None` when the instructions need no attention.
    pub fn status_line(&self) -> Option<String> {
        matches!(
            self.relation,
            Relation::SharedOnly | Relation::Conflict { .. } | Relation::Unreadable
        )
        .then(|| format!("Instructions: {}", self.explain()))
    }

    /// Text appended to the lead's standing instructions. `None` when Claude Code already has
    /// everything (the unchanged native-team path).
    pub fn lead_note(&self) -> Option<String> {
        let shared = self
            .shared
            .as_ref()?
            .display()
            .to_string()
            .replace('\\', "/");
        match &self.relation {
            Relation::SharedOnly => Some(format!(
                "Project instructions: shared project instructions are in {shared}. Agentree does not rely on Claude Code loading AGENTS.md; read it before starting work and pass the relevant parts to your teammates. Do not edit or overwrite it unless the objective asks for that."
            )),
            Relation::Conflict { .. } | Relation::Unreadable => Some(format!(
                "Project instructions: {shared} (shared) and CLAUDE.md (loaded natively) differ or could not be compared. Read {shared} as well. Where they conflict, follow CLAUDE.md and tell the human about the conflict. Do not edit or overwrite either file unless the objective asks for that."
            )),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn project(files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agentree-instr-{}", Uuid::new_v4()));
        fs::create_dir_all(dir.join(".github")).unwrap();
        for (name, text) in files {
            fs::write(dir.join(name), text).unwrap();
        }
        dir
    }

    #[test]
    fn claude_only_project_is_unchanged() {
        let r = resolve(&project(&[("CLAUDE.md", "be nice")]));
        assert_eq!(r.relation, Relation::NativeOnly);
        assert!(r.lead_note().is_none());
        let r = resolve(&project(&[]));
        assert_eq!(r.relation, Relation::Neither);
        assert!(r.lead_note().is_none());
    }

    #[test]
    fn agents_md_alone_is_handed_to_the_lead() {
        let r = resolve(&project(&[("AGENTS.md", "use tabs")]));
        assert_eq!(r.relation, Relation::SharedOnly);
        assert!(r.lead_note().unwrap().contains("AGENTS.md"));
    }

    #[test]
    fn identical_and_imported_files_do_not_conflict() {
        let r = resolve(&project(&[
            ("AGENTS.md", "a\r\nb"),
            ("CLAUDE.md", "a\nb\n"),
        ]));
        assert_eq!(r.relation, Relation::Identical);
        let r = resolve(&project(&[
            ("AGENTS.md", "a"),
            ("CLAUDE.md", "@AGENTS.md\nmore"),
        ]));
        assert_eq!(r.relation, Relation::Imports);
        assert!(r.lead_note().is_none());
        // Mentioning the import in prose is not an import.
        let r = resolve(&project(&[
            ("AGENTS.md", "a"),
            ("CLAUDE.md", "never write @AGENTS.md here"),
        ]));
        assert!(matches!(r.relation, Relation::Conflict { .. }));
    }

    #[test]
    fn differing_files_explain_the_conflict_and_stay_untouched() {
        let dir = project(&[
            ("AGENTS.md", "use tabs\nrun tests"),
            ("CLAUDE.md", "use spaces\nrun tests"),
            ("GEMINI.md", "x"),
        ]);
        let r = resolve(&dir);
        assert_eq!(
            r.relation,
            Relation::Conflict {
                only_shared: 1,
                only_native: 1
            }
        );
        let text = r.render();
        assert!(text.contains("differ") && text.contains("follow CLAUDE.md"));
        assert!(text.contains("GEMINI.md"));
        assert!(r.lead_note().unwrap().contains("follow CLAUDE.md"));
        assert_eq!(
            fs::read_to_string(dir.join("AGENTS.md")).unwrap(),
            "use tabs\nrun tests"
        );
        assert_eq!(
            fs::read_to_string(dir.join("CLAUDE.md")).unwrap(),
            "use spaces\nrun tests"
        );
    }
}
