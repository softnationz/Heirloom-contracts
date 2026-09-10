# Request Prioritization

Heirloom-Protocol backend requests previously had equal standing, so a burst of
low-value traffic could starve critical-path requests (vault release checks,
webhook delivery retries) of capacity. Clients can now declare relative
importance per-request, and the server enforces per-priority concurrency
budgets on the request path.

Implementation: `backend/src/priority.rs`.

## Declaring priority

Send the `X-Priority` header on any request:

```bash
curl -H "X-Priority: critical" http://localhost:3000/api/vaults/123/simulate-release
```

Supported values (case-insensitive): `low`, `normal`, `high`, `critical`.
Missing or unrecognized values default to `normal`, so existing clients keep
working unchanged.

## Priority-based queue

`priority::PriorityQueue<T>` is a thread-safe queue that dequeues the
highest-priority item first, and preserves FIFO order among items at the
same priority. It's available for ordering internal work (e.g. webhook or
notification dispatch) by declared `Priority`.

## Concurrency enforcement

`priority::PriorityEnforcer` bounds how many in-flight requests each
priority level may occupy at once. Requests over the limit for their
priority receive `429 Too Many Requests` with body:

```json
{ "code": "priority_limit_exceeded", "message": "...", "priority": "low" }
```

Enforcement happens in `admission_middleware` (`backend/src/load_shedding.rs`),
layered over every route in `build_router`, after load shedding (#128) has
had a chance to reject the request first.

## Configuration

Environment variables (all optional; `0` means unbounded):

| Variable | Default |
|---|---|
| `PRIORITY_LOW_MAX_CONCURRENT` | 50 |
| `PRIORITY_NORMAL_MAX_CONCURRENT` | 200 |
| `PRIORITY_HIGH_MAX_CONCURRENT` | 400 |
| `PRIORITY_CRITICAL_MAX_CONCURRENT` | 0 (unbounded) |

See also [`docs/load-shedding.md`](./load-shedding.md), which sheds lower
priority traffic first under overload.
