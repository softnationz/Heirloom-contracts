# Testing Guide: Graceful Degradation State Fix

## Unit Tests (Run with cargo test)

The implementation includes comprehensive unit tests to verify the fix works correctly.

### Running Tests

```bash
cd /workspaces/heirloom-contracts-backend
cargo test --package backend --lib degradation -- --nocapture
```

### Test Coverage

#### Regression Tests (Existing Behavior Preserved)

1. **`test_unregistered_capability_defaults_to_full`**
   - Verifies unregistered capabilities still default to `Full`
   - Ensures backward compatibility

2. **`test_set_and_get_capability_status`**
   - Verifies basic set/get operations work with database backend
   - Confirms status persistence in single instance

3. **`test_negotiate_allows_proceeding_with_fallback`**
   - Verifies negotiation works when fallback is available
   - Ensures clients can proceed with reduced functionality

4. **`test_negotiate_blocks_without_fallback`**
   - Verifies negotiation correctly blocks when no fallback exists
   - Ensures safety in unavailable-without-fallback scenario

5. **`test_degraded_capability_can_proceed`**
   - Verifies degraded capabilities allow negotiation to proceed
   - Tests intermediate degradation state

#### New Tests (Shared Store Verification) ★

6. **`test_two_handles_share_same_store`** ★
   - **Purpose**: Verify two instances sharing same database see each other's changes
   - **Scenario**: 
     - Create `DegradationState` instance 1
     - Create `DegradationState` instance 2 (same database)
     - Set capability status via instance 1
     - Read capability status via instance 2
   - **Assertion**: Instance 2 observes change made by instance 1
   - **This tests the core fix**: Load-balanced instances no longer have isolated state

7. **`test_degradation_state_persists_across_instances`** ★
   - **Purpose**: Verify capability status survives process restarts
   - **Scenario**:
     - Instance 1 sets capability status
     - Instance 1 is dropped (simulating process restart)
     - Instance 2 created and reads capability status
   - **Assertion**: Status persists in database across instances
   - **This tests persistence requirement**: Status should survive restarts

8. **`test_list_returns_all_registered_capabilities`**
   - Verifies list functionality with database backend
   - Tests that all registered statuses are returned

### Running Individual Tests

```bash
# Test shared store functionality
cargo test --package backend --lib degradation::tests::test_two_handles_share_same_store -- --nocapture

# Test persistence across instances
cargo test --package backend --lib degradation::tests::test_degradation_state_persists_across_instances -- --nocapture

# Test all regression tests
cargo test --package backend --lib degradation::tests::test_unregistered_capability_defaults_to_full -- --nocapture
cargo test --package backend --lib degradation::tests::test_set_and_get_capability_status -- --nocapture
cargo test --package backend --lib degradation::tests::test_negotiate_allows_proceeding_with_fallback -- --nocapture
cargo test --package backend --lib degradation::tests::test_negotiate_blocks_without_fallback -- --nocapture
cargo test --package backend --lib degradation::tests::test_degraded_capability_can_proceed -- --nocapture

# Test list functionality
cargo test --package backend --lib degradation::tests::test_list_returns_all_registered_capabilities -- --nocapture
```

## Integration Tests (Manual Verification)

### Setup

```bash
cd /workspaces/heirloom-contracts-backend
cp .env.example .env
# Edit .env with appropriate values if needed
docker-compose up -d
```

### Test 1: Single Instance Behavior (Regression)

```bash
# Terminal 1: Check health
curl http://localhost:3000/health

# Terminal 1: List capabilities (should be empty)
curl http://localhost:3000/admin/capabilities

# Terminal 1: Set a capability
curl -X POST http://localhost:3000/admin/capabilities \
  -H "Content-Type: application/json" \
  -d '{
    "name": "search",
    "level": "degraded",
    "reason": "index rebuilding",
    "fallback_available": true
  }'

# Expected response:
# {
#   "name": "search",
#   "level": "degraded",
#   "reason": "index rebuilding",
#   "fallback_available": true,
#   "updated_at": "2025-08-20T14:37:27.333Z"
# }

# Terminal 1: Verify status was set
curl http://localhost:3000/admin/capabilities

# Expected response:
# [{
#   "name": "search",
#   "level": "degraded",
#   "reason": "index rebuilding",
#   "fallback_available": true,
#   "updated_at": "2025-08-20T14:37:27.333Z"
# }]

# Terminal 1: Negotiate
curl -X POST http://localhost:3000/capabilities/negotiate \
  -H "Content-Type: application/json" \
  -d '{"requested": ["search"]}'

# Expected response:
# {
#   "capabilities": [{
#     "name": "search",
#     "level": "degraded",
#     "reason": "index rebuilding",
#     "use_fallback": true
#   }],
#   "can_proceed": true
# }
```

### Test 2: Multiple Instances with Load Balancer (Primary Fix)

This test verifies that the fix works as intended: all instances see the same degradation state.

#### Setup Two Instances

```bash
# Terminal 1: Start first backend instance on port 3000
cd /workspaces/heirloom-contracts-backend
cargo run --package backend

# Terminal 2: Start second backend instance on port 3001
# First, modify backend code to start on 3001 or use environment variable
# Or manually start with modified bind address
```

Alternatively, if both instances share the same Docker network:

```bash
# Use the existing docker-compose setup
docker-compose up -d
# Backend will be on localhost:3000

# Verify both instances can be reached (after scaling)
curl http://localhost:3000/health
curl http://localhost:3000/health  # May route to same or different instance
```

