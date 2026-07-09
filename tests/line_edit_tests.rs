use ast_editor::tools::session_db;
use ast_editor::tools::view;
use ast_editor::tools::edit;
use ast_editor::parser::ParserManager;
use std::fs;
use std::path::PathBuf;

struct TestFile {
    path: PathBuf,
}

impl TestFile {
    fn new(name: &str, content: &str) -> Self {
        let path = std::env::temp_dir().join(format!("ts_inspect_int_{}", name));
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

fn create_test_parser_manager() -> ParserManager {
    let tmp = std::env::temp_dir().join("line_edit_integration_tests");
    let cache_dir = tmp.join("cache");
    let compiler_path = tmp.join("compiler");
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let wasm_dir = manifest_dir.join("resources").join("wasm");
    
    let _ = fs::create_dir_all(&cache_dir);
    let _ = fs::create_dir_all(&compiler_path);
    
    ParserManager::with_paths(cache_dir, compiler_path, wasm_dir).unwrap()
}

fn acquire_db_lock() -> std::sync::MutexGuard<'static, ()> {
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

    let _meta = session_db::init_edit_session(file.path_str(), false).unwrap();
    
    // Retrieve lines (this triggers lazy hashing for range)
    let view_res = view::view_lines(file.path_str(), 1, 3).unwrap();
    let val: serde_json::Value = serde_json::from_str(&view_res).unwrap();
    let lines = val["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 3);
    let line0 = lines[0].as_array().unwrap();
    assert!(line0[1].as_str().unwrap().starts_with("1#"));
    assert_eq!(line0[2].as_str().unwrap(), "fn main() {");
    assert!(val["tip"].as_str().unwrap().contains("edit_lines"));
}

#[tokio::test]
async fn test_edit_operations_and_ast_validation() {
    let _lock = acquire_db_lock();
    let file = TestFile::new("edit.rs", "fn main() {\n    let a = 1;\n}\n");

    let pm = create_test_parser_manager();
    let meta = session_db::init_edit_session(file.path_str(), false).unwrap();
    session_db::ensure_hashes_for_range(&session_db::get_db_connection().unwrap(), &meta.session_id, 1, 3).unwrap();

    // Fetch the correct target ID for line 2
    let view_res = view::view_lines(file.path_str(), 2, 2).unwrap();
    let val: serde_json::Value = serde_json::from_str(&view_res).unwrap();
    let target_id = val["lines"][0].as_array().unwrap()[1].as_str().unwrap().to_string();

    // Perform invalid edit (Syntax error)
    let invalid_edits = vec![edit::LineEdit {
        op: "update".to_string(),
        target_id: Some(target_id.clone()),
        content: Some("let a = ;".to_string()), // missing value
        ..Default::default()
    }];
    let edit_res = edit::edit_lines(file.path_str(), invalid_edits, &pm).await;
    
    if let Err(ref e) = edit_res {
        println!("DEBUG: invalid edit error = {:?}", e);
    }
    
    assert!(edit_res.is_err());
    assert!(edit_res.unwrap_err().to_string().contains("Validation error"));

    // Verify file content didn't change (rolled back)
    let content = fs::read_to_string(file.path_str()).unwrap();
    assert!(content.contains("let a = 1;"));

    // Perform valid edit
    let valid_edits = vec![edit::LineEdit {
        op: "update".to_string(),
        target_id: Some(target_id),
        content: Some("    let a = 2;".to_string()),
        ..Default::default()
    }];
    let edit_res = edit::edit_lines(file.path_str(), valid_edits, &pm).await.unwrap();
    assert!(edit_res.contains("let a = 2;"));

    let content = fs::read_to_string(file.path_str()).unwrap();
    assert!(content.contains("let a = 2;"));
}

#[tokio::test]
async fn test_transactional_deletes_and_inserts() {
    let _lock = acquire_db_lock();
    let file = TestFile::new("trans_ops.rs", "fn main() {\n    let a = 1;\n    let b = 2;\n}\n");

    let pm = create_test_parser_manager();
    let meta = session_db::init_edit_session(file.path_str(), false).unwrap();
    session_db::ensure_hashes_for_range(&session_db::get_db_connection().unwrap(), &meta.session_id, 1, 4).unwrap();

    // Get Line IDs
    let view_res = view::view_lines(file.path_str(), 1, 4).unwrap();
    let val: serde_json::Value = serde_json::from_str(&view_res).unwrap();
    let lines_arr = val["lines"].as_array().unwrap();
    
    // Find line IDs for let a = 1 and let b = 2
    let mut id_a = String::new();
    let mut id_b = String::new();
    for line in lines_arr {
        let line_arr = line.as_array().unwrap();
        let code = line_arr[2].as_str().unwrap();
        if code.contains("let a = 1;") {
            id_a = line_arr[1].as_str().unwrap().to_string();
        } else if code.contains("let b = 2;") {
            id_b = line_arr[1].as_str().unwrap().to_string();
        }
    }
    assert!(!id_a.is_empty());
    assert!(!id_b.is_empty());

    // Transactional delete and insert_after
    let edits = vec![
        edit::LineEdit {
            op: "delete".to_string(),
            target_id: Some(id_b),
            content: None,
            ..Default::default()
        },
        edit::LineEdit {
            op: "insert_after".to_string(),
            target_id: Some(id_a),
            content: Some("    let c = 3;".to_string()),
            ..Default::default()
        }
    ];

    let edit_preview = edit::edit_lines(file.path_str(), edits, &pm).await.unwrap();
    
    // The final file should be:
    // fn main() {
    //     let a = 1;
    //     let c = 3;
    // }
    let content = fs::read_to_string(file.path_str()).unwrap();
    assert!(content.contains("let a = 1;"));
    assert!(content.contains("let c = 3;"));
    assert!(!content.contains("let b = 2;"));

    // Verify preview output contains the new/modified lines
    assert!(edit_preview.contains("let c = 3;"));
}

#[tokio::test]
async fn test_concurrency_error_out_of_sync_mtime() {
    let _lock = acquire_db_lock();
    let file = TestFile::new("concurrency.rs", "fn main() {\n    let a = 1;\n}\n");

    let pm = create_test_parser_manager();
    let meta = session_db::init_edit_session(file.path_str(), false).unwrap();
    session_db::ensure_hashes_for_range(&session_db::get_db_connection().unwrap(), &meta.session_id, 1, 3).unwrap();

    // Get line ID
    let view_res = view::view_lines(file.path_str(), 2, 2).unwrap();
    let val: serde_json::Value = serde_json::from_str(&view_res).unwrap();
    let target_id = val["lines"][0].as_array().unwrap()[1].as_str().unwrap().to_string();

    // Modify the file externally on disk, changing its mtime
    tokio::time::sleep(tokio::time::Duration::from_millis(1100)).await;
    fs::write(file.path_str(), "fn main() {\n    let a = 1;\n}\n// external change\n").unwrap();

    // Try applying line edits, should fail with CONCURRENCY_ERROR
    let edits = vec![edit::LineEdit {
        op: "update".to_string(),
        target_id: Some(target_id),
        content: Some("    let a = 3;".to_string()),
        ..Default::default()
    }];

    let edit_res = edit::edit_lines(file.path_str(), edits, &pm).await;
    assert!(edit_res.is_err());
    let err_msg = edit_res.unwrap_err().to_string();
    assert!(err_msg.contains("CONCURRENCY_ERROR"));
}

#[tokio::test]
async fn test_integration_append_operation() {
    let _lock = acquire_db_lock();
    let file = TestFile::new("append_integration.rs", "fn main() {\n    let a = 1;\n}\n");

    let pm = create_test_parser_manager();
    let _meta = session_db::init_edit_session(file.path_str(), false).unwrap();

    // Perform append
    let edits = vec![edit::LineEdit {
        op: "append".to_string(),
        target_id: None,
        content: Some("fn additional() {\n}".to_string()),
        ..Default::default()
    }];

    let preview = edit::edit_lines(file.path_str(), edits, &pm).await.unwrap();
    assert!(preview.contains("fn additional() {"));
    
    let content = fs::read_to_string(file.path_str()).unwrap();
    assert!(content.contains("fn main() {\n    let a = 1;\n}\nfn additional() {\n}\n"));
}

#[tokio::test]
async fn test_integration_advanced_operations() {
    let _lock = acquire_db_lock();
    let file = TestFile::new("advanced_int.rs", "fn main() {\n    let a = 1;\n    let b = 2;\n}\n");

    let pm = create_test_parser_manager();
    let _meta = session_db::init_edit_session(file.path_str(), false).unwrap();
    session_db::ensure_hashes_for_range(&session_db::get_db_connection().unwrap(), &_meta.session_id, 1, 4).unwrap();

    // 1. Get IDs for lines
    let view_res = view::view_lines(file.path_str(), 1, 4).unwrap();
    let val: serde_json::Value = serde_json::from_str(&view_res).unwrap();
    let lines_arr = val["lines"].as_array().unwrap();
    let _id_main = lines_arr[0].as_array().unwrap()[1].as_str().unwrap().to_string();
    let id_a = lines_arr[1].as_array().unwrap()[1].as_str().unwrap().to_string();
    let id_b = lines_arr[2].as_array().unwrap()[1].as_str().unwrap().to_string();

    // 2. Perform replace_range replacing let a = 1 and let b = 2 with let val = 100
    let edits = vec![edit::LineEdit {
        op: "replace_range".to_string(),
        target_id: Some(id_a.clone()),
        end_target_id: Some(id_b.clone()),
        content: Some("    let val = 100;".to_string()),
        ..Default::default()
    }];

    let preview = edit::edit_lines(file.path_str(), edits, &pm).await.unwrap();
    assert!(preview.contains("let val = 100;"));
    
    let content = fs::read_to_string(file.path_str()).unwrap();
    assert_eq!(content, "fn main() {\n    let val = 100;\n}\n");

    // Refresh session
    let _meta = session_db::init_edit_session(file.path_str(), false).unwrap();
    session_db::ensure_hashes_for_range(&session_db::get_db_connection().unwrap(), &_meta.session_id, 1, 3).unwrap();

    // Get new IDs
    let view_res = view::view_lines(file.path_str(), 1, 3).unwrap();
    let val: serde_json::Value = serde_json::from_str(&view_res).unwrap();
    let lines_arr = val["lines"].as_array().unwrap();
    let id_main_new = lines_arr[0].as_array().unwrap()[1].as_str().unwrap().to_string();
    let id_val_new = lines_arr[1].as_array().unwrap()[1].as_str().unwrap().to_string();

    // 3. Move let val = 100; before fn main() {
    let edits_move = vec![edit::LineEdit {
        op: "move".to_string(),
        target_id: Some(id_val_new),
        dest_target_id: Some(id_main_new),
        move_position: Some("before".to_string()),
        ..Default::default()
    }];

    let preview_move = edit::edit_lines(file.path_str(), edits_move, &pm).await.unwrap();
    assert!(preview_move.contains("let val = 100;"));

    let content_move = fs::read_to_string(file.path_str()).unwrap();
    assert_eq!(content_move, "    let val = 100;\nfn main() {\n}\n");
}
