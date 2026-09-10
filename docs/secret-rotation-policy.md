# Secret Rotation Policy (#103)

Heirloom-Protocol enforces a rotation schedule for all long-lived secrets.
Rotation minimises the blast radius of a compromise: an exposed secret is
usable only until it is rotated out and the grace period expires.

## Secret types and default schedules

| Secret type           | Rotation interval | Grace period | Auto-rotate |
|-----------------------|-------------------|--------------|-------------|
| `api_key`             | 90 days           | 24 hours     | disabled    |
| `database_password`   | 30 days           | 2 hours      | disabled    |
| `encryption_key`      | 365 days          | 48 hours     | disabled    |
| `jwt_secret`          | 30 days           | 1 hour       | disabled    |
| `webhook_secret`      | 90 days           | 24 hours     | disabled    |
| `reminders_api_key`   | 90 days           | 24 hours     | disabled    |

Default policies are seeded automatically on first startup.  They can be
overridden via the REST API.

## Grace period

During the grace period after a rotation, both the old and new values of a
secret are accepted.  This allows in-flight requests and downstream services
to drain before the old value is invalidated.

The grace period start and end time are recorded in `secret_rotation_logs`
so operators can see exactly when a period is active.

## Rotation process

### Manual rotation (recommended)

1. Generate the new secret value outside of the API (environment variable,
   secrets manager, or vault).
2. Update the secret in **all** consumers (services, `.env`, CI secrets, etc.).
3. Call the rotation API to record the event and start the grace period:

```bash
curl -X POST https://api.example.com/api/secret-rotation/api_key/rotate \
  -H "Authorization: Bearer $ADMIN_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"notes": "Routine 90-day rotation"}'
```

4. Monitor the grace period end time from the response.
5. After the grace period, remove the old secret value from all consumers.

### Automated rotation (`auto_rotate: true`)

When `auto_rotate` is enabled the background scheduler (hourly) detects
overdue secrets and records a rotation event automatically.  In the current
implementation this logs the event and fires notifications; actual secret
material rotation must be performed by a secrets-manager integration layer
(e.g. AWS Secrets Manager, HashiCorp Vault).

Enable auto-rotation:

```bash
curl -X PUT https://api.example.com/api/secret-rotation/policies/api_key \
  -H "Authorization: Bearer $ADMIN_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"rotation_interval_days": 90, "auto_rotate": true, "notify_channels": ["log"]}'
```

## Notifications

The `notify_channels` field on a policy controls where rotation alerts are
sent.  Currently supported channels:

| Channel  | Behaviour                                    |
|----------|----------------------------------------------|
| `log`    | Structured `tracing::info!` log entry        |

Additional channels (email, Slack, webhook) can be added by extending
`notify_rotation` in `backend/src/secret_rotation.rs`.

## REST API

### Policies

```
GET  /api/secret-rotation/policies                  — list all policies
GET  /api/secret-rotation/policies/:secret_type     — get one policy
PUT  /api/secret-rotation/policies/:secret_type     — upsert (admin)
```

**PUT body:**

```json
{
  "rotation_interval_days": 90,
  "grace_period_hours": 24,
  "auto_rotate": false,
  "notify_channels": ["log"]
}
```

### Status

```
GET  /api/secret-rotation/status                    — all secret statuses
GET  /api/secret-rotation/:secret_type/status       — single secret status
```

Status response:

```json
{
  "secret_type": "api_key",
  "last_rotated_at": "2026-04-27T10:00:00Z",
  "next_rotation_due": "2026-07-26T10:00:00Z",
  "is_overdue": false,
  "grace_period_active": false,
  "grace_period_ends_at": null
}
```

### Rotation

```
POST /api/secret-rotation/:secret_type/rotate       — record rotation (admin)
GET  /api/secret-rotation/:secret_type/history      — rotation history
```

**POST body:**

```json
{ "notes": "Optional description of this rotation" }
```

## Authorization

Mutating endpoints require `Authorization: Bearer <ADMIN_API_KEY>`.

## Scheduler

The rotation scheduler runs hourly inside the background task (`scheduler::run`).
It logs a warning for overdue secrets and, if `auto_rotate` is enabled, records
a system rotation event.
