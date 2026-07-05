-- Qurator initial schema (SPEC.md v3.1 - S5 Data Model & Metadata)
--
-- Six core tables + collections, the required indexes, and an FTS5 virtual
-- table over works.title with sync triggers. Applied once via db::run_migrations,
-- tracked with PRAGMA user_version (see db.rs).

CREATE TABLE collections (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    root_path TEXT NOT NULL,
    type TEXT NOT NULL CHECK (type IN ('personal', 'released', 'hybrid')),
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE works (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    collection_id INTEGER NOT NULL REFERENCES collections (id),
    work_identity TEXT,
    content_hash TEXT,
    identification_source TEXT NOT NULL DEFAULT 'failed'
        CHECK (identification_source IN ('api', 'filename', 'manual', 'failed')),
    title TEXT NOT NULL,
    medium_type TEXT NOT NULL
        CHECK (medium_type IN ('image', 'video', 'audio', 'book', 'document', 'other')),
    container_type TEXT NOT NULL CHECK (container_type IN ('gallery', 'standalone')),
    possession INTEGER NOT NULL DEFAULT 1,
    metadata TEXT NOT NULL DEFAULT '{}',
    extra TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_works_work_identity ON works (work_identity);
CREATE INDEX idx_works_content_hash ON works (content_hash);
CREATE INDEX idx_works_medium_type ON works (medium_type);
CREATE INDEX idx_works_title ON works (title);
CREATE INDEX idx_works_created_at ON works (created_at);
CREATE INDEX idx_works_container_type ON works (container_type);

CREATE TABLE files (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    work_id INTEGER NOT NULL REFERENCES works (id),
    sha256 TEXT,
    md5 TEXT,
    path TEXT NOT NULL,
    size_bytes INTEGER,
    mime_type TEXT,
    status TEXT NOT NULL DEFAULT 'local' CHECK (status IN ('local', 'network_only', 'broken'))
);

CREATE INDEX idx_files_sha256 ON files (sha256);
CREATE INDEX idx_files_md5 ON files (md5);
CREATE INDEX idx_files_work_id ON files (work_id);
CREATE INDEX idx_files_status ON files (status);

CREATE TABLE tags (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    work_id INTEGER NOT NULL REFERENCES works (id),
    value TEXT NOT NULL,
    tag_pack_id TEXT,
    is_subjective INTEGER NOT NULL DEFAULT 0,
    source_peer TEXT
);

CREATE INDEX idx_tags_work_id ON tags (work_id);
CREATE INDEX idx_tags_is_subjective ON tags (is_subjective);
CREATE INDEX idx_tags_tag_pack_id ON tags (tag_pack_id);

CREATE TABLE relationships (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    from_work_id INTEGER NOT NULL REFERENCES works (id),
    to_work_id INTEGER NOT NULL REFERENCES works (id),
    type TEXT NOT NULL CHECK (type IN ('contains', 'variant_of', 'related_to'))
);

CREATE INDEX idx_relationships_from_work_id ON relationships (from_work_id);
CREATE INDEX idx_relationships_to_work_id ON relationships (to_work_id);

CREATE TABLE annotations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    work_id INTEGER NOT NULL REFERENCES works (id),
    anchor TEXT NOT NULL,
    body TEXT,
    source_peer TEXT
);

CREATE INDEX idx_annotations_work_id ON annotations (work_id);

CREATE TABLE saved_views (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    context_type TEXT NOT NULL CHECK (context_type IN ('library', 'topic')),
    context_id TEXT,
    filter_state TEXT NOT NULL
);

-- FTS5 full-text search across works.title, kept in sync via triggers
-- (external-content table so title itself is not duplicated on disk).
CREATE VIRTUAL TABLE works_fts USING fts5 (
    title,
    content = 'works',
    content_rowid = 'id'
);

CREATE TRIGGER works_ai AFTER INSERT ON works BEGIN
    INSERT INTO works_fts (rowid, title) VALUES (new.id, new.title);
END;

CREATE TRIGGER works_ad AFTER DELETE ON works BEGIN
    INSERT INTO works_fts (works_fts, rowid, title) VALUES ('delete', old.id, old.title);
END;

CREATE TRIGGER works_au AFTER UPDATE ON works BEGIN
    INSERT INTO works_fts (works_fts, rowid, title) VALUES ('delete', old.id, old.title);
    INSERT INTO works_fts (rowid, title) VALUES (new.id, new.title);
END;
