# Database Migration Validation Testing (#84)

## Overview

The migration validation testing framework provides a structured harness for:

- Applying and verifying forward migrations
- Verifying backward (rollback) migrations
- Testing round-trip (forward → backward) cycles
- Measuring migration performance against production-like data volumes
- Preventing deployment failures by catching migration issues early

## Architecture

### MockDatabase

`MockDatabase` simulates a SQL database for deterministic, isolated migration tests. It supports a subset of SQL statements commonly used in schema migrations:

| SQL Statement | Supported |
|---------------|-----------|
| `CREATE TABLE` | ✅ |
| `ALTER TABLE` | ✅ |
| `DROP TABLE` | ✅ |
| `CREATE INDEX` | ✅ |
| `INSERT INTO` | ✅ (adds rows) |
| `DELETE FROM` | ✅ (clears rows) |
| Other | ❌ (returns error) |

For production use, replace `MockDatabase` with a real PostgreSQL test instance using transactional rollback to isolate tests.

### Migration Definition

```rust
use heirloom_protocol_backend::migration_testing::Migration;

let migration = Migration::new(
    "001",                                    // Version identifier
    "add_vaults_table",                       // Human-readable name
    "CREATE TABLE vaults (id TEXT PRIMARY KEY, owner TEXT NOT NULL)",  // Forward SQL
    Some("DROP TABLE vaults"),               // Backward SQL (None = irreversible)
);

assert!(migration.is_reversible()); // true when backward_sql is Some
```

### Validation Result

Every migration operation returns a `ValidationResult`:

```rust
pub struct ValidationResult {
    pub migration_version: String,    // e.g. "001"
    pub direction: MigrationDirection, // Forward or Backward
    pub success: bool,
    pub duration: Duration,
    pub error_message: Option<String>, // Set on failure
    pub rows_affected: Option<u64>,    // Set on success
}
```

## Usage

### Forward Migration Test

```rust
use heirloom_protocol_backend::migration_testing::{Migration, MigrationTester};

let mut tester = MigrationTester::new();

let migration = Migration::new(
    "001",
    "create_vaults",
    "CREATE TABLE vaults (id TEXT PRIMARY KEY, owner TEXT)",
    Some("DROP TABLE vaults"),
);

let result = tester.apply_forward(&migration);
assert!(result.success);
assert!(tester.database().table_exists("vaults"));
assert!(tester.applied_versions().contains(&"001".to_string()));
```

### Backward (Rollback) Migration Test

```rust
// After applying forward:
let rollback = tester.apply_backward(&migration);
assert!(rollback.success);
assert!(!tester.database().table_exists("vaults"));
assert!(tester.applied_versions().is_empty());
```

### Round-Trip Test

Tests forward + backward in one call. Returns `Vec<ValidationResult>` — one entry per direction:

```rust
let results = tester.test_round_trip(&migration);
assert_eq!(results.len(), 2);
assert!(results[0].success); // Forward
assert!(results[1].success); // Backward
```

If the migration has no `backward_sql`, only the forward result is returned.

### Irreversible Migration

```rust
let migration = Migration::new(
    "002",
    "drop_legacy_column",
    "ALTER TABLE vaults DROP COLUMN legacy_data",
    None, // Cannot be rolled back
);

let result = tester.apply_backward(&migration);
assert!(!result.success);
assert_eq!(result.error_message.as_deref(), Some("Migration is not reversible"));
```

## Performance Testing

`PerformanceBenchmark` measures migration execution time against a production-like number of rows:

```rust
use heirloom_protocol_backend::migration_testing::{Migration, MigrationTester, PerformanceBenchmark};

let mut tester = MigrationTester::new();

let migration = Migration::new(
    "001",
    "add_index_on_owner",
    "CREATE INDEX idx_vaults_owner ON test_table(owner)",
    None,
);

// Simulate 100,000 rows
let benchmark = PerformanceBenchmark::run(&migration, &mut tester, 100_000);

println!("Migration: {}", benchmark.migration_version);
println!("Rows before: {}", benchmark.rows_before);
println!("Duration: {:?}", benchmark.duration);
println!("Rows/sec: {:.0}", benchmark.rows_per_second);
```

### Benchmark Fields

| Field | Description |
|-------|-------------|
| `migration_version` | Version string from the migration |
| `rows_before` | Row count before migration ran |
| `rows_after` | Row count after migration ran |
| `duration` | Wall-clock time for the migration |
| `rows_per_second` | Throughput (rows_before / duration) |

## Migration Sequence Testing

Test a chain of dependent migrations:

```rust
let migrations = vec![
    Migration::new("001", "create_vaults", "CREATE TABLE vaults (id TEXT)", Some("DROP TABLE vaults")),
    Migration::new("002", "create_owners", "CREATE TABLE owners (id TEXT)", Some("DROP TABLE owners")),
    Migration::new("003", "add_index", "CREATE INDEX idx ON vaults(id)", None),
];

let mut tester = MigrationTester::new();
for migration in &migrations {
    let result = tester.apply_forward(migration);
    assert!(result.success, "Migration {} failed: {:?}", migration.version, result.error_message);
}

assert_eq!(tester.applied_versions().len(), 3);
```

## Production Integration

For real PostgreSQL:

1. Create a test database or use transaction-wrapped tests
2. Replace `MockDatabase::execute()` with `sqlx::query()` or `diesel::sql_query()`
3. Use `BEGIN` / `ROLLBACK` transactions to isolate each test run
4. Run tests against a database seeded with production-like data volumes

```sql
-- Sample test fixture for production-like volume
INSERT INTO vaults SELECT gen_random_uuid(), ... FROM generate_series(1, 100000);
```

## Performance Baselines

When validating migrations, fail the CI job if performance exceeds these thresholds:

| Operation | Max Duration | Notes |
|-----------|-------------|-------|
| `CREATE TABLE` | < 100ms | Even at large scale |
| `ADD COLUMN` | < 500ms | Depends on row count |
| `CREATE INDEX` | < 30s | On 1M+ rows with `CONCURRENTLY` |
| `DROP TABLE` | < 100ms | Fast metadata operation |

## CI Integration

Add migration validation as a pre-deploy CI step:

```yaml
# .github/workflows/ci.yml
- name: Validate migrations
  run: |
    cargo test migration_tester -- --nocapture
    cargo test performance_benchmark -- --nocapture
```

## Related Features

- [Database Architecture](./architecture.md)
- [Deployment Guide](./deployment-guide.md)
