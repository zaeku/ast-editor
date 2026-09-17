use crate::tools::line_id::compute_line_hash;
use crate::tools::repository::FileStore;
use anyhow::Result;

/// Where a line is broken for display. The break is at a fixed count of
/// characters, never at a space, so the rows of one line concatenate back to
/// exactly what the file holds. Wide enough that ordinary code is one row and
/// a search across the output is not split by it.
const WRAP_WIDTH: usize = 300;

/// What the caps count, which is not what a row is: lowering the display width
/// must not make a response run out of budget ten times sooner.
pub(crate) const SEGMENT_LENGTH: usize = 2048;

/// How much one call will answer with. The documents render both figures
/// rather than repeating them.
pub(crate) const LINE_CAP: usize = 800;
pub(crate) const RESPONSE_BYTE_CAP: usize = 45_000;

/// For a caller that stated how much it wanted. The cap is for a read given no
/// bounds; a call answering for lines it just wrote has nothing to guard
/// against (`D-01M2ATRFAMMMXD`).
pub(crate) const NO_LINE_CAP: usize = usize::MAX;

pub(crate) struct FormattedLinesResult {
    pub lines_text: Option<String>,
    pub ids_json: String,
    pub actual_end_line: usize,
    pub warning_msg: Option<String>,
}

pub(crate) fn retrieve_and_format_lines(
    repository: &impl FileStore,
    file_key: &str,
    start_line: usize,
    end_line: usize,
    only_ids: bool,
    wrap_trigger_length: usize,
    line_cap: usize,
) -> Result<FormattedLinesResult> {
    let lines = repository.fetch_lines_range(file_key, start_line, end_line)?;
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
        if segment_count + segments_to_add > line_cap {
            segments_to_add = line_cap - segment_count;
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

    let ids_json = if id_items.is_empty() {
        "[]".to_string()
    } else {
        format!(
            "[\n{}\n  ]",
            wrap_items(&id_items, wrap_trigger_length).join(",\n")
        )
    };

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

/// The rows of a json array, wrapped so that a row stops before it would run
/// past `wrap_trigger_length` — unless one item alone does, which is a row of
/// its own either way. Each row carries its own two-space indent and no comma:
/// the caller joins them, because what closes the array differs with where the
/// array is going.
fn wrap_items(items: &[String], wrap_trigger_length: usize) -> Vec<String> {
    let mut rows = Vec::new();
    let mut current = "  ".to_string();
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            if current.len() + 2 + item.len() > wrap_trigger_length {
                rows.push(std::mem::replace(&mut current, format!("  {item}")));
                continue;
            }
            current.push_str(", ");
        }
        current.push_str(item);
    }
    rows.push(current);
    rows
}

/// The ids an edit minted or touched, each with the line it is now, in the
/// shape `view --only-ids` already answers with. An id alone does not say
/// where its line went, so a caller wanting the line beside it had to read the
/// file again (card #5).
pub(crate) fn format_lines(ids: &[(String, usize)], wrap_trigger_length: usize) -> String {
    if ids.is_empty() {
        return "[]".to_string();
    }

    let items: Vec<String> = ids
        .iter()
        .map(|(id, line)| {
            format!(
                "[{},{}]",
                serde_json::to_string(id).unwrap_or_else(|_| format!("\"{}\"", id)),
                line
            )
        })
        .collect();
    format!(
        "[\n{}\n]",
        wrap_items(&items, wrap_trigger_length).join(",\n")
    )
}

