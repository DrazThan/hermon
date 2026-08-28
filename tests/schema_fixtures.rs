//! Smoke tests proving the captured schema fixtures build into a working
//! temp SQLite database with the tables hermon reads.

mod common;

use rusqlite::Connection;

use common::{fixture_path, temp_db_from_schema};

fn table_names(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .expect("prepare sqlite_master query");
    stmt.query_map([], |row| row.get::<_, String>(0))
        .expect("query sqlite_master")
        .collect::<Result<_, _>>()
        .expect("collect table names")
}

#[test]
fn hermes_schema_builds_with_expected_tables() {
    let (_dir, db_path) = temp_db_from_schema(&fixture_path("hermes_schema.sql"));
    let conn = Connection::open(&db_path).expect("reopen temp db");

    let tables = table_names(&conn);
    assert!(tables.contains(&"sessions".to_string()), "tables: {tables:?}");
    assert!(tables.contains(&"messages".to_string()), "tables: {tables:?}");
}

#[test]
fn opencode_schema_builds_with_expected_tables() {
    let (_dir, db_path) = temp_db_from_schema(&fixture_path("opencode_schema.sql"));
    let conn = Connection::open(&db_path).expect("reopen temp db");

    let tables = table_names(&conn);
    assert!(tables.contains(&"session".to_string()), "tables: {tables:?}");
    assert!(tables.contains(&"message".to_string()), "tables: {tables:?}");
    assert!(tables.contains(&"part".to_string()), "tables: {tables:?}");
}
