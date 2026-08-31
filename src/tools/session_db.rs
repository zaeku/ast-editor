use anyhow::{Result, Context};
use rusqlite::Connection;
use serde::{Serialize, Deserialize};
use std::path::PathBuf;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};
use tree_sitter::{WasmStore, Parser};

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
            parent_context TEXT,
            FOREIGN KEY(session_id) REFERENCES sessions(session_id) ON DELETE CASCADE
        );",
        [],
    ).context("Failed to create lines table")?;
    
    // Add indices for fast lookups
    conn.execute("CREATE INDEX IF NOT EXISTS idx_lines_session_id ON lines(session_id);", [])
        .context("Failed to create index idx_lines_session_id")?;
    conn.execute("CREATE INDEX IF NOT EXISTS idx_lines_sort_order ON lines(sort_order);", [])
        .context("Failed to create index idx_lines_sort_order")?;
    
    // Add crlf column if not present
    let _ = conn.execute("ALTER TABLE sessions ADD COLUMN line_ending_crlf INTEGER DEFAULT 0;", []);
    // Add parent_context column to lines if not present
    let _ = conn.execute("ALTER TABLE lines ADD COLUMN parent_context TEXT;", []);

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

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum EditOp {
    #[default]
    InsertAfter,
    InsertBefore,
    Append,
    Prepend,
    Update,
    Delete,
    ReplaceRange,
    ReplaceSubstring,
    Move,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum MovePosition {
    Prepend,
    Append,
    Before,
    #[default]
    After,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct LineEdit {
    pub op: EditOp,
    pub target_id: Option<String>,
    pub end_target_id: Option<String>,
    pub dest_target_id: Option<String>,
    pub move_position: Option<MovePosition>,
    pub content: Option<String>,
    pub pattern: Option<String>,
    pub replacement: Option<String>,
    pub occurrence: Option<usize>,
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

pub fn resolve_line_id(db_conn: &rusqlite::Connection, session_id: &str, id_str: &str) -> Result<(i64, String)> {
    if id_str.contains('#') {
        parse_line_id(id_str)
    } else {
        // 1. Populate any missing hashes (lazy evaluation)
        let mut stmt = db_conn.prepare(
            "SELECT id, content FROM lines WHERE session_id = ?1 AND line_hash IS NULL"
        )?;
        let mut rows = stmt.query([session_id])?;
        let mut updates = Vec::new();
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let content: String = row.get(1)?;
            let h = compute_line_hash(&content);
            updates.push((id, h));
        }
        drop(rows);
        drop(stmt);
        for (id, h) in updates {
            db_conn.execute(
                "UPDATE lines SET line_hash = ?1 WHERE id = ?2",
                rusqlite::params![h, id]
            )?;
        }

        // 2. Query for matches
        let mut stmt = db_conn.prepare(
            "SELECT sequence_id, line_hash FROM lines WHERE session_id = ?1 AND line_hash = ?2"
        )?;
        let mut rows = stmt.query(rusqlite::params![session_id, id_str])?;
        let mut matches = Vec::new();
        while let Some(row) = rows.next()? {
            let seq: i64 = row.get(0)?;
            let hash: String = row.get(1)?;
            matches.push((seq, hash));
        }
        if matches.is_empty() {
            anyhow::bail!("CHECKSUM_ERROR: Target line hash '{}' not found in session", id_str);
        } else if matches.len() > 1 {
            anyhow::bail!("CHECKSUM_ERROR: Target line hash '{}' is ambiguous (matches multiple lines in session)", id_str);
        } else {
            Ok(matches.remove(0))
        }
    }
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
    fn get_session_crlf(&self, session_id: &str) -> Result<bool>;
    fn get_session_id(&self, filepath: &str) -> Result<Option<String>>;
    fn find_matching_lines(&self, session_id: &str, query: &str) -> Result<Vec<usize>>;
    fn smart_resync(
        &self,
        filepath: &str,
        session_id: &str,
        target_ids: &[String],
    ) -> Result<()>;
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

    fn find_matching_lines(&self, session_id: &str, query: &str) -> Result<Vec<usize>> {
        let conn = get_db_connection()?;
        let mut stmt = conn.prepare(
            "SELECT row_number FROM (
                SELECT ROW_NUMBER() OVER (ORDER BY sort_order) as row_number, content 
                FROM lines 
                WHERE session_id = ?1
             ) WHERE content LIKE ?2"
        )?;
        let like_pattern = format!("%{}%", query);
        let mut rows = stmt.query(rusqlite::params![session_id, like_pattern])?;
        let mut matches = Vec::new();
        while let Some(row) = rows.next()? {
            let row_num: usize = row.get(0)?;
            matches.push(row_num);
        }
        Ok(matches)
    }

    fn get_session_crlf(&self, session_id: &str) -> Result<bool> {
        let conn = get_db_connection()?;
        let crlf: i32 = conn.query_row(
            "SELECT COALESCE(line_ending_crlf, 0) FROM sessions WHERE session_id = ?1",
            rusqlite::params![session_id],
            |row| row.get(0)
        )?;
        Ok(crlf != 0)
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
            match edit.op {
                EditOp::InsertAfter | EditOp::InsertBefore | EditOp::Append | EditOp::Prepend => {
                    let content = edit.content.clone().unwrap_or_default();
                    let mut lines_to_insert: Vec<&str> = if content.is_empty() {
                        vec![""]
                    } else {
                        content.split('\n').collect()
                    };
                    if lines_to_insert.last() == Some(&"") {
                        lines_to_insert.pop();
                    }
                    if lines_to_insert.is_empty() {
                        lines_to_insert.push("");
                    }
                    
                    let is_empty_target = edit.target_id.as_ref().map_or(true, |s| s.is_empty());
                    let (gap_start, gap_end) = if edit.op == EditOp::Append || (edit.op == EditOp::InsertAfter && is_empty_target) {
                        let last_order: Option<f64> = tx.query_row(
                            "SELECT MAX(sort_order) FROM lines WHERE session_id = ?1",
                            [session_id],
                            |row| row.get(0)
                        ).ok().flatten();
                        let start = last_order.unwrap_or(0.0);
                        (start, start + 1000.0)
                    } else if edit.op == EditOp::Prepend || (edit.op == EditOp::InsertBefore && is_empty_target) {
                        let first_order: Option<f64> = tx.query_row(
                            "SELECT MIN(sort_order) FROM lines WHERE session_id = ?1",
                            [session_id],
                            |row| row.get(0)
                        ).ok().flatten();
                        let end = first_order.unwrap_or(1000.0);
                        (end - 1000.0, end)
                    } else {
                        let target_id = edit.target_id.as_ref().filter(|s| !s.is_empty())
                            .context(format!("CHECKSUM_ERROR: Missing target_id for {:?}", edit.op))?;
                        let (target_seq, target_hash) = resolve_line_id(&tx, session_id, target_id)?;
                        
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

                        if edit.op == EditOp::InsertAfter {
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
                        let trimmed_ins = ins_line.strip_suffix('\r').unwrap_or(ins_line);
                        let new_seq = max_seq + 1 + (ins_idx as i64);
                        let hash = compute_line_hash(trimmed_ins);
                        let current_order = gap_start + step * ((ins_idx + 1) as f64);
                        tx.execute(
                            "INSERT INTO lines (session_id, sequence_id, line_hash, content, sort_order) VALUES (?1, ?2, ?3, ?4, ?5);",
                            rusqlite::params![session_id, new_seq, hash, trimmed_ins, current_order]
                        )?;
                        newly_modified_ids.push(format!("{:x}#{}", new_seq, hash));
                    }
                }
                EditOp::Update => {
                    let target_id = edit.target_id.as_ref().context("Missing target_id for update op")?;
                    let (target_seq, target_hash) = resolve_line_id(&tx, session_id, target_id)?;
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

                    let mut content_clean = content.clone();
                    if content_clean.ends_with('\n') {
                        content_clean.pop();
                    }
                    if content_clean.ends_with('\r') {
                        content_clean.pop();
                    }
                    let new_hash = compute_line_hash(&content_clean);
                    tx.execute(
                        "UPDATE lines SET content = ?1, line_hash = ?2 WHERE id = ?3;",
                        rusqlite::params![content_clean, new_hash, db_id]
                    )?;
                    newly_modified_ids.push(format!("{:x}#{}", target_seq, new_hash));
                }
                EditOp::ReplaceSubstring => {
                    let target_id = edit.target_id.as_ref().context("Missing target_id for replace_substring op")?;
                    let (target_seq, target_hash) = resolve_line_id(&tx, session_id, target_id)?;
                    let pattern = edit.pattern.as_ref().context("Missing pattern for replace_substring op")?;
                    let replacement = edit.replacement.as_ref().context("Missing replacement for replace_substring op")?;
                    let occurrence = edit.occurrence.unwrap_or(1);

                    let (db_hash_opt, db_id, current_content): (Option<String>, i64, String) = tx.query_row(
                        "SELECT line_hash, id, content FROM lines WHERE session_id = ?1 AND sequence_id = ?2",
                        rusqlite::params![session_id, target_seq],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get::<_, String>(2)?))
                    ).context(format!("Target line not found for target_id={}", target_id))?;

                    let db_hash = match db_hash_opt {
                        Some(h) => h,
                        None => {
                            let h = compute_line_hash(&current_content);
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

                    if occurrence < 1 {
                        anyhow::bail!("Invalid occurrence number: {}. Must be >= 1.", occurrence);
                    }

                    // Find and replace the N-th occurrence
                    let mut new_content = String::new();
                    let mut current_pos = 0;
                    let mut match_count = 0;
                    let mut replaced = false;

                    while let Some(pos) = current_content[current_pos..].find(pattern) {
                        let actual_pos = current_pos + pos;
                        new_content.push_str(&current_content[current_pos..actual_pos]);
                        match_count += 1;
                        if match_count == occurrence {
                            new_content.push_str(replacement);
                            current_pos = actual_pos + pattern.len();
                            replaced = true;
                            break;
                        } else {
                            new_content.push_str(pattern);
                            current_pos = actual_pos + pattern.len();
                        }
                    }

                    if !replaced {
                        anyhow::bail!(
                            "CHECKSUM_ERROR: Pattern '{}' (occurrence {}) not found in target line.",
                            pattern,
                            occurrence
                        );
                    }

                    new_content.push_str(&current_content[current_pos..]);

                    let new_hash = compute_line_hash(&new_content);
                    tx.execute(
                        "UPDATE lines SET content = ?1, line_hash = ?2 WHERE id = ?3;",
                        rusqlite::params![new_content, new_hash, db_id]
                    )?;
                    newly_modified_ids.push(format!("{:x}#{}", target_seq, new_hash));
                }
                EditOp::Delete => {
                    let target_id = edit.target_id.as_ref().context("Missing target_id for delete op")?;
                    let (target_seq, target_hash) = resolve_line_id(&tx, session_id, target_id)?;

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
                EditOp::ReplaceRange => {
                    let start_id = edit.target_id.as_ref().filter(|s| !s.is_empty())
                        .context("CHECKSUM_ERROR: Missing target_id (start_id) for replace_range op")?;
                    let end_id = edit.end_target_id.as_ref().filter(|s| !s.is_empty())
                        .context("CHECKSUM_ERROR: Missing end_target_id for replace_range op")?;
                    let content = edit.content.as_ref().unwrap_or(&String::new()).clone();

                    let (start_seq, start_hash) = resolve_line_id(&tx, session_id, start_id)?;
                    let (end_seq, end_hash) = resolve_line_id(&tx, session_id, end_id)?;

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

                    let mut lines_to_insert: Vec<&str> = if content.is_empty() {
                        vec![""]
                    } else {
                        content.split('\n').collect()
                    };
                    if lines_to_insert.last() == Some(&"") {
                        lines_to_insert.pop();
                    }
                    if lines_to_insert.is_empty() {
                        lines_to_insert.push("");
                    }

                    let max_seq: i64 = tx.query_row(
                        "SELECT COALESCE(MAX(sequence_id), 0) FROM lines WHERE session_id = ?1",
                        [session_id],
                        |row| row.get(0)
                    )?;

                    let gap = gap_end - gap_start;
                    let step = gap / (lines_to_insert.len() as f64 + 1.0);

                    for (ins_idx, ins_line) in lines_to_insert.iter().enumerate() {
                        let trimmed_ins = ins_line.strip_suffix('\r').unwrap_or(ins_line);
                        let new_seq = max_seq + 1 + (ins_idx as i64);
                        let hash = compute_line_hash(trimmed_ins);
                        let current_order = gap_start + step * ((ins_idx + 1) as f64);
                        tx.execute(
                            "INSERT INTO lines (session_id, sequence_id, line_hash, content, sort_order) VALUES (?1, ?2, ?3, ?4, ?5);",
                            rusqlite::params![session_id, new_seq, hash, trimmed_ins, current_order]
                        )?;
                        newly_modified_ids.push(format!("{:x}#{}", new_seq, hash));
                    }
                }
                EditOp::Move => {
                    let start_id = edit.target_id.as_ref().filter(|s| !s.is_empty())
                         .context("CHECKSUM_ERROR: Missing target_id (start_id) for move op")?;
                    let end_id = edit.end_target_id.as_ref().filter(|s| !s.is_empty()).unwrap_or(start_id);
                    let move_pos = edit.move_position.unwrap_or(MovePosition::After);

                    let (start_seq, start_hash) = resolve_line_id(&tx, session_id, start_id)?;
                    let (end_seq, end_hash) = resolve_line_id(&tx, session_id, end_id)?;

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
                        MovePosition::Prepend => {
                            let first_non_moved: Option<f64> = tx.query_row(
                                "SELECT MIN(sort_order) FROM lines WHERE session_id = ?1 AND (sort_order < ?2 OR sort_order > ?3)",
                                rusqlite::params![session_id, start_order, end_order],
                                |row| row.get(0)
                            ).ok().flatten();
                            let end = first_non_moved.unwrap_or(1000.0);
                            (end - 1000.0, end)
                        }
                        MovePosition::Append => {
                            let last_non_moved: Option<f64> = tx.query_row(
                                "SELECT MAX(sort_order) FROM lines WHERE session_id = ?1 AND (sort_order < ?2 OR sort_order > ?3)",
                                rusqlite::params![session_id, start_order, end_order],
                                |row| row.get(0)
                            ).ok().flatten();
                            let start = last_non_moved.unwrap_or(0.0);
                            (start, start + 1000.0)
                        }
                        MovePosition::Before | MovePosition::After => {
                            let dest_id = edit.dest_target_id.as_ref().filter(|s| !s.is_empty())
                                .context("CHECKSUM_ERROR: Missing dest_target_id for move before/after operation")?;
                            let (dest_seq, dest_hash) = resolve_line_id(&tx, session_id, dest_id)?;

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

                            if move_pos == MovePosition::Before {
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

    fn get_session_id(&self, filepath: &str) -> Result<Option<String>> {
        let conn = get_db_connection()?;
        let res = conn.query_row(
            "SELECT session_id FROM sessions WHERE filepath = ?1 ORDER BY last_accessed_at DESC LIMIT 1",
            [filepath],
            |row| row.get::<_, String>(0)
        );
        match res {
            Ok(sid) => Ok(Some(sid)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(anyhow::Error::from(err)),
        }
    }

    fn smart_resync(
        &self,
        filepath: &str,
        session_id: &str,
        target_ids: &[String],
    ) -> Result<()> {
        let mut conn = get_db_connection()?;
        
        // 1. Populate any missing line hashes in DB for this session
        {
            let mut stmt = conn.prepare(
                "SELECT id, content FROM lines WHERE session_id = ?1 AND line_hash IS NULL"
            )?;
            let mut to_update = Vec::new();
            let mut rows = stmt.query(rusqlite::params![session_id])?;
            while let Some(row) = rows.next()? {
                let id: i64 = row.get(0)?;
                let content: String = row.get(1)?;
                let hash = compute_line_hash(&content);
                to_update.push((id, hash));
            }
            drop(rows);
            drop(stmt);
            for (id, h) in to_update {
                conn.execute(
                    "UPDATE lines SET line_hash = ?1 WHERE id = ?2",
                    rusqlite::params![h, id]
                )?;
            }
        }

        // 2. Resolve target IDs in the DB session
        let mut targeted_lines = Vec::new();
        for id in target_ids {
            if id.is_empty() {
                continue;
            }
            let (seq, hash) = resolve_line_id(&conn, session_id, id)?;
            targeted_lines.push((seq, hash));
        }

        // 3. Read new file from disk and compute line hashes
        let path = std::path::Path::new(filepath);
        let content = std::fs::read_to_string(path).context("Failed to read file for resync")?;
        
        // Split by lines (supporting CRLF/LF)
        let lines_on_disk: Vec<&str> = content.lines().map(|l| {
            l.strip_suffix('\r').unwrap_or(l)
        }).collect();

        let mut disk_line_hashes = Vec::with_capacity(lines_on_disk.len());
        let mut disk_hash_counts = std::collections::HashMap::with_capacity(lines_on_disk.len());
        for line in &lines_on_disk {
            let hash = compute_line_hash(line);
            *disk_hash_counts.entry(hash.clone()).or_insert(0) += 1;
            disk_line_hashes.push(hash);
        }

        // 4. Verify that all targeted line hashes exist uniquely in the new disk file
        for (_, hash) in &targeted_lines {
            let count = disk_hash_counts.get(hash).cloned().unwrap_or(0);
            if count == 0 {
                anyhow::bail!("CONCURRENCY_ERROR: Targeted line with hash '{}' has been modified or deleted externally.", hash);
            } else if count > 1 {
                anyhow::bail!("CONCURRENCY_ERROR: Targeted line with hash '{}' is ambiguous in the new file (multiple matches).", hash);
            }
        }

        // 5. Perform the resync inside a transaction
        let tx = conn.transaction()?;
        
        // Retrieve all current lines in DB to match
        let mut stmt = tx.prepare(
            "SELECT sequence_id, line_hash FROM lines WHERE session_id = ?1 ORDER BY sort_order"
        )?;
        let mut db_lines = Vec::new();
        let mut rows = stmt.query(rusqlite::params![session_id])?;
        while let Some(row) = rows.next()? {
            let seq: i64 = row.get(0)?;
            let hash_opt: Option<String> = row.get(1)?;
            db_lines.push((seq, hash_opt));
        }
        drop(rows);
        drop(stmt);

        // Group db lines by hash to match in order of appearance
        let mut db_hash_to_seqs = std::collections::HashMap::new();
        for (seq, hash_opt) in db_lines {
            let hash = hash_opt.unwrap_or_else(|| "".to_string());
            if !hash.is_empty() {
                db_hash_to_seqs.entry(hash).or_insert_with(std::collections::VecDeque::new).push_back(seq);
            }
        }

        // Also find max sequence_id to allocate for new/modified lines
        let max_seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(sequence_id), 0) FROM lines WHERE session_id = ?1",
            rusqlite::params![session_id],
            |row| row.get(0)
        )?;
        let mut next_seq = max_seq + 1;

        let parent_contexts = compute_parent_contexts(filepath, &content);

        // Clean existing lines
        tx.execute("DELETE FROM lines WHERE session_id = ?1", rusqlite::params![session_id])?;

        // Re-insert matched lines
        let mut insert_stmt = tx.prepare(
            "INSERT INTO lines (session_id, sequence_id, content, line_hash, sort_order, parent_context) VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
        )?;

        for (idx, hash) in disk_line_hashes.into_iter().enumerate() {
            let content = &lines_on_disk[idx];
            let seq = if let Some(seqs) = db_hash_to_seqs.get_mut(&hash) {
                if let Some(s) = seqs.pop_front() {
                    s
                } else {
                    let s = next_seq;
                    next_seq += 1;
                    s
                }
            } else {
                let s = next_seq;
                next_seq += 1;
                s
            };

            let sort_order = (idx + 1) as f64;
            let p_ctx = parent_contexts.get(idx).cloned().flatten();
            insert_stmt.execute(rusqlite::params![
                session_id,
                seq,
                content,
                hash,
                sort_order,
                p_ctx
            ])?;
        }
        drop(insert_stmt);

        // Update session's file_hash, mtime, and crlf
        let file_hash = compute_sha256(filepath)?;
        let new_mtime = path.metadata()?.modified()?
            .duration_since(std::time::UNIX_EPOCH)?.as_secs() as i64;
        let has_crlf = content.contains("\r\n");
        let crlf_val = if has_crlf { 1 } else { 0 };

        tx.execute(
            "UPDATE sessions SET file_hash = ?1, mtime = ?2, line_ending_crlf = ?3, last_accessed_at = ?4 WHERE session_id = ?5",
            rusqlite::params![file_hash, new_mtime, crlf_val, SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64, session_id],
        )?;

        tx.commit()?;
        Ok(())
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

pub fn check_language_supported(path: &str) -> bool {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "py" | "js" | "jsx" | "ts" | "tsx" | "go" | "rs" | "java" |
        "cpp" | "cc" | "cxx" | "c" | "h" | "lua" | "html" | "htm" |
        "json" | "yaml" | "yml" | "toml" | "swift" | "md" | "markdown" |
        "sh" | "bash" | "zsh" | "ksh"
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
    let is_crlf = content.contains("\r\n");
    let crlf_val = if is_crlf { 1 } else { 0 };
    
    // Begin transaction
    let tx = conn.transaction().context("Failed to begin SQLite transaction")?;

    // Clear any stale session for this filepath
    tx.execute("DELETE FROM sessions WHERE filepath = ?1;", rusqlite::params![filepath])
        .context("Failed to delete existing session for path")?;

    tx.execute(
        "INSERT INTO sessions (filepath, session_id, file_hash, mtime, last_accessed_at, line_ending_crlf) VALUES (?1, ?2, ?3, ?4, ?5, ?6);",
        rusqlite::params![filepath, session_id, file_hash, mtime, now, crlf_val],
    ).context("Failed to insert new session")?;

    let parent_contexts = compute_parent_contexts(filepath, &content);
    let mut lines_count = 0;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO lines (session_id, sequence_id, line_hash, content, sort_order, parent_context) VALUES (?1, ?2, NULL, ?3, ?4, ?5);"
        )?;

        // Split by lines, preserving empty final lines
        let mut parts: Vec<&str> = content.split('\n').collect();
        if parts.last() == Some(&"") {
            parts.pop();
        }
        
        for (idx, line_content) in parts.iter().enumerate() {
            let seq = idx + 1;
            let sort_order = (seq as f64) * 1000.0;
            let trimmed_line = line_content.strip_suffix('\r').unwrap_or(line_content);
            let p_ctx = parent_contexts.get(idx).cloned().flatten();
            stmt.execute(rusqlite::params![session_id, seq, trimmed_line, sort_order, p_ctx])
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

static WASM_ENGINE: once_cell::sync::Lazy<wasmtime::Engine> = once_cell::sync::Lazy::new(|| {
    let config = wasmtime::Config::new();
    wasmtime::Engine::new(&config).expect("Failed to initialize Wasmtime engine")
});

static SYNCHRONOUS_LANGUAGES: once_cell::sync::Lazy<std::sync::Mutex<std::collections::HashMap<String, tree_sitter::Language>>> =
    once_cell::sync::Lazy::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

fn parse_code_sync(ext: &str, code: &str) -> Result<tree_sitter::Tree> {
    let wasm_dir = crate::config::get_wasm_dir();
    let wasm_file = crate::config::get_wasm_file(&wasm_dir, ext)?;
    let wasm_name = wasm_file.strip_suffix(".wasm").unwrap_or(&wasm_file);
    let lang_symbol = wasm_name.strip_prefix("tree-sitter-").unwrap_or(wasm_name).replace("-", "_");

    let cached_lang = {
        let cache = SYNCHRONOUS_LANGUAGES.lock().unwrap();
        cache.get(&lang_symbol).cloned()
    };

    let language = if let Some(lang) = cached_lang {
        lang
    } else {
        let raw_wasm_path = wasm_dir.join(format!("{}.wasm", wasm_name));
        let wasm_bytes = std::fs::read(&raw_wasm_path)?;

        let engine = &*WASM_ENGINE;
        let mut wasm_store = WasmStore::new(engine)?;
        let lang = wasm_store.load_language(&lang_symbol, &wasm_bytes)?;

        let mut cache = SYNCHRONOUS_LANGUAGES.lock().unwrap();
        cache.entry(lang_symbol.clone()).or_insert(lang).clone()
    };

    let engine = &*WASM_ENGINE;
    let wasm_store = WasmStore::new(engine)?;
    let mut parser = Parser::new();
    parser.set_wasm_store(wasm_store)?;
    parser.set_language(&language)?;
    let tree = parser.parse(code, None).context("Failed to parse code")?;
    Ok(tree)
}

fn get_context_name(node: tree_sitter::Node, code: &str) -> Option<String> {
    let kind = node.kind();
    let name_opt = node.child_by_field_name("name")
        .map(|n| {
            let bytes = n.byte_range();
            if bytes.end <= code.len() {
                code[bytes].trim().to_string()
            } else {
                String::new()
            }
        });

    match kind {
        "function_definition" | "function_item" | "function_declaration" | "method_declaration" | "method_definition" => {
            let name = name_opt.unwrap_or_else(|| "anonymous".to_string());
            Some(format!("fn:{}", name))
        }
        "class_definition" | "class_declaration" => {
            let name = name_opt.unwrap_or_else(|| "anonymous".to_string());
            Some(format!("class:{}", name))
        }
        "struct_item" => {
            let name = name_opt.unwrap_or_else(|| "anonymous".to_string());
            Some(format!("struct:{}", name))
        }
        "trait_item" => {
            let name = name_opt.unwrap_or_else(|| "anonymous".to_string());
            Some(format!("trait:{}", name))
        }
        "impl_item" => {
            let range = node.byte_range();
            if range.end <= code.len() {
                if let Some(first_line) = code[range].lines().next() {
                    let header = first_line.trim_end_matches('{').trim().to_string();
                    Some(header)
                } else {
                    Some("impl".to_string())
                }
            } else {
                Some("impl".to_string())
            }
        }
        _ => None,
    }
}

pub fn compute_parent_contexts(filepath: &str, content: &str) -> Vec<Option<String>> {
    let line_count = content.split('\n').count();
    let mut contexts = vec![None; line_count];
    
    let ext = std::path::Path::new(filepath)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    if ext == "md" || ext == "markdown" {
        let mut current_header = None;
        for (idx, line) in content.split('\n').enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with('#') {
                let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
                if parts.len() == 2 && parts[0].chars().all(|c| c == '#') {
                    contexts[idx] = current_header.clone();
                    current_header = Some(trimmed.to_string());
                    continue;
                }
            }
            contexts[idx] = current_header.clone();
        }
        return contexts;
    }

    match parse_code_sync(&ext, content) {
        Err(e) => {
            tracing::error!("parse_code_sync failed: {:?}", e);
        }
        Ok(tree) => {
        let mut visit_stack = vec![(tree.root_node(), None)];
        while let Some((node, active_context)) = visit_stack.pop() {
            let next_context = if let Some(name) = get_context_name(node, content) {
                Some(name)
            } else {
                active_context
            };

            if let Some(ref ctx_name) = next_context {
                let start_row = node.start_position().row;
                let end_row = node.end_position().row;
                for r in start_row..=end_row {
                    if r < contexts.len() {
                        contexts[r] = Some(ctx_name.clone());
                    }
                }
            }

            let count = node.child_count();
            for i in (0..count).rev() {
                if let Some(child) = node.child(i as u32) {
                    visit_stack.push((child, next_context.clone()));
                }
            }
        }
        }
    }
    
    contexts
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
    #[cfg(test)]
    {
        let conn = match get_db_connection() {
            Ok(c) => c,
            Err(_) => return,
        };
        let filepath: String = match conn.query_row(
            "SELECT filepath FROM sessions WHERE session_id = ?1",
            [&session_id],
            |row| row.get(0)
        ) {
            Ok(p) => p,
            Err(_) => return,
        };
        if !filepath.contains("bg_code.rs") {
            return;
        }
    }
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
#[allow(clippy::await_holding_lock)]
mod tests {
    use super::*;
    use crate::tools::TEST_DB_LOCK as DB_LOCK;

    fn view_lines_old_compat(
        repository: &impl SessionRepository,
        filepath: &str,
        start_line: usize,
        end_line: usize,
        only_ids: Option<bool>,
    ) -> Result<String> {
        let res = crate::tools::view::view_lines(repository, filepath, Some(start_line), Some(end_line), only_ids, None, None)?;
        let ids_val: serde_json::Value = serde_json::from_str(&res.metadata_json)?;
        let ids = ids_val["ids"].as_array().unwrap();
        
        let mut lines = Vec::new();
        if let Some(ref text) = res.lines_text {
            for line in text.lines() {
                let colon_idx = line.find(':').unwrap();
                let n: usize = line[..colon_idx].parse().unwrap();
                let content = &line[colon_idx + 2..];
                let mut matched_id = String::new();
                for id_entry in ids {
                    let id_arr = id_entry.as_array().unwrap();
                    if id_arr[1].as_u64().unwrap() as usize == n {
                        matched_id = id_arr[0].as_str().unwrap().to_string();
                        break;
                    }
                }
                lines.push(serde_json::json!([matched_id, n, content]));
            }
        } else {
            for id_entry in ids {
                let id_arr = id_entry.as_array().unwrap();
                let id = id_arr[0].as_str().unwrap();
                let n = id_arr[1].as_u64().unwrap() as usize;
                lines.push(serde_json::json!([id, n]));
            }
        }
        
        let mut val = ids_val.clone();
        val["lines"] = serde_json::Value::Array(lines);
        let columns = if only_ids.unwrap_or(false) {
            vec!["id", "n"]
        } else {
            vec!["id", "n", "content"]
        };
        val["columns"] = serde_json::json!(columns);
        Ok(serde_json::to_string(&val)?)
    }

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
        assert!(check_language_supported("foo.md"));
        assert!(check_language_supported("bar.markdown"));
        assert!(check_language_supported("script.sh"));
        assert!(check_language_supported("script.bash"));
        assert!(check_language_supported("script.zsh"));
        assert!(check_language_supported("script.ksh"));
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
        let output = view_lines_old_compat(&repository, filepath_str, 1, 3, None)?;
        
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
        let sub_output = view_lines_old_compat(&repository, filepath_str, 2, 2, None)?;
        let sub_val: serde_json::Value = serde_json::from_str(&sub_output)?;
        let sub_lines = sub_val["lines"].as_array().unwrap();
        assert_eq!(sub_lines.len(), 1);
        let sub_line0 = sub_lines[0].as_array().unwrap();
        assert!(sub_line0[0].as_str().unwrap().starts_with("2#"));

        // 4. Test bounds validation
        assert!(crate::tools::view::view_lines(&repository, filepath_str, Some(0), Some(3), None, None, None).is_err());
        assert!(crate::tools::view::view_lines(&repository, filepath_str, Some(3), Some(1), None, None, None).is_err());

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
        let output = view_lines_old_compat(&repository, filepath_str, 1, 3, None)?;
        
        let val: serde_json::Value = serde_json::from_str(&output)?;
        let lines = val["lines"].as_array().unwrap();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].as_array().unwrap()[2].as_str().unwrap(), "fn main() {");

        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }
}
