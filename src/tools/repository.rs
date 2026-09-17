//! The store as the rest of the binary asks for it: one trait, and the SQLite
//! implementation every tool reaches it through.

use anyhow::{bail, Context, Result};
use rusqlite::OptionalExtension;
use std::time::{SystemTime, UNIX_EPOCH};

use super::buffer::{load_buffer, LineBuffer};
use super::file_entry::{compute_sha256, init_file_entry, reconcile_index};
use super::line_id::{
    compute_line_hash, compute_normalized_hash, compute_stored_hash, FileEntry, LineEdit,
};
use super::store::{get_db_connection, PREVIEW_TTL_SECONDS};

pub(crate) trait FileStore: Send + Sync {
    fn init_session(&self, filepath: &str, is_binary: bool) -> Result<FileEntry>;
    fn get_total_lines(&self, file_key: &str) -> Result<usize>;
    fn fetch_lines_range(
        &self,
        file_key: &str,
        start_line: usize,
        end_line: usize,
    ) -> Result<Vec<(i64, Option<String>, String)>>;
    /// Work out what a batch would produce, touching nothing. Returns the
    /// resulting buffer and the ids the batch would report.
    fn plan_line_edits(
        &self,
        file_key: &str,
        edits: &[LineEdit],
    ) -> Result<(
        LineBuffer,
        Vec<String>,
        Vec<crate::tools::buffer::Renumbered>,
    )>;
    /// Persist a planned buffer as the file's new index. Call only once the
    /// buffer's content has been accepted and written to disk.
    fn commit_buffer(&self, file_key: &str, buffer: &LineBuffer) -> Result<()>;
    fn get_file_crlf(&self, file_key: &str) -> Result<bool>;
    fn get_file_key(&self, filepath: &str) -> Result<Option<String>>;
    fn find_matching_lines(&self, file_key: &str, pattern: &regex::Regex) -> Result<Vec<usize>>;
    fn smart_resync(&self, filepath: &str, file_key: &str, start_ids: &[String]) -> Result<()>;
    fn create_preview(&self, filepath: &str, edits: &[LineEdit]) -> Result<String>;
    fn take_preview(&self, filepath: &str, preview_id: &str) -> Result<Vec<LineEdit>>;
}

pub(crate) struct SqliteFileStore;

impl FileStore for SqliteFileStore {
    fn init_session(&self, filepath: &str, is_binary: bool) -> Result<FileEntry> {
        init_file_entry(filepath, is_binary)
    }

    fn get_total_lines(&self, file_key: &str) -> Result<usize> {
        let conn = get_db_connection()?;
        let total_lines: usize = conn.query_row(
            "SELECT COUNT(*) FROM lines WHERE file_key = ?1",
            rusqlite::params![file_key],
            |row| row.get(0),
        )?;
        Ok(total_lines)
    }

    fn find_matching_lines(&self, file_key: &str, pattern: &regex::Regex) -> Result<Vec<usize>> {
        let mut conn = get_db_connection()?;
        let buffer = load_buffer(&mut conn, file_key)?;
        Ok(buffer
            .lines
            .iter()
            .enumerate()
            .filter(|(_, line)| pattern.is_match(&line.content))
            .map(|(idx, _)| idx + 1)
            .collect())
    }

    fn get_file_crlf(&self, file_key: &str) -> Result<bool> {
        let conn = get_db_connection()?;
        let crlf: i32 = conn.query_row(
            "SELECT COALESCE(line_ending_crlf, 0) FROM files WHERE file_key = ?1",
            rusqlite::params![file_key],
            |row| row.get(0),
        )?;
        Ok(crlf != 0)
    }

    fn fetch_lines_range(
        &self,
        file_key: &str,
        start_line: usize,
        end_line: usize,
    ) -> Result<Vec<(i64, Option<String>, String)>> {
        let mut conn = get_db_connection()?;
        let buffer = load_buffer(&mut conn, file_key)?;
        let from = start_line.saturating_sub(1);
        let to = std::cmp::min(end_line, buffer.lines.len());
        if from >= to {
            return Ok(Vec::new());
        }
        Ok(buffer.lines[from..to]
            .iter()
            .map(|line| {
                (
                    line.seq,
                    Some(compute_line_hash(&line.content)),
                    line.content.clone(),
                )
            })
            .collect())
    }

