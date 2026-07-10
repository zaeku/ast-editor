use anyhow::{Result, Context, bail};
use std::fs;
pub use crate::tools::session_db::LineEdit;
use crate::tools::session_db::{
    compute_line_hash, parse_line_id, SessionRepository,
};

fn check_language_supported(path: &str) -> bool {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "py" | "js" | "jsx" | "ts" | "tsx" | "go" | "rs" | "java" |
        "cpp" | "cc" | "cxx" | "c" | "h" | "lua" | "html" | "htm" |
        "json" | "yaml" | "yml" | "toml" | "swift"
    )
}

async fn validate_syntax(filepath: &str, content: &str, parser_manager: &crate::parser::ParserManager) -> Result<()> {
    if !check_language_supported(filepath) {
        return Ok(());
    }

    let ext = std::path::Path::new(filepath)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    let (tree, _language) = parser_manager.parse_code(&ext, content).await
        .context("Failed to parse code for syntax validation")?;

    let ast = tree.root_node().to_sexp();
    
    // Check if AST has ERROR or MISSING nodes
    if ast.contains("ERROR") || ast.contains("MISSING") {
        bail!("Validation error: Syntactical errors detected in code after edits. Compilation/AST verification aborted.\n{}", ast);
    }
    
    Ok(())
}

pub async fn edit_lines(
    repository: &impl SessionRepository,
    filepath: &str,
    edits: Vec<LineEdit>,
    parser_manager: &crate::parser::ParserManager,
) -> Result<String> {
    let path = std::path::Path::new(filepath);
    
    // Out-of-Sync Check
    if let Some(old_mtime) = repository.get_session_mtime(filepath)? {
        let current_mtime = path.metadata()?.modified()?
            .duration_since(std::time::UNIX_EPOCH)?.as_secs() as i64;
        if current_mtime != old_mtime {
            bail!("CONCURRENCY_ERROR: File has been modified externally. Re-initialize the session.");
        }
    }

    let meta = repository.init_session(filepath, false)?;
    let session_id = meta.session_id;

    // Long Line Edit Protection Check
    for edit in &edits {
        let target_ids_to_check = vec![
            edit.target_id.as_ref(),
            edit.end_target_id.as_ref(),
            edit.dest_target_id.as_ref(),
        ];
        for target_id in target_ids_to_check.into_iter().flatten() {
            if !target_id.is_empty() {
                let ends_with_trunc = target_id.ends_with("#TRUNC");
                let (seq, _) = parse_line_id(target_id)?;
                if let Some(content) = repository.get_line_content(&session_id, seq)? {
                    let char_count = content.chars().count();
                    if ends_with_trunc || char_count > 2048 {
                        bail!(
                            "LINE_TOO_LONG_ERROR: Line is too long ({} chars) and has been truncated in the view. Surgical updates on truncated lines are disabled to prevent accidental data loss. Please format the file using a code beautifier (e.g. prettier, black, or cargo fmt) to break it into multiple lines, or rewrite the file using create_lines/write_to_file.",
                            char_count
                        );
                    }
                }
            }
        }
    }

    // Make a backup of the current lines in case syntax validation fails
    let total_lines = repository.get_total_lines(&session_id)?;
    let backup_lines = repository.fetch_lines_range(&session_id, 1, total_lines)?;

    // Apply the edits using the repository
    let newly_modified_ids = repository.apply_line_edits(filepath, &session_id, &edits)?;

    // Reconstruct the final content
    let new_total_lines = repository.get_total_lines(&session_id)?;
    let new_lines = repository.fetch_lines_range(&session_id, 1, new_total_lines)?;
    let final_content = new_lines.iter().map(|(_, _, content)| content.as_str()).collect::<Vec<_>>().join("\n") + "\n";

    // Validate syntax
    if let Err(err) = validate_syntax(filepath, &final_content, parser_manager).await {
        // Rollback: restore backup lines
        repository.restore_session_lines(&session_id, &backup_lines)?;
        return Err(err);
    }

    // Save to disk
    fs::write(filepath, &final_content)?;
    
    // Update session timestamp and file hash/mtime
    use sha2::{Sha256, Digest};
    let mut hasher = Sha256::new();
    hasher.update(final_content.as_bytes());
    let new_file_hash = format!("{:x}", hasher.finalize());
    let new_mtime = path.metadata()?.modified()?
        .duration_since(std::time::UNIX_EPOCH)?.as_secs() as i64;
    repository.update_session_metadata(&session_id, &new_file_hash, new_mtime)?;

    // Generate output preview
    let mut items = Vec::new();
    let mut indices_to_show = std::collections::BTreeSet::new();
    for (line_idx, (seq, hash_opt, content)) in new_lines.iter().enumerate() {
        let hash = hash_opt.clone().unwrap_or_else(|| compute_line_hash(content));
        let line_id = format!("{:x}#{}", seq, hash);
        if newly_modified_ids.contains(&line_id) {
            let start = line_idx.saturating_sub(2);
            let end = std::cmp::min(line_idx + 2, new_lines.len().saturating_sub(1));
            for i in start..=end {
                indices_to_show.insert(i);
            }
        }
    }

    // Default to last 5 lines if no modification ids were gathered (e.g. all deletions)
    if indices_to_show.is_empty() {
        let start = new_lines.len().saturating_sub(5);
        for i in start..new_lines.len() {
            indices_to_show.insert(i);
        }
    }

    for idx in indices_to_show {
        let (seq, hash_opt, content) = &new_lines[idx];
        let hash = hash_opt.clone().unwrap_or_else(|| compute_line_hash(content));
        let line_id = format!("{:x}#{}", seq, hash);
        items.push(serde_json::json!([
            line_id,
            idx + 1,
            content
        ]));
    }

    let result_val = serde_json::json!({
        "columns": ["id", "n", "content"],
        "lines": items,
        "tip": "Edit these lines by calling 'edit_lines' with the line IDs (e.g. 1a#f8c9) shown above."
    });

    let output = serde_json::to_string_pretty(&result_val)?;
    Ok(output)
}

