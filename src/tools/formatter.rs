use crate::tools::session_db::{compute_line_hash, SessionRepository};
use anyhow::Result;

/// Where a line is broken for display. The break is at a fixed count of
/// characters, never at a space, so the rows of one line concatenate back to
/// exactly what the file holds. Wide enough that ordinary code is one row and
/// a search across the output is not split by it.
const WRAP_WIDTH: usize = 300;

/// What the caps count, which is not what a row is: lowering the display width
/// must not make a response run out of budget ten times sooner.
pub const SEGMENT_LENGTH: usize = 2048;

/// How much one call will answer with. The documents render both figures
/// rather than repeating them.
pub const LINE_CAP: usize = 800;
pub const RESPONSE_BYTE_CAP: usize = 45_000;

pub struct FormattedLinesResult {
    pub lines_text: Option<String>,
    pub ids_json: String,
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
    let lines = repository.fetch_lines_range(session_id, start_line, end_line)?;
    let mut id_items = Vec::new();
    let mut pending: Vec<(String, usize, Vec<String>)> = Vec::new();

    let mut actual_end_line = start_line.saturating_sub(1);
    let mut cumulative_bytes = 0;
    let mut capacity_truncated = false;
    let mut segment_count = 0;
    let mut line_cap_reached = false;

    for (current_idx, (seq_id, line_hash_opt, content)) in (start_line..).zip(lines) {
        let line_hash = line_hash_opt.unwrap_or_else(|| compute_line_hash(&content));
        let line_id = format!("{:x}#{}", seq_id, line_hash);

        let chars: Vec<char> = content.chars().collect();
        let mut segments = Vec::new();
        let mut start = 0;
        while start < chars.len() {
            let end = std::cmp::min(start + SEGMENT_LENGTH, chars.len());
            let segment: String = chars[start..end].iter().collect();
            segments.push(segment);
            start = end;
        }
        if segments.is_empty() {
            segments.push(String::new());
        }

        let mut segments_to_add = segments.len();
        if segment_count + segments_to_add > LINE_CAP {
            segments_to_add = LINE_CAP - segment_count;
            line_cap_reached = true;
        }

        if segments_to_add == 0 {
            break;
        }

        let mut line_len = line_id.len() + 10;
        if !only_ids {
            for seg in segments.iter().take(segments_to_add) {
                line_len += seg.len();
            }
        }

        if cumulative_bytes + line_len > RESPONSE_BYTE_CAP {
            capacity_truncated = true;
            break;
        }

        cumulative_bytes += line_len;

        id_items.push(format!("[\"{}\", {}]", line_id, current_idx));
        if !only_ids {
            let kept = &chars[..std::cmp::min(chars.len(), segments_to_add * SEGMENT_LENGTH)];
            let rows: Vec<String> = if kept.is_empty() {
                vec![String::new()]
            } else {
                kept.chunks(WRAP_WIDTH)
                    .map(|row| row.iter().collect())
                    .collect()
            };
            pending.push((line_id.clone(), current_idx, rows));
        }
        segment_count += segments_to_add;
        actual_end_line = current_idx;

        if line_cap_reached {
            break;
        }
    }

    // One column for the id, one for the number, so a line's own indentation
    // reads true: with a ragged prefix two lines indented the same start at
    // different columns and the block looks like something it is not. The
    // padding is all before the colon — a space after it is the file's.
    let id_width = pending.iter().map(|(id, _, _)| id.len()).max().unwrap_or(0);
    let number_width = pending
        .iter()
        .map(|(_, number, _)| number.to_string().len())
        .max()
        .unwrap_or(0);
    let indent = " ".repeat(id_width + number_width);
    let mut text_lines = Vec::new();
    for (line_id, number, rows) in &pending {
        let last = rows.len() - 1;
        for (row_idx, row) in rows.iter().enumerate() {
            if row_idx == 0 {
                text_lines.push(format!(
                    "{:<id_width$}|{:>number_width$}: {}",
                    line_id, number, row
                ));
            } else if row_idx == last {
                text_lines.push(format!("{}└: {}", indent, row));
            } else {
                text_lines.push(format!("{}│: {}", indent, row));
            }
        }
    }

