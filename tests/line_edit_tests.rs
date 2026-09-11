#![allow(clippy::await_holding_lock)]
use ast_editor::parser::ParserManager;
use ast_editor::tools::edit;
use ast_editor::tools::session_db;
use ast_editor::tools::session_db::{
    EditOp, MovePosition, SessionRepository, SqliteSessionRepository,
};
use ast_editor::tools::view;
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
    repository: &impl session_db::SessionRepository,
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
        for id_entry in ids_val["ids"].as_array().unwrap() {
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

/// Point this test binary's store at a directory of its own. Integration
/// tests link the library built without cfg(test), and the two test binaries
/// run as separate processes that no in-process lock can serialise.
fn isolate_store() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("ast-editor-test-{}", std::process::id()));
        std::env::set_var("AST_EDITOR_CACHE_DIR", dir);
    });
}

fn acquire_db_lock() -> std::sync::MutexGuard<'static, ()> {
    isolate_store();
    match ast_editor::tools::TEST_DB_LOCK.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[tokio::test]
async fn test_sqlite_session_lifecycle() {
    let _lock = acquire_db_lock();
    let file = TestFile::new("lifecycle.py", "def foo():\n    print('bar')\n");

    let meta = session_db::init_edit_session(file.path_str(), false).unwrap();
    assert_eq!(meta.total_lines, 2);
    assert!(meta.is_supported);

    // Test session reuse
    let meta_reused = session_db::init_edit_session(file.path_str(), false).unwrap();
    assert_eq!(meta.session_id, meta_reused.session_id);
}

#[tokio::test]
async fn test_view_lines_lazy_hashing() {
    let _lock = acquire_db_lock();
    let file = TestFile::new("lazy.rs", "fn main() {\n    println!(\"hello\");\n}\n");
    let repository = SqliteSessionRepository;

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
    let repository = SqliteSessionRepository;

    let pm = create_test_parser_manager();

    // Fetch the correct target ID for line 2
    let view_res = view_range(&repository, file.path_str(), 2, 2, None).unwrap();
    let val: serde_json::Value = serde_json::from_str(&view_res).unwrap();
    let target_id = val["lines"][0].as_array().unwrap()[0]
        .as_str()
        .unwrap()
        .to_string();

    // Perform invalid edit (Syntax error)
    let invalid_edits = vec![edit::LineEdit {
        op: EditOp::Replace,
        target_id: Some(target_id.clone()),
        content: Some("let a = ;".to_string()), // missing value
        ..Default::default()
    }];
    // 1. Permissive Mode test: should save with errors
    let edit_res_permissive =
        edit::edit_lines(&repository, file.path_str(), invalid_edits.clone(), &pm)
            .await
            .unwrap();
    let res_permissive: serde_json::Value = serde_json::from_str(&edit_res_permissive).unwrap();
    assert_eq!(res_permissive["status"], "saved_with_errors");
    assert_eq!(res_permissive["syntax_valid"], false);

    let content_permissive = fs::read_to_string(file.path_str()).unwrap();
    assert!(content_permissive.contains("let a = ;"));

    // Reset file for strict test
    fs::write(file.path_str(), "fn main() {\n    let a = 1;\n}\n").unwrap();
    // Re-initialize session to clear the dirty session
    let _init = session_db::init_edit_session(file.path_str(), false).unwrap();
    let view_res_strict = view_range(&repository, file.path_str(), 2, 2, None).unwrap();
    let val_strict: serde_json::Value = serde_json::from_str(&view_res_strict).unwrap();
    let target_id_strict = val_strict["lines"][0].as_array().unwrap()[0]
        .as_str()
        .unwrap()
        .to_string();

    let invalid_edits_strict = vec![edit::LineEdit {
        op: EditOp::Replace,
        target_id: Some(target_id_strict),
        content: Some("let a = ;".to_string()),
        ..Default::default()
    }];

    // 2. Strict Mode test: should roll back
    let edit_res = edit::edit_lines_with_validation(
        &repository,
        file.path_str(),
        invalid_edits_strict,
        true,
        &pm,
    )
    .await;

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
    let target_id = val["lines"][0].as_array().unwrap()[0]
        .as_str()
        .unwrap()
        .to_string();

    let valid_edits = vec![edit::LineEdit {
        op: EditOp::Replace,
        target_id: Some(target_id),
        content: Some("    let a = 2;".to_string()),
        ..Default::default()
    }];
    let edit_res = edit::edit_lines(&repository, file.path_str(), valid_edits, &pm)
        .await
        .unwrap();
    let res: serde_json::Value = serde_json::from_str(&edit_res).unwrap();
    assert!(res["status"].is_null(), "success is the exit code: {}", res);
    assert!(!res["modified_ids"].as_array().unwrap().is_empty());

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
    let repository = SqliteSessionRepository;

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
        edit::LineEdit {
            op: EditOp::Delete,
            target_id: Some(id_b),
            content: None,
            ..Default::default()
        },
        edit::LineEdit {
            op: EditOp::InsertAfter,
            target_id: Some(id_a),
            content: Some("    let c = 3;".to_string()),
            ..Default::default()
        },
    ];

    let edit_preview = edit::edit_lines(&repository, file.path_str(), edits, &pm)
        .await
        .unwrap();
    let res: serde_json::Value = serde_json::from_str(&edit_preview).unwrap();
    assert!(res["status"].is_null(), "success is the exit code: {}", res);
    assert!(!res["modified_ids"].as_array().unwrap().is_empty());

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
    let repository = SqliteSessionRepository;

    let pm = create_test_parser_manager();

    // Get line ID
    let view_res = view_range(&repository, file.path_str(), 2, 2, None).unwrap();
    let val: serde_json::Value = serde_json::from_str(&view_res).unwrap();
    let target_id = val["lines"][0].as_array().unwrap()[0]
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
    let edits = vec![edit::LineEdit {
        op: EditOp::Replace,
        target_id: Some(target_id),
        content: Some("    let a = 3;".to_string()),
        ..Default::default()
    }];

    let edit_res = edit::edit_lines(&repository, file.path_str(), edits, &pm)
        .await
        .unwrap();
    assert!(edit_res.contains("modified_ids"), "{}", edit_res);

    // Verify disk content includes both the external change and our update
    let content = fs::read_to_string(file.path_str()).unwrap();
    assert!(content.contains("let a = 3;"));
    assert!(content.contains("external change"));
}

