use ast_editor::parser::ParserManager;
use ast_editor::tools::edit;
use ast_editor::tools::session_db::{EditOp, SqliteSessionRepository};
use ast_editor::tools::view;
use ast_editor::tools::ToolDispatcher;
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

fn create_test_parser_manager() -> ParserManager {
    // Every grammar is compiled in (D-01M28RAGW19ZZC), so there is no
    // directory to arrange.
    ParserManager::new().unwrap()
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
    let _guard = CleanupGuard {
        dir: temp_dir.clone(),
    };

    let repo = SqliteSessionRepository;
    let pm = create_test_parser_manager();

    // 2. Act:
    // Run create return_ids=false
    let file_default = temp_dir.join("create_default.rs");
    let content = "fn main() {\n    let x = 42;\n    let scratch = 0;\n}\n";
    let out_create_default =
        view::create_lines(&repo, file_default.to_str().unwrap(), content, Some(false)).unwrap();

    // Run create return_ids=true
    let file_ids = temp_dir.join("create_ids.rs");
    let out_create_ids =
        view::create_lines(&repo, file_ids.to_str().unwrap(), content, Some(true)).unwrap();

    // Run view only_ids=None
    let out_view_default = view::view_lines(
        &repo,
        file_ids.to_str().unwrap(),
        Some(1),
        Some(3),
        None,
        None,
        None,
        None,
    )
    .unwrap();

    // Run view only_ids=true
    let out_view_only_ids = view::view_lines(
        &repo,
        file_ids.to_str().unwrap(),
        Some(1),
        Some(3),
        Some(true),
        None,
        None,
        None,
    )
    .unwrap();

    // Parse the returned line IDs: line 2 is edited, line 3 is the one the
    // batch removes, so the example leaves a file that still parses.
    let val_create_ids: serde_json::Value = serde_json::from_str(&out_create_ids).unwrap();
    let ids = val_create_ids["lines"].as_array().unwrap();
    // Each entry is [id, line] (card #5).
    let id_to_update = ids[1][0].as_str().unwrap().to_string();
    let id_to_insert_after = ids[1][0].as_str().unwrap().to_string();
    let id_to_delete = ids[2][0].as_str().unwrap().to_string();

    // Run edit multi-operation batch (update, insert_after, delete)
    let edits = vec![
        edit::LineEdit {
            op: EditOp::InsertAfter,
            target_id: Some(id_to_insert_after.clone()),
            content: Some("    let y = 200;".to_string()),
            ..Default::default()
        },
        edit::LineEdit {
            op: EditOp::Replace,
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
    let out_edit_dry_run =
        edit::edit_lines_dry_run(&repo, file_ids.to_str().unwrap(), edits.clone(), &pm)
            .await
            .unwrap();
    let out_edit_dry_run_report = out_edit_dry_run.report.clone();
    let out_edit_dry_run = format!(
        "{}\n{}",
        ast_editor::tools::fenced("diff", out_edit_dry_run.diff.trim_end()),
        ast_editor::tools::fenced("json", &out_edit_dry_run.report)
    )
    .replace(file_ids.to_str().unwrap(), DOC_FILEPATH);
    // The preview id counts up with every run, so pin it or the generated
    // documentation differs on each invocation.
    let out_edit_dry_run = {
        let parsed: serde_json::Value = serde_json::from_str(&out_edit_dry_run_report).unwrap();
        let minted = parsed["preview_id"]
            .as_str()
            .expect("a valid preview mints an id");
        out_edit_dry_run.replace(minted, DOC_PREVIEW_ID)
    };

    let out_edit_compact = edit::edit_lines(&repo, file_ids.to_str().unwrap(), edits, &pm)
        .await
        .unwrap();

    // Construct pretty-printed JSON inputs
    let create_input_default_val = serde_json::json!({
        "filepath": "/path/to/project/create_default.rs",
        "content": content,
        "return_ids": false
    });
    let fmt_create_input_default = format!(
        "```json\n{}\n```",
        serde_json::to_string_pretty(&create_input_default_val).unwrap()
    );

    let create_input_ids_val = serde_json::json!({
        "filepath": "/path/to/project/create_ids.rs",
        "content": content,
        "return_ids": true
    });
    let fmt_create_input_ids = format!(
        "```json\n{}\n```",
        serde_json::to_string_pretty(&create_input_ids_val).unwrap()
    );

    let view_input_default_val = serde_json::json!({
        "filepath": "/path/to/project/create_ids.rs",
        "start_line": 1,
        "end_line": 3,
        "only_ids": false
    });
    let fmt_view_input_default = format!(
        "```json\n{}\n```",
        serde_json::to_string_pretty(&view_input_default_val).unwrap()
    );

    let view_input_only_ids_val = serde_json::json!({
        "filepath": "/path/to/project/create_ids.rs",
        "start_line": 1,
        "end_line": 3,
        "only_ids": true
    });
    let fmt_view_input_only_ids = format!(
        "```json\n{}\n```",
        serde_json::to_string_pretty(&view_input_only_ids_val).unwrap()
    );

    let edit_input_compact_val = serde_json::json!({
        "filepath": DOC_FILEPATH,
        "edits": [
            {
                "op": "insert_after",
                "target_id": id_to_insert_after,
                "content": "    let y = 200;"
            },
            {
                "op": "replace",
                "target_id": id_to_update,
                "content": "    let x = 100;"
            },
            {
                "op": "delete",
                "target_id": id_to_delete
            }
        ]
    });
    let fmt_edit_input_compact = format!(
        "```json\n{}\n```",
        serde_json::to_string_pretty(&edit_input_compact_val).unwrap()
    );

    let mut edit_input_dry_run_val = edit_input_compact_val.clone();
    edit_input_dry_run_val["dry_run"] = serde_json::Value::Bool(true);
    let fmt_edit_input_dry_run = format!(
        "```json\n{}\n```",
        serde_json::to_string_pretty(&edit_input_dry_run_val).unwrap()
    );

    // Extract schemas
    let dispatcher = ToolDispatcher::new();
    let tools = dispatcher.list_tools();

    let create_schema = tools
        .iter()
        .find(|t| t["name"] == "create")
        .and_then(|t| t.get("inputSchema"))
        .expect("create schema not found");

    let view_schema = tools
        .iter()
        .find(|t| t["name"] == "view")
        .and_then(|t| t.get("inputSchema"))
        .expect("view schema not found");

    let edit_schema = tools
        .iter()
        .find(|t| t["name"] == "edit")
        .and_then(|t| t.get("inputSchema"))
        .expect("edit schema not found");

    // Format all to pretty-printed json inside markdown code blocks
    let fmt_create_schema = format!(
        "```json\n{}\n```",
        serde_json::to_string_pretty(create_schema).unwrap()
    );
    let fmt_view_schema = format!(
        "```json\n{}\n```",
        serde_json::to_string_pretty(view_schema).unwrap()
    );
    let fmt_edit_schema = format!(
        "```json\n{}\n```",
        serde_json::to_string_pretty(edit_schema).unwrap()
    );

    // Format tool outputs to ensure they look perfect
    let fmt_create_default = format!("```json\n{}\n```", out_create_default);
    let fmt_create_ids = format!("```json\n{}\n```", out_create_ids);
    let fmt_view_default = format!(
        "```rust\n{}\n```\n\n```json\n{}\n```",
        out_view_default.lines_text.as_ref().unwrap(),
        out_view_default.metadata_json
    );
    let fmt_view_only_ids = format!("```json\n{}\n```", out_view_only_ids.metadata_json);
    let fmt_edit_compact = format!("```json\n{}\n```", out_edit_compact);
    let fmt_edit_dry_run = out_edit_dry_run.clone();

    // Each tool's one-line description is the one the binary prints, so the
    // document repeats what the tool says about itself rather than a second
    // account of it.
    let describe = |tool: &str| ast_editor::tools::metadata::get_tool_description(tool);
    let fmt_outline_description = describe("outline");
    let fmt_inspect_description = describe("inspect");
    let fmt_view_description = describe("view");
    let fmt_edit_description = describe("edit");
    let fmt_create_description = describe("create");

    // The caps, from the constants the formatter enforces.
    let fmt_line_cap = ast_editor::tools::formatter::LINE_CAP.to_string();
    let fmt_response_cap = {
        let bytes = ast_editor::tools::formatter::RESPONSE_BYTE_CAP;
        format!("{},{:03}", bytes / 1000, bytes % 1000)
    };
    let fmt_segment_length = ast_editor::tools::formatter::SEGMENT_LENGTH.to_string();

    // The version floor Cargo was given for this build.
    let fmt_msrv = env!("CARGO_PKG_RUST_VERSION").to_string();

    // The install paths, evaluated by just with a prefix written the way a
    // document wants it rather than the way this machine spells it.
    let evaluated = std::process::Command::new("just")
        .arg("--evaluate")
        .env("AST_EDITOR_PREFIX", "~/.agents")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("just is the task list and has to be on PATH to render the install paths");
    let evaluated = String::from_utf8(evaluated.stdout).unwrap();
    let evaluate = |name: &str| {
        evaluated
            .lines()
            .find_map(|line| {
                let (key, value) = line.split_once(":=")?;
                (key.trim() == name).then(|| value.trim().trim_matches('"').to_string())
            })
            .unwrap_or_else(|| panic!("just --evaluate printed no {name}"))
    };
    let fmt_prefix = evaluate("prefix");
    let fmt_bin_dir = evaluate("bin_dir");
    let fmt_skill_dir = evaluate("skill_dir");

    // The language table is the one the binary carries, so the document cannot
    // claim a grammar that is not compiled in or miss one that is.
    let fmt_languages = {
        let mut table = String::from("| Language | Extensions |\n|---|---|\n");
        for grammar in ast_editor::config::GRAMMARS {
            let extensions = grammar
                .extensions
                .iter()
                .map(|ext| format!("`.{ext}`"))
                .collect::<Vec<_>>()
                .join(", ");
            table.push_str(&format!("| {} | {} |\n", grammar.name, extensions));
        }
        table.trim_end().to_string()
    };

    // 3. Render every template from one placeholder table
    let placeholders: Vec<(&str, &str)> = vec![
        ("{{create_schema}}", &fmt_create_schema),
        ("{{languages}}", &fmt_languages),
        ("{{outline_description}}", &fmt_outline_description),
        ("{{inspect_description}}", &fmt_inspect_description),
        ("{{view_description}}", &fmt_view_description),
        ("{{edit_description}}", &fmt_edit_description),
        ("{{create_description}}", &fmt_create_description),
        ("{{line_cap}}", &fmt_line_cap),
        ("{{response_cap}}", &fmt_response_cap),
        ("{{segment_length}}", &fmt_segment_length),
        ("{{msrv}}", &fmt_msrv),
        ("{{prefix}}", &fmt_prefix),
        ("{{bin_dir}}", &fmt_bin_dir),
        ("{{skill_dir}}", &fmt_skill_dir),
        ("{{create_input_default}}", &fmt_create_input_default),
        ("{{create_output_default}}", &fmt_create_default),
        ("{{create_input_ids}}", &fmt_create_input_ids),
        ("{{create_output_ids}}", &fmt_create_ids),
        ("{{view_schema}}", &fmt_view_schema),
        ("{{view_input_default}}", &fmt_view_input_default),
        ("{{view_output_default}}", &fmt_view_default),
        ("{{view_input_only_ids}}", &fmt_view_input_only_ids),
        ("{{view_output_only_ids}}", &fmt_view_only_ids),
        ("{{edit_schema}}", &fmt_edit_schema),
        ("{{edit_input_compact}}", &fmt_edit_input_compact),
        ("{{edit_output_compact}}", &fmt_edit_compact),
        ("{{edit_input_dry_run}}", &fmt_edit_input_dry_run),
        ("{{edit_output_dry_run}}", &fmt_edit_dry_run),
    ];

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let templates_dir = manifest_dir.join("agent_skill").join("doc_templates");
    let render = |template: &std::path::Path, output: PathBuf| {
        let content = fs::read_to_string(template).unwrap();
        let rendered = placeholders
            .iter()
            .fold(content, |acc, (key, value)| acc.replace(key, value));
        assert!(
            !rendered.contains("{{"),
            "unfilled placeholder left in {}",
            output.display()
        );
        fs::write(output, rendered).unwrap();
    };

    render(
        &templates_dir.join("README.tpl.md"),
        manifest_dir.join("README.md"),
    );
    render(
        &templates_dir.join("api_specification.tpl.md"),
        manifest_dir
            .join("agent_skill")
            .join("references")
            .join("api_specification.md"),
    );
}
