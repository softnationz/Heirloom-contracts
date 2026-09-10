# Event Sourcing (#151)

Event sourcing turns every vault state change into an immutable fact. Instead of storing
only the current balance or status, the system appends events to a log and derives the
current state on demand.

## Core concepts

### Append-only log (`EventLog`)

`EventLog` (in `backend/src/event_sourcing.rs`) is the single source of truth.

- Events are **never** mutated or deleted after they are written.
- Each vault has its own monotonically increasing `sequence` counter (1-based).
- A global insertion order is preserved across all vaults.

```rust
let seq = event_log.append(
    "vault-abc",
    EventType::Deposit,
    serde_json::json!({"balance_delta": 1000}),
)?;
// seq == 1 for the first event on this vault
```

### Event versioning (`schema_version`)

Every `StoredEvent` carries a `schema_version` field (currently `1`).

- When the `data` payload shape changes in a breaking way, bump
  `CURRENT_SCHEMA_VERSION` in `event_sourcing.rs`.
- Add a migration arm inside `StoredEvent::migrate_to_current()`.
- The replayer calls `migrate_to_current()` on every event before applying it,
  so old and new events coexist without separate migration scripts.

```rust
// Example: v0 renamed "amount" → "balance_delta"
if self.schema_version == 0 {
    // rename field in self.data …
    self.schema_version = 1;
}
```

### Snapshots (`SnapshotStore`)

Replaying from the very beginning of a long-lived vault becomes expensive. Snapshots
bound the replay window.

- A snapshot captures the vault's materialized state (balance, status, TTL, last
  check-in) at a specific `snapshot_sequence`.
- Replay loads the snapshot, then only applies events with `sequence > snapshot_sequence`.
- Snapshots are stored in `SnapshotStore` and persisted to SQLite (durable) via `Db`.

```rust
// Take a snapshot after processing the 100th event
snapshot_store.take_snapshot(&vault, last_sequence)?;
```

**Snapshot strategy** — when to snapshot:

| Trigger | Recommendation |
|---|---|
| Event count since last snapshot | Every 100 events per vault |
| Time-based | Every 24 hours per vault |
| On vault release | Always snapshot before `Release` event |

### Event replay (`EventReplayer`)

```rust
let replayer = EventReplayer::new(&event_log, &snapshot_store);

// Rebuild latest state
let state = replayer.replay("vault-abc")?;
println!("balance = {}", state.state.balance);

// Point-in-time audit: rebuild state up to sequence 50
let historical = replayer.replay_to("vault-abc", 50)?;
```

`ReplayedState` returns:

| Field | Description |
|---|---|
| `state.balance` | Reconstructed balance in stroops |
| `state.status` | `active` / `expired` / `released` / `paused` |
| `state.last_check_in` | Timestamp of last check-in event |
| `state.ttl_remaining` | Seconds remaining from most recent TTL event |
| `last_sequence` | Sequence of the final event applied |
| `events_applied` | How many events were replayed in this run |

## Event types

| `EventType` | Required `data` fields | Effect on state |
|---|---|---|
| `deposit` | `balance_delta: i64` | Increases balance |
| `withdrawal` | `balance_delta: i64` | Decreases balance |
| `check_in` | `ttl_remaining?: u64` | Updates `last_check_in`; optionally sets TTL |
| `ttl_update` | `ttl_remaining: u64` | Sets `ttl_remaining` |
| `status_change` | `status: string` | Updates vault status |
| `release` | _(none required)_ | Sets status `released`, zeroes balance |

## Durability and Persistence (#267)

As of #267, events and snapshots are persisted to SQLite durably:

- **Events table**: All events are written to the `events` table before `append()` returns.
  Each row stores: `vault_id`, `sequence`, `event_type`, `timestamp`, `data` (JSON),
  `schema_version`.
- **Snapshots table**: Snapshots are written to the `snapshots` table (via UPSERT) when
  `save()` is called. Each row stores: `vault_id`, `snapshot_sequence`, `taken_at`,
  `state` (JSON).
- **In-memory cache**: The in-memory Vec and HashMap are still maintained for quick replay
  within the same process, but the database is the authoritative source of truth.
- **Bounded retention**: Events and snapshots can be archived or pruned via
  `Db::delete_old_events()` and `Db::delete_old_snapshots()` — recommended policy is to
  keep snapshots every 100 events (or on vault release), then prune events older than 90
  days (configurable per deployment).

## Integration with `AppState`

`EventSourcingState` is injected into `AppState.event_sourcing` with database persistence
enabled during initialization:

```rust
// In main.rs during startup:
let event_sourcing = EventSourcingState::with_db(db);

// In a handler:
let replayer = state.event_sourcing.replayer();
let current = replayer.replay(&vault_id)?;
```

## Running the tests

```bash
cargo test -p heirloom-protocol-backend event_sourcing
```

All tests live in the `tests` module at the bottom of `event_sourcing.rs`.
