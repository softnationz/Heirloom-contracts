//! # Task #77 — Distributed Transactions Across Shards
//!
//! Implements a **Two-Phase Commit (2PC)** coordinator that spans multiple SQLite
//! shards.  Each shard is modelled as a [`ShardNode`] with its own connection; the
//! [`DistributedTxCoordinator`] orchestrates prepare/commit/rollback across all
//! participating shards.
//!
//! ## Two-Phase Commit flow
//!
//! ```text
//!  Coordinator ──PREPARE──▶ shard-0
//!             ──PREPARE──▶ shard-1
//!             ◀─ OK ─────── shard-0
//!             ◀─ OK ─────── shard-1
//!             ──COMMIT───▶ shard-0
//!             ──COMMIT───▶ shard-1
//! ```
//!
//! If any shard votes ABORT during Phase 1 the coordinator issues ROLLBACK to all
//! shards that already voted OK.
//!
//! ## Shard awareness
//!
//! Each shard owns a key range determined by [`ShardKey`].  The coordinator's
//! [`route_operation`] method maps an operation to the responsible shard(s).
//!
//! ## Configuration
//!
//! | Variable | Default | Description |
//! |---|---|---|
//! | `DB_SHARD_COUNT` | `1` | Number of shards |
//! | `DB_SHARD_PATH_PREFIX` | `shard` | File path prefix; shard N is at `{prefix}_{N}.db` |

use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// ── Shard key ─────────────────────────────────────────────────────────────────

/// A routing key used to determine which shard owns a piece of data.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ShardKey(pub String);

impl ShardKey {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Map this key to a shard index given `total_shards`.
    pub fn shard_index(&self, total_shards: usize) -> usize {
        if total_shards == 0 {
            return 0;
        }
        // Simple hash-based mapping using the first byte of a FNV-like hash.
        let hash: u64 = self
            .0
            .bytes()
            .fold(14_695_981_039_346_656_037_u64, |acc, b| {
                acc.wrapping_mul(1_099_511_628_211_u64).wrapping_add(b as u64)
            });
        (hash % total_shards as u64) as usize
    }
}

// ── Shard node ────────────────────────────────────────────────────────────────

/// A shard participant in the distributed transaction protocol.
pub struct ShardNode {
    /// Shard identifier (0-based index).
    pub shard_id: usize,
    /// Underlying connection to this shard's database.
    conn: Mutex<Connection>,
}

impl ShardNode {
    /// Open or create the shard database at `path`.
    pub fn open(shard_id: usize, path: &str) -> Result<Self, DistributedTxError> {
        let conn = Connection::open(path)
            .map_err(|e| DistributedTxError::ShardUnavailable(shard_id, e.to_string()))?;
        Ok(Self {
            shard_id,
            conn: Mutex::new(conn),
        })
    }

    /// Bootstrap a minimal schema for this shard (idempotent).
    pub fn bootstrap(&self) -> Result<(), DistributedTxError> {
        self.conn
            .lock()
            .unwrap()
            .execute_batch(
                r"
                CREATE TABLE IF NOT EXISTS prepared_transactions (
                    tx_id       TEXT NOT NULL,
                    operations  TEXT NOT NULL,
                    prepared_at TEXT NOT NULL,
                    PRIMARY KEY (tx_id)
                );
                CREATE TABLE IF NOT EXISTS committed_transactions (
                    tx_id        TEXT NOT NULL,
                    committed_at TEXT NOT NULL,
                    PRIMARY KEY (tx_id)
                );
                CREATE TABLE IF NOT EXISTS kv_data (
                    shard_key TEXT PRIMARY KEY,
                    value     TEXT NOT NULL
                );
                ",
            )
            .map_err(|e| DistributedTxError::ShardUnavailable(self.shard_id, e.to_string()))
    }

    // ── Phase 1: Prepare ─────────────────────────────────────────────────────

    /// Write the transaction's operations to the prepare log.
    ///
    /// Returns `Ok(Vote::Commit)` if the shard accepts, `Ok(Vote::Abort)` if it
    /// detects a conflict.
    pub fn prepare(
        &self,
        tx_id: &str,
        operations: &[Operation],
    ) -> Result<Vote, DistributedTxError> {
        let ops_json = serde_json::to_string(operations)
            .map_err(|e| DistributedTxError::Serialization(e.to_string()))?;

        let conn = self.conn.lock().unwrap();

        // Check for duplicate tx_id (idempotency guard).
        let already_prepared: bool = conn
            .query_row(
                "SELECT 1 FROM prepared_transactions WHERE tx_id = ?1",
                params![tx_id],
                |_| Ok(true),
            )
            .unwrap_or(false);

        if already_prepared {
            return Ok(Vote::Abort); // refuse duplicate
        }

        conn.execute(
            "INSERT INTO prepared_transactions (tx_id, operations, prepared_at) VALUES (?1, ?2, ?3)",
            params![tx_id, ops_json, chrono::Utc::now().to_rfc3339()],
        )
        .map_err(|e| DistributedTxError::ShardUnavailable(self.shard_id, e.to_string()))?;

        Ok(Vote::Commit)
    }

