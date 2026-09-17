use anyhow::{bail, Context, Result};

use crate::tools::line_id::{
    compute_normalized_hash, compute_stored_hash, parse_line_id, FileEntry,
};
use crate::tools::store::{cleanup_stale_sessions, create_tables, get_db_connection};
use rusqlite::{Connection, OptionalExtension};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

/// Reconcile a file's index with the file on disk.
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
    file_key: &str,
    filepath: &str,
    start_ids: &[String],
) -> Result<usize> {
    let current: Option<(String, i64, usize)> = conn.query_row(
        "SELECT file_hash, mtime, (SELECT COUNT(*) FROM lines WHERE file_key = ?1) FROM files WHERE file_key = ?1",
        [file_key],
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
            "SELECT sequence_id, COALESCE(line_hash, ''), COALESCE(norm_hash, '') FROM lines WHERE file_key = ?1 ORDER BY sort_order"
        )?;
        let rows = stmt.query_map([file_key], |row| {
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

    // A retired line sits in both queues — once under its own hash and once
    // under its hash ignoring whitespace — so a seq the first pass hands out
    // has to be refused by the second. Handing one out twice gives two lines
    // the same id, and an edit addressed to it then takes whichever comes
    // first (D-01M27KBH6NNXZJ, D-01M280K52X35JE).
    let mut taken: std::collections::HashSet<i64> = assigned.iter().flatten().copied().collect();
    let mut claim = |slot: &mut Option<i64>, queue: &mut VecDeque<i64>| {
        while let Some(seq) = queue.pop_front() {
            if taken.insert(seq) {
                *slot = Some(seq);
                return;
            }
        }
    };

    // A moved line reads as a delete plus an insert. Where an inserted line
    // matches a retired one exactly, it is that same line in a new place, so
    // it keeps its identity.
    for (new_idx, slot) in assigned.iter_mut().enumerate() {
        if slot.is_none() {
            if let Some(queue) = retired.get_mut(disk_hashes[new_idx].as_str()) {
                claim(slot, queue);
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
                claim(slot, queue);
            }
        }
    }

    let stored_next: i64 = conn
        .query_row(
            "SELECT next_line_id FROM files WHERE file_key = ?1",
            [file_key],
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
    tx.execute("DELETE FROM lines WHERE file_key = ?1", [file_key])?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO lines (file_key, sequence_id, line_hash, norm_hash, sort_order, parent_context) VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
        )?;
        for (idx, seq) in seqs.iter().enumerate() {
            stmt.execute(rusqlite::params![
                file_key,
                seq,
                disk_hashes[idx],
                disk_norms[idx],
                (idx + 1) as f64,
                parent_contexts.get(idx).cloned().flatten(),
            ])?;
        }
    }
    tx.execute(
        "UPDATE files SET file_hash = ?1, mtime = ?2, line_ending_crlf = ?3, last_accessed_at = ?4, next_line_id = ?5 WHERE file_key = ?6",
        rusqlite::params![disk_file_hash, disk_mtime, crlf_val, now, next_seq, file_key],
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

pub(crate) fn init_file_entry(filepath: &str, create_if_not_exists: bool) -> Result<FileEntry> {
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

    // The entry for this path, and whether the file still looks the way the
    // entry last saw it.
    let existing: Option<(String, bool)> = conn
        .query_row(
            "SELECT file_key, file_hash = ?2 AND mtime = ?3 FROM files WHERE filepath = ?1",
            rusqlite::params![filepath, file_hash, mtime],
            |row| Ok((row.get(0)?, row.get::<_, i64>(1)? != 0)),
        )
        .optional()
        .context("Failed to query the existing entry")?;

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

    if let Some((file_key, unchanged)) = existing {
        // The one gate every entry point passes through: a file that moved
        // under us is reconciled here, not rebuilt, so the ids an agent is
        // holding survive whatever happened outside the tool.
        let total_lines = if unchanged {
            conn.execute(
                "UPDATE files SET last_accessed_at = ?1 WHERE file_key = ?2;",
                rusqlite::params![now, file_key],
            )
            .context("Failed to update last_accessed_at for a reused entry")?;
            conn.query_row(
                "SELECT COUNT(*) FROM lines WHERE file_key = ?1",
                rusqlite::params![file_key],
                |row| row.get(0),
            )?
        } else {
            reconcile_index(&mut conn, &file_key, filepath, &[])?
        };
        return Ok(FileEntry {
            file_key,
            total_lines,
            file_hash,
            mtime,
            is_supported,
            warning_message,
        });
    }

    // Create a new entry
    use sha1::{Digest, Sha1};
    let file_key = format!(
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

    // Clear any stale entry for this filepath
    tx.execute(
        "DELETE FROM files WHERE filepath = ?1;",
        rusqlite::params![filepath],
    )
    .context("Failed to delete the existing entry for path")?;

    tx.execute(
        "INSERT INTO files (filepath, file_key, file_hash, mtime, last_accessed_at, line_ending_crlf) VALUES (?1, ?2, ?3, ?4, ?5, ?6);",
        rusqlite::params![filepath, file_key, file_hash, mtime, now, crlf_val],
    ).context("Failed to insert the new entry")?;

    let parent_contexts = crate::parser::compute_parent_contexts(filepath, &content);
    let mut lines_count = 0;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO lines (file_key, sequence_id, line_hash, norm_hash, sort_order, parent_context) VALUES (?1, ?2, ?3, ?4, ?5, ?6);"
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
                file_key,
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
        "UPDATE files SET next_line_id = ?1 WHERE file_key = ?2",
        rusqlite::params![lines_count as i64 + 1, file_key],
    )
    .context("Failed to seed the line id counter")?;

    tx.commit().context("Failed to commit SQLite transaction")?;

    Ok(FileEntry {
        file_key,
        total_lines: lines_count,
        file_hash,
        mtime,
        is_supported,
        warning_message,
    })
}
