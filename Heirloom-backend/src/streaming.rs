//! Query result streaming (#67).
//!
//! Large result sets are streamed as Newline-Delimited JSON (NDJSON) so that
//! clients can process records incrementally without waiting for the full
//! payload to be buffered in memory.
//!
//! # Protocol
//!
//! Send `Accept: application/x-ndjson` on any supported endpoint to opt into
//! streaming mode.  Each line of the response body is a valid JSON object
//! terminated by `\n`.  The final line is a *cursor* envelope:
//!
//! ```json
//! {"cursor":"<opaque-token>","has_more":true}
//! ```
//!
//! Pass `?cursor=<token>` on subsequent requests to fetch the next page.
//!
//! # Endpoints
//!
//! ```text
//! GET /stream/vaults?cursor=<token>&limit=<n>   — stream vault records
//! GET /stream/events?vault_id=<id>&cursor=<token>&limit=<n>   — stream events
//! ```

use std::convert::Infallible;
use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use futures::stream;
use serde::{Deserialize, Serialize};

use crate::db::AppState;
use crate::models::{Vault, VaultEvent};

// ── Cursor encoding ───────────────────────────────────────────────────────────

/// An opaque, URL-safe cursor that encodes an offset into a result set.
///
/// The cursor is just a base64url-encoded JSON object so it's easy to inspect
/// and impossible for clients to forge meaningfully (no secret is needed here
/// because the in-memory stores don't have delete semantics that would create
/// security issues with replayed cursors).
#[derive(Debug, Serialize, Deserialize)]
struct CursorPayload {
    pub offset: usize,
}

fn encode_cursor(offset: usize) -> String {
    let payload = CursorPayload { offset };
    let json = serde_json::to_string(&payload).expect("cursor serialisation is infallible");
    URL_SAFE_NO_PAD.encode(json.as_bytes())
}

fn decode_cursor(cursor: &str) -> Option<usize> {
    let bytes = URL_SAFE_NO_PAD.decode(cursor).ok()?;
    let payload: CursorPayload = serde_json::from_slice(&bytes).ok()?;
    Some(payload.offset)
}

// ── Query parameters ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct StreamQuery {
    /// Opaque cursor returned by the previous response.
    pub cursor: Option<String>,
    /// Maximum records per page (default 50, max 500).
    pub limit: Option<usize>,
    /// Filter events by vault ID (only used on `/stream/events`).
    pub vault_id: Option<String>,
    /// Filter vaults by owner address.
    pub owner: Option<String>,
}

// ── Streaming response builder ────────────────────────────────────────────────

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 500;

/// Checks whether the client requested NDJSON streaming.
pub fn wants_ndjson(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.contains("application/x-ndjson"))
        .unwrap_or(false)
}

/// Build a streaming NDJSON [`Response`] from a vec of serialisable items.
///
/// Each item is serialised to a single line ending with `\n`.  After all items
/// a final metadata line `{"cursor":"…","has_more":…}` is emitted.
fn ndjson_response<T: Serialize + Send + 'static>(
    items: Vec<T>,
    next_offset: usize,
    has_more: bool,
) -> Response {
    let mut lines: Vec<axum::body::Bytes> = items
        .into_iter()
        .filter_map(|item| {
            serde_json::to_string(&item).ok().map(|mut s| {
                s.push('\n');
                axum::body::Bytes::from(s.into_bytes())
            })
        })
        .collect();

    // Cursor / pagination envelope as the last line.
    let cursor = encode_cursor(next_offset);
    let meta = format!(
        "{}\n",
        serde_json::json!({ "cursor": cursor, "has_more": has_more })
    );
    lines.push(axum::body::Bytes::from(meta.into_bytes()));

    let stream = stream::iter(lines.into_iter().map(Ok::<_, Infallible>));
    let body = Body::from_stream(stream);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .body(body)
        .expect("response builder should not fail")
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// `GET /stream/vaults` — stream vault records as NDJSON.
///
/// Supports both regular JSON (`Accept: application/json`) and NDJSON streaming
/// (`Accept: application/x-ndjson`).  In JSON mode the full page is returned as
/// a standard JSON array with a `cursor` field.
pub async fn stream_vaults(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<StreamQuery>,
) -> Response {
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
    let offset = query.cursor.as_deref().and_then(decode_cursor).unwrap_or(0);

    let vaults_guard = state.vault_store.lock().unwrap();
    let filtered: Vec<Vault> = vaults_guard
        .values()
        .filter(|v| {
            if let Some(ref owner) = query.owner {
                v.owner == *owner
            } else {
                true
            }
        })
        .cloned()
        .collect();

    drop(vaults_guard);

    let total = filtered.len();
    let page: Vec<Vault> = filtered.into_iter().skip(offset).take(limit).collect();
    let next_offset = offset + page.len();
    let has_more = next_offset < total;

    if wants_ndjson(&headers) {
        ndjson_response(page, next_offset, has_more)
    } else {
        // Plain JSON fallback with cursor metadata.
        Json(serde_json::json!({
            "vaults": page,
            "cursor": encode_cursor(next_offset),
            "has_more": has_more,
            "total": total,
        }))
        .into_response()
    }
}

/// `GET /stream/events` — stream vault events as NDJSON.
///
/// Optionally filtered by `vault_id`.
pub async fn stream_events(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<StreamQuery>,
) -> Response {
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
    let offset = query.cursor.as_deref().and_then(decode_cursor).unwrap_or(0);

    let events_guard = state.event_store.lock().unwrap();
    let filtered: Vec<VaultEvent> = events_guard
        .iter()
        .filter(|e| {
            if let Some(ref vid) = query.vault_id {
                e.vault_id == *vid
            } else {
                true
            }
        })
        .cloned()
        .collect();

    drop(events_guard);

    let total = filtered.len();
    let page: Vec<VaultEvent> = filtered.into_iter().skip(offset).take(limit).collect();
    let next_offset = offset + page.len();
    let has_more = next_offset < total;

    if wants_ndjson(&headers) {
        ndjson_response(page, next_offset, has_more)
    } else {
        Json(serde_json::json!({
            "events": page,
            "cursor": encode_cursor(next_offset),
            "has_more": has_more,
            "total": total,
        }))
        .into_response()
    }
}
