use std::collections::HashMap;
use std::fmt::{self, Display, Formatter};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{LokinError, Result};

pub const SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub items: Vec<PlanItem>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanItem {
    pub description: String,
    pub duration_minutes: u16,
    pub status: PlanItemStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanItemStatus {
    Pending,
    Active,
    Completed,
    Skipped,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlanSource {
    #[default]
    Manual,
    Llm {
        provider: String,
        model: String,
    },
}

impl Display for PlanSource {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manual => formatter.write_str("manual"),
            Self::Llm { provider, model } => write!(formatter, "{provider} ({model})"),
        }
    }
}

impl Display for PlanItemStatus {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Completed => "completed",
            Self::Skipped => "skipped",
        };
        formatter.write_str(label)
    }
}

impl PlanItemStatus {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "active" => Ok(Self::Active),
            "completed" => Ok(Self::Completed),
            "skipped" => Ok(Self::Skipped),
            _ => Err(LokinError::InvalidInput(format!(
                "Unknown plan status '{value}'."
            ))),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum SessionEvent {
    SessionStarted {
        goal: String,
        duration_minutes: u16,
    },
    SessionPaused,
    SessionResumed,
    CheckpointAdded {
        note: String,
    },
    PlanCreated {
        plan: Plan,
        #[serde(default)]
        source: PlanSource,
    },
    PlanRevised {
        plan: Plan,
        #[serde(default)]
        source: PlanSource,
    },
    SessionStopped,
}

impl SessionEvent {
    pub fn name(&self) -> &'static str {
        match self {
            Self::SessionStarted { .. } => "session_started",
            Self::SessionPaused => "session_paused",
            Self::SessionResumed => "session_resumed",
            Self::CheckpointAdded { .. } => "checkpoint_added",
            Self::PlanCreated { .. } => "plan_created",
            Self::PlanRevised { .. } => "plan_revised",
            Self::SessionStopped => "session_stopped",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredEvent {
    pub schema_version: u16,
    pub session_id: Uuid,
    pub occurred_at: DateTime<Utc>,
    #[serde(flatten)]
    pub event: SessionEvent,
}

impl StoredEvent {
    pub fn new(session_id: Uuid, occurred_at: DateTime<Utc>, event: SessionEvent) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            session_id,
            occurred_at,
            event,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionMetadata {
    pub session_id: Uuid,
    pub goal: String,
    pub start_time: DateTime<Utc>,
    pub duration_minutes: u16,
}

impl SessionMetadata {
    pub fn deadline(&self) -> DateTime<Utc> {
        self.start_time + Duration::minutes(i64::from(self.duration_minutes))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleState {
    Running,
    Paused,
    Stopped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectiveState {
    Running,
    Paused,
    Stopped,
    Completed,
}

impl Display for EffectiveState {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Running => "Running",
            Self::Paused => "Paused",
            Self::Stopped => "Stopped",
            Self::Completed => "Completed",
        };
        formatter.write_str(label)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    pub occurred_at: DateTime<Utc>,
    pub note: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanRevision {
    pub occurred_at: DateTime<Utc>,
    pub created: bool,
    pub plan: Plan,
    pub source: PlanSource,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionProjection {
    pub metadata: SessionMetadata,
    pub lifecycle_state: LifecycleState,
    pub effective_state: EffectiveState,
    pub wall_clock_elapsed: Duration,
    pub remaining_wall_clock: Duration,
    pub active_work_duration: Duration,
    pub paused_duration: Duration,
    pub current_pause_duration: Option<Duration>,
    pub checkpoints: Vec<Checkpoint>,
    pub current_plan: Option<Plan>,
    pub plan_revisions: Vec<PlanRevision>,
    pub stopped_at: Option<DateTime<Utc>>,
}

pub fn validate_plan(plan: &Plan) -> Result<()> {
    for (index, item) in plan.items.iter().enumerate() {
        if item.description.trim().is_empty() {
            return Err(LokinError::InvalidInput(format!(
                "Plan item {} must have a description.",
                index + 1
            )));
        }
        if item.duration_minutes == 0 {
            return Err(LokinError::InvalidInput(format!(
                "Plan item {} must be longer than zero minutes.",
                index + 1
            )));
        }
    }
    Ok(())
}

pub fn reconstruct_session(
    events: &[StoredEvent],
    now: DateTime<Utc>,
) -> Result<SessionProjection> {
    let first = events.first().ok_or_else(|| {
        LokinError::CorruptLog("cannot reconstruct a session without events".to_string())
    })?;

    if first.schema_version != SCHEMA_VERSION {
        return Err(LokinError::UnsupportedSchemaVersion(first.schema_version));
    }

    let (goal, duration_minutes) = match &first.event {
        SessionEvent::SessionStarted {
            goal,
            duration_minutes,
        } => (goal.trim().to_string(), *duration_minutes),
        _ => {
            return Err(LokinError::CorruptLog(format!(
                "session {} does not begin with session_started",
                first.session_id
            )));
        }
    };

    if goal.is_empty() || duration_minutes == 0 {
        return Err(LokinError::CorruptLog(format!(
            "session {} has invalid start metadata",
            first.session_id
        )));
    }

    let metadata = SessionMetadata {
        session_id: first.session_id,
        goal,
        start_time: first.occurred_at,
        duration_minutes,
    };
    let deadline = metadata.deadline();
    let mut lifecycle = LifecycleState::Running;
    let mut cursor = metadata.start_time;
    let mut active = Duration::zero();
    let mut paused = Duration::zero();
    let mut stopped_at = None;
    let mut checkpoints = Vec::new();
    let mut current_plan: Option<Plan> = None;
    let mut plan_revisions = Vec::new();

    for event in events.iter().skip(1) {
        if event.schema_version != SCHEMA_VERSION {
            return Err(LokinError::UnsupportedSchemaVersion(event.schema_version));
        }
        if event.session_id != metadata.session_id {
            return Err(LokinError::CorruptLog(format!(
                "session {} contains an event for {}",
                metadata.session_id, event.session_id
            )));
        }
        if event.occurred_at < cursor {
            return Err(LokinError::CorruptLog(format!(
                "session {} contains non-monotonic timestamps",
                metadata.session_id
            )));
        }
        if event.occurred_at >= deadline {
            return Err(LokinError::CorruptLog(format!(
                "session {} contains an event at or after its deadline",
                metadata.session_id
            )));
        }
        if lifecycle == LifecycleState::Stopped {
            return Err(LokinError::CorruptLog(format!(
                "session {} contains an event after it stopped",
                metadata.session_id
            )));
        }

        match &event.event {
            SessionEvent::SessionStarted { .. } => {
                return Err(LokinError::CorruptLog(format!(
                    "session {} contains more than one start event",
                    metadata.session_id
                )));
            }
            SessionEvent::SessionPaused => {
                if lifecycle != LifecycleState::Running {
                    return Err(LokinError::CorruptLog(format!(
                        "session {} was paused while not running",
                        metadata.session_id
                    )));
                }
                active += event.occurred_at - cursor;
                cursor = event.occurred_at;
                lifecycle = LifecycleState::Paused;
            }
            SessionEvent::SessionResumed => {
                if lifecycle != LifecycleState::Paused {
                    return Err(LokinError::CorruptLog(format!(
                        "session {} was resumed while not paused",
                        metadata.session_id
                    )));
                }
                paused += event.occurred_at - cursor;
                cursor = event.occurred_at;
                lifecycle = LifecycleState::Running;
            }
            SessionEvent::CheckpointAdded { note } => {
                if note.trim().is_empty() {
                    return Err(LokinError::CorruptLog(format!(
                        "session {} contains an empty checkpoint",
                        metadata.session_id
                    )));
                }
                checkpoints.push(Checkpoint {
                    occurred_at: event.occurred_at,
                    note: note.trim().to_string(),
                });
            }
            SessionEvent::PlanCreated { plan, source } => {
                if current_plan.is_some() {
                    return Err(LokinError::CorruptLog(format!(
                        "session {} contains more than one plan_created event",
                        metadata.session_id
                    )));
                }
                validate_plan(plan).map_err(|error| {
                    LokinError::CorruptLog(format!(
                        "session {} contains an invalid plan: {error}",
                        metadata.session_id
                    ))
                })?;
                current_plan = Some(plan.clone());
                plan_revisions.push(PlanRevision {
                    occurred_at: event.occurred_at,
                    created: true,
                    plan: plan.clone(),
                    source: source.clone(),
                });
            }
            SessionEvent::PlanRevised { plan, source } => {
                if current_plan.is_none() {
                    return Err(LokinError::CorruptLog(format!(
                        "session {} contains plan_revised before plan_created",
                        metadata.session_id
                    )));
                }
                validate_plan(plan).map_err(|error| {
                    LokinError::CorruptLog(format!(
                        "session {} contains an invalid plan: {error}",
                        metadata.session_id
                    ))
                })?;
                current_plan = Some(plan.clone());
                plan_revisions.push(PlanRevision {
                    occurred_at: event.occurred_at,
                    created: false,
                    plan: plan.clone(),
                    source: source.clone(),
                });
            }
            SessionEvent::SessionStopped => {
                match lifecycle {
                    LifecycleState::Running => active += event.occurred_at - cursor,
                    LifecycleState::Paused => paused += event.occurred_at - cursor,
                    LifecycleState::Stopped => unreachable!(),
                }
                cursor = event.occurred_at;
                lifecycle = LifecycleState::Stopped;
                stopped_at = Some(event.occurred_at);
            }
        }
    }

    if now < cursor {
        return Err(LokinError::CorruptLog(format!(
            "session {} contains events later than the evaluation time",
            metadata.session_id
        )));
    }

    let effective_end = stopped_at
        .unwrap_or_else(|| now.min(deadline))
        .max(metadata.start_time);
    if lifecycle != LifecycleState::Stopped {
        match lifecycle {
            LifecycleState::Running => active += effective_end - cursor,
            LifecycleState::Paused => paused += effective_end - cursor,
            LifecycleState::Stopped => unreachable!(),
        }
    }

    let effective_state = if lifecycle == LifecycleState::Stopped {
        EffectiveState::Stopped
    } else if now >= deadline {
        EffectiveState::Completed
    } else {
        match lifecycle {
            LifecycleState::Running => EffectiveState::Running,
            LifecycleState::Paused => EffectiveState::Paused,
            LifecycleState::Stopped => unreachable!(),
        }
    };

    let current_pause_duration = if effective_state == EffectiveState::Paused {
        Some(now - cursor)
    } else {
        None
    };
    let remaining_wall_clock = match stopped_at {
        Some(stopped) => (deadline - stopped).max(Duration::zero()),
        None => (deadline - now).max(Duration::zero()),
    };
    let wall_clock_elapsed = effective_end - metadata.start_time;

    if active + paused != wall_clock_elapsed {
        return Err(LokinError::CorruptLog(format!(
            "session {} timing intervals do not cover wall-clock elapsed time",
            metadata.session_id
        )));
    }

    Ok(SessionProjection {
        metadata,
        lifecycle_state: lifecycle,
        effective_state,
        wall_clock_elapsed,
        remaining_wall_clock,
        active_work_duration: active,
        paused_duration: paused,
        current_pause_duration,
        checkpoints,
        current_plan,
        plan_revisions,
        stopped_at,
    })
}

pub fn reconstruct_all(
    events: &[StoredEvent],
    now: DateTime<Utc>,
) -> Result<Vec<SessionProjection>> {
    let mut grouped: HashMap<Uuid, Vec<StoredEvent>> = HashMap::new();
    let mut order = Vec::new();

    for event in events {
        if !grouped.contains_key(&event.session_id) {
            order.push(event.session_id);
        }
        grouped
            .entry(event.session_id)
            .or_default()
            .push(event.clone());
    }

    let mut projections = Vec::with_capacity(order.len());
    for session_id in order {
        let session_events = grouped.get(&session_id).ok_or_else(|| {
            LokinError::CorruptLog(format!("events disappeared for session {session_id}"))
        })?;
        projections.push(reconstruct_session(session_events, now)?);
    }
    projections.sort_by_key(|projection| projection.metadata.start_time);
    let active_count = projections
        .iter()
        .filter(|projection| {
            matches!(
                projection.effective_state,
                EffectiveState::Running | EffectiveState::Paused
            )
        })
        .count();
    if active_count > 1 {
        return Err(LokinError::CorruptLog(
            "more than one session is currently active".to_string(),
        ));
    }
    Ok(projections)
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn at(minute: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 28, 12, minute as u32, 0)
            .unwrap()
    }

    fn started(session_id: Uuid, duration_minutes: u16) -> StoredEvent {
        StoredEvent::new(
            session_id,
            at(0),
            SessionEvent::SessionStarted {
                goal: "Build Lokin".to_string(),
                duration_minutes,
            },
        )
    }

    #[test]
    fn event_json_round_trip_has_stable_type() {
        let event = started(Uuid::nil(), 25);
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"session_started\""));
        assert_eq!(serde_json::from_str::<StoredEvent>(&json).unwrap(), event);
    }

    #[test]
    fn old_plan_events_without_provenance_default_to_manual() {
        let event: StoredEvent = serde_json::from_str(
            r#"{
                "schema_version": 1,
                "session_id": "00000000-0000-0000-0000-000000000000",
                "occurred_at": "2026-08-28T12:01:00Z",
                "type": "plan_created",
                "data": {
                    "plan": {
                        "items": [{
                            "description": "Existing plan",
                            "duration_minutes": 10,
                            "status": "pending"
                        }]
                    }
                }
            }"#,
        )
        .unwrap();

        assert!(matches!(
            event.event,
            SessionEvent::PlanCreated {
                source: PlanSource::Manual,
                ..
            }
        ));
    }

    #[test]
    fn running_session_calculates_wall_clock_and_active_time() {
        let projection = reconstruct_session(&[started(Uuid::nil(), 25)], at(10)).unwrap();
        assert_eq!(projection.effective_state, EffectiveState::Running);
        assert_eq!(projection.wall_clock_elapsed, Duration::minutes(10));
        assert_eq!(projection.active_work_duration, Duration::minutes(10));
        assert_eq!(projection.paused_duration, Duration::zero());
        assert_eq!(projection.remaining_wall_clock, Duration::minutes(15));
    }

    #[test]
    fn pause_resume_intervals_do_not_extend_deadline() {
        let id = Uuid::nil();
        let events = vec![
            started(id, 25),
            StoredEvent::new(id, at(5), SessionEvent::SessionPaused),
            StoredEvent::new(id, at(12), SessionEvent::SessionResumed),
        ];
        let projection = reconstruct_session(&events, at(20)).unwrap();
        assert_eq!(projection.active_work_duration, Duration::minutes(13));
        assert_eq!(projection.paused_duration, Duration::minutes(7));
        assert_eq!(projection.remaining_wall_clock, Duration::minutes(5));
        assert_eq!(projection.metadata.deadline(), at(25));
    }

    #[test]
    fn current_pause_duration_is_derived_from_latest_pause() {
        let id = Uuid::nil();
        let events = vec![
            started(id, 25),
            StoredEvent::new(id, at(5), SessionEvent::SessionPaused),
        ];
        let projection = reconstruct_session(&events, at(9)).unwrap();
        assert_eq!(projection.effective_state, EffectiveState::Paused);
        assert_eq!(
            projection.current_pause_duration,
            Some(Duration::minutes(4))
        );
        assert_eq!(projection.active_work_duration, Duration::minutes(5));
        assert_eq!(projection.paused_duration, Duration::minutes(4));
    }

    #[test]
    fn completion_while_paused_caps_time_at_deadline() {
        let id = Uuid::nil();
        let events = vec![
            started(id, 25),
            StoredEvent::new(id, at(5), SessionEvent::SessionPaused),
        ];
        let projection = reconstruct_session(&events, at(30)).unwrap();
        assert_eq!(projection.effective_state, EffectiveState::Completed);
        assert_eq!(projection.active_work_duration, Duration::minutes(5));
        assert_eq!(projection.paused_duration, Duration::minutes(20));
        assert_eq!(projection.current_pause_duration, None);
        assert_eq!(projection.remaining_wall_clock, Duration::zero());
    }

    #[test]
    fn stopped_session_preserves_unused_budget() {
        let id = Uuid::nil();
        let events = vec![
            started(id, 25),
            StoredEvent::new(id, at(4), SessionEvent::SessionPaused),
            StoredEvent::new(id, at(9), SessionEvent::SessionStopped),
        ];
        let projection = reconstruct_session(&events, at(20)).unwrap();
        assert_eq!(projection.effective_state, EffectiveState::Stopped);
        assert_eq!(projection.active_work_duration, Duration::minutes(4));
        assert_eq!(projection.paused_duration, Duration::minutes(5));
        assert_eq!(projection.remaining_wall_clock, Duration::minutes(16));
    }

    #[test]
    fn rejects_invalid_transition() {
        let id = Uuid::nil();
        let events = vec![
            started(id, 25),
            StoredEvent::new(id, at(5), SessionEvent::SessionResumed),
        ];
        assert!(matches!(
            reconstruct_session(&events, at(10)),
            Err(LokinError::CorruptLog(_))
        ));
    }

    #[test]
    fn rejects_event_at_or_after_deadline() {
        let id = Uuid::nil();
        let events = vec![
            started(id, 25),
            StoredEvent::new(id, at(25), SessionEvent::SessionPaused),
        ];
        assert!(matches!(
            reconstruct_session(&events, at(25)),
            Err(LokinError::CorruptLog(_))
        ));
    }

    #[test]
    fn rejects_plan_revision_before_creation() {
        let id = Uuid::nil();
        let events = vec![
            started(id, 25),
            StoredEvent::new(
                id,
                at(1),
                SessionEvent::PlanRevised {
                    plan: Plan { items: Vec::new() },
                    source: PlanSource::Manual,
                },
            ),
        ];
        assert!(matches!(
            reconstruct_session(&events, at(2)),
            Err(LokinError::CorruptLog(_))
        ));
    }

    #[test]
    fn rejects_events_after_stop() {
        let id = Uuid::nil();
        let events = vec![
            started(id, 25),
            StoredEvent::new(id, at(5), SessionEvent::SessionStopped),
            StoredEvent::new(
                id,
                at(6),
                SessionEvent::CheckpointAdded {
                    note: "Too late".to_string(),
                },
            ),
        ];
        assert!(matches!(
            reconstruct_session(&events, at(7)),
            Err(LokinError::CorruptLog(_))
        ));
    }

    #[test]
    fn rejects_more_than_one_active_session() {
        let first = started(Uuid::new_v4(), 25);
        let second = started(Uuid::new_v4(), 25);
        assert!(matches!(
            reconstruct_all(&[first, second], at(1)),
            Err(LokinError::CorruptLog(_))
        ));
    }

    #[test]
    fn plan_snapshots_reconstruct_latest_version() {
        let id = Uuid::nil();
        let initial = Plan {
            items: vec![PlanItem {
                description: "Model events".to_string(),
                duration_minutes: 10,
                status: PlanItemStatus::Pending,
            }],
        };
        let mut revised = initial.clone();
        revised.items[0].status = PlanItemStatus::Active;
        let events = vec![
            started(id, 25),
            StoredEvent::new(
                id,
                at(1),
                SessionEvent::PlanCreated {
                    plan: initial.clone(),
                    source: PlanSource::Manual,
                },
            ),
            StoredEvent::new(
                id,
                at(2),
                SessionEvent::PlanRevised {
                    plan: revised.clone(),
                    source: PlanSource::Llm {
                        provider: "groq".to_string(),
                        model: "test-model".to_string(),
                    },
                },
            ),
        ];
        let projection = reconstruct_session(&events, at(3)).unwrap();
        assert_eq!(projection.current_plan, Some(revised));
        assert_eq!(projection.plan_revisions.len(), 2);
        assert!(matches!(
            projection.plan_revisions[1].source,
            PlanSource::Llm { .. }
        ));
    }
}
