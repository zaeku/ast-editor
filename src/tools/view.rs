use anyhow::Result;
use crate::tools::session_db::{
    compute_line_hash, SessionRepository,
};

fn fetch_and_format_lines(
    repository: &impl SessionRepository,
    session_id: &str,
    start_line: usize,
    end_line: usize,
) -> Result<(serde_json::Value, usize, Option<String>)> {
    repository.ensure_hashes_range(session_id, start_line, end_line)?;

    let lines = repository.fetch_lines_range(session_id, start_line, end_line)?;
    let mut items = Vec::new();

    let mut current_idx = start_line;
    let mut actual_end_line = start_line.saturating_sub(1);
    let mut cumulative_bytes = 0;
    let mut capacity_truncated = false;

    for (seq_id, line_hash_opt, content) in lines {
        let (final_content, line_id) = if content.chars().count() > 2048 {
            let truncated: String = content.chars().take(2048).collect();
            let final_content = format!("{}... [TRUNCATED: Line is too long. DO NOT UPDATE this line directly unless replacing it completely.]", truncated);
            let line_id = format!("{:x}#TRUNC", seq_id);
            (final_content, line_id)
        } else {
            let line_hash = line_hash_opt.unwrap_or_else(|| compute_line_hash(&content));
            let line_id = format!("{:x}#{}", seq_id, line_hash);
            (content, line_id)
        };

        let line_len = final_content.len();
        if cumulative_bytes + line_len > 45000 {
            capacity_truncated = true;
            break;
        }

        cumulative_bytes += line_len;
        items.push(serde_json::json!([
            line_id,
            current_idx,
            final_content
        ]));
        actual_end_line = current_idx;
        current_idx += 1;
    }

    let warning_msg = if capacity_truncated {
        Some("Response truncated: cumulative response size limit (45,000 bytes) was reached.".to_string())
    } else {
        None
    };

    Ok((serde_json::Value::Array(items), actual_end_line, warning_msg))
}

pub fn view_lines(
    repository: &impl SessionRepository,
    filepath: &str,
    start_line: usize,
    end_line: usize,
) -> Result<String> {
    if start_line == 0 {
        anyhow::bail!("Invalid bounds: start_line must be greater than 0");
    }
    if start_line > end_line {
        anyhow::bail!("Invalid bounds: start_line ({}) cannot be greater than end_line ({})", start_line, end_line);
    }

    let meta = repository.init_session(filepath, false)?;
    let session_id = meta.session_id;

    let requested_len = if end_line >= start_line { end_line - start_line + 1 } else { 0 };
    let capped_end_line = if requested_len > 800 {
        start_line.saturating_add(799)
    } else {
        end_line
    };

    let total_lines = repository.get_total_lines(&session_id)?;

    let line_count_capped = requested_len > 800 && total_lines > 800;

    let total_bytes = std::path::Path::new(filepath).metadata()?.len();

    let (items, actual_end_line, capacity_warning) = fetch_and_format_lines(repository, &session_id, start_line, capped_end_line)?;

    let mut message = None;
    if line_count_capped {
        message = Some("Line count limit (800 lines max) exceeded. Output capped at 800 lines.".to_string());
    }
    if let Some(ref cap_msg) = capacity_warning {
        if let Some(existing_msg) = message {
            message = Some(format!("{}; {}", existing_msg, cap_msg));
        } else {
            message = Some(cap_msg.clone());
        }
    }

    let mut result_val = serde_json::json!({
        "columns": ["id", "n", "content"],
        "lines": items,
        "total_lines": total_lines,
        "total_bytes": total_bytes,
        "showing_start": start_line,
        "showing_end": actual_end_line,
        "tip": "Edit these lines by calling 'edit_lines' with the line IDs (e.g. 1a#f8c9) shown above."
    });

    if let Some(msg) = message {
        result_val["message"] = serde_json::json!(msg);
    }

    let output = serde_json::to_string_pretty(&result_val)?;
    Ok(output)
}

#[deprecated(since = "0.1.0", note = "use view_lines instead")]
pub fn view_session_lines(
    repository: &impl SessionRepository,
    filepath: &str,
    start_line: usize,
    end_line: usize,
) -> Result<String> {
    view_lines(repository, filepath, start_line, end_line)
}

