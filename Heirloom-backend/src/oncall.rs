//! Automated on-call schedule management.
//!
//! On-call schedules were previously managed by hand in a spreadsheet, which
//! led to gaps in coverage and missed handoffs. This module implements
//! rotation scheduling, handoff notifications, and tiered escalation policies
//! so the on-call roster can be generated and maintained automatically.
//!
//! # Architecture
//!
//! ```text
//! POST /admin/on-call-schedule        → create_on_call_schedule (builds a rotation)
//! GET  /admin/on-call-schedule        → list_on_call_schedules
//! GET  /admin/on-call-schedule/:id    → get_on_call_schedule
//! POST /admin/on-call-schedule/:id/escalate → trigger_escalation
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A single engineer eligible to be placed on the rotation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnCallParticipant {
    pub id: String,
    pub name: String,
    pub contact: String,
}

/// One scheduled on-call shift produced by the rotation algorithm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnCallShift {
    pub id: String,
    pub participant: OnCallParticipant,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
}

/// A tiered escalation level: if the primary on-call does not acknowledge in
/// time, the next level's contacts are notified.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationLevel {
    pub level: u32,
    pub delay_minutes: i64,
    pub contacts: Vec<String>,
}

/// The full escalation policy attached to a schedule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationPolicy {
    pub levels: Vec<EscalationLevel>,
}

impl Default for EscalationPolicy {
    fn default() -> Self {
        Self {
            levels: vec![
                EscalationLevel {
                    level: 1,
                    delay_minutes: 5,
                    contacts: vec![],
                },
                EscalationLevel {
                    level: 2,
                    delay_minutes: 15,
                    contacts: vec![],
                },
                EscalationLevel {
                    level: 3,
                    delay_minutes: 30,
                    contacts: vec![],
                },
            ],
        }
    }
}

/// A record of a handoff notification sent between rotations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffNotification {
    pub id: String,
    pub schedule_id: String,
    pub outgoing: OnCallParticipant,
    pub incoming: OnCallParticipant,
    pub sent_at: DateTime<Utc>,
}

/// A generated on-call schedule: a rotation of shifts plus its escalation
/// policy and the handoff notifications produced when shifts change hands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnCallSchedule {
    pub id: String,
    pub name: String,
    pub rotation_hours: i64,
    pub shifts: Vec<OnCallShift>,
    pub escalation_policy: EscalationPolicy,
    pub handoffs: Vec<HandoffNotification>,
    pub created_at: DateTime<Utc>,
}

/// Request body for `POST /admin/on-call-schedule`.
#[derive(Debug, Deserialize)]
pub struct CreateOnCallScheduleRequest {
    pub name: String,
    pub participants: Vec<OnCallParticipant>,
    /// Length of each rotation shift, in hours.
    pub rotation_hours: i64,
    /// How many shifts to generate up front.
    pub shift_count: u32,
    pub escalation_policy: Option<EscalationPolicy>,
}

/// Request body for triggering an escalation manually (e.g. from an alert
/// that has gone unacknowledged).
#[derive(Debug, Deserialize)]
pub struct TriggerEscalationRequest {
    pub reason: String,
    pub current_level: u32,
}

/// Response describing which escalation level fired and who was notified.
#[derive(Debug, Serialize)]
pub struct EscalationResult {
    pub schedule_id: String,
    pub level_notified: u32,
    pub contacts_notified: Vec<String>,
    pub reason: String,
}

pub type OnCallStore = Arc<Mutex<HashMap<String, OnCallSchedule>>>;

pub fn create_on_call_store() -> OnCallStore {
    Arc::new(Mutex::new(HashMap::new()))
}

#[derive(Clone)]
pub struct OnCallState {
    pub store: OnCallStore,
}

impl OnCallState {
    pub fn new() -> Self {
        Self {
            store: create_on_call_store(),
        }
    }
}

impl Default for OnCallState {
    fn default() -> Self {
        Self::new()
    }
}

