//! Tauri commands: the frontend's only way to talk to the Rust core
//! (SPEC.md - "the frontend calls Rust commands", no HTTP layer).

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::Serialize;

use crate::db;
use crate::write_queue::WriteQueue;

/// Shared app state handed to every command via `tauri::State`.
pub struct AppState {
    pub write_queue: WriteQueue,
    pub db_path: PathBuf,
    pub default_collection_id: i64,
}

#[derive(Debug, Serialize)]
pub struct ImportCounts {
    pub works_created: u32,
    pub files_created: u32,
}

/// Records each of `paths` as a placeholder work + file, through the write
/// queue's UI-immediate lane (SPEC S9). Real hashing/identification/
/// enrichment happen later, as background jobs on the same queue.
#[tauri::command]
pub async fn import_paths(
    paths: Vec<String>,
    state: tauri::State<'_, AppState>,
) -> Result<ImportCounts, String> {
    let default_collection_id = state.default_collection_id;
    let (result_tx, result_rx) = std::sync::mpsc::channel::<Result<ImportCounts, String>>();

    state
        .write_queue
        .submit_immediate(Box::new(move |conn| {
            let result = import_paths_tx(conn, &paths, default_collection_id);
            let _ = result_tx.send(result);
        }))
        .map_err(|e| e.to_string())?;

    tauri::async_runtime::spawn_blocking(move || {
        result_rx
            .recv()
            .map_err(|_| "write queue closed before import finished".to_string())?
    })
    .await
    .map_err(|e| e.to_string())?
}

fn import_paths_tx(
    conn: &Connection,
    paths: &[String],
    collection_id: i64,
) -> Result<ImportCounts, String> {
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    // Explicit rollback on any mid-batch failure: the writer connection is
    // long-lived and shared by every queued job, so a partially-written
    // transaction must never be left open on it.
    match insert_paths(&tx, paths, collection_id) {
        Ok(counts) => {
            tx.commit().map_err(|e| e.to_string())?;
            Ok(counts)
        }
        Err(e) => {
            let _ = tx.rollback();
            Err(e)
        }
    }
}

fn insert_paths(
    tx: &rusqlite::Transaction,
    paths: &[String],
    collection_id: i64,
) -> Result<ImportCounts, String> {
    let mut works_created = 0u32;
    let mut files_created = 0u32;

    for path in paths {
        let title = title_from_path(path);
        let medium_type = medium_type_from_path(path);

        tx.execute(
            "INSERT INTO works (collection_id, title, medium_type, container_type) \
             VALUES (?1, ?2, ?3, 'standalone')",
            rusqlite::params![collection_id, title, medium_type],
        )
        .map_err(|e| e.to_string())?;
        let work_id = tx.last_insert_rowid();
        works_created += 1;

        tx.execute(
            "INSERT INTO files (work_id, path, status) VALUES (?1, ?2, 'local')",
            rusqlite::params![work_id, path],
        )
        .map_err(|e| e.to_string())?;
        files_created += 1;
    }

    Ok(ImportCounts {
        works_created,
        files_created,
    })
}

fn title_from_path(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
        .to_string()
}

/// Best-effort medium-type guess from the file extension. Real 1.0 behavior
/// (EXIF/ID3 extraction, API/filename identification) lands later - see
/// SPEC.md "Metadata Extraction & Storage"; this just gives placeholder
/// works a sane `medium_type` so the grid/filter UI has something to show.
fn medium_type_from_path(path: &str) -> &'static str {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    match ext.as_str() {
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "tiff" | "tif" => "image",
        "mp4" | "mkv" | "avi" | "mov" | "webm" => "video",
        "mp3" | "flac" | "wav" | "ogg" | "m4a" => "audio",
        "epub" | "mobi" | "azw3" => "book",
        "pdf" | "txt" | "doc" | "docx" => "document",
        _ => "other",
    }
}

