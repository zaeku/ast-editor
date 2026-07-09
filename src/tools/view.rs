use anyhow::Result;
use crate::tools::session_db::{get_db_connection, ensure_hashes_for_range, compute_line_hash};

pub fn view_lines(filepath: &str, start_line: usize, end_line: usize) -> Result<String> {
    if start_line == 0 {
        anyhow::bail!("Invalid bounds: start_line must be greater than 0");
    }
    if start_line > end_line {
        anyhow::bail!("Invalid bounds: start_line ({}) cannot be greater than end_line ({})", start_line, end_line);
    }

    let conn = get_db_connection()?;
    
    let session_id = match conn.prepare("SELECT session_id, mtime FROM sessions WHERE filepath = ?1")?
        .query_row([filepath], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))
    {
        Ok((sid, old_mtime)) => {
            let path = std::path::Path::new(filepath);
            let current_mtime = path.metadata()?.modified()?
                .duration_since(std::time::UNIX_EPOCH)?.as_secs() as i64;
            if current_mtime != old_mtime {
                let meta = crate::tools::session_db::init_edit_session(filepath, false)?;
                meta.session_id
            } else {
                sid
            }
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            let meta = crate::tools::session_db::init_edit_session(filepath, false)?;
            meta.session_id
        }
        Err(err) => return Err(anyhow::Error::from(err)),
    };

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
            format!("{}#{}", hex_seq, line_hash),
            current_idx,
            content
        ]));
        current_idx += 1;
    }

    let result_val = serde_json::json!({
        "columns": ["id", "n", "content"],
        "lines": items,
        "tip": "Edit these lines by calling 'edit_lines' with the line IDs (e.g. 1a#f8c9) shown above."
    });

    let output = serde_json::to_string_pretty(&result_val)?;
    Ok(output)
}

#[deprecated(since = "0.1.0", note = "use view_lines instead")]
pub fn view_session_lines(filepath: &str, start_line: usize, end_line: usize) -> Result<String> {
    view_lines(filepath, start_line, end_line)
}

pub fn create_lines(filepath: &str, content: &str) -> Result<String> {
    let path = std::path::Path::new(filepath);
    if path.exists() {
        anyhow::bail!("FILE_ALREADY_EXISTS: File already exists at: {}", filepath);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    std::fs::write(filepath, content)?;

    let meta = crate::tools::session_db::init_edit_session(filepath, false)?;
    let conn = get_db_connection()?;

    if meta.total_lines > 0 {
        ensure_hashes_for_range(&conn, &meta.session_id, 1, meta.total_lines)?;
    }

    let mut stmt = conn.prepare(
        "SELECT sequence_id, line_hash, content FROM lines WHERE session_id = ?1 ORDER BY sort_order"
    )?;

    let mut rows = stmt.query(rusqlite::params![meta.session_id])?;
    let mut items = Vec::new();

    let mut current_idx = 1;
    while let Some(row) = rows.next()? {
        let seq_id: i64 = row.get(0)?;
        let line_hash_opt: Option<String> = row.get(1)?;
        let content: String = row.get(2)?;
        
        let line_hash = line_hash_opt.unwrap_or_else(|| compute_line_hash(&content));
        let hex_seq = format!("{:x}", seq_id);
        items.push(serde_json::json!([
            format!("{}#{}", hex_seq, line_hash),
            current_idx,
            content
        ]));
        current_idx += 1;
    }

    let result_val = serde_json::json!({
        "status": "success",
        "message": "File successfully created and line editing session initialized.",
        "columns": ["id", "n", "content"],
        "lines": items
    });

    let output = serde_json::to_string_pretty(&result_val)?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::TEST_DB_LOCK as DB_LOCK;
    use std::fs;

    #[test]
    fn test_create_lines_success() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let temp_dir = std::env::temp_dir().join("line-editor-test-create-success");
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir)?;
        }
        fs::create_dir_all(&temp_dir)?;
        let file_path = temp_dir.join("new_file.txt");
        let filepath_str = file_path.to_str().unwrap();

        let content = "line 1\nline 2\nline 3";
        let output = create_lines(filepath_str, content)?;

        let val: serde_json::Value = serde_json::from_str(&output)?;
        assert_eq!(val["status"], "success");
        assert_eq!(val["message"], "File successfully created and line editing session initialized.");
        assert_eq!(val["columns"], serde_json::json!(["id", "n", "content"]));

        let lines = val["lines"].as_array().unwrap();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0][1], 1);
        assert_eq!(lines[0][2], "line 1");
        assert_eq!(lines[1][1], 2);
        assert_eq!(lines[1][2], "line 2");
        assert_eq!(lines[2][1], 3);
        assert_eq!(lines[2][2], "line 3");

        // Verify that the file was indeed written
        let read_content = fs::read_to_string(filepath_str)?;
        assert_eq!(read_content, content);

        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }

    #[test]
    fn test_create_lines_already_exists() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let temp_dir = std::env::temp_dir().join("line-editor-test-create-exists");
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir)?;
        }
        fs::create_dir_all(&temp_dir)?;
        let file_path = temp_dir.join("existing_file.txt");
        let filepath_str = file_path.to_str().unwrap();

        // Write initial content
        let initial_content = "existing content";
        fs::write(filepath_str, initial_content)?;

        // Try to create again
        let res = create_lines(filepath_str, "new content");
        assert!(res.is_err());
        let err_msg = res.err().unwrap().to_string();
        assert!(err_msg.contains("FILE_ALREADY_EXISTS"));
        assert!(err_msg.contains("File already exists at"));

        // Verify that content was NOT overwritten
        let read_content = fs::read_to_string(filepath_str)?;
        assert_eq!(read_content, initial_content);

        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }
}