#[tokio::test]
async fn test_integration_append_operation() {
    let _lock = acquire_db_lock();
    let file = TestFile::new("append_integration.rs", "fn main() {\n    let a = 1;\n}\n");
    let repository = SqliteSessionRepository;

    let pm = create_test_parser_manager();

    // Perform append
    let edits = vec![edit::LineEdit {
        op: EditOp::Append,
        target_id: None,
        content: Some("fn additional() {\n}".to_string()),
        ..Default::default()
    }];

    let preview = edit::edit_lines(&repository, file.path_str(), edits, &pm)
        .await
        .unwrap();
    let res: serde_json::Value = serde_json::from_str(&preview).unwrap();
    assert!(res["status"].is_null(), "success is the exit code: {}", res);
    assert!(!res["modified_ids"].as_array().unwrap().is_empty());

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
    let repository = SqliteSessionRepository;

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
    let edits = vec![edit::LineEdit {
        op: EditOp::ReplaceRange,
        target_id: Some(id_a.clone()),
        end_target_id: Some(id_b.clone()),
        content: Some("    let val = 100;".to_string()),
        ..Default::default()
    }];

    let preview = edit::edit_lines(&repository, file.path_str(), edits, &pm)
        .await
        .unwrap();
    let res: serde_json::Value = serde_json::from_str(&preview).unwrap();
    assert!(res["status"].is_null(), "success is the exit code: {}", res);
    assert!(!res["modified_ids"].as_array().unwrap().is_empty());

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
    let edits_move = vec![edit::LineEdit {
        op: EditOp::Move,
        target_id: Some(id_val_new),
        dest_target_id: Some(id_main_new),
        move_position: Some(MovePosition::Before),
        ..Default::default()
    }];

    let preview_move = edit::edit_lines(&repository, file.path_str(), edits_move, &pm)
        .await
        .unwrap();
    let res: serde_json::Value = serde_json::from_str(&preview_move).unwrap();
    assert!(res["status"].is_null(), "success is the exit code: {}", res);
    assert!(!res["modified_ids"].as_array().unwrap().is_empty());

    let content_move = fs::read_to_string(file.path_str()).unwrap();
    assert_eq!(content_move, "    let val = 100;\nfn main() {\n}\n");
}

