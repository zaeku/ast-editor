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

/// Run the command the way a reader would, against a store of its own.
fn ast_editor(cache: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_ast-editor"))
        .args(args)
        .env("AST_EDITOR_CACHE_DIR", cache)
        .output()
        .expect("failed to run ast-editor");
    assert!(
        out.status.success(),
        "ast-editor {args:?} exited non-zero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("ast-editor answers in UTF-8")
}

/// The JSON an answer carries, taken back out of the block it was printed in.
fn json_of(answer: &str) -> serde_json::Value {
    let body = answer
        .split("```json")
        .nth(1)
        .and_then(|rest| rest.split("```").next())
        .expect("an answer carries a json block");
    serde_json::from_str(body).expect("the json block parses")
}

#[test]
fn generate_readme() {
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
    // A store of its own, so no lock is needed against the other suites: each
    // call is a process that shares nothing with them.
    let cache = temp_dir.join("cache");

    // 2. Act: every answer below is the command's own stdout, fences and all,
    // so the document shows what a reader gets rather than a rebuilt copy of
    // it.
    let content = "fn main() {\n    let x = 42;\n    let scratch = 0;\n}\n";
    let file_default = temp_dir.join("create_default.rs");
    let out_create_default = ast_editor(
        &cache,
        &[
            "create",
            file_default.to_str().unwrap(),
            "--content",
            content,
            "--return-ids",
            "false",
        ],
    );

    let file_ids = temp_dir.join("create_ids.rs");
    let path_ids = file_ids.to_str().unwrap().to_string();
    let out_create_ids = ast_editor(
        &cache,
        &[
            "create",
            &path_ids,
            "--content",
            content,
            "--return-ids",
            "true",
        ],
    );

    let out_view_default = ast_editor(&cache, &["view", &path_ids, "1,3"]);
    let out_view_only_ids = ast_editor(&cache, &["view", &path_ids, "1,3", "--only-ids"]);

    // Lines 2 and 3 go out together and one line comes back in their place, so
    // the example addresses a span and the file it leaves still parses.
    let ids = json_of(&out_create_ids);
    let ids = ids["lines"].as_array().unwrap();
    // Each entry is [id, line] (card #5).
    let id_at = |n: usize| ids[n][0].as_str().unwrap().to_string();
    let id_to_insert_after = id_at(0);
    let span_start = id_at(1);
    let span_end = id_at(2);

    let batch = serde_json::json!({
        "filepath": path_ids,
        "edits": [
            {
                "op": "insert_after",
                "start_id": id_to_insert_after,
                "content": "    let y = 200;"
            },
            {
                "op": "replace",
                "start_id": span_start,
                "end_id": span_end,
                "content": "    let x = 100;"
            }
        ]
    });
    let batch = serde_json::to_string(&batch).unwrap();

    // Preview the batch first, then apply it. The preview leaves the session
    // untouched, so the same IDs are still valid for the real call below.
    let out_edit_dry_run = ast_editor(&cache, &["edit", &path_ids, "--json", &batch, "--dry-run"]);
    // The preview id counts up with every run, so pin it or the generated
    // documentation differs on each invocation.
    let minted = json_of(&out_edit_dry_run)["preview_id"]
        .as_str()
        .expect("a valid preview mints an id")
        .to_string();
    let out_edit_dry_run = out_edit_dry_run.replace(&minted, DOC_PREVIEW_ID);

    let out_edit_compact = ast_editor(&cache, &["edit", &path_ids, "--json", &batch]);

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
                "start_id": id_to_insert_after,
                "content": "    let y = 200;"
            },
            {
                "op": "replace",
                "start_id": span_start,
                "end_id": span_end,
                "content": "    let x = 100;"
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

    // The parameters are what `skill api` prints: one table per tool, rendered
    // from the schemas the binary serves. Rendering them a second time here
    // gave the repository a longer, less readable copy of the same thing.
    let api = ast_editor(&cache, &["skill", "api"]);
    let section = |tool: &str| {
        let heading = format!("## `{tool}`");
        let rest = api
            .split_once(&heading)
            .unwrap_or_else(|| panic!("skill api prints no section for {tool}"))
            .1;
        rest.split("\n## ").next().unwrap().trim().to_string()
    };
    // The description is the line the section opens with, and the table is the
    // rest. README takes the first on its own.
    let split_section = |tool: &str| {
        let body = section(tool);
        let (description, table) = body
            .split_once("\n\n")
            .unwrap_or_else(|| panic!("the {tool} section carries no table"));
        (description.trim().to_string(), table.trim().to_string())
    };
    let (fmt_outline_description, fmt_outline_parameters) = split_section("outline");
    let (fmt_inspect_description, fmt_inspect_parameters) = split_section("inspect");
    let (fmt_view_description, fmt_view_parameters) = split_section("view");
    let (fmt_edit_description, fmt_edit_parameters) = split_section("edit");
    let (fmt_create_description, fmt_create_parameters) = split_section("create");

    // Each answer already carries the fences the command printed it in, and
    // the path it echoes is this machine's. Only the path is rewritten.
    let doc = |answer: &str| answer.trim_end().replace(&path_ids, DOC_FILEPATH);
    let fmt_create_default = doc(&out_create_default).replace(
        file_default.to_str().unwrap(),
        "/path/to/project/create_default.rs",
    );
    let fmt_create_ids = doc(&out_create_ids);
    let fmt_view_default = doc(&out_view_default);
    let fmt_view_only_ids = doc(&out_view_only_ids);
    let fmt_edit_compact = doc(&out_edit_compact);
    let fmt_edit_dry_run = doc(&out_edit_dry_run);

    // The caps, taken from the warnings that announce them. A number a reader
    // is given to budget with should be the one the tool says when it stops,
    // and `resources/` is where the tool keeps what it says.
    let config: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("resources")
                .join("tool_config.json"),
        )
        .expect("resources/tool_config.json is readable"),
    )
    .expect("resources/tool_config.json parses");
    let numbers_in = |key: &str| -> Vec<String> {
        let message = config[key]
            .as_str()
            .unwrap_or_else(|| panic!("tool_config.json holds no {key}"));
        let mut found = Vec::new();
        let mut current = String::new();
        for character in message.chars() {
            if character.is_ascii_digit() || (character == ',' && !current.is_empty()) {
                current.push(character);
            } else if !current.is_empty() {
                found.push(current.trim_end_matches(',').to_string());
                current.clear();
            }
        }
        if !current.is_empty() {
            found.push(current.trim_end_matches(',').to_string());
        }
        found
    };
    let caps = numbers_in("warning_line_cap");
    let [fmt_line_cap, fmt_segment_length] = caps.as_slice() else {
        panic!(
            "warning_line_cap names {} numbers, wanted 2: {caps:?}",
            caps.len()
        );
    };
    let (fmt_line_cap, fmt_segment_length) = (fmt_line_cap.clone(), fmt_segment_length.clone());
    let fmt_response_cap = numbers_in("warning_cumulative_limit")
        .into_iter()
        .next()
        .expect("warning_cumulative_limit names no number");

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

    // The language table is the one `--version` reports, so the document cannot
    // claim a grammar that is not compiled in or miss one that is.
    let fmt_languages = {
        let mut table = String::from("| Language | Extensions |\n|---|---|\n");
        let reported = ast_editor(&cache, &["--version"]);
        let mut rows = 0;
        for line in reported.lines() {
            // A grammar's line is indented under the count; the first two words
            // are its name and the version pinned for it.
            let Some(entry) = line.strip_prefix(' ') else {
                continue;
            };
            let mut words = entry.split_whitespace();
            let (Some(name), Some(_version)) = (words.next(), words.next()) else {
                continue;
            };
            let extensions = words
                .map(|extension| format!("`{extension}`"))
                .collect::<Vec<_>>()
                .join(", ");
            assert!(
                !extensions.is_empty(),
                "--version names {name} without the extensions it claims: {line:?}"
            );
            table.push_str(&format!("| {name} | {extensions} |\n"));
            rows += 1;
        }
        assert!(rows > 0, "--version reported no grammar: {reported:?}");
        table.trim_end().to_string()
    };

    // 3. Render every template from one placeholder table
    let placeholders: Vec<(&str, &str)> = vec![
        ("{{create_parameters}}", &fmt_create_parameters),
        ("{{languages}}", &fmt_languages),
        ("{{outline_parameters}}", &fmt_outline_parameters),
        ("{{inspect_parameters}}", &fmt_inspect_parameters),
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
        ("{{view_parameters}}", &fmt_view_parameters),
        ("{{view_input_default}}", &fmt_view_input_default),
        ("{{view_output_default}}", &fmt_view_default),
        ("{{view_input_only_ids}}", &fmt_view_input_only_ids),
        ("{{view_output_only_ids}}", &fmt_view_only_ids),
        ("{{edit_parameters}}", &fmt_edit_parameters),
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
