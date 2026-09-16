//! Where the store lives, how it is opened, the shape it holds, and how it
//! lets go of what nothing has touched.

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn get_db_path() -> Result<PathBuf> {
    let mut path = if let Some(dir) = env::var_os("AST_EDITOR_CACHE_DIR") {
        PathBuf::from(dir)
    } else if cfg!(test) {
        // Only reaches unit tests: the tests/ crate links this library built
        // without the flag, so integration tests set the variable above.
        crate::tools::test_temp_dir("line-editor-test")
    } else {
        dirs::cache_dir()
            .context("Failed to get cache directory")?
            .join("ast-editor")
    };
    fs::create_dir_all(&path).context("Failed to create cache directory")?;
    path.push("sessions.db");
    Ok(path)
}

pub(crate) fn get_db_connection() -> Result<Connection> {
    let db_path = get_db_path()?;
    let conn = Connection::open(db_path).context("Failed to open SQLite database")?;

    // Set busy timeout to 5 seconds to prevent SQLITE_BUSY errors
    conn.busy_timeout(std::time::Duration::from_millis(5000))
        .context("Failed to set SQLite busy timeout")?;

    // Configure high-performance memory pragmas and enable foreign keys
    conn.pragma_update(None, "journal_mode", "WAL")
        .context("Failed to configure WAL journal mode")?;
    conn.execute("PRAGMA synchronous = NORMAL;", [])
        .context("Failed to configure synchronous NORMAL")?;
    conn.execute("PRAGMA foreign_keys = ON;", [])
        .context("Failed to enable foreign keys")?;
    conn.execute("PRAGMA temp_store = MEMORY;", [])
        .context("Failed to configure temp_store MEMORY")?;

    // Every call makes what it needs: a caller whose first act is an edit opens
    // the store the same way a caller who reads first does, rather than finding
    // tables somebody else was expected to have created.
    let has_schema = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'sessions'",
            [],
            |_| Ok(()),
        )
        .optional()
        .context("Failed to look for the store's schema")?
        .is_some();
    if !has_schema {
        create_tables(&conn)?;
    }

    Ok(conn)
}

pub(crate) fn create_tables(conn: &Connection) -> Result<()> {
    let auto_vacuum: i64 = conn.pragma_query_value(None, "auto_vacuum", |row| row.get(0))?;
    if auto_vacuum != 2 {
        conn.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;
        // The mode change only takes effect once the file is rewritten.
        conn.execute_batch("VACUUM;")
            .context("Failed to switch the store to incremental vacuum")?;
    }

    conn.execute(
        "CREATE TABLE IF NOT EXISTS sessions (
            filepath TEXT PRIMARY KEY,
            session_id TEXT UNIQUE NOT NULL,
            file_hash TEXT NOT NULL,
            mtime INTEGER NOT NULL,
            last_accessed_at INTEGER NOT NULL,
            next_line_id INTEGER NOT NULL DEFAULT 1,
            line_ending_crlf INTEGER DEFAULT 0
        );",
        [],
    )
    .context("Failed to create sessions table")?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS lines (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            sequence_id INTEGER NOT NULL,
            line_hash TEXT,
            norm_hash TEXT,
            -- The line's position. Every write replaces a session's rows, so
            -- this is recomputed from the position rather than kept between
            -- them, and nothing reads it but ORDER BY.
            sort_order REAL NOT NULL,
            parent_context TEXT,
            FOREIGN KEY(session_id) REFERENCES sessions(session_id) ON DELETE CASCADE
        );",
        [],
    )
    .context("Failed to create lines table")?;

    // Add indices for fast lookups
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_lines_session_id ON lines(session_id);",
        [],
    )
    .context("Failed to create index idx_lines_session_id")?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_lines_sort_order ON lines(sort_order);",
        [],
    )
    .context("Failed to create index idx_lines_sort_order")?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS previews (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            filepath TEXT NOT NULL,
            file_hash TEXT NOT NULL,
            edits_json TEXT NOT NULL,
            created_at INTEGER NOT NULL
        );",
        [],
    )
    .context("Failed to create previews table")?;

    Ok(())
}

/// Previews older than this are pruned. A preview is only useful while the file
/// it was computed against is unchanged, so this is a backstop, not a lifetime.
pub(crate) const PREVIEW_TTL_SECONDS: i64 = 3600;

/// How long a file's entry outlives its last use. Evicting one costs nothing
/// but the stability of that file's ids, so the window only has to outlast the
/// task an agent is in the middle of.
const SESSION_TTL_SECONDS: i64 = 7 * 24 * 60 * 60;

pub(crate) fn cleanup_stale_sessions(conn: &Connection) -> Result<()> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
    conn.execute(
        "DELETE FROM sessions WHERE last_accessed_at < ?1;",
        [now - SESSION_TTL_SECONDS],
    )
    .context("Failed to clean up stale sessions")?;
    conn.execute(
        "DELETE FROM previews WHERE created_at < ?1;",
        [now - PREVIEW_TTL_SECONDS],
    )
    .context("Failed to clean up expired previews")?;
    // Deleting rows only moves pages to the free list; this hands them back to
    // the filesystem.
    conn.pragma_query_value(None, "incremental_vacuum", |_| Ok(()))
        .ok();
    Ok(())
}
