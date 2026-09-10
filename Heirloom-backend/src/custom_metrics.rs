//! Custom metric collection and Grafana dashboard support (issue: "Only
//! standard metrics are tracked. Custom dashboards would enable business
//! insights.").
//!
//! `metrics.rs` exposes a fixed set of Prometheus counters/gauges. This
//! module lets callers record arbitrary named, tagged metric points at
//! runtime, aggregate them (sum/avg/min/max/count), and ships ready-made
//! Grafana dashboard templates plus a lightweight sharing mechanism so a
//! generated dashboard link can be handed to a teammate.
//!
//! See `docs/custom-dashboards.md` for end-to-end usage.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A single recorded data point for a custom metric.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricPoint {
    pub timestamp: DateTime<Utc>,
    pub value: f64,
    #[serde(default)]
    pub tags: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct RecordMetricRequest {
    pub name: String,
    pub value: f64,
    #[serde(default)]
    pub tags: HashMap<String, String>,
}

/// Supported aggregation functions for custom metric queries.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Aggregation {
    Sum,
    Avg,
    Min,
    Max,
    Count,
}

impl Aggregation {
    fn apply(self, points: &[MetricPoint]) -> f64 {
        if points.is_empty() {
            return 0.0;
        }
        match self {
            Aggregation::Sum => points.iter().map(|p| p.value).sum(),
            Aggregation::Avg => points.iter().map(|p| p.value).sum::<f64>() / points.len() as f64,
            Aggregation::Min => points.iter().map(|p| p.value).fold(f64::INFINITY, f64::min),
            Aggregation::Max => points
                .iter()
                .map(|p| p.value)
                .fold(f64::NEG_INFINITY, f64::max),
            Aggregation::Count => points.len() as f64,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct AggregateQuery {
    #[serde(default = "default_aggregation")]
    pub agg: Aggregation,
}

fn default_aggregation() -> Aggregation {
    Aggregation::Avg
}

#[derive(Debug, Serialize)]
pub struct AggregateResponse {
    pub name: String,
    pub aggregation: String,
    pub value: f64,
    pub sample_count: usize,
}

/// A saved dashboard share: a stable token pointing at a named dashboard so
/// the underlying template/metric selection can change without breaking
/// links already handed out.
#[derive(Debug, Clone, Serialize)]
pub struct DashboardShare {
    pub token: String,
    pub dashboard: String,
    pub created_at: DateTime<Utc>,
    pub created_by: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateShareRequest {
    pub dashboard: String,
    #[serde(default)]
    pub created_by: Option<String>,
}

#[derive(Default)]
struct Inner {
    series: HashMap<String, Vec<MetricPoint>>,
    shares: HashMap<String, DashboardShare>,
}

/// Shared store for custom metric series and dashboard shares.
#[derive(Default)]
pub struct CustomMetricsStore {
    inner: RwLock<Inner>,
}

impl CustomMetricsStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn record(&self, name: &str, value: f64, tags: HashMap<String, String>) {
        let mut inner = self.inner.write().expect("metrics lock poisoned");
        inner.series.entry(name.to_string()).or_default().push(MetricPoint {
            timestamp: Utc::now(),
            value,
            tags,
        });
    }

    pub fn aggregate(&self, name: &str, agg: Aggregation) -> Option<AggregateResponse> {
        let inner = self.inner.read().expect("metrics lock poisoned");
        let points = inner.series.get(name)?;
        Some(AggregateResponse {
            name: name.to_string(),
            aggregation: format!("{agg:?}").to_lowercase(),
            value: agg.apply(points),
            sample_count: points.len(),
        })
    }

    pub fn list_metric_names(&self) -> Vec<String> {
        self.inner
            .read()
            .expect("metrics lock poisoned")
            .series
            .keys()
            .cloned()
            .collect()
    }

    pub fn create_share(&self, req: CreateShareRequest) -> DashboardShare {
        let share = DashboardShare {
            token: Uuid::new_v4().to_string(),
            dashboard: req.dashboard,
            created_at: Utc::now(),
            created_by: req.created_by,
        };
        self.inner
            .write()
            .expect("metrics lock poisoned")
            .shares
            .insert(share.token.clone(), share.clone());
        share
    }

    pub fn resolve_share(&self, token: &str) -> Option<DashboardShare> {
        self.inner
            .read()
            .expect("metrics lock poisoned")
            .shares
            .get(token)
            .cloned()
    }
}

/// Built-in Grafana dashboard templates. These are minimal but valid
/// dashboard JSON models (schema v36) covering the business metrics teams
/// most commonly ask for: vault lifecycle throughput and custom metric
/// exploration. Import them directly in Grafana ("Import dashboard from
/// JSON") or provision them via the Grafana provisioning API.
pub fn grafana_dashboard_templates() -> HashMap<&'static str, serde_json::Value> {
    let mut templates = HashMap::new();

    templates.insert(
        "vault-lifecycle",
        serde_json::json!({
            "title": "Heirloom Protocol - Vault Lifecycle",
            "schemaVersion": 36,
            "panels": [
                {
                    "type": "timeseries",
                    "title": "Vaults created / released",
                    "targets": [
                        {"expr": "rate(heirloom_protocol_vaults_total[5m])"},
                        {"expr": "rate(heirloom_protocol_releases_total[5m])"}
                    ]
                },
                {
                    "type": "stat",
                    "title": "Active vaults",
                    "targets": [{"expr": "heirloom_protocol_active_vaults"}]
                }
            ]
        }),
    );

    templates.insert(
        "custom-metric-explorer",
        serde_json::json!({
            "title": "Heirloom Protocol - Custom Metric Explorer",
            "schemaVersion": 36,
            "templating": {
                "list": [{"name": "metric_name", "type": "textbox"}]
            },
            "panels": [
                {
                    "type": "timeseries",
                    "title": "$metric_name over time",
                    "targets": [{"expr": "custom_metric{name=\"$metric_name\"}"}]
                }
            ]
        }),
    );

    templates
}

/// `POST /metrics/custom` - record a business metric point.
pub async fn record_custom_metric(
    State(store): State<Arc<CustomMetricsStore>>,
    Json(req): Json<RecordMetricRequest>,
) -> impl IntoResponse {
    store.record(&req.name, req.value, req.tags);
    StatusCode::ACCEPTED
}

/// `GET /metrics/custom` - list known custom metric names.
pub async fn list_custom_metrics(
    State(store): State<Arc<CustomMetricsStore>>,
) -> impl IntoResponse {
    Json(store.list_metric_names())
}

/// `GET /metrics/custom/:name/aggregate?agg=avg|sum|min|max|count`.
pub async fn aggregate_custom_metric(
    State(store): State<Arc<CustomMetricsStore>>,
    Path(name): Path<String>,
    Query(q): Query<AggregateQuery>,
) -> impl IntoResponse {
    match store.aggregate(&name, q.agg) {
        Some(resp) => (StatusCode::OK, Json(resp)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// `GET /dashboards/templates` - list built-in Grafana dashboard templates.
pub async fn list_dashboard_templates() -> impl IntoResponse {
    Json(grafana_dashboard_templates())
}

/// `POST /dashboards/share` - create a shareable link for a dashboard.
pub async fn create_dashboard_share(
    State(store): State<Arc<CustomMetricsStore>>,
    Json(req): Json<CreateShareRequest>,
) -> impl IntoResponse {
    (StatusCode::CREATED, Json(store.create_share(req)))
}

/// `GET /dashboards/shared/:token` - resolve a shared dashboard link.
pub async fn get_shared_dashboard(
    State(store): State<Arc<CustomMetricsStore>>,
    Path(token): Path<String>,
) -> impl IntoResponse {
    match store.resolve_share(&token) {
        Some(share) => (StatusCode::OK, Json(share)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregates_sum_avg_min_max_count() {
        let store = CustomMetricsStore::default();
        for v in [1.0, 2.0, 3.0, 4.0] {
            store.record("checkout_latency_ms", v, HashMap::new());
        }

        assert_eq!(
            store.aggregate("checkout_latency_ms", Aggregation::Sum).unwrap().value,
            10.0
        );
        assert_eq!(
            store.aggregate("checkout_latency_ms", Aggregation::Avg).unwrap().value,
            2.5
        );
        assert_eq!(
            store.aggregate("checkout_latency_ms", Aggregation::Min).unwrap().value,
            1.0
        );
        assert_eq!(
            store.aggregate("checkout_latency_ms", Aggregation::Max).unwrap().value,
            4.0
        );
        assert_eq!(
            store
                .aggregate("checkout_latency_ms", Aggregation::Count)
                .unwrap()
                .sample_count,
            4
        );
    }

    #[test]
    fn unknown_metric_returns_none() {
        let store = CustomMetricsStore::default();
        assert!(store.aggregate("does-not-exist", Aggregation::Avg).is_none());
    }

    #[test]
    fn dashboard_templates_are_present_and_valid_json() {
        let templates = grafana_dashboard_templates();
        assert!(templates.contains_key("vault-lifecycle"));
        assert!(templates.contains_key("custom-metric-explorer"));
    }

    #[test]
    fn dashboard_share_round_trips() {
        let store = CustomMetricsStore::default();
        let share = store.create_share(CreateShareRequest {
            dashboard: "vault-lifecycle".to_string(),
            created_by: Some("alice".to_string()),
        });
        let resolved = store.resolve_share(&share.token).expect("share should resolve");
        assert_eq!(resolved.dashboard, "vault-lifecycle");
    }
}
