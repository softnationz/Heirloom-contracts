use axum::{http::StatusCode, response::IntoResponse, Json};
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

/// Unified JSON error body returned by all handlers.
///
/// `context` is populated on a best-effort basis by handlers that opt into
/// error enrichment (see `error_context::ErrorContext`); it is omitted from
/// the JSON body entirely when absent, so existing consumers of this type
/// see no change. See `docs/error-format.md` for the full shape.
#[derive(Debug, Serialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
    pub details: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<crate::error_context::ErrorContext>,
    #[serde(skip)]
    status: StatusCode,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
            details: None,
            context: None,
            status,
        }
    }

    /// Attaches request/user/correlation-id context to this error response.
    pub fn with_context(mut self, context: crate::error_context::ErrorContext) -> Self {
        self.context = Some(context);
        self
    }

    /// The HTTP status this error responds with.
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// The machine-readable error code.
    pub fn code(&self) -> &str {
        &self.code
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let status = self.status;
        (status, Json(self)).into_response()
    }
}

#[derive(Debug, Error)]
pub enum AppError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("database error")]
    DatabaseError,
    #[error("not found")]
    NotFound,
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("2FA required")]
    TwoFactorRequired,
    #[error("2FA not enabled")]
    TwoFactorNotEnabled,
    /// Wraps an [`ApiError`] that already carries its own HTTP status and
    /// JSON body shape (e.g. from `audit::authorize_admin`); rendered as-is.
    #[error("{}", .0.message)]
    Api(ApiError),
}

impl From<ApiError> for AppError {
    fn from(value: ApiError) -> Self {
        // Manual impl (rather than #[from]) because ApiError intentionally
        // implements IntoResponse but not std::error::Error.
        AppError::Api(value)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        if let AppError::Api(api) = self {
            return api.into_response();
        }
        let (status, code) = match &self {
            AppError::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            AppError::InvalidInput(_) => (StatusCode::UNPROCESSABLE_ENTITY, "invalid_input"),
            AppError::Db(_) | AppError::DatabaseError => {
                (StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
            }
            AppError::TwoFactorRequired => (StatusCode::UNAUTHORIZED, "two_factor_required"),
            AppError::TwoFactorNotEnabled => (StatusCode::BAD_REQUEST, "two_factor_not_enabled"),
            AppError::Api(_) => unreachable!("handled above"),
        };
        ApiError::new(status, code, self.to_string()).into_response()
    }
}
