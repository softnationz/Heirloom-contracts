# Cache Invalidation Event System (#86)

## Overview

The cache invalidation event system replaces manual, error-prone cache clearing with automatic, event-driven invalidation. When vault state changes, domain events trigger precise cache invalidation — including cascading dependencies — keeping the cache consistent without requiring callers to manually manage cache state.

## Architecture

### Event Types

| Event | Triggers |
|-------|----------|
| `VaultStateChanged` | Check-in, deposit, withdrawal — affects Vault, TTL, Summary caches |
| `BeneficiaryUpdated` | Beneficiary address change — affects Vault, Summary caches |
| `VaultReleased` | TTL expiry release — affects all vault caches |
| `OwnerChanged` | Ownership transfer — affects Vault, Summary caches |
| `ReminderPreferencesUpdated` | Reminder config change — affects ReminderPreferences cache |
| `SubscriptionChanged` | Notification subscription change — affects Subscription cache |
| `GlobalFlush` | Full cache flush — invalidates all entries |

### Cache Keys

Each vault may have up to five cached data types:

- `Vault` — core vault struct
- `TtlRemaining` — current TTL countdown
- `Summary` — lightweight vault summary for listings
- `ReminderPreferences` — notification settings
- `Subscription` — subscription channels and frequency

### Invalidation Strategy

Each event type maps to a specific set of cache keys to invalidate. This smart mapping avoids unnecessary cache misses by only clearing what actually changed.

### Dependency Tracking

The dependency graph tracks additional cache entries that depend on the same vault. When a vault is invalidated, all registered dependents are cascaded through automatically.

```
VaultStateChanged("vault_001")
  ├── Invalidates: Vault, TtlRemaining, Summary
  └── Cascades to: any registered dependents
```

## Usage

### Emitting Events

Use `CacheEventEmitter` to emit events from application code:

```rust
use heirloom_protocol_backend::cache_invalidation::{CacheEventEmitter, CacheInvalidator};
use std::sync::Arc;

let invalidator = Arc::new(CacheInvalidator::new(Arc::clone(&cache)));
let emitter = CacheEventEmitter::new(Arc::clone(&invalidator));

// After a check-in
emitter.vault_state_changed("vault_001");

// After beneficiary update
emitter.beneficiary_updated("vault_001");

// After vault release
emitter.vault_released("vault_001");

// After owner transfer
emitter.owner_changed("vault_001", "new_owner_address");

// Emergency global flush
emitter.global_flush();
```

### Direct Event Handling

```rust
use heirloom_protocol_backend::cache_invalidation::{CacheEvent, CacheInvalidator};

let invalidator = CacheInvalidator::new(Arc::clone(&cache));

invalidator.handle_event(CacheEvent::VaultStateChanged {
    vault_id: "vault_001".to_string(),
});
```

### Dependency Registration

Register that a computed cache entry depends on a vault:

```rust
use heirloom_protocol_backend::cache_invalidation::CacheKey;

// When caching a derived summary, register its dependency
invalidator.register_dependency("vault_001", CacheKey::Summary);
```

When `vault_001` is next invalidated, any registered `Summary` dependency is included in the cascade count.

## Cascade Invalidation

Cascade invalidation fires automatically when dependencies are registered. Example flow:

1. A vault summary is computed and cached
2. `invalidator.register_dependency("vault_001", CacheKey::Summary)` is called
3. Later, `VaultStateChanged { vault_id: "vault_001" }` is emitted
4. The invalidator clears the primary vault entry **and** cascades to the registered `Summary` dependency

This prevents stale derived data from surviving beyond its source data's cache lifetime.

## Statistics

The invalidator tracks metrics accessible via `GET /admin/cache-stats`:

```json
{
  "invalidation": {
    "total_events": 500,
    "total_invalidations": 498,
    "cascade_invalidations": 120,
    "global_flushes": 2
  }
}
```

| Metric | Description |
|--------|-------------|
| `total_events` | All invalidation events received |
| `total_invalidations` | Direct cache entries removed |
| `cascade_invalidations` | Additional entries removed via dependency graph |
| `global_flushes` | Number of full-cache flush operations |

## API Endpoints

### Flush All Caches

**Endpoint**: `POST /admin/cache-invalidate`

Triggers a `GlobalFlush` event across both the L1/L2 multi-level cache and the VaultCache.

**Response**:
```json
{
  "status": "ok",
  "message": "All cache levels flushed"
}
```

## Integration Points

Call `cache_invalidator.handle_event(...)` or use `CacheEventEmitter` in any handler that mutates vault state:

- `POST /api/vaults/:id/check-in` → `VaultStateChanged`
- `POST /api/vaults/:id/deposit` → `VaultStateChanged`
- `POST /api/vaults/:id/withdraw` → `VaultStateChanged`
- `PATCH /api/vaults/:id/beneficiary` → `BeneficiaryUpdated`
- `POST /api/vaults/:id/release` → `VaultReleased`

## Related Features

- [Predictive Cache Warming (#87)](./cache-warming.md)
- [Multi-Level Caching Strategy (#85)](./cache-strategy.md)