/// The id `edit` would take for a line number, if the file has an entry.
pub(crate) fn line_id_at(
    repository: &impl FileStore,
    file_key: &Option<String>,
    line: usize,
) -> Option<String> {
    let file_key = file_key.as_ref()?;
    let row = repository
        .fetch_lines_range(file_key, line, line)
        .ok()?
        .into_iter()
        .next()?;
    let (seq, hash, _) = row;
    Some(format!("{:x}#{}", seq, hash?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::repository::SqliteFileStore;
    use crate::tools::TEST_DB_LOCK as DB_LOCK;
    use std::fs;

    /// A row is the two-space indent, the items on it, and the `", "` between
    /// each pair, and it breaks when the next item would take it past the
    /// trigger. The trigger is the width a row may reach, so a row that lands
    /// exactly on it has not gone past it.
    #[test]
    fn a_row_of_ids_breaks_only_when_the_next_one_would_not_fit() {
        let items: Vec<String> = ["aaaa", "bbbb", "cccc"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        // "  aaaa, bbbb" is twelve characters, so twelve is wide enough for it
        // and eleven is not.
        assert_eq!(
            wrap_items(&items, 12),
            vec!["  aaaa, bbbb".to_string(), "  cccc".to_string()]
        );
        assert_eq!(
            wrap_items(&items, 11),
            vec![
                "  aaaa".to_string(),
                "  bbbb".to_string(),
                "  cccc".to_string()
            ]
        );

        // Ten is narrower than two items and wider than one, which is what
        // separates counting the separator from counting it the other way.
        assert_eq!(
            wrap_items(&items, 10),
            vec![
                "  aaaa".to_string(),
                "  bbbb".to_string(),
                "  cccc".to_string()
            ]
        );

        // An item wider than the trigger is a row by itself rather than an
        // empty row followed by it.
        assert_eq!(
            wrap_items(&items, 1),
            vec![
                "  aaaa".to_string(),
                "  bbbb".to_string(),
                "  cccc".to_string()
            ]
        );
        assert_eq!(wrap_items(&[], 12), vec!["  ".to_string()]);
    }

    #[test]
    fn test_retrieve_and_format_lines_only_ids() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let temp_dir = crate::tools::test_temp_dir("line-editor-test-formatter-ids");
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir)?;
        }
        fs::create_dir_all(&temp_dir)?;
        let file_path = temp_dir.join("test.txt");
        let filepath_str = file_path.to_str().unwrap();

        let content = "line 1\nline 2\nline 3\nline 4";
        fs::write(filepath_str, content)?;

        let repository = SqliteFileStore;
        let meta = repository.init_session(filepath_str, false)?;
        let file_key = &meta.file_key;

        // Test with a small wrap_trigger_length to force wrapping
        let res = retrieve_and_format_lines(&repository, file_key, 1, 4, true, 30, LINE_CAP)?;
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
        let res_no_wrap =
            retrieve_and_format_lines(&repository, file_key, 1, 4, true, 1000, LINE_CAP)?;
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
        let temp_dir = crate::tools::test_temp_dir("line-editor-test-formatter-full");
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir)?;
        }
        fs::create_dir_all(&temp_dir)?;
        let file_path = temp_dir.join("test.txt");
        let filepath_str = file_path.to_str().unwrap();

        let content = "line 1\nline 2";
        fs::write(filepath_str, content)?;

        let repository = SqliteFileStore;
        let meta = repository.init_session(filepath_str, false)?;
        let file_key = &meta.file_key;

        let res = retrieve_and_format_lines(&repository, file_key, 1, 2, false, 1000, LINE_CAP)?;
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
        let temp_dir = crate::tools::test_temp_dir("line-editor-test-formatter-trunc");
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir)?;
        }
        fs::create_dir_all(&temp_dir)?;
        let file_path = temp_dir.join("test.txt");
        let filepath_str = file_path.to_str().unwrap();

        let long_line = "A".repeat(2500);
        fs::write(filepath_str, &long_line)?;

        let repository = SqliteFileStore;
        let meta = repository.init_session(filepath_str, false)?;
        let file_key = &meta.file_key;

        let res = retrieve_and_format_lines(&repository, file_key, 1, 1, false, 1000, LINE_CAP)?;
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
        let temp_dir = crate::tools::test_temp_dir("line-editor-test-formatter-cap");
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

        let repository = SqliteFileStore;
        let meta = repository.init_session(filepath_str, false)?;
        let file_key = &meta.file_key;

        // 44, not 45: the id each line carries is part of the response, so it
        // is counted against the 45,000 bytes.
        let res = retrieve_and_format_lines(&repository, file_key, 1, 50, false, 1000, LINE_CAP)?;
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
