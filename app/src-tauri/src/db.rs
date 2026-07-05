//! Opening the catalogue database and running schema migrations.
//!
//! SPEC.md S5 (Data Model & Metadata) + S9 (Write Architecture & Contention):
//! a single SQLite database in WAL mode, migrated with plain versioned SQL
//! files tracked via `PRAGMA user_version` (never delete-and-recreate).

use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;

/// Ordered list of migrations. Each is applied exactly once, in ascending
/// order, tracked via `PRAGMA user_version`. Add new migrations by appending
/// a new `(version, sql)` entry - never edit an already-shipped migration.
const MIGRATIONS: &[(i64, &str)] = &[(1, include_str!("migrations/0001_init.sql"))];

/// Opens the catalogue database at `path`, applies the required PRAGMAs, and
/// runs any pending migrations. This is the single writer connection; hand
/// it to `WriteQueue::start` rather than using it directly for writes.
pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    configure(&conn)?;
    run_migrations(&conn)?;
    Ok(conn)
}

/// Opens a second connection to the same database file for UI reads,
/// isolated from the write queue's single writer connection (SPEC S9:
/// "Read paths ... isolated from the write lane"). WAL mode lets this read
/// concurrently with the writer without blocking on it.
pub fn open_reader(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_millis(5_000))?;
    Ok(conn)
}

fn configure(conn: &Connection) -> rusqlite::Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.busy_timeout(Duration::from_millis(5_000))?;
    Ok(())
}

fn run_migrations(conn: &Connection) -> rusqlite::Result<()> {
    let current_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;

    for (version, sql) in MIGRATIONS {
        if *version <= current_version {
            continue;
        }
        conn.execute_batch(sql)?;
        // PRAGMA doesn't accept bound parameters; `version` is a compile-time
        // constant from the table above, never user input.
        conn.execute_batch(&format!("PRAGMA user_version = {version};"))?;
    }

    Ok(())
}

/// Finds (or creates, on a fresh database) the default "Imports" collection
/// that placeholder works from `import_paths` land in until the UI grows
/// real collection management. Typed `personal` so it never becomes
/// sync-eligible by accident (SPEC S5 `collections.type`).
pub fn ensure_default_collection(conn: &Connection) -> rusqlite::Result<i64> {
    let existing = conn.query_row(
        "SELECT id FROM collections WHERE name = 'Imports' LIMIT 1",
        [],
        |row| row.get::<_, i64>(0),
    );
    match existing {
        Ok(id) => return Ok(id),
        Err(rusqlite::Error::QueryReturnedNoRows) => {}
        // A real database error (corruption, schema mismatch) must surface,
        // not be swallowed into a duplicate INSERT attempt.
        Err(e) => return Err(e),
    }

    conn.execute(
        "INSERT INTO collections (name, root_path, type) VALUES ('Imports', '', 'personal')",
        [],
    )?;
    Ok(conn.last_insert_rowid())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_create_expected_tables_and_fts() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        run_migrations(&conn).unwrap();

        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap();
        let tables: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();

        for expected in [
            "annotations",
            "collections",
            "files",
            "relationships",
            "saved_views",
            "tags",
            "works",
            "works_fts",
        ] {
            assert!(
                tables.iter().any(|t| t == expected),
                "missing table {expected}"
            );
        }

        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 1);
    }

    #[test]
    fn migrations_are_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        run_migrations(&conn).unwrap();
        // Running again on an already-migrated database must be a no-op,
        // not a re-create attempt.
        run_migrations(&conn).unwrap();
    }

    #[test]
    fn fts_tracks_works_title_via_triggers() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        run_migrations(&conn).unwrap();
        let collection_id = ensure_default_collection(&conn).unwrap();

        conn.execute(
            "INSERT INTO works (collection_id, title, medium_type, container_type) \
             VALUES (?1, 'Seven Samurai', 'video', 'standalone')",
            [collection_id],
        )
        .unwrap();

        let hits: i64 = conn
            .query_row(
                "SELECT count(*) FROM works_fts WHERE works_fts MATCH 'Samurai'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1);
    }

    #[test]
    fn ensure_default_collection_reuses_existing_row() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        run_migrations(&conn).unwrap();

        let first = ensure_default_collection(&conn).unwrap();
        let second = ensure_default_collection(&conn).unwrap();
        assert_eq!(first, second);
    }
}
