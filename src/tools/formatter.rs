use anyhow::Result;
use crate::tools::session_db::{compute_line_hash, SessionRepository};

pub struct FormattedLinesResult {
    pub lines_json: String,
    pub actual_end_line: usize,
    pub warning_msg: Option<String>,
}

pub fn retrieve_and_format_lines(
    repository: &impl SessionRepository,
    session_id: &str,
    start_line: usize,
    end_line: usize,
    only_ids: bool,
    wrap_trigger_length: usize,
) -> Result<FormattedLinesResult> {
    repository.ensure_hashes_range(session_id, start_line, end_line)?;

    let lines = repository.fetch_lines_range(session_id, start_line, end_line)?;
    let mut items = Vec::new();

    let mut actual_end_line = start_line.saturating_sub(1);
    let mut cumulative_bytes = 0;
    let mut capacity_truncated = false;

    for (current_idx, (seq_id, line_hash_opt, content)) in (start_line..).zip(lines) {
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

        let line_len = if only_ids {
            line_id.len() + 10
        } else {
            final_content.len()
        };

        if cumulative_bytes + line_len > 45000 {
            capacity_truncated = true;
            break;
        }

        cumulative_bytes += line_len;

        let item_str = if only_ids {
            format!("[\"{}\", {}]", line_id, current_idx)
        } else {
            let content_escaped = serde_json::to_string(&final_content)?;
            format!("[\"{}\", {}, {}]", line_id, current_idx, content_escaped)
        };

        items.push(item_str);
        actual_end_line = current_idx;
    }

    let warning_msg = if capacity_truncated {
        Some("Response truncated: cumulative response size limit (45,000 bytes) was reached.".to_string())
    } else {
        None
    };

    let mut lines_json = String::new();
    lines_json.push('[');

    if !items.is_empty() {
        if only_ids {
            lines_json.push('\n');
            let mut current_line = "  ".to_string();
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    let next_len = current_line.len() + 2 + item.len();
                    if next_len > wrap_trigger_length {
                        lines_json.push_str(&current_line);
                        lines_json.push_str(",\n");
                        current_line = format!("  {}", item);
                    } else {
                        current_line.push_str(", ");
                        current_line.push_str(item);
                    }
                } else {
                    current_line.push_str(item);
                }
            }
            lines_json.push_str(&current_line);
            lines_json.push('\n');
        } else {
            lines_json.push('\n');
            for (i, item) in items.iter().enumerate() {
                lines_json.push_str("  ");
                lines_json.push_str(item);
                if i < items.len() - 1 {
                    lines_json.push_str(",\n");
                } else {
                    lines_json.push('\n');
                }
            }
        }
    }

    if items.is_empty() {
        lines_json.push(']');
    } else {
        lines_json.push_str("  ]");
    }

    Ok(FormattedLinesResult {
        lines_json,
        actual_end_line,
        warning_msg,
    })
}

