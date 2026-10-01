-- The schema of the document index (`index.rs`).
-- A change to this file needs a new SCHEMA_VERSION in `index.rs`. A
-- test there fails until the version and SCHEMA_SHA256 have new values.

-- One row per indexed document. `file_mtime` (nanoseconds since the
-- Unix epoch) and `file_size` are the stamp of the document file.
CREATE TABLE documents (
    cache_id   TEXT PRIMARY KEY,
    source     TEXT NOT NULL,
    file_mtime INTEGER NOT NULL,
    file_size  INTEGER NOT NULL,
    indexed_at TEXT NOT NULL
) STRICT;

-- One row per document that holds an agent session.
CREATE TABLE sessions (
    cache_id      TEXT PRIMARY KEY REFERENCES documents(cache_id) ON DELETE CASCADE,
    dir           TEXT NOT NULL,
    title         TEXT NOT NULL,
    started_at    TEXT,
    last_activity TEXT
) STRICT;
