//! Incident response workflow.
//!
//! Incident response was previously ad-hoc: engineers coordinated over chat
//! with no consistent record of severity, ownership, or timeline. This
//! module implements structured incident tracking with severity
//! classification, an escalation workflow, and a per-incident timeline so
//! response follows a consistent process end to end.
//!
//! # Architecture
//!
//! ```text
//! POST /incidents                     → create_incident
//! GET  /incidents                     → list_incidents
//! GET  /incidents/:id                 → get_incident
//! POST /incidents/:id/timeline        → add_timeline_entry
//! POST /incidents/:id/status          → update_incident_status
//! POST /incidents/:id/escalate        → escalate_incident
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Standard severity classification for an incident.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum IncidentSeverity {
    /// Full outage or data loss; page immediately.
    Sev1,
    /// Major functionality degraded for many users.
    Sev2,
    /// Minor functionality degraded or workaround available.
    Sev3,
    /// Cosmetic or low-impact issue.
    Sev4,
}

impl IncidentSeverity {
    /// Maximum time, in minutes, before an unresolved incident of this
    /// severity should be escalated to the next tier.
    pub fn escalation_sla_minutes(&self) -> i64 {
        match self {
            IncidentSeverity::Sev1 => 10,
            IncidentSeverity::Sev2 => 30,
            IncidentSeverity::Sev3 => 120,
            IncidentSeverity::Sev4 => 480,
        }
    }
}

/// Lifecycle status of an incident.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IncidentStatus {
    Open,
    Investigating,
    Mitigated,
    Resolved,
    Closed,
}

/// A single entry in an incident's timeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineEntry {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub actor: String,
    pub note: String,
}

/// A tracked incident.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Incident {
    pub id: String,
    pub title: String,
    pub description: String,
    pub severity: IncidentSeverity,
    pub status: IncidentStatus,
    pub escalation_level: u32,
    pub assigned_to: Option<String>,
    pub timeline: Vec<TimelineEntry>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Request body for `POST /incidents`.
#[derive(Debug, Deserialize)]
pub struct CreateIncidentRequest {
    pub title: String,
    pub description: String,
    pub severity: IncidentSeverity,
    pub assigned_to: Option<String>,
}

/// Request body for `POST /incidents/:id/timeline`.
#[derive(Debug, Deserialize)]
pub struct AddTimelineEntryRequest {
    pub actor: String,
    pub note: String,
}

/// Request body for `POST /incidents/:id/status`.
#[derive(Debug, Deserialize)]
pub struct UpdateStatusRequest {
    pub status: IncidentStatus,
    pub actor: String,
}

/// Request body for `POST /incidents/:id/escalate`.
#[derive(Debug, Deserialize)]
pub struct EscalateIncidentRequest {
    pub reason: String,
    pub actor: String,
}

pub type IncidentStore = Arc<Mutex<HashMap<String, Incident>>>;

pub fn create_incident_store() -> IncidentStore {
    Arc::new(Mutex::new(HashMap::new()))
}

#[derive(Clone)]
pub struct IncidentState {
    pub store: IncidentStore,
}

impl IncidentState {
    pub fn new() -> Self {
        Self {
            store: create_incident_store(),
        }
    }
}

impl Default for IncidentState {
    fn default() -> Self {
        Self::new()
    }
}

fn timeline_entry(actor: impl Into<String>, note: impl Into<String>) -> TimelineEntry {
    TimelineEntry {
        id: Uuid::new_v4().to_string(),
        timestamp: Utc::now(),
        actor: actor.into(),
        note: note.into(),
    }
}

