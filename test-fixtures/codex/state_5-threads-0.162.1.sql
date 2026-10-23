-- The threads table as codex-cli 0.162.1 creates it in state_5.sqlite (`sqlite3 state_5.sqlite '.schema threads'`),
-- schema only, 2026-10-19. The two REFERENCES clauses are dropped (their tables are not created here).
CREATE TABLE threads (
    id TEXT PRIMARY KEY,
    rollout_path TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    source TEXT NOT NULL,
    model_provider TEXT NOT NULL,
    cwd TEXT NOT NULL,
    title TEXT NOT NULL,
    sandbox_policy TEXT NOT NULL,
    approval_mode TEXT NOT NULL,
    tokens_used INTEGER NOT NULL DEFAULT 0,
    has_user_event INTEGER NOT NULL DEFAULT 0,
    archived INTEGER NOT NULL DEFAULT 0,
    archived_at INTEGER,
    git_sha TEXT,
    git_branch TEXT,
    git_origin_url TEXT
, cli_version TEXT NOT NULL DEFAULT '', first_user_message TEXT NOT NULL DEFAULT '', agent_nickname TEXT, agent_role TEXT, memory_mode TEXT NOT NULL DEFAULT 'enabled', model TEXT, reasoning_effort TEXT, agent_path TEXT, created_at_ms INTEGER, updated_at_ms INTEGER, thread_source TEXT, preview TEXT NOT NULL DEFAULT '', recency_at INTEGER NOT NULL DEFAULT 0, recency_at_ms INTEGER NOT NULL DEFAULT 0, history_mode TEXT NOT NULL DEFAULT 'legacy', name TEXT, is_pinned INTEGER NOT NULL DEFAULT 0, thread_section_id TEXT, section_position INTEGER, section_entered_at_ms INTEGER, project_id TEXT);
CREATE INDEX idx_threads_created_at ON threads(created_at DESC, id DESC);
CREATE INDEX idx_threads_updated_at ON threads(updated_at DESC, id DESC);
CREATE INDEX idx_threads_archived ON threads(archived);
CREATE INDEX idx_threads_source ON threads(source);
CREATE INDEX idx_threads_provider ON threads(model_provider);
CREATE TRIGGER threads_created_at_ms_after_insert
AFTER INSERT ON threads
WHEN NEW.created_at_ms IS NULL
BEGIN
    UPDATE threads
    SET created_at_ms = NEW.created_at * 1000
    WHERE id = NEW.id;
END;
CREATE TRIGGER threads_updated_at_ms_after_insert
AFTER INSERT ON threads
WHEN NEW.updated_at_ms IS NULL
BEGIN
    UPDATE threads
    SET updated_at_ms = NEW.updated_at * 1000
    WHERE id = NEW.id;
END;
CREATE TRIGGER threads_created_at_ms_after_update
AFTER UPDATE OF created_at ON threads
WHEN NEW.created_at != OLD.created_at
 AND NEW.created_at_ms IS OLD.created_at_ms
BEGIN
    UPDATE threads
    SET created_at_ms = NEW.created_at * 1000
    WHERE id = NEW.id;
END;
CREATE TRIGGER threads_updated_at_ms_after_update
AFTER UPDATE OF updated_at ON threads
WHEN NEW.updated_at != OLD.updated_at
 AND NEW.updated_at_ms IS OLD.updated_at_ms
BEGIN
    UPDATE threads
    SET updated_at_ms = NEW.updated_at * 1000
    WHERE id = NEW.id;
END;
CREATE INDEX idx_threads_created_at_ms ON threads(created_at_ms DESC, id DESC);
CREATE INDEX idx_threads_updated_at_ms ON threads(updated_at_ms DESC, id DESC);
CREATE INDEX idx_threads_archived_cwd_created_at_ms ON threads(archived, cwd, created_at_ms DESC, id DESC);
CREATE INDEX idx_threads_archived_cwd_updated_at_ms ON threads(archived, cwd, updated_at_ms DESC, id DESC);
CREATE INDEX idx_threads_visible_created_at_ms
    ON threads(archived, created_at_ms DESC)
    WHERE preview <> '';
CREATE INDEX idx_threads_visible_updated_at_ms
    ON threads(archived, updated_at_ms DESC)
    WHERE preview <> '';
CREATE TRIGGER threads_recency_at_after_insert
AFTER INSERT ON threads
WHEN NEW.recency_at_ms = 0
BEGIN
    UPDATE threads
    SET recency_at = NEW.updated_at,
        recency_at_ms = COALESCE(NEW.updated_at_ms, NEW.updated_at * 1000)
    WHERE id = NEW.id;
END;
CREATE INDEX idx_threads_recency_at_ms
    ON threads(recency_at_ms DESC, id DESC);
CREATE INDEX idx_threads_archived_cwd_recency_at_ms
    ON threads(archived, cwd, recency_at_ms DESC, id DESC);
CREATE INDEX idx_threads_visible_recency_at_ms
    ON threads(archived, recency_at_ms DESC, id DESC)
    WHERE preview <> '';
CREATE INDEX idx_threads_pinned_recency_at_ms
    ON threads(archived, recency_at_ms DESC, id DESC)
    WHERE is_pinned = 1 AND preview <> '';
CREATE INDEX idx_threads_project_id
    ON threads(project_id, archived, created_at_ms DESC, id DESC)
    WHERE project_id IS NOT NULL;
CREATE INDEX idx_threads_section_recency_at_ms
    ON threads(archived, thread_section_id, recency_at_ms DESC, id DESC)
    WHERE thread_section_id IS NOT NULL;
CREATE INDEX idx_threads_section_position
    ON threads(archived, thread_section_id, section_position ASC, id ASC)
    WHERE thread_section_id IS NOT NULL;
CREATE INDEX idx_threads_project_recency
    ON threads(project_id, recency_at_ms DESC)
    WHERE archived = 0 AND project_id IS NOT NULL;
