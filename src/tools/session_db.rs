use anyhow::{Result, Context};
use rusqlite::Connection;
use std::path::PathBuf;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn get_db_path() -> Result<PathBuf> {
    let mut path = if cfg!(test) {
        std::env::temp_dir().join("line-editor-test")
    } else {
        let mut p = dirs::home_dir().context("Failed to get home directory")?;
        p.push(".cache");
        p.push("line-editor");
        p
    };
    fs::create_dir_all(&path).context("Failed to create cache directory")?;
    path.push("sessions.db");
    Ok(path)
}

pub fn get_db_connection() -> Result<Connection> {
    let db_path = get_db_path()?;
    let conn = Connection::open(db_path).context("Failed to open SQLite database")?;
    
    // Set busy timeout to 5 seconds to prevent SQLITE_BUSY errors
    conn.busy_timeout(std::time::Duration::from_millis(5000))
        .context("Failed to set SQLite busy timeout")?;
    
    // Configure high-performance memory pragmas and enable foreign keys
    conn.pragma_update(None, "journal_mode", &"WAL")
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

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone)]
pub struct SessionMetadata {
    pub session_id: String,
    pub total_lines: usize,
    pub file_hash: String,
    pub mtime: i64,
    pub is_supported: bool,
    pub warning_message: Option<String>,
}

fn is_binary_file(path: &str) -> Result<bool> {
    use std::io::Read;
    let mut file = fs::File::open(path).with_context(|| format!("Failed to open file for binary check: {}", path))?;
    let mut buffer = [0; 8192];
    let bytes_read = file.read(&mut buffer).with_context(|| format!("Failed to read file for binary check: {}", path))?;
    Ok(buffer[..bytes_read].contains(&0))
}

fn compute_sha256(path: &str) -> Result<String> {
    use sha2::{Sha256, Digest};
    use std::io::Read;
    let mut file = fs::File::open(path).with_context(|| format!("Failed to open file for SHA256 hashing: {}", path))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 8192];
    loop {
        let n = file.read(&mut buffer).with_context(|| format!("Failed to read file for SHA256 hashing: {}", path))?;
        if n == 0 { break; }
        hasher.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn check_language_supported(path: &str) -> bool {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "py" | "js" | "jsx" | "ts" | "tsx" | "go" | "rs" | "java" |
        "cpp" | "cc" | "cxx" | "c" | "h" | "lua" | "html" | "htm" |
        "json" | "yaml" | "yml" | "toml" | "swift"
    )
}

pub fn cleanup_stale_sessions(conn: &Connection) -> Result<()> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
    let ttl_limit = now - 1800; // 30 minutes
    conn.execute("DELETE FROM sessions WHERE last_accessed_at < ?1;", [ttl_limit])
        .context("Failed to clean up stale sessions")?;
    Ok(())
}

