//! Service dependency mapping.
//!
//! Service dependencies were previously undocumented, making it hard to
//! reason about blast radius during an incident. This module implements
//! dependency discovery from observed service calls, a graph representation
//! that can be exported for visualization, change detection between
//! snapshots, and impact analysis for a given service.
//!
//! # Architecture
//!
//! ```text
//! POST /dependencies/discover   → discover_dependencies (record an observed call)
//! GET  /dependencies/graph      → get_dependency_graph (JSON + DOT export)
//! POST /dependencies/impact     → analyze_impact (upstream/downstream for a service)
//! GET  /dependencies/changes    → get_recent_changes
//! ```

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use axum::{extract::State, http::StatusCode, Json};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The kind of relationship between two services.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum DependencyKind {
    SynchronousRpc,
    AsyncMessage,
    DataStore,
}

/// A directed edge in the dependency graph: `from` depends on `to`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct DependencyEdge {
    pub from: String,
    pub to: String,
    pub kind: DependencyKind,
}

/// A discovery event recorded when a new dependency is observed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryEvent {
    pub edge: DependencyEdge,
    pub observed_at: DateTime<Utc>,
}

/// A change detected between the current graph and the previous snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyChange {
    pub added: Vec<DependencyEdge>,
    pub removed: Vec<DependencyEdge>,
    pub detected_at: DateTime<Utc>,
}

/// The live dependency graph, built incrementally from discovery events.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DependencyGraph {
    pub edges: HashSet<DependencyEdge>,
}

impl DependencyGraph {
    /// Render the graph in Graphviz DOT format for visualization tooling.
    pub fn to_dot(&self) -> String {
        let mut out = String::from("digraph services {\n");
        for edge in &self.edges {
            out.push_str(&format!(
                "  \"{}\" -> \"{}\" [label=\"{:?}\"];\n",
                edge.from, edge.to, edge.kind
            ));
        }
        out.push_str("}\n");
        out
    }

    /// Services that `service` directly depends on.
    pub fn downstream_of(&self, service: &str) -> Vec<String> {
        self.edges
            .iter()
            .filter(|e| e.from == service)
            .map(|e| e.to.clone())
            .collect()
    }

    /// Services that directly depend on `service`.
    pub fn upstream_of(&self, service: &str) -> Vec<String> {
        self.edges
            .iter()
            .filter(|e| e.to == service)
            .map(|e| e.from.clone())
            .collect()
    }

    /// Transitive upstream closure of `service`: every service that would be
    /// affected, directly or indirectly, if `service` failed.
    pub fn impacted_by(&self, service: &str) -> Vec<String> {
        let mut visited: HashSet<String> = HashSet::new();
        let mut frontier = vec![service.to_string()];

        while let Some(current) = frontier.pop() {
            for upstream in self.upstream_of(&current) {
                if visited.insert(upstream.clone()) {
                    frontier.push(upstream);
                }
            }
        }

        visited.into_iter().collect()
    }

    /// Diff this graph against `previous`, returning added/removed edges.
    pub fn diff(&self, previous: &DependencyGraph) -> DependencyChange {
        let added = self
            .edges
            .difference(&previous.edges)
            .cloned()
            .collect();
        let removed = previous
            .edges
            .difference(&self.edges)
            .cloned()
            .collect();

        DependencyChange {
            added,
            removed,
            detected_at: Utc::now(),
        }
    }
}

/// Request body for `POST /dependencies/discover` — records one observed
/// call between services (e.g. reported by an instrumentation middleware).
#[derive(Debug, Deserialize)]
pub struct DiscoverDependencyRequest {
    pub from: String,
    pub to: String,
    pub kind: DependencyKind,
}

/// Request body for `POST /dependencies/impact`.
#[derive(Debug, Deserialize)]
pub struct ImpactAnalysisRequest {
    pub service: String,
}

/// Response for impact analysis.
#[derive(Debug, Serialize)]
pub struct ImpactAnalysisResponse {
    pub service: String,
    pub direct_downstream: Vec<String>,
    pub direct_upstream: Vec<String>,
    pub transitive_impact: Vec<String>,
}

pub type DependencyGraphHandle = Arc<Mutex<DependencyGraph>>;

#[derive(Clone)]
pub struct DependencyMapState {
    pub graph: DependencyGraphHandle,
    /// History of change snapshots, most recent last.
    pub changes: Arc<Mutex<Vec<DependencyChange>>>,
}

impl DependencyMapState {
    pub fn new() -> Self {
        Self {
            graph: Arc::new(Mutex::new(DependencyGraph::default())),
            changes: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl Default for DependencyMapState {
    fn default() -> Self {
        Self::new()
    }
}

/// `POST /dependencies/discover` — record an observed dependency edge,
/// detecting and storing any change versus the prior graph snapshot.
pub async fn discover_dependencies(
    State(state): State<Arc<DependencyMapState>>,
    Json(body): Json<DiscoverDependencyRequest>,
) -> (StatusCode, Json<DiscoveryEvent>) {
    let edge = DependencyEdge {
        from: body.from,
        to: body.to,
        kind: body.kind,
    };

    let mut graph = state.graph.lock().unwrap();
    let previous = graph.clone();
    let is_new = graph.edges.insert(edge.clone());

    if is_new {
        let change = graph.diff(&previous);
        tracing::info!(
            added = change.added.len(),
            removed = change.removed.len(),
            "dependency graph changed"
        );
        state.changes.lock().unwrap().push(change);
    }

    (
        StatusCode::CREATED,
        Json(DiscoveryEvent {
            edge,
            observed_at: Utc::now(),
        }),
    )
}

/// `GET /dependencies/graph` — return the current graph, with a DOT export
/// alongside the raw edge list for use by visualization frontends.
pub async fn get_dependency_graph(
    State(state): State<Arc<DependencyMapState>>,
) -> Json<serde_json::Value> {
    let graph = state.graph.lock().unwrap();
    Json(serde_json::json!({
        "edges": graph.edges,
        "dot": graph.to_dot(),
    }))
}

/// `POST /dependencies/impact` — analyze the blast radius of a service
/// failing, combining direct edges with the transitive upstream closure.
pub async fn analyze_impact(
    State(state): State<Arc<DependencyMapState>>,
    Json(body): Json<ImpactAnalysisRequest>,
) -> Json<ImpactAnalysisResponse> {
    let graph = state.graph.lock().unwrap();
    Json(ImpactAnalysisResponse {
        direct_downstream: graph.downstream_of(&body.service),
        direct_upstream: graph.upstream_of(&body.service),
        transitive_impact: graph.impacted_by(&body.service),
        service: body.service,
    })
}

/// `GET /dependencies/changes` — list detected changes to the dependency
/// graph over time, most recent last.
pub async fn get_recent_changes(
    State(state): State<Arc<DependencyMapState>>,
) -> Json<Vec<DependencyChange>> {
    let changes = state.changes.lock().unwrap();
    Json(changes.clone())
}