#[tokio::test]
async fn test_integration_insert_without_target_id() {
    let _lock = acquire_db_lock();
    let file = TestFile::new(
        "insert_jit_no_target.rs",
        "fn main() {\n    let a = 1;\n}\n",
    );
    let repository = SqliteSessionRepository;

    let pm = create_test_parser_manager();

    // 1. Perform insert_before with target_id = None (should prepend to the beginning of the file)
    let edits_before = vec![edit::LineEdit {
        op: EditOp::InsertBefore,
        target_id: None,
        content: Some("// Prepend header".to_string()),
        ..Default::default()
    }];

    let preview1 = edit::edit_lines(&repository, file.path_str(), edits_before, &pm)
        .await
        .unwrap();
    let res: serde_json::Value = serde_json::from_str(&preview1).unwrap();
    assert!(res["status"].is_null(), "success is the exit code: {}", res);
    assert!(!res["modified_ids"].as_array().unwrap().is_empty());

    let content1 = fs::read_to_string(file.path_str()).unwrap();
    assert!(
        content1.starts_with("// Prepend header\nfn main() {"),
        "content1 was: {:?}",
        content1
    );

    // 2. Perform insert_after with target_id = Some("") (should append to the end of the file)
    let edits_after = vec![edit::LineEdit {
        op: EditOp::InsertAfter,
        target_id: Some("".to_string()),
        content: Some("// Append footer".to_string()),
        ..Default::default()
    }];

    let preview2 = edit::edit_lines(&repository, file.path_str(), edits_after, &pm)
        .await
        .unwrap();
    let res: serde_json::Value = serde_json::from_str(&preview2).unwrap();
    assert!(res["status"].is_null(), "success is the exit code: {}", res);
    assert!(!res["modified_ids"].as_array().unwrap().is_empty());

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
    let repository = SqliteSessionRepository;

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
        view::create_lines(&repository, &filepath_str, initial_content, Some(true)).unwrap();
    let val: serde_json::Value = serde_json::from_str(&create_res).unwrap();

    assert!(val["status"].is_null(), "success is the exit code: {}", val);
    assert_eq!(val["total_lines"].as_u64().unwrap(), 3);
    assert!(val["total_bytes"].as_u64().is_some());
    let ids = val["ids"].as_array().unwrap();
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
    let dup_res = view::create_lines(&repository, &filepath_str, "different content", None);
    assert!(dup_res.is_err());
    let dup_err = dup_res.unwrap_err().to_string();
    assert!(dup_err.contains("FILE_ALREADY_EXISTS"));

    // 4. Modify the newly created file using `edit` and verify the modified contents on disk
    let edits = vec![edit::LineEdit {
        op: EditOp::Replace,
        target_id: Some(id1.to_string()),
        content: Some("    let x = 100;".to_string()),
        ..Default::default()
    }];

    let edit_res = edit::edit_lines(&repository, &filepath_str, edits, &pm)
        .await
        .unwrap();
    let res: serde_json::Value = serde_json::from_str(&edit_res).unwrap();
    assert!(res["status"].is_null(), "success is the exit code: {}", res);
    assert!(!res["modified_ids"].as_array().unwrap().is_empty());

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
    let repository = SqliteSessionRepository;

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
    let edits = vec![edit::LineEdit {
        op: EditOp::ReplaceSubstring,
        target_id: Some(line1_id.to_string()),
        pattern: Some("aaaaa".to_string()),
        replacement: Some("bbbbb".to_string()),
        occurrence: Some(1),
        ..Default::default()
    }];
    let edit_res = edit::edit_lines(&repository, file.path_str(), edits, &pm)
        .await
        .unwrap();
    assert!(edit_res.contains("modified_ids"), "{}", edit_res);

    let disk_content = std::fs::read_to_string(file.path_str()).unwrap();
    assert!(disk_content.contains("bbbbbaaaaa"));
}

