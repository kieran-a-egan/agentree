//! Agentree-owned role and workflow contracts (Issue #9, v0.6).
//!
//! This module defines *contracts, routing policy and validation* only. It starts nothing: the
//! Claude adapter (#11) turns a [`contract::ValidatedWorkflow`] and the selections made here
//! into a native-team launch. Model choice never lives in routing: [`registry`] is the only place
//! that picks a model, and it does so from registry data, never from model names.
//!
//! Everything here is proprietary Agentree metadata and is kept out of the Agent Plugins v1.0
//! manifest; a plugin is referenced by name and skill only, and only a *trusted* plugin resolves.

pub mod contract;
pub mod evidence;
pub mod registry;
pub mod review;
pub mod route;

use std::fmt;

/// Wire identifier of the workflow definition format.
pub const SCHEMA: &str = "agentree.workflow/1";
/// Version of the contracts in this module. Older readers refuse a newer definition.
pub const VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Code {
    Schema,
    Duplicate,
    Unknown,
    Cycle,
    Capability,
    ModelPolicy,
    Ownership,
    Review,
    Enforcement,
    Plugin,
    Registry,
    Evidence,
    Verdict,
}

/// One reason a definition, packet or verdict is rejected. Validation returns every violation
/// it finds so a definition can be fixed in one pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    pub code: Code,
    /// Where: `roles[explorer]`, `stages[review].depends_on`, ...
    pub at: String,
    pub message: String,
}

impl Violation {
    pub fn new(code: Code, at: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code,
            at: at.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.at, self.message)
    }
}

/// How a backend applies a policy. Prompt instructions are never reported as `Enforced`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Enforcement {
    /// The backend cannot do this at all.
    Unsupported,
    /// Only asked for in instructions; the backend may ignore it.
    Instructed,
    /// Agentree can see afterwards whether it happened, but cannot prevent it.
    Observed,
    /// The backend refuses to run otherwise (a launch flag or a technical gate).
    Enforced,
}

impl Enforcement {
    pub fn label(self) -> &'static str {
        match self {
            Enforcement::Unsupported => "unsupported",
            Enforcement::Instructed => "instruction only",
            Enforcement::Observed => "observed, not prevented",
            Enforcement::Enforced => "enforced",
        }
    }
}