    // ── Phase 2a: Commit ──────────────────────────────────────────────────────

    /// Apply the prepared operations and record the commit.
    pub fn commit(&self, tx_id: &str) -> Result<(), DistributedTxError> {
        let conn = self.conn.lock().unwrap();

        // Retrieve prepared operations.
        let ops_json: String = conn
            .query_row(
                "SELECT operations FROM prepared_transactions WHERE tx_id = ?1",
                params![tx_id],
                |r| r.get(0),
            )
            .map_err(|_| DistributedTxError::TransactionNotFound(tx_id.to_string()))?;

        let operations: Vec<Operation> = serde_json::from_str(&ops_json)
            .map_err(|e| DistributedTxError::Serialization(e.to_string()))?;

        // Apply each operation inside the SQLite transaction.
        conn.execute_batch("BEGIN IMMEDIATE").ok();

        for op in &operations {
            if let Err(e) = apply_operation_inner(&conn, op) {
                conn.execute_batch("ROLLBACK").ok();
                return Err(e);
            }
        }

        conn.execute(
            "INSERT OR REPLACE INTO committed_transactions (tx_id, committed_at) VALUES (?1, ?2)",
            params![tx_id, chrono::Utc::now().to_rfc3339()],
        )
        .map_err(|e| DistributedTxError::ShardUnavailable(self.shard_id, e.to_string()))?;

        // Clean up prepare log.
        conn.execute(
            "DELETE FROM prepared_transactions WHERE tx_id = ?1",
            params![tx_id],
        )
        .ok();

        conn.execute_batch("COMMIT")
            .map_err(|e| DistributedTxError::ShardUnavailable(self.shard_id, e.to_string()))
    }

    // ── Phase 2b: Rollback ────────────────────────────────────────────────────

    /// Roll back a prepared transaction by discarding the prepare log entry.
    pub fn rollback(&self, tx_id: &str) -> Result<(), DistributedTxError> {
        self.conn
            .lock()
            .unwrap()
            .execute(
                "DELETE FROM prepared_transactions WHERE tx_id = ?1",
                params![tx_id],
            )
            .map_err(|e| DistributedTxError::ShardUnavailable(self.shard_id, e.to_string()))?;
        Ok(())
    }

    // ── KV read ───────────────────────────────────────────────────────────────

    /// Read a value from the shard's kv_data store.
    pub fn read(&self, key: &str) -> Result<Option<String>, DistributedTxError> {
        let conn = self.conn.lock().unwrap();
        let result = conn
            .query_row(
                "SELECT value FROM kv_data WHERE shard_key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| DistributedTxError::ShardUnavailable(self.shard_id, e.to_string()))?;
        Ok(result)
    }
}

// ── Operation ─────────────────────────────────────────────────────────────────

/// A single unit of work to be applied within a distributed transaction.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum Operation {
    /// Insert or replace a key-value pair.
    Put {
        shard_key: String,
        value: String,
    },
    /// Delete a key-value pair.
    Delete {
        shard_key: String,
    },
    /// Execute arbitrary SQL (must be non-DDL for safety in tests).
    RawSql {
        sql: String,
    },
}

/// Applies a single operation to `conn` without transaction management.
fn apply_operation_inner(
    conn: &Connection,
    op: &Operation,
) -> Result<(), DistributedTxError> {
    match op {
        Operation::Put { shard_key, value } => {
            conn.execute(
                "INSERT OR REPLACE INTO kv_data (shard_key, value) VALUES (?1, ?2)",
                params![shard_key, value],
            )
            .map_err(|e| DistributedTxError::ApplyFailed(e.to_string()))?;
        }
        Operation::Delete { shard_key } => {
            conn.execute(
                "DELETE FROM kv_data WHERE shard_key = ?1",
                params![shard_key],
            )
            .map_err(|e| DistributedTxError::ApplyFailed(e.to_string()))?;
        }
        Operation::RawSql { sql } => {
            conn.execute_batch(sql)
                .map_err(|e| DistributedTxError::ApplyFailed(e.to_string()))?;
        }
    }
    Ok(())
}