    let config = crate::tools::metadata::get_config();
    let warning_msg = if capacity_truncated {
        Some(config.warning_cumulative_limit.clone())
    } else if line_cap_reached {
        Some(config.warning_line_cap.clone())
    } else {
        None
    };

    // Format ids_json in a nice wrapped array
    let mut ids_json = String::new();
    ids_json.push('[');
    if !id_items.is_empty() {
        ids_json.push('\n');
        let mut current_line = "  ".to_string();
        for (i, item) in id_items.iter().enumerate() {
            if i > 0 {
                let next_len = current_line.len() + 2 + item.len();
                if next_len > wrap_trigger_length {
                    ids_json.push_str(&current_line);
                    ids_json.push_str(",\n");
                    current_line = format!("  {}", item);
                } else {
                    current_line.push_str(", ");
                    current_line.push_str(item);
                }
            } else {
                current_line.push_str(item);
            }
        }
        ids_json.push_str(&current_line);
        ids_json.push('\n');
    }
    if id_items.is_empty() {
        ids_json.push(']');
    } else {
        ids_json.push_str("  ]");
    }

    let lines_text = if only_ids {
        None
    } else {
        Some(text_lines.join("\n"))
    };

    Ok(FormattedLinesResult {
        lines_text,
        ids_json,
        actual_end_line,
        warning_msg,
    })
}

