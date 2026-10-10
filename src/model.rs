//! Runtime-neutral core contract (schema version 1).
//!
//! Adapters (Claude Code first) translate native runtime data into these types.
//! The dashboard consumes them. See `docs/EVENT_SCHEMA.md` for the wire format.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Version stamped on every [`Event`]. Bump only for breaking wire changes.
pub const EVENT_SCHEMA_VERSION: u32 = 1;

/// Unix epoch milliseconds, UTC.
pub type Timestamp = u64;

/// Opaque identifier, unique within its scope. Serialises as a plain string.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Id(String);

impl Id {
    /// Wraps any string-like value as an identifier.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrows the identifier text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A value that was observed, or explicitly absent. Unknown is never encoded as zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
pub enum Measure<T> {
    /// The adapter saw this value in the source.
    Observed(T),
    /// The source did not report this value.
    #[default]
    Unknown,
}

impl<T: Copy> Measure<T> {
    /// Returns the observed value, or `None` when unknown.
    pub fn get(self) -> Option<T> {
        match self {
            Measure::Observed(value) => Some(value),
            Measure::Unknown => None,
        }
    }
}

/// Token and cost totals. Every field defaults to [`Measure::Unknown`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: Measure<u64>,
    pub output_tokens: Measure<u64>,
    pub cache_read_tokens: Measure<u64>,
    pub cache_write_tokens: Measure<u64>,
    /// Estimated USD list-price cost. Unknown when pricing is incomplete.
    pub cost_usd: Measure<f64>,
}

/// Lifecycle state of a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Active,
    ObjectiveComplete,
    Ended,
}

/// One orchestrated attempt at a project objective.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub id: Id,
    pub project: String,
    pub started_at: Option<Timestamp>,
    pub status: RunStatus,
    pub usage: Usage,
}

/// How an agent was linked to the run. Governs how it may be presented.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Link {
    /// Confirmed as a native teammate by the runtime.
    NativeVerified,
    /// Matched to a native teammate by project metadata only.
    NativeProjectLinked,
    /// Seen in telemetry but not tied to a native teammate.
    Unverified,
    /// A background subagent. Never presented as a native teammate.
    BackgroundSubagent,
}

/// A participant in a run: the lead, a teammate, or a subagent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Agent {
    pub id: Id,
    pub run_id: Id,
    pub name: String,
    pub role: String,
    pub link: Link,
}

/// Progress state of a task.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
}

/// A unit of work assigned to an agent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: Id,
    pub agent_id: Id,
    pub subject: String,
    pub status: TaskStatus,
}

/// Kind of pipeline stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageKind {
    Work,
    FinalReview,
}

/// Progress state of a stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageStatus {
    Pending,
    Running,
    Passed,
    NeedsChanges,
    Blocked,
    Complete,
}

/// A phase of a run, such as work or final review.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stage {
    pub id: Id,
    pub run_id: Id,
    pub kind: StageKind,
    pub status: StageStatus,
}

/// Resolution state of an approval request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalState {
    Pending,
    Granted,
    Denied,
}

/// A request for a human decision. No adapter produces these yet.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Approval {
    pub id: Id,
    pub run_id: Id,
    pub agent_id: Option<Id>,
    pub question: String,
    pub state: ApprovalState,
    pub requested_at: Option<Timestamp>,
    pub resolved_at: Option<Timestamp>,
}

/// Rejects unknown schema versions rather than guessing.
fn current_schema_version<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<u32, D::Error> {
    let version = u32::deserialize(deserializer)?;
    if version == EVENT_SCHEMA_VERSION {
        Ok(version)
    } else {
        Err(serde::de::Error::custom(format!(
            "unsupported event schema_version {version}; this reader understands {EVENT_SCHEMA_VERSION}"
        )))
    }
}

/// Versioned envelope. Every event carries `schema_version`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    #[serde(deserialize_with = "current_schema_version")]
    pub schema_version: u32,
    /// Unique within a run.
    pub id: Id,
    pub run_id: Id,
    /// `None` for run-level events such as the advisor.
    pub agent_id: Option<Id>,
    /// Human-readable label, e.g. "main", a teammate name, "Opus advisor".
    pub actor: String,
    /// `None` when the source timestamp is absent or unparseable.
    pub at: Option<Timestamp>,
    pub kind: EventKind,
}

impl Event {
    /// Builds an event stamped with [`EVENT_SCHEMA_VERSION`].
    pub fn new(
        id: Id,
        run_id: Id,
        agent_id: Option<Id>,
        actor: impl Into<String>,
        at: Option<Timestamp>,
        kind: EventKind,
    ) -> Self {
        Self {
            schema_version: EVENT_SCHEMA_VERSION,
            id,
            run_id,
            agent_id,
            actor: actor.into(),
            at,
            kind,
        }
    }
}