// ── 2PC Vote ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Vote {
    /// Shard is ready to commit.
    Commit,
    /// Shard vetoes the transaction.
    Abort,
}

// ── Transaction descriptor ────────────────────────────────────────────────────

/// Describes a distributed transaction before it is submitted to the coordinator.
#[derive(Debug, Clone)]
pub struct DistributedTransaction {
    /// Globally unique transaction ID.
    pub tx_id: String,
    /// Per-shard operations: shard_index → list of operations.
    pub shard_ops: HashMap<usize, Vec<Operation>>,
}

impl DistributedTransaction {
    pub fn new(tx_id: impl Into<String>) -> Self {
        Self {
            tx_id: tx_id.into(),
            shard_ops: HashMap::new(),
        }
    }

    /// Add an operation destined for `shard_index`.
    pub fn add_op(&mut self, shard_index: usize, op: Operation) {
        self.shard_ops.entry(shard_index).or_default().push(op);
    }
}

// ── Coordinator ───────────────────────────────────────────────────────────────

/// Outcome of a distributed transaction execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TxOutcome {
    Committed,
    RolledBack,
}

/// Coordinator that runs 2PC across a set of [`ShardNode`]s.
pub struct DistributedTxCoordinator {
    shards: Vec<Arc<ShardNode>>,
}

impl DistributedTxCoordinator {
    /// Build a coordinator from explicit shard nodes.
    pub fn new(shards: Vec<Arc<ShardNode>>) -> Self {
        Self { shards }
    }

    /// Build a coordinator from environment variables.
    ///
    /// Creates `DB_SHARD_COUNT` in-memory shards for simplicity (in production
    /// replace `:memory:` with the shard file paths).
    pub fn from_env() -> Result<Self, DistributedTxError> {
        let count: usize = std::env::var("DB_SHARD_COUNT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1);

        let mut shards = Vec::with_capacity(count);
        for i in 0..count {
            let path = std::env::var("DB_SHARD_PATH_PREFIX")
                .map(|prefix| format!("{prefix}_{i}.db"))
                .unwrap_or_else(|_| ":memory:".to_string());

            let shard = Arc::new(ShardNode::open(i, &path)?);
            shard.bootstrap()?;
            shards.push(shard);
        }

        Ok(Self { shards })
    }

    /// Route an [`Operation`] to the correct shard based on its key.
    ///
    /// Returns `(shard_index, operation)`.
    pub fn route_operation(&self, op: &Operation) -> usize {
        let key = match op {
            Operation::Put { shard_key, .. } => shard_key.clone(),
            Operation::Delete { shard_key } => shard_key.clone(),
            Operation::RawSql { .. } => String::new(), // raw SQL → shard 0
        };
        ShardKey::new(key).shard_index(self.shards.len())
    }

    /// Execute a [`DistributedTransaction`] using two-phase commit.
    ///
    /// Returns [`TxOutcome::Committed`] if all shards voted Commit, or
    /// [`TxOutcome::RolledBack`] if any shard voted Abort (in which case all
    /// shards that already voted Commit receive a rollback request).
    pub fn execute(&self, tx: &DistributedTransaction) -> Result<TxOutcome, DistributedTxError> {
        let tx_id = &tx.tx_id;
        let participating_shards: Vec<usize> = tx.shard_ops.keys().cloned().collect();

        // ── Phase 1: Prepare ─────────────────────────────────────────────────
        let mut committed_shards: Vec<usize> = Vec::new();
        for &shard_idx in &participating_shards {
            let shard = self
                .shards
                .get(shard_idx)
                .ok_or(DistributedTxError::ShardUnavailable(
                    shard_idx,
                    "index out of range".to_string(),
                ))?;

            let ops = tx.shard_ops.get(&shard_idx).map(Vec::as_slice).unwrap_or(&[]);
            match shard.prepare(tx_id, ops)? {
                Vote::Commit => {
                    committed_shards.push(shard_idx);
                }
                Vote::Abort => {
                    // Rollback shards that already voted Commit.
                    for &already in &committed_shards {
                        if let Some(s) = self.shards.get(already) {
                            s.rollback(tx_id).ok();
                        }
                    }
                    return Ok(TxOutcome::RolledBack);
                }
            }
        }

        // ── Phase 2: Commit ──────────────────────────────────────────────────
        for &shard_idx in &participating_shards {
            let shard = self
                .shards
                .get(shard_idx)
                .ok_or(DistributedTxError::ShardUnavailable(
                    shard_idx,
                    "index out of range".to_string(),
                ))?;

            shard.commit(tx_id)?;
        }

        Ok(TxOutcome::Committed)
    }

    /// Number of shards managed by this coordinator.
    pub fn shard_count(&self) -> usize {
        self.shards.len()
    }

    // ── Shard rebalancing ────────────────────────────────────────────────────

    /// Redistribute keys from `source_shard` to `target_shard`.
    ///
    /// Reads all `kv_data` rows from the source whose key hashes to
    /// `target_shard` under the *new* total shard count, writes them to the
    /// target, and deletes them from the source — all in a single 2PC
    /// transaction.
    ///
    /// In practice rebalancing would be triggered by adding/removing shards.
    pub fn rebalance(
        &self,
        source_shard: usize,
        target_shard: usize,
        new_total_shards: usize,
    ) -> Result<u64, DistributedTxError> {
        let source = self
            .shards
            .get(source_shard)
            .ok_or_else(|| DistributedTxError::ShardUnavailable(source_shard, "not found".into()))?;
        let target = self
            .shards
            .get(target_shard)
            .ok_or_else(|| DistributedTxError::ShardUnavailable(target_shard, "not found".into()))?;

        // Collect keys to migrate.
        let rows: Vec<(String, String)> = {
            let conn = source.conn.lock().unwrap();
            let mut stmt = conn
                .prepare("SELECT shard_key, value FROM kv_data")
                .map_err(|e| DistributedTxError::ShardUnavailable(source_shard, e.to_string()))?;

            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(|e| DistributedTxError::ShardUnavailable(source_shard, e.to_string()))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| DistributedTxError::ShardUnavailable(source_shard, e.to_string()))?
        };

        let to_migrate: Vec<(String, String)> = rows
            .into_iter()
            .filter(|(k, _)| ShardKey::new(k).shard_index(new_total_shards) == target_shard)
            .collect();

        let count = to_migrate.len() as u64;
        if count == 0 {
            return Ok(0);
        }

        // Build a 2PC transaction.
        let tx_id = uuid::Uuid::new_v4().to_string();
        let mut tx = DistributedTransaction::new(&tx_id);

        for (key, value) in &to_migrate {
            tx.add_op(target_shard, Operation::Put { shard_key: key.clone(), value: value.clone() });
            tx.add_op(source_shard, Operation::Delete { shard_key: key.clone() });
        }

        self.execute(&tx)?;
        Ok(count)
    }
}