#### Perform Load-Balanced Test

```bash
# Step 1: List capabilities on instance A (empty)
curl http://localhost:3000/admin/capabilities
# Response: []

# Step 2: Set capability on instance A
curl -X POST http://localhost:3000/admin/capabilities \
  -H "Content-Type: application/json" \
  -d '{
    "name": "payments",
    "level": "unavailable",
    "reason": "gateway timeout",
    "fallback_available": true
  }'

# Step 3: Immediately read from instance B (via load balancer)
# In a real setup, requests may go to different instances
curl http://localhost:3000/admin/capabilities

# EXPECTED BEHAVIOR (THE FIX):
# Both instances see the same status set in Step 2
# Response: [{
#   "name": "payments",
#   "level": "unavailable",
#   "reason": "gateway timeout",
#   "fallback_available": true,
#   "updated_at": "2025-08-20T14:37:27.333Z"
# }]

# Step 4: Negotiate from instance B
curl -X POST http://localhost:3000/capabilities/negotiate \
  -H "Content-Type: application/json" \
  -d '{"requested": ["payments"]}'

# EXPECTED RESPONSE (FIXED):
# {
#   "capabilities": [{
#     "name": "payments",
#     "level": "unavailable",
#     "reason": "gateway timeout",
#     "use_fallback": true
#   }],
#   "can_proceed": true
# }

# OLD BEHAVIOR (BROKEN - would have seen):
# {
#   "capabilities": [{
#     "name": "payments",
#     "level": "full",
#     "reason": null,
#     "use_fallback": false
#   }],
#   "can_proceed": true
# }
# ^ This would have been contradictory guidance
```

### Test 3: Persistence Across Restarts

```bash
# Step 1: Set a capability
curl -X POST http://localhost:3000/admin/capabilities \
  -H "Content-Type: application/json" \
  -d '{
    "name": "notifications",
    "level": "degraded",
    "reason": "email queue backlog",
    "fallback_available": false
  }'

# Step 2: Verify it's set
curl http://localhost:3000/admin/capabilities

# Step 3: Kill the backend process (Ctrl+C)

# Step 4: Restart the backend process
cd /workspaces/heirloom-contracts-backend
cargo run --package backend

# Step 5: Verify capability status persisted
curl http://localhost:3000/admin/capabilities

# EXPECTED RESPONSE:
# [{
#   "name": "notifications",
#   "level": "degraded",
#   "reason": "email queue backlog",
#   "fallback_available": false,
#   "updated_at": "2025-08-20T14:37:27.333Z"
# }]

# OLD BEHAVIOR (BROKEN - would have seen):
# []
# ^ Status would be lost on restart because it was only in process memory
```

## Verification Checklist

After running tests and integration tests, verify:

- [ ] All unit tests pass: `cargo test --package backend --lib degradation`
- [ ] `test_two_handles_share_same_store` passes (tests shared store)
- [ ] `test_degradation_state_persists_across_instances` passes (tests persistence)
- [ ] All regression tests pass (backward compatibility preserved)
- [ ] `POST /admin/capabilities` returns 200 and persists data
- [ ] `GET /admin/capabilities` returns all registered capabilities
- [ ] `POST /capabilities/negotiate` uses persisted data
- [ ] `GET /capabilities/:name/fallback` uses persisted data
- [ ] Status survives process restart (persistence test)
- [ ] Multiple instances see same state (load-balanced test)
- [ ] Database migration #12 runs without error
- [ ] CI passes: `cargo clippy`, `cargo fmt`, `cargo test`, `cargo audit`

## Error Scenarios

### Scenario 1: Database Connection Failure

If the database becomes unavailable after startup:

```bash
# All degradation handlers should return 500
curl http://localhost:3000/admin/capabilities
# Expected: 500 INTERNAL_SERVER_ERROR with error message
```

### Scenario 2: Invalid Capability Name

```bash
curl -X POST http://localhost:3000/admin/capabilities \
  -H "Content-Type: application/json" \
  -d '{
    "name": "",
    "level": "degraded",
    "reason": "test",
    "fallback_available": false
  }'

# Expected: 422 UNPROCESSABLE_ENTITY
# Response: {"error": "name must not be empty"}
```

### Scenario 3: Malformed JSON

```bash
curl -X POST http://localhost:3000/admin/capabilities \
  -H "Content-Type: application/json" \
  -d '{invalid json'

# Expected: 400 BAD_REQUEST
```

## Performance Verification

### Database Query Performance

The implementation uses:
- Indexed queries on `capability_statuses(name)` for fast lookups
- `ON CONFLICT` clause for atomic upserts (no race conditions)
- In-memory parsing for JSON serialization (negligible cost)

Expected performance:
- Set capability: ~1-2ms (single INSERT with index)
- Get capability: <1ms (indexed lookup)
- List capabilities: <5ms (full table scan, typically only a few rows)

Monitor database logs:
```bash
# In docker-compose setup
docker-compose logs db | grep "SELECT\|INSERT\|UPDATE"
```

## Rollback Plan

If the fix needs to be reverted:

1. Remove migration #12 (capability_statuses table)
2. Revert DegradationState to in-memory HashMap version
3. Change `DegradationState::new(db)` back to `DegradationState::new()`
4. Remove degradation routes from router if no longer needed

This would restore the old behavior but break load-balanced deployments.
