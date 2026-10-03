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
const MIGRATIONS: &[(i64, &str)] = &[
    (1, include_str!("migrations/0001_init.sql")),
    (2, include_str!("migrations/0002_semantic_guard.sql")),
];

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
    // A3: a reader connection must never write.
    conn.pragma_update(None, "query_only", true)?;
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
        assert_eq!(version, 2);
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

    /// Builds an in-memory database migrated to exactly `version` - the old
    /// fixture for migration-compat tests: seed data at the older schema,
    /// then run the normal `run_migrations` path forward.
    fn open_conn_at_version(version: i64) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        let sql = MIGRATIONS.iter().find(|(v, _)| *v == version).unwrap().1;
        conn.execute_batch(sql).unwrap();
        conn.execute_batch(&format!("PRAGMA user_version = {version};"))
            .unwrap();
        conn
    }

    #[test]
    fn sem_a3_reader_connection_rejects_writes() {
        let path = std::env::temp_dir().join(format!(
            "qurator_sem_a3_{}_{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let writer = open(&path).unwrap();
        let reader = open_reader(&path).unwrap();

        let reader_insert = reader.execute(
            "INSERT INTO collections (name, root_path, type) VALUES ('Reader', '', 'personal')",
            [],
        );
        assert!(reader_insert.is_err(), "reader connection must not write");

        let writer_insert = writer.execute(
            "INSERT INTO collections (name, root_path, type) VALUES ('Reader', '', 'personal')",
            [],
        );
        assert!(
            writer_insert.is_ok(),
            "the same INSERT must succeed on the writer"
        );

        drop(reader);
        drop(writer);
        for suffix in ["", "-wal", "-shm"] {
            let mut file = path.as_os_str().to_os_string();
            file.push(suffix);
            let _ = std::fs::remove_file(&file);
        }
    }

    #[test]
    fn sem_b2_identified_work_requires_provenance() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        run_migrations(&conn).unwrap();
        // A 'released' collection keeps B2 isolated from B3, which forbids
        // identification inside personal collections.
        conn.execute(
            "INSERT INTO collections (name, root_path, type) VALUES ('Released', '', 'released')",
            [],
        )
        .unwrap();
        let collection_id = conn.last_insert_rowid();

        // Baseline: an unidentified work whose source DEFAULTs to 'failed'.
        conn.execute(
            "INSERT INTO works (collection_id, title, medium_type, container_type) \
             VALUES (?1, 'Unidentified', 'video', 'standalone')",
            [collection_id],
        )
        .unwrap();

        let failed_insert = conn.execute(
            "INSERT INTO works (collection_id, work_identity, identification_source, title, \
             medium_type, container_type) \
             VALUES (?1, 'ident-1', 'failed', 'Failed', 'video', 'standalone')",
            [collection_id],
        );
        assert!(
            failed_insert.is_err(),
            "B2: work_identity with identification_source 'failed' must be rejected"
        );

        conn.execute(
            "INSERT INTO works (collection_id, work_identity, identification_source, title, \
             medium_type, container_type) \
             VALUES (?1, 'ident-2', 'api', 'Identified', 'video', 'standalone')",
            [collection_id],
        )
        .unwrap();

        let promoted = conn.execute(
            "UPDATE works SET work_identity = 'ident-3' WHERE work_identity IS NULL",
            [],
        );
        assert!(
            promoted.is_err(),
            "B2: UPDATE must not set work_identity while the source stays 'failed'"
        );
    }

    #[test]
    fn sem_b3_personal_collection_work_cannot_be_identified() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        run_migrations(&conn).unwrap();

        conn.execute(
            "INSERT INTO collections (name, root_path, type) VALUES ('Personal', '', 'personal')",
            [],
        )
        .unwrap();
        let personal = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO collections (name, root_path, type) VALUES ('Released', '', 'released')",
            [],
        )
        .unwrap();
        let released = conn.last_insert_rowid();

        let personal_insert = conn.execute(
            "INSERT INTO works (collection_id, work_identity, identification_source, title, \
             medium_type, container_type) \
             VALUES (?1, 'ident-p', 'api', 'Identified', 'video', 'standalone')",
            [personal],
        );
        assert!(
            personal_insert.is_err(),
            "B3: an identified work must not enter a personal collection"
        );

        conn.execute(
            "INSERT INTO works (collection_id, work_identity, identification_source, title, \
             medium_type, container_type) \
             VALUES (?1, 'ident-r', 'api', 'Identified', 'video', 'standalone')",
            [released],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO works (collection_id, title, medium_type, container_type) \
             VALUES (?1, 'Unidentified', 'video', 'standalone')",
            [personal],
        )
        .unwrap();
        let unidentified = conn.last_insert_rowid();
        let promoted = conn.execute(
            "UPDATE works SET work_identity = 'ident-p2' WHERE id = ?1",
            [unidentified],
        );
        assert!(
            promoted.is_err(),
            "B3: UPDATE must not identify a work in a personal collection"
        );
    }

    #[test]
    fn sem_b5_migration_0002_preserves_0001_data() {
        let conn = open_conn_at_version(1);

        conn.execute(
            "INSERT INTO collections (name, root_path, type) VALUES ('Imports', '', 'personal')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO collections (name, root_path, type) VALUES ('Shared', '/mnt/shared', 'released')",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO works (collection_id, title, medium_type, container_type) \
             VALUES (1, 'Unidentified', 'video', 'standalone')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO works (collection_id, work_identity, identification_source, title, \
             medium_type, container_type) \
             VALUES (2, 'ident-api', 'api', 'Identified', 'image', 'gallery')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO files (work_id, path, size_bytes) VALUES (2, '/mnt/shared/a.png', 123)",
            [],
        )
        .unwrap();

        run_migrations(&conn).unwrap();

        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 2);

        let collections: i64 = conn
            .query_row("SELECT count(*) FROM collections", [], |row| row.get(0))
            .unwrap();
        assert_eq!(collections, 2);
        let works: i64 = conn
            .query_row("SELECT count(*) FROM works", [], |row| row.get(0))
            .unwrap();
        assert_eq!(works, 2);
        let files: i64 = conn
            .query_row("SELECT count(*) FROM files", [], |row| row.get(0))
            .unwrap();
        assert_eq!(files, 1);

        let (identity, source): (Option<String>, String) = conn
            .query_row(
                "SELECT work_identity, identification_source FROM works WHERE title = 'Identified'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(identity.as_deref(), Some("ident-api"));
        assert_eq!(source, "api");

        let personal_type: String = conn
            .query_row(
                "SELECT type FROM collections WHERE name = 'Imports'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(personal_type, "personal");
    }

    #[test]
    fn sem_a7_no_job_table() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        run_migrations(&conn).unwrap();

        // A7's restart-rederivation half (rebuild queue state from the
        // catalogue on startup instead of persisting it) is pinned to M3;
        // until then no job/queue table may exist at all.
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
            .unwrap();
        let tables: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();

        for table in &tables {
            let lowered = table.to_lowercase();
            assert!(
                !lowered.contains("job") && !lowered.contains("queue"),
                "unexpected job/queue table: {table}"
            );
        }
    }
}