    fn plan_line_edits(
        &self,
        file_key: &str,
        edits: &[LineEdit],
    ) -> Result<(
        LineBuffer,
        Vec<String>,
        Vec<crate::tools::buffer::Renumbered>,
    )> {
        let mut conn = get_db_connection()?;
        let mut buffer = load_buffer(&mut conn, file_key)?;
        let (modified_lines, renumbered) = buffer.apply(edits)?;
        Ok((buffer, modified_lines, renumbered))
    }

    fn commit_buffer(&self, file_key: &str, buffer: &LineBuffer) -> Result<()> {
        let mut conn = get_db_connection()?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM lines WHERE file_key = ?1", [file_key])?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO lines (file_key, sequence_id, line_hash, norm_hash, sort_order, parent_context) VALUES (?1, ?2, ?3, ?4, ?5, ?6);"
            )?;
            for (idx, line) in buffer.lines.iter().enumerate() {
                stmt.execute(rusqlite::params![
                    file_key,
                    line.seq,
                    compute_stored_hash(&line.content),
                    compute_normalized_hash(&line.content),
                    (idx + 1) as f64,
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

        conn.execute(
            "DELETE FROM previews WHERE created_at < ?1",
            [now - PREVIEW_TTL_SECONDS],
        )?;

        conn.execute(
            "INSERT INTO previews (filepath, file_hash, edits_json, created_at) VALUES (?1, ?2, ?3, ?4);",
            rusqlite::params![filepath, compute_sha256(filepath)?, serde_json::to_string(edits)?, now],
        )?;

        Ok(format!("p{:x}", conn.last_insert_rowid()))
    }

    fn take_preview(&self, filepath: &str, preview_id: &str) -> Result<Vec<LineEdit>> {
        let rowid = preview_id
            .strip_prefix('p')
            .and_then(|digits| i64::from_str_radix(digits, 16).ok())
            .with_context(|| {
                crate::tools::metadata::get_config()
                    .error_preview_id_shape
                    .replacen("{}", preview_id, 1)
            })?;

        let conn = get_db_connection()?;
        let row = conn
            .query_row(
                "SELECT filepath, file_hash, edits_json FROM previews WHERE id = ?1",
                [rowid],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;

        let (preview_path, file_hash, edits_json) = row.with_context(|| {
            crate::tools::metadata::get_config()
                .error_preview_unknown
                .replacen("{}", preview_id, 1)
        })?;

        if preview_path != filepath {
            bail!(
                "{}",
                crate::tools::metadata::get_config()
                    .error_preview_other_file
                    .replacen("{}", preview_id, 1)
                    .replacen("{}", &preview_path, 1)
                    .replacen("{}", filepath, 1)
            );
        }

        if compute_sha256(filepath)? != file_hash {
            conn.execute("DELETE FROM previews WHERE id = ?1", [rowid])?;
            bail!(
                "{}",
                crate::tools::metadata::get_config()
                    .error_preview_stale
                    .replacen("{}", filepath, 1)
                    .replacen("{}", preview_id, 1)
            );
        }

        conn.execute("DELETE FROM previews WHERE id = ?1", [rowid])?;
        Ok(serde_json::from_str(&edits_json)?)
    }

    fn get_file_key(&self, filepath: &str) -> Result<Option<String>> {
        let conn = get_db_connection()?;
        let res = conn.query_row(
            "SELECT file_key FROM files WHERE filepath = ?1 ORDER BY last_accessed_at DESC LIMIT 1",
            [filepath],
            |row| row.get::<_, String>(0),
        );
        match res {
            Ok(sid) => Ok(Some(sid)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(anyhow::Error::from(err)),
        }
    }

    fn smart_resync(&self, filepath: &str, file_key: &str, start_ids: &[String]) -> Result<()> {
        let mut conn = get_db_connection()?;
        reconcile_index(&mut conn, file_key, filepath, start_ids).map(|_| ())
    }
}
