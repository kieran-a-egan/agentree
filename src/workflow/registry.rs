//! Model registry and the two mandatory selection policies.
//!
//! Selection reads only registry data (cost, intelligence, context, tools, permissions, data
//! classes, availability). No function here, or anywhere in routing, names a model: changing
//! the registry changes the outcome. Intelligence is an explicit rank and is never derived
//! from price.

use super::{Code, Violation};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// What a role is allowed to do to the workspace. `ReadOnly < Edit`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    ReadOnly,
    Edit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelEntry {
    pub id: String,
    pub provider: String,
    /// Short names a setting may use, such as the Claude Code advisor alias.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Explicit capability rank. Higher is stronger. Independent of `cost`.
    pub intelligence: u32,
    /// Relative cost in whole units. Lower is cheaper. Independent of `intelligence`.
    pub cost: u32,
    pub context_tokens: u32,
    #[serde(default)]
    pub tools: BTreeSet<String>,
    pub max_permission: Permission,
    /// Data classes this model may be shown (`repository`, `secrets`, ...).
    #[serde(default)]
    pub data_classes: BTreeSet<String>,
    /// Runtime availability. Set from discovery or health checks, never assumed by selection.
    pub available: bool,
}

impl ModelEntry {
    fn matches(&self, name: &str) -> bool {
        self.id == name || self.aliases.iter().any(|a| a == name)
    }

    /// Capability checks that do not depend on context size or availability.
    fn capable(&self, req: &Requirement) -> bool {
        req.tools.iter().all(|t| self.tools.contains(t))
            && self.max_permission >= req.permission
            && self.data_classes.contains(&req.data_class)
    }

    fn fits(&self, req: &Requirement) -> bool {
        self.context_tokens >= req.context_tokens
    }
}

fn current_version() -> u32 {
    super::VERSION
}

/// What a piece of work needs from the model that runs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Requirement {
    pub tools: BTreeSet<String>,
    pub context_tokens: u32,
    pub permission: Permission,
    pub data_class: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    /// Format version; must equal [`super::VERSION`].
    #[serde(default = "current_version")]
    pub version: u32,
    pub models: Vec<ModelEntry>,
    /// Explicit final-review order, strongest first. When non-empty it is also an allowlist:
    /// a model not listed is never a reviewer. When empty, `intelligence` ranks the reviewers.
    #[serde(default)]
    pub review_priority: Vec<String>,
}

/// A human decision to use one named model that policy would not pick by itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Authorization {
    pub model: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EscalationKind {
    /// The cheapest capable tier is configured but not available; try again later.
    RetryLater,
    /// Only a more expensive (bulk) or weaker (review) model could run; a human must say yes.
    NeedsAuthorization,
    /// No configured model satisfies the requirement at all.
    NoCapableModel,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Escalation {
    pub kind: EscalationKind,
    pub message: String,
    /// Model ids a human may authorize, best first. Never used without an `Authorization`.
    pub options: Vec<String>,
}

/// Bounds for splitting work that does not fit the cheapest model's context.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChunkPolicy {
    /// Tokens of each window kept free for instructions and the handoff.
    pub reserve_tokens: u32,
    pub max_chunks: u32,
}

