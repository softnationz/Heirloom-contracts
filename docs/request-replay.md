# API Request Replay Capability (#73)

The replay system records API request/response pairs and allows them to be re-executed later — useful for debugging, regression testing, and auditing.

## Architecture

```
Incoming Request
      │
      ▼
  API Handler ──────────────────► Response
      │                                │
      └── record_request(store, log) ──┘
              (async, non-blocking)
```

Logs are stored in an in-memory `RequestLogStore` (backed by `Arc<Mutex<Vec<RequestLog>>>`). In production, swap this for a persistent store (PostgreSQL, Redis stream, etc.).

## Endpoints

### `POST /replay` — replay a single request

```json
{
  "log_id": "550e8400-e29b-41d4-a716-446655440000",
  "conditions": [
    { "original_status_equals": 200 }
  ],
  "validate": true
}
```

**Conditions** are ANDed together — all must pass or the replay is skipped:

| Condition key | Effect |
|---|---|
| `always` | Always replay (default when no conditions provided) |
| `original_status_equals` | Only replay if the logged status equals this code |
| `path_contains` | Only replay if the logged path contains this substring |
| `body_key_equals` | Only replay if the request body `key` equals `value` |

**`validate: true`** (default) compares the replayed response to the original and marks the outcome as `identical` or `diverged`.

### `POST /replay/batch` — batch replay (up to 50)

```json
{
  "log_ids": ["id1", "id2"],
  "validate": true
}
```

Returns per-entry `ReplayResult` plus summary counts (`identical`, `diverged`, `skipped`).

### `GET /replay/logs` — list stored logs

Query parameters:
- `path` — filter by URL path prefix (e.g. `/api/vaults`)
- `limit` — max results (default 50, max 200)

### `GET /replay/logs/:log_id` — get a single log entry

## Replay outcomes

| Outcome | Meaning |
|---|---|
| `identical` | Replayed response status and body match the original |
| `diverged` | Status or body differs — `diff_notes` explains what changed |
| `skipped` | A condition check failed; the replay did not execute |
| `unvalidated` | Replay completed but `validate: false` was set |

## Replay semantics

1. **Idempotency**: Replay re-issues the same request against the same handler. Non-idempotent operations (e.g., `POST /jobs`) will create duplicate resources — use `original_status_equals: 200` or path conditions to limit scope.
2. **Authorization**: Headers captured in the log are replayed as-is. The `Authorization` header is **stripped** from logs to avoid replaying stale credentials.
3. **Time sensitivity**: TTL checks and timestamp comparisons will use the current time during replay, which may produce different results from the original.
4. **Admin access**: The `/replay` and `/replay/batch` endpoints should be restricted to admin roles in production via middleware.

## Example: replay a failed check-in for debugging

```bash
# 1. Find the failed log entry
curl "http://localhost:3000/replay/logs?path=/api/vaults&limit=20"

# 2. Replay only the 404 responses
curl -X POST http://localhost:3000/replay \
  -H "Content-Type: application/json" \
  -d '{
    "log_id": "550e8400-e29b-41d4-a716-446655440000",
    "conditions": [{ "original_status_equals": 404 }],
    "validate": true
  }'
```

## Integrating request logging

To log a request from a handler, call `record_request` with a `RequestLog`:

```rust
use heirloom_protocol_backend::replay::{record_request, RequestLog};

let log = RequestLog::new(
    "POST",
    "/api/vaults/42/check-in",
    captured_headers,
    Some(request_body_json),
    response_status,
    response_body_json,
    duration_ms,
    Some("check_in".into()),
);
record_request(&state.request_log_store, log);
```

The log store is accessible from `AppState::request_log_store`.
