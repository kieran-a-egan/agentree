//! Evidence handoffs between stages: compact, source-referenced, hash-keyed.
//!
//! A bulk reader returns claims, each pointing at source ranges and the hash of the content it
//! read. A claim is reused only while that hash still matches the file, so a cache can never
//! serve evidence for text that has changed, and a repository is not scanned twice.

use super::{Code, Violation};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const HANDOFF_VERSION: u32 = 1;
/// A claim is a pointer to evidence, not a copy of the source.
pub const MAX_CLAIM_CHARS: usize = 400;
pub const MAX_EVIDENCE: usize = 64;

/// SHA-256 of `bytes` as lowercase hex. The `source_hash` of a [`SourceRef`].
pub fn source_hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRef {
    /// Workspace-relative, `/`-separated.
    pub path: String,
    pub start_line: Option<u32>,
    pub end_line: Option<u32>,
    /// Hash of the whole file as it was read.
    pub source_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub claim: String,
    pub refs: Vec<SourceRef>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkInfo {
    pub index: u32,
    pub total: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handoff {
    pub version: u32,
    pub from_stage: String,
    pub to_stage: String,
    pub evidence: Vec<Evidence>,
    #[serde(default)]
    pub open_questions: Vec<String>,
    #[serde(default)]
    pub chunk: Option<ChunkInfo>,
}

pub(crate) fn relative(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.starts_with('\\')
        && !path.contains(':')
        && !path.split(['/', '\\']).any(|c| c == "..")
}

fn is_hash(h: &str) -> bool {
    h.len() == 64
        && h.chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

impl Handoff {
    pub fn validate(&self) -> Vec<Violation> {
        let at = format!("handoff[{}→{}]", self.from_stage, self.to_stage);
        let mut out = Vec::new();
        let mut bad = |m: String| out.push(Violation::new(Code::Evidence, &at, m));
        if self.version != HANDOFF_VERSION {
            bad(format!("version {} is not supported", self.version));
        }
        if self.evidence.is_empty() && self.open_questions.is_empty() {
            bad("carries neither evidence nor open questions".into());
        }
        if self.evidence.len() > MAX_EVIDENCE {
            bad(format!(
                "has more than {MAX_EVIDENCE} items; summarize or chunk"
            ));
        }
        if let Some(c) = self.chunk {
            if c.total == 0 || c.index >= c.total {
                bad("chunk index is outside its total".into());
            }
        }
        for (n, e) in self.evidence.iter().enumerate() {
            if e.claim.trim().is_empty() || e.claim.chars().count() > MAX_CLAIM_CHARS {
                bad(format!(
                    "item {n}: claim must be 1 to {MAX_CLAIM_CHARS} characters"
                ));
            }
            if e.refs.is_empty() {
                bad(format!("item {n}: has no source reference"));
            }
            for r in &e.refs {
                if !relative(&r.path) {
                    bad(format!(
                        "item {n}: '{}' is not a workspace-relative path",
                        r.path
                    ));
                }
                if !is_hash(&r.source_hash) {
                    bad(format!("item {n}: '{}' has no valid source hash", r.path));
                }
                match (r.start_line, r.end_line) {
                    (None, None) => {}
                    (Some(s), Some(e)) if s >= 1 && s <= e => {}
                    _ => bad(format!("item {n}: '{}' has an invalid line range", r.path)),
                }
            }
        }
        out
    }
}

/// Evidence by file, valid only while the file still hashes to `source_hash`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EvidenceCache {
    entries: BTreeMap<String, (String, Vec<Evidence>)>,
}

/// One file the caller is about to read: its path, current hash and size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileState {
    pub path: String,
    pub source_hash: String,
    pub tokens: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanPlan {
    /// Paths whose cached evidence is still valid.
    pub reuse: Vec<String>,
    /// Paths that must be read, because they are new or their hash changed.
    pub scan: Vec<String>,
    pub scan_tokens: u32,
}

impl EvidenceCache {
    pub fn insert(&mut self, path: &str, source_hash: &str, evidence: Vec<Evidence>) {
        self.entries
            .insert(path.into(), (source_hash.into(), evidence));
    }

    /// Cached evidence for `path`, only if it was read at `current_hash`.
    pub fn get(&self, path: &str, current_hash: &str) -> Option<&[Evidence]> {
        self.entries
            .get(path)
            .filter(|(h, _)| h == current_hash)
            .map(|(_, e)| e.as_slice())
    }

    /// Drops every entry that no longer matches `current` (path to hash), including deleted
    /// files. Returns how many were dropped.
    pub fn invalidate(&mut self, current: &BTreeMap<String, String>) -> usize {
        let before = self.entries.len();
        self.entries
            .retain(|path, (hash, _)| current.get(path).is_some_and(|c| c == hash));
        before - self.entries.len()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Splits `files` into what can be reused and what must be read. Each file appears once,
    /// so the same file is never queued twice.
    pub fn plan(&self, files: &[FileState]) -> ScanPlan {
        let mut plan = ScanPlan::default();
        let mut seen = std::collections::BTreeSet::new();
        for f in files.iter().filter(|f| seen.insert(f.path.as_str())) {
            if self.get(&f.path, &f.source_hash).is_some() {
                plan.reuse.push(f.path.clone());
            } else {
                plan.scan.push(f.path.clone());
                plan.scan_tokens = plan.scan_tokens.saturating_add(f.tokens);
            }
        }
        plan
    }
}

/// A file, or one part of an oversized file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slice {
    pub path: String,
    pub part: u32,
    pub parts: u32,
    pub tokens: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChunkError {
    /// The window is zero, so nothing can be read.
    NoWindow,
    /// More windows would be needed than the bound allows.
    TooMany { needed: u32, max: u32 },
}

/// Packs `files` into windows of at most `window` tokens, splitting a file larger than a
/// window into parts. At most `max_chunks` windows; more is an error, never a silent truncation.
pub fn chunk(
    files: &[(String, u32)],
    window: u32,
    max_chunks: u32,
) -> Result<Vec<Vec<Slice>>, ChunkError> {
    if window == 0 {
        return Err(ChunkError::NoWindow);
    }
    let mut chunks: Vec<Vec<Slice>> = Vec::new();
    let mut used = 0u32;
    let mut place = |slice: Slice, chunks: &mut Vec<Vec<Slice>>| {
        if chunks.is_empty() || used + slice.tokens > window {
            chunks.push(Vec::new());
            used = 0;
        }
        used += slice.tokens;
        chunks
            .last_mut()
            .expect("a chunk was just pushed")
            .push(slice);
    };
    for (path, tokens) in files {
        let parts = tokens.div_ceil(window).max(1);
        for part in 0..parts {
            let t = if part + 1 == parts {
                tokens - window * (parts - 1)
            } else {
                window
            };
            place(
                Slice {
                    path: path.clone(),
                    part,
                    parts,
                    tokens: t,
                },
                &mut chunks,
            );
        }
    }
    let needed = chunks.len() as u32;
    if needed > max_chunks {
        return Err(ChunkError::TooMany {
            needed,
            max: max_chunks,
        });
    }
    Ok(chunks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sref(path: &str, text: &str) -> SourceRef {
        SourceRef {
            path: path.into(),
            start_line: Some(1),
            end_line: Some(3),
            source_hash: source_hash(text.as_bytes()),
        }
    }

    fn handoff(evidence: Vec<Evidence>) -> Handoff {
        Handoff {
            version: HANDOFF_VERSION,
            from_stage: "explore".into(),
            to_stage: "plan".into(),
            evidence,
            open_questions: vec![],
            chunk: None,
        }
    }

    #[test]
    fn hashes_are_sha256_hex() {
        assert_eq!(
            source_hash(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn handoffs_need_compact_source_referenced_claims() {
        let good = handoff(vec![Evidence {
            claim: "parse() rejects unknown schema".into(),
            refs: vec![sref("src/a.rs", "x")],
        }]);
        assert!(good.validate().is_empty());

        let mut bad = good.clone();
        bad.evidence[0].refs.clear();
        bad.evidence[0].claim = "x".repeat(MAX_CLAIM_CHARS + 1);
        assert_eq!(bad.validate().len(), 2);

        let mut bad = good.clone();
        bad.evidence[0].refs[0].path = "../secret".into();
        bad.evidence[0].refs[0].source_hash = "nope".into();
        bad.evidence[0].refs[0].end_line = Some(0);
        assert_eq!(bad.validate().len(), 3);

        assert!(!handoff(vec![]).validate().is_empty());
    }

    #[test]
    fn cache_serves_evidence_only_while_the_source_hash_matches() {
        let mut cache = EvidenceCache::default();
        let ev = vec![Evidence {
            claim: "c".into(),
            refs: vec![sref("a.rs", "v1")],
        }];
        cache.insert("a.rs", &source_hash(b"v1"), ev);
        assert!(cache.get("a.rs", &source_hash(b"v1")).is_some());
        assert!(cache.get("a.rs", &source_hash(b"v2")).is_none());

        let mut current = BTreeMap::new();
        current.insert("a.rs".to_string(), source_hash(b"v2"));
        assert_eq!(cache.invalidate(&current), 1);
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn a_scan_plan_reads_each_changed_file_once_and_reuses_the_rest() {
        let mut cache = EvidenceCache::default();
        cache.insert("same.rs", &source_hash(b"1"), vec![]);
        cache.insert("edited.rs", &source_hash(b"old"), vec![]);
        let file = |p: &str, t: &[u8], tokens| FileState {
            path: p.into(),
            source_hash: source_hash(t),
            tokens,
        };
        let plan = cache.plan(&[
            file("same.rs", b"1", 100),
            file("edited.rs", b"new", 50),
            file("edited.rs", b"new", 50),
            file("fresh.rs", b"z", 25),
        ]);
        assert_eq!(plan.reuse, ["same.rs"]);
        assert_eq!(plan.scan, ["edited.rs", "fresh.rs"]);
        assert_eq!(plan.scan_tokens, 75);
    }

    #[test]
    fn chunking_is_bounded_and_splits_oversized_files() {
        let files = vec![
            ("a".to_string(), 60),
            ("b".to_string(), 60),
            ("big".to_string(), 250),
        ];
        let chunks = chunk(&files, 100, 10).unwrap();
        let total: u32 = chunks.iter().flatten().map(|s| s.tokens).sum();
        assert_eq!(total, 370);
        assert!(chunks
            .iter()
            .all(|c| c.iter().map(|s| s.tokens).sum::<u32>() <= 100));
        assert_eq!(
            chunks.iter().flatten().filter(|s| s.path == "big").count(),
            3
        );
        assert_eq!(
            chunk(&files, 100, 2).unwrap_err(),
            ChunkError::TooMany { needed: 5, max: 2 }
        );
        assert_eq!(chunk(&files, 0, 2).unwrap_err(), ChunkError::NoWindow);
    }
}