#[deprecated(since = "0.1.0", note = "use edit_lines instead")]
pub async fn apply_line_edits(
    repository: &impl SessionRepository,
    filepath: &str,
    edits: Vec<LineEdit>,
    parser_manager: &crate::parser::ParserManager,
) -> Result<String> {
    edit_lines(repository, filepath, edits, parser_manager).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::session_db::{init_edit_session, SqliteSessionRepository};
    use crate::parser::ParserManager;
    use crate::tools::TEST_DB_LOCK as DB_LOCK;

    // Helper to setup mock language config and raw wasm files in temporary wasm directory
    struct TestEnvironment {
        dir: std::path::PathBuf,
        pm: ParserManager,
    }

    impl TestEnvironment {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("tree_sitter_edit_tests_{}", name));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();

            let cache_dir = dir.join("cache");
            let compiler_path = dir.join("compiler");
            let wasm_dir = dir.join("wasm");
            fs::create_dir_all(&cache_dir).unwrap();
            fs::create_dir_all(&wasm_dir).unwrap();

            let langs_json_path = wasm_dir.join("languages.json");
            let mock_config = serde_json::json!({
                "rust": {
                    "extensions": [".rs"],
                    "wasm_file": "tree-sitter-rust.wasm"
                }
            });
            fs::write(&langs_json_path, serde_json::to_string(&mock_config).unwrap()).unwrap();

            // Copy real rust wasm so parsing/compilation succeeds
            let manifest_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
            let real_wasm_path = manifest_dir.join("resources").join("wasm").join("tree-sitter-rust.wasm");
            let target_wasm_path = wasm_dir.join("tree-sitter-rust.wasm");
            if real_wasm_path.exists() {
                fs::copy(&real_wasm_path, &target_wasm_path).unwrap();
            }

            let pm = ParserManager::with_paths(cache_dir, compiler_path, wasm_dir).unwrap();
            Self { dir, pm }
        }
    }

    impl Drop for TestEnvironment {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn find_line_id(lines_json: &str, pattern: &str) -> String {
        let val: serde_json::Value = serde_json::from_str(lines_json).unwrap();
        let lines = val["lines"].as_array().unwrap();
        for line in lines {
            let line_arr = line.as_array().unwrap();
            let code = line_arr[2].as_str().unwrap();
            if code.contains(pattern) {
                return line_arr[0].as_str().unwrap().to_string();
            }
        }
        String::new()
    }

    #[tokio::test]
    async fn test_apply_line_edits_insert_update_delete() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let env = TestEnvironment::new("basic_ops");
        let repository = SqliteSessionRepository;

        let file_path = env.dir.join("code.rs");
        fs::write(&file_path, "fn main() {\n    let a = 1;\n}\n")?;
        let filepath_str = file_path.to_str().unwrap();

        let metadata = init_edit_session(filepath_str, false)?;
        assert_eq!(metadata.total_lines, 3);

        // Get the line IDs by viewing
        let lines_view = crate::tools::view::view_lines(&repository, filepath_str, 1, 3, None)?;
        let target_id = find_line_id(&lines_view, "let a = 1;");
        assert!(!target_id.is_empty());

        // 1. Test update
        let edits = vec![
            LineEdit {
                op: "update".to_string(),
                target_id: Some(target_id.clone()),
                content: Some("    let a = 42;".to_string()),
                ..Default::default()
            }
        ];
        let preview = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
        assert!(preview.contains("let a = 42;"));
        assert_eq!(fs::read_to_string(&file_path)?, "fn main() {\n    let a = 42;\n}\n");

        // Refresh metadata/session
        let _metadata = init_edit_session(filepath_str, false)?;
        let lines_view = crate::tools::view::view_lines(&repository, filepath_str, 1, 3, None)?;
        let new_target_id = find_line_id(&lines_view, "let a = 42;");
        assert!(!new_target_id.is_empty());

        // 2. Test insert_after
        let edits = vec![
            LineEdit {
                op: "insert_after".to_string(),
                target_id: Some(new_target_id.clone()),
                content: Some("    let b = 2;".to_string()),
                ..Default::default()
            }
        ];
        let preview = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
        assert!(preview.contains("let b = 2;"));
        assert_eq!(fs::read_to_string(&file_path)?, "fn main() {\n    let a = 42;\n    let b = 2;\n}\n");

        // Refresh session to get latest target IDs
        let _ = init_edit_session(filepath_str, false)?;
        let lines_view = crate::tools::view::view_lines(&repository, filepath_str, 1, 4, None)?;
        let b_id = find_line_id(&lines_view, "let b = 2;");
        assert!(!b_id.is_empty());

        // 3. Test delete
        let edits = vec![
            LineEdit {
                op: "delete".to_string(),
                target_id: Some(b_id.clone()),
                content: None,
                ..Default::default()
            }
        ];
        let _preview = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
        assert_eq!(fs::read_to_string(&file_path)?, "fn main() {\n    let a = 42;\n}\n");

        Ok(())
    }

    #[tokio::test]
    async fn test_concurrency_error() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let env = TestEnvironment::new("concurrency");
        let repository = SqliteSessionRepository;

        let file_path = env.dir.join("code.rs");
        fs::write(&file_path, "fn main() {}\n")?;
        let filepath_str = file_path.to_str().unwrap();

        let _metadata = init_edit_session(filepath_str, false)?;

        // Modify file externally on disk, changing its mtime
        tokio::time::sleep(tokio::time::Duration::from_millis(1100)).await;
        fs::write(&file_path, "fn main() {\n    // changed externally\n}\n")?;

        let edits = vec![
            LineEdit {
                op: "insert_after".to_string(),
                target_id: None,
                content: Some("// fail".to_string()),
                ..Default::default()
            }
        ];

        let res = edit_lines(&repository, filepath_str, edits, &env.pm).await;
        assert!(res.is_err());
        let err_msg = res.unwrap_err().to_string();
        assert!(err_msg.contains("CONCURRENCY_ERROR"), "Expected concurrency error, got: {}", err_msg);

        Ok(())
    }

    #[tokio::test]
    async fn test_checksum_error() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let env = TestEnvironment::new("checksum");
        let repository = SqliteSessionRepository;

        let file_path = env.dir.join("code.rs");
        fs::write(&file_path, "fn main() {}\n")?;
        let filepath_str = file_path.to_str().unwrap();

        let _metadata = init_edit_session(filepath_str, false)?;

        let edits = vec![
            LineEdit {
                op: "update".to_string(),
                target_id: Some("1#9999".to_string()), // Invalid hash prefix
                content: Some("fn main() { // updated }".to_string()),
                ..Default::default()
            }
        ];

        let res = edit_lines(&repository, filepath_str, edits, &env.pm).await;
        assert!(res.is_err());
        let err_msg = res.unwrap_err().to_string();
        assert!(err_msg.contains("CHECKSUM_ERROR"), "Expected checksum error, got: {}", err_msg);

        Ok(())
    }

    #[tokio::test]
    async fn test_syntax_validation_error_rolls_back() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let env = TestEnvironment::new("syntax_error");
        let repository = SqliteSessionRepository;

        let file_path = env.dir.join("code.rs");
        let initial_content = "fn main() {\n    let a = 1;\n}\n";
        fs::write(&file_path, initial_content)?;
        let filepath_str = file_path.to_str().unwrap();

        let _metadata = init_edit_session(filepath_str, false)?;

        // Find the line 2 ID
        let lines_view = crate::tools::view::view_lines(&repository, filepath_str, 1, 3, None)?;
        let target_id = find_line_id(&lines_view, "let a = 1;");
        assert!(!target_id.is_empty());

        // Apply edit that introduces syntax error (e.g. mismatched braces / parsing error)
        let edits = vec![
            LineEdit {
                op: "update".to_string(),
                target_id: Some(target_id),
                content: Some("    let a = {;".to_string()), // Syntax error
                ..Default::default()
            }
        ];

        let res = edit_lines(&repository, filepath_str, edits, &env.pm).await;
        assert!(res.is_err());
        let err_msg = res.unwrap_err().to_string();
        assert!(err_msg.contains("Validation error"), "Expected syntax validation error, got: {}", err_msg);

        // Verify disk content was rolled back (i.e. remains unchanged)
        assert_eq!(fs::read_to_string(&file_path)?, initial_content);

        // Verify DB content was rolled back (re-query content of line 2)
        let lines = repository.fetch_lines_range(&_metadata.session_id, 2, 2)?;
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].2.trim(), "let a = 1;");

        Ok(())
    }

    #[tokio::test]
    async fn test_insert_into_empty_file() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let env = TestEnvironment::new("empty_file");
        let repository = SqliteSessionRepository;

        let file_path = env.dir.join("code.rs");
        fs::write(&file_path, "")?;
        let filepath_str = file_path.to_str().unwrap();

        let metadata = init_edit_session(filepath_str, false)?;
        assert_eq!(metadata.total_lines, 0);

        let edits = vec![
            LineEdit {
                op: "append".to_string(),
                target_id: None,
                content: Some("fn main() {\n}".to_string()),
                ..Default::default()
            }
        ];

        let preview = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
        assert!(preview.contains("fn main() {"));
        assert_eq!(fs::read_to_string(&file_path)?, "fn main() {\n}\n");

        Ok(())
    }

    #[tokio::test]
    async fn test_case_insensitive_validation_and_deletion_preview() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let env = TestEnvironment::new("case_insensitive_and_delete");
        let repository = SqliteSessionRepository;

        // Use uppercase extension: .RS
        let file_path = env.dir.join("code.RS");
        fs::write(&file_path, "fn main() {\n    let a = 1;\n    let b = 2;\n    let c = 3;\n}\n")?;
        let filepath_str = file_path.to_str().unwrap();

        let metadata = init_edit_session(filepath_str, false)?;
        assert_eq!(metadata.total_lines, 5);

        // Get the line IDs by viewing
        let lines_view = crate::tools::view::view_lines(&repository, filepath_str, 1, 5, None)?;
        let b_id = find_line_id(&lines_view, "let b = 2;");
        assert!(!b_id.is_empty());

        // Test delete on .RS file (verifies lowercase lookup/delegation works for uppercase extensions too)
        let edits = vec![
            LineEdit {
                op: "delete".to_string(),
                target_id: Some(b_id.clone()),
                content: None,
                ..Default::default()
            }
        ];

        let preview = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
        
        // The deleted line was `let b = 2;`.
        // The remaining lines around it should be rendered in the preview.
        assert!(preview.contains("let a = 1;"));
        assert!(preview.contains("let c = 3;"));
        assert!(!preview.contains("let b = 2;"));

        Ok(())
    }

    #[tokio::test]
    async fn test_append_operation_empty_file() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap();
        let env = TestEnvironment::new("append_empty");
        let repository = SqliteSessionRepository;

        let file_path = env.dir.join("code.rs");
        fs::write(&file_path, "")?;
        let filepath_str = file_path.to_str().unwrap();

        let metadata = init_edit_session(filepath_str, false)?;
        assert_eq!(metadata.total_lines, 0);

        let edits = vec![
            LineEdit {
                op: "append".to_string(),
                target_id: None,
                content: Some("pub fn foo() {}".to_string()),
                ..Default::default()
            }
        ];

        let preview = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
        assert!(preview.contains("pub fn foo()"));
        assert_eq!(fs::read_to_string(&file_path)?, "pub fn foo() {}\n");

        Ok(())
    }

    #[tokio::test]
    async fn test_append_operation_with_content() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = TestEnvironment::new("append_content");
        let repository = SqliteSessionRepository;

        let file_path = env.dir.join("code.rs");
        fs::write(&file_path, "pub fn foo() {}\n")?;
        let filepath_str = file_path.to_str().unwrap();

        let metadata = init_edit_session(filepath_str, false)?;
        assert_eq!(metadata.total_lines, 1);

        let edits = vec![
            LineEdit {
                op: "append".to_string(),
                target_id: None,
                content: Some("pub fn bar() -> i32 {\n    42\n}".to_string()),
                ..Default::default()
            }
        ];

        let preview = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
        assert!(preview.contains("pub fn bar() -> i32"));
        assert!(preview.contains("42"));
        assert_eq!(fs::read_to_string(&file_path)?, "pub fn foo() {}\npub fn bar() -> i32 {\n    42\n}\n");

        Ok(())
    }

    #[tokio::test]
    async fn test_strict_target_id_validation() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = TestEnvironment::new("strict_validation");
        let repository = SqliteSessionRepository;

        let file_path = env.dir.join("code.rs");
        fs::write(&file_path, "pub fn foo() {}\n")?;
        let filepath_str = file_path.to_str().unwrap();

        let metadata = init_edit_session(filepath_str, false)?;
        assert_eq!(metadata.total_lines, 1);

        // Call update with target_id = None
        let edits = vec![
            LineEdit {
                op: "update".to_string(),
                target_id: None,
                content: Some("pub fn bar() {}".to_string()),
                ..Default::default()
            }
        ];

        let result = edit_lines(&repository, filepath_str, edits, &env.pm).await;
        assert!(result.is_err());
        let err_msg = result.err().unwrap().to_string();
        assert!(err_msg.contains("Missing target_id for update op"));

        Ok(())
    }

    #[tokio::test]
    async fn test_prepend_operation() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = TestEnvironment::new("prepend");
        let repository = SqliteSessionRepository;

        let file_path = env.dir.join("code.rs");
        fs::write(&file_path, "pub fn hello() {}\n")?;
        let filepath_str = file_path.to_str().unwrap();

        let _metadata = init_edit_session(filepath_str, false)?;

        let edits = vec![
            LineEdit {
                op: "prepend".to_string(),
                target_id: None,
                content: Some("use std::collections::HashMap;\n".to_string()),
                ..Default::default()
            }
        ];

        let preview = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
        assert!(preview.contains("use std::collections::HashMap;"));
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
        let repository = SqliteSessionRepository;

        let file_path = env.dir.join("code.rs");
        fs::write(&file_path, "pub fn foo() {\n    let a = 1;\n    let b = 2;\n}\n")?;
        let filepath_str = file_path.to_str().unwrap();

        let _metadata = init_edit_session(filepath_str, false)?;

        // Find IDs for lines 2 and 3
        let lines_view = crate::tools::view::view_lines(&repository, filepath_str, 2, 3, None)?;
        let id_2 = find_line_id(&lines_view, "let a = 1;");
        let id_3 = find_line_id(&lines_view, "let b = 2;");

        let edits = vec![
            LineEdit {
                op: "replace_range".to_string(),
                target_id: Some(id_2),
                end_target_id: Some(id_3),
                content: Some("    let val = 42;".to_string()),
                ..Default::default()
            }
        ];

        let preview = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
        assert!(preview.contains("let val = 42;"));
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
        let repository = SqliteSessionRepository;

        let file_path = env.dir.join("code.rs");
        fs::write(&file_path, "pub fn main() {\n    foo();\n}\npub fn foo() {}\n")?;
        let filepath_str = file_path.to_str().unwrap();

        let _metadata = init_edit_session(filepath_str, false)?;

        // Get target ID for lines 4 (pub fn foo() {})
        let lines_view = crate::tools::view::view_lines(&repository, filepath_str, 4, 4, None)?;
        let foo_id = find_line_id(&lines_view, "pub fn foo() {}");

        // Get target ID for line 1 (pub fn main() {)
        let lines_view_main = crate::tools::view::view_lines(&repository, filepath_str, 1, 1, None)?;
        let main_id = find_line_id(&lines_view_main, "pub fn main() {");

        // Move 'foo' function before 'main' function
        let edits = vec![
            LineEdit {
                op: "move".to_string(),
                target_id: Some(foo_id),
                dest_target_id: Some(main_id),
                move_position: Some("before".to_string()),
                ..Default::default()
            }
        ];

        let preview = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
        assert!(preview.contains("pub fn foo() {}"));
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

        // Ensure no stale session exists in DB from previous test runs
        let repository = SqliteSessionRepository;
        let _ = repository.delete_session(filepath_str);

        // Call edit_lines directly without calling init_edit_session.
        // We can append a line. Since it's append, target_id is ignored.
        let edits = vec![
            LineEdit {
                op: "append".to_string(),
                content: Some("line 3".to_string()),
                ..Default::default()
            }
        ];

        let preview = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
        assert!(preview.contains("line 3"));

        let content = fs::read_to_string(&file_path)?;
        assert_eq!(content, "line 1\nline 2\nline 3\n");

        fs::remove_dir_all(&env.dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn test_long_line_edit_protection() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = TestEnvironment::new("long_line_test");
        let repository = SqliteSessionRepository;

        let file_path = env.dir.join("code.rs");
        // Create a file containing a line > 2,048 chars.
        let long_line = "a".repeat(2050);
        let file_content = format!("fn main() {{\n    // {}\n}}\n", long_line);
        fs::write(&file_path, &file_content)?;
        let filepath_str = file_path.to_str().unwrap();

        let metadata = init_edit_session(filepath_str, false)?;
        assert_eq!(metadata.total_lines, 3);

        // Get the lines view
        let lines_view = crate::tools::view::view_lines(&repository, filepath_str, 1, 3, None)?;
        
        // Assert that the line has been truncated in the view (ends with #TRUNC)
        let target_id = find_line_id(&lines_view, "    // aaaaa");
        assert!(target_id.ends_with("#TRUNC"));

        // 1. Try to update using target_id (ends with #TRUNC)
        let edits = vec![
            LineEdit {
                op: "update".to_string(),
                target_id: Some(target_id.clone()),
                content: Some("    // updated long line".to_string()),
                ..Default::default()
            }
        ];

        let result = edit_lines(&repository, filepath_str, edits, &env.pm).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("LINE_TOO_LONG_ERROR"));
        assert!(err_msg.contains("Line is too long (2057 chars)"));
        assert!(err_msg.contains("prettier, black, or cargo fmt"));

        // Verify that the file remains unchanged on disk
        let disk_content = fs::read_to_string(&file_path)?;
        assert_eq!(disk_content, file_content);

        // 2. Try to update using a fake valid-looking ID but same sequence ID
        let parts: Vec<&str> = target_id.split('#').collect();
        let normal_target_id = format!("{}#abcdefabcdef", parts[0]);
        
        let edits_normal = vec![
            LineEdit {
                op: "update".to_string(),
                target_id: Some(normal_target_id),
                content: Some("    // updated long line".to_string()),
                ..Default::default()
            }
        ];
        
        let result_normal = edit_lines(&repository, filepath_str, edits_normal, &env.pm).await;
        assert!(result_normal.is_err());
        let err_msg_normal = result_normal.unwrap_err().to_string();
        assert!(err_msg_normal.contains("LINE_TOO_LONG_ERROR"));
        assert!(err_msg_normal.contains("Line is too long (2057 chars)"));
        assert!(err_msg_normal.contains("prettier, black, or cargo fmt"));

        // Verify that the file remains unchanged on disk
        let disk_content = fs::read_to_string(&file_path)?;
        assert_eq!(disk_content, file_content);

        // 3. Try replace_range operation where the end line is truncated
        let start_line_id = find_line_id(&lines_view, "fn main() {");
        assert!(!start_line_id.ends_with("#TRUNC"));

        let edits_replace = vec![
            LineEdit {
                op: "replace_range".to_string(),
                target_id: Some(start_line_id),
                end_target_id: Some(target_id),
                content: Some("fn main() {\n    // replaced".to_string()),
                ..Default::default()
            }
        ];

        let result_replace = edit_lines(&repository, filepath_str, edits_replace, &env.pm).await;
        assert!(result_replace.is_err());
        let err_msg_replace = result_replace.unwrap_err().to_string();
        assert!(err_msg_replace.contains("LINE_TOO_LONG_ERROR"));
        assert!(err_msg_replace.contains("Line is too long (2057 chars)"));
        assert!(err_msg_replace.contains("prettier, black, or cargo fmt"));

        // Verify that the file remains unchanged on disk
        let disk_content = fs::read_to_string(&file_path)?;
        assert_eq!(disk_content, file_content);

        fs::remove_dir_all(&env.dir)?;
        Ok(())
    }
}