pub fn format_modified_ids(ids: &[String], wrap_trigger_length: usize) -> String {
    if ids.is_empty() {
        return "[]".to_string();
    }

    let mut result = String::new();
    result.push('[');
    result.push('\n');

    let mut current_line = "  ".to_string();
    for (i, id) in ids.iter().enumerate() {
        let item = serde_json::to_string(id).unwrap_or_else(|_| format!("\"{}\"", id));
        if i > 0 {
            let next_len = current_line.len() + 2 + item.len();
            if next_len > wrap_trigger_length {
                result.push_str(&current_line);
                result.push_str(",\n");
                current_line = format!("  {}", item);
            } else {
                current_line.push_str(", ");
                current_line.push_str(&item);
            }
        } else {
            current_line.push_str(&item);
        }
    }
    result.push_str(&current_line);
    result.push('\n');
    result.push(']');
    result
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::session_db::SqliteSessionRepository;
    use crate::tools::TEST_DB_LOCK as DB_LOCK;
    use std::fs;

    #[test]
    fn test_retrieve_and_format_lines_only_ids() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let temp_dir = std::env::temp_dir().join("line-editor-test-formatter-ids");
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir)?;
        }
        fs::create_dir_all(&temp_dir)?;
        let file_path = temp_dir.join("test.txt");
        let filepath_str = file_path.to_str().unwrap();

        let content = "line 1\nline 2\nline 3\nline 4";
        fs::write(filepath_str, content)?;

        let repository = SqliteSessionRepository;
        let meta = repository.init_session(filepath_str, false)?;
        let session_id = &meta.session_id;

        // Test with a small wrap_trigger_length to force wrapping
        let res = retrieve_and_format_lines(&repository, session_id, 1, 4, true, 30)?;
        assert_eq!(res.actual_end_line, 4);
        assert!(res.warning_msg.is_none());

        let val: serde_json::Value = serde_json::from_str(&res.lines_json)?;
        let arr = val.as_array().unwrap();
        assert_eq!(arr.len(), 4);
        assert_eq!(arr[0][1].as_u64().unwrap(), 1);
        assert_eq!(arr[3][1].as_u64().unwrap(), 4);

        let lines: Vec<&str> = res.lines_json.lines().collect();
        assert!(lines.len() > 3, "Expected formatting to wrap into multiple lines: {}", res.lines_json);
        assert_eq!(lines[0], "[");
        assert!(lines[1].starts_with("  "));
        assert_eq!(*lines.last().unwrap(), "  ]");

        // Test with large wrap_trigger_length so everything is on one line
        let res_no_wrap = retrieve_and_format_lines(&repository, session_id, 1, 4, true, 1000)?;
        let lines_no_wrap: Vec<&str> = res_no_wrap.lines_json.lines().collect();
        assert_eq!(lines_no_wrap.len(), 3);
        assert_eq!(lines_no_wrap[0], "[");
        assert_eq!(lines_no_wrap[2], "  ]");

        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }

    #[test]
    fn test_retrieve_and_format_lines_full() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let temp_dir = std::env::temp_dir().join("line-editor-test-formatter-full");
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir)?;
        }
        fs::create_dir_all(&temp_dir)?;
        let file_path = temp_dir.join("test.txt");
        let filepath_str = file_path.to_str().unwrap();

        let content = "line 1\nline 2";
        fs::write(filepath_str, content)?;

        let repository = SqliteSessionRepository;
        let meta = repository.init_session(filepath_str, false)?;
        let session_id = &meta.session_id;

        let res = retrieve_and_format_lines(&repository, session_id, 1, 2, false, 1000)?;
        assert_eq!(res.actual_end_line, 2);
        assert!(res.warning_msg.is_none());

        let val: serde_json::Value = serde_json::from_str(&res.lines_json)?;
        let arr = val.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0][1].as_u64().unwrap(), 1);
        assert_eq!(arr[0][2].as_str().unwrap(), "line 1");

        let lines: Vec<&str> = res.lines_json.lines().collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0], "[");
        assert_eq!(lines[3], "  ]");
        assert!(lines[1].ends_with(","));
        assert!(!lines[2].ends_with(","));

        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }

    #[test]
    fn test_retrieve_and_format_lines_truncation() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let temp_dir = std::env::temp_dir().join("line-editor-test-formatter-trunc");
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir)?;
        }
        fs::create_dir_all(&temp_dir)?;
        let file_path = temp_dir.join("test.txt");
        let filepath_str = file_path.to_str().unwrap();

        let long_line = "A".repeat(2500);
        fs::write(filepath_str, &long_line)?;

        let repository = SqliteSessionRepository;
        let meta = repository.init_session(filepath_str, false)?;
        let session_id = &meta.session_id;

        let res = retrieve_and_format_lines(&repository, session_id, 1, 1, false, 1000)?;
        let val: serde_json::Value = serde_json::from_str(&res.lines_json)?;
        let arr = val.as_array().unwrap();
        let line_id = arr[0][0].as_str().unwrap();
        let content = arr[0][2].as_str().unwrap();

        assert!(line_id.ends_with("#TRUNC"));
        assert!(content.contains("[TRUNCATED:"));

        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }

    #[test]
    fn test_retrieve_and_format_lines_capacity() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let temp_dir = std::env::temp_dir().join("line-editor-test-formatter-cap");
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir)?;
        }
        fs::create_dir_all(&temp_dir)?;
        let file_path = temp_dir.join("test.txt");
        let filepath_str = file_path.to_str().unwrap();

        let line_content = "B".repeat(1000);
        let mut lines = Vec::new();
        for _ in 1..=50 {
            lines.push(line_content.clone());
        }
        fs::write(filepath_str, lines.join("\n"))?;

        let repository = SqliteSessionRepository;
        let meta = repository.init_session(filepath_str, false)?;
        let session_id = &meta.session_id;

        let res = retrieve_and_format_lines(&repository, session_id, 1, 50, false, 1000)?;
        assert_eq!(res.actual_end_line, 45);
        assert_eq!(res.warning_msg.as_deref(), Some("Response truncated: cumulative response size limit (45,000 bytes) was reached."));

        let val: serde_json::Value = serde_json::from_str(&res.lines_json)?;
        assert_eq!(val.as_array().unwrap().len(), 45);

        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }

    #[test]
    fn test_format_modified_ids_empty() {
        let ids: Vec<String> = vec![];
        assert_eq!(format_modified_ids(&ids, 80), "[]");
    }

    #[test]
    fn test_format_modified_ids_wrap() {
        let ids = vec![
            "1#77cf".to_string(),
            "2#bcb4".to_string(),
            "3#c2b7".to_string(),
        ];
        // small trigger -> wraps
        let res_wrap = format_modified_ids(&ids, 15);
        let expected_wrap = "[\n  \"1#77cf\",\n  \"2#bcb4\",\n  \"3#c2b7\"\n]";
        assert_eq!(res_wrap, expected_wrap);

        // large trigger -> no wrap
        let res_no_wrap = format_modified_ids(&ids, 100);
        let expected_no_wrap = "[\n  \"1#77cf\", \"2#bcb4\", \"3#c2b7\"\n]";
        assert_eq!(res_no_wrap, expected_no_wrap);
    }

}
