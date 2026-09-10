# Error Format

Errors previously carried only a `code`/`message`, making it hard to trace
a failure back to the request, user, or point in code that produced it.
`backend/src/error_context.rs` adds structured enrichment on top of the
existing `ApiError`/`AppError` types in `backend/src/error.rs`.

## JSON shape

```json
{
  "code": "not_found",
  "message": "not found",
  "details": null,
  "context": {
    "correlation_id": "b3b1c2b0-...-...",
    "timestamp": "2026-07-26T12:00:00Z",
    "request": {
      "method": "GET",
      "path": "/api/vaults/42/reminder-preferences",
      "query": null
    },
    "user": {
      "user_id": "0xabc123",
      "tenant_id": "tenant-1",
      "roles": ["admin"]
    },
    "stack_trace": "0: heirloom_protocol_backend::handlers::...\n..."
  }
}
```

`context` (and every field inside it) is optional and omitted from the
JSON body entirely when not populated, so existing consumers of
`ApiError` are unaffected.

## Correlation IDs

`correlation_id_middleware` (layered globally in `main.rs::build_router`)
ensures every request/response pair carries an `X-Correlation-Id` header:
the caller's own value is honored if present, otherwise one is generated.
`ErrorContext::from_request` reads this header so an error produced deep
in a handler can be tied back to the exact request that caused it, and to
any logs tagged with the same id.

## Request & user context

- `RequestContext` captures `method`, `path`, and `query`.
- `UserContext` is derived from `X-User-Id`, `X-Tenant-Id`, and
  `X-User-Roles` (comma-separated) headers when present.

## Stack traces

`ErrorContext::capture_stack_trace()` calls
`std::backtrace::Backtrace::force_capture()`, honoring `RUST_BACKTRACE`
the same way a panic would. Attach it for internal/500-class errors where
the extra detail is worth the cost; skip it for expected 4xx errors like
`NotFound` or `InvalidInput`.

## Usage

```rust
use heirloom_protocol_backend::error_context::{ErrorContext, EnrichExt};

async fn handler(request: Request /* or headers */) -> Result<Json<T>, EnrichedError> {
    let ctx = ErrorContext::from_request(&request).capture_stack_trace();
    db.get(id).map_err(|_| AppError::NotFound.enrich(ctx))
}
```

`AppError` itself is unchanged and still implements `IntoResponse`
directly for handlers that don't need enrichment — `EnrichedError` is an
opt-in wrapper for call sites that do.