pub fn create_lines(
    repository: &impl SessionRepository,
    filepath: &str,
    content: &str,
) -> Result<String> {
    let path = std::path::Path::new(filepath);
    if path.exists() {
        anyhow::bail!("FILE_ALREADY_EXISTS: File already exists at: {}", filepath);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    std::fs::write(filepath, content)?;

    let setup_db_and_fetch = || -> Result<(serde_json::Value, usize, Option<String>, usize, u64)> {
        let meta = repository.init_session(filepath, false)?;
        let total_bytes = std::path::Path::new(filepath).metadata()?.len();
        if meta.total_lines > 0 {
            let end_line = if meta.total_lines > 800 { 800 } else { meta.total_lines };
            let (items, actual_end_line, capacity_warning) = fetch_and_format_lines(repository, &meta.session_id, 1, end_line)?;
            Ok((items, actual_end_line, capacity_warning, meta.total_lines, total_bytes))
        } else {
            Ok((serde_json::json!([]), 0, None, 0, total_bytes))
        }
    };

    match setup_db_and_fetch() {
        Ok((items, actual_end_line, capacity_warning, total_lines, total_bytes)) => {
            let line_count_capped = total_lines > 800;
            let mut warning_parts = Vec::new();
            if line_count_capped {
                warning_parts.push("Line count limit (800 lines max) exceeded. Output capped at 800 lines.");
            }
            if let Some(ref cap_msg) = capacity_warning {
                warning_parts.push(cap_msg);
            }

            let mut message = "File successfully created and line editing session initialized.".to_string();
            if !warning_parts.is_empty() {
                message = format!("{} Truncation details: {}", message, warning_parts.join("; "));
            }

            let result_val = serde_json::json!({
                "status": "success",
                "message": message,
                "columns": ["id", "n", "content"],
                "lines": items,
                "total_lines": total_lines,
                "total_bytes": total_bytes,
                "showing_start": 1,
                "showing_end": actual_end_line,
            });

            let output = serde_json::to_string_pretty(&result_val)?;
            Ok(output)
        }
        Err(err) => {
            let _ = std::fs::remove_file(filepath);
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::session_db::SqliteSessionRepository;
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
        let repository = SqliteSessionRepository;
        let output = create_lines(&repository, filepath_str, content)?;

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
        let repository = SqliteSessionRepository;
        let res = create_lines(&repository, filepath_str, "new content");
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

    #[test]
    fn test_create_lines_db_failure_rollback() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let temp_dir = std::env::temp_dir().join("line-editor-test-create-rollback");
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir)?;
        }
        fs::create_dir_all(&temp_dir)?;
        let file_path = temp_dir.join("binary_file.bin");
        let filepath_str = file_path.to_str().unwrap();

        // Write content containing a null byte to trigger BINARY_FILE_ERROR during DB initialization
        let content = "hello \x00 world";
        let repository = SqliteSessionRepository;
        let res = create_lines(&repository, filepath_str, content);

        assert!(res.is_err());
        let err_msg = res.err().unwrap().to_string();
        assert!(err_msg.contains("BINARY_FILE_ERROR"));

        // Verify that the file was deleted/rolled back
        assert!(!file_path.exists());

        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }

    #[test]
    fn test_view_lines_capping() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let temp_dir = std::env::temp_dir().join("line-editor-test-capping");
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir)?;
        }
        fs::create_dir_all(&temp_dir)?;
        let file_path = temp_dir.join("capping_file.txt");
        let filepath_str = file_path.to_str().unwrap();

        // Create 1000 lines
        let mut lines = Vec::new();
        for idx in 1..=1000 {
            lines.push(format!("line {}", idx));
        }
        let content = lines.join("\n");
        let repository = SqliteSessionRepository;
        let _ = create_lines(&repository, filepath_str, &content)?;

        // View lines from 1 to 1000
        let output = view_lines(&repository, filepath_str, 1, 1000)?;
        let val: serde_json::Value = serde_json::from_str(&output)?;

        assert_eq!(val["total_lines"], 1000);
        assert_eq!(val["showing_start"], 1);
        assert_eq!(val["showing_end"], 800);
        assert!(val["message"].as_str().unwrap().contains("800 lines max"));

        let lines_array = val["lines"].as_array().unwrap();
        assert_eq!(lines_array.len(), 800);
        assert_eq!(lines_array[0][2], "line 1");
        assert_eq!(lines_array[799][2], "line 800");

        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }

    #[test]
    fn test_view_lines_truncation() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let temp_dir = std::env::temp_dir().join("line-editor-test-truncation");
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir)?;
        }
        fs::create_dir_all(&temp_dir)?;
        let file_path = temp_dir.join("truncation_file.txt");
        let filepath_str = file_path.to_str().unwrap();

        // Line 1: normal, Line 2: 2500 characters, Line 3: normal
        let long_line = "A".repeat(2500);
        let content = format!("short 1\n{}\nshort 3", long_line);
        let repository = SqliteSessionRepository;
        let _ = create_lines(&repository, filepath_str, &content)?;

        let output = view_lines(&repository, filepath_str, 1, 3)?;
        let val: serde_json::Value = serde_json::from_str(&output)?;

        let lines_array = val["lines"].as_array().unwrap();
        assert_eq!(lines_array.len(), 3);
        assert_eq!(lines_array[0][2], "short 1");
        assert!(lines_array[0][0].as_str().unwrap().contains('#'));
        assert!(!lines_array[0][0].as_str().unwrap().ends_with("#TRUNC"));

        // Truncated line:
        let trunc_content = lines_array[1][2].as_str().unwrap();
        assert!(trunc_content.starts_with(&"A".repeat(2048)));
        assert!(trunc_content.contains("[TRUNCATED: Line is too long."));
        assert!(lines_array[1][0].as_str().unwrap().ends_with("#TRUNC"));

        assert_eq!(lines_array[2][2], "short 3");

        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }

    #[test]
    fn test_view_lines_capacity() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let temp_dir = std::env::temp_dir().join("line-editor-test-capacity");
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir)?;
        }
        fs::create_dir_all(&temp_dir)?;
        let file_path = temp_dir.join("capacity_file.txt");
        let filepath_str = file_path.to_str().unwrap();

        // 50 lines, each ~1000 characters. 50 * 1000 = 50,000 characters/bytes
        let line_content = "B".repeat(1000);
        let mut lines = Vec::new();
        for _ in 1..=50 {
            lines.push(line_content.clone());
        }
        let content = lines.join("\n");
        let repository = SqliteSessionRepository;
        let _ = create_lines(&repository, filepath_str, &content)?;

        let output = view_lines(&repository, filepath_str, 1, 50)?;
        let val: serde_json::Value = serde_json::from_str(&output)?;

        let lines_array = val["lines"].as_array().unwrap();
        // Since limit is 45,000 bytes, we should stop before exceeding 45,000 bytes.
        // 45 lines * 1000 chars = 45,000 chars.
        assert_eq!(lines_array.len(), 45);
        assert_eq!(val["showing_end"], 45);
        assert!(val["message"].as_str().unwrap().contains("cumulative response size limit"));

        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }
}

