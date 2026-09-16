use anyhow::{bail, Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
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

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone)]
pub(crate) struct SessionMetadata {
    pub session_id: String,
    pub total_lines: usize,
    pub file_hash: String,
    pub mtime: i64,
    pub is_supported: bool,
    pub warning_message: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EditOp {
    #[default]
    InsertAfter,
    InsertBefore,
    Append,
    Prepend,
    Replace,
    Delete,
    ReplaceSubstring,
    Move,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub(crate) enum MovePosition {
    Prepend,
    Append,
    Before,
    #[default]
    After,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub(crate) struct LineEdit {
    pub op: EditOp,
    pub start_id: Option<String>,
    pub end_id: Option<String>,
    pub dest_id: Option<String>,
    pub move_position: Option<MovePosition>,
    pub content: Option<String>,
    pub pattern: Option<String>,
    pub replacement: Option<String>,
    pub occurrence: Option<usize>,
}

/// An op's name as a caller spells it, which is what `serde` renamed it to.
pub(crate) fn op_name(op: EditOp) -> String {
    serde_json::to_value(op)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{op:?}"))
}

/// What to say when an edit does not carry a field its op needs. The op and the
/// field are named as a caller spells them, and so is what the edit did carry,
/// because the mistake is usually a field in the wrong place rather than a
/// field nobody thought of.
pub(crate) fn missing_field(edit: &LineEdit, needs: &str) -> anyhow::Error {
    let mut carried: Vec<&str> = Vec::new();
    let filled = |value: &Option<String>| value.as_deref().is_some_and(|s| !s.is_empty());
    if filled(&edit.start_id) {
        carried.push("start_id");
    }
    if filled(&edit.end_id) {
        carried.push("end_id");
    }
    if filled(&edit.dest_id) {
        carried.push("dest_id");
    }
    if edit.content.is_some() {
        carried.push("content");
    }
    if filled(&edit.pattern) {
        carried.push("pattern");
    }
    if filled(&edit.replacement) {
        carried.push("replacement");
    }
    let carried = if carried.is_empty() {
        "no other field".to_string()
    } else {
        carried.join(", ")
    };
    let op = op_name(edit.op);
    anyhow::anyhow!(
        "{}",
        crate::tools::metadata::get_config()
            .error_edit_missing_field
            .replacen("{}", &op, 1)
            .replacen("{}", needs, 1)
            .replacen("{}", &carried, 1)
    )
}

pub(crate) fn parse_line_id(id_str: &str) -> Result<(i64, String)> {
    let parts: Vec<&str> = id_str.split('#').collect();
    if parts.len() == 1 {
        anyhow::bail!(crate::tools::metadata::get_config()
            .error_address_needs_a_number
            .replacen("{}", id_str, 1));
    }
    if parts.len() != 2 {
        anyhow::bail!("Invalid Line ID format: {}", id_str);
    }
    let seq = i64::from_str_radix(parts[0], 16).context("Failed to parse sequence ID hex")?;
    Ok((seq, parts[1].to_string()))
}

/// Reconcile a session's index with the file on disk.
///
/// Lines that survived keep their sequence number; only genuinely new lines
/// get a fresh one. Alignment comes from a patience diff over the stored
/// hashes, which anchors on lines unique to both sides — code is dense with
/// duplicates (`}`, blank lines) that a plain longest-common-subsequence
/// mis-pairs.
///
/// `start_ids` name lines the caller is about to edit. If any of them did not
/// survive, the edit cannot be applied safely and this fails instead of
/// guessing which line was meant.
pub(crate) fn reconcile_index(
    conn: &mut Connection,
    session_id: &str,
    filepath: &str,
    start_ids: &[String],
) -> Result<usize> {
    let current: Option<(String, i64, usize)> = conn.query_row(
        "SELECT file_hash, mtime, (SELECT COUNT(*) FROM lines WHERE session_id = ?1) FROM sessions WHERE session_id = ?1",
        [session_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).optional()?;
    let disk_mtime = std::path::Path::new(filepath)
        .metadata()?
        .modified()?
        .duration_since(UNIX_EPOCH)?
        .as_secs() as i64;
    let disk_file_hash = compute_sha256(filepath)?;

    let mut targeted = Vec::new();
    for id in start_ids {
        if !id.is_empty() {
            targeted.push(parse_line_id(id)?);
        }
    }

    let content = fs::read_to_string(filepath)
        .with_context(|| format!("Failed to read file for reconcile: {}", filepath))?;
    let mut parts: Vec<&str> = content.split('\n').collect();
    if parts.last() == Some(&"") {
        parts.pop();
    }
    let disk: Vec<&str> = parts
        .iter()
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    let disk_hashes: Vec<String> = disk.iter().map(|line| compute_stored_hash(line)).collect();

    // Nothing to reconcile if the file still looks the way the index last saw
    // it *and* the index still covers it. Matching hashes alone are not enough:
    // an index that is short of the file must be rebuilt rather than trusted.
    if let Some((stored_hash, stored_mtime, line_count)) = &current {
        if *stored_hash == disk_file_hash
            && *stored_mtime == disk_mtime
            && *line_count == disk.len()
        {
            return Ok(*line_count);
        }
    }

    let index: Vec<(i64, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT sequence_id, COALESCE(line_hash, ''), COALESCE(norm_hash, '') FROM lines WHERE session_id = ?1 ORDER BY sort_order"
        )?;
        let rows = stmt.query_map([session_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let index_hashes: Vec<&str> = index.iter().map(|(_, hash, _)| hash.as_str()).collect();
    let disk_refs: Vec<&str> = disk_hashes.iter().map(String::as_str).collect();

    let mut assigned: Vec<Option<i64>> = vec![None; disk.len()];
    let mut retired: HashMap<&str, VecDeque<i64>> = HashMap::new();
    let mut respaced: HashMap<&str, VecDeque<i64>> = HashMap::new();

    for op in similar::capture_diff_slices(similar::Algorithm::Patience, &index_hashes, &disk_refs)
    {
        let (old_range, new_range) = (op.old_range(), op.new_range());
        match op.tag() {
            similar::DiffTag::Equal => {
                for (old_idx, new_idx) in old_range.zip(new_range) {
                    assigned[new_idx] = Some(index[old_idx].0);
                }
            }
            similar::DiffTag::Delete | similar::DiffTag::Replace => {
                for old_idx in old_range {
                    retired
                        .entry(index_hashes[old_idx])
                        .or_default()
                        .push_back(index[old_idx].0);
                    respaced
                        .entry(index[old_idx].2.as_str())
                        .or_default()
                        .push_back(index[old_idx].0);
                }
            }
            similar::DiffTag::Insert => {}
        }
    }

    // A moved line reads as a delete plus an insert. Where an inserted line
    // matches a retired one exactly, it is that same line in a new place, so
    // it keeps its identity.
    for (new_idx, slot) in assigned.iter_mut().enumerate() {
        if slot.is_none() {
            if let Some(queue) = retired.get_mut(disk_hashes[new_idx].as_str()) {
                *slot = queue.pop_front();
            }
        }
    }

    // Then the same pass ignoring whitespace, so a formatter respacing the
    // file leaves every line holding the id the agent already has.
    let disk_norms: Vec<String> = disk
        .iter()
        .map(|line| compute_normalized_hash(line))
        .collect();
    for (new_idx, slot) in assigned.iter_mut().enumerate() {
        if slot.is_none() {
            if let Some(queue) = respaced.get_mut(disk_norms[new_idx].as_str()) {
                *slot = queue.pop_front();
            }
        }
    }

    let stored_next: i64 = conn
        .query_row(
            "SELECT next_line_id FROM sessions WHERE session_id = ?1",
            [session_id],
            |row| row.get(0),
        )
        .unwrap_or(1);
    let highest = index.iter().map(|(seq, _, _)| *seq).max().unwrap_or(0);
    let mut next_seq = std::cmp::max(stored_next, highest + 1);
    let seqs: Vec<i64> = assigned
        .into_iter()
        .map(|slot| {
            slot.unwrap_or_else(|| {
                let seq = next_seq;
                next_seq += 1;
                seq
            })
        })
        .collect();

    for (seq, hash) in &targeted {
        if !seqs.contains(seq) {
            bail!(
                "{}",
                crate::tools::metadata::get_config()
                    .error_target_changed_outside
                    .replacen("{}", hash, 1)
            );
        }
    }

    let parent_contexts = crate::parser::compute_parent_contexts(filepath, &content);
    let crlf_val = if content.contains("\r\n") { 1 } else { 0 };
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;

    let tx = conn.transaction()?;
    tx.execute("DELETE FROM lines WHERE session_id = ?1", [session_id])?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO lines (session_id, sequence_id, line_hash, norm_hash, sort_order, parent_context) VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
        )?;
        for (idx, seq) in seqs.iter().enumerate() {
            stmt.execute(rusqlite::params![
                session_id,
                seq,
                disk_hashes[idx],
                disk_norms[idx],
                (idx + 1) as f64,
                parent_contexts.get(idx).cloned().flatten(),
            ])?;
        }
    }
    tx.execute(
        "UPDATE sessions SET file_hash = ?1, mtime = ?2, line_ending_crlf = ?3, last_accessed_at = ?4, next_line_id = ?5 WHERE session_id = ?6",
        rusqlite::params![disk_file_hash, disk_mtime, crlf_val, now, next_seq, session_id],
    )?;
    tx.commit()?;

    Ok(seqs.len())
}

pub(crate) fn is_binary_file(path: &str) -> Result<bool> {
    use std::io::Read;
    let mut file = fs::File::open(path)
        .with_context(|| format!("Failed to open file for binary check: {}", path))?;
    let mut buffer = [0; 8192];
    let bytes_read = file
        .read(&mut buffer)
        .with_context(|| format!("Failed to read file for binary check: {}", path))?;
    Ok(buffer[..bytes_read].contains(&0))
}

pub(crate) fn compute_sha256(path: &str) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = fs::File::open(path)
        .with_context(|| format!("Failed to open file for SHA256 hashing: {}", path))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 8192];
    loop {
        let n = file
            .read(&mut buffer)
            .with_context(|| format!("Failed to read file for SHA256 hashing: {}", path))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Whether a file's syntax can be checked at all: the grammar directory names
/// its extension, or it is markdown, which comrak parses without one. Derived
/// rather than listed, so that dropping a grammar in reaches the check too
/// (D-01M27WE1VYAKPP).
pub(crate) fn check_language_supported(path: &str) -> bool {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    crate::config::language_for_extension(&ext).is_some()
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

pub(crate) fn init_edit_session(
    filepath: &str,
    create_if_not_exists: bool,
) -> Result<SessionMetadata> {
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
    let mtime = path
        .metadata()?
        .modified()?
        .duration_since(UNIX_EPOCH)?
        .as_secs() as i64;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;

    // A session for this path, and whether the file still looks the way the
    // session last saw it.
    let existing: Option<(String, bool)> = conn
        .query_row(
            "SELECT session_id, file_hash = ?2 AND mtime = ?3 FROM sessions WHERE filepath = ?1",
            rusqlite::params![filepath, file_hash, mtime],
            |row| Ok((row.get(0)?, row.get::<_, i64>(1)? != 0)),
        )
        .optional()
        .context("Failed to query existing session")?;

    let is_supported = check_language_supported(filepath);
    let warning_message = if !is_supported {
        Some(
            crate::tools::metadata::get_config()
                .warning_no_grammar
                .clone(),
        )
    } else {
        None
    };

    if let Some((session_id, unchanged)) = existing {
        // The one gate every entry point passes through: a file that moved
        // under us is reconciled here, not rebuilt, so the ids an agent is
        // holding survive whatever happened outside the tool.
        let total_lines = if unchanged {
            conn.execute(
                "UPDATE sessions SET last_accessed_at = ?1 WHERE session_id = ?2;",
                rusqlite::params![now, session_id],
            )
            .context("Failed to update last_accessed_at for reused session")?;
            conn.query_row(
                "SELECT COUNT(*) FROM lines WHERE session_id = ?1",
                rusqlite::params![session_id],
                |row| row.get(0),
            )?
        } else {
            reconcile_index(&mut conn, &session_id, filepath, &[])?
        };
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
    use sha1::{Digest, Sha1};
    let session_id = format!(
        "{:x}",
        Sha1::digest(format!("{}-{}-{}", filepath, file_hash, now).as_bytes())
    );

    // Read the file and populate rows
    let content = fs::read_to_string(filepath).context("Failed to read file content")?;
    let is_crlf = content.contains("\r\n");
    let crlf_val = if is_crlf { 1 } else { 0 };

    // Begin transaction
    let tx = conn
        .transaction()
        .context("Failed to begin SQLite transaction")?;

    // Clear any stale session for this filepath
    tx.execute(
        "DELETE FROM sessions WHERE filepath = ?1;",
        rusqlite::params![filepath],
    )
    .context("Failed to delete existing session for path")?;

    tx.execute(
        "INSERT INTO sessions (filepath, session_id, file_hash, mtime, last_accessed_at, line_ending_crlf) VALUES (?1, ?2, ?3, ?4, ?5, ?6);",
        rusqlite::params![filepath, session_id, file_hash, mtime, now, crlf_val],
    ).context("Failed to insert new session")?;

    let parent_contexts = crate::parser::compute_parent_contexts(filepath, &content);
    let mut lines_count = 0;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO lines (session_id, sequence_id, line_hash, norm_hash, sort_order, parent_context) VALUES (?1, ?2, ?3, ?4, ?5, ?6);"
        )?;

        // Split by lines, preserving empty final lines
        let mut parts: Vec<&str> = content.split('\n').collect();
        if parts.last() == Some(&"") {
            parts.pop();
        }

        for (idx, line_content) in parts.iter().enumerate() {
            let seq = idx + 1;
            let sort_order = seq as f64;
            let trimmed_line = line_content.strip_suffix('\r').unwrap_or(line_content);
            let p_ctx = parent_contexts.get(idx).cloned().flatten();
            stmt.execute(rusqlite::params![
                session_id,
                seq,
                compute_stored_hash(trimmed_line),
                compute_normalized_hash(trimmed_line),
                sort_order,
                p_ctx
            ])
            .context("Failed to insert line")?;
            lines_count += 1;
        }
    }

    tx.execute(
        "UPDATE sessions SET next_line_id = ?1 WHERE session_id = ?2",
        rusqlite::params![lines_count as i64 + 1, session_id],
    )
    .context("Failed to seed the line id counter")?;

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
pub(crate) fn compute_stored_hash(content: &str) -> String {
    use sha1::{Digest, Sha1};
    let mut hasher = Sha1::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())[..16].to_string()
}

/// A hash of the line with every space removed, so respacing it does not
/// change the result. Collapsing runs of whitespace instead would not be
/// enough: formatters add and remove spaces around operators and delimiters,
/// not just at the margin.
pub(crate) fn compute_normalized_hash(content: &str) -> String {
    compute_stored_hash(
        &content
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>(),
    )
}

/// The hash surfaced to agents inside `{sequence_id:x}#{hash}`. It is a prefix
/// of the stored hash, so the short form can be checked against a stored row
/// without keeping a second column. The sequence number does the identifying
/// here, which is why four characters are enough.
pub(crate) fn compute_line_hash(content: &str) -> String {
    compute_stored_hash(content)[..4].to_string()
}
