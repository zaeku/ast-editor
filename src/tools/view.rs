use anyhow::Result;
use crate::tools::session_db::SessionRepository;

pub struct ViewLinesOutput {
    pub lines_text: Option<String>,
    pub metadata_json: String,
}

pub fn view_lines(
    repository: &impl SessionRepository,
    filepath: &str,
    start_line: Option<usize>,
    end_line: Option<usize>,
    only_ids: Option<bool>,
    query: Option<String>,
    context_lines: Option<usize>,
) -> Result<ViewLinesOutput> {
    let meta = repository.init_session(filepath, false)?;
    let session_id = meta.session_id;

    let total_lines = repository.get_total_lines(&session_id)?;

    // Fetch enclosing contexts map and absolute ranges
    let conn = crate::tools::session_db::get_db_connection()?;
    let all_parent_contexts = {
        let mut stmt = conn.prepare(
            "SELECT parent_context FROM lines WHERE session_id = ?1 ORDER BY sort_order"
        )?;
        let mut rows = stmt.query(rusqlite::params![session_id])?;
        let mut contexts = Vec::new();
        while let Some(row) = rows.next()? {
            let ctx: Option<String> = row.get(0)?;
            contexts.push(ctx);
        }
        contexts
    };

    let mut context_ranges = std::collections::HashMap::new();
    for (idx, ctx_opt) in all_parent_contexts.iter().enumerate() {
        if let Some(ctx) = ctx_opt {
            let entry = context_ranges.entry(ctx.clone()).or_insert((idx + 1, idx + 1));
            entry.1 = idx + 1;
        }
    }

    // Determine target intervals
    let mut intervals = Vec::new();
    if let Some(ref q) = query {
        if q.is_empty() {
            anyhow::bail!("Invalid query: query string cannot be empty");
        }
        let ctx_lines = context_lines.unwrap_or(5);
        
        // Find matching lines
        let matches = repository.find_matching_lines(&session_id, q)?;

        if matches.is_empty() {
            let config = crate::tools::metadata::get_config();
            let msg = config.error_no_query_match.replace("{}", &q.replace("\"", "\\\""));
            let metadata_json = format!(
                "{{\n  \"enclosing_contexts\": [],\n  \"ids\": [],\n  \"showing_start\": 1,\n  \"showing_end\": 0,\n  \"total_lines\": {},\n  \"total_bytes\": {},\n  \"message\": \"{}\"\n}}",
                total_lines,
                std::path::Path::new(filepath).metadata()?.len(),
                msg
            );
            return Ok(ViewLinesOutput {
                lines_text: Some(String::new()),
                metadata_json,
            });
        }

        for m in matches {
            let start = if m > ctx_lines { m - ctx_lines } else { 1 };
            let end = std::cmp::min(total_lines, m + ctx_lines);
            intervals.push((start, end));
        }

        // Sort and merge intervals
        intervals.sort_by_key(|val| val.0);
        let mut merged: Vec<(usize, usize)> = Vec::new();
        for (start, end) in intervals {
            if let Some(last) = merged.last_mut() {
                if start <= last.1 + 1 {
                    last.1 = std::cmp::max(last.1, end);
                } else {
                    merged.push((start, end));
                }
            } else {
                merged.push((start, end));
            }
        }
        intervals = merged;
    } else {
        let start = start_line.unwrap_or(1);
        if start == 0 {
            anyhow::bail!("Invalid bounds: start_line must be greater than 0");
        }
        let end = end_line.unwrap_or_else(|| {
            if total_lines == 0 {
                start
            } else {
                std::cmp::min(total_lines, start.saturating_add(799))
            }
        });
        if start > end {
            anyhow::bail!("Invalid bounds: start_line ({}) cannot be greater than end_line ({})", start, end);
        }
        let requested_len = if end >= start { end - start + 1 } else { 0 };
        let capped_end = if requested_len > 800 {
            start.saturating_add(799)
        } else {
            end
        };
        intervals.push((start, capped_end));
    }

    let only_ids_bool = only_ids.unwrap_or(false);
    let config = crate::tools::metadata::get_config();

    let mut all_formatted_text = Vec::new();
    let mut all_ids = Vec::new();
    let mut warning_messages = Vec::new();
    let mut actual_start = None;
    let mut actual_end = 0;

    for (idx, (start, end)) in intervals.iter().enumerate() {
        let formatted_res = crate::tools::formatter::retrieve_and_format_lines(
            repository,
            &session_id,
            *start,
            *end,
            only_ids_bool,
            config.only_ids_wrap_trigger_length,
        )?;

        if actual_start.is_none() {
            actual_start = Some(*start);
        }
        actual_end = formatted_res.actual_end_line;

        if let Some(ref text) = formatted_res.lines_text {
            all_formatted_text.push(text.clone());
        }

        let val_ids: Vec<serde_json::Value> = serde_json::from_str(&formatted_res.ids_json)?;
        all_ids.extend(val_ids);

        if let Some(ref cap_msg) = formatted_res.warning_msg {
            warning_messages.push(cap_msg.clone());
        }

        if idx < intervals.len() - 1 && !only_ids_bool {
            all_formatted_text.push("...".to_string());
        }
    }

    let config = crate::tools::metadata::get_config();
    let line_count_capped = query.is_none() && (end_line.unwrap_or(0) - start_line.unwrap_or(1) + 1 > 800) && total_lines > 800;
    let mut message = None;
    if line_count_capped {
        message = Some(config.warning_line_limit_exceeded.clone());
    }
    for cap_msg in warning_messages {
        if let Some(existing_msg) = message {
            message = Some(format!("{}; {}", existing_msg, cap_msg));
        } else {
            message = Some(cap_msg.clone());
        }
    }

    // Determine active enclosing contexts
    let mut active_contexts = std::collections::HashSet::new();
    for (s, e) in &intervals {
        for line_num in *s..=*e {
            if let Some(Some(ctx)) = all_parent_contexts.get(line_num - 1) {
                active_contexts.insert(ctx.clone());
            }
        }
    }

    let mut enclosing_list = Vec::new();
    for ctx_name in active_contexts {
        if let Some(&(s, e)) = context_ranges.get(&ctx_name) {
            enclosing_list.push(serde_json::json!({
                "name": ctx_name,
                "start": s,
                "end": e
            }));
        }
    }
    // Sort enclosing_list by start line for stability
    enclosing_list.sort_by_key(|v| v["start"].as_u64().unwrap_or(0));

    let total_bytes = std::path::Path::new(filepath).metadata()?.len();

    let mut parts = Vec::new();
    parts.push(format!("  \"enclosing_contexts\": {}", serde_json::to_string(&enclosing_list)?));
    parts.push(format!("  \"ids\": {}", serde_json::to_value(&all_ids)?));
    if let Some(msg) = message {
        parts.push(format!("  \"message\": {}", serde_json::to_string(&msg)?));
    }
    parts.push(format!("  \"showing_end\": {}", actual_end));
    parts.push(format!("  \"showing_start\": {}", actual_start.unwrap_or(1)));
    parts.push(format!("  \"tip\": {}", serde_json::to_string(&config.view_lines_response_tip)?));
    parts.push(format!("  \"total_bytes\": {}", total_bytes));
    parts.push(format!("  \"total_lines\": {}", total_lines));

    let metadata_json = format!("{{\n{}\n}}", parts.join(",\n"));
    let lines_text = if only_ids_bool {
        None
    } else {
        Some(all_formatted_text.join("\n"))
    };

    Ok(ViewLinesOutput {
        lines_text,
        metadata_json,
    })
}

