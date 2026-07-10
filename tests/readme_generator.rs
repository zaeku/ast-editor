use ast_editor::tools::session_db::SqliteSessionRepository;
use ast_editor::tools::view;
use ast_editor::tools::edit;
use ast_editor::tools::ToolDispatcher;
use ast_editor::parser::ParserManager;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static TEST_FILE_COUNTER: AtomicUsize = AtomicUsize::new(0);

struct CleanupGuard {
    dir: PathBuf,
}

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        if self.dir.exists() {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

fn acquire_db_lock() -> std::sync::MutexGuard<'static, ()> {
    match ast_editor::tools::TEST_DB_LOCK.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn create_test_parser_manager(tmp: &std::path::Path) -> ParserManager {
    let cache_dir = tmp.join("cache");
    let compiler_path = tmp.join("compiler");
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let wasm_dir = manifest_dir.join("resources").join("wasm");
    
    let _ = fs::create_dir_all(&cache_dir);
    let _ = fs::create_dir_all(&compiler_path);
    
    ParserManager::with_paths(cache_dir, compiler_path, wasm_dir).unwrap()
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn generate_readme() {
    let _lock = acquire_db_lock();

    // 1. Arrange: setup workspace folder
    let pid = std::process::id();
    let counter = TEST_FILE_COUNTER.fetch_add(1, Ordering::SeqCst);
    let temp_dir = std::env::temp_dir().join(format!("readme_gen_workspace_{}_{}", pid, counter));
    if temp_dir.exists() {
        let _ = fs::remove_dir_all(&temp_dir);
    }
    fs::create_dir_all(&temp_dir).unwrap();
    let _guard = CleanupGuard { dir: temp_dir.clone() };

    let repo = SqliteSessionRepository;
    let pm = create_test_parser_manager(&temp_dir);

    // 2. Act:
    // Run create_lines return_ids=false
    let file_default = temp_dir.join("create_default.rs");
    let content = "fn main() {\n    let x = 42;\n}\n";
    let out_create_default = view::create_lines(
        &repo,
        file_default.to_str().unwrap(),
        content,
        Some(false),
    ).unwrap();

    // Run create_lines return_ids=true
    let file_ids = temp_dir.join("create_ids.rs");
    let out_create_ids = view::create_lines(
        &repo,
        file_ids.to_str().unwrap(),
        content,
        Some(true),
    ).unwrap();

    // Run view_lines only_ids=None
    let out_view_default = view::view_lines(
        &repo,
        file_ids.to_str().unwrap(),
        1,
        3,
        None,
    ).unwrap();

    // Run view_lines only_ids=true
    let out_view_only_ids = view::view_lines(
        &repo,
        file_ids.to_str().unwrap(),
        1,
        3,
        Some(true),
    ).unwrap();

    // Parse the returned line ID for `let x = 42;` (line 2)
    let val_create_ids: serde_json::Value = serde_json::from_str(&out_create_ids).unwrap();
    let ids = val_create_ids["ids"].as_array().unwrap();
    let id_to_update = ids[1].as_str().unwrap().to_string();

    // Run edit_lines update operation
    let edits = vec![edit::LineEdit {
        op: "update".to_string(),
        target_id: Some(id_to_update),
        content: Some("    let x = 100;".to_string()),
        ..Default::default()
    }];
    let out_edit_compact = edit::edit_lines(
        &repo,
        file_ids.to_str().unwrap(),
        edits,
        &pm,
    ).await.unwrap();

    // Extract schemas
    let dispatcher = ToolDispatcher::new();
    let tools = dispatcher.list_tools();

    let create_lines_schema = tools.iter()
        .find(|t| t["name"] == "create_lines")
        .and_then(|t| t.get("inputSchema"))
        .expect("create_lines schema not found");

    let view_lines_schema = tools.iter()
        .find(|t| t["name"] == "view_lines")
        .and_then(|t| t.get("inputSchema"))
        .expect("view_lines schema not found");

    let edit_lines_schema = tools.iter()
        .find(|t| t["name"] == "edit_lines")
        .and_then(|t| t.get("inputSchema"))
        .expect("edit_lines schema not found");

    // Format all to pretty-printed json inside markdown code blocks
    let fmt_create_lines_schema = format!("```json\n{}\n```", serde_json::to_string_pretty(create_lines_schema).unwrap());
    let fmt_view_lines_schema = format!("```json\n{}\n```", serde_json::to_string_pretty(view_lines_schema).unwrap());
    let fmt_edit_lines_schema = format!("```json\n{}\n```", serde_json::to_string_pretty(edit_lines_schema).unwrap());

    // Re-parse and pretty print tool outputs to ensure they look perfect
    let fmt_create_default = format!("```json\n{}\n```", serde_json::to_string_pretty(&serde_json::from_str::<serde_json::Value>(&out_create_default).unwrap()).unwrap());
    let fmt_create_ids = format!("```json\n{}\n```", serde_json::to_string_pretty(&serde_json::from_str::<serde_json::Value>(&out_create_ids).unwrap()).unwrap());
    let fmt_view_default = format!("```json\n{}\n```", out_view_default);
    let fmt_view_only_ids = format!("```json\n{}\n```", out_view_only_ids);
    let fmt_edit_compact = format!("```json\n{}\n```", serde_json::to_string_pretty(&serde_json::from_str::<serde_json::Value>(&out_edit_compact).unwrap()).unwrap());

    // 3. Read README.tpl.md
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let readme_tpl_path = manifest_dir.join("README.tpl.md");
    let mut readme_content = fs::read_to_string(readme_tpl_path).unwrap();

    // 4. Replace all 8 placeholders
    readme_content = readme_content.replace("{{create_lines_schema}}", &fmt_create_lines_schema);
    readme_content = readme_content.replace("{{create_lines_output_default}}", &fmt_create_default);
    readme_content = readme_content.replace("{{create_lines_output_ids}}", &fmt_create_ids);
    readme_content = readme_content.replace("{{view_lines_schema}}", &fmt_view_lines_schema);
    readme_content = readme_content.replace("{{view_lines_output_default}}", &fmt_view_default);
    readme_content = readme_content.replace("{{view_lines_output_only_ids}}", &fmt_view_only_ids);
    readme_content = readme_content.replace("{{edit_lines_schema}}", &fmt_edit_lines_schema);
    readme_content = readme_content.replace("{{edit_lines_output_compact}}", &fmt_edit_compact);

    // 5. Write to README.md at the workspace root
    let readme_path = manifest_dir.join("README.md");
    fs::write(readme_path, readme_content).unwrap();
}
