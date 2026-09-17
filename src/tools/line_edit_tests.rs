#![allow(clippy::await_holding_lock)]
use crate::parser::ParserManager;
use crate::tools::create;
use crate::tools::edit;
use crate::tools::edit::edit_lines;
use crate::tools::file_entry;
use crate::tools::file_entry::init_file_entry;
use crate::tools::line_id::{parse_line_id, EditOp, LineEdit, MovePosition};
use crate::tools::repository;
use crate::tools::repository::{FileStore, SqliteFileStore};
use crate::tools::view;
use crate::tools::TEST_DB_LOCK as DB_LOCK;
use anyhow::Result;
use std::fs;
use std::path::PathBuf;

use std::sync::atomic::{AtomicUsize, Ordering};
static TEST_FILE_COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TestFile {
    path: PathBuf,
}

impl TestFile {
    fn new(name: &str, content: &str) -> Self {
        let pid = std::process::id();
        let counter = TEST_FILE_COUNTER.fetch_add(1, Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("ts_inspect_int_{}_{}_{}", pid, counter, name));
        if path.exists() {
            let _ = fs::remove_file(&path);
        }
        fs::write(&path, content).unwrap();
        Self { path }
    }

    fn path_str(&self) -> &str {
        self.path.to_str().unwrap()
    }
}