/// Build a round-robin rotation of `shift_count` shifts across
/// `participants`, each `rotation_hours` long, starting now.
fn build_rotation(
    participants: &[OnCallParticipant],
    rotation_hours: i64,
    shift_count: u32,
) -> Vec<OnCallShift> {
    if participants.is_empty() || shift_count == 0 {
        return vec![];
    }

    let mut shifts = Vec::with_capacity(shift_count as usize);
    let mut cursor = Utc::now();

    for i in 0..shift_count {
        let participant = participants[(i as usize) % participants.len()].clone();
        let starts_at = cursor;
        let ends_at = cursor + Duration::hours(rotation_hours);
        shifts.push(OnCallShift {
            id: Uuid::new_v4().to_string(),
            participant,
            starts_at,
            ends_at,
        });
        cursor = ends_at;
    }

    shifts
}

/// Produce handoff notifications for each consecutive pair of shifts.
fn build_handoffs(schedule_id: &str, shifts: &[OnCallShift]) -> Vec<HandoffNotification> {
    let mut handoffs = Vec::new();
    for window in shifts.windows(2) {
        let outgoing = &window[0];
        let incoming = &window[1];
        handoffs.push(HandoffNotification {
            id: Uuid::new_v4().to_string(),
            schedule_id: schedule_id.to_string(),
            outgoing: outgoing.participant.clone(),
            incoming: incoming.participant.clone(),
            sent_at: outgoing.ends_at,
        });
    }
    handoffs
}

/// `POST /admin/on-call-schedule` — generate a new rotation schedule.
pub async fn create_on_call_schedule(
    State(state): State<Arc<OnCallState>>,
    Json(body): Json<CreateOnCallScheduleRequest>,
) -> Result<(StatusCode, Json<OnCallSchedule>), (StatusCode, Json<serde_json::Value>)> {
    if body.participants.is_empty() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": "at least one participant is required" })),
        ));
    }
    if body.rotation_hours <= 0 {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": "rotation_hours must be positive" })),
        ));
    }

    let id = Uuid::new_v4().to_string();
    let shifts = build_rotation(&body.participants, body.rotation_hours, body.shift_count);
    let handoffs = build_handoffs(&id, &shifts);

    for handoff in &handoffs {
        tracing::info!(
            schedule_id = %handoff.schedule_id,
            outgoing = %handoff.outgoing.name,
            incoming = %handoff.incoming.name,
            "on-call handoff notification queued"
        );
    }

    let schedule = OnCallSchedule {
        id: id.clone(),
        name: body.name,
        rotation_hours: body.rotation_hours,
        shifts,
        escalation_policy: body.escalation_policy.unwrap_or_default(),
        handoffs,
        created_at: Utc::now(),
    };

    let mut store = state.store.lock().unwrap();
    store.insert(id, schedule.clone());

    Ok((StatusCode::CREATED, Json(schedule)))
}

/// `GET /admin/on-call-schedule` — list all rotation schedules.
pub async fn list_on_call_schedules(
    State(state): State<Arc<OnCallState>>,
) -> Json<Vec<OnCallSchedule>> {
    let store = state.store.lock().unwrap();
    Json(store.values().cloned().collect())
}

/// `GET /admin/on-call-schedule/:id` — fetch a single schedule.
pub async fn get_on_call_schedule(
    State(state): State<Arc<OnCallState>>,
    Path(id): Path<String>,
) -> Result<Json<OnCallSchedule>, StatusCode> {
    let store = state.store.lock().unwrap();
    store
        .get(&id)
        .cloned()
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

/// `POST /admin/on-call-schedule/:id/escalate` — walk the escalation policy
/// starting at `current_level` and notify the next tier's contacts.
pub async fn trigger_escalation(
    State(state): State<Arc<OnCallState>>,
    Path(id): Path<String>,
    Json(body): Json<TriggerEscalationRequest>,
) -> Result<Json<EscalationResult>, StatusCode> {
    let store = state.store.lock().unwrap();
    let schedule = store.get(&id).ok_or(StatusCode::NOT_FOUND)?;

    let next_level = schedule
        .escalation_policy
        .levels
        .iter()
        .find(|lvl| lvl.level > body.current_level)
        .or_else(|| schedule.escalation_policy.levels.last());

    let Some(level) = next_level else {
        return Err(StatusCode::NOT_FOUND);
    };

    tracing::warn!(
        schedule_id = %id,
        level = level.level,
        reason = %body.reason,
        "escalation triggered"
    );

    Ok(Json(EscalationResult {
        schedule_id: id,
        level_notified: level.level,
        contacts_notified: level.contacts.clone(),
        reason: body.reason,
    }))
}
