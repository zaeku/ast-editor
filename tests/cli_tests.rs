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
        r#"{{"filepath":"{}","edits":[{{"op":"update","target_id":"{}","content":"    let a = 2;"}}]}}"#,
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
fn test_mcp_mode_still_answers_json_rpc() {
    use std::io::Write;
    use std::process::Stdio;

    let file = scratch("served.rs", "fn main() {}\n");
    let mut child = Command::new(env!("CARGO_BIN_EXE_ast-editor"))
        .arg("mcp")
        .env("AST_EDITOR_CACHE_DIR", store_for("mcp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let request = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"view_lines","arguments":{{"filepath":"{}"}}}}}}"#,
        file.display()
    );
    child.stdin.as_mut().unwrap().write_all(format!("{}\n", request).as_bytes()).unwrap();
    child.stdin.take();

    let out = child.wait_with_output().unwrap();
    let response: serde_json::Value = serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
    assert_eq!(response["jsonrpc"], "2.0");
    assert!(response["result"]["content"][0]["text"].as_str().unwrap().contains("fn main() {}"));
}

#[test]
fn test_a_bare_invocation_asks_rather_than_serving() {
    // Serving MCP used to be what this did. A client configured without the
    // subcommand would otherwise sit waiting on stdin, so it has to fail.
    let out = ast_editor("bare", &[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty(), "usage went to stdout");
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("ast-editor mcp"), "{}", text);
}
