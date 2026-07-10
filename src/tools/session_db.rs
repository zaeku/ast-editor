use anyhow::{Result, Context};
use rusqlite::Connection;
use serde::{Serialize, Deserialize};
use std::path::PathBuf;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

fn get_db_path() -> Result<PathBuf> {
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

fn get_db_connection() -> Result<Connection> {
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
    
    Ok(conn)
}

fn create_tables(conn: &Connection) -> Result<()> {
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

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct LineEdit {
    pub op: String,
    pub target_id: Option<String>,
    pub end_target_id: Option<String>,
    pub dest_target_id: Option<String>,
    pub move_position: Option<String>,
    pub content: Option<String>,
}

pub fn parse_line_id(id_str: &str) -> Result<(i64, String)> {
    let parts: Vec<&str> = id_str.split('#').collect();
    if parts.len() != 2 {
        anyhow::bail!("Invalid Line ID format: {}", id_str);
    }
    let seq = i64::from_str_radix(parts[0], 16)
        .context("Failed to parse sequence ID hex")?;
    Ok((seq, parts[1].to_string()))
}

pub trait SessionRepository: Send + Sync {
    fn init_session(&self, filepath: &str, is_binary: bool) -> Result<SessionMetadata>;
    fn get_total_lines(&self, session_id: &str) -> Result<usize>;
    fn ensure_hashes_range(&self, session_id: &str, start_line: usize, end_line: usize) -> Result<()>;
    fn fetch_lines_range(
        &self,
        session_id: &str,
        start_line: usize,
        end_line: usize,
    ) -> Result<Vec<(i64, Option<String>, String)>>;
    fn get_line_content(&self, session_id: &str, sequence_id: i64) -> Result<Option<String>>;
    fn apply_line_edits(
        &self,
        filepath: &str,
        session_id: &str,
        edits: &[LineEdit],
    ) -> Result<Vec<String>>; // returns modified IDs
    fn restore_session_lines(
        &self,
        session_id: &str,
        lines: &[(i64, Option<String>, String)],
    ) -> Result<()>;
    fn update_session_metadata(
        &self,
        session_id: &str,
        file_hash: &str,
        mtime: i64,
    ) -> Result<()>;
    fn delete_session(&self, filepath: &str) -> Result<()>;
    fn get_session_mtime(&self, filepath: &str) -> Result<Option<i64>>;
}

pub struct SqliteSessionRepository;

impl SessionRepository for SqliteSessionRepository {
    fn init_session(&self, filepath: &str, is_binary: bool) -> Result<SessionMetadata> {
        init_edit_session(filepath, is_binary)
    }

    fn get_total_lines(&self, session_id: &str) -> Result<usize> {
        let conn = get_db_connection()?;
        let total_lines: usize = conn.query_row(
            "SELECT COUNT(*) FROM lines WHERE session_id = ?1",
            rusqlite::params![session_id],
            |row| row.get(0)
        )?;
        Ok(total_lines)
    }

    fn ensure_hashes_range(&self, session_id: &str, start_line: usize, end_line: usize) -> Result<()> {
        let conn = get_db_connection()?;
        ensure_hashes_for_range(&conn, session_id, start_line, end_line)
    }

    fn fetch_lines_range(
        &self,
        session_id: &str,
        start_line: usize,
        end_line: usize,
    ) -> Result<Vec<(i64, Option<String>, String)>> {
        let conn = get_db_connection()?;
        let limit = if end_line >= start_line { end_line - start_line + 1 } else { 0 };
        let offset = start_line.saturating_sub(1);

        let mut stmt = conn.prepare(
            "SELECT sequence_id, line_hash, content FROM lines WHERE session_id = ?1 ORDER BY sort_order LIMIT ?2 OFFSET ?3"
        )?;

        let mut rows = stmt.query(rusqlite::params![session_id, limit, offset])?;
        let mut results = Vec::new();
        while let Some(row) = rows.next()? {
            results.push((row.get(0)?, row.get(1)?, row.get(2)?));
        }
        Ok(results)
    }

    fn get_line_content(&self, session_id: &str, sequence_id: i64) -> Result<Option<String>> {
        let conn = get_db_connection()?;
        let mut stmt = conn.prepare(
            "SELECT content FROM lines WHERE session_id = ?1 AND sequence_id = ?2"
        )?;
        match stmt.query_row(rusqlite::params![session_id, sequence_id], |row| row.get::<_, String>(0)) {
            Ok(content) => Ok(Some(content)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(anyhow::Error::from(err)),
        }
    }

    fn apply_line_edits(
        &self,
        _filepath: &str,
        session_id: &str,
        edits: &[LineEdit],
    ) -> Result<Vec<String>> {
        let mut conn = get_db_connection()?;
        let tx = conn.transaction()?;
        let mut newly_modified_ids = Vec::new();

        for edit in edits {
            match edit.op.as_str() {
                "insert_after" | "insert_before" | "append" | "prepend" => {
                    let content = edit.content.clone().unwrap_or_default();
                    let lines_to_insert: Vec<&str> = if content.is_empty() {
                        vec![""]
                    } else {
                        content.split('\n').collect()
                    };
                    
                    let is_empty_target = edit.target_id.as_ref().map_or(true, |s| s.is_empty());
                    let (gap_start, gap_end) = if edit.op == "append" || (edit.op == "insert_after" && is_empty_target) {
                        let last_order: Option<f64> = tx.query_row(
                            "SELECT MAX(sort_order) FROM lines WHERE session_id = ?1",
                            [session_id],
                            |row| row.get(0)
                        ).ok().flatten();
                        let start = last_order.unwrap_or(0.0);
                        (start, start + 1000.0)
                    } else if edit.op == "prepend" || (edit.op == "insert_before" && is_empty_target) {
                        let first_order: Option<f64> = tx.query_row(
                            "SELECT MIN(sort_order) FROM lines WHERE session_id = ?1",
                            [session_id],
                            |row| row.get(0)
                        ).ok().flatten();
                        let end = first_order.unwrap_or(1000.0);
                        (end - 1000.0, end)
                    } else {
                        let target_id = edit.target_id.as_ref().filter(|s| !s.is_empty())
                            .context(format!("CHECKSUM_ERROR: Missing target_id for {} operation", edit.op))?;
                        let (target_seq, target_hash) = parse_line_id(target_id)?;
                        
                        let (db_hash_opt, db_order): (Option<String>, f64) = tx.query_row(
                            "SELECT line_hash, sort_order FROM lines WHERE session_id = ?1 AND sequence_id = ?2",
                            rusqlite::params![session_id, target_seq],
                            |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, f64>(1)?))
                        ).context(format!("Target line not found for target_id={}", target_id))?;

                        let db_hash = match db_hash_opt {
                            Some(h) => h,
                            None => {
                                let line_content: String = tx.query_row(
                                    "SELECT content FROM lines WHERE session_id = ?1 AND sequence_id = ?2",
                                    rusqlite::params![session_id, target_seq],
                                    |row| row.get(0)
                                )?;
                                let h = compute_line_hash(&line_content);
                                tx.execute(
                                    "UPDATE lines SET line_hash = ?1 WHERE session_id = ?2 AND sequence_id = ?3",
                                    rusqlite::params![h, session_id, target_seq],
                                )?;
                                h
                            }
                        };

                        if db_hash != target_hash {
                            anyhow::bail!("CHECKSUM_ERROR: Target line hash mismatch for target_id={}. Edit rejected.", target_id);
                        }

                        if edit.op == "insert_after" {
                            let next_order: Option<f64> = tx.query_row(
                                "SELECT MIN(sort_order) FROM lines WHERE session_id = ?1 AND sort_order > ?2",
                                rusqlite::params![session_id, db_order],
                                |row| row.get(0)
                            ).ok().flatten();
                            (db_order, next_order.unwrap_or(db_order + 1000.0))
                        } else {
                            let prev_order: Option<f64> = tx.query_row(
                                "SELECT MAX(sort_order) FROM lines WHERE session_id = ?1 AND sort_order < ?2",
                                rusqlite::params![session_id, db_order],
                                |row| row.get(0)
                            ).ok().flatten();
                            (prev_order.unwrap_or(0.0), db_order)
                        }
                    };

                    let max_seq: i64 = tx.query_row(
                        "SELECT COALESCE(MAX(sequence_id), 0) FROM lines WHERE session_id = ?1",
                        [session_id],
                        |row| row.get(0)
                    )?;

                    let gap = gap_end - gap_start;
                    let step = gap / (lines_to_insert.len() as f64 + 1.0);

                    for (ins_idx, ins_line) in lines_to_insert.iter().enumerate() {
                        let new_seq = max_seq + 1 + (ins_idx as i64);
                        let hash = compute_line_hash(ins_line);
                        let current_order = gap_start + step * ((ins_idx + 1) as f64);
                        tx.execute(
                            "INSERT INTO lines (session_id, sequence_id, line_hash, content, sort_order) VALUES (?1, ?2, ?3, ?4, ?5);",
                            rusqlite::params![session_id, new_seq, hash, ins_line, current_order]
                        )?;
                        newly_modified_ids.push(format!("{:x}#{}", new_seq, hash));
                    }
                }
                "update" => {
                    let target_id = edit.target_id.as_ref().context("Missing target_id for update op")?;
                    let (target_seq, target_hash) = parse_line_id(target_id)?;
                    let content = edit.content.as_ref().context("Missing content for update op")?;

                    let (db_hash_opt, db_id): (Option<String>, i64) = tx.query_row(
                        "SELECT line_hash, id FROM lines WHERE session_id = ?1 AND sequence_id = ?2",
                        rusqlite::params![session_id, target_seq],
                        |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?))
                    ).context(format!("Target line not found for target_id={}", target_id))?;
                    
                    let db_hash = match db_hash_opt {
                        Some(h) => h,
                        None => {
                            let line_content: String = tx.query_row(
                                "SELECT content FROM lines WHERE id = ?1",
                                rusqlite::params![db_id],
                                |row| row.get(0)
                            )?;
                            let h = compute_line_hash(&line_content);
                            tx.execute(
                                "UPDATE lines SET line_hash = ?1 WHERE id = ?2",
                                rusqlite::params![h, db_id],
                            )?;
                            h
                        }
                    };

                    if db_hash != target_hash {
                        anyhow::bail!("CHECKSUM_ERROR: Target line hash mismatch for target_id={}. Edit rejected.", target_id);
                    }

                    let new_hash = compute_line_hash(content);
                    tx.execute(
                        "UPDATE lines SET content = ?1, line_hash = ?2 WHERE id = ?3;",
                        rusqlite::params![content, new_hash, db_id]
                    )?;
                    newly_modified_ids.push(format!("{:x}#{}", target_seq, new_hash));
                }
                "delete" => {
                    let target_id = edit.target_id.as_ref().context("Missing target_id for delete op")?;
                    let (target_seq, target_hash) = parse_line_id(target_id)?;

                    let (db_hash_opt, db_id, sort_order): (Option<String>, i64, f64) = tx.query_row(
                        "SELECT line_hash, id, sort_order FROM lines WHERE session_id = ?1 AND sequence_id = ?2",
                        rusqlite::params![session_id, target_seq],
                        |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?, row.get::<_, f64>(2)?))
                    ).context(format!("Target line not found for target_id={}", target_id))?;
                    
                    let db_hash = match db_hash_opt {
                        Some(h) => h,
                        None => {
                            let line_content: String = tx.query_row(
                                "SELECT content FROM lines WHERE id = ?1",
                                rusqlite::params![db_id],
                                |row| row.get(0)
                            )?;
                            let h = compute_line_hash(&line_content);
                            tx.execute(
                                "UPDATE lines SET line_hash = ?1 WHERE id = ?2",
                                rusqlite::params![h, db_id],
                            )?;
                            h
                        }
                    };

                    if db_hash != target_hash {
                        anyhow::bail!("CHECKSUM_ERROR: Target line hash mismatch for target_id={}. Edit rejected.", target_id);
                    }

                    tx.execute("DELETE FROM lines WHERE id = ?1;", [db_id])?;

                    // Find closest remaining line to deletion and add to newly_modified_ids to show context
                    let closest: Option<(i64, i64, Option<String>)> = tx.query_row(
                        "SELECT id, sequence_id, line_hash FROM lines WHERE session_id = ?1 ORDER BY ABS(sort_order - ?2) LIMIT 1",
                        rusqlite::params![session_id, sort_order],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                    ).ok();
                    if let Some((db_id, seq, hash_opt)) = closest {
                        let hash = match hash_opt {
                            Some(h) => h,
                            None => {
                                let line_content: String = tx.query_row(
                                    "SELECT content FROM lines WHERE id = ?1",
                                    [db_id],
                                    |row| row.get(0)
                                )?;
                                let h = compute_line_hash(&line_content);
                                tx.execute(
                                    "UPDATE lines SET line_hash = ?1 WHERE id = ?2",
                                    rusqlite::params![h, db_id],
                                )?;
                                h
                            }
                        };
                        newly_modified_ids.push(format!("{:x}#{}", seq, hash));
                    }
                }
                "replace_range" => {
                    let start_id = edit.target_id.as_ref().filter(|s| !s.is_empty())
                        .context("CHECKSUM_ERROR: Missing target_id (start_id) for replace_range op")?;
                    let end_id = edit.end_target_id.as_ref().filter(|s| !s.is_empty())
                        .context("CHECKSUM_ERROR: Missing end_target_id for replace_range op")?;
                    let content = edit.content.as_ref().unwrap_or(&String::new()).clone();

                    let (start_seq, start_hash) = parse_line_id(start_id)?;
                    let (end_seq, end_hash) = parse_line_id(end_id)?;

                    let (start_hash_opt, start_order): (Option<String>, f64) = tx.query_row(
                        "SELECT line_hash, sort_order FROM lines WHERE session_id = ?1 AND sequence_id = ?2",
                        rusqlite::params![session_id, start_seq],
                        |row| Ok((row.get(0)?, row.get(1)?))
                    ).context(format!("Start line not found for target_id={}", start_id))?;

                    let db_start_hash = match start_hash_opt {
                        Some(h) => h,
                        None => {
                            let line_content: String = tx.query_row(
                                "SELECT content FROM lines WHERE session_id = ?1 AND sequence_id = ?2",
                                rusqlite::params![session_id, start_seq],
                                |row| row.get(0)
                            )?;
                            let h = compute_line_hash(&line_content);
                            tx.execute(
                                "UPDATE lines SET line_hash = ?1 WHERE session_id = ?2 AND sequence_id = ?3",
                                rusqlite::params![h, session_id, start_seq],
                            )?;
                            h
                        }
                    };
                    if db_start_hash != start_hash {
                        anyhow::bail!("CHECKSUM_ERROR: Start line hash mismatch for target_id={}. Edit rejected.", start_id);
                    }

                    let (end_hash_opt, end_order): (Option<String>, f64) = tx.query_row(
                        "SELECT line_hash, sort_order FROM lines WHERE session_id = ?1 AND sequence_id = ?2",
                        rusqlite::params![session_id, end_seq],
                        |row| Ok((row.get(0)?, row.get(1)?))
                    ).context(format!("End line not found for end_target_id={}", end_id))?;

                    let db_end_hash = match end_hash_opt {
                        Some(h) => h,
                        None => {
                            let line_content: String = tx.query_row(
                                "SELECT content FROM lines WHERE session_id = ?1 AND sequence_id = ?2",
                                rusqlite::params![session_id, end_seq],
                                |row| row.get(0)
                            )?;
                            let h = compute_line_hash(&line_content);
                            tx.execute(
                                "UPDATE lines SET line_hash = ?1 WHERE session_id = ?2 AND sequence_id = ?3",
                                rusqlite::params![h, session_id, end_seq],
                            )?;
                            h
                        }
                    };
                    if db_end_hash != end_hash {
                        anyhow::bail!("CHECKSUM_ERROR: End line hash mismatch for end_target_id={}. Edit rejected.", end_id);
                    }

                    if start_order > end_order {
                        anyhow::bail!("VALIDATION_ERROR: start_id sort order is after end_id sort order for replace_range.");
                    }

                    let prev_order: Option<f64> = tx.query_row(
                        "SELECT MAX(sort_order) FROM lines WHERE session_id = ?1 AND sort_order < ?2",
                        rusqlite::params![session_id, start_order],
                        |row| row.get(0)
                    ).ok().flatten();
                    let gap_start = prev_order.unwrap_or(start_order - 1000.0);

                    let next_order: Option<f64> = tx.query_row(
                        "SELECT MIN(sort_order) FROM lines WHERE session_id = ?1 AND sort_order > ?2",
                        rusqlite::params![session_id, end_order],
                        |row| row.get(0)
                    ).ok().flatten();
                    let gap_end = next_order.unwrap_or(end_order + 1000.0);

                    tx.execute(
                        "DELETE FROM lines WHERE session_id = ?1 AND sort_order >= ?2 AND sort_order <= ?3",
                        rusqlite::params![session_id, start_order, end_order]
                    )?;

                    let lines_to_insert: Vec<&str> = if content.is_empty() {
                        vec![""]
                    } else {
                        content.split('\n').collect()
                    };

                    let max_seq: i64 = tx.query_row(
                        "SELECT COALESCE(MAX(sequence_id), 0) FROM lines WHERE session_id = ?1",
                        [session_id],
                        |row| row.get(0)
                    )?;

                    let gap = gap_end - gap_start;
                    let step = gap / (lines_to_insert.len() as f64 + 1.0);

                    for (ins_idx, ins_line) in lines_to_insert.iter().enumerate() {
                        let new_seq = max_seq + 1 + (ins_idx as i64);
                        let hash = compute_line_hash(ins_line);
                        let current_order = gap_start + step * ((ins_idx + 1) as f64);
                        tx.execute(
                            "INSERT INTO lines (session_id, sequence_id, line_hash, content, sort_order) VALUES (?1, ?2, ?3, ?4, ?5);",
                            rusqlite::params![session_id, new_seq, hash, ins_line, current_order]
                        )?;
                        newly_modified_ids.push(format!("{:x}#{}", new_seq, hash));
                    }
                }
                "move" => {
                    let start_id = edit.target_id.as_ref().filter(|s| !s.is_empty())
                        .context("CHECKSUM_ERROR: Missing target_id (start_id) for move op")?;
                    let end_id = edit.end_target_id.as_ref().filter(|s| !s.is_empty()).unwrap_or(start_id);
                    let move_pos = edit.move_position.as_deref().unwrap_or("after");

                    let (start_seq, start_hash) = parse_line_id(start_id)?;
                    let (end_seq, end_hash) = parse_line_id(end_id)?;

                    let (start_hash_opt, start_order): (Option<String>, f64) = tx.query_row(
                        "SELECT line_hash, sort_order FROM lines WHERE session_id = ?1 AND sequence_id = ?2",
                        rusqlite::params![session_id, start_seq],
                        |row| Ok((row.get(0)?, row.get(1)?))
                    ).context(format!("Start line not found for target_id={}", start_id))?;
                    let db_start_hash = match start_hash_opt {
                        Some(h) => h,
                        None => {
                            let line_content: String = tx.query_row(
                                "SELECT content FROM lines WHERE session_id = ?1 AND sequence_id = ?2",
                                rusqlite::params![session_id, start_seq],
                                |row| row.get(0)
                            )?;
                            let h = compute_line_hash(&line_content);
                            tx.execute(
                                "UPDATE lines SET line_hash = ?1 WHERE session_id = ?2 AND sequence_id = ?3",
                                rusqlite::params![h, session_id, start_seq],
                            )?;
                            h
                        }
                    };
                    if db_start_hash != start_hash {
                        anyhow::bail!("CHECKSUM_ERROR: Start line hash mismatch for target_id={}. Edit rejected.", start_id);
                    }

                    let (end_hash_opt, end_order): (Option<String>, f64) = tx.query_row(
                        "SELECT line_hash, sort_order FROM lines WHERE session_id = ?1 AND sequence_id = ?2",
                        rusqlite::params![session_id, end_seq],
                        |row| Ok((row.get(0)?, row.get(1)?))
                    ).context(format!("End line not found for end_target_id={}", end_id))?;
                    let db_end_hash = match end_hash_opt {
                        Some(h) => h,
                        None => {
                            let line_content: String = tx.query_row(
                                "SELECT content FROM lines WHERE session_id = ?1 AND sequence_id = ?2",
                                rusqlite::params![session_id, end_seq],
                                |row| row.get(0)
                            )?;
                            let h = compute_line_hash(&line_content);
                            tx.execute(
                                "UPDATE lines SET line_hash = ?1 WHERE session_id = ?2 AND sequence_id = ?3",
                                rusqlite::params![h, session_id, end_seq],
                            )?;
                            h
                        }
                    };
                    if db_end_hash != end_hash {
                        anyhow::bail!("CHECKSUM_ERROR: End line hash mismatch for end_target_id={}. Edit rejected.", end_id);
                    }

                    if start_order > end_order {
                        anyhow::bail!("VALIDATION_ERROR: start_id sort order is after end_id sort order for move.");
                    }

                    struct MovedLine {
                        id: i64,
                        seq: i64,
                        hash: String,
                    }
                    let mut moved_lines = Vec::new();
                    {
                        let mut stmt = tx.prepare("SELECT id, sequence_id, COALESCE(line_hash, '') FROM lines WHERE session_id = ?1 AND sort_order >= ?2 AND sort_order <= ?3 ORDER BY sort_order")?;
                        let mut rows = stmt.query(rusqlite::params![session_id, start_order, end_order])?;
                        while let Some(row) = rows.next()? {
                            let id: i64 = row.get(0)?;
                            let seq: i64 = row.get(1)?;
                            let mut hash: String = row.get(2)?;
                            if hash.is_empty() {
                                let line_content: String = tx.query_row(
                                    "SELECT content FROM lines WHERE id = ?1",
                                    [id],
                                    |r| r.get(0)
                                )?;
                                hash = compute_line_hash(&line_content);
                                tx.execute(
                                    "UPDATE lines SET line_hash = ?1 WHERE id = ?2",
                                    rusqlite::params![hash, id],
                                )?;
                            }
                            moved_lines.push(MovedLine { id, seq, hash });
                        }
                    }

                    let (gap_start, gap_end) = match move_pos {
                        "prepend" => {
                            let first_non_moved: Option<f64> = tx.query_row(
                                "SELECT MIN(sort_order) FROM lines WHERE session_id = ?1 AND (sort_order < ?2 OR sort_order > ?3)",
                                rusqlite::params![session_id, start_order, end_order],
                                |row| row.get(0)
                            ).ok().flatten();
                            let end = first_non_moved.unwrap_or(1000.0);
                            (end - 1000.0, end)
                        }
                        "append" => {
                            let last_non_moved: Option<f64> = tx.query_row(
                                "SELECT MAX(sort_order) FROM lines WHERE session_id = ?1 AND (sort_order < ?2 OR sort_order > ?3)",
                                rusqlite::params![session_id, start_order, end_order],
                                |row| row.get(0)
                            ).ok().flatten();
                            let start = last_non_moved.unwrap_or(0.0);
                            (start, start + 1000.0)
                        }
                        "before" | "after" => {
                            let dest_id = edit.dest_target_id.as_ref().filter(|s| !s.is_empty())
                                .context("CHECKSUM_ERROR: Missing dest_target_id for move before/after operation")?;
                            let (dest_seq, dest_hash) = parse_line_id(dest_id)?;

                            let (db_dest_hash_opt, dest_order): (Option<String>, f64) = tx.query_row(
                                "SELECT line_hash, sort_order FROM lines WHERE session_id = ?1 AND sequence_id = ?2",
                                rusqlite::params![session_id, dest_seq],
                                |row| Ok((row.get(0)?, row.get(1)?))
                            ).context(format!("Destination line not found for dest_target_id={}", dest_id))?;
                            let db_dest_hash = match db_dest_hash_opt {
                                Some(h) => h,
                                None => {
                                    let line_content: String = tx.query_row(
                                        "SELECT content FROM lines WHERE session_id = ?1 AND sequence_id = ?2",
                                        rusqlite::params![session_id, dest_seq],
                                        |row| row.get(0)
                                    )?;
                                    let h = compute_line_hash(&line_content);
                                     tx.execute(
                                        "UPDATE lines SET line_hash = ?1 WHERE session_id = ?2 AND sequence_id = ?3",
                                        rusqlite::params![h, session_id, dest_seq],
                                    )?;
                                    h
                                }
                            };
                            if db_dest_hash != dest_hash {
                                anyhow::bail!("CHECKSUM_ERROR: Destination line hash mismatch for dest_target_id={}. Edit rejected.", dest_id);
                            }

                            if dest_order >= start_order && dest_order <= end_order {
                                anyhow::bail!("VALIDATION_ERROR: Cannot move a range into itself (dest_target_id lies within source range).");
                            }

                            if move_pos == "before" {
                                let prev_non_moved: Option<f64> = tx.query_row(
                                    "SELECT MAX(sort_order) FROM lines WHERE session_id = ?1 AND sort_order < ?2 AND (sort_order < ?3 OR sort_order > ?4)",
                                    rusqlite::params![session_id, dest_order, start_order, end_order],
                                    |row| row.get(0)
                                ).ok().flatten();
                                (prev_non_moved.unwrap_or(dest_order - 1000.0), dest_order)
                            } else {
                                let next_non_moved: Option<f64> = tx.query_row(
                                    "SELECT MIN(sort_order) FROM lines WHERE session_id = ?1 AND sort_order > ?2 AND (sort_order < ?3 OR sort_order > ?4)",
                                    rusqlite::params![session_id, dest_order, start_order, end_order],
                                    |row| row.get(0)
                                ).ok().flatten();
                                (dest_order, next_non_moved.unwrap_or(dest_order + 1000.0))
                            }
                        }
                        other => anyhow::bail!("Unknown move position: {}", other),
                    };

                    let gap = gap_end - gap_start;
                    let step = gap / (moved_lines.len() as f64 + 1.0);

                    for (idx, line) in moved_lines.iter().enumerate() {
                        let current_order = gap_start + step * ((idx + 1) as f64);
                        tx.execute(
                            "UPDATE lines SET sort_order = ?1 WHERE id = ?2;",
                            rusqlite::params![current_order, line.id]
                        )?;
                        newly_modified_ids.push(format!("{:x}#{}", line.seq, line.hash));
                    }
                }
                other => anyhow::bail!("Unknown edit operation: {}", other),
            }
        }

        tx.commit()?;
        Ok(newly_modified_ids)
    }

    fn restore_session_lines(
        &self,
        session_id: &str,
        lines: &[(i64, Option<String>, String)],
    ) -> Result<()> {
        let mut conn = get_db_connection()?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM lines WHERE session_id = ?1", [session_id])?;
        
        {
            let mut stmt = tx.prepare(
                "INSERT INTO lines (session_id, sequence_id, line_hash, content, sort_order) VALUES (?1, ?2, ?3, ?4, ?5);"
            )?;
            
            for (idx, (seq, hash_opt, content)) in lines.iter().enumerate() {
                let sort_order = ((idx + 1) as f64) * 1000.0;
                stmt.execute(rusqlite::params![session_id, seq, hash_opt, content, sort_order])?;
            }
        }
        
        tx.commit()?;
        Ok(())
    }

    fn update_session_metadata(
        &self,
        session_id: &str,
        file_hash: &str,
        mtime: i64,
    ) -> Result<()> {
        let conn = get_db_connection()?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
        conn.execute(
            "UPDATE sessions SET file_hash = ?1, mtime = ?2, last_accessed_at = ?3 WHERE session_id = ?4;",
            rusqlite::params![file_hash, mtime, now, session_id]
        )?;
        Ok(())
    }

    fn delete_session(&self, filepath: &str) -> Result<()> {
        let conn = get_db_connection()?;
        conn.execute(
            "DELETE FROM lines WHERE session_id IN (SELECT session_id FROM sessions WHERE filepath = ?1);",
            [filepath]
        )?;
        conn.execute(
            "DELETE FROM sessions WHERE filepath = ?1;",
            [filepath]
        )?;
        Ok(())
    }

    fn get_session_mtime(&self, filepath: &str) -> Result<Option<i64>> {
        let conn = get_db_connection()?;
        let res = conn.query_row(
            "SELECT mtime FROM sessions WHERE filepath = ?1",
            [filepath],
            |row| row.get(0)
        );
        match res {
            Ok(mtime) => Ok(Some(mtime)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(anyhow::Error::from(err)),
        }
    }
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

fn cleanup_stale_sessions(conn: &Connection) -> Result<()> {
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
        Some("This file type is not supported for AST syntax validation. However, you can still view and edit it safely using line-level editing tools (view_lines and edit_lines). All line sequence IDs are fully active!".to_string())
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

fn ensure_hashes_for_range(conn: &Connection, session_id: &str, start_line: usize, end_line: usize) -> Result<()> {
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
    async fn test_view_lines() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let temp_dir = std::env::temp_dir().join("line-editor-test-view");
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir)?;
        }
        fs::create_dir_all(&temp_dir)?;
        let file_path = temp_dir.join("code.rs");
        fs::write(&file_path, "fn main() {\n    println!(\"Hello!\");\n}\n")?;
        let filepath_str = file_path.to_str().unwrap();

        let repository = SqliteSessionRepository;

        // 1. Initialize session
        let metadata = init_edit_session(filepath_str, false)?;
        assert_eq!(metadata.total_lines, 3);

        // 2. View all lines (1 to 3)
        let output = crate::tools::view::view_lines(&repository, filepath_str, 1, 3, None)?;
        
        // Verify structure
        let val: serde_json::Value = serde_json::from_str(&output)?;
        let lines = val["lines"].as_array().unwrap();
        assert_eq!(lines.len(), 3);
        
        let line0 = lines[0].as_array().unwrap();
        let line1 = lines[1].as_array().unwrap();
        let line2 = lines[2].as_array().unwrap();

        assert!(line0[0].as_str().unwrap().starts_with("1#"));
        assert!(line1[0].as_str().unwrap().starts_with("2#"));
        assert!(line2[0].as_str().unwrap().starts_with("3#"));
        assert_eq!(line0[2].as_str().unwrap(), "fn main() {");
        assert_eq!(line1[2].as_str().unwrap(), "    println!(\"Hello!\");");
        assert_eq!(line2[2].as_str().unwrap(), "}");

        // 3. View sub-range (2 to 2)
        let sub_output = crate::tools::view::view_lines(&repository, filepath_str, 2, 2, None)?;
        let sub_val: serde_json::Value = serde_json::from_str(&sub_output)?;
        let sub_lines = sub_val["lines"].as_array().unwrap();
        assert_eq!(sub_lines.len(), 1);
        let sub_line0 = sub_lines[0].as_array().unwrap();
        assert!(sub_line0[0].as_str().unwrap().starts_with("2#"));

        // 4. Test bounds validation
        assert!(crate::tools::view::view_lines(&repository, filepath_str, 0, 3, None).is_err());
        assert!(crate::tools::view::view_lines(&repository, filepath_str, 3, 1, None).is_err());

        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn test_view_lines_jit_initialization() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let temp_dir = std::env::temp_dir().join("line-editor-test-jit-view");
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir)?;
        }
        fs::create_dir_all(&temp_dir)?;
        let file_path = temp_dir.join("code.rs");
        fs::write(&file_path, "fn main() {\n    println!(\"Hello!\");\n}\n")?;
        let filepath_str = file_path.to_str().unwrap();

        let repository = SqliteSessionRepository;

        // Call view_lines directly without calling init_edit_session
        let output = crate::tools::view::view_lines(&repository, filepath_str, 1, 3, None)?;
        
        let val: serde_json::Value = serde_json::from_str(&output)?;
        let lines = val["lines"].as_array().unwrap();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].as_array().unwrap()[2].as_str().unwrap(), "fn main() {");

        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }
}
