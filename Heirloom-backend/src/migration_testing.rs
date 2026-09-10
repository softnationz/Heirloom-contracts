/// Database migration validation testing framework.
///
/// Provides a testing harness for validating forward and backward migrations,
/// testing with production-like data volumes, and measuring migration performance.
/// Prevents migration failures by catching issues before deployment.

use std::collections::HashMap;
use std::time::{Duration, Instant};

// ── Migration Types ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationDirection {
    Forward,
    Backward,
}

#[derive(Debug, Clone)]
pub struct Migration {
    pub version: String,
    pub name: String,
    pub forward_sql: String,
    pub backward_sql: Option<String>,
}

impl Migration {
    pub fn new(version: &str, name: &str, forward_sql: &str, backward_sql: Option<&str>) -> Self {
        Self {
            version: version.to_string(),
            name: name.to_string(),
            forward_sql: forward_sql.to_string(),
            backward_sql: backward_sql.map(|s| s.to_string()),
        }
    }

    /// Check if this migration is reversible (has backward SQL).
    pub fn is_reversible(&self) -> bool {
        self.backward_sql.is_some()
    }
}

// ── Migration Validation Result ───────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ValidationResult {
    pub migration_version: String,
    pub direction: MigrationDirection,
    pub success: bool,
    pub duration: Duration,
    pub error_message: Option<String>,
    pub rows_affected: Option<u64>,
}

impl ValidationResult {
    pub fn success(
        version: &str,
        direction: MigrationDirection,
        duration: Duration,
        rows_affected: u64,
    ) -> Self {
        Self {
            migration_version: version.to_string(),
            direction,
            success: true,
            duration,
            error_message: None,
            rows_affected: Some(rows_affected),
        }
    }

    pub fn failure(
        version: &str,
        direction: MigrationDirection,
        duration: Duration,
        error: &str,
    ) -> Self {
        Self {
            migration_version: version.to_string(),
            direction,
            success: false,
            duration,
            error_message: Some(error.to_string()),
            rows_affected: None,
        }
    }
}

// ── Migration Test Harness ────────────────────────────────────────────────────

/// A mock database state for testing migrations without a real database.
///
/// In production, replace this with a test database connection (e.g., PostgreSQL
/// test instance with transaction rollback support).
#[derive(Debug, Clone, Default)]
pub struct MockDatabase {
    tables: HashMap<String, Vec<HashMap<String, String>>>,
    indexes: Vec<String>,
    constraints: Vec<String>,
}

impl MockDatabase {
    pub fn new() -> Self {
        Self::default()
    }

    /// Execute a SQL statement (simplified simulation).
    pub fn execute(&mut self, sql: &str) -> Result<u64, String> {
        let sql = sql.trim().to_lowercase();

        if sql.starts_with("create table") {
            self.parse_create_table(sql)
        } else if sql.starts_with("alter table") {
            self.parse_alter_table(sql)
        } else if sql.starts_with("drop table") {
            self.parse_drop_table(sql)
        } else if sql.starts_with("create index") {
            self.parse_create_index(sql)
        } else if sql.starts_with("insert into") {
            self.parse_insert(sql)
        } else if sql.starts_with("delete from") {
            self.parse_delete(sql)
        } else {
            Err(format!("Unsupported SQL statement: {}", sql))
        }
    }

    fn parse_create_table(&mut self, sql: String) -> Result<u64, String> {
        // Extract table name from "CREATE TABLE table_name (...)"
        let parts: Vec<&str> = sql.split_whitespace().collect();
        if parts.len() < 3 {
            return Err("Invalid CREATE TABLE syntax".to_string());
        }
        let table_name = parts[2].trim_matches(|c| c == '(' || c == ')');
        self.tables.insert(table_name.to_string(), Vec::new());
        Ok(0)
    }

