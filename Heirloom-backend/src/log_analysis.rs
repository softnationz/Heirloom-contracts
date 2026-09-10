//! Structured log parsing, field extraction, pattern matching, and search
//! (issue: "Logs are unstructured and hard to parse. Analysis would enable
//! log-based debugging.").
//!
//! Accepts raw log lines in the common
//! `TIMESTAMP LEVEL target: message key=value key2="quoted value"` shape
//! (matching what `tracing`/`env_logger` emit), extracts structured fields
//! from them, stores them, and exposes search/pattern-matching over the
//! stored entries.
//!
//! See `docs/log-format.md` for the exact format and search semantics.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use axum::{
    extract::{Query, State},
    response::IntoResponse,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A structured, parsed representation of one log line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub raw: String,
    pub timestamp: Option<DateTime<Utc>>,
    pub level: Option<String>,
    pub target: Option<String>,
    pub message: String,
    pub fields: HashMap<String, String>,
}

/// Parse one raw log line into a [`LogEntry`].
///
/// Expected shape (each part optional, parsed best-effort):
/// `2026-07-26T10:00:00Z INFO checkin_handler: vault released id=42 region="eu-west-1"`
///
/// - First whitespace-delimited token is tried as an RFC3339 timestamp.
/// - Next token is treated as the level if it is one of the standard
///   tracing/log levels (case-insensitive).
/// - If the following token ends in `:`, it is treated as the target.
/// - Remaining `key=value` / `key="quoted value"` pairs are extracted into
///   `fields`; everything else becomes `message`.
pub fn parse_log_line(line: &str) -> LogEntry {
    let raw = line.to_string();
    let mut rest: Vec<&str> = line.split_whitespace().collect();

    let timestamp = rest
        .first()
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|dt| dt.with_timezone(&Utc));
    if timestamp.is_some() {
        rest.remove(0);
    }

    let level = rest.first().and_then(|t| {
        let upper = t.to_uppercase();
        matches!(upper.as_str(), "TRACE" | "DEBUG" | "INFO" | "WARN" | "ERROR").then_some(upper)
    });
    if level.is_some() {
        rest.remove(0);
    }

    let target = rest.first().and_then(|t| {
        t.ends_with(':').then(|| t.trim_end_matches(':').to_string())
    });
    if target.is_some() {
        rest.remove(0);
    }

    let mut fields = HashMap::new();
    let mut message_parts = Vec::new();
    let mut iter = rest.into_iter().peekable();
    while let Some(token) = iter.next() {
        if let Some((key, mut value)) = token.split_once('=') {
            if value.starts_with('"') && !value.ends_with('"') {
                // Reassemble a quoted value split across whitespace tokens.
                let mut joined = value.to_string();
                while let Some(next) = iter.peek() {
                    joined.push(' ');
                    joined.push_str(next);
                    let done = next.ends_with('"');
                    iter.next();
                    if done {
                        break;
                    }
                }
                fields.insert(key.to_string(), joined.trim_matches('"').to_string());
                continue;
            }
            value = value.trim_matches('"');
            fields.insert(key.to_string(), value.to_string());
        } else {
            message_parts.push(token);
        }
    }

    LogEntry {
        raw,
        timestamp,
        level,
        target,
        message: message_parts.join(" "),
        fields,
    }
}

/// Match `text` against a simple glob-style `pattern` where `*` matches any
/// run of characters. Matching is case-insensitive.
pub fn matches_pattern(text: &str, pattern: &str) -> bool {
    let text = text.to_lowercase();
    let pattern = pattern.to_lowercase();
    if !pattern.contains('*') {
        return text.contains(&pattern);
    }

    let segments: Vec<&str> = pattern.split('*').collect();
    let mut cursor = 0usize;
    for (i, seg) in segments.iter().enumerate() {
        if seg.is_empty() {
            continue;
        }
        match text[cursor..].find(seg) {
            Some(pos) => {
                if i == 0 && pos != 0 && !pattern.starts_with('*') {
                    return false;
                }
                cursor += pos + seg.len();
            }
            None => return false,
        }
    }
    if let Some(last) = segments.last() {
        if !pattern.ends_with('*') && !last.is_empty() && !text.ends_with(last) {
            return false;
        }
    }
    true
}

#[derive(Debug, Deserialize)]
pub struct IngestRequest {
    pub lines: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub level: Option<String>,
    pub query: Option<String>,
    pub pattern: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    100
}