pub fn init_edit_session(filepath: &str, create_if_not_exists: bool) -> Result<SessionMetadata> {
    let mut conn = get_db_connection()?;
    create_tables(&conn)?;
    cleanup_stale_sessions(&conn)?;

    let path = std::path::Path::new(filepath);
    if !path.exists() {
        if create_if_not_exists {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).context("Failed to create parent directories")?;
            }
            fs::write(filepath, "").context("Failed to create empty file")?;
        } else {
            anyhow::bail!("File not found: {}", filepath);
        }
    }

    if is_binary_file(filepath)? {
        anyhow::bail!("BINARY_FILE_ERROR: Binary files are not supported for line editing.");
    }

    let file_hash = compute_sha256(filepath)?;
    let mtime = path.metadata()?.modified()?
        .duration_since(UNIX_EPOCH)?.as_secs() as i64;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;

    // Check if session already exists and file mtime/hash matches
    let existing = {
        let mut stmt = conn.prepare(
            "SELECT session_id FROM sessions WHERE filepath = ?1 AND file_hash = ?2 AND mtime = ?3"
        )?;
        match stmt.query_row(rusqlite::params![filepath, file_hash, mtime], |row| {
            row.get::<_, String>(0)
        }) {
            Ok(session_id) => Some(session_id),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(err) => return Err(anyhow::Error::from(err).context("Failed to query existing session")),
        }
    };

    let is_supported = check_language_supported(filepath);
    let warning_message = if !is_supported {
        Some("Unsupported file format. Syntax validation is disabled.".to_string())
    } else {
        None
    };

    if let Some(session_id) = existing {
        // Reuse session
        conn.execute("UPDATE sessions SET last_accessed_at = ?1 WHERE session_id = ?2;", rusqlite::params![now, session_id])
            .context("Failed to update last_accessed_at for reused session")?;
        let total_lines: usize = conn.query_row(
            "SELECT COUNT(*) FROM lines WHERE session_id = ?1",
            rusqlite::params![session_id],
            |row| row.get(0)
        )?;
        return Ok(SessionMetadata {
            session_id,
            total_lines,
            file_hash,
            mtime,
            is_supported,
            warning_message,
        });
    }

    // Create a new session
    use sha1::{Sha1, Digest};
    let session_id = format!("{:x}", Sha1::digest(format!("{}-{}-{}", filepath, file_hash, now).as_bytes()));
    
    // Read the file and populate rows
    let content = fs::read_to_string(filepath).context("Failed to read file content")?;
    
    // Begin transaction
    let tx = conn.transaction().context("Failed to begin SQLite transaction")?;

    // Clear any stale session for this filepath
    tx.execute("DELETE FROM sessions WHERE filepath = ?1;", rusqlite::params![filepath])
        .context("Failed to delete existing session for path")?;

    tx.execute(
        "INSERT INTO sessions (filepath, session_id, file_hash, mtime, last_accessed_at) VALUES (?1, ?2, ?3, ?4, ?5);",
        rusqlite::params![filepath, session_id, file_hash, mtime, now],
    ).context("Failed to insert new session")?;

    let mut lines_count = 0;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO lines (session_id, sequence_id, line_hash, content, sort_order) VALUES (?1, ?2, NULL, ?3, ?4);"
        )?;

        // Split by lines, preserving empty final lines
        let mut parts: Vec<&str> = content.split('\n').collect();
        if parts.last() == Some(&"") {
            parts.pop();
        }
        
        for (idx, line_content) in parts.iter().enumerate() {
            let seq = idx + 1;
            let sort_order = (seq as f64) * 1000.0;
            stmt.execute(rusqlite::params![session_id, seq, line_content, sort_order])
                .context("Failed to insert line")?;
            lines_count += 1;
        }
    }

    tx.commit().context("Failed to commit SQLite transaction")?;

    start_background_hash_worker(session_id.clone());

    Ok(SessionMetadata {
        session_id,
        total_lines: lines_count,
        file_hash,
        mtime,
        is_supported,
        warning_message,
    })
}

pub fn compute_line_hash(content: &str) -> String {
    use sha1::{Sha1, Digest};
    let mut hasher = Sha1::new();
    hasher.update(content.as_bytes());
    let hex = format!("{:x}", hasher.finalize());
    hex[..4].to_string()
}

pub fn ensure_hashes_for_range(conn: &Connection, session_id: &str, start_line: usize, end_line: usize) -> Result<()> {
    let limit = if end_line >= start_line { end_line - start_line + 1 } else { 0 };
    let offset = start_line.saturating_sub(1);

    let mut stmt = conn.prepare(
        "SELECT id, content, line_hash FROM lines WHERE session_id = ?1 ORDER BY sort_order LIMIT ?2 OFFSET ?3"
    )?;
    
    let mut rows = stmt.query(rusqlite::params![session_id, limit, offset])?;
    let mut updates = Vec::new();
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let content: String = row.get(1)?;
        let line_hash: Option<String> = row.get(2)?;
        if line_hash.is_none() {
            let hash = compute_line_hash(&content);
            updates.push((id, hash));
        }
    }

    if !updates.is_empty() {
        let mut update_stmt = conn.prepare("UPDATE lines SET line_hash = ?1 WHERE id = ?2;")?;
        for (id, hash) in updates {
            update_stmt.execute(rusqlite::params![hash, id])?;
        }
    }
    
    Ok(())
}

