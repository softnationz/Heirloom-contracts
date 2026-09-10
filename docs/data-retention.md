# Data Retention Policies (#100)

Heirloom-Protocol applies configurable data retention policies to limit how long
different classes of records are kept.  Automated purging is performed daily by
the background scheduler; manual purges can be triggered via the REST API.

## Supported data types

| `data_type`              | Table                    | Timestamp column | Default retention |
|--------------------------|--------------------------|------------------|-------------------|
| `audit_logs`             | `audit_logs`             | `timestamp`      | 90 days           |
| `reminder_preferences`   | `reminder_preferences`   | `deleted_at`     | 365 days          |
| `idempotency_keys`       | `idempotency_keys`       | `created_at`     | 1 day             |
| `secret_rotation_logs`   | `secret_rotation_logs`   | `rotated_at`     | 730 days (2 yrs)  |

New types are added by inserting a row in `data_retention_policies` and
adding a mapping in `TABLE_MAP` in `backend/src/retention.rs`.

## Retention exceptions

Individual records can be exempted from purging via the exceptions API.
Exceptions can be permanent or time-limited (`expires_in_seconds`).  This
supports legal holds, active investigations, and compliance requirements.

## Deletion audit trail

Every purge run (automated or manual) writes an entry to `retention_deletion_log`
recording the data type, number of rows deleted, timestamp, and actor.  The
audit trail is viewable via `GET /api/retention/deletion-log`.

## REST API

### Policies

```
GET    /api/retention/policies                    — list all policies
GET    /api/retention/policies/:data_type         — get one policy
PUT    /api/retention/policies/:data_type         — create/update (admin)
```

**PUT body:**

```json
{
  "retention_days": 90,
  "enabled": true,
  "description": "Keep audit logs for 90 days per compliance policy"
}
```

Set `retention_days` to `0` to retain records forever (policy disabled).

### Manual purge

```
POST   /api/retention/purge/:data_type            — trigger purge (admin)
```

Returns:

```json
{
  "data_type": "audit_logs",
  "deleted_rows": 42,
  "purged_at": "2026-07-26T14:00:00Z"
}
```

### Deletion log

```
GET    /api/retention/deletion-log?data_type=audit_logs&limit=50
```

### Exceptions

```
GET    /api/retention/exceptions/:data_type       — list active exceptions
POST   /api/retention/exceptions/:data_type       — add exception (admin)
```

**POST body:**

```json
{
  "record_id": "123",
  "reason": "Pending legal hold — litigation ref #LIT-2026-001",
  "expires_in_seconds": 2592000
}
```

## Authorization

Mutating endpoints (`PUT`, `POST`) require an `Authorization: Bearer <ADMIN_API_KEY>`
header.  Read endpoints are publicly accessible within the service network.

## Scheduler

The purge scheduler runs inside the existing background task (`scheduler::run`).
It fires once every 24 hours and calls `retention::run_purge_scheduler`, which
iterates all enabled policies and purges expired rows from the corresponding
tables.
