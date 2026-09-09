//! The command-line surface, driven as a shell would drive it.

use std::process::Command;

/// Each test gets its own store: these run in parallel, and two processes
/// opening one SQLite file contend on the journal-mode pragma.
fn store_for(test: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("ast-editor-cli-{}-{}", std::process::id(), test))
}

fn ast_editor(test: &str, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ast-editor"))
        .args(args)
        .env("AST_EDITOR_CACHE_DIR", store_for(test))
        .output()
        .expect("failed to run ast-editor")
}

fn scratch(name: &str, content: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ast-editor-cli-files-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, content).unwrap();
    path
}

#[test]
fn test_help_lists_the_tools() {
    let out = ast_editor("help", &["--help"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("ast-editor <tool>"), "{}", text);
    for tool in ["view_lines", "edit_lines", "inspect_ast"] {
        assert!(text.contains(tool), "{} missing from help: {}", tool, text);
    }
}

#[test]
fn test_a_tool_prints_its_own_output() {
    let file = scratch("shown.rs", "fn main() {\n    let a = 1;\n}\n");
    let out = ast_editor("shown", &["view_lines", &format!(r#"{{"filepath":"{}"}}"#, file.display())]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    // The tool's text, not the JSON-RPC envelope around it.
    assert!(text.contains("1: fn main() {"), "{}", text);
    assert!(!text.contains("jsonrpc"), "{}", text);
}

#[test]
fn test_an_edit_reaches_the_file() {
    let file = scratch("edited.rs", "fn main() {\n    let a = 1;\n}\n");
    let path = file.display().to_string();

    let ids = ast_editor("edited", &["view_lines", &format!(r#"{{"filepath":"{}","only_ids":true}}"#, path)]);
    assert!(ids.status.success(), "{}", String::from_utf8_lossy(&ids.stderr));
    let meta: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&ids.stdout)).unwrap();
    let target = meta["ids"][1][0].as_str().unwrap().to_string();

    let edit = ast_editor("edited", &["edit_lines", &format!(
        r#"{{"filepath":"{}","edits":[{{"op":"replace","target_id":"{}","content":"    let a = 2;"}}]}}"#,
        path, target
    )]);
    assert!(edit.status.success(), "{}", String::from_utf8_lossy(&edit.stderr));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "fn main() {\n    let a = 2;\n}\n");
}

#[test]
fn test_a_failure_goes_to_stderr_with_a_nonzero_status() {
    let unknown = ast_editor("failure", &["no_such_tool", "{}"]);
    assert!(!unknown.status.success());
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("Unknown tool"));
    assert!(unknown.stdout.is_empty());

    let malformed = ast_editor("failure", &["view_lines", "{not json}"]);
    assert!(!malformed.status.success());
    assert!(String::from_utf8_lossy(&malformed.stderr).contains("not valid JSON"));
}

#[test]
fn test_a_bare_invocation_asks_rather_than_waiting() {
    let out = ast_editor("bare", &[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty(), "usage went to stdout");
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("ast-editor edit"), "{}", text);
}

#[test]
fn test_version_reports_the_grammar_set_it_is_paired_with() {
    // A binary and a grammar directory that do not match is the likeliest
    // reason a file will not parse, so --version names both.
    let out = ast_editor("version", &["--version"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.starts_with("ast-editor "), "{}", text);
    assert!(text.contains("grammars "), "{}", text);
    assert!(text.contains("rust"), "the grammar list is missing: {}", text);
}

#[test]
fn test_a_tool_takes_a_path_and_options_instead_of_json() {
    let file = scratch("flags.rs", "fn main() {\n    let a = 1;\n    let b = 2;\n}\n");
    let out = ast_editor("flags", &[
        "view_lines", file.to_str().unwrap(), "--start-line", "2", "--end-line", "2",
    ]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("let a = 1;"), "{}", text);
    assert!(!text.contains("let b = 2;"), "the range was ignored: {}", text);
}

#[test]
fn test_a_relative_path_is_taken_from_the_working_directory() {
    let file = scratch("relative.rs", "fn main() {}\n");
    let dir = file.parent().unwrap();

    // The session is keyed by the path, so a relative one has to be resolved
    // before it reaches the store or the same file becomes two sessions.
    let out = Command::new(env!("CARGO_BIN_EXE_ast-editor"))
        .args(["view_lines", "relative.rs", "--only-ids"])
        .current_dir(dir)
        .env("AST_EDITOR_CACHE_DIR", store_for("relative"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let meta: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    assert_eq!(meta["total_lines"], 1);
}

#[test]
fn test_an_unknown_option_lists_the_ones_the_tool_has() {
    let file = scratch("unknownopt.rs", "fn main() {}\n");
    let out = ast_editor("unknownopt", &["view_lines", file.to_str().unwrap(), "--start-lines", "2"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--start-lines"), "{}", err);
    assert!(err.contains("--start-line"), "the real options are not offered: {}", err);
}

#[test]
fn test_the_json_form_a_program_would_send_still_works() {
    let file = scratch("stilljson.rs", "fn main() {\n    let a = 1;\n}\n");
    let path = file.display().to_string();

    let bare = ast_editor("stilljson", &["view_lines", &format!(r#"{{"filepath":"{}","only_ids":true}}"#, path)]);
    assert!(bare.status.success(), "{}", String::from_utf8_lossy(&bare.stderr));

    let flagged = ast_editor("stilljson", &[
        "view_lines", file.to_str().unwrap(), "--json", r#"{"only_ids":true}"#,
    ]);
    assert!(flagged.status.success(), "{}", String::from_utf8_lossy(&flagged.stderr));
    let meta: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&flagged.stdout)).unwrap();
    assert!(meta["ids"].is_array(), "{}", String::from_utf8_lossy(&flagged.stdout));
}
