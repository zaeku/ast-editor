use anyhow::{Result, Context, bail};
use rusqlite::{Connection, OptionalExtension};
use serde::{Serialize, Deserialize};
use std::path::PathBuf;
use std::env;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};
use tree_sitter::{WasmStore, Parser};

pub fn get_db_path() -> Result<PathBuf> {
    let mut path = if let Some(dir) = env::var_os("AST_EDITOR_CACHE_DIR") {
        PathBuf::from(dir)
    } else if cfg!(test) {
        // Only reaches unit tests: the tests/ crate links this library built
        // without the flag, so integration tests set the variable above.
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
    
    conn.execute(
        "CREATE TABLE IF NOT EXISTS previews (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            filepath TEXT NOT NULL,
            file_hash TEXT NOT NULL,
            edits_json TEXT NOT NULL,
            created_at INTEGER NOT NULL
        );",
        [],
    ).context("Failed to create previews table")?;

    // Add crlf column if not present
    let _ = conn.execute("ALTER TABLE sessions ADD COLUMN line_ending_crlf INTEGER DEFAULT 0;", []);
    // Add parent_context column to lines if not present
    let _ = conn.execute("ALTER TABLE lines ADD COLUMN parent_context TEXT;", []);
    // Content used to be cached here. Drop it, and with it every row built
    // under the old schema: line_hash was four characters wide then, and the
    // index rebuilds itself from disk on next access anyway.
    if conn.execute("ALTER TABLE lines DROP COLUMN content;", []).is_ok() {
        conn.execute("DELETE FROM lines;", []).context("Failed to clear the pre-index line cache")?;
    }

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
        // The id carries the surfaced prefix of the stored hash.
        let mut stmt = db_conn.prepare(
            "SELECT sequence_id, substr(line_hash, 1, 4) FROM lines WHERE session_id = ?1 AND substr(line_hash, 1, 4) = ?2"
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

/// One line of a file being edited, paired with the sequence number that
/// identifies it. Position in `LineBuffer::lines` is the line's order.
#[derive(Clone)]
pub struct BufLine {
    pub seq: i64,
    pub content: String,
    pub parent_context: Option<String>,
}

/// A file's lines held in memory for the duration of one edit batch. Edits
/// mutate the buffer; nothing reaches the database or the disk until the
/// caller decides the result is good.
pub struct LineBuffer {
    pub lines: Vec<BufLine>,
    next_seq: i64,
}

/// Split an op's `content` field into the lines it should insert, matching the
/// behaviour edits have always had: a single trailing newline is dropped, and
/// empty content still inserts one empty line.
fn split_insert_content(content: &str) -> Vec<String> {
    let mut parts: Vec<&str> = if content.is_empty() { vec![""] } else { content.split('\n').collect() };
    if parts.last() == Some(&"") {
        parts.pop();
    }
    if parts.is_empty() {
        parts.push("");
    }
    parts.iter().map(|line| line.strip_suffix('\r').unwrap_or(line).to_string()).collect()
}

fn strip_line_ending(content: &str) -> String {
    let trimmed = content.strip_suffix('\n').unwrap_or(content);
    trimmed.strip_suffix('\r').unwrap_or(trimmed).to_string()
}

impl LineBuffer {
    pub fn new(lines: Vec<BufLine>) -> Self {
        let next_seq = lines.iter().map(|l| l.seq).max().unwrap_or(0) + 1;
        Self { lines, next_seq }
    }

    fn take_seq(&mut self) -> i64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        seq
    }

    pub fn join(&self, line_ending: &str) -> String {
        self.lines.iter().map(|l| l.content.as_str()).collect::<Vec<_>>().join(line_ending) + line_ending
    }

    /// Resolve a target id to a position, verifying the caller's view of the
    /// line still matches. A bare hash with no sequence prefix is resolved by
    /// searching the buffer, and must match exactly one line.
    fn position_of(&self, target_id: &str, role: &str, field: &str) -> Result<usize> {
        if target_id.contains('#') {
            let (seq, hash) = parse_line_id(target_id)?;
            let idx = self.lines.iter().position(|l| l.seq == seq)
                .with_context(|| format!("Target line not found for target_id={}", target_id))?;
            let actual = compute_line_hash(&self.lines[idx].content);
            if actual != hash {
                bail!("CHECKSUM_ERROR: {} line hash mismatch for {}={}. Edit rejected.", role, field, target_id);
            }
            Ok(idx)
        } else {
            let matches: Vec<usize> = self.lines.iter().enumerate()
                .filter(|(_, l)| compute_line_hash(&l.content) == target_id)
                .map(|(idx, _)| idx)
                .collect();
            match matches.len() {
                0 => bail!("CHECKSUM_ERROR: Target line hash '{}' not found in session", target_id),
                1 => Ok(matches[0]),
                _ => bail!("CHECKSUM_ERROR: Target line hash '{}' is ambiguous (matches multiple lines in session)", target_id),
            }
        }
    }

    fn id_at(&self, idx: usize) -> String {
        format!("{:x}#{}", self.lines[idx].seq, compute_line_hash(&self.lines[idx].content))
    }

    fn insert_at(&mut self, idx: usize, contents: Vec<String>, modified: &mut Vec<String>) {
        for (offset, content) in contents.into_iter().enumerate() {
            let seq = self.take_seq();
            self.lines.insert(idx + offset, BufLine { seq, content, parent_context: None });
            modified.push(self.id_at(idx + offset));
        }
    }

    /// Apply one batch of edits in order, returning the ids the batch touched.
    pub fn apply(&mut self, edits: &[LineEdit]) -> Result<Vec<String>> {
        let mut modified = Vec::new();

        for edit in edits {
            match edit.op {
                EditOp::InsertAfter | EditOp::InsertBefore | EditOp::Append | EditOp::Prepend => {
                    let contents = split_insert_content(edit.content.as_deref().unwrap_or(""));
                    let no_target = edit.target_id.as_ref().map_or(true, |s| s.is_empty());

                    let at = if edit.op == EditOp::Append || (edit.op == EditOp::InsertAfter && no_target) {
                        self.lines.len()
                    } else if edit.op == EditOp::Prepend || (edit.op == EditOp::InsertBefore && no_target) {
                        0
                    } else {
                        let target_id = edit.target_id.as_ref().filter(|s| !s.is_empty())
                            .with_context(|| format!("CHECKSUM_ERROR: Missing target_id for {:?}", edit.op))?;
                        let idx = self.position_of(target_id, "Target", "target_id")?;
                        if edit.op == EditOp::InsertAfter { idx + 1 } else { idx }
                    };

                    self.insert_at(at, contents, &mut modified);
                }

                EditOp::Update => {
                    let target_id = edit.target_id.as_ref().context("Missing target_id for update op")?;
                    let content = edit.content.as_ref().context("Missing content for update op")?;
                    let idx = self.position_of(target_id, "Target", "target_id")?;
                    self.lines[idx].content = strip_line_ending(content);
                    modified.push(self.id_at(idx));
                }

                EditOp::ReplaceSubstring => {
                    let target_id = edit.target_id.as_ref().context("Missing target_id for replace_substring op")?;
                    let pattern = edit.pattern.as_ref().context("Missing pattern for replace_substring op")?;
                    let replacement = edit.replacement.as_ref().context("Missing replacement for replace_substring op")?;
                    let occurrence = edit.occurrence.unwrap_or(1);
                    if occurrence < 1 {
                        bail!("Invalid occurrence number: {}. Must be >= 1.", occurrence);
                    }
                    let idx = self.position_of(target_id, "Target", "target_id")?;

                    let current = &self.lines[idx].content;
                    let start = current.match_indices(pattern.as_str()).nth(occurrence - 1)
                        .map(|(pos, _)| pos)
                        .with_context(|| format!(
                            "CHECKSUM_ERROR: Pattern '{}' (occurrence {}) not found in target line.",
                            pattern, occurrence
                        ))?;

                    let mut updated = String::with_capacity(current.len());
                    updated.push_str(&current[..start]);
                    updated.push_str(replacement);
                    updated.push_str(&current[start + pattern.len()..]);
                    self.lines[idx].content = updated;
                    modified.push(self.id_at(idx));
                }

                EditOp::Delete => {
                    let target_id = edit.target_id.as_ref().context("Missing target_id for delete op")?;
                    let idx = self.position_of(target_id, "Target", "target_id")?;
                    self.lines.remove(idx);

                    // Report the line now nearest the hole, so the agent has a
                    // live id to anchor its next edit on.
                    if !self.lines.is_empty() {
                        let neighbour = if idx < self.lines.len() { idx } else { self.lines.len() - 1 };
                        modified.push(self.id_at(neighbour));
                    }
                }

                EditOp::ReplaceRange => {
                    let start_id = edit.target_id.as_ref().filter(|s| !s.is_empty())
                        .context("CHECKSUM_ERROR: Missing target_id (start_id) for replace_range op")?;
                    let end_id = edit.end_target_id.as_ref().filter(|s| !s.is_empty())
                        .context("CHECKSUM_ERROR: Missing end_target_id for replace_range op")?;

                    let start_idx = self.position_of(start_id, "Start", "target_id")?;
                    let end_idx = self.position_of(end_id, "End", "end_target_id")?;
                    if start_idx > end_idx {
                        bail!("VALIDATION_ERROR: start_id sort order is after end_id sort order for replace_range.");
                    }

                    let contents = split_insert_content(edit.content.as_deref().unwrap_or(""));
                    self.lines.drain(start_idx..=end_idx);
                    self.insert_at(start_idx, contents, &mut modified);
                }

                EditOp::Move => {
                    let start_id = edit.target_id.as_ref().filter(|s| !s.is_empty())
                        .context("CHECKSUM_ERROR: Missing target_id (start_id) for move op")?;
                    let end_id = edit.end_target_id.as_ref().filter(|s| !s.is_empty()).unwrap_or(start_id);
                    let move_pos = edit.move_position.unwrap_or(MovePosition::After);

                    let start_idx = self.position_of(start_id, "Start", "target_id")?;
                    let end_idx = self.position_of(end_id, "End", "end_target_id")?;
                    if start_idx > end_idx {
                        bail!("VALIDATION_ERROR: start_id sort order is after end_id sort order for move.");
                    }

                    // Where the block lands is decided against the lines that
                    // stay put, so resolve the destination before lifting it.
                    let dest_idx = match move_pos {
                        MovePosition::Prepend | MovePosition::Append => None,
                        MovePosition::Before | MovePosition::After => {
                            let dest_id = edit.dest_target_id.as_ref().filter(|s| !s.is_empty())
                                .context("CHECKSUM_ERROR: Missing dest_target_id for move before/after operation")?;
                            let idx = self.position_of(dest_id, "Destination", "dest_target_id")?;
                            if idx >= start_idx && idx <= end_idx {
                                bail!("VALIDATION_ERROR: Cannot move a range into itself (dest_target_id lies within source range).");
                            }
                            Some(idx)
                        }
                    };

                    let block: Vec<BufLine> = self.lines.drain(start_idx..=end_idx).collect();
                    let moved = block.len();

                    let at = match (move_pos, dest_idx) {
                        (MovePosition::Prepend, _) => 0,
                        (MovePosition::Append, _) => self.lines.len(),
                        (pos, Some(dest)) => {
                            // The drain shifted every position after the block.
                            let dest = if dest > end_idx { dest - moved } else { dest };
                            if pos == MovePosition::Before { dest } else { dest + 1 }
                        }
                        (_, None) => unreachable!("before/after always resolve a destination"),
                    };

                    for (offset, line) in block.into_iter().enumerate() {
                        self.lines.insert(at + offset, line);
                        modified.push(self.id_at(at + offset));
                    }
                }
            }
        }

        Ok(modified)
    }
}

/// The path a session tracks. Content lives on disk, so most operations need
/// to get back to it from a session id alone.
fn session_filepath(conn: &Connection, session_id: &str) -> Result<String> {
    conn.query_row(
        "SELECT filepath FROM sessions WHERE session_id = ?1",
        [session_id],
        |row| row.get(0),
    ).with_context(|| format!("No session found for session_id={}", session_id))
}

/// Read a session's file and pair each line with the identity the index holds
/// for it. The index stores order and identity; the text comes from disk.
///
/// If the index and the file disagree on length the index is stale, and the
/// caller is served a fresh one built from disk — the same rebuild the tool
/// has always fallen back to. Phase 2 replaces this with a real reconcile.
fn load_buffer(conn: &Connection, session_id: &str) -> Result<LineBuffer> {
    let filepath = session_filepath(conn, session_id)?;
    let content = fs::read_to_string(&filepath)
        .with_context(|| format!("Failed to read file for line buffer: {}", filepath))?;

    let mut parts: Vec<&str> = content.split('\n').collect();
    if parts.last() == Some(&"") {
        parts.pop();
    }
    let disk: Vec<String> = parts.iter().map(|l| l.strip_suffix('\r').unwrap_or(l).to_string()).collect();

    let index: Vec<(i64, Option<String>)> = {
        let mut stmt = conn.prepare(
            "SELECT sequence_id, parent_context FROM lines WHERE session_id = ?1 ORDER BY sort_order"
        )?;
        let rows = stmt.query_map([session_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };

    let lines = if index.len() == disk.len() {
        disk.into_iter().zip(index).map(|(content, (seq, parent_context))| BufLine {
            seq,
            content,
            parent_context,
        }).collect()
    } else {
        disk.into_iter().enumerate().map(|(idx, content)| BufLine {
            seq: idx as i64 + 1,
            content,
            parent_context: None,
        }).collect()
    };

    Ok(LineBuffer::new(lines))
}

pub trait SessionRepository: Send + Sync {
    fn init_session(&self, filepath: &str, is_binary: bool) -> Result<SessionMetadata>;
    fn get_total_lines(&self, session_id: &str) -> Result<usize>;
    fn fetch_lines_range(
        &self,
        session_id: &str,
        start_line: usize,
        end_line: usize,
    ) -> Result<Vec<(i64, Option<String>, String)>>;
    fn get_line_content(&self, session_id: &str, sequence_id: i64) -> Result<Option<String>>;
    /// Work out what a batch would produce, touching nothing. Returns the
    /// resulting buffer and the ids the batch would report.
    fn plan_line_edits(&self, session_id: &str, edits: &[LineEdit]) -> Result<(LineBuffer, Vec<String>)>;
    /// Persist a planned buffer as the session's new index. Call only once the
    /// buffer's content has been accepted and written to disk.
    fn commit_buffer(&self, session_id: &str, buffer: &LineBuffer) -> Result<()>;
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
    fn create_preview(&self, filepath: &str, edits: &[LineEdit]) -> Result<String>;
    fn take_preview(&self, filepath: &str, preview_id: &str) -> Result<Vec<LineEdit>>;
}

/// Previews older than this are pruned whenever a new one is stored. A preview
/// is only useful for as long as the file it was computed against is unchanged.
const PREVIEW_TTL_SECONDS: i64 = 3600;

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
        let buffer = load_buffer(&conn, session_id)?;
        Ok(buffer.lines.iter().enumerate()
            .filter(|(_, line)| line.content.contains(query))
            .map(|(idx, _)| idx + 1)
            .collect())
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

    fn fetch_lines_range(
        &self,
        session_id: &str,
        start_line: usize,
        end_line: usize,
    ) -> Result<Vec<(i64, Option<String>, String)>> {
        let conn = get_db_connection()?;
        let buffer = load_buffer(&conn, session_id)?;
        let from = start_line.saturating_sub(1);
        let to = std::cmp::min(end_line, buffer.lines.len());
        if from >= to {
            return Ok(Vec::new());
        }
        Ok(buffer.lines[from..to].iter()
            .map(|line| (line.seq, Some(compute_line_hash(&line.content)), line.content.clone()))
            .collect())
    }

    fn get_line_content(&self, session_id: &str, sequence_id: i64) -> Result<Option<String>> {
        let conn = get_db_connection()?;
        let buffer = load_buffer(&conn, session_id)?;
        Ok(buffer.lines.iter().find(|line| line.seq == sequence_id).map(|line| line.content.clone()))
    }

    fn plan_line_edits(&self, session_id: &str, edits: &[LineEdit]) -> Result<(LineBuffer, Vec<String>)> {
        let conn = get_db_connection()?;
        let mut buffer = load_buffer(&conn, session_id)?;
        let modified_ids = buffer.apply(edits)?;
        Ok((buffer, modified_ids))
    }

    fn commit_buffer(&self, session_id: &str, buffer: &LineBuffer) -> Result<()> {
        let mut conn = get_db_connection()?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM lines WHERE session_id = ?1", [session_id])?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO lines (session_id, sequence_id, line_hash, sort_order, parent_context) VALUES (?1, ?2, ?3, ?4, ?5);"
            )?;
            for (idx, line) in buffer.lines.iter().enumerate() {
                stmt.execute(rusqlite::params![
                    session_id,
                    line.seq,
                    compute_stored_hash(&line.content),
                    ((idx + 1) as f64) * 1000.0,
                    line.parent_context,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn create_preview(&self, filepath: &str, edits: &[LineEdit]) -> Result<String> {
        let conn = get_db_connection()?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;

        conn.execute("DELETE FROM previews WHERE created_at < ?1", [now - PREVIEW_TTL_SECONDS])?;

        conn.execute(
            "INSERT INTO previews (filepath, file_hash, edits_json, created_at) VALUES (?1, ?2, ?3, ?4);",
            rusqlite::params![filepath, compute_sha256(filepath)?, serde_json::to_string(edits)?, now],
        )?;

        Ok(format!("p{:x}", conn.last_insert_rowid()))
    }

    fn take_preview(&self, filepath: &str, preview_id: &str) -> Result<Vec<LineEdit>> {
        let rowid = preview_id.strip_prefix('p')
            .and_then(|digits| i64::from_str_radix(digits, 16).ok())
            .with_context(|| format!("Invalid preview id '{}'. Preview ids look like 'p1f'.", preview_id))?;

        let conn = get_db_connection()?;
        let row = conn.query_row(
            "SELECT filepath, file_hash, edits_json FROM previews WHERE id = ?1",
            [rowid],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)),
        ).optional()?;

        let (preview_path, file_hash, edits_json) = row.with_context(|| format!(
            "Preview '{}' is unknown, already applied, or expired. Run edit_lines with dry_run again to get a fresh preview.",
            preview_id
        ))?;

        if preview_path != filepath {
            bail!(
                "Preview '{}' belongs to {}, not {}. Apply it against the file it was previewed on.",
                preview_id, preview_path, filepath
            );
        }

        if compute_sha256(filepath)? != file_hash {
            conn.execute("DELETE FROM previews WHERE id = ?1", [rowid])?;
            bail!(
                "PREVIEW_STALE: {} changed since preview '{}' was taken, so its diff and syntax check no longer describe the result. Run dry_run again.",
                filepath, preview_id
            );
        }

        conn.execute("DELETE FROM previews WHERE id = ?1", [rowid])?;
        Ok(serde_json::from_str(&edits_json)?)
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

        // Stored hashes carry the sequence numbers across the resync; the
        // surfaced prefixes answer whether a targeted line survived, since
        // that is the width the caller's target_id carries.
        let mut disk_line_hashes = Vec::with_capacity(lines_on_disk.len());
        let mut disk_hash_counts = std::collections::HashMap::with_capacity(lines_on_disk.len());
        for line in &lines_on_disk {
            let hash = compute_stored_hash(line);
            *disk_hash_counts.entry(hash[..4].to_string()).or_insert(0) += 1;
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
            "INSERT INTO lines (session_id, sequence_id, line_hash, sort_order, parent_context) VALUES (?1, ?2, ?3, ?4, ?5)"
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
                compute_stored_hash(content),
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
            "INSERT INTO lines (session_id, sequence_id, line_hash, sort_order, parent_context) VALUES (?1, ?2, ?3, ?4, ?5);"
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
            stmt.execute(rusqlite::params![session_id, seq, compute_stored_hash(trimmed_line), sort_order, p_ctx])
                .context("Failed to insert line")?;
            lines_count += 1;
        }
    }

    tx.commit().context("Failed to commit SQLite transaction")?;

    Ok(SessionMetadata {
        session_id,
        total_lines: lines_count,
        file_hash,
        mtime,
        is_supported,
        warning_message,
    })
}

/// The hash kept in the index. Reconciliation aligns disk lines against stored
/// rows before any sequence number is known, so this is the only key available
/// there and needs enough width that a file's lines do not collide.
pub fn compute_stored_hash(content: &str) -> String {
    use sha1::{Sha1, Digest};
    let mut hasher = Sha1::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())[..16].to_string()
}

/// The hash surfaced to agents inside `{sequence_id:x}#{hash}`. It is a prefix
/// of the stored hash, so the short form can be checked against a stored row
/// without keeping a second column. The sequence number does the identifying
/// here, which is why four characters are enough.
pub fn compute_line_hash(content: &str) -> String {
    compute_stored_hash(content)[..4].to_string()
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
        
        // The store holds no text, so the rebuilt session is checked through
        // the buffer the repository serves from disk.
        let repository = SqliteSessionRepository;
        let lines: Vec<String> = repository
            .fetch_lines_range(&metadata3.session_id, 1, metadata3.total_lines)?
            .into_iter()
            .map(|(_, _, content)| content)
            .collect();
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
    fn test_surfaced_hash_is_a_prefix_of_the_stored_hash() {
        // The index keeps the wide hash; ids carry its first four characters,
        // so a short id can be checked against a stored row directly.
        for content in ["", "fn main() {", "    let a = 1;", "}"] {
            let stored = compute_stored_hash(content);
            assert_eq!(stored.len(), 16);
            assert_eq!(compute_line_hash(content), stored[..4]);
        }
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