impl Drop for TestFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn view_range(
    repository: &impl repository::FileStore,
    filepath: &str,
    start_line: usize,
    end_line: usize,
    only_ids: Option<bool>,
) -> std::result::Result<String, anyhow::Error> {
    let res = view::view_lines(
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
            let (head, content) = row.split_once(": ").unwrap();
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

fn create_test_parser_manager() -> ParserManager {
    // Every grammar is compiled in (D-01M28RAGW19ZZC), so there is no
    // directory to arrange.
    ParserManager::new().unwrap()
}

/// The store these tests reach is the one `get_db_path` picks under
/// `cfg!(test)`, which is a directory of this build's own. Pointing an
/// environment variable at another one is what an integration test has to do,
/// and doing it here would take the store out from under the unit tests that
/// share this binary.
fn acquire_db_lock() -> std::sync::MutexGuard<'static, ()> {
    match crate::tools::TEST_DB_LOCK.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[tokio::test]
async fn test_sqlite_file_entry_lifecycle() {
    let _lock = acquire_db_lock();
    let file = TestFile::new("lifecycle.py", "def foo():\n    print('bar')\n");

    let meta = file_entry::init_file_entry(file.path_str(), false).unwrap();
    assert_eq!(meta.total_lines, 2);
    assert!(meta.is_supported);

    // Test entry reuse
    let meta_reused = file_entry::init_file_entry(file.path_str(), false).unwrap();
    assert_eq!(meta.file_key, meta_reused.file_key);
}

#[tokio::test]
async fn test_view_lines_lazy_hashing() {
    let _lock = acquire_db_lock();
    let file = TestFile::new("lazy.rs", "fn main() {\n    println!(\"hello\");\n}\n");
    let repository = SqliteFileStore;

    // Retrieve lines (this triggers lazy hashing for range)
    let view_res = view_range(&repository, file.path_str(), 1, 3, None).unwrap();
    let val: serde_json::Value = serde_json::from_str(&view_res).unwrap();
    let lines = val["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 3);
    let line0 = lines[0].as_array().unwrap();
    assert!(line0[0].as_str().unwrap().starts_with("1#"));
    assert_eq!(line0[2].as_str().unwrap(), "fn main() {");
    assert!(
        val["tip"].is_null(),
        "a response does not re-teach the tool"
    );
    assert_eq!(val["total_lines"].as_u64().unwrap(), 3);
    assert_eq!(val["showing_start"].as_u64().unwrap(), 1);
    assert_eq!(val["showing_end"].as_u64().unwrap(), 3);
    assert!(val["total_bytes"].as_u64().is_some());
}

#[tokio::test]
async fn test_edit_operations_and_ast_validation() {
    let _lock = acquire_db_lock();
    let file = TestFile::new("edit.rs", "fn main() {\n    let a = 1;\n}\n");
    let repository = SqliteFileStore;

    let pm = create_test_parser_manager();

    // Fetch the correct target ID for line 2
    let view_res = view_range(&repository, file.path_str(), 2, 2, None).unwrap();
    let val: serde_json::Value = serde_json::from_str(&view_res).unwrap();
    let start_id = val["lines"][0].as_array().unwrap()[0]
        .as_str()
        .unwrap()
        .to_string();

    // Perform invalid edit (Syntax error)
    let invalid_edits = vec![LineEdit {
        op: EditOp::Replace,
        start_id: Some(start_id.clone()),
        content: Some("let a = ;".to_string()), // missing value
        ..Default::default()
    }];
    // The batch is refused and the file is left alone.
    let edit_res = edit::edit_lines(&repository, file.path_str(), invalid_edits, &pm).await;

    if let Err(ref e) = edit_res {
        println!("DEBUG: invalid edit error = {:?}", e);
    }

    assert!(edit_res.is_err());
    assert!(edit_res
        .unwrap_err()
        .to_string()
        .contains("\"syntax_valid\": false"));

    // Verify file content didn't change (rolled back)
    let content = fs::read_to_string(file.path_str()).unwrap();
    assert!(content.contains("let a = 1;"));

    // The file was rewritten from outside between these edits, so the line was
    // changed and changed back without the tool seeing it. Its id is not the
    // one captured at the top; re-read it.
    let view_res = view_range(&repository, file.path_str(), 2, 2, None).unwrap();
    let val: serde_json::Value = serde_json::from_str(&view_res).unwrap();
    let start_id = val["lines"][0].as_array().unwrap()[0]
        .as_str()
        .unwrap()
        .to_string();

    let valid_edits = vec![LineEdit {
        op: EditOp::Replace,
        start_id: Some(start_id),
        content: Some("    let a = 2;".to_string()),
        ..Default::default()
    }];
    let edit_res = edit::edit_lines(&repository, file.path_str(), valid_edits, &pm)
        .await
        .unwrap();
    let res: serde_json::Value = serde_json::from_str(&edit_res).unwrap();
    assert!(res["status"].is_null(), "success is the exit code: {}", res);
    assert!(!res["modified_lines"].as_array().unwrap().is_empty());

    let content = fs::read_to_string(file.path_str()).unwrap();
    assert!(content.contains("let a = 2;"));
}

#[tokio::test]
async fn test_transactional_deletes_and_inserts() {
    let _lock = acquire_db_lock();
    let file = TestFile::new(
        "trans_ops.rs",
        "fn main() {\n    let a = 1;\n    let b = 2;\n}\n",
    );
    let repository = SqliteFileStore;

    let pm = create_test_parser_manager();

    // Get Line IDs
    let view_res = view_range(&repository, file.path_str(), 1, 4, None).unwrap();
    let val: serde_json::Value = serde_json::from_str(&view_res).unwrap();
    let lines_arr = val["lines"].as_array().unwrap();

    // Find line IDs for let a = 1 and let b = 2
    let mut id_a = String::new();
    let mut id_b = String::new();
    for line in lines_arr {
        let line_arr = line.as_array().unwrap();
        let code = line_arr[2].as_str().unwrap();
        if code.contains("let a = 1;") {
            id_a = line_arr[0].as_str().unwrap().to_string();
        } else if code.contains("let b = 2;") {
            id_b = line_arr[0].as_str().unwrap().to_string();
        }
    }
    assert!(!id_a.is_empty());
    assert!(!id_b.is_empty());

    // Transactional delete and insert_after
    let edits = vec![
        LineEdit {
            op: EditOp::Delete,
            start_id: Some(id_b),
            content: None,
            ..Default::default()
        },
        LineEdit {
            op: EditOp::InsertAfter,
            start_id: Some(id_a),
            content: Some("    let c = 3;".to_string()),
            ..Default::default()
        },
    ];

    let edit_preview = edit::edit_lines(&repository, file.path_str(), edits, &pm)
        .await
        .unwrap();
    let res: serde_json::Value = serde_json::from_str(&edit_preview).unwrap();
    assert!(res["status"].is_null(), "success is the exit code: {}", res);
    assert!(!res["modified_lines"].as_array().unwrap().is_empty());

    // The final file should be:
    // fn main() {
    //     let a = 1;
    //     let c = 3;
    // }
    let content = fs::read_to_string(file.path_str()).unwrap();
    assert!(content.contains("let a = 1;"));
    assert!(content.contains("let c = 3;"));
    assert!(!content.contains("let b = 2;"));
}

#[tokio::test]
async fn test_concurrency_error_out_of_sync_mtime() {
    let _lock = acquire_db_lock();
    let file = TestFile::new("concurrency.rs", "fn main() {\n    let a = 1;\n}\n");
    let repository = SqliteFileStore;

    let pm = create_test_parser_manager();

    // Get line ID
    let view_res = view_range(&repository, file.path_str(), 2, 2, None).unwrap();
    let val: serde_json::Value = serde_json::from_str(&view_res).unwrap();
    let start_id = val["lines"][0].as_array().unwrap()[0]
        .as_str()
        .unwrap()
        .to_string();

    // Modify the file externally on disk, changing its mtime
    tokio::time::sleep(tokio::time::Duration::from_millis(1100)).await;
    fs::write(
        file.path_str(),
        "fn main() {\n    let a = 1;\n}\n// external change\n",
    )
    .unwrap();

    // Try applying line edits, should succeed due to Smart Resync
    let edits = vec![LineEdit {
        op: EditOp::Replace,
        start_id: Some(start_id),
        content: Some("    let a = 3;".to_string()),
        ..Default::default()
    }];

    let edit_res = edit::edit_lines(&repository, file.path_str(), edits, &pm)
        .await
        .unwrap();
    assert!(edit_res.contains("modified_lines"), "{}", edit_res);

    // Verify disk content includes both the external change and our update
    let content = fs::read_to_string(file.path_str()).unwrap();
    assert!(content.contains("let a = 3;"));
    assert!(content.contains("external change"));
}

#[tokio::test]
async fn test_integration_append_operation() {
    let _lock = acquire_db_lock();
    let file = TestFile::new("append_integration.rs", "fn main() {\n    let a = 1;\n}\n");
    let repository = SqliteFileStore;

    let pm = create_test_parser_manager();

    // Perform append
    let edits = vec![LineEdit {
        op: EditOp::Append,
        start_id: None,
        content: Some("fn additional() {\n}".to_string()),
        ..Default::default()
    }];

    let preview = edit::edit_lines(&repository, file.path_str(), edits, &pm)
        .await
        .unwrap();
    let res: serde_json::Value = serde_json::from_str(&preview).unwrap();
    assert!(res["status"].is_null(), "success is the exit code: {}", res);
    assert!(!res["modified_lines"].as_array().unwrap().is_empty());

    let content = fs::read_to_string(file.path_str()).unwrap();
    assert!(content.contains("fn main() {\n    let a = 1;\n}\nfn additional() {\n}\n"));
}

#[tokio::test]
async fn test_integration_advanced_operations() {
    let _lock = acquire_db_lock();
    let file = TestFile::new(
        "advanced_int.rs",
        "fn main() {\n    let a = 1;\n    let b = 2;\n}\n",
    );
    let repository = SqliteFileStore;

    let pm = create_test_parser_manager();

    // 1. Get IDs for lines
    let view_res = view_range(&repository, file.path_str(), 1, 4, None).unwrap();
    let val: serde_json::Value = serde_json::from_str(&view_res).unwrap();
    let lines_arr = val["lines"].as_array().unwrap();
    let _id_main = lines_arr[0].as_array().unwrap()[0]
        .as_str()
        .unwrap()
        .to_string();
    let id_a = lines_arr[1].as_array().unwrap()[0]
        .as_str()
        .unwrap()
        .to_string();
    let id_b = lines_arr[2].as_array().unwrap()[0]
        .as_str()
        .unwrap()
        .to_string();

    // 2. Perform replace_range replacing let a = 1 and let b = 2 with let val = 100
    let edits = vec![LineEdit {
        op: EditOp::Replace,
        start_id: Some(id_a.clone()),
        end_id: Some(id_b.clone()),
        content: Some("    let val = 100;".to_string()),
        ..Default::default()
    }];

    let preview = edit::edit_lines(&repository, file.path_str(), edits, &pm)
        .await
        .unwrap();
    let res: serde_json::Value = serde_json::from_str(&preview).unwrap();
    assert!(res["status"].is_null(), "success is the exit code: {}", res);
    assert!(!res["modified_lines"].as_array().unwrap().is_empty());

    let content = fs::read_to_string(file.path_str()).unwrap();
    assert_eq!(content, "fn main() {\n    let val = 100;\n}\n");

    // Get new IDs
    let view_res = view_range(&repository, file.path_str(), 1, 3, None).unwrap();
    let val: serde_json::Value = serde_json::from_str(&view_res).unwrap();
    let lines_arr = val["lines"].as_array().unwrap();
    let id_main_new = lines_arr[0].as_array().unwrap()[0]
        .as_str()
        .unwrap()
        .to_string();
    let id_val_new = lines_arr[1].as_array().unwrap()[0]
        .as_str()
        .unwrap()
        .to_string();

    // 3. Move let val = 100; before fn main() {
    let edits_move = vec![LineEdit {
        op: EditOp::Move,
        start_id: Some(id_val_new),
        dest_id: Some(id_main_new),
        move_position: Some(MovePosition::Before),
        ..Default::default()
    }];

    let preview_move = edit::edit_lines(&repository, file.path_str(), edits_move, &pm)
        .await
        .unwrap();
    let res: serde_json::Value = serde_json::from_str(&preview_move).unwrap();
    assert!(res["status"].is_null(), "success is the exit code: {}", res);
    assert!(!res["modified_lines"].as_array().unwrap().is_empty());

    let content_move = fs::read_to_string(file.path_str()).unwrap();
    assert_eq!(content_move, "    let val = 100;\nfn main() {\n}\n");
}

#[tokio::test]
async fn test_integration_insert_without_start_id() {
    let _lock = acquire_db_lock();
    let file = TestFile::new(
        "insert_jit_no_target.rs",
        "fn main() {\n    let a = 1;\n}\n",
    );
    let repository = SqliteFileStore;

    let pm = create_test_parser_manager();

    // 1. Perform insert_before with start_id = None (should prepend to the beginning of the file)
    let edits_before = vec![LineEdit {
        op: EditOp::InsertBefore,
        start_id: None,
        content: Some("// Prepend header".to_string()),
        ..Default::default()
    }];

    let preview1 = edit::edit_lines(&repository, file.path_str(), edits_before, &pm)
        .await
        .unwrap();
    let res: serde_json::Value = serde_json::from_str(&preview1).unwrap();
    assert!(res["status"].is_null(), "success is the exit code: {}", res);
    assert!(!res["modified_lines"].as_array().unwrap().is_empty());

    let content1 = fs::read_to_string(file.path_str()).unwrap();
    assert!(
        content1.starts_with("// Prepend header\nfn main() {"),
        "content1 was: {:?}",
        content1
    );

    // 2. Perform insert_after with start_id = Some("") (should append to the end of the file)
    let edits_after = vec![LineEdit {
        op: EditOp::InsertAfter,
        start_id: Some("".to_string()),
        content: Some("// Append footer".to_string()),
        ..Default::default()
    }];

    let preview2 = edit::edit_lines(&repository, file.path_str(), edits_after, &pm)
        .await
        .unwrap();
    let res: serde_json::Value = serde_json::from_str(&preview2).unwrap();
    assert!(res["status"].is_null(), "success is the exit code: {}", res);
    assert!(!res["modified_lines"].as_array().unwrap().is_empty());

    let content2 = fs::read_to_string(file.path_str()).unwrap();
    assert!(
        content2.ends_with("// Append footer\n"),
        "content2 was: {:?}",
        content2
    );
}

#[tokio::test]
async fn test_integration_create_lines_flow() {
    let _lock = acquire_db_lock();
    let pm = create_test_parser_manager();
    let repository = SqliteFileStore;

    let pid = std::process::id();
    let counter = TEST_FILE_COUNTER.fetch_add(1, Ordering::SeqCst);
    let temp_file_path =
        std::env::temp_dir().join(format!("ts_inspect_int_{}_{}_create_flow.rs", pid, counter));
    if temp_file_path.exists() {
        let _ = fs::remove_file(&temp_file_path);
    }
    let filepath_str = temp_file_path.to_str().unwrap().to_string();

    // 1. Create the new file via calling `create` tool logic
    let initial_content = "fn main() {\n    let x = 42;\n}\n";
    let create_res =
        create::create_lines(&repository, &filepath_str, initial_content, Some(true)).unwrap();
    let val: serde_json::Value = serde_json::from_str(&create_res).unwrap();

    assert!(val["status"].is_null(), "success is the exit code: {}", val);
    assert_eq!(val["total_lines"].as_u64().unwrap(), 3);
    assert!(val["total_bytes"].as_u64().is_some());
    let ids = val["lines"].as_array().unwrap();
    assert_eq!(ids.len(), 3);

    // 2. Assert that the returned line IDs are correct
    // Each entry is [id, line] (card #5).
    let id0 = ids[0][0].as_str().unwrap();
    let id1 = ids[1][0].as_str().unwrap();
    let id2 = ids[2][0].as_str().unwrap();

    assert!(id0.contains('#'));
    assert!(id1.contains('#'));
    assert!(id2.contains('#'));

    // 3. Try calling `create` on the same file path again and verify it returns a `FILE_ALREADY_EXISTS` error
    let dup_res = create::create_lines(&repository, &filepath_str, "different content", None);
    assert!(dup_res.is_err());
    let dup_err = dup_res.unwrap_err().to_string();
    assert!(dup_err.contains("FILE_ALREADY_EXISTS"));

    // 4. Modify the newly created file using `edit` and verify the modified contents on disk
    let edits = vec![LineEdit {
        op: EditOp::Replace,
        start_id: Some(id1.to_string()),
        content: Some("    let x = 100;".to_string()),
        ..Default::default()
    }];

    let edit_res = edit::edit_lines(&repository, &filepath_str, edits, &pm)
        .await
        .unwrap();
    let res: serde_json::Value = serde_json::from_str(&edit_res).unwrap();
    assert!(res["status"].is_null(), "success is the exit code: {}", res);
    assert!(!res["modified_lines"].as_array().unwrap().is_empty());

    // Verify modified contents on disk
    let disk_content = fs::read_to_string(&filepath_str).unwrap();
    assert!(disk_content.contains("let x = 100;"));
    assert!(!disk_content.contains("let x = 42;"));

    // Clean up
    let _ = fs::remove_file(&temp_file_path);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_integration_view_lines_truncation_and_protection() {
    let _lock = acquire_db_lock();
    let pm = create_test_parser_manager();
    let repository = SqliteFileStore;

    let long_line = "a".repeat(2500);
    let content = format!("fn first() {{\n{}\n}}\n", long_line);
    let file = TestFile::new("truncation_protection.rs", &content);

    let view_res = view_range(&repository, file.path_str(), 1, 3, None).unwrap();
    let val: serde_json::Value = serde_json::from_str(&view_res).unwrap();

    assert_eq!(val["total_lines"].as_u64().unwrap(), 3);
    let expected_bytes = std::fs::metadata(file.path_str()).unwrap().len();
    assert_eq!(val["total_bytes"].as_u64().unwrap(), expected_bytes);

    let lines = val["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 3);
    let line1_arr = lines[1].as_array().unwrap();
    let line1_id = line1_arr[0].as_str().unwrap();
    let line1_content = line1_arr[2].as_str().unwrap();

    // Verify it accumulated correctly
    assert_eq!(line1_content, &long_line);
    assert!(!line1_id.contains("#TRUNC"));

    // Verify replace_substring works on this long line
    let edits = vec![LineEdit {
        op: EditOp::ReplaceSubstring,
        start_id: Some(line1_id.to_string()),
        pattern: Some("aaaaa".to_string()),
        replacement: Some("bbbbb".to_string()),
        occurrence: Some(1),
        ..Default::default()
    }];
    let edit_res = edit::edit_lines(&repository, file.path_str(), edits, &pm)
        .await
        .unwrap();
    assert!(edit_res.contains("modified_lines"), "{}", edit_res);

    let disk_content = std::fs::read_to_string(file.path_str()).unwrap();
    assert!(disk_content.contains("bbbbbaaaaa"));
}

#[tokio::test]
async fn test_integration_view_lines_capacity_cap() {
    let _lock = acquire_db_lock();
    let repository = SqliteFileStore;

    let line_content = "x".repeat(1000);
    let mut lines = Vec::new();
    for _ in 0..50 {
        lines.push(line_content.clone());
    }
    let content = lines.join("\n");
    let file = TestFile::new("capacity_cap.txt", &content);

    let view_res = view_range(&repository, file.path_str(), 1, 50, None).unwrap();
    let val: serde_json::Value = serde_json::from_str(&view_res).unwrap();

    // 44 of the 1000-character lines, not 45: the id each line carries is
    // part of the response and counts against the same budget.
    let returned_lines = val["lines"].as_array().unwrap();
    assert_eq!(returned_lines.len(), 44);
    assert_eq!(val["showing_end"].as_u64().unwrap(), 44);

    let warning_msg = val["message"].as_str().unwrap();
    assert!(warning_msg.contains("cumulative response size limit"));
}

#[tokio::test]
async fn test_integration_create_lines_return_ids_false() {
    let _lock = acquire_db_lock();
    let repository = SqliteFileStore;

    let pid = std::process::id();
    let counter = TEST_FILE_COUNTER.fetch_add(1, Ordering::SeqCst);
    let temp_file_path = std::env::temp_dir().join(format!(
        "ts_inspect_int_{}_{}_create_flow_ids_false.rs",
        pid, counter
    ));
    if temp_file_path.exists() {
        let _ = fs::remove_file(&temp_file_path);
    }
    let filepath_str = temp_file_path.to_str().unwrap().to_string();

    let initial_content = "fn main() {\n    let x = 42;\n}\n";
    let create_res =
        create::create_lines(&repository, &filepath_str, initial_content, Some(false)).unwrap();
    let val: serde_json::Value = serde_json::from_str(&create_res).unwrap();

    assert!(val["status"].is_null(), "success is the exit code: {}", val);
    assert_eq!(val["total_lines"].as_u64().unwrap(), 3);
    assert!(val["total_bytes"].as_u64().is_some());
    assert!(val["lines"].is_null());

    let _ = fs::remove_file(&temp_file_path);
}

#[tokio::test]
async fn test_integration_view_lines_only_ids() {
    let _lock = acquire_db_lock();
    let repository = SqliteFileStore;
    let file = TestFile::new("only_ids.rs", "fn main() {\n    let a = 1;\n}\n");

    let view_res = view_range(&repository, file.path_str(), 1, 3, Some(true)).unwrap();
    let val: serde_json::Value = serde_json::from_str(&view_res).unwrap();

    assert_eq!(val["columns"], serde_json::json!(["id", "n"]));
    let lines = val["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 3);
    for line in lines {
        let line_arr = line.as_array().unwrap();
        assert_eq!(line_arr.len(), 2); // [id, n]
    }
}

#[tokio::test]
async fn test_dry_run_preview_leaves_everything_untouched() {
    let _lock = acquire_db_lock();
    let original = "fn main() {\n    let a = 1;\n}\n";
    let file = TestFile::new("dry_run.rs", original);
    let repository = SqliteFileStore;
    let pm = create_test_parser_manager();

    let before_view = view_range(&repository, file.path_str(), 1, 3, None).unwrap();
    let val: serde_json::Value = serde_json::from_str(&before_view).unwrap();
    let start_id = val["lines"][1].as_array().unwrap()[0]
        .as_str()
        .unwrap()
        .to_string();
    let before_mtime = fs::metadata(file.path_str()).unwrap().modified().unwrap();

    let valid_edits = vec![LineEdit {
        op: EditOp::Replace,
        start_id: Some(start_id.clone()),
        content: Some("    let a = 2;".to_string()),
        ..Default::default()
    }];

    // 1. A valid batch previews the diff it would produce, and mints no IDs.
    let res = edit::edit_lines_dry_run(&repository, file.path_str(), valid_edits.clone(), &pm)
        .await
        .unwrap();
    let preview: serde_json::Value = serde_json::from_str(&res.report).unwrap();
    assert_eq!(preview["syntax_valid"], true);
    assert!(preview["modified_lines"].is_null());
    let diff = &res.diff;
    assert!(
        diff.contains("-    let a = 1;"),
        "diff missing removal: {}",
        diff
    );
    assert!(
        diff.contains("+    let a = 2;"),
        "diff missing addition: {}",
        diff
    );

    // 3. Nothing on disk or in the entry changed.
    assert_eq!(fs::read_to_string(file.path_str()).unwrap(), original);
    assert_eq!(
        fs::metadata(file.path_str()).unwrap().modified().unwrap(),
        before_mtime
    );
    assert_eq!(
        view_range(&repository, file.path_str(), 1, 3, None).unwrap(),
        before_view
    );

    // 2. A batch that breaks syntax reports the failure, still without writing.
    let broken_edits = vec![LineEdit {
        op: EditOp::Replace,
        start_id: Some(start_id),
        content: Some("    let a = ;".to_string()),
        ..Default::default()
    }];
    let res = edit::edit_lines_dry_run(&repository, file.path_str(), broken_edits, &pm)
        .await
        .unwrap();
    let preview: serde_json::Value = serde_json::from_str(&res.report).unwrap();
    assert_eq!(preview["syntax_valid"], false);
    assert!(!preview["diagnostics"].as_array().unwrap().is_empty());
    assert_eq!(fs::read_to_string(file.path_str()).unwrap(), original);
    assert_eq!(
        view_range(&repository, file.path_str(), 1, 3, None).unwrap(),
        before_view
    );

    // 4. Applying the previewed batch produces what the preview showed.
    let res = edit::edit_lines(&repository, file.path_str(), valid_edits, &pm)
        .await
        .unwrap();
    let applied: serde_json::Value = serde_json::from_str(&res).unwrap();
    assert!(
        applied["status"].is_null(),
        "success is the exit code: {}",
        applied
    );
    assert_eq!(
        fs::read_to_string(file.path_str()).unwrap(),
        "fn main() {\n    let a = 2;\n}\n"
    );
}

#[tokio::test]
async fn test_dry_run_preview_reports_markdown_warnings_as_valid() {
    let _lock = acquire_db_lock();
    let file = TestFile::new("dry_run.md", "# Title\n\nbody\n");
    let repository = SqliteFileStore;
    let pm = create_test_parser_manager();

    let view = view_range(&repository, file.path_str(), 3, 3, None).unwrap();
    let val: serde_json::Value = serde_json::from_str(&view).unwrap();
    let start_id = val["lines"][0].as_array().unwrap()[0]
        .as_str()
        .unwrap()
        .to_string();

    let edits = vec![LineEdit {
        op: EditOp::Replace,
        start_id: Some(start_id),
        content: Some("### Skipped a level".to_string()),
        ..Default::default()
    }];

    let res = edit::edit_lines_dry_run(&repository, file.path_str(), edits, &pm)
        .await
        .unwrap();
    let preview: serde_json::Value = serde_json::from_str(&res.report).unwrap();
    assert_eq!(preview["syntax_valid"], true);
    assert!(!preview["warnings"].as_array().unwrap().is_empty());
    assert_eq!(
        fs::read_to_string(file.path_str()).unwrap(),
        "# Title\n\nbody\n"
    );
}

#[tokio::test]
async fn test_view_lines_without_a_range_does_not_overflow() {
    let _lock = acquire_db_lock();
    let big = (1..=900)
        .map(|n| format!("line {}", n))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let small = TestFile::new("norange_small.rs", "fn main() {}\n");
    let large = TestFile::new("norange_large.txt", &big);
    let repository = SqliteFileStore;

    let res = view::view_lines(
        &repository,
        small.path_str(),
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    let meta: serde_json::Value = serde_json::from_str(&res.metadata_json).unwrap();
    assert_eq!(meta["total_lines"], 1);
    assert!(
        meta["message"].is_null(),
        "short file reported as truncated: {}",
        meta
    );

    let res = view::view_lines(
        &repository,
        large.path_str(),
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    let meta: serde_json::Value = serde_json::from_str(&res.metadata_json).unwrap();
    assert_eq!(meta["total_lines"], 900);
    assert!(
        meta["message"].as_str().unwrap().contains("800"),
        "900 lines should report the cap: {}",
        meta
    );
}

#[tokio::test]
async fn test_preview_id_applies_the_validated_batch() {
    let _lock = acquire_db_lock();
    let original = "fn main() {\n    let a = 1;\n}\n";
    let file = TestFile::new("preview_apply.rs", original);
    let repository = SqliteFileStore;
    let pm = create_test_parser_manager();

    let view = view_range(&repository, file.path_str(), 1, 3, None).unwrap();
    let val: serde_json::Value = serde_json::from_str(&view).unwrap();
    let start_id = val["lines"][1].as_array().unwrap()[0]
        .as_str()
        .unwrap()
        .to_string();

    let edits = vec![LineEdit {
        op: EditOp::Replace,
        start_id: Some(start_id),
        content: Some("    let a = 2;".to_string()),
        ..Default::default()
    }];

    let res = edit::edit_lines_dry_run(&repository, file.path_str(), edits, &pm)
        .await
        .unwrap();
    let preview: serde_json::Value = serde_json::from_str(&res.report).unwrap();
    let preview_id = preview["preview_id"].as_str().unwrap().to_string();
    assert!(
        preview_id.starts_with('p'),
        "unexpected preview id: {}",
        preview_id
    );
    assert_eq!(fs::read_to_string(file.path_str()).unwrap(), original);

    let res = edit::apply_preview(&repository, file.path_str(), &preview_id, &pm)
        .await
        .unwrap();
    let applied: serde_json::Value = serde_json::from_str(&res).unwrap();
    assert!(
        applied["status"].is_null(),
        "success is the exit code: {}",
        applied
    );
    assert!(!applied["modified_lines"].as_array().unwrap().is_empty());
    assert_eq!(
        fs::read_to_string(file.path_str()).unwrap(),
        "fn main() {\n    let a = 2;\n}\n"
    );

    // A preview id is single-use.
    let err = edit::apply_preview(&repository, file.path_str(), &preview_id, &pm)
        .await
        .unwrap_err();
    assert!(
        format!("{}", err).contains("unknown, already applied, or expired"),
        "{}",
        err
    );
}

#[tokio::test]
async fn test_preview_id_is_refused_when_stale_or_misaddressed() {
    let _lock = acquire_db_lock();
    let file = TestFile::new("preview_stale.rs", "fn main() {\n    let a = 1;\n}\n");
    let other = TestFile::new("preview_other.rs", "fn other() {}\n");
    let repository = SqliteFileStore;
    let pm = create_test_parser_manager();

    let view = view_range(&repository, file.path_str(), 1, 3, None).unwrap();
    let val: serde_json::Value = serde_json::from_str(&view).unwrap();
    let start_id = val["lines"][1].as_array().unwrap()[0]
        .as_str()
        .unwrap()
        .to_string();

    let edits = vec![LineEdit {
        op: EditOp::Replace,
        start_id: Some(start_id),
        content: Some("    let a = 2;".to_string()),
        ..Default::default()
    }];

    let res = edit::edit_lines_dry_run(&repository, file.path_str(), edits, &pm)
        .await
        .unwrap();
    let preview: serde_json::Value = serde_json::from_str(&res.report).unwrap();
    let preview_id = preview["preview_id"].as_str().unwrap().to_string();

    // Addressed at the wrong file.
    let err = edit::apply_preview(&repository, other.path_str(), &preview_id, &pm)
        .await
        .unwrap_err();
    assert!(format!("{}", err).contains("belongs to"), "{}", err);

    // The file moves under the preview.
    fs::write(file.path_str(), "fn main() {\n    let a = 99;\n}\n").unwrap();
    let err = edit::apply_preview(&repository, file.path_str(), &preview_id, &pm)
        .await
        .unwrap_err();
    // Compared against what the message is stored as, so editing that copy
    // stays a change to resources/ rather than a change to this file too.
    let stored = &crate::tools::metadata::get_config().error_preview_stale;
    for segment in stored.split("{}").filter(|part| !part.trim().is_empty()) {
        assert!(format!("{}", err).contains(segment), "{}", err);
    }
    assert_eq!(
        fs::read_to_string(file.path_str()).unwrap(),
        "fn main() {\n    let a = 99;\n}\n"
    );
}

#[tokio::test]
/// A verdict is a report, not a veto: a dry run whose result does not parse
/// says so and still names the batch, so a caller who judges the parser wrong
/// applies it rather than sending it again (D-01M28NM3ECNY08).
async fn test_a_failed_dry_run_still_names_its_batch() {
    let _lock = acquire_db_lock();
    let file = TestFile::new("preview_invalid.rs", "fn main() {\n    let a = 1;\n}\n");
    let repository = SqliteFileStore;
    let pm = create_test_parser_manager();

    let view = view_range(&repository, file.path_str(), 1, 3, None).unwrap();
    let val: serde_json::Value = serde_json::from_str(&view).unwrap();
    let start_id = val["lines"][1].as_array().unwrap()[0]
        .as_str()
        .unwrap()
        .to_string();

    let edits = vec![LineEdit {
        op: EditOp::Replace,
        start_id: Some(start_id),
        content: Some("    let a = ;".to_string()),
        ..Default::default()
    }];

    let res = edit::edit_lines_dry_run(&repository, file.path_str(), edits, &pm)
        .await
        .unwrap();
    let preview: serde_json::Value = serde_json::from_str(&res.report).unwrap();
    assert_eq!(preview["syntax_valid"], false);
    assert!(
        preview["diagnostics"].is_array(),
        "no diagnostics beside the verdict: {}",
        res.report
    );
    assert!(
        preview["preview_id"].is_string(),
        "kept no id for the refused batch: {}",
        res.report
    );
}

#[tokio::test]
/// A replace may be given more than one line, and every line it writes has to
/// come back: the answer is what a following edit is addressed with, so an id
/// missing from it, or an id in it that names no line, sends the caller back to
/// re-read a file it was just told about.
async fn test_a_multi_line_replace_names_every_line_it_wrote() {
    let _lock = acquire_db_lock();
    let file = TestFile::new("multi_replace.txt", "one\ntwo\nthree\n");
    let repository = SqliteFileStore;
    let pm = create_test_parser_manager();

    let view = view_range(&repository, file.path_str(), 1, 3, None).unwrap();
    let val: serde_json::Value = serde_json::from_str(&view).unwrap();
    let start_id = val["lines"][1].as_array().unwrap()[0]
        .as_str()
        .unwrap()
        .to_string();

    let edits = vec![LineEdit {
        op: EditOp::Replace,
        start_id: Some(start_id),
        content: Some("alpha\nbeta\ngamma\n".to_string()),
        ..Default::default()
    }];

    let res = edit::edit_lines(&repository, file.path_str(), edits, &pm)
        .await
        .unwrap();
    let answer: serde_json::Value = serde_json::from_str(&res).unwrap();
    let reported = answer["modified_lines"].as_array().unwrap();

    assert_eq!(
        fs::read_to_string(file.path_str()).unwrap(),
        "one\nalpha\nbeta\ngamma\nthree\n"
    );
    assert_eq!(reported.len(), 3, "three lines written, reported: {}", res);

    // Each reported id has to be the id the file's own line carries now.
    let after = view_range(&repository, file.path_str(), 1, 5, None).unwrap();
    let after: serde_json::Value = serde_json::from_str(&after).unwrap();
    for entry in reported {
        let pair = entry.as_array().unwrap();
        let (id, number) = (pair[0].as_str().unwrap(), pair[1].as_i64().unwrap());
        let line = &after["lines"][(number - 1) as usize];
        assert_eq!(
            line.as_array().unwrap()[0].as_str().unwrap(),
            id,
            "line {number} is not the id the edit answered with: {res}"
        );
    }
}
#[tokio::test]
async fn test_store_holds_no_file_text() {
    let _lock = acquire_db_lock();
    let secret = "let api_key = \"correct-horse-battery-staple\";";
    let file = TestFile::new("no_text.rs", &format!("fn main() {{\n    {}\n}}\n", secret));
    let repository = SqliteFileStore;
    let pm = create_test_parser_manager();

    // Touch every path that populates the index.
    let view = view_range(&repository, file.path_str(), 1, 3, None).unwrap();
    let val: serde_json::Value = serde_json::from_str(&view).unwrap();
    let start_id = val["lines"][1].as_array().unwrap()[0]
        .as_str()
        .unwrap()
        .to_string();
    edit::edit_lines(
        &repository,
        file.path_str(),
        vec![LineEdit {
            op: EditOp::Replace,
            start_id: Some(start_id),
            content: Some(format!("    {}", secret)),
            ..Default::default()
        }],
        &pm,
    )
    .await
    .unwrap();

    // Invariant 9: the index is an index, not a copy. Nothing in the database
    // file should contain the text of the lines it tracks.
    let db = crate::tools::store::get_db_path().unwrap();
    // Recent writes sit in the write-ahead log until it is folded back in.
    let raw: Vec<u8> = [db.clone(), db.with_extension("db-wal")]
        .iter()
        .filter_map(|path| fs::read(path).ok())
        .flatten()
        .collect();
    assert!(
        !raw.windows(secret.len()).any(|w| w == secret.as_bytes()),
        "the store contains file text at {}",
        db.display()
    );
}

/// Read the (sequence id, content) pairs the index currently holds.
fn index_pairs(repository: &SqliteFileStore, path: &str) -> Vec<(i64, String)> {
    let meta = file_entry::init_file_entry(path, false).unwrap();
    repository
        .fetch_lines_range(&meta.file_key, 1, meta.total_lines)
        .unwrap()
        .into_iter()
        .map(|(seq, _, content)| (seq, content))
        .collect()
}

#[tokio::test]
async fn test_external_insert_preserves_surrounding_ids() {
    let _lock = acquire_db_lock();
    let file = TestFile::new(
        "reconcile_insert.rs",
        "fn main() {\n    let a = 1;\n    let b = 2;\n}\n",
    );
    let repository = SqliteFileStore;

    let before = index_pairs(&repository, file.path_str());
    assert_eq!(before.len(), 4);

    // Another writer inserts a line in the middle.
    fs::write(
        file.path_str(),
        "fn main() {\n    let a = 1;\n    let mid = 0;\n    let b = 2;\n}\n",
    )
    .unwrap();

    let after = index_pairs(&repository, file.path_str());
    assert_eq!(after.len(), 5);
    for (seq, content) in &before {
        let found = after
            .iter()
            .find(|(_, c)| c == content)
            .expect("a line vanished");
        assert_eq!(found.0, *seq, "line {:?} was re-identified", content);
    }
    let fresh = after.iter().find(|(_, c)| c == "    let mid = 0;").unwrap();
    assert!(
        !before.iter().any(|(seq, _)| *seq == fresh.0),
        "the new line reused an id"
    );
}

#[tokio::test]
async fn test_duplicate_lines_reconcile_without_misalignment() {
    let _lock = acquire_db_lock();
    // Blank lines and bare braces are what a naive diff mis-pairs.
    let original = "fn a() {\n}\n\nfn b() {\n}\n";
    let file = TestFile::new("reconcile_dupes.rs", original);
    let repository = SqliteFileStore;

    let before = index_pairs(&repository, file.path_str());
    assert_eq!(before.len(), 5);

    // A third function is appended, repeating the same brace-and-blank shape.
    fs::write(
        file.path_str(),
        "fn a() {\n}\n\nfn b() {\n}\n\nfn c() {\n}\n",
    )
    .unwrap();

    let after = index_pairs(&repository, file.path_str());
    assert_eq!(after.len(), 8);
    // The original five lines keep their ids and their order.
    assert_eq!(&after[..5], &before[..]);
}

/// The id an agent would hold for a given line, straight from view.
fn live_id(repository: &SqliteFileStore, path: &str, line: usize) -> String {
    let view = view_range(repository, path, line, line, None).unwrap();
    let val: serde_json::Value = serde_json::from_str(&view).unwrap();
    val["lines"][0].as_array().unwrap()[0]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn test_edit_conflicts_only_when_the_target_itself_changed() {
    let _lock = acquire_db_lock();
    let file = TestFile::new(
        "reconcile_conflict.rs",
        "fn main() {\n    let a = 1;\n    let b = 2;\n}\n",
    );
    let repository = SqliteFileStore;
    let pm = create_test_parser_manager();

    // An untargeted line changes underneath. The edit should still land.
    let start_id = live_id(&repository, file.path_str(), 2);
    fs::write(
        file.path_str(),
        "fn main() {\n    let a = 1;\n    let b = 99;\n}\n",
    )
    .unwrap();
    let res = edit::edit_lines(
        &repository,
        file.path_str(),
        vec![LineEdit {
            op: EditOp::Replace,
            start_id: Some(start_id),
            content: Some("    let a = 7;".to_string()),
            ..Default::default()
        }],
        &pm,
    )
    .await
    .unwrap();
    let val: serde_json::Value = serde_json::from_str(&res).unwrap();
    assert!(val["status"].is_null(), "success is the exit code: {}", val);
    assert_eq!(
        fs::read_to_string(file.path_str()).unwrap(),
        "fn main() {\n    let a = 7;\n    let b = 99;\n}\n"
    );

    // The targeted line itself changes underneath. The edit must be refused.
    let start_id = live_id(&repository, file.path_str(), 2);
    fs::write(
        file.path_str(),
        "fn main() {\n    let a = 123;\n    let b = 99;\n}\n",
    )
    .unwrap();
    let err = edit::edit_lines(
        &repository,
        file.path_str(),
        vec![LineEdit {
            op: EditOp::Replace,
            start_id: Some(start_id),
            content: Some("    let a = 8;".to_string()),
            ..Default::default()
        }],
        &pm,
    )
    .await
    .unwrap_err();
    let msg = format!("{}", err);
    assert!(
        msg.contains("changed") && msg.contains("Read that"),
        "expected a refusal naming the conflict and the remedy, got: {}",
        msg
    );
    assert_eq!(
        fs::read_to_string(file.path_str()).unwrap(),
        "fn main() {\n    let a = 123;\n    let b = 99;\n}\n"
    );
}

#[tokio::test]
async fn test_reformatting_preserves_every_id() {
    let _lock = acquire_db_lock();
    let file = TestFile::new(
        "reconcile_format.rs",
        "fn main() {\n    let a = 1;\n    let b = a + 2;\n}\n",
    );
    let repository = SqliteFileStore;

    let before = index_pairs(&repository, file.path_str());
    assert_eq!(before.len(), 4);

    // A formatter respaces the whole file without changing what it means.
    fs::write(
        file.path_str(),
        "fn main( )   {\n\tlet a=1;\n        let b   =   a+2;\n}\n",
    )
    .unwrap();

    let after = index_pairs(&repository, file.path_str());
    assert_eq!(after.len(), 4);
    let ids_before: Vec<i64> = before.iter().map(|(seq, _)| *seq).collect();
    let ids_after: Vec<i64> = after.iter().map(|(seq, _)| *seq).collect();
    assert_eq!(ids_after, ids_before, "a reformat re-identified lines");

    // The index tracks the new spelling, not the old one.
    assert_eq!(after[1].1, "\tlet a=1;");
}

#[tokio::test]
async fn test_a_deleted_id_is_never_reissued() {
    let _lock = acquire_db_lock();
    let file = TestFile::new(
        "no_reuse.rs",
        "fn main() {\n    let a = 1;\n    let b = 2;\n}\n",
    );
    let repository = SqliteFileStore;
    let pm = create_test_parser_manager();

    // Delete a line the file parses without, retiring the id it held.
    let doomed = live_id(&repository, file.path_str(), 3);
    edit::edit_lines(
        &repository,
        file.path_str(),
        vec![LineEdit {
            op: EditOp::Delete,
            start_id: Some(doomed.clone()),
            ..Default::default()
        }],
        &pm,
    )
    .await
    .unwrap();
    let retired: i64 = i64::from_str_radix(doomed.split('#').next().unwrap(), 16).unwrap();

    // Invariant 2: a retired id is never reassigned to a different line.
    let res = edit::edit_lines(
        &repository,
        file.path_str(),
        vec![LineEdit {
            op: EditOp::Append,
            content: Some("// tail".to_string()),
            ..Default::default()
        }],
        &pm,
    )
    .await
    .unwrap();
    let val: serde_json::Value = serde_json::from_str(&res).unwrap();
    let minted = val["modified_lines"][0][0].as_str().unwrap();
    let minted_seq = i64::from_str_radix(minted.split('#').next().unwrap(), 16).unwrap();
    assert_ne!(minted_seq, retired, "id {} was handed out twice", retired);
}

#[tokio::test]
async fn test_repeated_insertion_between_the_same_pair() {
    let _lock = acquire_db_lock();
    let file = TestFile::new("endurance.rs", "fn main() {\n}\n");
    let repository = SqliteFileStore;
    let pm = create_test_parser_manager();

    // Invariant 7: ordering does not degrade under repeated insertion at one
    // point, and no id is handed out twice.
    let mut seen = std::collections::HashSet::new();
    for n in 0..200 {
        let anchor = live_id(&repository, file.path_str(), 1);
        let res = edit::edit_lines(
            &repository,
            file.path_str(),
            vec![LineEdit {
                op: EditOp::InsertAfter,
                start_id: Some(anchor),
                content: Some(format!("    let v{} = {};", n, n)),
                ..Default::default()
            }],
            &pm,
        )
        .await
        .unwrap();
        let val: serde_json::Value = serde_json::from_str(&res).unwrap();
        let minted = val["modified_lines"][0][0]
            .as_str()
            .unwrap()
            .split('#')
            .next()
            .unwrap()
            .to_string();
        assert!(
            seen.insert(minted.clone()),
            "id {} was handed out twice",
            minted
        );
    }

    // Each insert went in right after line 1, so the file reads in reverse.
    let content = fs::read_to_string(file.path_str()).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 202);
    assert_eq!(lines[1], "    let v199 = 199;");
    assert_eq!(lines[200], "    let v0 = 0;");
}

#[tokio::test]
async fn test_a_desynced_index_reconciles_instead_of_renumbering() {
    let _lock = acquire_db_lock();
    let file = TestFile::new("desync.rs", "fn main() {\n    let a = 1;\n}\n");
    let repository = SqliteFileStore;
    let pm = create_test_parser_manager();

    // Push the ids out of step with the positions, so that renumbering the
    // file 1..N would give a visibly different answer from reconciling it.
    let anchor = live_id(&repository, file.path_str(), 1);
    edit::edit_lines(
        &repository,
        file.path_str(),
        vec![LineEdit {
            op: EditOp::InsertAfter,
            start_id: Some(anchor),
            content: Some("    let inserted = 0;".to_string()),
            ..Default::default()
        }],
        &pm,
    )
    .await
    .unwrap();

    let before = index_pairs(&repository, file.path_str());
    assert_eq!(before.len(), 4);
    let ids: Vec<i64> = before.iter().map(|(seq, _)| *seq).collect();
    assert_ne!(
        ids,
        vec![1, 2, 3, 4],
        "the ids never diverged from the positions"
    );

    // Stand in for the narrow window where the file changes between the
    // reconcile at the gate and the buffer load: drop the last index row so the
    // index is shorter than the file, without touching the file itself.
    let meta = file_entry::init_file_entry(file.path_str(), false).unwrap();
    let db = rusqlite::Connection::open(crate::tools::store::get_db_path().unwrap()).unwrap();
    db.execute(
        "DELETE FROM lines WHERE file_key = ?1 AND sequence_id = (SELECT MAX(sequence_id) FROM lines WHERE file_key = ?1)",
        [&meta.file_key],
    ).unwrap();
    drop(db);

    // Read a fixed range rather than the entry's line count: the point is
    // what the buffer recovers, not what the stale count claims.
    let meta = file_entry::init_file_entry(file.path_str(), false).unwrap();
    let after: Vec<(i64, String)> = repository
        .fetch_lines_range(&meta.file_key, 1, 4)
        .unwrap()
        .into_iter()
        .map(|(seq, _, content)| (seq, content))
        .collect();
    assert_eq!(after.len(), 4);

    // Every line whose index row survived keeps the id an agent would hold.
    let destroyed = ids.iter().copied().max().unwrap();
    for (seq, content) in &before {
        if *seq == destroyed {
            continue;
        }
        let found = after
            .iter()
            .find(|(_, c)| c == content)
            .expect("a line vanished");
        assert_eq!(found.0, *seq, "line {:?} was re-identified", content);
    }

    // The one line whose identity was destroyed draws a fresh id, never a
    // number already in use.
    let replacement = after
        .iter()
        .find(|(seq, _)| !ids.contains(seq))
        .expect("no fresh id was minted");
    assert!(!ids.contains(&replacement.0));
}

/// run_inspect prints a JSON report; this is it, parsed.
async fn inspect_report(path: &str, template: Option<&str>) -> serde_json::Value {
    let res = crate::tools::inspect::run_inspect(
        crate::tools::inspect::InspectArgs {
            filepath: path.to_string(),
            query: None,
            template: template.map(str::to_string),
            include_code: Some(false),
        },
        &std::sync::Arc::new(create_test_parser_manager()),
    )
    .await
    .unwrap();
    serde_json::from_str(&res.report).unwrap()
}

#[tokio::test]
async fn test_the_outline_lists_the_definitions_with_ids_to_act_on() {
    let _lock = acquire_db_lock();
    let file = TestFile::new(
        "outline.rs",
        "fn alpha() {\n    let a = 1;\n}\n\nfn beta() {\n}\n",
    );
    let pm = std::sync::Arc::new(create_test_parser_manager());

    let text = crate::tools::outline::run_outline(
        serde_json::from_value(serde_json::json!({ "filepath": file.path_str() })).unwrap(),
        &pm,
    )
    .await
    .unwrap();
    let res: serde_json::Value = serde_json::from_str(&text).unwrap();

    let outline = res["outline"].as_array().unwrap();
    let names: Vec<&str> = outline
        .iter()
        .map(|e| e["signature"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["fn alpha() {", "fn beta() {"], "{:?}", names);
    for entry in outline {
        assert!(
            entry["start_id"].as_str().unwrap().contains('#'),
            "{}",
            entry
        );
        assert!(entry["end_id"].as_str().unwrap().contains('#'), "{}", entry);
    }

    // The same file, dumped: one form is the survey, the other the grammar.
    let sexp = crate::tools::outline::run_outline(
        serde_json::from_value(serde_json::json!({ "filepath": file.path_str(), "sexp": true }))
            .unwrap(),
        &pm,
    )
    .await
    .unwrap();
    assert!(sexp.contains("function_item"), "{}", sexp);
}

#[tokio::test]
async fn test_inspect_refuses_to_search_for_nothing_and_names_the_outline() {
    let _lock = acquire_db_lock();
    let file = TestFile::new("nothing.rs", "fn alpha() {\n}\n");
    let err = crate::tools::inspect::run_inspect(
        crate::tools::inspect::InspectArgs {
            filepath: file.path_str().to_string(),
            query: None,
            template: None,
            include_code: Some(false),
        },
        &std::sync::Arc::new(create_test_parser_manager()),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("ast-editor outline"), "{}", err);
}

#[tokio::test]
async fn test_a_structural_match_carries_the_ids_that_edit_it() {
    let _lock = acquire_db_lock();
    let file = TestFile::new(
        "bridge.rs",
        "fn keep() {\n}\n\nfn doomed() {\n    let x = 1;\n}\n",
    );
    let repository = SqliteFileStore;
    let pm = create_test_parser_manager();

    let res = inspect_report(file.path_str(), Some("functions")).await;

    let doomed = res["matches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["text"].as_str().unwrap().contains("doomed"))
        .expect("the function was not matched");

    // The ids the query returned go straight to edit_lines_permissive, with no call in
    // between to turn line numbers into ids.
    let out = edit::edit_lines(
        &repository,
        file.path_str(),
        vec![LineEdit {
            op: EditOp::Replace,
            start_id: Some(doomed["start_id"].as_str().unwrap().to_string()),
            end_id: Some(doomed["end_id"].as_str().unwrap().to_string()),
            content: Some("// gone".to_string()),
            ..Default::default()
        }],
        &pm,
    )
    .await
    .unwrap();
    let val: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(val["status"].is_null(), "success is the exit code: {}", val);
    assert_eq!(
        fs::read_to_string(file.path_str()).unwrap(),
        "fn keep() {\n}\n\n// gone\n"
    );
}

/// A temporary directory to put fixtures in. The grammars are compiled in
/// (D-01M28RAGW19ZZC), so nothing else has to be arranged.
struct TestEnvironment {
    dir: std::path::PathBuf,
    pm: ParserManager,
}

impl TestEnvironment {
    fn new(name: &str) -> Self {
        let dir = crate::tools::test_temp_dir(&format!("tree_sitter_edit_tests_{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self {
            dir,
            pm: ParserManager::new().unwrap(),
        }
    }
}

impl Drop for TestEnvironment {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// The id of the line whose text matches, read off the line that carries it.
fn find_line_id(view_res: &crate::tools::view::ViewLinesOutput, pattern: &str) -> String {
    let lines_text = view_res.lines_text.as_ref().unwrap();
    let mut current = String::new();
    for row in lines_text.lines() {
        if let Some((head, _)) = row.split_once(": ") {
            if let Some((id, _)) = head.split_once('|') {
                current = id.trim().to_string();
            }
        }
        if row.contains(pattern) {
            return current;
        }
    }
    String::new()
}

fn test_view_lines(
    repository: &impl crate::tools::repository::FileStore,
    filepath: &str,
    start_line: usize,
    end_line: usize,
    only_ids: Option<bool>,
) -> Result<crate::tools::view::ViewLinesOutput> {
    crate::tools::view::view_lines(
        repository,
        filepath,
        Some(start_line),
        Some(end_line),
        only_ids,
        None,
        None,
        None,
    )
}

#[tokio::test]
async fn test_apply_line_edits_insert_update_delete() -> Result<()> {
    let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
    let env = TestEnvironment::new("basic_ops");
    let repository = SqliteFileStore;

    let file_path = env.dir.join("code.rs");
    fs::write(&file_path, "fn main() {\n    let a = 1;\n}\n")?;
    let filepath_str = file_path.to_str().unwrap();

    let metadata = init_file_entry(filepath_str, false)?;
    assert_eq!(metadata.total_lines, 3);

    // Get the line IDs by viewing
    let lines_view = test_view_lines(&repository, filepath_str, 1, 3, None)?;
    let start_id = find_line_id(&lines_view, "let a = 1;");
    assert!(!start_id.is_empty());

    // 1. Test update
    let edits = vec![LineEdit {
        op: EditOp::Replace,
        start_id: Some(start_id.clone()),
        content: Some("    let a = 42;".to_string()),
        ..Default::default()
    }];
    let preview = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
    let res: serde_json::Value = serde_json::from_str(&preview)?;
    assert!(res["status"].is_null(), "success is the exit code: {}", res);
    let modified = res["modified_lines"].as_array().unwrap();
    let (seq, _) = parse_line_id(&start_id)?;
    let expected_prefix = format!("{:x}#", seq);
    // Each entry is [id, line] (card #5).
    assert!(modified
        .iter()
        .any(|entry| entry[0].as_str().unwrap().starts_with(&expected_prefix)));
    assert_eq!(
        fs::read_to_string(&file_path)?,
        "fn main() {\n    let a = 42;\n}\n"
    );

    // Refresh the entry
    let _metadata = init_file_entry(filepath_str, false)?;
    let lines_view = test_view_lines(&repository, filepath_str, 1, 3, None)?;
    let new_start_id = find_line_id(&lines_view, "let a = 42;");
    assert!(!new_start_id.is_empty());

    // 2. Test insert_after
    let edits = vec![LineEdit {
        op: EditOp::InsertAfter,
        start_id: Some(new_start_id.clone()),
        content: Some("    let b = 2;".to_string()),
        ..Default::default()
    }];
    let preview = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
    let res: serde_json::Value = serde_json::from_str(&preview)?;
    assert!(res["status"].is_null(), "success is the exit code: {}", res);
    let modified = res["modified_lines"].as_array().unwrap();
    assert_eq!(
        fs::read_to_string(&file_path)?,
        "fn main() {\n    let a = 42;\n    let b = 2;\n}\n"
    );

    // Refresh the entry to get the latest ids
    let _ = init_file_entry(filepath_str, false)?;
    let lines_view = test_view_lines(&repository, filepath_str, 1, 4, None)?;
    let b_id = find_line_id(&lines_view, "let b = 2;");
    assert!(!b_id.is_empty());
    assert!(modified
        .iter()
        .any(|entry| entry[0].as_str().unwrap() == b_id));

    // 3. Test delete
    let edits = vec![LineEdit {
        op: EditOp::Delete,
        start_id: Some(b_id.clone()),
        content: None,
        ..Default::default()
    }];
    let _preview = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
    assert_eq!(
        fs::read_to_string(&file_path)?,
        "fn main() {\n    let a = 42;\n}\n"
    );

    Ok(())
}

#[tokio::test]
async fn test_concurrency_smart_resync() -> Result<()> {
    let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
    let env = TestEnvironment::new("concurrency");
    let repository = SqliteFileStore;

    let file_path = env.dir.join("code.rs");
    fs::write(&file_path, "fn main() {}\n")?;
    let filepath_str = file_path.to_str().unwrap();

    let _metadata = init_file_entry(filepath_str, false)?;

    // Modify file externally on disk, changing its mtime
    tokio::time::sleep(tokio::time::Duration::from_millis(1100)).await;
    fs::write(&file_path, "fn main() {\n    // changed externally\n}\n")?;

    let edits = vec![LineEdit {
        op: EditOp::InsertAfter,
        start_id: None,
        content: Some("// success after resync\n".to_string()),
        ..Default::default()
    }];

    let res = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
    assert!(res.contains("modified_lines"), "{}", res);

    // Verify disk content includes both the external change and our new edit!
    let content = fs::read_to_string(&file_path)?;
    assert!(content.contains("changed externally"));
    assert!(content.contains("success after resync"));

    Ok(())
}

#[tokio::test]
async fn test_checksum_error() -> Result<()> {
    let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
    let env = TestEnvironment::new("checksum");
    let repository = SqliteFileStore;

    let file_path = env.dir.join("code.rs");
    fs::write(&file_path, "fn main() {}\n")?;
    let filepath_str = file_path.to_str().unwrap();

    let _metadata = init_file_entry(filepath_str, false)?;

    let edits = vec![LineEdit {
        op: EditOp::Replace,
        start_id: Some("1#9999".to_string()), // Invalid hash prefix
        content: Some("fn main() { // updated }".to_string()),
        ..Default::default()
    }];

    let res = edit_lines(&repository, filepath_str, edits, &env.pm).await;
    assert!(res.is_err());
    let err_msg = res.unwrap_err().to_string();
    assert!(
        err_msg.contains("changed since you read it"),
        "Expected checksum error, got: {}",
        err_msg
    );

    Ok(())
}

#[tokio::test]
async fn test_syntax_validation_error_rolls_back() -> Result<()> {
    let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
    let env = TestEnvironment::new("syntax_error");
    let repository = SqliteFileStore;

    let file_path = env.dir.join("code.rs");
    let initial_content = "fn main() {\n    let a = 1;\n}\n";
    fs::write(&file_path, initial_content)?;
    let filepath_str = file_path.to_str().unwrap();

    let _metadata = init_file_entry(filepath_str, false)?;

    // Find the line 2 ID
    let lines_view = test_view_lines(&repository, filepath_str, 1, 3, None)?;
    let start_id = find_line_id(&lines_view, "let a = 1;");
    assert!(!start_id.is_empty());

    // Apply edit that introduces syntax error (e.g. mismatched braces / parsing error)
    let edits = vec![LineEdit {
        op: EditOp::Replace,
        start_id: Some(start_id),
        content: Some("    let a = {;".to_string()), // Syntax error
        ..Default::default()
    }];

    // The batch is refused, the file is left alone, and the batch is kept
    // under an id the answer names.
    let res_strict = edit_lines(&repository, filepath_str, edits, &env.pm).await;
    assert!(res_strict.is_err());
    let err_msg = res_strict.unwrap_err().to_string();
    assert!(
        err_msg.contains("\"syntax_valid\": false"),
        "Expected the refusal to carry its verdict, got: {}",
        err_msg
    );
    assert!(
        err_msg.contains("preview_id"),
        "Expected the refusal to name the batch it kept, got: {}",
        err_msg
    );

    // Verify disk content was rolled back (i.e. remains unchanged)
    assert_eq!(fs::read_to_string(&file_path)?, initial_content);

    // Verify DB content was rolled back (re-query content of line 2)
    let lines_strict = repository.fetch_lines_range(&_metadata.file_key, 2, 2)?;
    assert_eq!(lines_strict.len(), 1);
    assert_eq!(lines_strict[0].2.trim(), "let a = 1;");

    fs::remove_dir_all(&env.dir)?;
    Ok(())
}

#[tokio::test]
async fn test_insert_into_empty_file() -> Result<()> {
    let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
    let env = TestEnvironment::new("empty_file");
    let repository = SqliteFileStore;

    let file_path = env.dir.join("code.rs");
    fs::write(&file_path, "")?;
    let filepath_str = file_path.to_str().unwrap();

    let metadata = init_file_entry(filepath_str, false)?;
    assert_eq!(metadata.total_lines, 0);

    let edits = vec![LineEdit {
        op: EditOp::Append,
        start_id: None,
        content: Some("fn main() {\n}".to_string()),
        ..Default::default()
    }];

    let preview = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
    let res: serde_json::Value = serde_json::from_str(&preview)?;
    assert!(res["status"].is_null(), "success is the exit code: {}", res);
    let modified = res["modified_lines"].as_array().unwrap();
    assert!(!modified.is_empty());
    assert_eq!(fs::read_to_string(&file_path)?, "fn main() {\n}\n");

    Ok(())
}

#[tokio::test]
async fn test_case_insensitive_validation_and_deletion_preview() -> Result<()> {
    let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
    let env = TestEnvironment::new("case_insensitive_and_delete");
    let repository = SqliteFileStore;

    // Use uppercase extension: .RS
    let file_path = env.dir.join("code.RS");
    fs::write(
        &file_path,
        "fn main() {\n    let a = 1;\n    let b = 2;\n    let c = 3;\n}\n",
    )?;
    let filepath_str = file_path.to_str().unwrap();

    let metadata = init_file_entry(filepath_str, false)?;
    assert_eq!(metadata.total_lines, 5);

    // Get the line IDs by viewing
    let lines_view = test_view_lines(&repository, filepath_str, 1, 5, None)?;
    let b_id = find_line_id(&lines_view, "let b = 2;");
    assert!(!b_id.is_empty());

    // Test delete on .RS file (verifies lowercase lookup/delegation works for uppercase extensions too)
    let edits = vec![LineEdit {
        op: EditOp::Delete,
        start_id: Some(b_id.clone()),
        content: None,
        ..Default::default()
    }];

    let preview = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
    let res: serde_json::Value = serde_json::from_str(&preview)?;
    assert!(res["status"].is_null(), "success is the exit code: {}", res);
    // A delete writes no line, so what says it happened is the run of lines
    // that moved up into the hole.
    assert!(
        res["modified_lines"].as_array().unwrap().is_empty(),
        "a delete wrote a line: {}",
        res
    );
    assert!(
        !res["renumbered"].as_array().unwrap().is_empty(),
        "the lines after the hole kept their numbers: {}",
        res
    );

    let disk_content = fs::read_to_string(&file_path)?;
    assert!(disk_content.contains("let a = 1;"));
    assert!(disk_content.contains("let c = 3;"));
    assert!(!disk_content.contains("let b = 2;"));

    Ok(())
}

#[tokio::test]
async fn test_append_operation_empty_file() -> Result<()> {
    let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
    let env = TestEnvironment::new("append_empty");
    let repository = SqliteFileStore;

    let file_path = env.dir.join("code.rs");
    fs::write(&file_path, "")?;
    let filepath_str = file_path.to_str().unwrap();

    let metadata = init_file_entry(filepath_str, false)?;
    assert_eq!(metadata.total_lines, 0);

    let edits = vec![LineEdit {
        op: EditOp::Append,
        start_id: None,
        content: Some("pub fn foo() {}".to_string()),
        ..Default::default()
    }];

    let preview = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
    let res: serde_json::Value = serde_json::from_str(&preview)?;
    assert!(res["status"].is_null(), "success is the exit code: {}", res);
    let modified = res["modified_lines"].as_array().unwrap();
    assert!(!modified.is_empty());
    assert_eq!(fs::read_to_string(&file_path)?, "pub fn foo() {}\n");

    Ok(())
}

#[tokio::test]
async fn test_append_operation_with_content() -> Result<()> {
    let _lock = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = TestEnvironment::new("append_content");
    let repository = SqliteFileStore;

    let file_path = env.dir.join("code.rs");
    fs::write(&file_path, "pub fn foo() {}\n")?;
    let filepath_str = file_path.to_str().unwrap();

    let metadata = init_file_entry(filepath_str, false)?;
    assert_eq!(metadata.total_lines, 1);

    let edits = vec![LineEdit {
        op: EditOp::Append,
        start_id: None,
        content: Some("pub fn bar() -> i32 {\n    42\n}".to_string()),
        ..Default::default()
    }];

    let preview = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
    let res: serde_json::Value = serde_json::from_str(&preview)?;
    assert!(res["status"].is_null(), "success is the exit code: {}", res);
    let modified = res["modified_lines"].as_array().unwrap();
    assert!(!modified.is_empty());
    assert_eq!(
        fs::read_to_string(&file_path)?,
        "pub fn foo() {}\npub fn bar() -> i32 {\n    42\n}\n"
    );

    Ok(())
}

#[tokio::test]
async fn test_strict_start_id_validation() -> Result<()> {
    let _lock = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = TestEnvironment::new("strict_validation");
    let repository = SqliteFileStore;

    let file_path = env.dir.join("code.rs");
    fs::write(&file_path, "pub fn foo() {}\n")?;
    let filepath_str = file_path.to_str().unwrap();

    let metadata = init_file_entry(filepath_str, false)?;
    assert_eq!(metadata.total_lines, 1);

    // Call update with start_id = None
    let edits = vec![LineEdit {
        op: EditOp::Replace,
        start_id: None,
        content: Some("pub fn bar() {}".to_string()),
        ..Default::default()
    }];

    let result = edit_lines(&repository, filepath_str, edits, &env.pm).await;
    assert!(result.is_err());
    let err_msg = result.err().unwrap().to_string();
    // The op and the field are named as a caller spells them, and so is
    // what the edit did carry (card #54).
    assert!(err_msg.contains("'replace' op needs start_id"), "{err_msg}");
    assert!(err_msg.contains("carries content"), "{err_msg}");

    Ok(())
}

#[tokio::test]
async fn test_prepend_operation() -> Result<()> {
    let _lock = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = TestEnvironment::new("prepend");
    let repository = SqliteFileStore;

    let file_path = env.dir.join("code.rs");
    fs::write(&file_path, "pub fn hello() {}\n")?;
    let filepath_str = file_path.to_str().unwrap();

    let _metadata = init_file_entry(filepath_str, false)?;

    let edits = vec![LineEdit {
        op: EditOp::Prepend,
        start_id: None,
        content: Some("use std::collections::HashMap;\n\n".to_string()),
        ..Default::default()
    }];

    let preview = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
    let res: serde_json::Value = serde_json::from_str(&preview)?;
    assert!(res["status"].is_null(), "success is the exit code: {}", res);
    let modified = res["modified_lines"].as_array().unwrap();
    assert!(!modified.is_empty());
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
    let repository = SqliteFileStore;

    let file_path = env.dir.join("code.rs");
    fs::write(
        &file_path,
        "pub fn foo() {\n    let a = 1;\n    let b = 2;\n}\n",
    )?;
    let filepath_str = file_path.to_str().unwrap();

    let _metadata = init_file_entry(filepath_str, false)?;

    // Find IDs for lines 2 and 3
    let lines_view = test_view_lines(&repository, filepath_str, 2, 3, None)?;
    let id_2 = find_line_id(&lines_view, "let a = 1;");
    let id_3 = find_line_id(&lines_view, "let b = 2;");

    let edits = vec![LineEdit {
        op: EditOp::Replace,
        start_id: Some(id_2),
        end_id: Some(id_3),
        content: Some("    let val = 42;".to_string()),
        ..Default::default()
    }];

    let preview = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
    let res: serde_json::Value = serde_json::from_str(&preview)?;
    assert!(res["status"].is_null(), "success is the exit code: {}", res);
    let modified = res["modified_lines"].as_array().unwrap();
    assert!(!modified.is_empty());
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
    let repository = SqliteFileStore;

    let file_path = env.dir.join("code.rs");
    fs::write(
        &file_path,
        "pub fn main() {\n    foo();\n}\npub fn foo() {}\n",
    )?;
    let filepath_str = file_path.to_str().unwrap();

    let _metadata = init_file_entry(filepath_str, false)?;

    // Get target ID for lines 4 (pub fn foo() {})
    let lines_view = test_view_lines(&repository, filepath_str, 4, 4, None)?;
    let foo_id = find_line_id(&lines_view, "pub fn foo() {}");

    // Get target ID for line 1 (pub fn main() {)
    let lines_view_main = test_view_lines(&repository, filepath_str, 1, 1, None)?;
    let main_id = find_line_id(&lines_view_main, "pub fn main() {");

    // Move 'foo' function before 'main' function
    let edits = vec![LineEdit {
        op: EditOp::Move,
        start_id: Some(foo_id),
        dest_id: Some(main_id),
        move_position: Some(MovePosition::Before),
        ..Default::default()
    }];

    let preview = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
    let res: serde_json::Value = serde_json::from_str(&preview)?;
    assert!(res["status"].is_null(), "success is the exit code: {}", res);
    let modified = res["modified_lines"].as_array().unwrap();
    assert!(!modified.is_empty());
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

    let repository = SqliteFileStore;

    // Call edit_lines_permissive directly without calling init_file_entry.
    // We can append a line. Since it's append, start_id is ignored.
    let edits = vec![LineEdit {
        op: EditOp::Append,
        content: Some("line 3".to_string()),
        ..Default::default()
    }];

    let preview = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
    let res: serde_json::Value = serde_json::from_str(&preview)?;
    assert!(res["status"].is_null(), "success is the exit code: {}", res);
    let modified = res["modified_lines"].as_array().unwrap();
    assert!(!modified.is_empty());

    let content = fs::read_to_string(&file_path)?;
    assert_eq!(content, "line 1\nline 2\nline 3\n");

    fs::remove_dir_all(&env.dir)?;
    Ok(())
}

#[tokio::test]
async fn test_long_line_replace_substring() -> Result<()> {
    let _lock = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = TestEnvironment::new("long_line_substring_test");
    let repository = SqliteFileStore;

    let file_path = env.dir.join("code.rs");
    let long_line = "a".repeat(2050);
    let file_content = format!("fn main() {{\n    // {}\n}}\n", long_line);
    fs::write(&file_path, &file_content)?;
    let filepath_str = file_path.to_str().unwrap();

    let metadata = init_file_entry(filepath_str, false)?;
    assert_eq!(metadata.total_lines, 3);

    // Get the lines view
    let lines_view = test_view_lines(&repository, filepath_str, 1, 3, None)?;
    let lines_text = lines_view.lines_text.as_ref().unwrap();

    // Verify that the long line is soft-wrapped in the output
    assert!(lines_text.contains("2:     // aaaaa"));
    assert!(lines_text.contains("└: aaaaa"));

    // Get target ID (which should NOT have #TRUNC suffix)
    let start_id = find_line_id(&lines_view, "    // aaaaa");
    assert!(!start_id.is_empty());
    assert!(!start_id.contains("#TRUNC"));

    // 1. Perform replace_substring on the long line
    let edits = vec![LineEdit {
        op: EditOp::ReplaceSubstring,
        start_id: Some(start_id.clone()),
        pattern: Some("aaaaa".to_string()),
        replacement: Some("bbbbb".to_string()),
        occurrence: Some(1),
        ..Default::default()
    }];

    let result = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
    assert!(result.contains("modified_lines"), "{}", result);

    // Verify that the file content was updated on disk
    let disk_content = fs::read_to_string(&file_path)?;
    assert!(disk_content.contains("bbbbbaaaaa")); // Only the first match was replaced

    fs::remove_dir_all(&env.dir)?;
    Ok(())
}

#[tokio::test]
async fn test_hybrid_policy_markdown_warning() -> Result<()> {
    let _lock = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = TestEnvironment::new("hybrid_md_warning");
    let repository = SqliteFileStore;

    let file_path = env.dir.join("doc.md");
    let md_content = "# Title\nSome content\n";
    fs::write(&file_path, md_content)?;
    let filepath_str = file_path.to_str().unwrap();

    let metadata = init_file_entry(filepath_str, false)?;
    assert_eq!(metadata.total_lines, 2);

    // Get the lines view
    let lines_view = test_view_lines(&repository, filepath_str, 1, 2, None)?;
    let start_id = find_line_id(&lines_view, "Some content");
    assert!(!start_id.is_empty());

    // Make an edit that introduces a lint warning (unclosed fence, header hierarchy gap, and malformed link)
    let edits = vec![LineEdit {
        op: EditOp::Replace,
        start_id: Some(start_id),
        content: Some("### Heading Gap\n\n[text(url)\n\n```rust\nlet x = 1;\n".to_string()),
        ..Default::default()
    }];

    let result = edit_lines(&repository, filepath_str, edits, &env.pm).await?;

    // The edit should succeed (status success) but return warnings!
    assert!(
        !result.contains("\"status\""),
        "success is the exit code: {}",
        result
    );
    assert!(result.contains("\"warnings\":"));
    assert!(result.contains("Header hierarchy mismatch"));
    assert!(result.contains("Possible malformed link syntax"));
    assert!(result.contains("Unclosed fenced code block"));

    // Verify that the file was indeed updated on disk
    let disk_content = fs::read_to_string(&file_path)?;
    assert!(disk_content.contains("### Heading Gap"));
    assert!(disk_content.contains("[text(url)"));

    fs::remove_dir_all(&env.dir)?;
    Ok(())
}

#[tokio::test]
async fn test_hybrid_policy_html_warning() -> Result<()> {
    let _lock = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = TestEnvironment::new("hybrid_html_warning");
    let repository = SqliteFileStore;

    let file_path = env.dir.join("index.html");
    let html_content = "<div>\n  <p>Hello</p>\n</div>\n";
    fs::write(&file_path, html_content)?;
    let filepath_str = file_path.to_str().unwrap();

    let metadata = init_file_entry(filepath_str, false)?;
    assert_eq!(metadata.total_lines, 3);

    // Get the lines view
    let lines_view = test_view_lines(&repository, filepath_str, 1, 3, None)?;
    let start_id = find_line_id(&lines_view, "<p>Hello</p>");
    assert!(!start_id.is_empty());

    // Make an edit that has invalid HTML syntax
    let edits = vec![LineEdit {
        op: EditOp::Replace,
        start_id: Some(start_id),
        content: Some("<p>Hello <span class=</p>".to_string()),
        ..Default::default()
    }];

    let result = edit_lines(&repository, filepath_str, edits, &env.pm).await?;

    // The edit should succeed (status success) but return HTML warnings!
    assert!(
        !result.contains("\"status\""),
        "success is the exit code: {}",
        result
    );
    assert!(result.contains("\"warnings\":"));
    assert!(result.contains("HTML syntax warning"));

    fs::remove_dir_all(&env.dir)?;
    Ok(())
}

#[tokio::test]
async fn test_edit_lines_markdown_success() -> Result<()> {
    let _lock = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = TestEnvironment::new("markdown_success");
    let repository = SqliteFileStore;

    let file_path = env.dir.join("test.md");
    let file_content = "# Title\n\n```rust\nfn main() {}\n```\n";
    fs::write(&file_path, file_content)?;

    let filepath_str = file_path.to_str().unwrap();
    let _init_res = init_file_entry(filepath_str, false)?;

    let lines_view = test_view_lines(&repository, filepath_str, 1, 100, None)?;
    let start_line_id = find_line_id(&lines_view, "fn main()");

    let edits = vec![LineEdit {
        op: EditOp::Replace,
        start_id: Some(start_line_id),
        content: Some("fn main() { println!(\"x\"); }".to_string()),
        ..Default::default()
    }];

    let result = edit_lines(&repository, filepath_str, edits, &env.pm).await;
    assert!(result.is_ok());

    let disk_content = fs::read_to_string(&file_path)?;
    assert_eq!(
        disk_content,
        "# Title\n\n```rust\nfn main() { println!(\"x\"); }\n```\n"
    );

    fs::remove_dir_all(&env.dir)?;
    Ok(())
}

#[tokio::test]
async fn test_edit_lines_markdown_soft_warning() -> Result<()> {
    let _lock = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = TestEnvironment::new("markdown_failure");
    let repository = SqliteFileStore;

    let file_path = env.dir.join("test.md");
    let file_content = "# Title\n\n```rust\nfn main() {}\n````\n";
    fs::write(&file_path, file_content)?;

    let filepath_str = file_path.to_str().unwrap();
    let _init_res = init_file_entry(filepath_str, false)?;

    let lines_view = test_view_lines(&repository, filepath_str, 1, 100, None)?;
    let target_line_id = find_line_id(&lines_view, "````");

    let edits = vec![LineEdit {
        op: EditOp::Delete,
        start_id: Some(target_line_id),
        ..Default::default()
    }];

    let result = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
    assert!(
        !result.contains("\"status\""),
        "success is the exit code: {}",
        result
    );
    assert!(result.contains("Unclosed fenced code block"));

    let disk_content = fs::read_to_string(&file_path)?;
    assert_eq!(disk_content, "# Title\n\n```rust\nfn main() {}\n");

    fs::remove_dir_all(&env.dir)?;
    Ok(())
}

#[tokio::test]
async fn test_edit_lines_success_response_formatting() -> Result<()> {
    let _lock = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = TestEnvironment::new("response_formatting");
    let repository = SqliteFileStore;

    // A file a grammar covers: an edit that parses says nothing but its
    // ids, which is what this checks. The other case is its own test.
    let file_path = env.dir.join("test.rs");
    let file_content = "fn a() {}\nfn b() {}\nfn c() {}";
    fs::write(&file_path, file_content)?;

    let filepath_str = file_path.to_str().unwrap();
    let _init_res = init_file_entry(filepath_str, false)?;

    let lines_view = test_view_lines(&repository, filepath_str, 1, 3, None)?;
    let line_1_id = find_line_id(&lines_view, "fn a() {}");

    let edits = vec![LineEdit {
        op: EditOp::Replace,
        start_id: Some(line_1_id.clone()),
        content: Some("fn renamed() {}".to_string()),
        ..Default::default()
    }];

    let result = edit_lines(&repository, filepath_str, edits, &env.pm).await?;

    let val: serde_json::Value = serde_json::from_str(&result)?;
    assert!(val["status"].is_null(), "success is the exit code: {}", val);
    let modified_lines = val["modified_lines"].as_array().unwrap();
    assert_eq!(modified_lines.len(), 1);
    let new_id = modified_lines[0][0].as_str().unwrap();
    assert_eq!(
        modified_lines[0][1].as_u64().unwrap(),
        1,
        "the id says which line it is: {}",
        val
    );
    let (old_seq, _) = crate::tools::line_id::parse_line_id(&line_1_id)?;
    let (new_seq, _) = crate::tools::line_id::parse_line_id(new_id)?;
    assert_eq!(old_seq, new_seq);

    let expected_json = format!(
        "{{\n  \"modified_lines\": [\n    [\"{}\",1]\n  ]\n}}",
        new_id
    );
    assert_eq!(result, expected_json);

    fs::remove_dir_all(&env.dir)?;
    Ok(())
}

#[tokio::test]
async fn test_bash_syntax_validation_error_rolls_back() -> Result<()> {
    let _lock = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = TestEnvironment::new("bash_syntax_error");
    let repository = SqliteFileStore;

    let file_path = env.dir.join("script.sh");
    let initial_content = "if [ \"$x\" = \"1\" ]; then\n    echo \"one\"\nfi\n";
    fs::write(&file_path, initial_content)?;
    let filepath_str = file_path.to_str().unwrap();

    let _metadata = init_file_entry(filepath_str, false)?;

    // Find the line 3 ID (which is "fi")
    let lines_view = test_view_lines(&repository, filepath_str, 1, 3, None)?;
    let start_id = find_line_id(&lines_view, "fi");
    assert!(!start_id.is_empty());

    // Apply edit that introduces syntax error (e.g. replacing "fi" with "else")
    let edits = vec![LineEdit {
        op: EditOp::Replace,
        start_id: Some(start_id),
        content: Some("else".to_string()), // Syntax error since mismatched if/else without fi
        ..Default::default()
    }];

    // The batch is refused, the file is left alone, and the batch is kept
    // under an id the answer names.
    let res_strict = edit_lines(&repository, filepath_str, edits, &env.pm).await;
    assert!(res_strict.is_err());
    let err_msg = res_strict.unwrap_err().to_string();
    assert!(
        err_msg.contains("\"syntax_valid\": false"),
        "Expected the refusal to carry its verdict, got: {}",
        err_msg
    );
    assert!(
        err_msg.contains("preview_id"),
        "Expected the refusal to name the batch it kept, got: {}",
        err_msg
    );

    // Verify disk content was rolled back (i.e. remains unchanged)
    assert_eq!(fs::read_to_string(&file_path)?, initial_content);

    // Verify DB content was rolled back (re-query content of line 3)
    let lines_strict = repository.fetch_lines_range(&_metadata.file_key, 3, 3)?;
    assert_eq!(lines_strict.len(), 1);
    assert_eq!(lines_strict[0].2.trim(), "fi");

    fs::remove_dir_all(&env.dir)?;
    Ok(())
}

/// Taking a preview sweeps the ones that outlived their hour.
///
/// `create_preview` clears the expired rows before it writes its own, with the
/// same cutoff the periodic sweep uses. Read as anything but `now` less the
/// lifetime it either spares every stale row or takes every live one.
///
/// The stale row goes in after the last read, because `init_file_entry` sweeps
/// on its way past and would take it before `create_preview` was asked to.
#[tokio::test]
async fn taking_a_preview_clears_the_ones_that_expired() {
    let _lock = acquire_db_lock();
    let file = TestFile::new("preview_sweep.rs", "fn main() {}\n");
    let repository = SqliteFileStore;
    let target = live_id(&repository, file.path_str(), 1);

    let stale = "/preview/expired.rs";
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    {
        let conn = crate::tools::store::get_db_connection().unwrap();
        conn.execute("DELETE FROM previews WHERE filepath = ?1", [stale])
            .unwrap();
        conn.execute(
            "INSERT INTO previews (filepath, file_hash, edits_json, created_at)
             VALUES (?1, 'h', '[]', ?2);",
            rusqlite::params![stale, now - crate::tools::store::PREVIEW_TTL_SECONDS - 60],
        )
        .unwrap();
    }

    repository
        .create_preview(
            file.path_str(),
            &[LineEdit {
                op: EditOp::Replace,
                start_id: Some(target),
                content: Some("fn main() {}\n".to_string()),
                ..Default::default()
            }],
        )
        .unwrap();

    let conn = crate::tools::store::get_db_connection().unwrap();
    let left: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM previews WHERE filepath = ?1",
            [stale],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(left, 0, "a preview past its hour survived a new one");
}

/// A move whose destination is inside the block being moved is refused, at
/// either end of it.
///
/// `move_span` shifts a destination that sits after the block by the number of
/// lines the drain took, and leaves one before it alone. The two readings of
/// that comparison differ only when the destination is the block's own last
/// line, which this refuses first — so no test can tell them apart, and
/// `mutants.toml` says so on the strength of this.
#[tokio::test]
async fn a_move_into_the_block_being_moved_is_refused() {
    let _lock = acquire_db_lock();
    let file = TestFile::new("self_move.txt", "a\nb\nc\nd\n");
    let path = file.path_str();
    let repository = SqliteFileStore;
    let pm = create_test_parser_manager();

    for line in [1, 2] {
        let start = live_id(&repository, path, 1);
        let end = live_id(&repository, path, 2);
        let dest = live_id(&repository, path, line);
        let refused = edit_lines(
            &repository,
            path,
            vec![LineEdit {
                op: EditOp::Move,
                start_id: Some(start),
                end_id: Some(end),
                dest_id: Some(dest),
                move_position: Some(MovePosition::After),
                ..Default::default()
            }],
            &pm,
        )
        .await;
        let err = refused
            .expect_err(&format!(
                "a move onto line {line} of its own block was allowed"
            ))
            .to_string();
        assert!(
            err.contains("Cannot move a range into itself"),
            "line {line}: {err}"
        );
    }

    assert_eq!(std::fs::read_to_string(path).unwrap(), "a\nb\nc\nd\n");
}

/// After an edit, the store's counter stands above every sequence number the
/// file holds.
///
/// `commit_buffer` writes lines without touching `files.next_line_id`; what
/// keeps it ahead is the reconcile that runs on the next read, because the edit
/// changed the file on disk. `load_buffer` takes
/// `max(stored_next, highest_live + 1)`, and this invariant is why the second
/// term never decides — which is why mutating it survives a mutation run and is
/// excluded rather than chased.
///
/// If this stops holding, that term is the only thing between a new line and a
/// live line's number, and the exclusion has to go with it.
#[tokio::test]
async fn the_stored_counter_stays_above_every_live_sequence_number() {
    let _lock = acquire_db_lock();
    let file = TestFile::new("seq_counter.rs", "fn main() {}\n");
    let path = file.path_str();
    let repository = SqliteFileStore;
    let pm = create_test_parser_manager();

    for content in ["// one\n// two\n", "// three\n"] {
        let target = live_id(&repository, path, 1);
        edit_lines(
            &repository,
            path,
            vec![LineEdit {
                op: EditOp::InsertAfter,
                start_id: Some(target),
                content: Some(content.to_string()),
                ..Default::default()
            }],
            &pm,
        )
        .await
        .unwrap();

        let meta = file_entry::init_file_entry(path, false).unwrap();
        let highest = index_pairs(&repository, path)
            .into_iter()
            .map(|(seq, _)| seq)
            .max()
            .unwrap();
        let conn = crate::tools::store::get_db_connection().unwrap();
        let stored: i64 = conn
            .query_row(
                "SELECT next_line_id FROM files WHERE file_key = ?1",
                [&meta.file_key],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            stored > highest,
            "the counter is {stored} and a live line holds {highest}"
        );
    }
}