/// Event payload, tagged by `type` on the wire.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventKind {
    /// A tool call. `target` is a short, sanitised label (basename or pattern), empty when none; never raw command text.
    ToolUsed {
        tool: String,
        target: String,
    },
    AdvisorInvoked,
    AdvisorCompleted,
    ReviewStarted,
    ObjectiveCompleted,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn measure_unknown_is_explicit() {
        assert_eq!(
            serde_json::to_value(Measure::<u64>::Unknown).unwrap(),
            json!({"state": "unknown"})
        );
        assert_eq!(
            serde_json::to_value(Measure::Observed(5u64)).unwrap(),
            json!({"state": "observed", "value": 5})
        );
        let unknown: Measure<u64> = serde_json::from_value(json!({"state": "unknown"})).unwrap();
        let observed: Measure<u64> =
            serde_json::from_value(json!({"state": "observed", "value": 5})).unwrap();
        assert_eq!(unknown, Measure::Unknown);
        assert_eq!(observed, Measure::Observed(5));
        assert_eq!(Measure::<u64>::default(), Measure::Unknown);
        assert_eq!(Measure::<u64>::default().get(), None);
    }

    #[test]
    fn usage_defaults_to_unknown_not_zero() {
        let value = serde_json::to_value(Usage::default()).unwrap();
        assert_eq!(
            value,
            json!({
                "input_tokens": {"state": "unknown"},
                "output_tokens": {"state": "unknown"},
                "cache_read_tokens": {"state": "unknown"},
                "cache_write_tokens": {"state": "unknown"},
                "cost_usd": {"state": "unknown"}
            })
        );
    }

    #[test]
    fn event_envelope_is_versioned() {
        let event = Event::new(
            Id::new("run-1@0#0"),
            Id::new("run-1"),
            None,
            "main",
            Some(1_781_000_000_000),
            EventKind::ToolUsed {
                tool: "Read".into(),
                target: "main.rs".into(),
            },
        );
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["schema_version"], json!(1));
        assert_eq!(value["id"], json!("run-1@0#0"));
        assert_eq!(value["run_id"], json!("run-1"));
        assert_eq!(value["agent_id"], json!(null));
        assert_eq!(value["actor"], json!("main"));
        assert_eq!(value["at"], json!(1_781_000_000_000u64));
        assert_eq!(
            value["kind"],
            json!({"type": "tool_used", "tool": "Read", "target": "main.rs"})
        );
        let back: Event = serde_json::from_value(value).unwrap();
        assert_eq!(back, event);
    }

    #[test]
    fn event_rejects_unknown_schema_version() {
        let mut value = json!({
            "schema_version": 1,
            "id": "run-1@0#0",
            "run_id": "run-1",
            "agent_id": null,
            "actor": "main",
            "at": 1_781_000_000_000u64,
            "kind": {"type": "tool_used", "tool": "Read", "target": "main.rs"}
        });
        assert!(serde_json::from_value::<Event>(value.clone()).is_ok());
        for version in [2, 0] {
            value["schema_version"] = json!(version);
            assert!(serde_json::from_value::<Event>(value.clone()).is_err());
        }
        // A missing version is an error too, never a default.
        value.as_object_mut().unwrap().remove("schema_version");
        assert!(serde_json::from_value::<Event>(value).is_err());
    }

    #[test]
    fn every_event_kind_has_stable_tag() {
        let cases = [
            (EventKind::AdvisorInvoked, "advisor_invoked"),
            (EventKind::AdvisorCompleted, "advisor_completed"),
            (EventKind::ReviewStarted, "review_started"),
            (EventKind::ObjectiveCompleted, "objective_completed"),
        ];
        for (kind, tag) in cases {
            assert_eq!(serde_json::to_value(&kind).unwrap()["type"], json!(tag));
        }
    }

    #[test]
    fn id_is_transparent_string() {
        assert_eq!(
            serde_json::to_value(Id::new("run-1")).unwrap(),
            json!("run-1")
        );
        assert_eq!(Id::new("run-1").to_string(), "run-1");
    }

    #[test]
    fn link_and_status_wire_names() {
        assert_eq!(
            serde_json::to_value(Link::BackgroundSubagent).unwrap(),
            json!("background_subagent")
        );
        assert_eq!(
            serde_json::to_value(RunStatus::ObjectiveComplete).unwrap(),
            json!("objective_complete")
        );
        assert_eq!(
            serde_json::to_value(StageStatus::NeedsChanges).unwrap(),
            json!("needs_changes")
        );
        assert_eq!(
            serde_json::to_value(TaskStatus::InProgress).unwrap(),
            json!("in_progress")
        );
        assert_eq!(
            serde_json::to_value(ApprovalState::Granted).unwrap(),
            json!("granted")
        );
    }
}