    fn parse_alter_table(&mut self, sql: String) -> Result<u64, String> {
        // Simplified: just check the table exists
        let parts: Vec<&str> = sql.split_whitespace().collect();
        if parts.len() < 3 {
            return Err("Invalid ALTER TABLE syntax".to_string());
        }
        let table_name = parts[2];
        if !self.tables.contains_key(table_name) {
            return Err(format!("Table '{}' does not exist", table_name));
        }
        Ok(0)
    }

    fn parse_drop_table(&mut self, sql: String) -> Result<u64, String> {
        let parts: Vec<&str> = sql.split_whitespace().collect();
        if parts.len() < 3 {
            return Err("Invalid DROP TABLE syntax".to_string());
        }
        let table_name = parts[2].trim_matches(';');
        if self.tables.remove(table_name).is_none() {
            return Err(format!("Table '{}' does not exist", table_name));
        }
        Ok(0)
    }

    fn parse_create_index(&mut self, sql: String) -> Result<u64, String> {
        let parts: Vec<&str> = sql.split_whitespace().collect();
        if parts.len() < 3 {
            return Err("Invalid CREATE INDEX syntax".to_string());
        }
        let index_name = parts[2];
        self.indexes.push(index_name.to_string());
        Ok(0)
    }

    fn parse_insert(&mut self, sql: String) -> Result<u64, String> {
        let parts: Vec<&str> = sql.split_whitespace().collect();
        if parts.len() < 3 {
            return Err("Invalid INSERT syntax".to_string());
        }
        let table_name = parts[2];
        if !self.tables.contains_key(table_name) {
            return Err(format!("Table '{}' does not exist", table_name));
        }
        // Simplified: just increment row count
        self.tables.get_mut(table_name).unwrap().push(HashMap::new());
        Ok(1)
    }

    fn parse_delete(&mut self, sql: String) -> Result<u64, String> {
        let parts: Vec<&str> = sql.split_whitespace().collect();
        if parts.len() < 3 {
            return Err("Invalid DELETE syntax".to_string());
        }
        let table_name = parts[2];
        if !self.tables.contains_key(table_name) {
            return Err(format!("Table '{}' does not exist", table_name));
        }
        let rows = self.tables.get(table_name).unwrap().len() as u64;
        self.tables.get_mut(table_name).unwrap().clear();
        Ok(rows)
    }

    pub fn table_exists(&self, table_name: &str) -> bool {
        self.tables.contains_key(table_name)
    }

    pub fn index_exists(&self, index_name: &str) -> bool {
        self.indexes.iter().any(|idx| idx == index_name)
    }

    pub fn row_count(&self, table_name: &str) -> usize {
        self.tables.get(table_name).map_or(0, |rows| rows.len())
    }
}

/// Migration test harness.
pub struct MigrationTester {
    db: MockDatabase,
    applied_versions: Vec<String>,
}

impl MigrationTester {
    pub fn new() -> Self {
        Self {
            db: MockDatabase::new(),
            applied_versions: Vec::new(),
        }
    }

    /// Apply a migration (forward).
    pub fn apply_forward(&mut self, migration: &Migration) -> ValidationResult {
        let start = Instant::now();

        match self.db.execute(&migration.forward_sql) {
            Ok(rows) => {
                self.applied_versions.push(migration.version.clone());
                ValidationResult::success(
                    &migration.version,
                    MigrationDirection::Forward,
                    start.elapsed(),
                    rows,
                )
            }
            Err(e) => ValidationResult::failure(
                &migration.version,
                MigrationDirection::Forward,
                start.elapsed(),
                &e,
            ),
        }
    }

