use crate::tools::formatter;
use crate::tools::repository::FileStore;
use anyhow::Result;

pub(crate) struct ViewLinesOutput {
    pub lines_text: Option<String>,
    pub metadata_json: String,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn view_lines(
    repository: &impl FileStore,
    filepath: &str,
    start_line: Option<usize>,
    end_line: Option<usize>,
    only_ids: Option<bool>,
    query: Option<String>,
    context_lines: Option<usize>,
    fixed_string: Option<bool>,
) -> Result<ViewLinesOutput> {
    let meta = repository.init_session(filepath, false)?;
    let file_key = meta.file_key;

    let total_lines = repository.get_total_lines(&file_key)?;

    // Fetch enclosing contexts map and absolute ranges
    let conn = crate::tools::store::get_db_connection()?;
    let all_parent_contexts = {
        let mut stmt = conn
            .prepare("SELECT parent_context FROM lines WHERE file_key = ?1 ORDER BY sort_order")?;
        let mut rows = stmt.query(rusqlite::params![file_key])?;
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
            let entry = context_ranges
                .entry(ctx.clone())
                .or_insert((idx + 1, idx + 1));
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

        // The query is a regular expression, so `(?i)` at its front is how a
        // search is made case-insensitive, and a search for text that reads as
        // a pattern asks for it to be taken literally.
        let pattern = if fixed_string.unwrap_or(false) {
            regex::Regex::new(&regex::escape(q))
        } else {
            regex::Regex::new(q)
        }
        .map_err(|err| {
            anyhow::anyhow!(
                "'{}' is not a regular expression: {}. Pass fixed_string to search for it literally.",
                q,
                err
            )
        })?;
        let matches = repository.find_matching_lines(&file_key, &pattern)?;

        if matches.is_empty() {
            let config = crate::tools::metadata::get_config();
            let msg = serde_json::to_string(&config.error_no_query_match.replace("{}", q))?;
            let metadata_json = format!(
                "{{\n  \"enclosing_contexts\": [],\n  \"showing_start\": 1,\n  \"showing_end\": 0,\n  \"total_lines\": {},\n  \"total_bytes\": {},\n  \"message\": {}\n}}",
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
        // One line past what may be printed, so the read that applies the cap
        // is the one that sees it reached and says so. Stopping at the cap here
        // would leave that read unable to fail, and bounding the fetch is the
        // only thing this number is for.
        let end = end_line.unwrap_or_else(|| {
            if total_lines == 0 {
                start
            } else {
                std::cmp::min(total_lines, start.saturating_add(formatter::LINE_CAP))
            }
        });
        if start > end {
            anyhow::bail!(
                "Invalid bounds: start_line ({}) cannot be greater than end_line ({})",
                start,
                end
            );
        }
        // The range goes to the formatter as asked for. It applies the line cap
        // while printing, and narrowing here as well left that one unable to
        // fail: a cap that never decides makes the cap that does untestable.
        intervals.push((start, end));
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
            &file_key,
            *start,
            *end,
            only_ids_bool,
            config.only_ids_wrap_trigger_length,
            formatter::LINE_CAP,
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

    let mut message = None;
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
    parts.push(format!(
        "  \"enclosing_contexts\": {}",
        serde_json::to_string(&enclosing_list)?
    ));
    if only_ids_bool {
        parts.push(format!("  \"lines\": {}", serde_json::to_value(&all_ids)?));
    }
    if let Some(msg) = message {
        parts.push(format!("  \"message\": {}", serde_json::to_string(&msg)?));
    }
    parts.push(format!("  \"showing_end\": {}", actual_end));
    parts.push(format!(
        "  \"showing_start\": {}",
        actual_start.unwrap_or(1)
    ));
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

#[cfg(test)]
mod tests {
    use super::*;
    // A view test needs a file to view, and `create` is how one is written.
    use crate::tools::create::create_lines;
    use crate::tools::repository::SqliteFileStore;
    use crate::tools::TEST_DB_LOCK as DB_LOCK;
    use std::fs;

    fn test_view_lines(
        repository: &impl FileStore,
        filepath: &str,
        start_line: usize,
        end_line: usize,
        only_ids: Option<bool>,
    ) -> Result<String> {
        let res = view_lines(
            repository,
            filepath,
            Some(start_line),
            Some(end_line),
            only_ids,
            None,
            None,
            None,
        )?;
        let ids_val: serde_json::Value = serde_json::from_str(&res.metadata_json)?;

        let mut lines = Vec::new();
        if let Some(ref text) = res.lines_text {
            let mut current_id = String::new();
            let mut current_n = 0;
            let mut current_content = String::new();
            let mut has_pending = false;

            for row in text.lines() {
                let colon_idx = row.find(':').unwrap();
                let head = row[..colon_idx].trim();
                let content = &row[colon_idx + 2..];

                match head.split_once('|') {
                    // A line names itself; anything else is its continuation.
                    Some((id, number)) => {
                        if has_pending {
                            lines.push(serde_json::json!([current_id, current_n, current_content]));
                        }
                        current_id = id.trim().to_string();
                        current_n = number.trim().parse::<usize>().unwrap();
                        current_content = content.to_string();
                        has_pending = true;
                    }
                    None => current_content.push_str(content),
                }
            }
            if has_pending {
                lines.push(serde_json::json!([current_id, current_n, current_content]));
            }
        } else {
            for id_entry in ids_val["lines"].as_array().unwrap() {
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
    fn test_view_lines_capping() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let temp_dir = crate::tools::test_temp_dir("line-editor-test-capping");
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
        let repository = SqliteFileStore;
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
        let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let temp_dir = crate::tools::test_temp_dir("line-editor-test-truncation");
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir)?;
        }
        fs::create_dir_all(&temp_dir)?;
        let file_path = temp_dir.join("truncation_file.txt");
        let filepath_str = file_path.to_str().unwrap();

        // Line 1: normal, Line 2: 2500 characters, Line 3: normal
        let long_line = "A".repeat(2500);
        let content = format!("short 1\n{}\nshort 3", long_line);
        let repository = SqliteFileStore;
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
        assert!(!lines_array_only_ids[1][0]
            .as_str()
            .unwrap()
            .ends_with("#TRUNC"));
        assert_eq!(lines_array_only_ids[1].as_array().unwrap().len(), 2); // [id, n], content omitted

        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }

    #[test]
    fn test_view_lines_capacity() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let temp_dir = crate::tools::test_temp_dir("line-editor-test-capacity");
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
        let repository = SqliteFileStore;
        let _ = create_lines(&repository, filepath_str, &content, None)?;

        let output = test_view_lines(&repository, filepath_str, 1, 50, None)?;
        let val: serde_json::Value = serde_json::from_str(&output)?;

        let lines_array = val["lines"].as_array().unwrap();
        // 44 lines of 1000 characters, rather than 45: the id each line
        // carries counts against the same 45,000 bytes.
        assert_eq!(lines_array.len(), 44);
        assert_eq!(val["showing_end"], 44);
        assert!(val["message"]
            .as_str()
            .unwrap()
            .contains("cumulative response size limit"));

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
        let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let temp_dir = crate::tools::test_temp_dir("line-editor-test-contexts-query");
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

        let repository = SqliteFileStore;

        let res = view_lines(
            &repository,
            filepath_str,
            None,
            None,
            None,
            Some("field".to_string()),
            Some(1),
            None,
        )?;

        let val: serde_json::Value = serde_json::from_str(&res.metadata_json)?;

        let enclosing = val["enclosing_contexts"].as_array().unwrap();
        assert!(enclosing
            .iter()
            .any(|item| item["name"].as_str().unwrap() == "impl MyStruct"));
        assert!(enclosing
            .iter()
            .any(|item| item["name"].as_str().unwrap() == "fn:new"));
        assert!(enclosing
            .iter()
            .any(|item| item["name"].as_str().unwrap() == "fn:get_field"));

        let get_field_ctx = enclosing
            .iter()
            .find(|item| item["name"].as_str().unwrap() == "fn:get_field")
            .unwrap();
        assert_eq!(get_field_ctx["start"].as_u64().unwrap(), 10);
        assert_eq!(get_field_ctx["end"].as_u64().unwrap(), 12);

        let text = res.lines_text.unwrap();
        assert!(text.contains("..."));

        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn test_markdown_parent_contexts() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let temp_dir = crate::tools::test_temp_dir("line-editor-test-markdown-contexts");
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

        let repository = SqliteFileStore;
        let res = view_lines(
            &repository,
            filepath_str,
            Some(1),
            Some(8),
            None,
            None,
            None,
            None,
        )?;

        let val: serde_json::Value = serde_json::from_str(&res.metadata_json)?;
        let enclosing = val["enclosing_contexts"].as_array().unwrap();

        assert!(enclosing
            .iter()
            .any(|item| item["name"].as_str().unwrap() == "# Main Title"));
        assert!(enclosing
            .iter()
            .any(|item| item["name"].as_str().unwrap() == "## Section 1"));
        assert!(enclosing
            .iter()
            .any(|item| item["name"].as_str().unwrap() == "### Subsection 1.1"));

        let sec1 = enclosing
            .iter()
            .find(|item| item["name"].as_str().unwrap() == "## Section 1")
            .unwrap();
        assert_eq!(sec1["start"].as_u64().unwrap(), 5);
        assert_eq!(sec1["end"].as_u64().unwrap(), 7);

        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }
}
