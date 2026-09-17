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
    // The one place the old word stays. Renaming the file would abandon every
    // store already on disk rather than carry it over, which is the opposite of
    // what the rename inside it is for.
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

    // A store written before the tables were named for what they hold. The
    // entries are a file's identity and the line ids keyed to it, so moving
    // them is what keeps those ids: created afresh instead, every id a caller
    // is holding would be refused on the next call.
    migrate_sessions_to_files(&conn)?;

    // Every call makes what it needs: a caller whose first act is an edit opens
    // the store the same way a caller who reads first does, rather than finding
    // tables somebody else was expected to have created.
    let has_schema = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'files'",
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

/// Carry a store written under the old names over to the new ones. A file's
/// entry was a `session` and its key a `session_id`, which described something
/// opened and closed; nothing here is. The rows are the same rows, so they are
/// renamed rather than rebuilt — the line ids a caller is holding are in them.
///
/// Looks before it locks. Every call opens the store, and taking the write lock
/// each time would serialise calls that have nothing to write against each
/// other; the read says no on every open after the first. It is safe to delete
/// once no store predates it.
fn migrate_sessions_to_files(conn: &Connection) -> Result<()> {
    if !has_old_table(conn)? {
        return Ok(());
    }

    // Two calls can find the old shape at the same moment, so the rename looks
    // again under the write lock: whichever arrives second finds the work done
    // and the table gone.
    conn.execute_batch("BEGIN IMMEDIATE;")
        .context("Failed to take the store's write lock")?;
    let renamed = (|| -> Result<()> {
        if !has_old_table(conn)? {
            return Ok(());
        }
        // `lines` keeps its name; only the key it joins on moves.
        conn.execute_batch(
            "ALTER TABLE sessions RENAME TO files;
             ALTER TABLE files RENAME COLUMN session_id TO file_key;
             ALTER TABLE lines RENAME COLUMN session_id TO file_key;",
        )
        .context("Failed to carry the store over to the names it uses now")
    })();
    match renamed {
        Ok(()) => conn
            .execute_batch("COMMIT;")
            .context("Failed to commit the store's rename"),
        Err(err) => {
            let _ = conn.execute_batch("ROLLBACK;");
            Err(err)
        }
    }
}

/// Whether the store still carries the table the entries used to live in.
fn has_old_table(conn: &Connection) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'sessions'",
            [],
            |_| Ok(()),
        )
        .optional()
        .context("Failed to look for the store's previous schema")?
        .is_some())
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
        "CREATE TABLE IF NOT EXISTS files (
            filepath TEXT PRIMARY KEY,
            file_key TEXT UNIQUE NOT NULL,
            file_hash TEXT NOT NULL,
            mtime INTEGER NOT NULL,
            last_accessed_at INTEGER NOT NULL,
            next_line_id INTEGER NOT NULL DEFAULT 1,
            line_ending_crlf INTEGER DEFAULT 0
        );",
        [],
    )
    .context("Failed to create files table")?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS lines (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            file_key TEXT NOT NULL,
            sequence_id INTEGER NOT NULL,
            line_hash TEXT,
            norm_hash TEXT,
            -- The line's position. Every write replaces a file's rows, so
            -- this is recomputed from the position rather than kept between
            -- them, and nothing reads it but ORDER BY.
            sort_order REAL NOT NULL,
            parent_context TEXT,
            FOREIGN KEY(file_key) REFERENCES files(file_key) ON DELETE CASCADE
        );",
        [],
    )
    .context("Failed to create lines table")?;

    // Add indices for fast lookups
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_lines_session_id ON lines(file_key);",
        [],
    )
    .context("Failed to create index idx_lines_session_id")?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_lines_sort_order ON lines(sort_order);",
        [],
    )
    .context("Failed to create index idx_lines_sort_order")?;

    // An id is `sequence_id#hash`, so two lines of one file sharing a sequence
    // number share an id, and an edit addressed to it takes whichever the
    // store returns first. The constraint says so where it cannot be forgotten
    // (D-01M27KBH6NNXZJ): a path that issues one twice fails at the insert
    // rather than leaving the file wrong until someone reads the listing.
    //
    // A store written before this may already hold such a pair, and the index
    // cannot be built over it. Those files lose their rows and are re-indexed
    // on the next read, which is what losing the store costs anyway — it holds
    // no copy of any file (D-01M27K7HHEWS57).
    conn.execute(
        "DELETE FROM lines WHERE file_key IN (
            SELECT file_key FROM lines
            GROUP BY file_key, sequence_id
            HAVING COUNT(*) > 1
        );",
        [],
    )
    .context("Failed to clear files whose lines share a sequence number")?;
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_lines_file_sequence
            ON lines(file_key, sequence_id);",
        [],
    )
    .context("Failed to create index idx_lines_file_sequence")?;

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
        "DELETE FROM files WHERE last_accessed_at < ?1;",
        [now - SESSION_TTL_SECONDS],
    )
    .context("Failed to clean up stale files")?;
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