    /// Rollback a migration (backward).
    pub fn apply_backward(&mut self, migration: &Migration) -> ValidationResult {
        let start = Instant::now();

        if !migration.is_reversible() {
            return ValidationResult::failure(
                &migration.version,
                MigrationDirection::Backward,
                start.elapsed(),
                "Migration is not reversible",
            );
        }

        match self.db.execute(migration.backward_sql.as_ref().unwrap()) {
            Ok(rows) => {
                self.applied_versions.retain(|v| v != &migration.version);
                ValidationResult::success(
                    &migration.version,
                    MigrationDirection::Backward,
                    start.elapsed(),
                    rows,
                )
            }
            Err(e) => ValidationResult::failure(
                &migration.version,
                MigrationDirection::Backward,
                start.elapsed(),
                &e,
            ),
        }
    }

    /// Test forward + backward migration cycle.
    pub fn test_round_trip(&mut self, migration: &Migration) -> Vec<ValidationResult> {
        let mut results = Vec::new();

        // Apply forward.
        let forward_result = self.apply_forward(migration);
        results.push(forward_result.clone());

        // If forward succeeded and migration is reversible, try backward.
        if forward_result.success && migration.is_reversible() {
            let backward_result = self.apply_backward(migration);
            results.push(backward_result);
        }

        results
    }

    /// Get the current database state (for inspection).
    pub fn database(&self) -> &MockDatabase {
        &self.db
    }

    /// Get list of applied migration versions.
    pub fn applied_versions(&self) -> &[String] {
        &self.applied_versions
    }
}

impl Default for MigrationTester {
    fn default() -> Self {
        Self::new()
    }
}

// ── Performance Testing ───────────────────────────────────────────────────────

/// Performance benchmark for migrations with production-like data volumes.
pub struct PerformanceBenchmark {
    pub migration_version: String,
    pub rows_before: u64,
    pub rows_after: u64,
    pub duration: Duration,
    pub rows_per_second: f64,
}

impl PerformanceBenchmark {
    pub fn run(
        migration: &Migration,
        tester: &mut MigrationTester,
        data_volume: u64,
    ) -> Self {
        // Pre-populate with test data.
        let table_name = "test_table";
        tester.db.tables.insert(table_name.to_string(), Vec::new());
        for _ in 0..data_volume {
            tester.db.execute(&format!("INSERT INTO {} VALUES ()", table_name)).ok();
        }

        let rows_before = tester.db.row_count(table_name) as u64;
        let start = Instant::now();

        // Execute migration.
        let result = tester.apply_forward(migration);

        let duration = start.elapsed();
        let rows_after = tester.db.row_count(table_name) as u64;

        let rows_per_second = if duration.as_secs_f64() > 0.0 {
            rows_before as f64 / duration.as_secs_f64()
        } else {
            0.0
        };

        Self {
            migration_version: migration.version.clone(),
            rows_before,
            rows_after,
            duration,
            rows_per_second,
        }
    }
}