#[tokio::test]
async fn test_integration_view_lines_capacity_cap() {
    let _lock = acquire_db_lock();
    let repository = SqliteSessionRepository;

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
    let repository = SqliteSessionRepository;

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
        view::create_lines(&repository, &filepath_str, initial_content, Some(false)).unwrap();
    let val: serde_json::Value = serde_json::from_str(&create_res).unwrap();

    assert!(val["status"].is_null(), "success is the exit code: {}", val);
    assert_eq!(val["total_lines"].as_u64().unwrap(), 3);
    assert!(val["total_bytes"].as_u64().is_some());
    assert!(val["ids"].is_null());

    let _ = fs::remove_file(&temp_file_path);
}

#[tokio::test]
async fn test_integration_view_lines_only_ids() {
    let _lock = acquire_db_lock();
    let repository = SqliteSessionRepository;
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
    let repository = SqliteSessionRepository;
    let pm = create_test_parser_manager();

    let before_view = view_range(&repository, file.path_str(), 1, 3, None).unwrap();
    let val: serde_json::Value = serde_json::from_str(&before_view).unwrap();
    let target_id = val["lines"][1].as_array().unwrap()[0]
        .as_str()
        .unwrap()
        .to_string();
    let before_mtime = fs::metadata(file.path_str()).unwrap().modified().unwrap();

    let valid_edits = vec![edit::LineEdit {
        op: EditOp::Replace,
        target_id: Some(target_id.clone()),
        content: Some("    let a = 2;".to_string()),
        ..Default::default()
    }];

    // 1. A valid batch previews the diff it would produce, and mints no IDs.
    let res = edit::edit_lines_dry_run(&repository, file.path_str(), valid_edits.clone(), &pm)
        .await
        .unwrap();
    let preview: serde_json::Value = serde_json::from_str(&res.report).unwrap();
    assert_eq!(preview["syntax_valid"], true);
    assert!(preview["modified_ids"].is_null());
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

    // 3. Nothing on disk or in the session changed.
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
    let broken_edits = vec![edit::LineEdit {
        op: EditOp::Replace,
        target_id: Some(target_id),
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
    let repository = SqliteSessionRepository;
    let pm = create_test_parser_manager();

    let view = view_range(&repository, file.path_str(), 3, 3, None).unwrap();
    let val: serde_json::Value = serde_json::from_str(&view).unwrap();
    let target_id = val["lines"][0].as_array().unwrap()[0]
        .as_str()
        .unwrap()
        .to_string();

    let edits = vec![edit::LineEdit {
        op: EditOp::Replace,
        target_id: Some(target_id),
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
    let repository = SqliteSessionRepository;

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
    let repository = SqliteSessionRepository;
    let pm = create_test_parser_manager();

    let view = view_range(&repository, file.path_str(), 1, 3, None).unwrap();
    let val: serde_json::Value = serde_json::from_str(&view).unwrap();
    let target_id = val["lines"][1].as_array().unwrap()[0]
        .as_str()
        .unwrap()
        .to_string();

    let edits = vec![edit::LineEdit {
        op: EditOp::Replace,
        target_id: Some(target_id),
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

    let res = edit::apply_preview(&repository, file.path_str(), &preview_id, false, &pm)
        .await
        .unwrap();
    let applied: serde_json::Value = serde_json::from_str(&res).unwrap();
    assert!(
        applied["status"].is_null(),
        "success is the exit code: {}",
        applied
    );
    assert!(!applied["modified_ids"].as_array().unwrap().is_empty());
    assert_eq!(
        fs::read_to_string(file.path_str()).unwrap(),
        "fn main() {\n    let a = 2;\n}\n"
    );

    // A preview id is single-use.
    let err = edit::apply_preview(&repository, file.path_str(), &preview_id, false, &pm)
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
    let repository = SqliteSessionRepository;
    let pm = create_test_parser_manager();

    let view = view_range(&repository, file.path_str(), 1, 3, None).unwrap();
    let val: serde_json::Value = serde_json::from_str(&view).unwrap();
    let target_id = val["lines"][1].as_array().unwrap()[0]
        .as_str()
        .unwrap()
        .to_string();

    let edits = vec![edit::LineEdit {
        op: EditOp::Replace,
        target_id: Some(target_id),
        content: Some("    let a = 2;".to_string()),
        ..Default::default()
    }];

    let res = edit::edit_lines_dry_run(&repository, file.path_str(), edits, &pm)
        .await
        .unwrap();
    let preview: serde_json::Value = serde_json::from_str(&res.report).unwrap();
    let preview_id = preview["preview_id"].as_str().unwrap().to_string();

    // Addressed at the wrong file.
    let err = edit::apply_preview(&repository, other.path_str(), &preview_id, false, &pm)
        .await
        .unwrap_err();
    assert!(format!("{}", err).contains("belongs to"), "{}", err);

    // The file moves under the preview.
    fs::write(file.path_str(), "fn main() {\n    let a = 99;\n}\n").unwrap();
    let err = edit::apply_preview(&repository, file.path_str(), &preview_id, false, &pm)
        .await
        .unwrap_err();
    assert!(format!("{}", err).contains("PREVIEW_STALE"), "{}", err);
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
    let repository = SqliteSessionRepository;
    let pm = create_test_parser_manager();

    let view = view_range(&repository, file.path_str(), 1, 3, None).unwrap();
    let val: serde_json::Value = serde_json::from_str(&view).unwrap();
    let target_id = val["lines"][1].as_array().unwrap()[0]
        .as_str()
        .unwrap()
        .to_string();

    let edits = vec![edit::LineEdit {
        op: EditOp::Replace,
        target_id: Some(target_id),
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
async fn test_store_holds_no_file_text() {
    let _lock = acquire_db_lock();
    let secret = "let api_key = \"correct-horse-battery-staple\";";
    let file = TestFile::new("no_text.rs", &format!("fn main() {{\n    {}\n}}\n", secret));
    let repository = SqliteSessionRepository;
    let pm = create_test_parser_manager();

    // Touch every path that populates the index.
    let view = view_range(&repository, file.path_str(), 1, 3, None).unwrap();
    let val: serde_json::Value = serde_json::from_str(&view).unwrap();
    let target_id = val["lines"][1].as_array().unwrap()[0]
        .as_str()
        .unwrap()
        .to_string();
    edit::edit_lines(
        &repository,
        file.path_str(),
        vec![edit::LineEdit {
            op: EditOp::Replace,
            target_id: Some(target_id),
            content: Some(format!("    {}", secret)),
            ..Default::default()
        }],
        &pm,
    )
    .await
    .unwrap();

    // Invariant 9: the index is an index, not a copy. Nothing in the database
    // file should contain the text of the lines it tracks.
    let db = session_db::get_db_path().unwrap();
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
fn index_pairs(repository: &SqliteSessionRepository, path: &str) -> Vec<(i64, String)> {
    let meta = session_db::init_edit_session(path, false).unwrap();
    repository
        .fetch_lines_range(&meta.session_id, 1, meta.total_lines)
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
    let repository = SqliteSessionRepository;

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
    let repository = SqliteSessionRepository;

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
fn live_id(repository: &SqliteSessionRepository, path: &str, line: usize) -> String {
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
    let repository = SqliteSessionRepository;
    let pm = create_test_parser_manager();

    // An untargeted line changes underneath. The edit should still land.
    let target_id = live_id(&repository, file.path_str(), 2);
    fs::write(
        file.path_str(),
        "fn main() {\n    let a = 1;\n    let b = 99;\n}\n",
    )
    .unwrap();
    let res = edit::edit_lines(
        &repository,
        file.path_str(),
        vec![edit::LineEdit {
            op: EditOp::Replace,
            target_id: Some(target_id),
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
    let target_id = live_id(&repository, file.path_str(), 2);
    fs::write(
        file.path_str(),
        "fn main() {\n    let a = 123;\n    let b = 99;\n}\n",
    )
    .unwrap();
    let err = edit::edit_lines(
        &repository,
        file.path_str(),
        vec![edit::LineEdit {
            op: EditOp::Replace,
            target_id: Some(target_id),
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
    let repository = SqliteSessionRepository;

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
    let file = TestFile::new("no_reuse.rs", "fn main() {\n    let a = 1;\n}\n");
    let repository = SqliteSessionRepository;
    let pm = create_test_parser_manager();

    // Delete the last line of the file, retiring the highest id in use.
    let doomed = live_id(&repository, file.path_str(), 3);
    edit::edit_lines(
        &repository,
        file.path_str(),
        vec![edit::LineEdit {
            op: EditOp::Delete,
            target_id: Some(doomed.clone()),
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
        vec![edit::LineEdit {
            op: EditOp::Append,
            content: Some("}".to_string()),
            ..Default::default()
        }],
        &pm,
    )
    .await
    .unwrap();
    let val: serde_json::Value = serde_json::from_str(&res).unwrap();
    let minted = val["modified_ids"][0][0].as_str().unwrap();
    let minted_seq = i64::from_str_radix(minted.split('#').next().unwrap(), 16).unwrap();
    assert_ne!(minted_seq, retired, "id {} was handed out twice", retired);
}

#[tokio::test]
async fn test_repeated_insertion_between_the_same_pair() {
    let _lock = acquire_db_lock();
    let file = TestFile::new("endurance.rs", "fn main() {\n}\n");
    let repository = SqliteSessionRepository;
    let pm = create_test_parser_manager();

    // Invariant 7: ordering does not degrade under repeated insertion at one
    // point, and no id is handed out twice.
    let mut seen = std::collections::HashSet::new();
    for n in 0..200 {
        let anchor = live_id(&repository, file.path_str(), 1);
        let res = edit::edit_lines(
            &repository,
            file.path_str(),
            vec![edit::LineEdit {
                op: EditOp::InsertAfter,
                target_id: Some(anchor),
                content: Some(format!("    let v{} = {};", n, n)),
                ..Default::default()
            }],
            &pm,
        )
        .await
        .unwrap();
        let val: serde_json::Value = serde_json::from_str(&res).unwrap();
        let minted = val["modified_ids"][0][0]
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
    let repository = SqliteSessionRepository;
    let pm = create_test_parser_manager();

    // Push the ids out of step with the positions, so that renumbering the
    // file 1..N would give a visibly different answer from reconciling it.
    let anchor = live_id(&repository, file.path_str(), 1);
    edit::edit_lines(
        &repository,
        file.path_str(),
        vec![edit::LineEdit {
            op: EditOp::InsertAfter,
            target_id: Some(anchor),
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
    let meta = session_db::init_edit_session(file.path_str(), false).unwrap();
    let db = rusqlite::Connection::open(session_db::get_db_path().unwrap()).unwrap();
    db.execute(
        "DELETE FROM lines WHERE session_id = ?1 AND sequence_id = (SELECT MAX(sequence_id) FROM lines WHERE session_id = ?1)",
        [&meta.session_id],
    ).unwrap();
    drop(db);

    // Read a fixed range rather than the session's line count: the point is
    // what the buffer recovers, not what the stale count claims.
    let meta = session_db::init_edit_session(file.path_str(), false).unwrap();
    let after: Vec<(i64, String)> = repository
        .fetch_lines_range(&meta.session_id, 1, 4)
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
    let res = ast_editor::tools::inspect::run_inspect(
        ast_editor::tools::inspect::InspectArgs {
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

    let text = ast_editor::tools::outline::run_outline(
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
    let sexp = ast_editor::tools::outline::run_outline(
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
    let err = ast_editor::tools::inspect::run_inspect(
        ast_editor::tools::inspect::InspectArgs {
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
    let repository = SqliteSessionRepository;
    let pm = create_test_parser_manager();

    let res = inspect_report(file.path_str(), Some("functions")).await;

    let doomed = res["matches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["text"].as_str().unwrap().contains("doomed"))
        .expect("the function was not matched");

    // The ids the query returned go straight to edit_lines, with no call in
    // between to turn line numbers into ids.
    let out = edit::edit_lines(
        &repository,
        file.path_str(),
        vec![edit::LineEdit {
            op: EditOp::ReplaceRange,
            target_id: Some(doomed["start_id"].as_str().unwrap().to_string()),
            end_target_id: Some(doomed["end_id"].as_str().unwrap().to_string()),
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
