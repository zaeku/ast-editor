use ast_editor::tools::session_db::{SqliteSessionRepository, EditOp};
use ast_editor::tools::view;
use ast_editor::tools::edit;
use ast_editor::tools::ToolDispatcher;
use ast_editor::parser::ParserManager;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static TEST_FILE_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Stands in for the workspace's temporary path wherever a tool echoes the file
/// it was given, so the generated documents do not carry a machine-local path.
const DOC_FILEPATH: &str = "/path/to/project/create_ids.rs";
const DOC_PREVIEW_ID: &str = "p1f";

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
    let file_ids = temp_dir.join("create_ids.txt");
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
        Some(1),
        Some(3),
        None,
        None,
        None,
    ).unwrap();

    // Run view_lines only_ids=true
    let out_view_only_ids = view::view_lines(
        &repo,
        file_ids.to_str().unwrap(),
        Some(1),
        Some(3),
        Some(true),
        None,
        None,
    ).unwrap();

    // Parse the returned line ID for `let x = 42;` (line 2)
    let val_create_ids: serde_json::Value = serde_json::from_str(&out_create_ids).unwrap();
    let ids = val_create_ids["ids"].as_array().unwrap();
    let id_to_update = ids[1].as_str().unwrap().to_string();
    let id_to_insert_after = ids[1].as_str().unwrap().to_string();
    let id_to_delete = ids[2].as_str().unwrap().to_string();

    // Run edit_lines multi-operation batch (update, insert_after, delete)
    let edits = vec![
        edit::LineEdit {
            op: EditOp::InsertAfter,
            target_id: Some(id_to_insert_after.clone()),
            content: Some("    let y = 200;".to_string()),
            ..Default::default()
        },
        edit::LineEdit {
            op: EditOp::Update,
            target_id: Some(id_to_update.clone()),
            content: Some("    let x = 100;".to_string()),
            ..Default::default()
        },
        edit::LineEdit {
            op: EditOp::Delete,
            target_id: Some(id_to_delete.clone()),
            ..Default::default()
        },
    ];
    // Preview the batch first, then apply it. The preview leaves the session
    // untouched, so the same IDs are still valid for the real call below.
    let out_edit_dry_run = edit::edit_lines_dry_run(
        &repo,
        file_ids.to_str().unwrap(),
        edits.clone(),
        &pm,
    ).await.unwrap();
    let out_edit_dry_run = out_edit_dry_run.replace(file_ids.to_str().unwrap(), DOC_FILEPATH);
    // The preview id counts up with every run, so pin it or the generated
    // documentation differs on each invocation.
    let out_edit_dry_run = {
        let parsed: serde_json::Value = serde_json::from_str(&out_edit_dry_run).unwrap();
        let minted = parsed["preview_id"].as_str().expect("a valid preview mints an id");
        out_edit_dry_run.replace(minted, DOC_PREVIEW_ID)
    };

    let out_edit_compact = edit::edit_lines(
        &repo,
        file_ids.to_str().unwrap(),
        edits,
        &pm,
    ).await.unwrap();

    // Construct pretty-printed JSON inputs
    let create_lines_input_default_val = serde_json::json!({
        "filepath": "/path/to/project/create_default.rs",
        "content": content,
        "return_ids": false
    });
    let fmt_create_lines_input_default = format!("```json\n{}\n```", serde_json::to_string_pretty(&create_lines_input_default_val).unwrap());

    let create_lines_input_ids_val = serde_json::json!({
        "filepath": "/path/to/project/create_ids.rs",
        "content": content,
        "return_ids": true
    });
    let fmt_create_lines_input_ids = format!("```json\n{}\n```", serde_json::to_string_pretty(&create_lines_input_ids_val).unwrap());

    let view_lines_input_default_val = serde_json::json!({
        "filepath": "/path/to/project/create_ids.rs",
        "start_line": 1,
        "end_line": 3,
        "only_ids": false
    });
    let fmt_view_lines_input_default = format!("```json\n{}\n```", serde_json::to_string_pretty(&view_lines_input_default_val).unwrap());

    let view_lines_input_only_ids_val = serde_json::json!({
        "filepath": "/path/to/project/create_ids.rs",
        "start_line": 1,
        "end_line": 3,
        "only_ids": true
    });
    let fmt_view_lines_input_only_ids = format!("```json\n{}\n```", serde_json::to_string_pretty(&view_lines_input_only_ids_val).unwrap());

    let edit_lines_input_compact_val = serde_json::json!({
        "filepath": DOC_FILEPATH,
        "edits": [
            {
                "op": "insert_after",
                "target_id": id_to_insert_after,
                "content": "    let y = 200;"
            },
            {
                "op": "update",
                "target_id": id_to_update,
                "content": "    let x = 100;"
            },
            {
                "op": "delete",
                "target_id": id_to_delete
            }
        ]
    });
    let fmt_edit_lines_input_compact = format!("```json\n{}\n```", serde_json::to_string_pretty(&edit_lines_input_compact_val).unwrap());

    let mut edit_lines_input_dry_run_val = edit_lines_input_compact_val.clone();
    edit_lines_input_dry_run_val["dry_run"] = serde_json::Value::Bool(true);
    let fmt_edit_lines_input_dry_run = format!("```json\n{}\n```", serde_json::to_string_pretty(&edit_lines_input_dry_run_val).unwrap());

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

    // Format tool outputs to ensure they look perfect
    let fmt_create_default = format!("```json\n{}\n```", out_create_default);
    let fmt_create_ids = format!("```json\n{}\n```", out_create_ids);
    let fmt_view_default = format!(
        "```rust\n{}\n```\n\n```json\n{}\n```",
        out_view_default.lines_text.as_ref().unwrap(),
        out_view_default.metadata_json
    );
    let fmt_view_only_ids = format!(
        "```json\n{}\n```",
        out_view_only_ids.metadata_json
    );
    let fmt_edit_compact = format!("```json\n{}\n```", out_edit_compact);
    let fmt_edit_dry_run = format!("```json\n{}\n```", out_edit_dry_run);

    // 3. Render every template from one placeholder table
    let placeholders: Vec<(&str, &str)> = vec![
        ("{{create_lines_schema}}", &fmt_create_lines_schema),
        ("{{create_lines_input_default}}", &fmt_create_lines_input_default),
        ("{{create_lines_output_default}}", &fmt_create_default),
        ("{{create_lines_input_ids}}", &fmt_create_lines_input_ids),
        ("{{create_lines_output_ids}}", &fmt_create_ids),
        ("{{view_lines_schema}}", &fmt_view_lines_schema),
        ("{{view_lines_input_default}}", &fmt_view_lines_input_default),
        ("{{view_lines_output_default}}", &fmt_view_default),
        ("{{view_lines_input_only_ids}}", &fmt_view_lines_input_only_ids),
        ("{{view_lines_output_only_ids}}", &fmt_view_only_ids),
        ("{{edit_lines_schema}}", &fmt_edit_lines_schema),
        ("{{edit_lines_input_compact}}", &fmt_edit_lines_input_compact),
        ("{{edit_lines_output_compact}}", &fmt_edit_compact),
        ("{{edit_lines_input_dry_run}}", &fmt_edit_lines_input_dry_run),
        ("{{edit_lines_output_dry_run}}", &fmt_edit_dry_run),
    ];

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let templates_dir = manifest_dir.join("agent_skill").join("doc_templates");
    let render = |template: &std::path::Path, output: PathBuf| {
        let content = fs::read_to_string(template).unwrap();
        let rendered = placeholders.iter()
            .fold(content, |acc, (key, value)| acc.replace(key, value));
        assert!(
            !rendered.contains("{{"),
            "unfilled placeholder left in {}",
            output.display()
        );
        fs::write(output, rendered).unwrap();
    };

    render(&templates_dir.join("README.tpl.md"), manifest_dir.join("README.md"));
    render(
        &templates_dir.join("api_specification.tpl.md"),
        manifest_dir.join("agent_skill").join("references").join("api_specification.md"),
    );

    // Read generated SKILL.md to sync with the global config folder later
    let skill_path = manifest_dir.join("agent_skill").join("SKILL.md");
    let skill_content = fs::read_to_string(skill_path).unwrap();



    // 9. Copy to global config folder if home_dir is found
    if let Some(mut global_skill_path) = dirs::home_dir() {
        global_skill_path.push(".gemini");
        global_skill_path.push("config");
        global_skill_path.push("plugins");
        global_skill_path.push("custom-developer-plugin");
        global_skill_path.push("skills");
        global_skill_path.push("ast-editor");
        let _ = fs::create_dir_all(&global_skill_path);
        global_skill_path.push("SKILL.md");
        fs::write(global_skill_path, skill_content).unwrap();
    }
}
