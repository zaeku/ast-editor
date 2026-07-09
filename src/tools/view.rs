use anyhow::{Result, Context};
use crate::tools::session_db::{get_db_connection, ensure_hashes_for_range, compute_line_hash};

pub fn view_session_lines(filepath: &str, start_line: usize, end_line: usize) -> Result<String> {
    if start_line == 0 {
        anyhow::bail!("Invalid bounds: start_line must be greater than 0");
    }
    if start_line > end_line {
        anyhow::bail!("Invalid bounds: start_line ({}) cannot be greater than end_line ({})", start_line, end_line);
    }

    let conn = get_db_connection()?;
    
    let mut stmt = conn.prepare("SELECT session_id FROM sessions WHERE filepath = ?1")?;
    let session_id: String = stmt.query_row([filepath], |row| row.get(0))
        .context("No active session found. You must initialize session first.")?;

    ensure_hashes_for_range(&conn, &session_id, start_line, end_line)?;

    let limit = if end_line >= start_line { end_line - start_line + 1 } else { 0 };
    let offset = start_line.saturating_sub(1);

    let mut stmt = conn.prepare(
        "SELECT sequence_id, line_hash, content FROM lines WHERE session_id = ?1 ORDER BY sort_order LIMIT ?2 OFFSET ?3"
    )?;

    let mut rows = stmt.query(rusqlite::params![session_id, limit, offset])?;
    let mut items = Vec::new();

    let mut current_idx = start_line;
    while let Some(row) = rows.next()? {
        let seq_id: i64 = row.get(0)?;
        let line_hash_opt: Option<String> = row.get(1)?;
        let content: String = row.get(2)?;
        
        let line_hash = line_hash_opt.unwrap_or_else(|| compute_line_hash(&content));
        let hex_seq = format!("{:x}", seq_id);
        items.push(serde_json::json!([
            current_idx,
            format!("{}#{}", hex_seq, line_hash),
            content
        ]));
        current_idx += 1;
    }

    let result_val = serde_json::json!({
        "lines": items,
        "tip": "Edit these lines by calling 'apply_line_edits' with the line IDs (e.g. 1a#f8c9) shown above."
    });

    let output = serde_json::to_string_pretty(&result_val)?;
    Ok(output)
}