#[derive(Default)]
struct Inner {
    entries: Vec<LogEntry>,
}

/// Shared store of parsed structured log entries.
#[derive(Default)]
pub struct LogStore {
    inner: RwLock<Inner>,
}

impl LogStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn ingest_line(&self, line: &str) -> LogEntry {
        let entry = parse_log_line(line);
        self.inner
            .write()
            .expect("log store lock poisoned")
            .entries
            .push(entry.clone());
        entry
    }

    pub fn search(&self, q: &SearchQuery) -> Vec<LogEntry> {
        let inner = self.inner.read().expect("log store lock poisoned");
        inner
            .entries
            .iter()
            .rev()
            .filter(|e| {
                q.level
                    .as_ref()
                    .map(|lvl| e.level.as_deref() == Some(lvl.to_uppercase().as_str()))
                    .unwrap_or(true)
            })
            .filter(|e| {
                q.query
                    .as_ref()
                    .map(|text| {
                        e.message.to_lowercase().contains(&text.to_lowercase())
                            || e.raw.to_lowercase().contains(&text.to_lowercase())
                    })
                    .unwrap_or(true)
            })
            .filter(|e| {
                q.pattern
                    .as_ref()
                    .map(|pat| matches_pattern(&e.raw, pat))
                    .unwrap_or(true)
            })
            .take(q.limit)
            .cloned()
            .collect()
    }
}

/// `POST /logs/ingest` - parse and store one or more raw log lines.
pub async fn ingest_logs(
    State(store): State<Arc<LogStore>>,
    Json(req): Json<IngestRequest>,
) -> impl IntoResponse {
    let parsed: Vec<LogEntry> = req.lines.iter().map(|l| store.ingest_line(l)).collect();
    Json(parsed)
}

/// `GET /logs/search?level=&query=&pattern=&limit=` - search stored logs.
pub async fn search_logs(
    State(store): State<Arc<LogStore>>,
    Query(q): Query<SearchQuery>,
) -> impl IntoResponse {
    Json(store.search(&q))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_timestamp_level_target_and_fields() {
        let entry = parse_log_line(
            r#"2026-07-26T10:00:00Z INFO checkin_handler: vault released id=42 region="eu-west-1""#,
        );
        assert!(entry.timestamp.is_some());
        assert_eq!(entry.level, Some("INFO".to_string()));
        assert_eq!(entry.target, Some("checkin_handler".to_string()));
        assert_eq!(entry.message, "vault released");
        assert_eq!(entry.fields.get("id"), Some(&"42".to_string()));
        assert_eq!(entry.fields.get("region"), Some(&"eu-west-1".to_string()));
    }

    #[test]
    fn parses_line_missing_optional_parts() {
        let entry = parse_log_line("just a plain message with no metadata");
        assert!(entry.timestamp.is_none());
        assert!(entry.level.is_none());
        assert!(entry.target.is_none());
        assert_eq!(entry.message, "just a plain message with no metadata");
    }

    #[test]
    fn pattern_matching_supports_wildcards() {
        assert!(matches_pattern("vault checkin failed for id=7", "vault*failed*"));
        assert!(matches_pattern("ERROR: timeout", "*timeout"));
        assert!(!matches_pattern("all good here", "*failed*"));
    }

    #[test]
    fn search_filters_by_level_and_query() {
        let store = LogStore::default();
        store.ingest_line("2026-07-26T10:00:00Z INFO svc: vault created id=1");
        store.ingest_line("2026-07-26T10:00:05Z ERROR svc: vault release failed id=2");

        let results = store.search(&SearchQuery {
            level: Some("error".to_string()),
            query: None,
            pattern: None,
            limit: 10,
        });
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].fields.get("id"), Some(&"2".to_string()));

        let results = store.search(&SearchQuery {
            level: None,
            query: Some("created".to_string()),
            pattern: None,
            limit: 10,
        });
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].fields.get("id"), Some(&"1".to_string()));
    }

    #[test]
    fn search_respects_limit_and_returns_newest_first() {
        let store = LogStore::default();
        store.ingest_line("2026-07-26T10:00:00Z INFO svc: first");
        store.ingest_line("2026-07-26T10:00:01Z INFO svc: second");

        let results = store.search(&SearchQuery {
            level: None,
            query: None,
            pattern: None,
            limit: 1,
        });
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].message, "second");
    }
}
