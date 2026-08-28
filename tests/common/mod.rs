//! Shared test helpers for building temporary SQLite databases from schema
//! fixture files (see `tests/fixtures/`).

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use tempfile::TempDir;

/// Creates a fresh temp SQLite database, applies the DDL in `schema_path`
/// to it, and returns the owning [`TempDir`] (keep it alive for as long as
/// the database is needed — dropping it deletes the file) along with the
/// database's path.
pub fn temp_db_from_schema(schema_path: &Path) -> (TempDir, PathBuf) {
    let schema = fs::read_to_string(schema_path)
        .unwrap_or_else(|e| panic!("failed to read schema fixture {schema_path:?}: {e}"));

    let dir = TempDir::new().expect("create temp dir");
    let db_path = dir.path().join("test.db");

    let conn = Connection::open(&db_path).expect("open temp sqlite db");
    conn.execute_batch(&schema)
        .unwrap_or_else(|e| panic!("failed to apply schema {schema_path:?}: {e}"));

    (dir, db_path)
}

/// Path to a fixture file under `tests/fixtures/`.
pub fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}
