use anyhow::{Result, Context};
use rusqlite::Connection;
use std::path::PathBuf;
use std::fs;

pub fn get_db_path() -> Result<PathBuf> {
    let mut path = if let Ok(test_dir) = std::env::var("TEST_DB_DIR") {
        PathBuf::from(test_dir)
    } else {
        dirs::home_dir().context("Failed to get home directory")?
    };
    path.push(".cache");
    path.push("line-editor");
    fs::create_dir_all(&path).context("Failed to create cache directory")?;
    path.push("sessions.db");
    Ok(path)
}

pub fn get_db_connection() -> Result<Connection> {
    let db_path = get_db_path()?;
    let conn = Connection::open(db_path).context("Failed to open SQLite database")?;
    
    // Configure high-performance memory pragmas and enable foreign keys
    conn.execute("PRAGMA journal_mode = WAL;", [])
        .context("Failed to configure WAL journal mode")?;
    conn.execute("PRAGMA synchronous = NORMAL;", [])
        .context("Failed to configure synchronous NORMAL")?;
    conn.execute("PRAGMA foreign_keys = ON;", [])
        .context("Failed to enable foreign keys")?;
    conn.execute("PRAGMA temp_store = MEMORY;", [])
        .context("Failed to configure temp_store MEMORY")?;
    
    Ok(conn)
}

pub fn create_tables(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS sessions (
            filepath TEXT PRIMARY KEY,
            session_id TEXT UNIQUE NOT NULL,
            file_hash TEXT NOT NULL,
            mtime INTEGER NOT NULL,
            last_accessed_at INTEGER NOT NULL
        );",
        [],
    ).context("Failed to create sessions table")?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS lines (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            sequence_id INTEGER NOT NULL,
            line_hash TEXT,
            content TEXT NOT NULL,
            sort_order REAL NOT NULL,
            FOREIGN KEY(session_id) REFERENCES sessions(session_id) ON DELETE CASCADE
        );",
        [],
    ).context("Failed to create lines table")?;
    
    // Add indices for fast lookups
    conn.execute("CREATE INDEX IF NOT EXISTS idx_lines_session_id ON lines(session_id);", [])
        .context("Failed to create index idx_lines_session_id")?;
    conn.execute("CREATE INDEX IF NOT EXISTS idx_lines_sort_order ON lines(sort_order);", [])
        .context("Failed to create index idx_lines_sort_order")?;
    
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_tables_in_memory() -> Result<()> {
        let conn = Connection::open_in_memory()?;
        
        // Configure pragmas
        conn.execute("PRAGMA foreign_keys = ON;", [])
            .context("Failed to enable foreign keys in test database")?;
        
        // Create tables
        create_tables(&conn)?;

        // Verify that tables exist by query
        let mut stmt = conn.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='sessions';")?;
        let mut rows = stmt.query([])?;
        assert!(rows.next()?.is_some(), "sessions table should exist");

        let mut stmt = conn.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='lines';")?;
        let mut rows = stmt.query([])?;
        assert!(rows.next()?.is_some(), "lines table should exist");

        Ok(())
    }

    #[test]
    fn test_get_db_path() -> Result<()> {
        let temp_dir = std::env::temp_dir().join("line-editor-test");
        std::env::set_var("TEST_DB_DIR", &temp_dir);

        let res = get_db_path();

        std::env::remove_var("TEST_DB_DIR");

        let path = res?;
        assert!(path.to_string_lossy().contains("sessions.db"));
        assert!(path.starts_with(&temp_dir));

        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir).context("Failed to clean up test directory")?;
        }

        Ok(())
    }
}