#[derive(Debug, Serialize)]
pub struct WorkSummary {
    pub id: i64,
    pub title: String,
    pub medium_type: String,
    pub container_type: String,
    pub possession: bool,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
pub struct WorksPage {
    pub items: Vec<WorkSummary>,
    pub offset: i64,
    pub limit: i64,
    pub total: i64,
}

/// The largest page `list_works` will ever return in one call, regardless of
/// what the caller asks for, so the UI can never accidentally load the full
/// catalogue (SPEC.md Performance Requirements - "Paginated SQL queries;
/// don't load full dataset into memory").
const MAX_PAGE_SIZE: i64 = 500;

/// Paginated, filterable listing of works. Opens its own short-lived read
/// connection (SPEC S9: read paths isolated from the write lane) rather than
/// going through the write queue.
#[tauri::command]
pub async fn list_works(
    offset: i64,
    limit: i64,
    medium_filter: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<WorksPage, String> {
    let db_path = state.db_path.clone();
    let offset = offset.max(0);
    let limit = limit.clamp(1, MAX_PAGE_SIZE);

    tauri::async_runtime::spawn_blocking(move || {
        let conn = db::open_reader(&db_path).map_err(|e| e.to_string())?;
        list_works_tx(&conn, offset, limit, medium_filter.as_deref())
    })
    .await
    .map_err(|e| e.to_string())?
}

fn list_works_tx(
    conn: &Connection,
    offset: i64,
    limit: i64,
    medium_filter: Option<&str>,
) -> Result<WorksPage, String> {
    let total: i64 = match medium_filter {
        Some(medium) => conn
            .query_row(
                "SELECT count(*) FROM works WHERE medium_type = ?1",
                [medium],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?,
        None => conn
            .query_row("SELECT count(*) FROM works", [], |row| row.get(0))
            .map_err(|e| e.to_string())?,
    };

    let map_row = |row: &rusqlite::Row| -> rusqlite::Result<WorkSummary> {
        Ok(WorkSummary {
            id: row.get(0)?,
            title: row.get(1)?,
            medium_type: row.get(2)?,
            container_type: row.get(3)?,
            possession: row.get::<_, i64>(4)? != 0,
            created_at: row.get(5)?,
        })
    };

    let items: Vec<WorkSummary> = match medium_filter {
        Some(medium) => {
            let mut stmt = conn
                .prepare(
                    "SELECT id, title, medium_type, container_type, possession, created_at \
                     FROM works WHERE medium_type = ?1 \
                     ORDER BY created_at DESC LIMIT ?2 OFFSET ?3",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(rusqlite::params![medium, limit, offset], map_row)
                .map_err(|e| e.to_string())?
                .collect::<rusqlite::Result<_>>()
                .map_err(|e| e.to_string())?;
            rows
        }
        None => {
            let mut stmt = conn
                .prepare(
                    "SELECT id, title, medium_type, container_type, possession, created_at \
                     FROM works ORDER BY created_at DESC LIMIT ?1 OFFSET ?2",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(rusqlite::params![limit, offset], map_row)
                .map_err(|e| e.to_string())?
                .collect::<rusqlite::Result<_>>()
                .map_err(|e| e.to_string())?;
            rows
        }
    };

    Ok(WorksPage {
        items,
        offset,
        limit,
        total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn migrated_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        conn.execute_batch(include_str!("migrations/0001_init.sql"))
            .unwrap();
        conn
    }

    #[test]
    fn import_paths_tx_creates_placeholder_works_and_files() {
        let conn = migrated_conn();
        let collection_id = db::ensure_default_collection(&conn).unwrap();

        let paths = vec![
            "/hoard/vacation.jpg".to_string(),
            "/hoard/notes.pdf".to_string(),
        ];
        let counts = import_paths_tx(&conn, &paths, collection_id).unwrap();

        assert_eq!(counts.works_created, 2);
        assert_eq!(counts.files_created, 2);

        let titles: Vec<String> = conn
            .prepare("SELECT title FROM works ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(titles, vec!["vacation", "notes"]);
    }

    #[test]
    fn list_works_tx_paginates_and_filters_by_medium() {
        let conn = migrated_conn();
        let collection_id = db::ensure_default_collection(&conn).unwrap();

        let paths = vec![
            "/a.jpg".to_string(),
            "/b.jpg".to_string(),
            "/c.mp3".to_string(),
        ];
        import_paths_tx(&conn, &paths, collection_id).unwrap();

        let all = list_works_tx(&conn, 0, 10, None).unwrap();
        assert_eq!(all.total, 3);
        assert_eq!(all.items.len(), 3);

        let images = list_works_tx(&conn, 0, 10, Some("image")).unwrap();
        assert_eq!(images.total, 2);
        assert!(images.items.iter().all(|w| w.medium_type == "image"));

        let first_page = list_works_tx(&conn, 0, 2, None).unwrap();
        assert_eq!(first_page.items.len(), 2);
        assert_eq!(first_page.total, 3);
    }
}
