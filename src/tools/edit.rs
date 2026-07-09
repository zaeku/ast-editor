use anyhow::{Result, Context, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use crate::tools::session_db::{get_db_connection, compute_line_hash};

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct LineEdit {
    pub op: String,
    pub target_id: Option<String>,
    pub end_target_id: Option<String>,
    pub dest_target_id: Option<String>,
    pub move_position: Option<String>,
    pub content: Option<String>,
}

fn parse_line_id(id_str: &str) -> Result<(i64, String)> {
    let parts: Vec<&str> = id_str.split('#').collect();
    if parts.len() != 2 {
        bail!("Invalid Line ID format: {}", id_str);
    }
    let seq = i64::from_str_radix(parts[0], 16)
        .context("Failed to parse sequence ID hex")?;
    Ok((seq, parts[1].to_string()))
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

async fn validate_syntax(filepath: &str, content: &str, parser_manager: &crate::parser::ParserManager) -> Result<()> {
    if !check_language_supported(filepath) {
        return Ok(());
    }

    let ext = std::path::Path::new(filepath)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    let (tree, _language) = parser_manager.parse_code(&ext, content).await
        .context("Failed to parse code for syntax validation")?;

    let ast = tree.root_node().to_sexp();
    
    // Check if AST has ERROR or MISSING nodes
    if ast.contains("ERROR") || ast.contains("MISSING") {
        bail!("Validation error: Syntactical errors detected in code after edits. Compilation/AST verification aborted.\n{}", ast);
    }
    
    Ok(())
}

pub async fn edit_lines(
    filepath: &str,
    edits: Vec<LineEdit>,
    parser_manager: &crate::parser::ParserManager,
) -> Result<String> {
    let mut conn = get_db_connection()?;
    let path = std::path::Path::new(filepath);
    
    let session_info = conn.query_row(
        "SELECT session_id, mtime, file_hash FROM sessions WHERE filepath = ?1",
        [filepath],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?))
    );

    let (session_id, old_mtime) = match session_info {
        Ok((sid, mtime, _hash)) => (sid, mtime),
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            let meta = crate::tools::session_db::init_edit_session(filepath, false)?;
            (meta.session_id, meta.mtime)
        }
        Err(err) => return Err(anyhow::Error::from(err)),
    };

    // Out-of-Sync Check
    let current_mtime = path.metadata()?.modified()?
        .duration_since(std::time::UNIX_EPOCH)?.as_secs() as i64;
    if current_mtime != old_mtime {
        bail!("CONCURRENCY_ERROR: File has been modified externally. Re-initialize the session.");
    }

    // Long Line Edit Protection Check
    for edit in &edits {
        let target_ids_to_check = vec![
            edit.target_id.as_ref(),
            edit.end_target_id.as_ref(),
            edit.dest_target_id.as_ref(),
        ];
        for target_id in target_ids_to_check.into_iter().flatten() {
            if !target_id.is_empty() {
                let ends_with_trunc = target_id.ends_with("#TRUNC");
                let (seq, _) = parse_line_id(target_id)?;
                let db_content_res: Result<String, rusqlite::Error> = conn.query_row(
                    "SELECT content FROM lines WHERE session_id = ?1 AND sequence_id = ?2",
                    rusqlite::params![session_id, seq],
                    |row| row.get(0)
                );
                match db_content_res {
                    Ok(content) => {
                        let char_count = content.chars().count();
                        if ends_with_trunc || char_count > 2048 {
                            bail!(
                                "LINE_TOO_LONG_ERROR: Line is too long ({} chars) and has been truncated in the view. Surgical updates on truncated lines are disabled to prevent accidental data loss. Please format the file using a code beautifier (e.g. prettier, black, or cargo fmt) to break it into multiple lines, or rewrite the file using create_lines/write_to_file.",
                                char_count
                            );
                        }
                    }
                    Err(rusqlite::Error::QueryReturnedNoRows) => {}
                    Err(err) => return Err(anyhow::Error::from(err)),
                }
            }
        }
    }

    let tx = conn.transaction()?;

    let mut newly_modified_ids = Vec::new();
    let mut deleted_orders = Vec::new();


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
                        [&session_id],
                        |row| row.get(0)
                    ).ok().flatten();
                    let start = last_order.unwrap_or(0.0);
                    (start, start + 1000.0)
                } else if edit.op == "prepend" || (edit.op == "insert_before" && is_empty_target) {
                    let first_order: Option<f64> = tx.query_row(
                        "SELECT MIN(sort_order) FROM lines WHERE session_id = ?1",
                        [&session_id],
                        |row| row.get(0)
                    ).ok().flatten();
                    let end = first_order.unwrap_or(1000.0);
                    (end - 1000.0, end)
                } else {
                    let target_id = edit.target_id.as_ref().filter(|s| !s.is_empty())
                        .context(format!("CHECKSUM_ERROR: Missing target_id for {} operation", edit.op))?;
                    let (target_seq, target_hash) = parse_line_id(target_id)?;
                    
                    // Validate hash and retrieve db_order
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
                        bail!("CHECKSUM_ERROR: Target line hash mismatch for target_id={}. Edit rejected.", target_id);
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
                    [&session_id],
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
                    newly_modified_ids.push((new_seq, hash));
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
                    bail!("CHECKSUM_ERROR: Target line hash mismatch for target_id={}. Edit rejected.", target_id);
                }

                let new_hash = compute_line_hash(content);
                tx.execute(
                    "UPDATE lines SET content = ?1, line_hash = ?2 WHERE id = ?3;",
                    rusqlite::params![content, new_hash, db_id]
                )?;
                newly_modified_ids.push((target_seq, new_hash));
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
                    bail!("CHECKSUM_ERROR: Target line hash mismatch for target_id={}. Edit rejected.", target_id);
                }

                tx.execute("DELETE FROM lines WHERE id = ?1;", [db_id])?;
                deleted_orders.push(sort_order);
            }
            "replace_range" => {
                let start_id = edit.target_id.as_ref().filter(|s| !s.is_empty())
                    .context("CHECKSUM_ERROR: Missing target_id (start_id) for replace_range op")?;
                let end_id = edit.end_target_id.as_ref().filter(|s| !s.is_empty())
                    .context("CHECKSUM_ERROR: Missing end_target_id for replace_range op")?;
                let content = edit.content.as_ref().unwrap_or(&String::new()).clone();

                let (start_seq, start_hash) = parse_line_id(start_id)?;
                let (end_seq, end_hash) = parse_line_id(end_id)?;

                // Resolve start line details
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
                    bail!("CHECKSUM_ERROR: Start line hash mismatch for target_id={}. Edit rejected.", start_id);
                }

                // Resolve end line details
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
                    bail!("CHECKSUM_ERROR: End line hash mismatch for end_target_id={}. Edit rejected.", end_id);
                }

                if start_order > end_order {
                    bail!("VALIDATION_ERROR: start_id sort order is after end_id sort order for replace_range.");
                }

                // Retrieve context limits
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

                // Add to deleted_orders for preview context rendering
                let mut stmt = tx.prepare("SELECT sort_order FROM lines WHERE session_id = ?1 AND sort_order >= ?2 AND sort_order <= ?3")?;
                let mut rows = stmt.query(rusqlite::params![session_id, start_order, end_order])?;
                while let Some(row) = rows.next()? {
                    deleted_orders.push(row.get(0)?);
                }

                // Delete the lines in the range
                tx.execute(
                    "DELETE FROM lines WHERE session_id = ?1 AND sort_order >= ?2 AND sort_order <= ?3",
                    rusqlite::params![session_id, start_order, end_order]
                )?;

                // Split and insert new content lines in the gap
                let lines_to_insert: Vec<&str> = if content.is_empty() {
                    vec![""]
                } else {
                    content.split('\n').collect()
                };

                let max_seq: i64 = tx.query_row(
                    "SELECT COALESCE(MAX(sequence_id), 0) FROM lines WHERE session_id = ?1",
                    [&session_id],
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
                    newly_modified_ids.push((new_seq, hash));
                }
            }
            "move" => {
                let start_id = edit.target_id.as_ref().filter(|s| !s.is_empty())
                    .context("CHECKSUM_ERROR: Missing target_id (start_id) for move op")?;
                let end_id = edit.end_target_id.as_ref().filter(|s| !s.is_empty()).unwrap_or(start_id);
                let move_pos = edit.move_position.as_ref().map(|s| s.as_str()).unwrap_or("after");

                let (start_seq, start_hash) = parse_line_id(start_id)?;
                let (end_seq, end_hash) = parse_line_id(end_id)?;

                // Verify start line
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
                    bail!("CHECKSUM_ERROR: Start line hash mismatch for target_id={}. Edit rejected.", start_id);
                }

                // Verify end line
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
                    bail!("CHECKSUM_ERROR: End line hash mismatch for end_target_id={}. Edit rejected.", end_id);
                }

                if start_order > end_order {
                    bail!("VALIDATION_ERROR: start_id sort order is after end_id sort order for move.");
                }

                // Get all moved lines sorted by sort_order
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
                            bail!("CHECKSUM_ERROR: Destination line hash mismatch for dest_target_id={}. Edit rejected.", dest_id);
                        }

                        if dest_order >= start_order && dest_order <= end_order {
                            bail!("VALIDATION_ERROR: Cannot move a range into itself (dest_target_id lies within source range).");
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
                    other => bail!("Unknown move position: {}", other),
                };

                // Space the moved lines in the destination gap
                let gap = gap_end - gap_start;
                let step = gap / (moved_lines.len() as f64 + 1.0);

                for (idx, line) in moved_lines.iter().enumerate() {
                    let current_order = gap_start + step * ((idx + 1) as f64);
                    tx.execute(
                        "UPDATE lines SET sort_order = ?1 WHERE id = ?2;",
                        rusqlite::params![current_order, line.id]
                    )?;
                    newly_modified_ids.push((line.seq, line.hash.clone()));
                }
            }
            other => bail!("Unknown edit operation: {}", other),
        }
    }

    // Build the final content
    let final_content = {
        let mut stmt = tx.prepare("SELECT content FROM lines WHERE session_id = ?1 ORDER BY sort_order")?;
        let mut rows = stmt.query([&session_id])?;
        let mut contents = Vec::new();
        while let Some(row) = rows.next()? {
            let line_content: String = row.get(0)?;
            contents.push(line_content);
        }
        contents.join("\n") + "\n"
    };

    // Validate syntax
    validate_syntax(filepath, &final_content, parser_manager).await?;

    // If syntax passes, save to disk
    fs::write(filepath, &final_content)?;
    
    // Update session timestamp and file hash/mtime
    use sha2::{Sha256, Digest};
    let mut hasher = Sha256::new();
    hasher.update(final_content.as_bytes());
    let new_file_hash = format!("{:x}", hasher.finalize());
    let new_mtime = path.metadata()?.modified()?
        .duration_since(std::time::UNIX_EPOCH)?.as_secs() as i64;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?.as_secs() as i64;

    tx.execute(
        "UPDATE sessions SET file_hash = ?1, mtime = ?2, last_accessed_at = ?3 WHERE session_id = ?4;",
        rusqlite::params![new_file_hash, new_mtime, now, session_id]
    )?;

    tx.commit()?;

    // Generate output preview: read the current sorted lines from DB
    let mut sorted_lines = Vec::new();
    let mut missing_hashes = Vec::new();
    {
        let mut stmt = conn.prepare("SELECT id, sequence_id, line_hash, content, sort_order FROM lines WHERE session_id = ?1 ORDER BY sort_order")?;
        let mut rows = stmt.query([&session_id])?;
        
        while let Some(row) = rows.next()? {
            let db_id: i64 = row.get(0)?;
            let seq: i64 = row.get(1)?;
            let hash_opt: Option<String> = row.get(2)?;
            let content: String = row.get(3)?;
            let sort_order: f64 = row.get(4)?;
            
            let hash = match hash_opt {
                Some(h) => h,
                None => {
                    let h = compute_line_hash(&content);
                    missing_hashes.push((db_id, h.clone()));
                    h
                }
            };
            sorted_lines.push((seq, hash, content, sort_order));
        }
    }
    
    // Save any newly computed hashes back to the database
    if !missing_hashes.is_empty() {
        let mut update_stmt = conn.prepare("UPDATE lines SET line_hash = ?1 WHERE id = ?2;")?;
        for (db_id, hash) in missing_hashes {
            update_stmt.execute(rusqlite::params![hash, db_id])?;
        }
    }

    let mut items = Vec::new();
    
    // Find modified rows and show context around them
    let mut indices_to_show = std::collections::BTreeSet::new();
    for (line_idx, (seq, hash, _, _)) in sorted_lines.iter().enumerate() {
        if newly_modified_ids.iter().any(|(m_seq, m_hash)| m_seq == seq && m_hash == hash) {
            // Show modified row plus 2 rows before and 2 rows after
            let start = line_idx.saturating_sub(2);
            let end = std::cmp::min(line_idx + 2, sorted_lines.len().saturating_sub(1));
            for i in start..=end {
                indices_to_show.insert(i);
            }
        }
    }

    // Find closest lines to deleted ones and show context around them
    for &del_order in &deleted_orders {
        if sorted_lines.is_empty() {
            continue;
        }
        let mut min_diff = f64::MAX;
        let mut closest_idx = 0;
        for (line_idx, (_, _, _, line_order)) in sorted_lines.iter().enumerate() {
            let diff = (line_order - del_order).abs();
            if diff < min_diff {
                min_diff = diff;
                closest_idx = line_idx;
            }
        }
        let start = closest_idx.saturating_sub(2);
        let end = std::cmp::min(closest_idx + 2, sorted_lines.len().saturating_sub(1));
        for i in start..=end {
            indices_to_show.insert(i);
        }
    }

    // Default to last 5 lines if no modification ids were gathered (e.g. all deletions)
    if indices_to_show.is_empty() {
        let start = sorted_lines.len().saturating_sub(5);
        for i in start..sorted_lines.len() {
            indices_to_show.insert(i);
        }
    }

    for idx in indices_to_show {
        let (seq, hash, content, _) = &sorted_lines[idx];
        let hex_seq = format!("{:x}", seq);
        items.push(serde_json::json!([
            format!("{}#{}", hex_seq, hash),
            idx + 1,
            content
        ]));
    }

    let result_val = serde_json::json!({
        "columns": ["id", "n", "content"],
        "lines": items,
        "tip": "Edit these lines by calling 'edit_lines' with the line IDs (e.g. 1a#f8c9) shown above."
    });

    let output = serde_json::to_string_pretty(&result_val)?;
    Ok(output)
}

