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
    let listed = text
        .lines()
        .find(|line| line.starts_with("Tools:"))
        .unwrap_or_else(|| panic!("no Tools: line in help: {}", text));
    for tool in ["outline", "view", "inspect", "edit", "create"] {
        assert!(listed.contains(tool), "{} missing from {:?}", tool, listed);
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
    let target = meta["lines"][1][0].as_str().unwrap().to_string();

    let edit = ast_editor(
        "edited",
        &[
            "edit",
            &format!(
                r#"{{"filepath":"{}","edits":[{{"op":"replace","start_id":"{}","content":"    let a = 2;"}}]}}"#,
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
        meta["lines"].is_array(),
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

/// The first thirty lines are budgeted for a reader who stops there.
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
        "<start_id>,<end_id>",
        "delete",
    ] {
        assert!(
            head.contains(needed),
            "the first thirty lines do not carry {needed:?}:\n{head}"
        );
    }
}

/// A reader that stops reading closes the pipe under the writer, and the write
/// that lands on it used to panic. One response cannot reach that write: the
/// formatter caps a response at 45,000 bytes and a pipe buffers 64K, so the
/// whole of it fits and the writer never blocks. Four paths in one call is four
/// responses, which does not fit.
#[test]
fn a_reader_that_stops_does_not_break_the_writer() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let long: String = (0..4000).map(|n| format!("let line{n} = {n};\n")).collect();
    let path = scratch("long_enough_to_fill_a_pipe.rs", &long);
    let mut child = Command::new(env!("CARGO_BIN_EXE_ast-editor"))
        .args([
            "view",
            path.to_str().unwrap(),
            path.to_str().unwrap(),
            path.to_str().unwrap(),
            path.to_str().unwrap(),
        ])
        .env("AST_EDITOR_CACHE_DIR", store_for("stopped-reader"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to run ast-editor");

    {
        let mut reader = BufReader::new(child.stdout.take().expect("stdout was piped"));
        let mut first = String::new();
        reader.read_line(&mut first).expect("the first line");
    }

    let out = child
        .wait_with_output()
        .expect("failed to wait for ast-editor");
    assert!(
        out.status.success(),
        "exited {:?} when its reader stopped: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A broken pipe is the one write failure that means the reader left on
/// purpose, and it alone is a clean exit. Any other one means the answer did
/// not arrive and the call has to say so (D-01M28HCSAMTEFS). `/dev/full` is how
/// a write is made to fail without a pipe, and only Linux has it.
#[cfg(target_os = "linux")]
#[test]
fn a_write_that_fails_for_any_other_reason_exits_non_zero() {
    let full = std::fs::File::create("/dev/full").expect("/dev/full");
    let out = Command::new(env!("CARGO_BIN_EXE_ast-editor"))
        .arg("--help")
        .env("AST_EDITOR_CACHE_DIR", store_for("full-device"))
        .stdout(full)
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("failed to run ast-editor");

    assert_eq!(
        out.status.code(),
        Some(1),
        "a write that failed left the exit code at {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Silence used to mean two things: the file parsed, or nothing could read it.
/// An edit to a file no grammar covers says so; one that parsed stays silent.
#[test]
fn an_unchecked_edit_says_it_was_not_checked() {
    let prose = scratch("unchecked.txt", "one\ntwo\n");
    let said = edit_first_line("unchecked", &prose, "ONE");
    assert!(
        said.contains("\"syntax_valid\": null") && said.contains("without a syntax check"),
        "an edit nothing could check reported nothing about it: {said}"
    );

    let code = scratch("checked.rs", "fn a() {}\nfn b() {}\n");
    let said = edit_first_line("checked", &code, "fn renamed() {}");
    assert!(
        !said.contains("syntax_valid") && !said.contains("message"),
        "an edit that parsed said more than its ids: {said}"
    );
}

/// Replace a file's first line through the script form, returning the response.
fn edit_first_line(test: &str, file: &std::path::Path, replacement: &str) -> String {
    let listing = ast_editor(test, &["view", file.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&listing.stdout);
    let target = text
        .lines()
        .filter_map(|line| line.split('|').next())
        .map(str::trim)
        .find(|id| id.contains('#'))
        .expect("no line id")
        .to_string();

    let mut child = Command::new(env!("CARGO_BIN_EXE_ast-editor"))
        .args(["edit", file.to_str().unwrap()])
        .env("AST_EDITOR_CACHE_DIR", store_for(test))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to run ast-editor");
    {
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(format!("replace {target} ```\n{replacement}\n```\n").as_bytes())
            .unwrap();
    }
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// A schema that advertises an option the tool dropped gets it accepted and
/// then ignored, which is worse than refusing it.
#[test]
fn no_tool_offers_an_option_it_will_ignore() {
    for tool in ["inspect", "outline", "view", "edit", "create"] {
        let help = ast_editor("offered", &[tool, "--help"]);
        assert!(help.status.success());
        let text = String::from_utf8_lossy(&help.stdout);
        assert!(
            !text.contains("--output-file"),
            "{tool} still offers --output-file, which nothing reads: {text}"
        );
    }
}

/// A file no grammar covers has no parent contexts, which is an answer rather
/// than a failure. Asking anyway logged an error per call, twice, in the
/// stream a caller watches for real ones.
#[test]
fn reading_a_file_without_a_grammar_is_quiet() {
    let prose = scratch("quiet.txt", "one\ntwo\nthree\n");
    let out = ast_editor("quiet", &["view", prose.to_str().unwrap()]);
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "",
        "a successful read of a text file wrote to stderr"
    );
}

/// The two rejections mean different things and used to read the same, so a
/// caller treated both as "my ids are dead" and re-read everything.
#[test]
fn a_rejected_id_says_what_to_do_next() {
    let file = scratch("rejected.txt", "one\ntwo\nthree\n");
    let listing = ast_editor("rejected", &["view", file.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&listing.stdout);
    let held: Vec<String> = text
        .lines()
        .filter_map(|line| line.split('|').next())
        .map(|id| id.trim().to_string())
        .filter(|id| id.contains('#'))
        .collect();

    let gone = refuse("rejected", &file, "ff#0000");
    assert!(
        gone.contains("gone") && gone.contains("nothing to re-read"),
        "an id that names no line should say so: {gone}"
    );

    std::fs::write(&file, "one\nCHANGED\nthree\n").unwrap();
    let changed = refuse("rejected", &file, &held[1]);
    assert!(
        changed.contains("changed") && changed.contains("Read that"),
        "a changed line should say to read it again: {changed}"
    );
}

/// Try to replace `target` and return what the refusal said.
fn refuse(test: &str, file: &std::path::Path, target: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ast-editor"))
        .args(["edit", file.to_str().unwrap()])
        .env("AST_EDITOR_CACHE_DIR", store_for(test))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to run ast-editor");
    {
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(format!("replace {target} ```\nX\n```\n").as_bytes())
            .unwrap();
    }
    let out = child.wait_with_output().unwrap();
    assert!(!out.status.success(), "the edit was not refused");
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// Agents reach for `sed -n '10,40p'`, so a range after the path means the
/// same thing here. The flags keep working.
#[test]
fn a_range_can_follow_the_path() {
    let body: String = (1..=20).map(|n| format!("line-{n}\n")).collect();
    let file = scratch("ranged.txt", &body);
    let path = file.to_str().unwrap();

    for (range, start, end) in [("5,8", 5, 8), ("18", 18, 20), ("18,", 18, 20), (",3", 1, 3)] {
        let out = ast_editor("ranged", &["view", path, range]);
        assert!(
            out.status.success(),
            "{range}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let data = json_block(&out.stdout);
        assert_eq!(data["showing_start"].as_u64().unwrap(), start, "{range}");
        assert_eq!(data["showing_end"].as_u64().unwrap(), end, "{range}");
    }

    // A path is never read as a range. `view` takes several files, so a
    // second path there is a second file; a tool that takes one says so.
    let both = ast_editor("ranged", &["view", path, path]);
    assert!(both.status.success());
    assert_eq!(
        json_block(&both.stdout)["files"].as_array().unwrap().len(),
        2
    );

    let one_only = ast_editor("ranged", &["outline", path, path]);
    assert!(!one_only.status.success());
    assert!(String::from_utf8_lossy(&one_only.stderr).contains("takes one file"));
}

/// Skimming a directory was a shell loop over sed, because view refused a
/// second path. Several paths answer with one block each.
#[test]
fn several_files_in_one_call() {
    let first = scratch("several_one.rs", "fn a() {}\nfn b() {}\n");
    let second = scratch("several_two.py", "x = 1\ny = 2\n");

    let out = ast_editor(
        "several",
        &["view", first.to_str().unwrap(), second.to_str().unwrap()],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("```rust"), "no rust block: {text}");
    assert!(text.contains("```python"), "no python block: {text}");

    let data = json_block(&out.stdout);
    let files = data["files"].as_array().expect("a files array");
    assert_eq!(files.len(), 2);
    assert!(files[0]["filepath"].as_str().unwrap().ends_with("one.rs"));
    assert!(files[1]["filepath"].as_str().unwrap().ends_with("two.py"));

    // One file answers as it always has.
    let alone = ast_editor("severalone", &["view", first.to_str().unwrap()]);
    let data = json_block(&alone.stdout);
    assert!(
        data["files"].is_null(),
        "one file grew a files array: {data}"
    );
    assert_eq!(data["total_lines"].as_u64().unwrap(), 2);

    // A range still reads as a range, and applies to both.
    let ranged = ast_editor(
        "severalrange",
        &[
            "view",
            first.to_str().unwrap(),
            second.to_str().unwrap(),
            "2",
        ],
    );
    let data = json_block(&ranged.stdout);
    for file in data["files"].as_array().unwrap() {
        assert_eq!(file["showing_start"].as_u64().unwrap(), 2);
    }

    // A file that is not there fails the call and names itself.
    let missing = ast_editor(
        "severalmissing",
        &["view", first.to_str().unwrap(), "/nonexistent/file.rs"],
    );
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("/nonexistent/file.rs"));
}

/// `move` is arithmetic over indexes that shift under it. The block is drained
/// before the destination is resolved, so a destination past the block moves
/// back by the block's length and one before it does not; `after` is one past
/// `before`; and a block goes in in its own order. One line in one direction
/// exercises none of that.
#[test]
fn a_move_lands_where_it_was_addressed() {
    let start = "one\ntwo\nthree\nfour\nfive\nsix\n";
    let cases = [
        (vec![0], "after", 3, "two\nthree\nfour\none\nfive\nsix\n"),
        (vec![4], "before", 1, "one\nfive\ntwo\nthree\nfour\nsix\n"),
        (vec![0, 1], "after", 4, "three\nfour\nfive\none\ntwo\nsix\n"),
        (
            vec![3, 4],
            "before",
            0,
            "four\nfive\none\ntwo\nthree\nsix\n",
        ),
    ];

    for (n, (block, position, dest, want)) in cases.iter().enumerate() {
        let test = format!("move{n}");
        let file = scratch(&format!("move{n}.rs"), start);
        let ids = ast_editor(&test, &["view", file.to_str().unwrap(), "--only-ids"]);
        assert!(
            ids.status.success(),
            "{}",
            String::from_utf8_lossy(&ids.stderr)
        );
        let meta = json_block(&ids.stdout);
        let id = |row: usize| meta["lines"][row][0].as_str().unwrap().to_string();

        let address = match block[..] {
            [only] => id(only),
            [first, last] => format!("{},{}", id(first), id(last)),
            _ => unreachable!("a case addresses one line or a span"),
        };
        let script = format!("move {address} {position} {}\n", id(*dest));

        let mut child = Command::new(env!("CARGO_BIN_EXE_ast-editor"))
            .args(["edit", file.to_str().unwrap()])
            .env("AST_EDITOR_CACHE_DIR", store_for(&test))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("failed to run ast-editor");
        {
            use std::io::Write;
            child
                .stdin
                .take()
                .expect("stdin was piped")
                .write_all(script.as_bytes())
                .expect("failed to write the script");
        }
        let out = child.wait_with_output().expect("failed to wait");
        assert!(
            out.status.success(),
            "{script}exited {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            *want,
            "after {script}"
        );

        // The answer is addressed to the next edit, so the ids it names have to
        // be the lines that moved, where they now are.
        let moved = json_block(&out.stdout);
        let named: Vec<&str> = moved["modified_lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pair| pair[0].as_str().unwrap())
            .collect();
        let after = ast_editor(&test, &["view", file.to_str().unwrap(), "--only-ids"]);
        let rows = json_block(&after.stdout);
        let landed: Vec<String> = (0..block.len())
            .map(|offset| {
                let row = want
                    .lines()
                    .position(|l| l == start.lines().nth(block[offset]).unwrap());
                rows["lines"][row.unwrap()][0].as_str().unwrap().to_string()
            })
            .collect();
        assert_eq!(named, landed, "modified_lines after {script}");
    }
}

/// `replace_substring` is published in the schema and reachable only as JSON.
/// It cuts a line at the nth occurrence of a pattern and rejoins it around the
/// replacement, so what it gets wrong is which occurrence and where the tail
/// resumes, and an occurrence it cannot find has to be refused rather than
/// guessed at.
#[test]
fn replace_substring_takes_the_occurrence_it_was_given() {
    let start = "let a = foo(foo(1));\nlet b = 2;\n";
    let cases = [
        // (occurrence, expected first line, or None when the edit is refused)
        (None, Some("let a = bar(foo(1));")),
        (Some(1), Some("let a = bar(foo(1));")),
        (Some(2), Some("let a = foo(bar(1));")),
        (Some(3), None),
        (Some(0), None),
    ];

    for (n, (occurrence, want)) in cases.iter().enumerate() {
        let test = format!("substr{n}");
        let file = scratch(&format!("substr{n}.rs"), start);
        let ids = ast_editor(&test, &["view", file.to_str().unwrap(), "--only-ids"]);
        let id = json_block(&ids.stdout)["lines"][0][0]
            .as_str()
            .unwrap()
            .to_string();
        let occurrence_field = match occurrence {
            Some(k) => format!(r#","occurrence":{k}"#),
            None => String::new(),
        };
        let json = format!(
            r#"{{"edits":[{{"op":"replace_substring","start_id":"{id}","pattern":"foo","replacement":"bar"{occurrence_field}}}]}}"#
        );
        let out = ast_editor(&test, &["edit", file.to_str().unwrap(), "--json", &json]);

        let content = std::fs::read_to_string(&file).unwrap();
        match want {
            Some(line) => {
                assert!(
                    out.status.success(),
                    "occurrence {occurrence:?} was refused: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                assert_eq!(content, format!("{line}\nlet b = 2;\n"));
            }
            None => {
                assert!(
                    !out.status.success(),
                    "occurrence {occurrence:?} was accepted and wrote: {content}"
                );
                assert_eq!(content, start, "a refused edit wrote to the file");
            }
        }
    }
}

/// An edit script through stdin, which is how `edit` is driven from a shell.
fn run_script(test: &str, file: &std::path::Path, script: &str) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ast-editor"))
        .args(["edit", file.to_str().unwrap()])
        .env("AST_EDITOR_CACHE_DIR", store_for(test))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to run ast-editor");
    {
        use std::io::Write;
        child
            .stdin
            .take()
            .expect("stdin was piped")
            .write_all(script.as_bytes())
            .expect("failed to write the script");
    }
    child.wait_with_output().expect("failed to wait")
}

/// The ids of a file's lines, in order.
fn line_ids(test: &str, file: &std::path::Path) -> Vec<String> {
    let out = ast_editor(test, &["view", file.to_str().unwrap(), "--only-ids"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    json_block(&out.stdout)["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pair| pair[0].as_str().unwrap().to_string())
        .collect()
}

/// A span runs from its first line to its last. Addressed the other way round
/// it is refused rather than quietly reversed or emptied, and a destination
/// inside the block a `move` is lifting is refused for the same reason: there
/// is no answer to give, and writing one would be a guess at what was meant.
#[test]
fn a_span_addressed_backwards_is_refused() {
    let start = "one\ntwo\nthree\nfour\n";
    let scripts = [
        "replace {1},{0} ```\nX\n```",
        "delete {2},{1}",
        "move {2},{1} after {3}",
        "move {0},{1} after {1}",
    ];

    for (n, shape) in scripts.iter().enumerate() {
        let test = format!("backwards{n}");
        let file = scratch(&format!("backwards{n}.rs"), start);
        let ids = line_ids(&test, &file);
        let script = shape
            .replace("{0}", &ids[0])
            .replace("{1}", &ids[1])
            .replace("{2}", &ids[2])
            .replace("{3}", &ids[3]);
        let out = run_script(&test, &file, &format!("{script}\n"));
        assert!(
            !out.status.success(),
            "accepted {script:?} and wrote: {}",
            std::fs::read_to_string(&file).unwrap()
        );
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            start,
            "a refused edit wrote to the file: {script:?}"
        );
    }
}

/// A span of one line is one line, however it was spelled: the same id at both
/// ends, or an `end_id` that is present and empty. An empty string is not an
/// address, so it means the same as saying nothing.
#[test]
fn a_span_of_one_line_is_one_line() {
    let start = "one\ntwo\nthree\nfour\n";

    let file = scratch("span_same_id.rs", start);
    let ids = line_ids("spansame", &file);
    let out = run_script(
        "spansame",
        &file,
        &format!("replace {},{} ```\nX\n```\n", ids[1], ids[1]),
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "one\nX\nthree\nfour\n"
    );

    for (n, op) in ["delete", "move"].iter().enumerate() {
        let test = format!("spanempty{n}");
        let file = scratch(&format!("span_empty_end{n}.rs"), start);
        let ids = line_ids(&test, &file);
        let extra = if *op == "move" {
            format!(r#","dest_id":"{}","move_position":"after""#, ids[3])
        } else {
            String::new()
        };
        let json = format!(
            r#"{{"edits":[{{"op":"{op}","start_id":"{}","end_id":""{extra}}}]}}"#,
            ids[1]
        );
        let out = ast_editor(&test, &["edit", file.to_str().unwrap(), "--json", &json]);
        assert!(
            out.status.success(),
            "{op} with an empty end_id was refused: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let want = if *op == "move" {
            "one\nthree\nfour\ntwo\n"
        } else {
            "one\nthree\nfour\n"
        };
        assert_eq!(std::fs::read_to_string(&file).unwrap(), want, "after {op}");
    }
}

/// A delete answers with the line now nearest the hole, so the next edit has a
/// live id to anchor on. Taking the last line out leaves the hole past the end,
/// and the nearest line is then the one before it.
#[test]
fn deleting_the_last_line_answers_with_the_new_last_line() {
    let file = scratch("delete_tail.rs", "one\ntwo\nthree\n");
    let ids = line_ids("deltail", &file);
    let out = run_script("deltail", &file, &format!("delete {}\n", ids[2]));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "one\ntwo\n");

    let named = json_block(&out.stdout)["modified_lines"][0][0]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(named, ids[1], "the answer did not name the new last line");
}

/// Deleting a line in the middle leaves the hole where that line was, so the
/// line now nearest it is the one that moved up into the gap — not the last
/// line of the file, which is only nearest when the hole is past the end.
#[test]
fn deleting_a_line_answers_with_the_line_that_took_its_place() {
    let file = scratch("delete_middle.rs", "one\ntwo\nthree\nfour\n");
    let before = line_ids("delmid", &file);
    let out = run_script("delmid", &file, &format!("delete {}\n", before[1]));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "one\nthree\nfour\n"
    );

    let named = json_block(&out.stdout)["modified_lines"][0][0]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(named, before[2], "the answer did not name the line below");
}

/// `delete a,a` addresses one line twice, which is a span of one and not an
/// error: the ends of a span may meet.
#[test]
fn a_span_may_begin_and_end_on_the_same_line() {
    let file = scratch("span_meets.rs", "one\ntwo\nthree\n");
    let ids = line_ids("spanmeet", &file);
    let out = run_script(
        "spanmeet",
        &file,
        &format!("delete {},{}\n", ids[1], ids[1]),
    );
    assert!(
        out.status.success(),
        "delete refused a span of one: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "one\nthree\n");
}

/// An insert with no line to act on has one place to go, and which end it is
/// depends on the direction the caller asked for. `insert_before` with nothing
/// named puts the content at the top.
#[test]
fn an_insert_with_no_line_named_goes_to_the_end_it_faces() {
    for (op, want) in [
        ("insert_before", "X\none\ntwo\n"),
        ("insert_after", "one\ntwo\nX\n"),
    ] {
        let test = format!("notarget_{op}");
        let file = scratch(&format!("no_target_{op}.rs"), "one\ntwo\n");
        let json = format!(r#"{{"edits":[{{"op":"{op}","content":"X\n"}}]}}"#);
        let out = ast_editor(&test, &["edit", file.to_str().unwrap(), "--json", &json]);
        assert!(
            out.status.success(),
            "{op} with no start_id was refused: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), want, "after {op}");
    }
}

/// An insert that names a line acts at that line, and the two directions put
/// the content on either side of it. Nothing about the file's ends is involved.
#[test]
fn an_insert_acts_at_the_line_it_names() {
    for (directive, want) in [
        ("insert_before", "one\nX\ntwo\nthree\n"),
        ("insert_after", "one\ntwo\nX\nthree\n"),
    ] {
        let test = format!("at_line_{directive}");
        let file = scratch(&format!("at_line_{directive}.rs"), "one\ntwo\nthree\n");
        let ids = line_ids(&test, &file);
        let out = run_script(
            &test,
            &file,
            &format!("{directive} {} ```\nX\n```\n", ids[1]),
        );
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            want,
            "after {directive}"
        );
    }
}

/// A read is capped at 800 lines, and the cap is where a read stops being what
/// was asked for: the answer has to say so, and it has to stop in the right
/// place. The boundary is the whole subject here — 800 lines is a full answer
/// and 801 is a capped one.
#[test]
fn a_read_past_the_cap_stops_at_it_and_says_so() {
    let long: String = (1..=1000)
        .map(|n| format!("let line{n} = {n};\n"))
        .collect();
    let file = scratch("thousand.rs", &long);
    let cap = 800;

    let read = |test: &str, range: Option<&str>| {
        let mut args = vec!["view", file.to_str().unwrap()];
        if let Some(range) = range {
            args.push(range);
        }
        args.push("--only-ids");
        let out = ast_editor(test, &args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let block = json_block(&out.stdout);
        let rows: Vec<u64> = block["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pair| pair[1].as_u64().unwrap())
            .collect();
        let said = block["message"].as_str().unwrap_or("").to_string();
        (rows, said)
    };

    // The whole file, which is more than the cap.
    let (rows, said) = read("capall", None);
    assert_eq!(rows.len(), cap, "the whole file was not capped");
    assert_eq!((rows[0], rows[cap - 1]), (1, cap as u64));
    assert!(said.contains("800"), "a capped read said nothing: {said:?}");

    // Exactly the cap, which is not capped.
    let (rows, said) = read("capexact", Some("101,900"));
    assert_eq!(rows.len(), cap, "exactly the cap should be answered whole");
    assert_eq!((rows[0], rows[cap - 1]), (101, 900));
    assert!(said.is_empty(), "a full answer warned anyway: {said:?}");

    // One past the cap, which is.
    let (rows, said) = read("capover", Some("101,901"));
    assert_eq!(rows.len(), cap, "one past the cap was not capped");
    assert_eq!(
        (rows[0], rows[cap - 1]),
        (101, 900),
        "the cap did not start where the request did"
    );
    assert!(said.contains("800"), "a capped read said nothing: {said:?}");

    // A range well inside the cap is untouched.
    let (rows, said) = read("capunder", Some("400,500"));
    assert_eq!(rows.len(), 101);
    assert_eq!((rows[0], rows[100]), (400, 500));
    assert!(said.is_empty(), "an uncapped read warned: {said:?}");
}

/// The warning is about lines withheld, not about the size of the request. A
/// file exactly as long as the cap is answered whole however wide the range
/// asked for was, so there is nothing to warn about.
#[test]
fn a_file_exactly_the_cap_is_answered_without_a_warning() {
    let cap = 800;
    let exact: String = (1..=cap).map(|n| format!("let line{n} = {n};\n")).collect();
    let file = scratch("eight_hundred.rs", &exact);
    let out = ast_editor(
        "capfile",
        &["view", file.to_str().unwrap(), "1,900", "--only-ids"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let block = json_block(&out.stdout);
    assert_eq!(block["lines"].as_array().unwrap().len(), cap);
    assert_eq!(
        block["message"].as_str().unwrap_or(""),
        "",
        "nothing was withheld and it warned anyway"
    );
}

/// A query answers with a window around each match, and the windows are merged
/// where they meet. What that arithmetic gets wrong is the edges: a window that
/// would start before the first line, one that would end past the last, and
/// whether two windows a single line apart are one answer or two.
#[test]
fn a_query_answers_with_merged_windows_around_its_matches() {
    let twenty: String = (1..=20).map(|n| format!("let line{n} = {n};\n")).collect();
    let file = scratch("twenty.rs", &twenty);

    let rows = |test: &str, query: &str, context: &str| -> Vec<u64> {
        let out = ast_editor(
            test,
            &[
                "view",
                file.to_str().unwrap(),
                "--query",
                query,
                "--context-lines",
                context,
                "--only-ids",
            ],
        );
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        json_block(&out.stdout)["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pair| pair[1].as_u64().unwrap())
            .collect()
    };

    // Far apart: two windows, and the gap between them is not answered.
    assert_eq!(
        rows("qfar", "line3 |line12 ", "1"),
        vec![2, 3, 4, 11, 12, 13]
    );

    // One line apart: the windows touch and become one run.
    assert_eq!(rows("qtouch", "line3 |line6 ", "1"), vec![2, 3, 4, 5, 6, 7]);

    // Two lines apart: they do not.
    assert_eq!(rows("qapart", "line3 |line8 ", "1"), vec![2, 3, 4, 7, 8, 9]);

    // A window that would begin before the first line begins at it.
    assert_eq!(rows("qhead", "line1 ", "1"), vec![1, 2]);
    assert_eq!(rows("qhead3", "line3 ", "3"), vec![1, 2, 3, 4, 5, 6]);

    // A window that would end past the last line ends at it.
    assert_eq!(rows("qtail", "line20 ", "1"), vec![19, 20]);
}

/// Windows that meet are one run and windows with a gap are two, and the
/// difference is visible only in the text: the same lines come back either way,
/// separated by an elision or not. So the elision is what says whether
/// anything was skipped between them.
#[test]
fn an_elision_marks_the_lines_a_query_skipped() {
    let file = scratch(
        "elide.rs",
        "fn alpha() {\n    let a = 1;\n    let b = 2;\n    let c = 3;\n}\n",
    );

    let text = |test: &str, query: &str| -> String {
        let out = ast_editor(
            test,
            &[
                "view",
                file.to_str().unwrap(),
                "--query",
                query,
                "--context-lines",
                "0",
            ],
        );
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    };

    let touching = text("elidenone", "let a|let b");
    assert!(
        !touching.contains("\n...\n"),
        "consecutive matches were separated: {touching}"
    );

    let gapped = text("elidegap", "let a|let c");
    assert!(
        gapped.contains("\n...\n"),
        "a skipped line was not marked: {gapped}"
    );
}

/// A read answers with what encloses the lines it printed, so a caller sees
/// which function it is looking into without reading around the window. The
/// range is the enclosing node's own, not the window's, and a function no
/// printed line falls inside is not named.
#[test]
fn a_read_names_what_encloses_the_lines_it_printed() {
    let file = scratch(
        "enclosed.rs",
        "fn alpha() {\n    let a = 1;\n    let b = 2;\n}\n\nfn beta() {\n    let c = 3;\n}\n",
    );
    let out = ast_editor(
        "enclosing",
        &[
            "view",
            file.to_str().unwrap(),
            "--query",
            "let b",
            "--context-lines",
            "0",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let contexts = json_block(&out.stdout)["enclosing_contexts"].clone();
    assert_eq!(
        contexts,
        serde_json::json!([{"name": "fn:alpha", "start": 1, "end": 4}]),
        "the enclosing range was not alpha's own"
    );
}

/// The line that closes a function is still inside it, and what follows the
/// function is not. A read of that one line names the function and nothing
/// else, which is the difference between asking what encloses a line and
/// asking what encloses the line after it.
#[test]
fn the_line_that_closes_a_function_is_inside_it() {
    let file = scratch(
        "closing.rs",
        "fn alpha() {\n    let a = 1;\n}\nconst X: u8 = 1;\n",
    );
    let out = ast_editor(
        "closing",
        &[
            "view",
            file.to_str().unwrap(),
            "--query",
            r"^\}",
            "--context-lines",
            "0",
            "--only-ids",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let block = json_block(&out.stdout);
    assert_eq!(
        block["lines"].as_array().unwrap().len(),
        1,
        "more than the closing line was printed"
    );
    let names: Vec<&str> = block["enclosing_contexts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|ctx| ctx["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["fn:alpha"]);
}