pub fn start_background_hash_worker(session_id: String) {
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            let mut conn = match get_db_connection() {
                Ok(c) => c,
                Err(_) => return,
            };
            let tx = match conn.transaction() {
                Ok(t) => t,
                Err(_) => return,
            };
            
            let mut updates = Vec::new();
            {
                let mut stmt = match tx.prepare("SELECT id, content FROM lines WHERE session_id = ?1 AND line_hash IS NULL") {
                    Ok(s) => s,
                    Err(_) => return,
                };
                let query_res = stmt.query([&session_id]);
                if let Ok(mut rows) = query_res {
                    while let Ok(Some(row)) = rows.next() {
                        if let (Ok(id), Ok(content)) = (row.get::<_, i64>(0), row.get::<_, String>(1)) {
                            let hash = compute_line_hash(&content);
                            updates.push((id, hash));
                        }
                    }
                }
            }
            
            if !updates.is_empty() {
                let mut update_stmt = match tx.prepare("UPDATE lines SET line_hash = ?1 WHERE id = ?2;") {
                    Ok(s) => s,
                    Err(_) => return,
                };
                for (id, hash) in updates {
                    if update_stmt.execute(rusqlite::params![hash, id]).is_err() {
                        return;
                    }
                }
            }
            
            let _ = tx.commit();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::TEST_DB_LOCK as DB_LOCK;

    #[test]
    fn test_create_tables_in_memory() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
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
        let _lock = DB_LOCK.lock().unwrap();
        let path = get_db_path()?;
        assert!(path.to_string_lossy().contains("sessions.db"));
        
        let temp_dir = std::env::temp_dir().join("line-editor-test");
        assert!(path.starts_with(&temp_dir));

        if path.exists() {
            fs::remove_file(&path).context("Failed to clean up test DB file")?;
        }
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir).context("Failed to clean up test directory")?;
        }

        Ok(())
    }

    #[test]
    fn test_is_binary_file() -> Result<()> {
        let temp_dir = std::env::temp_dir().join("line-editor-test-binary");
        fs::create_dir_all(&temp_dir)?;
        
        let text_path = temp_dir.join("text.txt");
        fs::write(&text_path, "Hello world!")?;
        assert!(!is_binary_file(text_path.to_str().unwrap())?);
        
        let binary_path = temp_dir.join("binary.bin");
        fs::write(&binary_path, b"Hello\x00world")?;
        assert!(is_binary_file(binary_path.to_str().unwrap())?);
        
        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }

    #[test]
    fn test_check_language_supported() {
        assert!(check_language_supported("foo.rs"));
        assert!(check_language_supported("bar.py"));
        assert!(check_language_supported("baz.ts"));
        assert!(check_language_supported("main.cpp"));
        assert!(check_language_supported("index.html"));
        assert!(check_language_supported("config.toml"));
        assert!(check_language_supported("FOO.RS"));
        assert!(check_language_supported("Bar.Py"));
        assert!(!check_language_supported("foo.txt"));
        assert!(!check_language_supported("foo.pdf"));
        assert!(!check_language_supported("foo"));
    }

    #[test]
    fn test_compute_sha256() -> Result<()> {
        let temp_dir = std::env::temp_dir().join("line-editor-test-sha");
        fs::create_dir_all(&temp_dir)?;
        let file_path = temp_dir.join("test.txt");
        fs::write(&file_path, "hello")?;
        
        let hash = compute_sha256(file_path.to_str().unwrap())?;
        assert_eq!(hash, "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824");
        
        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }

    #[test]
    fn test_cleanup_stale_sessions() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let conn = Connection::open_in_memory()?;
        create_tables(&conn)?;
        
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
        let stale_time = now - 2000;
        let fresh_time = now - 500;
        
        conn.execute(
            "INSERT INTO sessions (filepath, session_id, file_hash, mtime, last_accessed_at) VALUES (?1, ?2, ?3, ?4, ?5);",
            rusqlite::params!["stale.rs", "session_stale", "hash1", 100, stale_time],
        )?;
        
        conn.execute(
            "INSERT INTO sessions (filepath, session_id, file_hash, mtime, last_accessed_at) VALUES (?1, ?2, ?3, ?4, ?5);",
            rusqlite::params!["fresh.rs", "session_fresh", "hash2", 100, fresh_time],
        )?;
        
        cleanup_stale_sessions(&conn)?;
        
        let mut stmt = conn.prepare("SELECT count(*) FROM sessions")?;
        let count: i64 = stmt.query_row([], |row| row.get(0))?;
        assert_eq!(count, 1);
        
        let mut stmt = conn.prepare("SELECT filepath FROM sessions")?;
        let filepath: String = stmt.query_row([], |row| row.get(0))?;
        assert_eq!(filepath, "fresh.rs");
        
        Ok(())
    }

    #[test]
    fn test_init_edit_session_nonexistent_file() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let temp_dir = std::env::temp_dir().join("line-editor-test-init-nonexistent");
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir)?;
        }
        let file_path = temp_dir.join("missing.rs");
        let filepath_str = file_path.to_str().unwrap();
        
        let res = init_edit_session(filepath_str, false);
        assert!(res.is_err());
        
        let metadata = init_edit_session(filepath_str, true)?;
        assert_eq!(metadata.total_lines, 0);
        assert!(metadata.is_supported);
        assert!(file_path.exists());
        
        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }

    #[test]
    fn test_init_edit_session_binary_file() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let temp_dir = std::env::temp_dir().join("line-editor-test-init-binary");
        fs::create_dir_all(&temp_dir)?;
        let file_path = temp_dir.join("binary.bin");
        fs::write(&file_path, b"Hello\x00world")?;
        
        let res = init_edit_session(file_path.to_str().unwrap(), false);
        assert!(res.is_err());
        let err_msg = format!("{:?}", res.err().unwrap());
        assert!(err_msg.contains("BINARY_FILE_ERROR"));
        
        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }

    #[test]
    fn test_init_edit_session_unsupported_language() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let temp_dir = std::env::temp_dir().join("line-editor-test-init-unsupported");
        fs::create_dir_all(&temp_dir)?;
        let file_path = temp_dir.join("unsupported.txt");
        fs::write(&file_path, "Hello\nworld")?;
        
        let metadata = init_edit_session(file_path.to_str().unwrap(), false)?;
        assert_eq!(metadata.total_lines, 2);
        assert!(!metadata.is_supported);
        assert!(metadata.warning_message.is_some());
        
        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }

    #[test]
    fn test_init_edit_session_lifecycle() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let temp_dir = std::env::temp_dir().join("line-editor-test-init-lifecycle");
        fs::create_dir_all(&temp_dir)?;
        let file_path = temp_dir.join("code.rs");
        fs::write(&file_path, "fn main() {\n    println!(\"Hello!\");\n}\n")?;
        let filepath_str = file_path.to_str().unwrap();
        
        let metadata1 = init_edit_session(filepath_str, false)?;
        assert_eq!(metadata1.total_lines, 3);
        assert!(metadata1.is_supported);
        assert!(metadata1.warning_message.is_none());
        
        let metadata2 = init_edit_session(filepath_str, false)?;
        assert_eq!(metadata1.session_id, metadata2.session_id);
        assert_eq!(metadata1.file_hash, metadata2.file_hash);
        assert_eq!(metadata1.mtime, metadata2.mtime);
        
        fs::write(&file_path, "fn main() {\n    println!(\"Hello!\");\n    // extra line\n}\n")?;
        let metadata3 = init_edit_session(filepath_str, false)?;
        assert_ne!(metadata1.session_id, metadata3.session_id);
        assert_ne!(metadata1.file_hash, metadata3.file_hash);
        assert_eq!(metadata3.total_lines, 4);
        
        let conn = get_db_connection()?;
        let mut stmt = conn.prepare("SELECT content FROM lines WHERE session_id = ?1 ORDER BY sort_order")?;
        let lines: Vec<String> = stmt.query_map([&metadata3.session_id], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        assert_eq!(lines, vec![
            "fn main() {".to_string(),
            "    println!(\"Hello!\");".to_string(),
            "    // extra line".to_string(),
            "}".to_string(),
        ]);
        
        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }

    #[test]
    fn test_line_hashing_and_lazy_populating() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let conn = Connection::open_in_memory()?;
        create_tables(&conn)?;

        // Test compute_line_hash
        let hash = compute_line_hash("hello world");
        assert_eq!(hash.len(), 4);
        assert_eq!(hash, "2aae"); // sha1 prefix
        
        let hash2 = compute_line_hash("hello world");
        assert_eq!(hash, hash2);

        // Setup a dummy session
        let session_id = "test_session_id";
        conn.execute(
            "INSERT INTO sessions (filepath, session_id, file_hash, mtime, last_accessed_at) VALUES (?1, ?2, ?3, ?4, ?5);",
            rusqlite::params!["dummy.rs", session_id, "hash", 100, 100],
        )?;

        // Insert three lines with line_hash = NULL
        conn.execute(
            "INSERT INTO lines (session_id, sequence_id, line_hash, content, sort_order) VALUES (?1, ?2, NULL, ?3, ?4);",
            rusqlite::params![session_id, 1, "line 1 content", 1000.0],
        )?;
        conn.execute(
            "INSERT INTO lines (session_id, sequence_id, line_hash, content, sort_order) VALUES (?1, ?2, NULL, ?3, ?4);",
            rusqlite::params![session_id, 2, "line 2 content", 2000.0],
        )?;
        conn.execute(
            "INSERT INTO lines (session_id, sequence_id, line_hash, content, sort_order) VALUES (?1, ?2, NULL, ?3, ?4);",
            rusqlite::params![session_id, 3, "line 3 content", 3000.0],
        )?;

        // Ensure hashes only for lines 1 and 2
        ensure_hashes_for_range(&conn, session_id, 1, 2)?;

        // Retrieve lines and check hashes
        let mut stmt = conn.prepare("SELECT sequence_id, line_hash FROM lines WHERE session_id = ?1 ORDER BY sort_order")?;
        let results: Vec<(i64, Option<String>)> = stmt.query_map([session_id], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?.collect::<Result<_, _>>()?;

        assert_eq!(results[0].0, 1);
        assert!(results[0].1.is_some());
        assert_eq!(results[0].1.as_ref().unwrap(), &compute_line_hash("line 1 content"));

        assert_eq!(results[1].0, 2);
        assert!(results[1].1.is_some());
        assert_eq!(results[1].1.as_ref().unwrap(), &compute_line_hash("line 2 content"));

        assert_eq!(results[2].0, 3);
        assert!(results[2].1.is_none());

        Ok(())
    }

    #[tokio::test]
    async fn test_background_hash_worker() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let temp_dir = std::env::temp_dir().join("line-editor-test-bg-worker");
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir)?;
        }
        fs::create_dir_all(&temp_dir)?;
        let file_path = temp_dir.join("bg_code.rs");
        fs::write(&file_path, "line 1\nline 2\n")?;

        let metadata = init_edit_session(file_path.to_str().unwrap(), false)?;
        
        // Wait for the background worker to finish hashing
        tokio::time::sleep(tokio::time::Duration::from_millis(800)).await;

        let conn = get_db_connection()?;
        let mut stmt = conn.prepare("SELECT line_hash FROM lines WHERE session_id = ?1 ORDER BY sort_order")?;
        let hashes: Vec<Option<String>> = stmt.query_map([&metadata.session_id], |row| row.get(0))?
            .collect::<Result<_, _>>()?;

        assert_eq!(hashes.len(), 2);
        assert!(hashes[0].is_some());
        assert!(hashes[1].is_some());
        assert_eq!(hashes[0].as_ref().unwrap(), &compute_line_hash("line 1"));
        assert_eq!(hashes[1].as_ref().unwrap(), &compute_line_hash("line 2"));

        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn test_view_session_lines() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let temp_dir = std::env::temp_dir().join("line-editor-test-view");
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir)?;
        }
        fs::create_dir_all(&temp_dir)?;
        let file_path = temp_dir.join("code.rs");
        fs::write(&file_path, "fn main() {\n    println!(\"Hello!\");\n}\n")?;
        let filepath_str = file_path.to_str().unwrap();

        // 1. Initialize session
        let metadata = init_edit_session(filepath_str, false)?;
        assert_eq!(metadata.total_lines, 3);

        // 2. View all lines (1 to 3)
        let output = crate::tools::view::view_session_lines(filepath_str, 1, 3)?;
        
        // Verify structure
        assert!(output.contains("LINE | LINE ID | CODE"));
        assert!(output.contains("1 | 1#"));
        assert!(output.contains("2 | 2#"));
        assert!(output.contains("3 | 3#"));
        assert!(output.contains("fn main() {"));
        assert!(output.contains("println!(\"Hello!\");"));
        assert!(output.contains("}"));

        // 3. View sub-range (2 to 2)
        let sub_output = crate::tools::view::view_session_lines(filepath_str, 2, 2)?;
        assert!(sub_output.contains("2 | 2#"));
        assert!(!sub_output.contains("1 | 1#"));
        assert!(!sub_output.contains("3 | 3#"));

        // 4. Test bounds validation
        assert!(crate::tools::view::view_session_lines(filepath_str, 0, 3).is_err());
        assert!(crate::tools::view::view_session_lines(filepath_str, 3, 1).is_err());

        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }
}