// ── Unit Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_migration_is_reversible() {
        let migration = Migration::new(
            "001",
            "add_users_table",
            "CREATE TABLE users (id INT)",
            Some("DROP TABLE users"),
        );
        assert!(migration.is_reversible());

        let migration_no_backward = Migration::new(
            "002",
            "drop_old_table",
            "DROP TABLE old_table",
            None,
        );
        assert!(!migration_no_backward.is_reversible());
    }

    #[test]
    fn test_mock_database_create_table() {
        let mut db = MockDatabase::new();
        let result = db.execute("CREATE TABLE users (id INT, name TEXT)");
        assert!(result.is_ok());
        assert!(db.table_exists("users"));
    }

    #[test]
    fn test_mock_database_drop_table() {
        let mut db = MockDatabase::new();
        db.execute("CREATE TABLE users (id INT)").unwrap();
        let result = db.execute("DROP TABLE users;");
        assert!(result.is_ok());
        assert!(!db.table_exists("users"));
    }

    #[test]
    fn test_mock_database_alter_table() {
        let mut db = MockDatabase::new();
        db.execute("CREATE TABLE users (id INT)").unwrap();
        let result = db.execute("ALTER TABLE users ADD COLUMN email TEXT");
        assert!(result.is_ok());
    }

    #[test]
    fn test_mock_database_create_index() {
        let mut db = MockDatabase::new();
        let result = db.execute("CREATE INDEX idx_users ON users(id)");
        assert!(result.is_ok());
        assert!(db.index_exists("idx_users"));
    }

    #[test]
    fn test_mock_database_insert() {
        let mut db = MockDatabase::new();
        db.execute("CREATE TABLE users (id INT)").unwrap();
        let result = db.execute("INSERT INTO users VALUES ()");
        assert!(result.is_ok());
        assert_eq!(db.row_count("users"), 1);
    }

    #[test]
    fn test_mock_database_delete() {
        let mut db = MockDatabase::new();
        db.execute("CREATE TABLE users (id INT)").unwrap();
        db.execute("INSERT INTO users VALUES ()").unwrap();
        db.execute("INSERT INTO users VALUES ()").unwrap();
        let result = db.execute("DELETE FROM users");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 2);
        assert_eq!(db.row_count("users"), 0);
    }

    #[test]
    fn test_migration_tester_apply_forward() {
        let mut tester = MigrationTester::new();
        let migration = Migration::new(
            "001",
            "create_users",
            "CREATE TABLE users (id INT)",
            Some("DROP TABLE users"),
        );

        let result = tester.apply_forward(&migration);
        assert!(result.success);
        assert!(tester.database().table_exists("users"));
        assert_eq!(tester.applied_versions(), &["001"]);
    }

    #[test]
    fn test_migration_tester_apply_backward() {
        let mut tester = MigrationTester::new();
        let migration = Migration::new(
            "001",
            "create_users",
            "CREATE TABLE users (id INT)",
            Some("DROP TABLE users"),
        );

        tester.apply_forward(&migration);
        let result = tester.apply_backward(&migration);

        assert!(result.success);
        assert!(!tester.database().table_exists("users"));
        assert!(tester.applied_versions().is_empty());
    }

    #[test]
    fn test_migration_tester_round_trip() {
        let mut tester = MigrationTester::new();
        let migration = Migration::new(
            "001",
            "create_users",
            "CREATE TABLE users (id INT)",
            Some("DROP TABLE users"),
        );

        let results = tester.test_round_trip(&migration);
        assert_eq!(results.len(), 2);
        assert!(results[0].success); // Forward.
        assert!(results[1].success); // Backward.
    }

    #[test]
    fn test_migration_tester_non_reversible_backward_fails() {
        let mut tester = MigrationTester::new();
        let migration = Migration::new(
            "001",
            "drop_old_table",
            "DROP TABLE old_table",
            None,
        );

        let result = tester.apply_backward(&migration);
        assert!(!result.success);
        assert!(result.error_message.is_some());
    }

    #[test]
    fn test_performance_benchmark() {
        let mut tester = MigrationTester::new();
        let migration = Migration::new(
            "001",
            "add_index",
            "CREATE INDEX idx_test ON test_table(id)",
            None,
        );

        let benchmark = PerformanceBenchmark::run(&migration, &mut tester, 1000);

        assert_eq!(benchmark.migration_version, "001");
        assert_eq!(benchmark.rows_before, 1000);
        assert!(benchmark.duration.as_millis() > 0);
    }

    #[test]
    fn test_validation_result_success() {
        let result = ValidationResult::success(
            "001",
            MigrationDirection::Forward,
            Duration::from_millis(50),
            10,
        );

        assert!(result.success);
        assert_eq!(result.rows_affected, Some(10));
        assert!(result.error_message.is_none());
    }

    #[test]
    fn test_validation_result_failure() {
        let result = ValidationResult::failure(
            "001",
            MigrationDirection::Backward,
            Duration::from_millis(5),
            "Table does not exist",
        );

        assert!(!result.success);
        assert!(result.error_message.is_some());
        assert!(result.rows_affected.is_none());
    }
}