#[deprecated(since = "0.1.0", note = "use view_lines instead")]
pub fn view_session_lines(
    repository: &impl SessionRepository,
    filepath: &str,
    start_line: usize,
    end_line: usize,
    only_ids: Option<bool>,
) -> Result<ViewLinesOutput> {
    view_lines(repository, filepath, Some(start_line), Some(end_line), only_ids, None, None)
}

pub fn create_lines(
    repository: &impl SessionRepository,
    filepath: &str,
    content: &str,
    return_ids: Option<bool>,
) -> Result<String> {
    let path = std::path::Path::new(filepath);
    if path.exists() {
        anyhow::bail!("FILE_ALREADY_EXISTS: File already exists at: {}", filepath);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    std::fs::write(filepath, content)?;

    let return_ids_bool = return_ids.unwrap_or(false);

    let setup_db_and_fetch = || -> Result<(Option<String>, Option<String>, usize, u64)> {
        let meta = repository.init_session(filepath, false)?;
        let total_bytes = std::path::Path::new(filepath).metadata()?.len();
        if return_ids_bool {
            if meta.total_lines > 0 {
                let end_line = if meta.total_lines > 800 { 800 } else { meta.total_lines };
                let config = crate::tools::metadata::get_config();
                let formatted_res = crate::tools::formatter::retrieve_and_format_lines(
                    repository,
                    &meta.session_id,
                    1,
                    end_line,
                    true,
                    config.only_ids_wrap_trigger_length,
                )?;
                Ok((Some(formatted_res.ids_json), formatted_res.warning_msg, meta.total_lines, total_bytes))
            } else {
                Ok((Some("[]".to_string()), None, 0, total_bytes))
            }
        } else {
            Ok((None, None, meta.total_lines, total_bytes))
        }
    };

    match setup_db_and_fetch() {
        Ok((items_opt, capacity_warning, total_lines, total_bytes)) => {
            let config = crate::tools::metadata::get_config();
            let line_count_capped = return_ids_bool && total_lines > 800;
            let mut warning_parts = Vec::new();
            if line_count_capped {
                warning_parts.push(config.warning_line_limit_exceeded.as_str());
            }
            if let Some(ref cap_msg) = capacity_warning {
                warning_parts.push(cap_msg.as_str());
            }

            let mut message = config.status_file_created.clone();
            if !warning_parts.is_empty() {
                message = format!("{} Truncation details: {}", message, warning_parts.join("; "));
            }

            let output = if return_ids_bool {
                let ids: Vec<String> = items_opt
                    .and_then(|json_str| serde_json::from_str::<serde_json::Value>(&json_str).ok())
                    .and_then(|val| val.as_array().cloned())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|val| {
                                val.as_array()
                                    .and_then(|item| item.first())
                                    .and_then(|id_val| id_val.as_str())
                                    .map(|s| s.to_string())
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let formatted_ids = crate::tools::formatter::format_modified_ids(&ids, config.only_ids_wrap_trigger_length);
                let indented_ids = formatted_ids.replace("\n", "\n  ");
                format!(
                    "{{\n  \"ids\": {},\n  \"message\": {},\n  \"status\": \"success\",\n  \"total_bytes\": {},\n  \"total_lines\": {}\n}}",
                    indented_ids,
                    serde_json::to_string(&message)?,
                    total_bytes,
                    total_lines
                )
            } else {
                format!(
                    "{{\n  \"message\": {},\n  \"status\": \"success\",\n  \"total_bytes\": {},\n  \"total_lines\": {}\n}}",
                    serde_json::to_string(&message)?,
                    total_bytes,
                    total_lines
                )
            };
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

    fn test_view_lines(
        repository: &impl SessionRepository,
        filepath: &str,
        start_line: usize,
        end_line: usize,
        only_ids: Option<bool>,
    ) -> Result<String> {
        let res = view_lines(repository, filepath, Some(start_line), Some(end_line), only_ids, None, None)?;
        let ids_val: serde_json::Value = serde_json::from_str(&res.metadata_json)?;
        let ids = ids_val["ids"].as_array().unwrap();
        
        let mut lines = Vec::new();
        if let Some(ref text) = res.lines_text {
            let mut current_id = String::new();
            let mut current_n = 0;
            let mut current_content = String::new();
            let mut has_pending = false;

            for line in text.lines() {
                let colon_idx = line.find(':').unwrap();
                let prefix = line[..colon_idx].trim();
                let content = &line[colon_idx + 2..];

                if prefix.chars().all(|c| c.is_ascii_digit()) && !prefix.is_empty() {
                    if has_pending {
                        lines.push(serde_json::json!([current_id, current_n, current_content]));
                    }
                    current_n = prefix.parse::<usize>().unwrap();
                    current_content = content.to_string();
                    current_id = String::new();
                    for id_entry in ids {
                        let id_arr = id_entry.as_array().unwrap();
                        if id_arr[1].as_u64().unwrap() as usize == current_n {
                            current_id = id_arr[0].as_str().unwrap().to_string();
                            break;
                        }
                    }
                    has_pending = true;
                } else {
                    current_content.push_str(content);
                }
            }
            if has_pending {
                lines.push(serde_json::json!([current_id, current_n, current_content]));
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
        let output = create_lines(&repository, filepath_str, content, Some(true))?;

        let val: serde_json::Value = serde_json::from_str(&output)?;
        assert_eq!(val["status"], "success");
        assert_eq!(val["message"], "File successfully created and line editing session initialized.");
        assert!(val["columns"].is_null());
        assert!(val["lines"].is_null());

        let ids = val["ids"].as_array().unwrap();
        assert_eq!(ids.len(), 3);
        assert!(ids[0].as_str().unwrap().starts_with("1#"));
        assert!(ids[1].as_str().unwrap().starts_with("2#"));
        assert!(ids[2].as_str().unwrap().starts_with("3#"));
        assert_eq!(val["total_lines"].as_u64().unwrap(), 3);
        assert_eq!(val["total_bytes"].as_u64().unwrap(), content.len() as u64);

        // Verify that the file was indeed written
        let read_content = fs::read_to_string(filepath_str)?;
        assert_eq!(read_content, content);

        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }

    #[test]
    fn test_create_lines_return_ids_false() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let temp_dir = std::env::temp_dir().join("line-editor-test-create-return-ids-false");
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir)?;
        }
        fs::create_dir_all(&temp_dir)?;
        let file_path = temp_dir.join("new_file.txt");
        let filepath_str = file_path.to_str().unwrap();

        let content = "line 1\nline 2\nline 3";
        let repository = SqliteSessionRepository;
        let output = create_lines(&repository, filepath_str, content, Some(false))?;

        let val: serde_json::Value = serde_json::from_str(&output)?;
        assert_eq!(val["status"], "success");
        assert_eq!(val["message"], "File successfully created and line editing session initialized.");
        assert!(val["ids"].is_null());
        assert_eq!(val["total_lines"].as_u64().unwrap(), 3);
        assert_eq!(val["total_bytes"].as_u64().unwrap(), content.len() as u64);

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
        let res = create_lines(&repository, filepath_str, "new content", None);
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
        let res = create_lines(&repository, filepath_str, content, None);

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
        let _ = create_lines(&repository, filepath_str, &content, None)?;

        // View lines from 1 to 1000
        let output = test_view_lines(&repository, filepath_str, 1, 1000, None)?;
        let val: serde_json::Value = serde_json::from_str(&output)?;

        assert_eq!(val["total_lines"], 1000);
        assert_eq!(val["showing_start"], 1);
        assert_eq!(val["showing_end"], 800);
        assert!(val["message"].as_str().unwrap().contains("800 lines max"));

        let lines_array = val["lines"].as_array().unwrap();
        assert_eq!(lines_array.len(), 800);
        assert_eq!(lines_array[0][2], "line 1");
        assert_eq!(lines_array[799][2], "line 800");

        // View lines with only_ids = true
        let output_only_ids = test_view_lines(&repository, filepath_str, 1, 1000, Some(true))?;
        let val_only_ids: serde_json::Value = serde_json::from_str(&output_only_ids)?;
        assert_eq!(val_only_ids["columns"], serde_json::json!(["id", "n"]));
        let lines_array_only_ids = val_only_ids["lines"].as_array().unwrap();
        assert_eq!(lines_array_only_ids.len(), 800);
        assert_eq!(lines_array_only_ids[0].as_array().unwrap().len(), 2);

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
        let _ = create_lines(&repository, filepath_str, &content, None)?;

        let output = test_view_lines(&repository, filepath_str, 1, 3, None)?;
        let val: serde_json::Value = serde_json::from_str(&output)?;

        let lines_array = val["lines"].as_array().unwrap();
        assert_eq!(lines_array.len(), 3);
        assert_eq!(lines_array[0][2], "short 1");
        assert!(lines_array[0][0].as_str().unwrap().contains('#'));
        assert!(!lines_array[0][0].as_str().unwrap().ends_with("#TRUNC"));

        // Wrapped line accumulated content:
        let trunc_content = lines_array[1][2].as_str().unwrap();
        assert_eq!(trunc_content, &"A".repeat(2500));
        assert!(!lines_array[1][0].as_str().unwrap().ends_with("#TRUNC"));

        assert_eq!(lines_array[2][2], "short 3");

        // With only_ids = true, it should return normal IDs
        let output_only_ids = test_view_lines(&repository, filepath_str, 1, 3, Some(true))?;
        let val_only_ids: serde_json::Value = serde_json::from_str(&output_only_ids)?;
        let lines_array_only_ids = val_only_ids["lines"].as_array().unwrap();
        assert_eq!(lines_array_only_ids.len(), 3);
        assert!(!lines_array_only_ids[1][0].as_str().unwrap().ends_with("#TRUNC"));
        assert_eq!(lines_array_only_ids[1].as_array().unwrap().len(), 2); // [id, n], content omitted

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
        let _ = create_lines(&repository, filepath_str, &content, None)?;

        let output = test_view_lines(&repository, filepath_str, 1, 50, None)?;
        let val: serde_json::Value = serde_json::from_str(&output)?;

        let lines_array = val["lines"].as_array().unwrap();
        // Since limit is 45,000 bytes, we should stop before exceeding 45,000 bytes.
        // 45 lines * 1000 chars = 45,000 chars.
        assert_eq!(lines_array.len(), 45);
        assert_eq!(val["showing_end"], 45);
        assert!(val["message"].as_str().unwrap().contains("cumulative response size limit"));

        // With only_ids = true, the limit of 45,000 bytes should NOT be exceeded
        let output_only_ids = test_view_lines(&repository, filepath_str, 1, 50, Some(true))?;
        let val_only_ids: serde_json::Value = serde_json::from_str(&output_only_ids)?;
        let lines_array_only_ids = val_only_ids["lines"].as_array().unwrap();
        assert_eq!(lines_array_only_ids.len(), 50);
        assert_eq!(val_only_ids["showing_end"], 50);
        assert!(val_only_ids["message"].is_null());

        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn test_enclosing_contexts_and_query_filtering() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let temp_dir = std::env::temp_dir().join("line-editor-test-contexts-query");
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir)?;
        }
        fs::create_dir_all(&temp_dir)?;
        let file_path = temp_dir.join("code.rs");
        let filepath_str = file_path.to_str().unwrap();

        let code = r#"struct MyStruct {
    field: i32,
}

impl MyStruct {
    pub fn new() -> Self {
        MyStruct { field: 42 }
    }

    pub fn get_field(&self) -> i32 {
        self.field
    }
}

fn helper_func() {
    println!("helper");
}
"#;
        fs::write(filepath_str, code)?;

        let repository = SqliteSessionRepository;

        let res = view_lines(
            &repository,
            filepath_str,
            None,
            None,
            None,
            Some("field".to_string()),
            Some(1),
        )?;

        let val: serde_json::Value = serde_json::from_str(&res.metadata_json)?;
        
        let enclosing = val["enclosing_contexts"].as_array().unwrap();
        assert!(enclosing.iter().any(|item| item["name"].as_str().unwrap() == "impl MyStruct"));
        assert!(enclosing.iter().any(|item| item["name"].as_str().unwrap() == "fn:new"));
        assert!(enclosing.iter().any(|item| item["name"].as_str().unwrap() == "fn:get_field"));

        let get_field_ctx = enclosing.iter().find(|item| item["name"].as_str().unwrap() == "fn:get_field").unwrap();
        assert_eq!(get_field_ctx["start"].as_u64().unwrap(), 10);
        assert_eq!(get_field_ctx["end"].as_u64().unwrap(), 12);

        let text = res.lines_text.unwrap();
        assert!(text.contains("..."));
        
        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn test_markdown_parent_contexts() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let temp_dir = std::env::temp_dir().join("line-editor-test-markdown-contexts");
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir)?;
        }
        fs::create_dir_all(&temp_dir)?;
        let file_path = temp_dir.join("doc.md");
        let filepath_str = file_path.to_str().unwrap();

        let doc = r#"# Main Title
Intro text

## Section 1
More text here

### Subsection 1.1
Details here
"#;
        fs::write(filepath_str, doc)?;

        let repository = SqliteSessionRepository;
        let res = view_lines(
            &repository,
            filepath_str,
            Some(1),
            Some(8),
            None,
            None,
            None,
        )?;

        let val: serde_json::Value = serde_json::from_str(&res.metadata_json)?;
        let enclosing = val["enclosing_contexts"].as_array().unwrap();

        assert!(enclosing.iter().any(|item| item["name"].as_str().unwrap() == "# Main Title"));
        assert!(enclosing.iter().any(|item| item["name"].as_str().unwrap() == "## Section 1"));
        assert!(enclosing.iter().any(|item| item["name"].as_str().unwrap() == "### Subsection 1.1"));

        let sec1 = enclosing.iter().find(|item| item["name"].as_str().unwrap() == "## Section 1").unwrap();
        assert_eq!(sec1["start"].as_u64().unwrap(), 5);
        assert_eq!(sec1["end"].as_u64().unwrap(), 7);

        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }
}