#[deprecated(since = "0.1.0", note = "use edit_lines instead")]
pub async fn apply_line_edits(
    filepath: &str,
    edits: Vec<LineEdit>,
    parser_manager: &crate::parser::ParserManager,
) -> Result<String> {
    edit_lines(filepath, edits, parser_manager).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::session_db::init_edit_session;
    use crate::parser::ParserManager;
    use crate::tools::TEST_DB_LOCK as DB_LOCK;

    // Helper to setup mock language config and raw wasm files in temporary wasm directory
    struct TestEnvironment {
        dir: std::path::PathBuf,
        pm: ParserManager,
    }

    impl TestEnvironment {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("tree_sitter_edit_tests_{}", name));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();

            let cache_dir = dir.join("cache");
            let compiler_path = dir.join("compiler");
            let wasm_dir = dir.join("wasm");
            fs::create_dir_all(&cache_dir).unwrap();
            fs::create_dir_all(&wasm_dir).unwrap();

            let langs_json_path = wasm_dir.join("languages.json");
            let mock_config = serde_json::json!({
                "rust": {
                    "extensions": [".rs"],
                    "wasm_file": "tree-sitter-rust.wasm"
                }
            });
            fs::write(&langs_json_path, serde_json::to_string(&mock_config).unwrap()).unwrap();

            // Copy real rust wasm so parsing/compilation succeeds
            let manifest_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
            let real_wasm_path = manifest_dir.join("resources").join("wasm").join("tree-sitter-rust.wasm");
            let target_wasm_path = wasm_dir.join("tree-sitter-rust.wasm");
            if real_wasm_path.exists() {
                fs::copy(&real_wasm_path, &target_wasm_path).unwrap();
            }

            let pm = ParserManager::with_paths(cache_dir, compiler_path, wasm_dir).unwrap();
            Self { dir, pm }
        }
    }

    impl Drop for TestEnvironment {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn find_line_id(lines_json: &str, pattern: &str) -> String {
        let val: serde_json::Value = serde_json::from_str(lines_json).unwrap();
        let lines = val["lines"].as_array().unwrap();
        for line in lines {
            let line_arr = line.as_array().unwrap();
            let code = line_arr[2].as_str().unwrap();
            if code.contains(pattern) {
                return line_arr[0].as_str().unwrap().to_string();
            }
        }
        String::new()
    }

    #[tokio::test]
    async fn test_apply_line_edits_insert_update_delete() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let env = TestEnvironment::new("basic_ops");

        let file_path = env.dir.join("code.rs");
        fs::write(&file_path, "fn main() {\n    let a = 1;\n}\n")?;
        let filepath_str = file_path.to_str().unwrap();

        let metadata = init_edit_session(filepath_str, false)?;
        assert_eq!(metadata.total_lines, 3);

        // Get the line IDs by viewing
        let lines_view = crate::tools::view::view_lines(filepath_str, 1, 3)?;
        let target_id = find_line_id(&lines_view, "let a = 1;");
        assert!(!target_id.is_empty());

        // 1. Test update
        let edits = vec![
            LineEdit {
                op: "update".to_string(),
                target_id: Some(target_id.clone()),
                content: Some("    let a = 42;".to_string()),
                ..Default::default()
            }
        ];
        let preview = edit_lines(filepath_str, edits, &env.pm).await?;
        assert!(preview.contains("let a = 42;"));
        assert_eq!(fs::read_to_string(&file_path)?, "fn main() {\n    let a = 42;\n}\n");

        // Refresh metadata/session
        let _metadata = init_edit_session(filepath_str, false)?;
        let lines_view = crate::tools::view::view_lines(filepath_str, 1, 3)?;
        let new_target_id = find_line_id(&lines_view, "let a = 42;");
        assert!(!new_target_id.is_empty());

        // 2. Test insert_after
        let edits = vec![
            LineEdit {
                op: "insert_after".to_string(),
                target_id: Some(new_target_id.clone()),
                content: Some("    let b = 2;".to_string()),
                ..Default::default()
            }
        ];
        let preview = edit_lines(filepath_str, edits, &env.pm).await?;
        assert!(preview.contains("let b = 2;"));
        assert_eq!(fs::read_to_string(&file_path)?, "fn main() {\n    let a = 42;\n    let b = 2;\n}\n");

        // Refresh session to get latest target IDs
        let _ = init_edit_session(filepath_str, false)?;
        let lines_view = crate::tools::view::view_lines(filepath_str, 1, 4)?;
        let b_id = find_line_id(&lines_view, "let b = 2;");
        assert!(!b_id.is_empty());

        // 3. Test delete
        let edits = vec![
            LineEdit {
                op: "delete".to_string(),
                target_id: Some(b_id.clone()),
                content: None,
                ..Default::default()
            }
        ];
        let _preview = edit_lines(filepath_str, edits, &env.pm).await?;
        assert_eq!(fs::read_to_string(&file_path)?, "fn main() {\n    let a = 42;\n}\n");

        Ok(())
    }

    #[tokio::test]
    async fn test_concurrency_error() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let env = TestEnvironment::new("concurrency");

        let file_path = env.dir.join("code.rs");
        fs::write(&file_path, "fn main() {}\n")?;
        let filepath_str = file_path.to_str().unwrap();

        let _metadata = init_edit_session(filepath_str, false)?;

        // Modify file externally on disk, changing its mtime
        tokio::time::sleep(tokio::time::Duration::from_millis(1100)).await;
        fs::write(&file_path, "fn main() {\n    // changed externally\n}\n")?;

        let edits = vec![
            LineEdit {
                op: "insert_after".to_string(),
                target_id: None,
                content: Some("// fail".to_string()),
                ..Default::default()
            }
        ];

        let res = edit_lines(filepath_str, edits, &env.pm).await;
        assert!(res.is_err());
        let err_msg = res.unwrap_err().to_string();
        assert!(err_msg.contains("CONCURRENCY_ERROR"), "Expected concurrency error, got: {}", err_msg);

        Ok(())
    }

    #[tokio::test]
    async fn test_checksum_error() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let env = TestEnvironment::new("checksum");

        let file_path = env.dir.join("code.rs");
        fs::write(&file_path, "fn main() {}\n")?;
        let filepath_str = file_path.to_str().unwrap();

        let _metadata = init_edit_session(filepath_str, false)?;

        let edits = vec![
            LineEdit {
                op: "update".to_string(),
                target_id: Some("1#9999".to_string()), // Invalid hash prefix
                content: Some("fn main() { // updated }".to_string()),
                ..Default::default()
            }
        ];

        let res = edit_lines(filepath_str, edits, &env.pm).await;
        assert!(res.is_err());
        let err_msg = res.unwrap_err().to_string();
        assert!(err_msg.contains("CHECKSUM_ERROR"), "Expected checksum error, got: {}", err_msg);

        Ok(())
    }

    #[tokio::test]
    async fn test_syntax_validation_error_rolls_back() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let env = TestEnvironment::new("syntax_error");

        let file_path = env.dir.join("code.rs");
        let initial_content = "fn main() {\n    let a = 1;\n}\n";
        fs::write(&file_path, initial_content)?;
        let filepath_str = file_path.to_str().unwrap();

        let _metadata = init_edit_session(filepath_str, false)?;

        // Find the line 2 ID
        let lines_view = crate::tools::view::view_lines(filepath_str, 1, 3)?;
        let target_id = find_line_id(&lines_view, "let a = 1;");
        assert!(!target_id.is_empty());

        // Apply edit that introduces syntax error (e.g. mismatched braces / parsing error)
        let edits = vec![
            LineEdit {
                op: "update".to_string(),
                target_id: Some(target_id),
                content: Some("    let a = {;".to_string()), // Syntax error
                ..Default::default()
            }
        ];

        let res = edit_lines(filepath_str, edits, &env.pm).await;
        assert!(res.is_err());
        let err_msg = res.unwrap_err().to_string();
        assert!(err_msg.contains("Validation error"), "Expected syntax validation error, got: {}", err_msg);

        // Verify disk content was rolled back (i.e. remains unchanged)
        assert_eq!(fs::read_to_string(&file_path)?, initial_content);

        // Verify DB content was rolled back (re-query content of line 2)
        let conn = get_db_connection()?;
        let content: String = conn.query_row(
            "SELECT content FROM lines WHERE session_id = ?1 ORDER BY sort_order LIMIT 1 OFFSET 1",
            [&_metadata.session_id],
            |row| row.get(0)
        )?;
        assert_eq!(content.trim(), "let a = 1;");

        Ok(())
    }

    #[tokio::test]
    async fn test_insert_into_empty_file() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let env = TestEnvironment::new("empty_file");

        let file_path = env.dir.join("code.rs");
        fs::write(&file_path, "")?;
        let filepath_str = file_path.to_str().unwrap();

        let metadata = init_edit_session(filepath_str, false)?;
        assert_eq!(metadata.total_lines, 0);

        let edits = vec![
            LineEdit {
                op: "append".to_string(),
                target_id: None,
                content: Some("fn main() {\n}".to_string()),
                ..Default::default()
            }
        ];

        let preview = edit_lines(filepath_str, edits, &env.pm).await?;
        assert!(preview.contains("fn main() {"));
        assert_eq!(fs::read_to_string(&file_path)?, "fn main() {\n}\n");

        Ok(())
    }

    #[tokio::test]
    async fn test_case_insensitive_validation_and_deletion_preview() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let env = TestEnvironment::new("case_insensitive_and_delete");

        // Use uppercase extension: .RS
        let file_path = env.dir.join("code.RS");
        fs::write(&file_path, "fn main() {\n    let a = 1;\n    let b = 2;\n    let c = 3;\n}\n")?;
        let filepath_str = file_path.to_str().unwrap();

        let metadata = init_edit_session(filepath_str, false)?;
        assert_eq!(metadata.total_lines, 5);

        // Get the line IDs by viewing
        let lines_view = crate::tools::view::view_lines(filepath_str, 1, 5)?;
        let b_id = find_line_id(&lines_view, "let b = 2;");
        assert!(!b_id.is_empty());

        // Test delete on .RS file (verifies lowercase lookup/delegation works for uppercase extensions too)
        let edits = vec![
            LineEdit {
                op: "delete".to_string(),
                target_id: Some(b_id.clone()),
                content: None,
                ..Default::default()
            }
        ];

        let preview = edit_lines(filepath_str, edits, &env.pm).await?;
        
        // The deleted line was `let b = 2;`.
        // The remaining lines around it should be rendered in the preview.
        assert!(preview.contains("let a = 1;"));
        assert!(preview.contains("let c = 3;"));
        assert!(!preview.contains("let b = 2;"));

        Ok(())
    }

    #[tokio::test]
    async fn test_append_operation_empty_file() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let env = TestEnvironment::new("append_empty");

        let file_path = env.dir.join("code.rs");
        fs::write(&file_path, "")?;
        let filepath_str = file_path.to_str().unwrap();

        let metadata = init_edit_session(filepath_str, false)?;
        assert_eq!(metadata.total_lines, 0);

        let edits = vec![
            LineEdit {
                op: "append".to_string(),
                target_id: None,
                content: Some("pub fn foo() {}".to_string()),
                ..Default::default()
            }
        ];

        let preview = edit_lines(filepath_str, edits, &env.pm).await?;
        assert!(preview.contains("pub fn foo()"));
        assert_eq!(fs::read_to_string(&file_path)?, "pub fn foo() {}\n");

        Ok(())
    }

    #[tokio::test]
    async fn test_append_operation_with_content() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = TestEnvironment::new("append_content");

        let file_path = env.dir.join("code.rs");
        fs::write(&file_path, "pub fn foo() {}\n")?;
        let filepath_str = file_path.to_str().unwrap();

        let metadata = init_edit_session(filepath_str, false)?;
        assert_eq!(metadata.total_lines, 1);

        let edits = vec![
            LineEdit {
                op: "append".to_string(),
                target_id: None,
                content: Some("pub fn bar() -> i32 {\n    42\n}".to_string()),
                ..Default::default()
            }
        ];

        let preview = edit_lines(filepath_str, edits, &env.pm).await?;
        assert!(preview.contains("pub fn bar() -> i32"));
        assert!(preview.contains("42"));
        assert_eq!(fs::read_to_string(&file_path)?, "pub fn foo() {}\npub fn bar() -> i32 {\n    42\n}\n");

        Ok(())
    }

    #[tokio::test]
    async fn test_strict_target_id_validation() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = TestEnvironment::new("strict_validation");

        let file_path = env.dir.join("code.rs");
        fs::write(&file_path, "pub fn foo() {}\n")?;
        let filepath_str = file_path.to_str().unwrap();

        let metadata = init_edit_session(filepath_str, false)?;
        assert_eq!(metadata.total_lines, 1);

        // Call update with target_id = None
        let edits = vec![
            LineEdit {
                op: "update".to_string(),
                target_id: None,
                content: Some("pub fn bar() {}".to_string()),
                ..Default::default()
            }
        ];

        let result = edit_lines(filepath_str, edits, &env.pm).await;
        assert!(result.is_err());
        let err_msg = result.err().unwrap().to_string();
        assert!(err_msg.contains("Missing target_id for update op"));

        Ok(())
    }

    #[tokio::test]
    async fn test_prepend_operation() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = TestEnvironment::new("prepend");

        let file_path = env.dir.join("code.rs");
        fs::write(&file_path, "pub fn hello() {}\n")?;
        let filepath_str = file_path.to_str().unwrap();

        let _metadata = init_edit_session(filepath_str, false)?;

        let edits = vec![
            LineEdit {
                op: "prepend".to_string(),
                target_id: None,
                content: Some("use std::collections::HashMap;\n".to_string()),
                ..Default::default()
            }
        ];

        let preview = edit_lines(filepath_str, edits, &env.pm).await?;
        assert!(preview.contains("use std::collections::HashMap;"));
        assert_eq!(
            fs::read_to_string(&file_path)?,
            "use std::collections::HashMap;\n\npub fn hello() {}\n"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_replace_range_operation() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = TestEnvironment::new("replace_range");

        let file_path = env.dir.join("code.rs");
        fs::write(&file_path, "pub fn foo() {\n    let a = 1;\n    let b = 2;\n}\n")?;
        let filepath_str = file_path.to_str().unwrap();

        let _metadata = init_edit_session(filepath_str, false)?;

        // Find IDs for lines 2 and 3
        let lines_view = crate::tools::view::view_lines(filepath_str, 2, 3)?;
        let id_2 = find_line_id(&lines_view, "let a = 1;");
        let id_3 = find_line_id(&lines_view, "let b = 2;");

        let edits = vec![
            LineEdit {
                op: "replace_range".to_string(),
                target_id: Some(id_2),
                end_target_id: Some(id_3),
                content: Some("    let val = 42;".to_string()),
                ..Default::default()
            }
        ];

        let preview = edit_lines(filepath_str, edits, &env.pm).await?;
        assert!(preview.contains("let val = 42;"));
        assert_eq!(
            fs::read_to_string(&file_path)?,
            "pub fn foo() {\n    let val = 42;\n}\n"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_move_operation() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = TestEnvironment::new("move_ops");

        let file_path = env.dir.join("code.rs");
        fs::write(&file_path, "pub fn main() {\n    foo();\n}\npub fn foo() {}\n")?;
        let filepath_str = file_path.to_str().unwrap();

        let _metadata = init_edit_session(filepath_str, false)?;

        // Get target ID for lines 4 (pub fn foo() {})
        let lines_view = crate::tools::view::view_lines(filepath_str, 4, 4)?;
        let foo_id = find_line_id(&lines_view, "pub fn foo() {}");

        // Get target ID for line 1 (pub fn main() {)
        let lines_view_main = crate::tools::view::view_lines(filepath_str, 1, 1)?;
        let main_id = find_line_id(&lines_view_main, "pub fn main() {");

        // Move 'foo' function before 'main' function
        let edits = vec![
            LineEdit {
                op: "move".to_string(),
                target_id: Some(foo_id),
                dest_target_id: Some(main_id),
                move_position: Some("before".to_string()),
                ..Default::default()
            }
        ];

        let preview = edit_lines(filepath_str, edits, &env.pm).await?;
        assert!(preview.contains("pub fn foo() {}"));
        assert_eq!(
            fs::read_to_string(&file_path)?,
            "pub fn foo() {}\npub fn main() {\n    foo();\n}\n"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_edit_lines_jit_initialization() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = TestEnvironment::new("test_jit_edit");
        
        let file_path = env.dir.join("code.txt");
        fs::write(&file_path, "line 1\nline 2\n")?;
        let filepath_str = file_path.to_str().unwrap();

        // Ensure no stale session exists in DB from previous test runs
        if let Ok(conn) = get_db_connection() {
            let _ = conn.execute("DELETE FROM sessions WHERE filepath = ?1", [filepath_str]);
            let _ = conn.execute("DELETE FROM lines WHERE session_id IN (SELECT session_id FROM sessions WHERE filepath = ?1)", [filepath_str]);
        }

        // Call edit_lines directly without calling init_edit_session.
        // We can append a line. Since it's append, target_id is ignored.
        let edits = vec![
            LineEdit {
                op: "append".to_string(),
                content: Some("line 3".to_string()),
                ..Default::default()
            }
        ];

        let preview = edit_lines(filepath_str, edits, &env.pm).await?;
        assert!(preview.contains("line 3"));

        let content = fs::read_to_string(&file_path)?;
        assert_eq!(content, "line 1\nline 2\nline 3\n");

        fs::remove_dir_all(&env.dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn test_long_line_edit_protection() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = TestEnvironment::new("long_line_test");

        let file_path = env.dir.join("code.rs");
        // Create a file containing a line > 2,048 chars.
        let long_line = "a".repeat(2050);
        let file_content = format!("fn main() {{\n    // {}\n}}\n", long_line);
        fs::write(&file_path, &file_content)?;
        let filepath_str = file_path.to_str().unwrap();

        let metadata = init_edit_session(filepath_str, false)?;
        assert_eq!(metadata.total_lines, 3);

        // Get the lines view
        let lines_view = crate::tools::view::view_lines(filepath_str, 1, 3)?;
        
        // Assert that the line has been truncated in the view (ends with #TRUNC)
        let target_id = find_line_id(&lines_view, "    // aaaaa");
        assert!(target_id.ends_with("#TRUNC"));

        // 1. Try to update using target_id (ends with #TRUNC)
        let edits = vec![
            LineEdit {
                op: "update".to_string(),
                target_id: Some(target_id.clone()),
                content: Some("    // updated long line".to_string()),
                ..Default::default()
            }
        ];

        let result = edit_lines(filepath_str, edits, &env.pm).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("LINE_TOO_LONG_ERROR"));
        assert!(err_msg.contains("Line is too long (2057 chars)"));
        assert!(err_msg.contains("prettier, black, or cargo fmt"));

        // Verify that the file remains unchanged on disk
        let disk_content = fs::read_to_string(&file_path)?;
        assert_eq!(disk_content, file_content);

        // 2. Try to update using a fake valid-looking ID but same sequence ID
        let parts: Vec<&str> = target_id.split('#').collect();
        let normal_target_id = format!("{}#abcdefabcdef", parts[0]);
        
        let edits_normal = vec![
            LineEdit {
                op: "update".to_string(),
                target_id: Some(normal_target_id),
                content: Some("    // updated long line".to_string()),
                ..Default::default()
            }
        ];
        
        let result_normal = edit_lines(filepath_str, edits_normal, &env.pm).await;
        assert!(result_normal.is_err());
        let err_msg_normal = result_normal.unwrap_err().to_string();
        assert!(err_msg_normal.contains("LINE_TOO_LONG_ERROR"));
        assert!(err_msg_normal.contains("Line is too long (2057 chars)"));
        assert!(err_msg_normal.contains("prettier, black, or cargo fmt"));

        // Verify that the file remains unchanged on disk
        let disk_content = fs::read_to_string(&file_path)?;
        assert_eq!(disk_content, file_content);

        // 3. Try replace_range operation where the end line is truncated
        let start_line_id = find_line_id(&lines_view, "fn main() {");
        assert!(!start_line_id.ends_with("#TRUNC"));

        let edits_replace = vec![
            LineEdit {
                op: "replace_range".to_string(),
                target_id: Some(start_line_id),
                end_target_id: Some(target_id),
                content: Some("fn main() {\n    // replaced".to_string()),
                ..Default::default()
            }
        ];

        let result_replace = edit_lines(filepath_str, edits_replace, &env.pm).await;
        assert!(result_replace.is_err());
        let err_msg_replace = result_replace.unwrap_err().to_string();
        assert!(err_msg_replace.contains("LINE_TOO_LONG_ERROR"));
        assert!(err_msg_replace.contains("Line is too long (2057 chars)"));
        assert!(err_msg_replace.contains("prettier, black, or cargo fmt"));

        // Verify that the file remains unchanged on disk
        let disk_content = fs::read_to_string(&file_path)?;
        assert_eq!(disk_content, file_content);

        fs::remove_dir_all(&env.dir)?;
        Ok(())
    }
}