/// `POST /incidents` — open a new incident with severity classification.
pub async fn create_incident(
    State(state): State<Arc<IncidentState>>,
    Json(body): Json<CreateIncidentRequest>,
) -> (StatusCode, Json<Incident>) {
    let now = Utc::now();
    let incident = Incident {
        id: Uuid::new_v4().to_string(),
        title: body.title,
        description: body.description,
        severity: body.severity,
        status: IncidentStatus::Open,
        escalation_level: 0,
        assigned_to: body.assigned_to,
        timeline: vec![timeline_entry("system", "incident opened")],
        created_at: now,
        updated_at: now,
    };

    tracing::warn!(
        incident_id = %incident.id,
        severity = ?incident.severity,
        "incident opened"
    );

    let mut store = state.store.lock().unwrap();
    store.insert(incident.id.clone(), incident.clone());

    (StatusCode::CREATED, Json(incident))
}

/// `GET /incidents` — list all tracked incidents.
pub async fn list_incidents(State(state): State<Arc<IncidentState>>) -> Json<Vec<Incident>> {
    let store = state.store.lock().unwrap();
    Json(store.values().cloned().collect())
}

/// `GET /incidents/:id` — fetch a single incident.
pub async fn get_incident(
    State(state): State<Arc<IncidentState>>,
    Path(id): Path<String>,
) -> Result<Json<Incident>, StatusCode> {
    let store = state.store.lock().unwrap();
    store.get(&id).cloned().map(Json).ok_or(StatusCode::NOT_FOUND)
}

/// `POST /incidents/:id/timeline` — append an entry to the incident timeline.
pub async fn add_timeline_entry(
    State(state): State<Arc<IncidentState>>,
    Path(id): Path<String>,
    Json(body): Json<AddTimelineEntryRequest>,
) -> Result<Json<Incident>, StatusCode> {
    let mut store = state.store.lock().unwrap();
    let incident = store.get_mut(&id).ok_or(StatusCode::NOT_FOUND)?;
    incident.timeline.push(timeline_entry(body.actor, body.note));
    incident.updated_at = Utc::now();
    Ok(Json(incident.clone()))
}

/// `POST /incidents/:id/status` — transition incident status, recording the
/// change in the timeline.
pub async fn update_incident_status(
    State(state): State<Arc<IncidentState>>,
    Path(id): Path<String>,
    Json(body): Json<UpdateStatusRequest>,
) -> Result<Json<Incident>, StatusCode> {
    let mut store = state.store.lock().unwrap();
    let incident = store.get_mut(&id).ok_or(StatusCode::NOT_FOUND)?;

    let note = format!("status changed from {:?} to {:?}", incident.status, body.status);
    incident.status = body.status;
    incident.updated_at = Utc::now();
    incident.timeline.push(timeline_entry(body.actor, note));

    Ok(Json(incident.clone()))
}

/// `POST /incidents/:id/escalate` — bump the escalation level, e.g. when the
/// severity's SLA window has elapsed without resolution.
pub async fn escalate_incident(
    State(state): State<Arc<IncidentState>>,
    Path(id): Path<String>,
    Json(body): Json<EscalateIncidentRequest>,
) -> Result<Json<Incident>, StatusCode> {
    let mut store = state.store.lock().unwrap();
    let incident = store.get_mut(&id).ok_or(StatusCode::NOT_FOUND)?;

    incident.escalation_level += 1;
    incident.updated_at = Utc::now();
    let note = format!(
        "escalated to level {} ({})",
        incident.escalation_level, body.reason
    );
    incident.timeline.push(timeline_entry(body.actor, note));

    tracing::warn!(
        incident_id = %id,
        level = incident.escalation_level,
        "incident escalated"
    );

    Ok(Json(incident.clone()))
}

/// Returns `true` if `incident` has been open longer than its severity's
/// escalation SLA and has not yet reached `Resolved`/`Closed`.
pub fn is_past_escalation_sla(incident: &Incident) -> bool {
    if matches!(incident.status, IncidentStatus::Resolved | IncidentStatus::Closed) {
        return false;
    }
    let elapsed = Utc::now() - incident.created_at;
    elapsed.num_minutes() > incident.severity.escalation_sla_minutes()
}