/// The ids an edit minted or touched, each with the line it is now, in the
/// shape `view --only-ids` already answers with. An id alone does not say
/// where its line went, so a caller wanting the line beside it had to read the
/// file again (card #5).
pub fn format_lines(ids: &[(String, usize)], wrap_trigger_length: usize) -> String {
    if ids.is_empty() {
        return "[]".to_string();
    }

    let mut result = String::new();
    result.push('[');
    result.push('\n');

    let mut current_line = "  ".to_string();
    for (i, (id, line)) in ids.iter().enumerate() {
        let item = format!(
            "[{},{}]",
            serde_json::to_string(id).unwrap_or_else(|_| format!("\"{}\"", id)),
            line
        );
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

pub fn format_definition_json(
    repository: &impl SessionRepository,
    session_id: &str,
    start_line: usize,
    end_line: usize,
) -> Result<String> {
    let lines = repository.fetch_lines_range(session_id, start_line, end_line)?;

    let mut items = Vec::new();
    for (current_idx, (seq_id, line_hash_opt, content)) in (start_line..).zip(lines) {
        let line_hash = line_hash_opt.unwrap_or_else(|| compute_line_hash(&content));
        let line_id = format!("{:x}#{}", seq_id, line_hash);
        let content_escaped = serde_json::to_string(&content)?;
        items.push(format!(
            "[\"{}\", {}, {}]",
            line_id, current_idx, content_escaped
        ));
    }

    let mut lines_json = String::new();
    lines_json.push('[');
    if !items.is_empty() {
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
        lines_json.push_str("  ]");
    } else {
        lines_json.push(']');
    }

    let output = format!(
        "{{\n  \"columns\": [\n    \"id\",\n    \"n\",\n    \"content\"\n  ],\n  \"lines\": {}\n}}",
        lines_json
    );
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::session_db::SqliteSessionRepository;
    use crate::tools::TEST_DB_LOCK as DB_LOCK;
    use std::fs;

    #[test]
    fn test_retrieve_and_format_lines_only_ids() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
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
        assert!(res.lines_text.is_none());

        let val: serde_json::Value = serde_json::from_str(&res.ids_json)?;
        let arr = val.as_array().unwrap();
        assert_eq!(arr.len(), 4);
        assert_eq!(arr[0][1].as_u64().unwrap(), 1);
        assert_eq!(arr[3][1].as_u64().unwrap(), 4);

        let lines: Vec<&str> = res.ids_json.lines().collect();
        assert!(
            lines.len() > 3,
            "Expected formatting to wrap into multiple lines: {}",
            res.ids_json
        );
        assert_eq!(lines[0], "[");
        assert!(lines[1].starts_with("  "));
        assert_eq!(*lines.last().unwrap(), "  ]");

        // Test with large wrap_trigger_length so everything is on one line
        let res_no_wrap = retrieve_and_format_lines(&repository, session_id, 1, 4, true, 1000)?;
        let lines_no_wrap: Vec<&str> = res_no_wrap.ids_json.lines().collect();
        assert_eq!(lines_no_wrap.len(), 3);
        assert_eq!(lines_no_wrap[0], "[");
        assert_eq!(lines_no_wrap[2], "  ]");

        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }

    #[test]
    fn test_retrieve_and_format_lines_full() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
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

        let rows: Vec<&str> = res.lines_text.as_ref().unwrap().lines().collect();
        assert!(rows[0].starts_with("1#"), "{}", rows[0]);
        assert!(rows[0].ends_with("|1: line 1"), "{}", rows[0]);
        assert!(rows[1].ends_with("|2: line 2"), "{}", rows[1]);

        let val: serde_json::Value = serde_json::from_str(&res.ids_json)?;
        let arr = val.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0][1].as_u64().unwrap(), 1);

        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }

    #[test]
    fn test_retrieve_and_format_lines_truncation() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
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
        let text = res.lines_text.as_ref().unwrap();
        assert!(text.contains("|1: "));
        assert!(text.contains("│: "));
        assert!(text.contains("└: "));

        // The rows of one line join back to exactly the line, because the
        // break is at a fixed count of characters and adds nothing.
        let rejoined: String = text
            .lines()
            .map(|row| row.split_once(": ").unwrap().1)
            .collect();
        assert_eq!(rejoined, long_line);

        let val: serde_json::Value = serde_json::from_str(&res.ids_json)?;
        let arr = val.as_array().unwrap();
        let line_id = arr[0][0].as_str().unwrap();

        assert!(!line_id.ends_with("#TRUNC"));

        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }

    #[test]
    fn test_retrieve_and_format_lines_capacity() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
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

        // 44, not 45: the id each line carries is part of the response, so it
        // is counted against the 45,000 bytes.
        let res = retrieve_and_format_lines(&repository, session_id, 1, 50, false, 1000)?;
        assert_eq!(res.actual_end_line, 44);
        assert_eq!(
            res.warning_msg.as_deref(),
            Some("Response truncated: cumulative response size limit (45,000 bytes) was reached.")
        );

        let val: serde_json::Value = serde_json::from_str(&res.ids_json)?;
        assert_eq!(val.as_array().unwrap().len(), 44);

        fs::remove_dir_all(&temp_dir)?;
        Ok(())
    }

    #[test]
    fn test_format_lines_empty() {
        let ids: Vec<(String, usize)> = vec![];
        assert_eq!(format_lines(&ids, 80), "[]");
    }

    #[test]
    fn test_format_lines_wrap() {
        let ids = vec![
            ("1#77cf".to_string(), 1),
            ("2#bcb4".to_string(), 2),
            ("3#c2b7".to_string(), 3),
        ];
        // small trigger -> wraps
        let res_wrap = format_lines(&ids, 15);
        let expected_wrap = "[\n  [\"1#77cf\",1],\n  [\"2#bcb4\",2],\n  [\"3#c2b7\",3]\n]";
        assert_eq!(res_wrap, expected_wrap);

        // large trigger -> no wrap
        let res_no_wrap = format_lines(&ids, 100);
        let expected_no_wrap = "[\n  [\"1#77cf\",1], [\"2#bcb4\",2], [\"3#c2b7\",3]\n]";
        assert_eq!(res_no_wrap, expected_no_wrap);
    }
}