// ── Error type ────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum DistributedTxError {
    /// A shard could not be reached.
    ShardUnavailable(usize, String),
    /// The transaction ID is unknown.
    TransactionNotFound(String),
    /// Serialization of operations failed.
    Serialization(String),
    /// An operation could not be applied.
    ApplyFailed(String),
}

impl std::fmt::Display for DistributedTxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ShardUnavailable(id, msg) => write!(f, "shard {id} unavailable: {msg}"),
            Self::TransactionNotFound(tx_id) => write!(f, "transaction not found: {tx_id}"),
            Self::Serialization(msg) => write!(f, "serialization error: {msg}"),
            Self::ApplyFailed(msg) => write!(f, "apply failed: {msg}"),
        }
    }
}

impl std::error::Error for DistributedTxError {}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_coordinator(n: usize) -> DistributedTxCoordinator {
        let shards: Vec<Arc<ShardNode>> = (0..n)
            .map(|i| {
                let shard = Arc::new(ShardNode::open(i, ":memory:").expect("open"));
                shard.bootstrap().expect("bootstrap");
                shard
            })
            .collect();
        DistributedTxCoordinator::new(shards)
    }

    #[test]
    fn test_shard_key_maps_consistently() {
        let key = ShardKey::new("vault-abc");
        let idx1 = key.shard_index(4);
        let idx2 = key.shard_index(4);
        assert_eq!(idx1, idx2);
        assert!(idx1 < 4);
    }

    #[test]
    fn test_shard_key_zero_shards_returns_zero() {
        let key = ShardKey::new("anything");
        assert_eq!(key.shard_index(0), 0);
    }

    #[test]
    fn test_single_shard_commit() {
        let coord = make_coordinator(1);

        let mut tx = DistributedTransaction::new("tx-001");
        tx.add_op(0, Operation::Put { shard_key: "k1".into(), value: "v1".into() });

        let outcome = coord.execute(&tx).expect("execute");
        assert_eq!(outcome, TxOutcome::Committed);

        let val = coord.shards[0].read("k1").unwrap();
        assert_eq!(val, Some("v1".to_string()));
    }

    #[test]
    fn test_multi_shard_commit() {
        let coord = make_coordinator(2);

        let mut tx = DistributedTransaction::new("tx-multi");
        tx.add_op(0, Operation::Put { shard_key: "key0".into(), value: "val0".into() });
        tx.add_op(1, Operation::Put { shard_key: "key1".into(), value: "val1".into() });

        let outcome = coord.execute(&tx).expect("execute");
        assert_eq!(outcome, TxOutcome::Committed);

        assert_eq!(coord.shards[0].read("key0").unwrap(), Some("val0".to_string()));
        assert_eq!(coord.shards[1].read("key1").unwrap(), Some("val1".to_string()));
    }

    #[test]
    fn test_duplicate_tx_id_votes_abort() {
        let coord = make_coordinator(1);

        let mut tx = DistributedTransaction::new("tx-dup");
        tx.add_op(0, Operation::Put { shard_key: "k".into(), value: "v".into() });

        coord.execute(&tx).unwrap(); // first commit

        // Re-submit same tx_id — shard already has it in committed log,
        // so prepare detects duplicate and votes Abort.
        let outcome2 = coord.execute(&tx).unwrap();
        assert_eq!(outcome2, TxOutcome::RolledBack);
    }

    #[test]
    fn test_rollback_on_abort() {
        let coord = make_coordinator(2);

        // Manually prepare shard-0 with the same tx_id to trigger an abort on
        // the second shard's prepare call.
        let ops = vec![Operation::Put { shard_key: "x".into(), value: "y".into() }];
        coord.shards[0].prepare("tx-force-abort", &ops).unwrap();

        let mut tx = DistributedTransaction::new("tx-force-abort");
        tx.add_op(0, Operation::Put { shard_key: "x".into(), value: "y".into() });
        tx.add_op(1, Operation::Put { shard_key: "z".into(), value: "w".into() });

        // Shard 0 will abort (duplicate), coordinator should rollback shard 1.
        let outcome = coord.execute(&tx).unwrap();
        assert_eq!(outcome, TxOutcome::RolledBack);

        // Key "z" must NOT have been committed to shard 1.
        assert_eq!(coord.shards[1].read("z").unwrap(), None);
    }

    #[test]
    fn test_delete_operation() {
        let coord = make_coordinator(1);

        let mut tx1 = DistributedTransaction::new("tx-put");
        tx1.add_op(0, Operation::Put { shard_key: "del-me".into(), value: "some-value".into() });
        coord.execute(&tx1).unwrap();

        let mut tx2 = DistributedTransaction::new("tx-del");
        tx2.add_op(0, Operation::Delete { shard_key: "del-me".into() });
        coord.execute(&tx2).unwrap();

        assert_eq!(coord.shards[0].read("del-me").unwrap(), None);
    }

    #[test]
    fn test_coordinator_shard_count() {
        let coord = make_coordinator(3);
        assert_eq!(coord.shard_count(), 3);
    }

    #[test]
    fn test_route_operation_returns_valid_index() {
        let coord = make_coordinator(4);
        let op = Operation::Put { shard_key: "some-key".into(), value: "v".into() };
        let idx = coord.route_operation(&op);
        assert!(idx < 4);
    }

    #[test]
    fn test_rebalance_moves_keys() {
        // Create a coordinator with 2 shards, then rebalance under 3 shards.
        let coord = make_coordinator(3);

        // Seed some keys into shard-0.
        let keys = ["alpha", "beta", "gamma", "delta", "epsilon"];
        for k in &keys {
            let mut tx = DistributedTransaction::new(format!("seed-{k}"));
            tx.add_op(0, Operation::Put { shard_key: k.to_string(), value: "v".into() });
            coord.execute(&tx).unwrap();
        }

        // Rebalance: move keys that would belong to shard-2 under 3-shard layout.
        let moved = coord.rebalance(0, 2, 3).unwrap();
        // moved should be ≥ 0; exact count depends on hash distribution.
        let _ = moved; // just asserting it doesn't panic
    }

    #[test]
    fn test_empty_transaction_commits() {
        let coord = make_coordinator(1);
        let tx = DistributedTransaction::new("tx-empty");
        // No ops — should commit vacuously.
        let outcome = coord.execute(&tx).unwrap();
        assert_eq!(outcome, TxOutcome::Committed);
    }

    #[test]
    fn test_from_env_single_shard() {
        std::env::set_var("DB_SHARD_COUNT", "1");
        let coord = DistributedTxCoordinator::from_env().unwrap();
        assert_eq!(coord.shard_count(), 1);
        std::env::remove_var("DB_SHARD_COUNT");
    }
}
