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

/// The data half of a response. A caller reads it the way the skill document
/// says to: the block fenced as `json`, which for a json block is always
/// exactly three backticks.
fn json_block(stdout: &[u8]) -> serde_json::Value {
    let text = String::from_utf8_lossy(stdout);
    let body = text
        .lines()
        .skip_while(|line| *line != "```json")
        .skip(1)
        .take_while(|line| !line.starts_with("```"))
        .collect::<Vec<_>>()
        .join("\n");
    serde_json::from_str(&body).unwrap_or_else(|err| panic!("{}\n  in: {}", err, text))
}

#[test]
fn test_help_lists_the_tools() {
    let out = ast_editor("help", &["--help"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("ast-editor <tool>"), "{}", text);
    for tool in ["view", "edit", "inspect"] {
        assert!(text.contains(tool), "{} missing from help: {}", tool, text);
    }
}

#[test]
fn test_a_tool_prints_its_own_output() {
    let file = scratch("shown.rs", "fn main() {\n    let a = 1;\n}\n");
    let out = ast_editor(
        "shown",
        &["view", &format!(r#"{{"filepath":"{}"}}"#, file.display())],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    // The tool's text, not the JSON-RPC envelope around it.
    assert!(text.contains("1: fn main() {"), "{}", text);
    assert!(!text.contains("jsonrpc"), "{}", text);
}

#[test]
fn test_an_edit_reaches_the_file() {
    let file = scratch("edited.rs", "fn main() {\n    let a = 1;\n}\n");
    let path = file.display().to_string();

    let ids = ast_editor(
        "edited",
        &[
            "view",
            &format!(r#"{{"filepath":"{}","only_ids":true}}"#, path),
        ],
    );
    assert!(
        ids.status.success(),
        "{}",
        String::from_utf8_lossy(&ids.stderr)
    );
    let meta = json_block(&ids.stdout);
    let target = meta["ids"][1][0].as_str().unwrap().to_string();

    let edit = ast_editor(
        "edited",
        &[
            "edit",
            &format!(
                r#"{{"filepath":"{}","edits":[{{"op":"replace","target_id":"{}","content":"    let a = 2;"}}]}}"#,
                path, target
            ),
        ],
    );
    assert!(
        edit.status.success(),
        "{}",
        String::from_utf8_lossy(&edit.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "fn main() {\n    let a = 2;\n}\n"
    );
}

#[test]
fn test_a_failure_goes_to_stderr_with_a_nonzero_status() {
    let unknown = ast_editor("failure", &["no_such_tool", "{}"]);
    assert!(!unknown.status.success());
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("Unknown tool"));
    assert!(unknown.stdout.is_empty());

    let malformed = ast_editor("failure", &["view", "{not json}"]);
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
    assert!(
        text.contains("rust"),
        "the grammar list is missing: {}",
        text
    );
}

#[test]
fn test_a_tool_takes_a_path_and_options_instead_of_json() {
    let file = scratch(
        "flags.rs",
        "fn main() {\n    let a = 1;\n    let b = 2;\n}\n",
    );
    let out = ast_editor(
        "flags",
        &[
            "view",
            file.to_str().unwrap(),
            "--start-line",
            "2",
            "--end-line",
            "2",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("let a = 1;"), "{}", text);
    assert!(
        !text.contains("let b = 2;"),
        "the range was ignored: {}",
        text
    );
}

#[test]
fn test_a_relative_path_is_taken_from_the_working_directory() {
    let file = scratch("relative.rs", "fn main() {}\n");
    let dir = file.parent().unwrap();

    // The session is keyed by the path, so a relative one has to be resolved
    // before it reaches the store or the same file becomes two sessions.
    let out = Command::new(env!("CARGO_BIN_EXE_ast-editor"))
        .args(["view", "relative.rs", "--only-ids"])
        .current_dir(dir)
        .env("AST_EDITOR_CACHE_DIR", store_for("relative"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let meta = json_block(&out.stdout);
    assert_eq!(meta["total_lines"], 1);
}

#[test]
fn test_an_unknown_option_lists_the_ones_the_tool_has() {
    let file = scratch("unknownopt.rs", "fn main() {}\n");
    let out = ast_editor(
        "unknownopt",
        &["view", file.to_str().unwrap(), "--start-lines", "2"],
    );
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--start-lines"), "{}", err);
    assert!(
        err.contains("--start-line"),
        "the real options are not offered: {}",
        err
    );
}

#[test]
fn test_the_json_form_a_program_would_send_still_works() {
    let file = scratch("stilljson.rs", "fn main() {\n    let a = 1;\n}\n");
    let path = file.display().to_string();

    let bare = ast_editor(
        "stilljson",
        &[
            "view",
            &format!(r#"{{"filepath":"{}","only_ids":true}}"#, path),
        ],
    );
    assert!(
        bare.status.success(),
        "{}",
        String::from_utf8_lossy(&bare.stderr)
    );

    let flagged = ast_editor(
        "stilljson",
        &[
            "view",
            file.to_str().unwrap(),
            "--json",
            r#"{"only_ids":true}"#,
        ],
    );
    assert!(
        flagged.status.success(),
        "{}",
        String::from_utf8_lossy(&flagged.stderr)
    );
    let meta = json_block(&flagged.stdout);
    assert!(
        meta["ids"].is_array(),
        "{}",
        String::from_utf8_lossy(&flagged.stdout)
    );
}

/// Cargo sets CARGO_MANIFEST_DIR for everything it runs, so an installed
/// ast-editor called from another crate's build was sent looking for grammars
/// in whichever crate cargo was building, and lost every language.
#[test]
fn a_foreign_manifest_dir_does_not_hide_the_grammars() {
    let file = scratch("foreign_manifest.rs", "fn named() {}\n");
    let elsewhere =
        std::env::temp_dir().join(format!("ast-editor-not-this-crate-{}", std::process::id()));
    std::fs::create_dir_all(&elsewhere).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_ast-editor"))
        .args(["outline", file.to_str().unwrap()])
        .env("AST_EDITOR_CACHE_DIR", store_for("foreignmanifest"))
        .env("CARGO_MANIFEST_DIR", &elsewhere)
        .output()
        .expect("failed to run ast-editor");

    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("named"),
        "outlined nothing for a Rust file: {}",
        text
    );
}

/// The first call on a machine may be an edit: an agent holding ids from an
/// earlier session has no reason to look first. It used to die with
/// "no such table: sessions", because only the read path created the schema.
#[test]
fn the_first_call_may_be_an_edit() {
    let file = scratch("first_call.txt", "first\nsecond\n");
    let store = store_for("firstcall");
    std::fs::remove_dir_all(&store).ok();

    let mut child = Command::new(env!("CARGO_BIN_EXE_ast-editor"))
        .args(["edit", file.to_str().unwrap()])
        .env("AST_EDITOR_CACHE_DIR", &store)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to run ast-editor");
    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().unwrap();
        stdin.write_all(b"append ```\nthird\n```\n").unwrap();
    }
    let out = child.wait_with_output().unwrap();

    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "first\nsecond\nthird\n"
    );
}

/// A query that does not compile is not a query that found nothing: nothing
/// ran, so the call did not do what it was asked (D-01M28HCSAMTEFS).
#[test]
fn a_query_that_does_not_compile_is_an_error() {
    let file = scratch("bad_query.rs", "fn named() {}\n");

    let broken = ast_editor(
        "badquery",
        &["inspect", file.to_str().unwrap(), "--query", "zzz"],
    );
    assert!(
        !broken.status.success(),
        "exited 0 for a query that does not compile: {}{}",
        String::from_utf8_lossy(&broken.stdout),
        String::from_utf8_lossy(&broken.stderr)
    );
    assert!(
        String::from_utf8_lossy(&broken.stderr).contains("zzz"),
        "did not name the query that failed: {}",
        String::from_utf8_lossy(&broken.stderr)
    );

    let fine = ast_editor(
        "goodquery",
        &[
            "inspect",
            file.to_str().unwrap(),
            "--query",
            "(function_item) @f",
        ],
    );
    assert!(
        fine.status.success(),
        "{}",
        String::from_utf8_lossy(&fine.stderr)
    );
}

/// Nix ships a grammar and was never syntax-checked: a hand-written list of
/// what could be validated had been left one language behind the configuration
/// the parser reads (D-01M27WE1VYAKPP).
#[test]
fn every_shipped_language_is_syntax_checked() {
    let file = scratch("checked.nix", "{\n  a = 1;\n}\n");
    let listing = ast_editor("nixchecked", &["view", file.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&listing.stdout);
    let target = text
        .lines()
        .filter_map(|line| line.split('|').next())
        .map(str::trim)
        .filter(|id| id.contains('#'))
        .nth(1)
        .expect("no second line id")
        .to_string();

    let before = std::fs::read_to_string(&file).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_ast-editor"))
        .args(["edit", file.to_str().unwrap(), "--strict"])
        .env("AST_EDITOR_CACHE_DIR", store_for("nixchecked"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to run ast-editor");
    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().unwrap();
        stdin
            .write_all(format!("replace {} ```\n  a = ((( ;\n```\n", target).as_bytes())
            .unwrap();
    }
    let out = child.wait_with_output().unwrap();

    assert!(
        !out.status.success(),
        "wrote syntax the nix grammar rejects under --strict"
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), before);
}

/// Asking what a tool takes is a question, not a mistake. It used to answer
/// with "'view' takes no option '--help'" and exit 1.
#[test]
fn a_tool_answers_its_own_help() {
    for (tool, expected) in [
        ("view", "--only-ids"),
        ("edit", "--apply"),
        ("ins", "--template"),
    ] {
        let out = ast_editor("toolhelp", &[tool, "--help"]);
        assert!(
            out.status.success(),
            "{} --help exited {}: {}",
            tool,
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            text.contains(expected),
            "{} --help did not list {}: {}",
            tool,
            expected,
            text
        );
        assert!(
            text.contains("skill api"),
            "{} --help did not say where the full reference is: {}",
            tool,
            text
        );
    }
}

/// The help is read by piping it to `head`, which closes the pipe partway
/// through: that used to panic, and the first thirty lines are budgeted for
/// exactly that reader.
#[test]
fn the_first_thirty_lines_of_help_stand_alone() {
    let out = ast_editor("helphead", &["--help"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    let head: Vec<&str> = text.lines().take(30).collect();
    let head = head.join("\n");

    for needed in [
        "ast-editor <tool>",
        "outline",
        "#<hash>",
        "insert_after",
        "replace_range",
        "delete",
    ] {
        assert!(
            head.contains(needed),
            "the first thirty lines do not carry {needed:?}:\n{head}"
        );
    }
}
