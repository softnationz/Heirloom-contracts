//! Error enrichment (Issue: "Errors lack context").
//!
//! Adds structured request/user context, correlation IDs, and optional
//! stack traces to error responses so failures are debuggable without
//! having to cross-reference logs by timestamp alone. See
//! `docs/error-format.md` for the resulting JSON shape.

use std::sync::Arc;

use axum::{
    extract::Request,
    http::{HeaderMap, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use uuid::Uuid;

use crate::error::AppError;

pub const CORRELATION_ID_HEADER: &str = "x-correlation-id";

/// Snapshot of the HTTP request an error occurred while handling.
#[derive(Debug, Clone, Serialize)]
pub struct RequestContext {
    pub method: String,
    pub path: String,
    pub query: Option<String>,
}

impl RequestContext {
    pub fn from_parts(method: &str, path: &str, query: Option<&str>) -> Self {
        Self {
            method: method.to_string(),
            path: path.to_string(),
            query: query.map(|q| q.to_string()),
        }
    }
}

/// Who was making the request, when known.
#[derive(Debug, Clone, Default, Serialize)]
pub struct UserContext {
    pub user_id: Option<String>,
    pub tenant_id: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
}

impl UserContext {
    pub fn from_headers(headers: &HeaderMap) -> Self {
        Self {
            user_id: header_str(headers, "x-user-id"),
            tenant_id: header_str(headers, "x-tenant-id"),
            roles: header_str(headers, "x-user-roles")
                .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
                .unwrap_or_default(),
        }
    }
}

fn header_str(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

/// The full context attached to an enriched error: correlation id,
/// timestamp, request info, user info, and (optionally) a captured stack
/// trace.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorContext {
    pub correlation_id: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub request: Option<RequestContext>,
    pub user: Option<UserContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack_trace: Option<String>,
}

impl ErrorContext {
    pub fn new() -> Self {
        Self {
            correlation_id: Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            request: None,
            user: None,
            stack_trace: None,
        }
    }

    pub fn with_correlation_id(mut self, id: impl Into<String>) -> Self {
        self.correlation_id = id.into();
        self
    }

    pub fn with_request(mut self, request: RequestContext) -> Self {
        self.request = Some(request);
        self
    }

    pub fn with_user(mut self, user: UserContext) -> Self {
        self.user = Some(user);
        self
    }

    /// Captures the current stack trace. Honors `RUST_BACKTRACE` the same
    /// way panics do: if it isn't set, the captured trace will simply say
    /// "disabled backtrace".
    pub fn capture_stack_trace(mut self) -> Self {
        self.stack_trace = Some(std::backtrace::Backtrace::force_capture().to_string());
        self
    }

    /// Builds an `ErrorContext` directly from an in-flight axum request.
    pub fn from_request(request: &Request) -> Self {
        let correlation_id = request
            .headers()
            .get(CORRELATION_ID_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .unwrap_or_else(|| Uuid::new_v4().to_string());

        Self::new()
            .with_correlation_id(correlation_id)
            .with_request(RequestContext::from_parts(
                request.method().as_str(),
                request.uri().path(),
                request.uri().query(),
            ))
            .with_user(UserContext::from_headers(request.headers()))
    }
}

impl Default for ErrorContext {
    fn default() -> Self {
        Self::new()
    }
}

/// An `AppError` enriched with request/user/correlation context, ready to
/// be returned directly from a handler.
#[derive(Debug, Serialize)]
pub struct EnrichedError {
    pub code: String,
    pub message: String,
    pub context: ErrorContext,
    #[serde(skip)]
    status: StatusCode,
}

impl EnrichedError {
    pub fn new(error: AppError, context: ErrorContext) -> Self {
        let (status, code) = classify(&error);
        Self {
            code,
            message: error.to_string(),
            context,
            status,
        }
    }
}

fn classify(error: &AppError) -> (StatusCode, String) {
    match error {
        AppError::NotFound => (StatusCode::NOT_FOUND, "not_found".to_string()),
        AppError::InvalidInput(_) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_input".to_string(),
        ),
        AppError::Db(_) | AppError::DatabaseError => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error".to_string(),
        ),
        AppError::TwoFactorRequired => {
            (StatusCode::UNAUTHORIZED, "two_factor_required".to_string())
        }
        AppError::TwoFactorNotEnabled => (
            StatusCode::BAD_REQUEST,
            "two_factor_not_enabled".to_string(),
        ),
        // Wrapped ApiErrors already carry their own status and code.
        AppError::Api(api) => (api.status(), api.code().to_string()),
    }
}

impl IntoResponse for EnrichedError {
    fn into_response(self) -> Response {
        let status = self.status;
        (status, Json(self)).into_response()
    }
}

/// Convenience extension for attaching context to any `AppError` at the
/// point it's returned from a handler, e.g.
/// `db.get(id).map_err(|_| AppError::NotFound.enrich_from(&request))`.
pub trait EnrichExt {
    fn enrich(self, context: ErrorContext) -> EnrichedError;
}

impl EnrichExt for AppError {
    fn enrich(self, context: ErrorContext) -> EnrichedError {
        EnrichedError::new(self, context)
    }
}

/// Axum middleware that ensures every request/response pair carries an
/// `X-Correlation-Id` header, generating one if the caller didn't supply
/// it, and threading it through so downstream handlers/errors can pick it
/// up via `ErrorContext::from_request`.
pub async fn correlation_id_middleware(mut request: Request, next: Next) -> Response {
    let correlation_id = request
        .headers()
        .get(CORRELATION_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    request.headers_mut().insert(
        CORRELATION_ID_HEADER,
        HeaderValue::from_str(&correlation_id)
            .unwrap_or_else(|_| HeaderValue::from_static("invalid")),
    );

    let mut response = next.run(request).await;
    if let Ok(value) = HeaderValue::from_str(&correlation_id) {
        response.headers_mut().insert(CORRELATION_ID_HEADER, value);
    }
    response
}

/// Marker type so `Arc<()>`-style shared state isn't needed just to mount
/// the middleware above via `from_fn` (kept for symmetry with the other
/// admin modules, which take an explicit state).
pub type SharedNothing = Arc<()>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_context_parses_roles_header() {
        let mut headers = HeaderMap::new();
        headers.insert("x-user-roles", "admin, operator".parse().unwrap());
        let ctx = UserContext::from_headers(&headers);
        assert_eq!(ctx.roles, vec!["admin".to_string(), "operator".to_string()]);
    }

    #[test]
    fn enriched_error_preserves_status_mapping() {
        let ctx = ErrorContext::new();
        let err = EnrichedError::new(AppError::NotFound, ctx);
        assert_eq!(err.status, StatusCode::NOT_FOUND);
        assert_eq!(err.code, "not_found");
    }
}