impl Default for ChunkPolicy {
    fn default() -> Self {
        Self {
            reserve_tokens: 8_000,
            max_chunks: 8,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BulkSelection {
    /// The whole input fits the model.
    Whole { model: String, authorized: bool },
    /// The input is processed in `chunks` windows of `chunk_tokens` each. `chunks` is a lower
    /// bound: packing whole files with `evidence::chunk` can need more, and that is reported
    /// there as `TooMany` rather than being truncated.
    Chunked {
        model: String,
        chunk_tokens: u32,
        chunks: u32,
    },
}

impl BulkSelection {
    pub fn model(&self) -> &str {
        match self {
            BulkSelection::Whole { model, .. } | BulkSelection::Chunked { model, .. } => model,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewSelection {
    pub model: String,
    /// The strongest eligible model by policy. Equals `model` unless a human authorized another.
    pub strongest: String,
    pub authorized_downgrade: bool,
}

impl Registry {
    pub fn get(&self, name: &str) -> Option<&ModelEntry> {
        self.models.iter().find(|m| m.matches(name))
    }

    pub fn set_available(&mut self, name: &str, available: bool) -> bool {
        match self.models.iter_mut().find(|m| m.matches(name)) {
            Some(m) => {
                m.available = available;
                true
            }
            None => false,
        }
    }

    pub fn validate(&self) -> Vec<Violation> {
        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        if self.version != super::VERSION {
            out.push(Violation::new(
                Code::Schema,
                "version",
                format!("registry version {} is not supported", self.version),
            ));
        }
        if self.models.is_empty() {
            out.push(Violation::new(
                Code::Registry,
                "models",
                "the registry lists no models",
            ));
        }
        for m in &self.models {
            let at = format!("models[{}]", m.id);
            if m.id.is_empty() || m.context_tokens == 0 {
                out.push(Violation::new(
                    Code::Registry,
                    &at,
                    "needs an id and a context size",
                ));
            }
            for name in std::iter::once(&m.id).chain(&m.aliases) {
                if !seen.insert(name.clone()) {
                    out.push(Violation::new(
                        Code::Duplicate,
                        &at,
                        format!("'{name}' names more than one model"),
                    ));
                }
            }
        }
        for name in &self.review_priority {
            if self.get(name).is_none() {
                out.push(Violation::new(
                    Code::Registry,
                    "review_priority",
                    format!("'{name}' is not in the registry"),
                ));
            }
        }
        out
    }

    /// Reviewer candidates, strongest first.
    fn review_order(&self) -> Vec<&ModelEntry> {
        if self.review_priority.is_empty() {
            let mut all: Vec<_> = self.models.iter().collect();
            all.sort_by(|a, b| b.intelligence.cmp(&a.intelligence).then(a.id.cmp(&b.id)));
            all
        } else {
            self.review_priority
                .iter()
                .filter_map(|n| self.get(n))
                .collect()
        }
    }

    /// Whether any configured model could ever run bulk work with this requirement, ignoring
    /// availability and context. Validation uses it to reject a workflow before execution.
    pub fn any_capable(&self, req: &Requirement) -> bool {
        self.models.iter().any(|m| m.capable(req))
    }

    /// Whether any configured reviewer could ever perform a review with this requirement.
    pub fn any_reviewer(&self, req: &Requirement) -> bool {
        self.review_order().iter().any(|m| m.capable(req))
    }

    /// Whether `name` is in the lowest-cost tier of the configured capable models.
    pub fn is_lowest_cost(&self, name: &str, req: &Requirement) -> bool {
        let floor = self
            .models
            .iter()
            .filter(|m| m.capable(req))
            .map(|m| m.cost)
            .min();
        self.get(name)
            .is_some_and(|m| m.capable(req) && Some(m.cost) == floor)
    }

    /// The configured model that policy would use for review, available or not.
    pub fn strongest_reviewer(&self, req: &Requirement) -> Option<&ModelEntry> {
        self.review_order()
            .into_iter()
            .find(|m| m.capable(req) && m.fits(req))
    }

    /// Bulk-context work goes to the cheapest capable model. The cheapest tier is the lowest
    /// cost among *configured* capable models, not only the available ones, so an outage of
    /// the cheap model is an escalation and never a silent move to a pricier one.
    pub fn select_bulk(
        &self,
        req: &Requirement,
        chunking: Option<ChunkPolicy>,
        auth: Option<&Authorization>,
    ) -> Result<BulkSelection, Escalation> {
        let capable: Vec<&ModelEntry> = self.models.iter().filter(|m| m.capable(req)).collect();
        let Some(floor) = capable.iter().map(|m| m.cost).min() else {
            return Err(Escalation {
                kind: EscalationKind::NoCapableModel,
                message: "No configured model has the tools, permissions and data access this \
                          bulk-reading work needs."
                    .into(),
                options: vec![],
            });
        };
        let tier: Vec<&ModelEntry> = capable
            .iter()
            .copied()
            .filter(|m| m.cost == floor && m.available)
            .collect();
        // Cheapest tier, whole input fits with room for the instructions: the normal case.
        let reserve = chunking.map_or(0, |p| p.reserve_tokens);
        let needed = Requirement {
            context_tokens: req.context_tokens.saturating_add(reserve),
            ..req.clone()
        };
        if let Some(m) = tier
            .iter()
            .filter(|m| m.fits(&needed))
            .min_by(|a, b| a.id.cmp(&b.id))
        {
            return Ok(BulkSelection::Whole {
                model: m.id.clone(),
                authorized: false,
            });
        }
        // Cheapest tier is available but too small: chunk within the bounds.
        if let (Some(policy), Some(m)) = (
            chunking,
            tier.iter().max_by(|a, b| {
                a.context_tokens
                    .cmp(&b.context_tokens)
                    .then(b.id.cmp(&a.id))
            }),
        ) {
            let window = m.context_tokens.saturating_sub(policy.reserve_tokens);
            if window > 0 {
                let chunks = req.context_tokens.div_ceil(window);
                if chunks <= policy.max_chunks {
                    return Ok(BulkSelection::Chunked {
                        model: m.id.clone(),
                        chunk_tokens: window,
                        chunks,
                    });
                }
            }
        }
        // Anything else is a pricier model. Only an explicit human choice may use one.
        let mut options: Vec<&ModelEntry> = capable
            .iter()
            .copied()
            .filter(|m| m.cost > floor && m.available && m.fits(&needed))
            .collect();
        options.sort_by(|a, b| a.cost.cmp(&b.cost).then(a.id.cmp(&b.id)));
        if let Some(a) = auth {
            if let Some(m) = options.iter().find(|m| m.matches(&a.model)) {
                return Ok(BulkSelection::Whole {
                    model: m.id.clone(),
                    authorized: true,
                });
            }
        }
        if options.is_empty() {
            let (kind, message) = if tier.is_empty() {
                (
                    EscalationKind::RetryLater,
                    "The lowest-cost capable model is configured but unavailable, and no other                      capable model is available. Retry later.",
                )
            } else {
                (
                    EscalationKind::NoCapableModel,
                    "No available model can read this much context within the chunk limits.",
                )
            };
            return Err(Escalation {
                kind,
                message: message.into(),
                options: vec![],
            });
        }
        Err(Escalation {
            kind: EscalationKind::NeedsAuthorization,
            message: "The lowest-cost capable model cannot complete this bulk-reading work. A \
                      more expensive model needs your explicit authorization."
                .into(),
            options: options.iter().map(|m| m.id.clone()).collect(),
        })
    }

    /// The final reviewer is the strongest *eligible* configured model. If it is unavailable
    /// the outcome is an escalation: there is no automatic fallback to a weaker model.
    pub fn select_review(
        &self,
        req: &Requirement,
        auth: Option<&Authorization>,
    ) -> Result<ReviewSelection, Escalation> {
        let order = self.review_order();
        let eligible: Vec<&ModelEntry> = order
            .into_iter()
            .filter(|m| m.capable(req) && m.fits(req))
            .collect();
        let Some(strongest) = eligible.first() else {
            return Err(Escalation {
                kind: EscalationKind::NoCapableModel,
                message: "No configured model can perform the independent final review.".into(),
                options: vec![],
            });
        };
        if strongest.available {
            return Ok(ReviewSelection {
                model: strongest.id.clone(),
                strongest: strongest.id.clone(),
                authorized_downgrade: false,
            });
        }
        let fallbacks: Vec<&ModelEntry> = eligible
            .iter()
            .skip(1)
            .copied()
            .filter(|m| m.available)
            .collect();
        if let Some(m) = auth.and_then(|a| fallbacks.iter().find(|m| m.matches(&a.model))) {
            return Ok(ReviewSelection {
                model: m.id.clone(),
                strongest: strongest.id.clone(),
                authorized_downgrade: true,
            });
        }
        Err(Escalation {
            kind: EscalationKind::NeedsAuthorization,
            message: format!(
                "The strongest eligible reviewer '{}' is unavailable. Final review is blocked \
                 until you authorize a named weaker reviewer or the model returns.",
                strongest.id
            ),
            options: fallbacks.iter().map(|m| m.id.clone()).collect(),
        })
    }
}

/// The Claude Code models Agentree can launch, ranked by declared intelligence and cost.
/// The numbers are defaults a user may edit; no selection code reads a model name.
pub fn claude_default() -> Registry {
    use crate::roles::MODELS;
    let entry = |id: &str, alias: &str, intelligence, cost| ModelEntry {
        id: id.into(),
        provider: "claude".into(),
        aliases: vec![alias.into()],
        intelligence,
        cost,
        context_tokens: 200_000,
        tools: ["read", "search", "shell", "edit", "web", "mcp"]
            .into_iter()
            .map(String::from)
            .collect(),
        max_permission: Permission::Edit,
        data_classes: ["repository".to_string()].into(),
        available: true,
    };
    Registry {
        version: super::VERSION,
        models: vec![
            entry(MODELS[0], "haiku", 1, 1),
            entry(MODELS[1], "sonnet", 2, 3),
            entry(MODELS[2], "opus", 3, 5),
        ],
        review_priority: vec![],
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    pub fn entry(id: &str, intelligence: u32, cost: u32, context: u32) -> ModelEntry {
        ModelEntry {
            id: id.into(),
            provider: "test".into(),
            aliases: vec![],
            intelligence,
            cost,
            context_tokens: context,
            tools: ["read", "search", "shell", "edit"]
                .into_iter()
                .map(String::from)
                .collect(),
            max_permission: Permission::Edit,
            data_classes: ["repository".to_string()].into(),
            available: true,
        }
    }

    pub fn req(tokens: u32) -> Requirement {
        Requirement {
            tools: ["read".to_string(), "search".to_string()].into(),
            context_tokens: tokens,
            permission: Permission::ReadOnly,
            data_class: "repository".into(),
        }
    }

    /// Deliberately not named after any real model, and ordered so that cost and intelligence
    /// disagree: "mid" is the priciest, "top" the strongest.
    pub fn registry() -> Registry {
        Registry {
            version: crate::workflow::VERSION,
            models: vec![
                entry("cheap", 1, 1, 100_000),
                entry("mid", 2, 9, 200_000),
                entry("top", 3, 5, 200_000),
            ],
            review_priority: vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    #[test]
    fn bulk_reading_picks_the_cheapest_capable_model_by_registry_data() {
        let reg = registry();
        let sel = reg.select_bulk(&req(10_000), None, None).unwrap();
        assert_eq!(sel.model(), "cheap");
        // Make "mid" the cheapest: the choice follows the registry, with no name in the logic.
        let mut reg = reg;
        reg.models[1].cost = 0;
        assert_eq!(
            reg.select_bulk(&req(10_000), None, None).unwrap().model(),
            "mid"
        );
    }

    #[test]
    fn an_unavailable_cheap_model_is_never_silently_replaced_by_a_pricier_one() {
        let mut reg = registry();
        reg.set_available("cheap", false);
        let err = reg.select_bulk(&req(10_000), None, None).unwrap_err();
        assert_eq!(err.kind, EscalationKind::NeedsAuthorization);
        assert_eq!(err.options, vec!["top".to_string(), "mid".to_string()]);
        // Only an explicit, named authorization may use a pricier model.
        let ok = reg
            .select_bulk(
                &req(10_000),
                None,
                Some(&Authorization {
                    model: "top".into(),
                }),
            )
            .unwrap();
        assert!(matches!(
            ok,
            BulkSelection::Whole {
                authorized: true,
                ..
            }
        ));
        // With nothing else available the outcome is to retry, never to guess.
        reg.set_available("top", false);
        reg.set_available("mid", false);
        let err = reg.select_bulk(&req(10_000), None, None).unwrap_err();
        assert_eq!(err.kind, EscalationKind::RetryLater);
    }

    #[test]
    fn oversized_input_chunks_within_bounds_otherwise_asks_for_authorization() {
        let reg = registry();
        let policy = ChunkPolicy {
            reserve_tokens: 10_000,
            max_chunks: 4,
        };
        let sel = reg.select_bulk(&req(250_000), Some(policy), None).unwrap();
        assert_eq!(
            sel,
            BulkSelection::Chunked {
                model: "cheap".into(),
                chunk_tokens: 90_000,
                chunks: 3
            }
        );
        // 1M tokens exceeds 4 chunks and every model's window: nothing may run.
        let err = reg
            .select_bulk(&req(1_000_000), Some(policy), None)
            .unwrap_err();
        assert_eq!(err.kind, EscalationKind::NoCapableModel);
        // 150k tokens, no chunking allowed: a pricier model is only an option for a human.
        let err = reg.select_bulk(&req(150_000), None, None).unwrap_err();
        assert_eq!(err.kind, EscalationKind::NeedsAuthorization);
        assert_eq!(err.options, vec!["top".to_string(), "mid".to_string()]);
        let sel = reg
            .select_bulk(
                &req(150_000),
                None,
                Some(&Authorization {
                    model: "top".into(),
                }),
            )
            .unwrap();
        assert_eq!(
            sel,
            BulkSelection::Whole {
                model: "top".into(),
                authorized: true
            }
        );
    }

    #[test]
    fn tools_permissions_and_data_access_restrict_eligibility() {
        let mut reg = registry();
        reg.models[0].data_classes.clear();
        assert_ne!(
            reg.select_bulk(&req(1), None, None).unwrap().model(),
            "cheap"
        );
        // "cheap" is no longer capable, so the cheapest tier is now what remains.
        assert_eq!(reg.select_bulk(&req(1), None, None).unwrap().model(), "top");
        let mut r = req(1);
        r.tools.insert("web".into());
        assert_eq!(
            reg.select_bulk(&r, None, None).unwrap_err().kind,
            EscalationKind::NoCapableModel
        );
    }

    #[test]
    fn review_uses_the_strongest_model_even_though_it_is_not_the_priciest() {
        let reg = registry();
        let sel = reg.select_review(&req(1_000), None).unwrap();
        assert_eq!(sel.model, "top");
        assert!(!sel.authorized_downgrade);
    }

    #[test]
    fn review_priority_overrides_intelligence_and_is_an_allowlist() {
        let mut reg = registry();
        reg.review_priority = vec!["mid".into()];
        assert_eq!(reg.select_review(&req(1), None).unwrap().model, "mid");
        reg.set_available("mid", false);
        let err = reg.select_review(&req(1), None).unwrap_err();
        // "top" is stronger but not on the allowlist, so it is not even offered.
        assert_eq!(err.kind, EscalationKind::NeedsAuthorization);
        assert!(err.options.is_empty());
    }

    #[test]
    fn unavailable_strongest_reviewer_blocks_and_never_downgrades() {
        let mut reg = registry();
        reg.set_available("top", false);
        let err = reg.select_review(&req(1_000), None).unwrap_err();
        assert_eq!(err.kind, EscalationKind::NeedsAuthorization);
        assert!(err.message.contains("top"));
        assert_eq!(err.options, vec!["mid".to_string(), "cheap".to_string()]);
        // Only an explicit, named authorization may use a weaker reviewer.
        let sel = reg
            .select_review(
                &req(1_000),
                Some(&Authorization {
                    model: "mid".into(),
                }),
            )
            .unwrap();
        assert!(sel.authorized_downgrade);
        assert_eq!(sel.strongest, "top");
        // An authorization for the unavailable model itself changes nothing.
        assert!(reg
            .select_review(
                &req(1_000),
                Some(&Authorization {
                    model: "top".into()
                })
            )
            .is_err());
    }

    #[test]
    fn a_new_stronger_model_becomes_the_reviewer_with_no_code_change() {
        let mut reg = registry();
        reg.models.push(entry("future", 10, 20, 500_000));
        assert_eq!(reg.select_review(&req(1), None).unwrap().model, "future");
        assert_eq!(
            reg.select_bulk(&req(1), None, None).unwrap().model(),
            "cheap"
        );
    }

    #[test]
    fn registry_validation_rejects_duplicates_and_unknown_priorities() {
        let mut reg = registry();
        reg.models[1].aliases.push("cheap".into());
        reg.review_priority = vec!["ghost".into()];
        reg.version = 99;
        let codes: Vec<_> = reg.validate().iter().map(|v| v.code).collect();
        assert!(codes.contains(&Code::Duplicate));
        assert!(codes.contains(&Code::Registry));
        assert!(codes.contains(&Code::Schema));
        assert!(registry().validate().is_empty());
        let misspelled = r#"{"models":[],"review_prioirty":["x"]}"#;
        assert!(serde_json::from_str::<Registry>(misspelled).is_err());
    }
}
