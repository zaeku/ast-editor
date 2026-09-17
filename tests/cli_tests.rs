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

/// What `skill api` prints: one `## `name`` section per tool, each a
/// description and a table of parameters. `--help` names the tools too, but in
/// a sentence, and a test that parses prose fails when the prose is improved.
fn api_reference() -> String {
    let out = ast_editor("api_reference", &["skill", "api"]);
    assert!(
        out.status.success(),
        "skill api failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("skill api answers in UTF-8")
}

fn tool_names() -> Vec<String> {
    let names: Vec<String> = api_reference()
        .lines()
        .filter_map(|line| Some(line.strip_prefix("## `")?.strip_suffix('`')?.to_string()))
        .collect();
    assert!(!names.is_empty(), "skill api named no tool");
    names
}

/// One tool's parameters as `(name, description)`. A cell may carry an escaped
/// pipe — the type column lists a template's values that way — so the escapes
/// are put aside before the row is split on the column separator.
fn tool_options(tool: &str) -> Vec<(String, String)> {
    const ESCAPED: &str = "\u{0}";
    let reference = api_reference();
    let section = reference
        .split_once(&format!("## `{tool}`"))
        .unwrap_or_else(|| panic!("skill api prints no section for {tool}"))
        .1
        .split("\n## ")
        .next()
        .unwrap()
        .to_string();
    section
        .lines()
        .filter(|line| line.starts_with("| `"))
        .filter_map(|line| {
            let cells: Vec<String> = line
                .replace("\\|", ESCAPED)
                .trim_matches('|')
                .split('|')
                .map(|cell| cell.trim().replace(ESCAPED, "|"))
                .collect();
            let [name, _type, _required, description] = cells.as_slice() else {
                return None;
            };
            Some((
                name.trim_matches('`').to_string(),
                description.trim().to_string(),
            ))
        })
        .collect()
}

/// A fixture for one test, under a directory this process owns.
///
/// The number is the directory rather than the name, so a test that addresses
/// its fixture by basename still can. Two tests that pick one name share a
/// directory otherwise, run on their own threads, and the second one's content
/// lands under the first one's path — which is not a failure until there are
/// enough threads for the reads to interleave. A 44-vCPU machine has them and
/// a laptop does not, so it cost a mutation run rather than a test run.
fn scratch(name: &str, content: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir()
        .join(format!("ast-editor-cli-files-{}", std::process::id()))
        .join(NEXT.fetch_add(1, Ordering::SeqCst).to_string());
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

    // The header counts the grammars and the lines under it name them, so the
    // two have to agree or one of them is describing a different binary.
    let counted: usize = text
        .lines()
        .find_map(|line| line.strip_prefix("grammars "))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("--version does not count its grammars: {text}"));
    let listed: Vec<(&str, &str, Vec<&str>)> = text
        .lines()
        .skip(2)
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            Some((words.next()?, words.next()?, words.collect()))
        })
        .collect();
    assert_eq!(
        listed.len(),
        counted,
        "--version counted {counted} grammars and listed {}: {text}",
        listed.len()
    );

    // Each grammar carries the version Cargo.lock pinned for it, which is a
    // number rather than a word, and its own rather than its neighbour's.
    for (name, version, extensions) in &listed {
        assert!(
            version.split('.').count() >= 2
                && version.split('.').all(|part| part.parse::<u32>().is_ok()),
            "the grammar {name:?} reports {version:?} as a version: {text}"
        );
        // The extensions are what decides whether a file is parsed at all, so a
        // grammar that names none is compiled in and unreachable.
        assert!(
            !extensions.is_empty(),
            "the grammar {name:?} claims no extension: {text}"
        );
        assert!(
            extensions.iter().all(|extension| extension.starts_with('.')
                && extension.len() > 1
                && !extension[1..].contains('.')),
            "the grammar {name:?} reports {extensions:?} as extensions: {text}"
        );
    }
    // The grammars are pinned one crate at a time, so a version shared by most
    // of them is not eighteen coincidences: it is one lookup answering for
    // everyone. A few do share one, being built from a single crate.
    let mut seen: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for (_, version, _) in &listed {
        *seen.entry(version).or_default() += 1;
    }
    let (common, count) = seen
        .iter()
        .max_by_key(|(_, count)| **count)
        .map(|(version, count)| (*version, *count))
        .unwrap();
    assert!(
        count * 2 < listed.len(),
        "{count} of {} grammars report {common:?}, so they are not reporting their own: {text}",
        listed.len()
    );
}

/// A store that cannot be opened is not a failed read. The line ids go missing
/// and the answer still carries the code, and the reason lands on stderr where
/// a caller reading the answer on stdout will not confuse it for output — which
/// only happens if something is listening for it.
#[test]
fn a_store_that_will_not_open_leaves_the_read_standing_and_says_why() {
    let file = scratch("nostore.rs", "struct Point {\n    x: u8,\n}\n");
    // A plain file where the cache directory should be: creating it fails, and
    // it fails for a reason the binary did not choose.
    let blocked = scratch("blocked-store", "not a directory\n");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_ast-editor"))
        .args(["inspect", file.to_str().unwrap(), "--template", "classes"])
        .env("AST_EDITOR_CACHE_DIR", &blocked)
        .output()
        .expect("failed to run ast-editor");

    assert!(
        out.status.success(),
        "a read failed because the store did not open: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let block = json_block(&out.stdout);
    assert_eq!(block["match_count"], 1, "{block}");
    assert!(
        block["matches"][0]["start_id"].is_null(),
        "a read with no store answered with a line id: {block}"
    );

    let said = String::from_utf8_lossy(&out.stderr);
    assert!(
        said.contains("WARN") && said.contains("cache directory"),
        "nothing on stderr said the store could not be opened: {said}"
    );
}

/// `--no-x` turns a switch off, and the three shapes it can take are answered
/// differently: a switch is negated, an option that carries a value is refused
/// by name because there is nothing to negate, and a flag no tool has is the
/// unknown-option refusal that lists the ones it has.
#[test]
fn a_switch_is_turned_off_by_name_and_only_a_switch_is() {
    let file = scratch("negated.rs", "fn a() {}\nfn b() {}\n");
    let path = file.to_str().unwrap();

    // The last word wins, in both directions, so the flag is read rather than
    // its presence counted.
    let off = ast_editor("negoff", &["view", path, "--only-ids", "--no-only-ids"]);
    assert!(
        String::from_utf8_lossy(&off.stdout).contains("fn a() {}"),
        "--no-only-ids did not turn the switch off: {}",
        String::from_utf8_lossy(&off.stdout)
    );
    let on = ast_editor("negon", &["view", path, "--no-only-ids", "--only-ids"]);
    assert!(
        !String::from_utf8_lossy(&on.stdout).contains("fn a() {}"),
        "--only-ids after --no-only-ids did not turn it back on: {}",
        String::from_utf8_lossy(&on.stdout)
    );

    // An option that takes a value has nothing to negate, and saying so names
    // the option rather than leaving the next word to be read as its value.
    let out = ast_editor("negvalue", &["view", path, "--no-start-line", "2"]);
    let said = String::from_utf8_lossy(&out.stderr) + String::from_utf8_lossy(&out.stdout);
    assert!(
        said.contains("start-line") && said.contains("not a switch"),
        "--no- on an option with a value was not refused: {said}"
    );

    // A flag no tool has is the other refusal, which lists what it does have.
    let out = ast_editor("negunknown", &["view", path, "--no-nonsense"]);
    let said = String::from_utf8_lossy(&out.stderr) + String::from_utf8_lossy(&out.stdout);
    assert!(
        said.contains("--no-nonsense") && said.contains("--only-ids"),
        "an unknown --no- flag was not refused with the options that exist: {said}"
    );
}

/// `skill <topic>` refuses a topic it does not have by listing the ones it
/// does, because a reader who guessed wrong has no other way to find them and
/// the documents are compiled into the binary.
#[test]
fn a_skill_topic_it_does_not_have_is_refused_with_the_ones_it_does() {
    let out = ast_editor("skilltopic", &["skill", "nosuchtopic"]);
    assert!(!out.status.success(), "an unknown topic was served");
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(
        text.contains("nosuchtopic"),
        "the refusal does not name what was asked for: {text}"
    );
    for topic in ["api", "rust", "python", "usage"] {
        assert!(
            text.contains(topic),
            "the refusal does not offer {topic:?}: {text}"
        );
    }

    // Each offered topic answers, so the list is the list rather than a label.
    for topic in ["api", "rust"] {
        let out = ast_editor(&format!("skill_{topic}"), &["skill", topic]);
        assert!(
            out.status.success() && !out.stdout.is_empty(),
            "the offered topic {topic:?} does not answer: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
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
        .args(["edit", file.to_str().unwrap()])
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

    // One address is one line and a trailing comma is what runs to the end,
    // which is how `sed -n` reads the same four addresses.
    for (range, start, end) in [("5,8", 5, 8), ("18", 18, 18), ("18,", 18, 20), (",3", 1, 3)] {
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

    // One file answers as it always has: its own data block and nothing that
    // says which file it was, because there is only one. Counting the blocks is
    // the assertion — reading the first one would not see a summary appended
    // after it.
    let alone = ast_editor("severalone", &["view", first.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&alone.stdout);
    assert_eq!(
        text.lines().filter(|line| *line == "```json").count(),
        1,
        "one file answered with more than one data block: {text}"
    );
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
        let file = scratch(&format!("move{n}.txt"), start);
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
    run_script_with(test, file, script, &[])
}

/// The same, with options after the path.
fn run_script_with(
    test: &str,
    file: &std::path::Path,
    script: &str,
    options: &[&str],
) -> std::process::Output {
    let mut args = vec!["edit", file.to_str().unwrap()];
    args.extend_from_slice(options);
    let mut child = Command::new(env!("CARGO_BIN_EXE_ast-editor"))
        .args(args)
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

/// Two previews stand at once. Taking one is not a reason to drop another: a
/// caller may look at two batches before deciding which to commit, and each
/// `preview_id` is answered as if it will keep.
#[test]
fn a_second_preview_does_not_spend_the_first() {
    let file = scratch(
        "two_previews.rs",
        "fn a() {\n    let x = 1;\n    let y = 2;\n}\n",
    );
    let ids = line_ids("twoprev", &file);

    let take = |test: &str, id: &str, text: &str| -> String {
        let out = run_script_with(
            test,
            &file,
            &format!("replace {id} ```\n    {text}\n```\n"),
            &["--dry-run"],
        );
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        json_block(&out.stdout)["preview_id"]
            .as_str()
            .expect("a dry run answers with a preview_id")
            .to_string()
    };

    let first = take("twoprev", &ids[1], "let x = 9;");
    let second = take("twoprev", &ids[2], "let y = 9;");
    assert_ne!(first, second, "two previews share an id");

    // The first is applied after the second was taken, which is the order that
    // tells whether taking one swept the other.
    let out = ast_editor(
        "twoprev",
        &["edit", file.to_str().unwrap(), "--apply", &first],
    );
    assert!(
        out.status.success(),
        "the first preview was gone once a second was taken: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "fn a() {\n    let x = 9;\n    let y = 2;\n}\n"
    );
}

/// A file written with CRLF endings keeps them. An edit rewrites the whole
/// file, so a tool that forgot which ending the file used would change every
/// line of it while being asked to change one — a diff nobody asked for, in a
/// file someone else's tools also read.
#[test]
fn a_file_written_with_crlf_is_written_back_with_crlf() {
    let file = scratch("crlf.txt", "one\r\ntwo\r\nthree\r\n");
    let ids = line_ids("crlf", &file);
    let out = run_script(
        "crlf",
        &file,
        &format!("replace {} ```\nTWO\n```\n", ids[1]),
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "one\r\nTWO\r\nthree\r\n",
        "an edit to a CRLF file did not keep its endings"
    );

    // An inserted line takes the file's ending too, not the one the script
    // arrived with.
    let ids = line_ids("crlf", &file);
    let out = run_script(
        "crlf",
        &file,
        &format!("insert_after {} ```\nMIDDLE\n```\n", ids[0]),
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "one\r\nMIDDLE\r\nTWO\r\nthree\r\n",
        "a line inserted into a CRLF file did not take its endings"
    );

    // And a file with LF endings is not given CRLF for having been edited.
    let plain = scratch("lf.txt", "one\ntwo\n");
    let ids = line_ids("lf", &plain);
    let out = run_script("lf", &plain, &format!("replace {} ```\nTWO\n```\n", ids[1]));
    assert!(out.status.success());
    assert_eq!(std::fs::read_to_string(&plain).unwrap(), "one\nTWO\n");
}

/// A preview is taken, looked at, and applied. The looking is a call of its
/// own, so a read between the two must leave the preview standing — a preview
/// that a plain read invalidates cannot be used the way it is offered.
#[test]
fn a_preview_survives_a_read_taken_before_it_is_applied() {
    let file = scratch("previewed.rs", "fn a() {\n    let x = 1;\n}\n");
    let ids = line_ids("previewed", &file);
    let out = run_script_with(
        "previewed",
        &file,
        &format!("replace {} ```\n    let x = 2;\n```\n", ids[1]),
        &["--dry-run"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let preview = json_block(&out.stdout)["preview_id"]
        .as_str()
        .expect("a dry run answers with a preview_id")
        .to_string();

    // The read in between is the point: it opens the store and sweeps what has
    // expired, and this preview has not.
    let between = ast_editor("previewed", &["view", file.to_str().unwrap()]);
    assert!(between.status.success());

    let out = ast_editor(
        "previewed",
        &["edit", file.to_str().unwrap(), "--apply", &preview],
    );
    assert!(
        out.status.success(),
        "a preview taken one read ago was refused: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "fn a() {\n    let x = 2;\n}\n",
        "the applied preview did not write what it previewed"
    );
}

/// The number in a line id names the line, and a number is never handed out
/// twice for one file. It is a counter the file carries, not a position: it
/// keeps counting across calls, and a number that belonged to a deleted line
/// stays spent. Handing it out again would point an id a caller still holds at
/// a line it has never seen.
#[test]
fn a_line_number_is_spent_once_and_never_reissued() {
    let seq = |id: &str| -> i64 {
        let (number, _) = id.split_once('#').expect("an id is a number and a hash");
        i64::from_str_radix(number, 16).expect("the number is hex")
    };

    let file = scratch("counted.txt", "one\ntwo\nthree\n");
    let ids = line_ids("counted", &file);
    let start: Vec<i64> = ids.iter().map(|id| seq(id)).collect();
    assert_eq!(
        start,
        vec![1, 2, 3],
        "a new file is numbered from one: {ids:?}"
    );

    // Two lines in one batch take two numbers, not one twice.
    let out = run_script(
        "counted",
        &file,
        &format!("insert_after {} ```\nA\nB\n```\n", ids[0]),
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let minted: Vec<i64> = json_block(&out.stdout)["modified_lines"]
        .as_array()
        .expect("no modified_lines")
        .iter()
        .map(|pair| seq(pair[0].as_str().unwrap()))
        .collect();
    assert_eq!(
        minted.len(),
        2,
        "two inserted lines answered with {minted:?}"
    );
    assert_ne!(
        minted[0], minted[1],
        "two lines inserted at once share a number: {minted:?}"
    );
    let mut spent = start.clone();
    spent.extend(&minted);
    for number in &minted {
        assert!(
            *number > 3,
            "an inserted line took {number}, which a line of the file already had: {minted:?}"
        );
    }

    // The counter is the file's, so it survives the call that advanced it: a
    // second process reads it back rather than deriving it from what is there.
    // The line deleted is the highest-numbered one, which is the only case that
    // tells the two apart — while it is still there, the lines that survive
    // carry the counter's value between them.
    let gone = *minted.iter().max().expect("two numbers");
    let ids = line_ids("counted", &file);
    let target = ids
        .iter()
        .find(|id| seq(id) == gone)
        .expect("the line just inserted")
        .clone();
    let out = run_script("counted", &file, &format!("delete {target}\n"));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let ids = line_ids("counted", &file);
    let anchor = ids
        .iter()
        .find(|id| seq(id) == 1)
        .expect("the first line")
        .clone();
    let out = run_script(
        "counted",
        &file,
        &format!("insert_after {anchor} ```\nC\n```\n"),
    );
    let after: Vec<i64> = json_block(&out.stdout)["modified_lines"]
        .as_array()
        .expect("no modified_lines")
        .iter()
        .map(|pair| seq(pair[0].as_str().unwrap()))
        .collect();
    assert_eq!(after.len(), 1, "one inserted line answered with {after:?}");
    assert!(
        !spent.contains(&after[0]),
        "{} was handed out again after {spent:?}",
        after[0]
    );

    // And every number in the file is still its own.
    let numbers: Vec<i64> = line_ids("counted", &file)
        .iter()
        .map(|id| seq(id))
        .collect();
    let mut unique = numbers.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        unique.len(),
        numbers.len(),
        "a number names two lines: {numbers:?}"
    );
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

    let file = scratch("span_same_id.txt", start);
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
        let file = scratch(&format!("span_empty_end{n}.txt"), start);
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

/// A delete writes no line, so `modified_lines` is empty and what it did say is
/// carried by the run of lines that moved up into the hole. Taking the last
/// line out moves nothing, so there is no run and the answer names none.
#[test]
fn deleting_the_last_line_renumbers_nothing() {
    let file = scratch("delete_tail.txt", "one\ntwo\nthree\n");
    let ids = line_ids("deltail", &file);
    let out = run_script("deltail", &file, &format!("delete {}\n", ids[2]));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "one\ntwo\n");

    let answer = json_block(&out.stdout);
    assert_eq!(
        answer["modified_lines"].as_array().map(Vec::len),
        Some(0),
        "a delete wrote a line: {answer}"
    );
    assert!(
        answer["renumbered"].as_array().is_none_or(Vec::is_empty),
        "every surviving line kept its number: {answer}"
    );
}

/// Deleting a line in the middle moves every line below it up one, and the
/// answer names that run by its two ends.
#[test]
fn deleting_a_line_names_the_run_that_moved_up() {
    let file = scratch("delete_middle.txt", "one\ntwo\nthree\nfour\n");
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

    let answer = json_block(&out.stdout);
    let runs = answer["renumbered"].as_array().unwrap();
    assert_eq!(runs.len(), 1, "one hole moves one run: {answer}");
    assert_eq!(runs[0]["from"][0].as_str().unwrap(), before[2]);
    assert_eq!(runs[0]["from"][1].as_u64().unwrap(), 2);
    assert_eq!(runs[0]["to"][0].as_str().unwrap(), before[3]);
    assert_eq!(runs[0]["to"][1].as_u64().unwrap(), 3);
}

/// `delete a,a` addresses one line twice, which is a span of one and not an
/// error: the ends of a span may meet.
#[test]
fn a_span_may_begin_and_end_on_the_same_line() {
    let file = scratch("span_meets.txt", "one\ntwo\nthree\n");
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
        let file = scratch(&format!("no_target_{op}.txt"), "one\ntwo\n");
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
        let file = scratch(&format!("at_line_{directive}.txt"), "one\ntwo\nthree\n");
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

/// `create --return-ids` answers for every line it wrote, however many there
/// are: a caller that cannot see the ids of what it just wrote has to read the
/// file again to edit it. The response still has a size, so what bounds the
/// answer is bytes rather than lines, and reaching that bound is said out loud
/// — an answer that stops early without saying so reads as a file that ended.
#[test]
fn a_create_answers_for_every_line_until_the_response_is_full() {
    let dir = std::env::temp_dir().join(format!("ast-editor-cli-files-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    // Past the line cap and inside the byte cap: every line comes back.
    let path = dir.join("created_long.txt");
    let _ = std::fs::remove_file(&path);
    let content: String = (1..=900).map(|n| format!("line {n}\n")).collect();
    let out = ast_editor(
        "createlong",
        &[
            "create",
            path.to_str().unwrap(),
            "--content",
            &content,
            "--return-ids",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let block = json_block(&out.stdout);
    assert_eq!(block["total_lines"], 900, "{block}");
    assert_eq!(
        block["lines"].as_array().unwrap().len(),
        900,
        "a create withheld ids for lines it wrote: {block}"
    );
    assert!(
        block["message"].is_null(),
        "a create inside the byte cap warned: {block}"
    );

    // Past the byte cap: the answer stops and says that it did.
    let path = dir.join("created_huge.txt");
    let _ = std::fs::remove_file(&path);
    let content: String = (1..=4000).map(|n| format!("line {n}\n")).collect();
    let out = ast_editor(
        "createhuge",
        &[
            "create",
            path.to_str().unwrap(),
            "--content",
            &content,
            "--return-ids",
        ],
    );
    let block = json_block(&out.stdout);
    assert_eq!(block["total_lines"], 4000, "{block}");
    let answered = block["lines"].as_array().unwrap().len();
    assert!(
        answered > 0 && answered < 4000,
        "the byte cap withheld {answered} of 4000 lines: {block}"
    );
    let said = block["message"].as_str().unwrap_or("");
    assert!(
        said.contains("45,000"),
        "the answer stopped without saying why: {block}"
    );
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

/// The file on disk is the truth and the store is brought back to it. A line
/// that survived an outside change keeps the number it had, and lines that are
/// new get numbers of their own — one each, past everything the file has used,
/// because a caller holding an id from before the change must still be pointing
/// at the line it read.
#[test]
fn a_file_changed_outside_keeps_the_ids_of_what_survived() {
    let seq = |id: &str| -> i64 {
        let (number, _) = id.split_once('#').expect("an id is a number and a hash");
        i64::from_str_radix(number, 16).expect("the number is hex")
    };

    let file = scratch("outside.txt", "alpha\nbeta\n");
    let before = line_ids("outside", &file);
    assert_eq!(before.len(), 2);

    // Two lines appended and one inserted, by something that is not this tool.
    std::fs::write(&file, "alpha\nmiddle\nbeta\ngamma\n").unwrap();
    let after = line_ids("outside", &file);
    assert_eq!(
        after.len(),
        4,
        "the read did not follow the file: {after:?}"
    );

    // What survived kept its id, in the places the new file puts it.
    assert_eq!(after[0], before[0], "the first line was renumbered");
    assert_eq!(
        after[2], before[1],
        "the line that moved down was renumbered"
    );

    // The new lines took numbers of their own, past what the file had used.
    let minted = [seq(&after[1]), seq(&after[3])];
    assert_ne!(
        minted[0], minted[1],
        "two new lines share a number: {after:?}"
    );
    for number in minted {
        assert!(
            number > seq(&before[1]),
            "a new line took {number}, which the file had already used: {after:?}"
        );
    }

    // And an id taken before the change still edits the line it named.
    let out = run_script(
        "outside",
        &file,
        &format!("replace {} ```\nBETA\n```\n", before[1]),
    );
    assert!(
        out.status.success(),
        "an id from before the change was refused: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "alpha\nmiddle\nBETA\ngamma\n"
    );
}

/// Respacing a line does not make it a different line. The store keeps a second
/// hash that ignores whitespace, so a file reformatted under the store is
/// recognised line by line rather than read as every line having been replaced
/// — which would renumber all of them and void every id a caller holds.
#[test]
fn a_line_that_was_only_reindented_is_the_same_line() {
    let file = scratch(
        "respaced.rs",
        "fn f() {\n    let a = 1;\n    let b = 2;\n}\n",
    );
    let before = line_ids("respaced", &file);

    // The same four lines, every one of them indented differently by something
    // else, and the two middle lines swapped. Respacing alone would not
    // discriminate: a store handing the leftover ids out in order answers the
    // same numbers whether or not it looks at the content. Swapping makes the
    // content-matched answer the only one that keeps each id on its own line.
    std::fs::write(
        &file,
        "  fn f() {\n      let b = 2;\n      let a = 1;\n  }\n",
    )
    .unwrap();
    let after = line_ids("respaced", &file);

    let numbers = |ids: &[String]| -> Vec<String> {
        ids.iter()
            .map(|id| id.split_once('#').unwrap().0.to_string())
            .collect()
    };
    let n = numbers(&before);
    assert_eq!(
        numbers(&after),
        vec![n[0].clone(), n[2].clone(), n[1].clone(), n[3].clone()],
        "a reindented file did not carry its ids to where the lines went: {after:?}"
    );

    // The hash moved with the content, so the old id is refused and the new one
    // is not: the line is the same line, and the caller's view of it is stale.
    let out = run_script(
        "respaced",
        &file,
        &format!("replace {} ```\n    let a = 9;\n```\n", before[1]),
    );
    assert!(
        !out.status.success(),
        "an id whose line was respaced was accepted as unchanged"
    );
    let out = run_script(
        "respaced",
        &file,
        &format!("replace {} ```\n      let a = 9;\n```\n", after[2]),
    );
    assert!(
        out.status.success(),
        "the id just read was refused: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// An op that needs a field it was not given says which field it needs and
/// which ones the edit did carry. The second half is what makes the message
/// actionable: a caller building edits from a template learns whether the field
/// is missing or spelled wrong, without reading the reference again.
#[test]
fn an_op_missing_a_field_names_what_it_was_given() {
    let file = scratch("missing.txt", "one\ntwo\n");
    let ids = line_ids("missing", &file);
    let edits = serde_json::json!({
        "filepath": file.to_str().unwrap(),
        "edits": [{ "op": "replace_substring", "start_id": ids[0], "replacement": "X" }],
    });
    let out = ast_editor(
        "missing",
        &["edit", "--json", &serde_json::to_string(&edits).unwrap()],
    );
    assert!(
        !out.status.success(),
        "an edit missing a field was accepted"
    );
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(
        said.contains("needs pattern"),
        "the refusal does not name the field it needs: {said}"
    );
    assert!(
        said.contains("carries start_id, replacement"),
        "the refusal does not name the fields it was given: {said}"
    );

    // An edit carrying nothing else says so, rather than listing the fields it
    // does not have.
    let edits = serde_json::json!({
        "filepath": file.to_str().unwrap(),
        "edits": [{ "op": "replace_substring" }],
    });
    let out = ast_editor(
        "missing2",
        &["edit", "--json", &serde_json::to_string(&edits).unwrap()],
    );
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(
        said.contains("carries no other field"),
        "an edit with nothing else did not say so: {said}"
    );
}

/// Markdown has no grammar behind it, so what encloses a line is worked out
/// from its headings. A `#` only opens a heading when a space follows it, so
/// `#notaheading` is a word a paragraph begins with; treating it as a heading
/// would put every line under it inside a section that CommonMark says is not
/// there, and a read of those lines would name it.
#[test]
fn a_hash_with_no_space_after_it_opens_no_section() {
    let file = scratch(
        "headings.md",
        "#notaheading some text\n\nbody one\n\n# Real Heading\n\nbody two\n",
    );
    let read = |test: &str, range: &str| -> Vec<String> {
        let out = ast_editor(test, &["view", file.to_str().unwrap(), range]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        json_block(&out.stdout)["enclosing_contexts"]
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .map(|entry| entry["name"].as_str().unwrap_or("").to_string())
                    .collect()
            })
            .unwrap_or_default()
    };

    // The line after the word is in no section at all.
    assert_eq!(
        read("hd_body", "3,3"),
        Vec::<String>::new(),
        "a paragraph beginning with a hash opened a section"
    );

    // The line after the heading is in the heading's section, which is what
    // says the difference is the space and not the hash.
    assert_eq!(
        read("hd_real", "7,7"),
        vec!["# Real Heading".to_string()],
        "the line under a heading is not inside it"
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

/// A path in the scratch directory that nothing has written, for `create`,
/// which refuses a file that exists.
fn unwritten(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ast-editor-cli-files-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    let _ = std::fs::remove_file(&path);
    path
}

/// `create` answers with the ids of every line it wrote, however many that is
/// (D-01M2ATRFAMMMXD). The 800-line cap is for a read given no bounds; a caller
/// that just wrote the content and asked for its ids stated the amount, and
/// capping the answer would send it to read back a file it had just written.
#[test]
fn create_answers_with_every_line_it_wrote() {
    let make = |test: &str, name: &str, lines: usize| {
        let content: String = (1..=lines)
            .map(|n| format!("let line{n} = {n};\n"))
            .collect();
        let path = unwritten(name);
        let json = format!(
            r#"{{"filepath":"{}","content":{},"return_ids":true}}"#,
            path.display(),
            serde_json::to_string(&content).unwrap()
        );
        let out = ast_editor(test, &["create", "--json", &json]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let block = json_block(&out.stdout);
        let rows: Vec<u64> = block["lines"]
            .as_array()
            .map(|a| a.iter().map(|p| p[1].as_u64().unwrap()).collect())
            .unwrap_or_default();
        let said = block["message"].as_str().unwrap_or("").to_string();
        (rows, said)
    };

    // Past the read cap, which has nothing to do with this answer.
    let (rows, said) = make("createlong", "long_create.rs", 1000);
    assert_eq!(rows.len(), 1000, "the ids stopped short of the file");
    assert_eq!((rows[0], rows[999]), (1, 1000));
    assert!(said.is_empty(), "a whole answer warned anyway: {said:?}");

    // At the read cap.
    let (rows, said) = make("createexact", "at_cap_create.rs", 800);
    assert_eq!(rows.len(), 800);
    assert!(said.is_empty(), "a whole answer warned anyway: {said:?}");

    // No lines, no ids.
    let (rows, said) = make("createempty", "no_lines.rs", 0);
    assert!(rows.is_empty(), "an empty file answered with ids");
    assert!(said.is_empty(), "an empty file warned: {said:?}");
}

/// Every template the tool serves, against a file in the language it is for.
/// A template is a query the tool wrote, so a template that does not compile is
/// the tool shipping a broken query, and one that compiles but matches nothing
/// is a query for a node the grammar does not call that.
#[test]
fn every_template_finds_what_it_names() {
    let rust = "use a::b;\nstruct S;\ntrait T {}\nimpl S {}\nfn f() {}\n";
    let python =
        "import os\n\n\nclass C:\n    def m(self):\n        pass\n\n\ndef f():\n    pass\n";
    let go = "package main\n\nimport \"fmt\"\n\ntype S struct{ a int }\n\ntype I interface{ M() }\n\nfunc f() {}\n\nfunc (s S) m() {}\n";
    let ts = "import { a } from \"b\";\n\nclass C {\n    m() {}\n}\n\nfunction f() {}\n\nconst g = () => {};\n";
    let java = "import java.util.List;\n\nclass C {\n    void m() {}\n}\n\ninterface I {\n    void n();\n}\n";
    let c = "#include <stdio.h>\n#define M 1\n#define F(x) (x)\nstruct S { int a; };\nint f(void) { return 0; }\n";
    let cpp =
        "#include <stdio.h>\nclass C { int a; };\nstruct S { int b; };\nint f() { return 0; }\n";
    let swift = "import Foundation\n\nclass C {\n    func m() {}\n}\n\nfunc f() {}\n";
    let bash = "f() {\n  echo hi\n}\n";
    // Markdown has no grammar behind it, so its templates are a separate list
    // and a template off that list is answered with a warning rather than a
    // match of zero.
    let markdown = concat!(
        "# Title\n\n",
        "Text with a [link](http://x).\n\n",
        "## Second\n\n",
        "| a | b |\n|---|---|\n| 1 | 2 |\n\n",
        "- a\n- b\n\n",
        "```rust\nfn f() {}\n```\n",
    );

    // (file name, source, template, matches expected)
    let cases: &[(&str, &str, &str, usize)] = &[
        ("t.rs", rust, "functions", 1),
        ("t.rs", rust, "classes", 1),
        ("t.rs", rust, "imports", 1),
        ("t.rs", rust, "traits", 1),
        ("t.rs", rust, "impls", 1),
        ("t.py", python, "functions", 2),
        ("t.py", python, "classes", 1),
        ("t.py", python, "imports", 1),
        ("t.go", go, "functions", 2),
        ("t.go", go, "classes", 2),
        ("t.go", go, "imports", 1),
        ("t.go", go, "interfaces", 1),
        ("t.go", go, "structs", 1),
        ("t.ts", ts, "functions", 3),
        ("t.ts", ts, "classes", 1),
        ("t.ts", ts, "imports", 1),
        ("T.java", java, "functions", 2),
        ("T.java", java, "classes", 2),
        ("T.java", java, "imports", 1),
        ("t.c", c, "functions", 1),
        ("t.c", c, "classes", 1),
        ("t.c", c, "imports", 1),
        ("t.c", c, "macros", 2),
        ("t.cpp", cpp, "functions", 1),
        ("t.cpp", cpp, "classes", 2),
        ("t.cpp", cpp, "imports", 1),
        ("t.swift", swift, "functions", 2),
        ("t.swift", swift, "classes", 1),
        ("t.swift", swift, "imports", 1),
        ("t.sh", bash, "functions", 1),
        ("t.md", markdown, "headings", 2),
        ("t.md", markdown, "headers", 2),
        ("t.md", markdown, "codeblocks", 1),
        ("t.md", markdown, "code_blocks", 1),
        ("t.md", markdown, "links", 1),
        ("t.md", markdown, "tables", 1),
        ("t.md", markdown, "lists", 1),
    ];

    for (name, source, template, want) in cases {
        let file = scratch(name, source);
        let test = format!("tpl_{name}_{template}");
        let out = ast_editor(
            &test,
            &["inspect", file.to_str().unwrap(), "--template", template],
        );
        assert!(
            out.status.success(),
            "--template {template} on {name} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let block = json_block(&out.stdout);
        assert_eq!(
            block["match_count"].as_u64().unwrap() as usize,
            *want,
            "--template {template} on {name} used {} and matched {} of {want}",
            block["query"].as_str().unwrap_or("?"),
            block["match_count"]
        );
        assert!(
            block.get("hint").is_none() && block.get("status").is_none(),
            "--template {template} on {name} is supported and still warned: {block}"
        );
    }

    // A template this language has no answer for is a warning that names the
    // ones it does, not an empty match list a caller would read as an absence.
    let file = scratch("t.md", markdown);
    let out = ast_editor(
        "tpl_md_unsupported",
        &["inspect", file.to_str().unwrap(), "--template", "functions"],
    );
    let block = json_block(&out.stdout);
    assert_eq!(block["status"], "warning", "{block}");
    let hint = block["hint"].as_str().unwrap_or("");
    for named in ["headings", "codeblocks", "links", "tables", "lists"] {
        assert!(
            hint.contains(named),
            "the hint does not name {named:?} as supported: {hint}"
        );
    }
}

/// A definition spans lines, and both tools that report one report where it
/// ends as well as where it begins: `outline` for everything a file declares,
/// and `inspect` for the one kind asked about. The range is what `view` is
/// given next, so a definition that reports its own first line as its last is
/// a read of one line out of a block.
#[test]
fn a_definition_reports_the_lines_it_spans() {
    let source = "struct Point {\n    x: u8,\n    y: u8,\n}\n\nfn main() {\n    let p = 1;\n}\n";
    let file = scratch("spans.rs", source);

    let out = ast_editor("spans_outline", &["outline", file.to_str().unwrap()]);
    let block = json_block(&out.stdout);
    let outline = block["outline"].as_array().unwrap();
    let ranges: Vec<(u64, u64)> = outline
        .iter()
        .map(|e| {
            (
                e["start_line"].as_u64().unwrap(),
                e["end_line"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        ranges,
        vec![(1, 4), (6, 8)],
        "the outline does not span the definitions it found: {block}"
    );

    // The same two numbers again from `inspect`, and again inside the
    // definition it reports beside them, which is what names the block of code
    // the answer carries.
    let out = ast_editor(
        "spans_inspect",
        &["inspect", file.to_str().unwrap(), "--template", "classes"],
    );
    let block = json_block(&out.stdout);
    let m = &block["matches"][0];
    assert_eq!(m["start_line"], 1, "{block}");
    assert_eq!(m["end_line"], 4, "{block}");
    let definition = &m["definition"];
    assert!(
        !definition.is_null(),
        "a class match carries no definition: {block}"
    );
    assert_eq!(definition["start_line"], 1, "{block}");
    assert_eq!(definition["end_line"], 4, "{block}");
}

/// A line too long for one row is broken onto continuation rows that join back
/// to it exactly: the pieces concatenate to the line, the text of every row
/// starts in the same column so the block reads as one line, and the last row
/// is marked differently from the ones before it so a reader can see where the
/// line ends.
#[test]
fn a_long_line_is_broken_into_rows_that_join_back_to_it() {
    let long = "x".repeat(1000);
    let file = scratch("wrapped.rs", &format!("let a = 1;\n{long}\nlet b = 2;\n"));
    let out = ast_editor("wrapped", &["view", file.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    let block: Vec<&str> = text
        .lines()
        .skip_while(|line| !line.starts_with("```"))
        .skip(1)
        .take_while(|line| !line.starts_with("```"))
        .collect();

    // The head row for the long line, and every continuation under it.
    let head = block
        .iter()
        .position(|row| row.contains("|2: "))
        .expect("no row for line 2");
    let rows: Vec<&str> = block[head..]
        .iter()
        .take_while(|row| row.contains("|2: ") || row.contains("│: ") || row.contains("└: "))
        .copied()
        .collect();
    // At least three, so that there is a row between the first and the last:
    // how wide a row is is a display choice and not what this checks.
    assert!(
        rows.len() >= 3,
        "a 1000-character line made {} rows",
        rows.len()
    );

    // The pieces are the line.
    // In characters, not bytes: the box-drawing marker is three bytes wide and
    // one column, and it is the column that has to line up.
    let column = rows[0][..rows[0].find("|2: ").unwrap() + "|2: ".len()]
        .chars()
        .count();
    let mut joined = String::new();
    for (n, row) in rows.iter().enumerate() {
        let marker = if n == 0 {
            "|2: "
        } else if n == rows.len() - 1 {
            "└: "
        } else {
            "│: "
        };
        let at = row.find(marker).unwrap_or_else(|| {
            panic!("row {n} is not marked {marker:?}: {row:?}");
        });
        assert_eq!(
            row[..at + marker.len()].chars().count(),
            column,
            "row {n} starts its text in a different column: {row:?}"
        );
        joined.push_str(&row[at + marker.len()..]);
    }
    assert_eq!(joined, long, "the rows do not join back to the line");
}

/// A match answers with the text of the node, which begins where the node
/// begins: a nested list item starts at its bullet and not at the whitespace
/// indenting it. The file is written in multi-byte characters because the text
/// is cut by byte offset, and a cut inside a character is not a string.
#[test]
fn a_match_answers_with_the_text_of_the_node() {
    let file = scratch(
        "nested.md",
        "# 제목\n\n- 한글 항목\n  - 중첩 항목\n    - 더 깊은 항목\n",
    );

    let heading = ast_editor(
        "nodetext",
        &["inspect", file.to_str().unwrap(), "--query", "Heading"],
    );
    assert!(
        heading.status.success(),
        "{}",
        String::from_utf8_lossy(&heading.stderr)
    );
    let found = json_block(&heading.stdout);
    assert_eq!(
        found["matches"][0]["text"].as_str().unwrap(),
        "# 제목",
        "a heading of multi-byte characters came back cut"
    );

    let lists = ast_editor(
        "nodetext2",
        &["inspect", file.to_str().unwrap(), "--query", "List"],
    );
    assert!(
        lists.status.success(),
        "{}",
        String::from_utf8_lossy(&lists.stderr)
    );
    let found = json_block(&lists.stdout);
    let texts: Vec<&str> = found["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["text"].as_str().unwrap())
        .collect();
    assert!(
        texts.len() >= 3,
        "three nested lists matched {} times",
        texts.len()
    );
    for text in &texts {
        assert!(
            text.starts_with("- "),
            "a list's text began before the list did: {text:?}"
        );
    }
}

/// An edit to markdown warns about a heading that skips a level, and the
/// warning names which levels: the one found, the one before it, and the one
/// missing between them. A warning whose numbers are wrong sends a reader to
/// the wrong heading.
#[test]
fn a_skipped_heading_level_is_named_by_the_levels_involved() {
    let file = scratch("headings.md", "# One\n\n### Three\n");
    let out = run_script("headings", &file, "append ```\ntext\n```\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let warnings = json_block(&out.stdout)["warnings"].clone();
    let said = warnings[0].as_str().unwrap_or("");
    assert!(
        said.contains("line 3")
            && said.contains("H3")
            && said.contains("H1")
            && said.contains("H2"),
        "the warning did not name the line and the three levels: {said:?}"
    );

    // A level at a time is not a skip, and the first heading has nothing above
    // it to skip from, however deep it is.
    for (name, content) in [
        ("steps.md", "# One\n\n## Two\n\n### Three\n"),
        ("deep_first.md", "### Three\n\n#### Four\n"),
    ] {
        let file = scratch(name, content);
        let out = run_script(&format!("h_{name}"), &file, "append ```\ntext\n```\n");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let warnings = json_block(&out.stdout)["warnings"].clone();
        let hierarchy: Vec<&str> = warnings
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|w| w.as_str())
                    .filter(|w| w.contains("Header hierarchy"))
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            hierarchy.is_empty(),
            "{name} warned about a hierarchy it keeps: {hierarchy:?}"
        );
    }
}

/// The same edit warns about a bracket that was opened and not closed, and
/// names the line it is on. A line with a whole link is not that.
#[test]
fn an_unclosed_bracket_is_named_by_its_line() {
    let file = scratch(
        "links.md",
        "# Title\n\nA whole [link](https://example.com) here.\n\nA broken [link(https://example.com) here.\n",
    );
    let out = run_script("links", &file, "append ```\ntext\n```\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let warnings = json_block(&out.stdout)["warnings"].clone();
    let said: Vec<&str> = warnings
        .as_array()
        .map(|a| a.iter().filter_map(|w| w.as_str()).collect())
        .unwrap_or_default();
    let about_links: Vec<&&str> = said
        .iter()
        .filter(|w| w.contains("malformed link"))
        .collect();
    assert_eq!(
        about_links.len(),
        1,
        "the whole link and the broken one were not told apart: {said:?}"
    );
    assert!(
        about_links[0].contains("line 5"),
        "the warning named the wrong line: {:?}",
        about_links[0]
    );
}

/// `move` takes four positions. Two of them name a destination and two of them
/// name an end of the file, so those two take no destination at all.
#[test]
fn a_move_takes_each_of_its_four_positions() {
    let start = "one\ntwo\nthree\nfour\n";
    let cases = [
        (2, "after 3", "one\ntwo\nfour\nthree\n"),
        (2, "before 0", "three\none\ntwo\nfour\n"),
        (2, "prepend", "three\none\ntwo\nfour\n"),
        (2, "append", "one\ntwo\nfour\nthree\n"),
    ];

    for (n, (block, tail, want)) in cases.iter().enumerate() {
        let test = format!("pos{n}");
        let file = scratch(&format!("pos{n}.txt"), start);
        let ids = line_ids(&test, &file);
        let script = tail
            .split(' ')
            .map(|word| match word.parse::<usize>() {
                Ok(row) => ids[row].clone(),
                Err(_) => word.to_string(),
            })
            .collect::<Vec<_>>()
            .join(" ");
        let out = run_script(&test, &file, &format!("move {} {script}\n", ids[*block]));
        assert!(
            out.status.success(),
            "move {tail} was refused: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            *want,
            "after move {tail}"
        );
    }
}

/// A directive given the wrong number of arguments is told which directive it
/// was and how that one is spelled. The usage line is the whole of the help a
/// script gets, so a wrong one sends the writer to the wrong shape.
#[test]
fn a_misused_directive_is_shown_its_own_usage() {
    let cases = [
        ("replace a b ```\nX\n```", "replace", "replace <id>"),
        (
            "insert_after a b ```\nX\n```",
            "insert_after",
            "insert_after <id>",
        ),
        (
            "insert_before a b ```\nX\n```",
            "insert_before",
            "insert_before <id>",
        ),
        ("append x ```\nX\n```", "append", "append ```"),
        ("prepend x ```\nX\n```", "prepend", "prepend ```"),
        ("delete a b", "delete", "delete <id>"),
        ("move a", "move", "move <start_id>"),
    ];

    for (n, (script, directive, usage)) in cases.iter().enumerate() {
        let file = scratch(&format!("usage{n}.txt"), "one\ntwo\n");
        let out = run_script(&format!("usage{n}"), &file, &format!("{script}\n"));
        assert!(
            !out.status.success(),
            "{directive:?} misused was accepted: {}",
            std::fs::read_to_string(&file).unwrap()
        );
        let said = String::from_utf8_lossy(&out.stderr);
        assert!(
            said.contains(&format!("'{directive}'")),
            "the error did not name {directive:?}: {said}"
        );
        assert!(
            said.contains(usage),
            "the error did not carry {directive:?}'s usage {usage:?}: {said}"
        );
    }
}

/// A directive nobody serves is named as unknown rather than as a known one
/// used wrongly, and the answer lists what is known. The two messages send a
/// writer to different places: one to the spelling of an operation, the other
/// to the list of them.
#[test]
fn an_unknown_directive_is_named_as_unknown() {
    let file = scratch("unknown.txt", "one\ntwo\n");
    let out = run_script("unknown", &file, "zzz a\n");
    assert!(!out.status.success());
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(
        said.contains("unknown operation 'zzz'"),
        "an unserved directive was not named as unknown: {said}"
    );
    assert!(
        said.contains("replace") && said.contains("move"),
        "the answer did not list what is known: {said}"
    );
}

/// An address is one id or two separated by one comma. Anything else is
/// refused: a span with an end and no start, a start and no end, or more
/// commas than an address has places for.
#[test]
fn a_malformed_address_is_refused() {
    for (n, address) in [",2#abcd", "1#abcd,", "1#abcd,2#abcd,3#abcd"]
        .iter()
        .enumerate()
    {
        let file = scratch(&format!("addr{n}.txt"), "one\ntwo\n");
        let out = run_script(&format!("addr{n}"), &file, &format!("delete {address}\n"));
        assert!(
            !out.status.success(),
            "{address:?} was accepted as an address: {}",
            std::fs::read_to_string(&file).unwrap()
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("as an address"),
            "{address:?} was refused for some other reason: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// A script is directives and payloads, and a blank line or a comment is
/// neither: they are skipped rather than read as an operation.
#[test]
fn a_script_skips_blank_lines_and_comments() {
    let file = scratch("commented.txt", "one\ntwo\n");
    let ids = line_ids("commented", &file);
    let script = format!(
        "# what this does\n\ndelete {}\n\n# and nothing after it\n",
        ids[0]
    );
    let out = run_script("commented", &file, &script);
    assert!(
        out.status.success(),
        "a comment or a blank line was read as a directive: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "two\n");
}

/// `prepend` puts its payload at the top of the file, which is the only thing
/// that distinguishes it from `append`.
#[test]
fn prepend_puts_its_payload_at_the_top() {
    let file = scratch("prepended.txt", "one\ntwo\n");
    let out = run_script("prepended", &file, "prepend ```\nzero\n```\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "zero\none\ntwo\n");
}

/// `replace_substring` is an operation the tool serves and the script format
/// cannot carry, since it takes a pattern and a replacement rather than a
/// payload. A script naming it is told where to find it rather than told it
/// does not exist: one of those sends the writer to the JSON form and the
/// other sends them looking for a typo.
#[test]
fn a_script_naming_replace_substring_is_sent_to_the_json_form() {
    let file = scratch("substr_script.txt", "one\ntwo\n");
    let out = run_script("substrscript", &file, "replace_substring 1#abcd foo bar\n");
    assert!(!out.status.success());
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(
        said.contains("'replace_substring'") && said.contains("JSON"),
        "a script naming replace_substring was not sent to the JSON form: {said}"
    );
    assert!(
        !said.contains("unknown operation"),
        "an operation the tool serves was called unknown: {said}"
    );
}

/// `<tool> --help` is the only place a caller reads what an option means, and
/// it carries each description whole. The text is broken across lines to fit
/// beside the flag column, so the check is on the words rather than on the
/// layout: every description in the schema appears in the help, unbroken once
/// the wrapping is undone.
#[test]
fn a_tool_help_carries_each_option_description_whole() {
    let flat = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");

    for name in tool_names() {
        let out = ast_editor(&format!("desc_{name}"), &[&name, "--help"]);
        assert!(
            out.status.success(),
            "{name} --help failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let help = flat(&String::from_utf8_lossy(&out.stdout));

        let options = tool_options(&name);
        assert!(
            !options.is_empty(),
            "{name} has no described options to check"
        );
        for (field, description) in options {
            assert!(
                help.contains(&flat(&description)),
                "{name} --help does not carry {field}'s description whole:\n  \
                 wanted {:?}\n  in {help:?}",
                flat(&description)
            );
        }
    }
}

/// A boolean option is true by being named, and there are three other ways to
/// say it: `--flag true`, `--flag false`, and `--no-flag`. The word after the
/// flag is consumed when it is one of those, and left alone when it is not —
/// otherwise the next argument would be read as the flag's value.
#[test]
fn a_boolean_option_is_written_four_ways() {
    let file = scratch("bools.rs", "fn main() {\n    let a = 1;\n}\n");
    let path = file.to_str().unwrap().to_string();
    let ids_shown = |test: &str, args: &[&str]| -> bool {
        let mut all = vec!["view", path.as_str()];
        all.extend_from_slice(args);
        let out = ast_editor(test, &all);
        assert!(
            out.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        // only_ids answers with the ids alone and no line text.
        json_block(&out.stdout).get("lines").is_some()
    };

    assert!(
        ids_shown("bool_bare", &["--only-ids"]),
        "a named flag was not taken as true"
    );
    assert!(
        ids_shown("bool_true", &["--only-ids", "true"]),
        "--only-ids true was not true"
    );
    assert!(
        !ids_shown("bool_false", &["--only-ids", "false"]),
        "--only-ids false was not false"
    );
    assert!(
        !ids_shown("bool_no", &["--no-only-ids"]),
        "--no-only-ids was not false"
    );

    // A word that is not true or false belongs to whatever comes next, and here
    // there is nothing next, so it is a path and the call fails on it.
    let out = ast_editor("bool_other", &["view", &path, "--only-ids", "maybe"]);
    assert!(
        !out.status.success(),
        "'maybe' was swallowed as the flag's value"
    );

    // `--no-` on something that is not an option is not a negation.
    let out = ast_editor("bool_nonsense", &["view", &path, "--no-such-thing"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--no-such-thing"),
        "the error did not name the option: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A range after the path is read the way `sed -n '40,80p'` reads one, and what
/// is not a range is a path. Line numbers begin at one, so `0,2` is not a
/// range at all, and
/// the tool looks for a file by that name rather than reading from a line that
/// cannot exist.
#[test]
fn what_is_not_a_range_is_a_path() {
    let file = scratch("ranged.rs", "a\nb\nc\nd\ne\n");
    let rows = |test: &str, arg: &str| -> Vec<u64> {
        let out = ast_editor(test, &["view", file.to_str().unwrap(), arg, "--only-ids"]);
        assert!(
            out.status.success(),
            "{arg:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        json_block(&out.stdout)["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pair| pair[1].as_u64().unwrap())
            .collect()
    };

    assert_eq!(rows("range_both", "2,4"), vec![2, 3, 4]);
    assert_eq!(rows("range_from", "3,"), vec![3, 4, 5]);
    assert_eq!(rows("range_to", ",2"), vec![1, 2]);
    // One address is one line, which is what `sed -n '4p'` reads.
    assert_eq!(rows("range_one", "4"), vec![4]);

    for not_a_range in ["0,2", "2,0", "x,2"] {
        let out = ast_editor(
            &format!("nr_{not_a_range}"),
            &["view", file.to_str().unwrap(), not_a_range],
        );
        assert!(
            !out.status.success(),
            "{not_a_range:?} was read as a range rather than a path"
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(not_a_range),
            "{not_a_range:?} was not named as the path it was taken for: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// A fenced code block is closed by a fence of the same character, at least as
/// long as the one that opened it, indented no more than three spaces past it,
/// and with nothing but whitespace after it. Those are CommonMark's rules, and
/// an edit to markdown warns when a block is left open — so each rule needs a
/// pair that sits on either side of it.
#[test]
fn an_unclosed_code_fence_is_told_apart_from_a_closed_one() {
    // (name, body, whether the block is closed)
    let cases: &[(&str, &str, bool)] = &[
        ("plain", "# T\n\n```\ncode\n```\n", true),
        ("missing", "# T\n\n```\ncode\n", false),
        ("indented_3", "# T\n\n```\ncode\n   ```\n", true),
        ("indented_4", "# T\n\n```\ncode\n    ```\n", false),
        ("other_char", "# T\n\n```\ncode\n~~~\n", false),
        ("tilde_pair", "# T\n\n~~~\ncode\n~~~\n", true),
        ("shorter", "# T\n\n````\ncode\n```\n", false),
        ("longer", "# T\n\n```\ncode\n`````\n", true),
        ("trailing_text", "# T\n\n```\ncode\n``` done\n", false),
    ];

    for (name, body, closed) in cases {
        let file = scratch(&format!("fence_{name}.md"), body);
        let out = run_script(&format!("fence_{name}"), &file, "append ```\ntext\n```\n");
        assert!(
            out.status.success(),
            "{name} was refused: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let warnings = json_block(&out.stdout)["warnings"].clone();
        let unclosed = warnings
            .as_array()
            .map(|all| {
                all.iter()
                    .filter_map(|w| w.as_str())
                    .any(|w| w.contains("Unclosed fenced code block"))
            })
            .unwrap_or(false);
        assert_eq!(
            !unclosed,
            *closed,
            "{name}: the block is {}, and the answer {} it open",
            if *closed { "closed" } else { "open" },
            if unclosed { "calls" } else { "does not call" }
        );
    }
}

/// Markdown has templates of its own, spelled two ways where the name is
/// ambiguous, and each finds the thing it is named for. Four of the kinds carry
/// a definition block with a hash of their text and the rest carry none: a
/// heading or a table is a structure an edit can be addressed against, and a
/// link inside a sentence is not.
#[test]
fn a_markdown_template_finds_its_kind_and_says_which_are_structures() {
    let body = "# Head\n\ntext with a [link](http://e.com) in it.\n\n```\ncode\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n- one\n- two\n";
    let file = scratch("kinds.md", body);

    let found = |test: &str, args: &[&str]| -> serde_json::Value {
        let mut all = vec!["inspect", file.to_str().unwrap()];
        all.extend_from_slice(args);
        let out = ast_editor(test, &all);
        assert!(
            out.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        json_block(&out.stdout)
    };

    // (template, the kind it answers with, whether that kind is a structure)
    let templates: &[(&str, &str, bool)] = &[
        ("headings", "Heading", true),
        ("headers", "Heading", true),
        ("codeblocks", "CodeBlock", true),
        ("code_blocks", "CodeBlock", true),
        ("tables", "Table", true),
        ("lists", "List", true),
        ("links", "Link", false),
    ];

    for (template, kind, structural) in templates {
        let block = found(&format!("mdt_{template}"), &["--template", template]);
        assert_eq!(
            block["match_count"].as_u64().unwrap(),
            1,
            "--template {template} found nothing in a file with one {kind}"
        );
        let first = &block["matches"][0];
        assert_eq!(first["capture_name"].as_str().unwrap(), *kind);
        let definition = &first["definition"];
        assert_eq!(
            !definition.is_null(),
            *structural,
            "{kind}: definition is {definition:?} and it {} a structure",
            if *structural { "is" } else { "is not" }
        );
        if *structural {
            assert_eq!(definition["type"].as_str().unwrap(), *kind);
            assert!(
                definition["block_hash"]
                    .as_str()
                    .is_some_and(|h| !h.is_empty()),
                "{kind}'s definition carries no hash of its text"
            );
        }
    }

    // A query names a kind, and only the letters of it count: the punctuation
    // and case a caller writes are dropped before the names are compared.
    for spelling in ["CodeBlock", "code_block", "code-block", "CODEBLOCK"] {
        let block = found(&format!("mdq_{spelling}"), &["--query", spelling]);
        assert_eq!(
            block["match_count"].as_u64().unwrap(),
            1,
            "--query {spelling:?} did not reach CodeBlock"
        );
    }
}

/// A hash with no line number in front of it is not an address. The number is
/// what names the line; a hash on its own would land on whatever carries that
/// content now, which is editing by quoted text and the failure the ids exist
/// to remove (D-01M27JNJJDYSWD). It is refused, and the refusal says what an
/// address looks like.
#[test]
fn a_hash_with_no_number_is_not_an_address() {
    let file = scratch("bare_hash.txt", "x\ny\n");
    let ids = line_ids("barehash", &file);
    let (full, hash) = {
        let (_, hash) = ids[0].split_once('#').unwrap();
        (ids[0].clone(), hash.to_string())
    };

    // The line the caller read goes, and its content comes back elsewhere. The
    // full id is refused for it, which is the behaviour a bare hash undoes.
    let churn = run_script(
        "barehash_churn",
        &file,
        &format!("delete {full}\nappend ```\nx\n```\n"),
    );
    assert!(
        churn.status.success(),
        "{}",
        String::from_utf8_lossy(&churn.stderr)
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "y\nx\n");

    let out = run_script(
        "barehash_use",
        &file,
        &format!("replace {hash} ```\nlanded\n```\n"),
    );
    assert!(
        !out.status.success(),
        "a hash on its own was taken as an address and wrote: {}",
        std::fs::read_to_string(&file).unwrap()
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "y\nx\n",
        "a refused address wrote to the file"
    );
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(
        said.contains(&hash) && said.contains("<number>#<hash>"),
        "the refusal did not name the hash and the shape of an address: {said}"
    );

    // The full id whose line is gone is refused too, and differently: there is
    // nothing to re-read for it.
    let gone = run_script(
        "barehash_gone",
        &file,
        &format!("replace {full} ```\nlanded\n```\n"),
    );
    assert!(!gone.status.success());
    assert_ne!(
        String::from_utf8_lossy(&gone.stderr),
        said,
        "an address with no number and an id whose line is gone were refused alike"
    );
}

/// `outline --sexp` answers with the parse tree as text, for writing a query
/// against a grammar whose node names are not known yet. It stops at a fixed
/// depth and says so where it stopped, names the field a child sits under, and
/// carries a token's own text — which is what makes the dump readable as a
/// guide to the node names.
#[test]
fn an_sexp_dump_stops_at_its_depth_and_says_where() {
    let file = scratch("dumped.rs", "fn f() {\n    let a = 1 + 2;\n}\n");
    let out = ast_editor("sexp", &["outline", file.to_str().unwrap(), "--sexp"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout).to_string();

    // The header says which node was dumped and how far it reaches. The root of
    // a three-line file ends at the start of the fourth, which is where the
    // tree itself ends rather than a count of lines.
    assert!(
        text.contains("Target Node Type: source_file"),
        "the header does not name the node dumped:\n{text}"
    );
    assert!(
        text.contains("Target Node Line Range: 1-4"),
        "the header does not carry the root's own range:\n{text}"
    );

    // The root, and a node at each level under it, with its line range.
    for wanted in [
        "source_file [1:0",
        "  function_item [1:0",
        "    fn [1:0 - 1:2] \"fn\"",
        "    name: identifier",
        "      let_declaration [2:4",
    ] {
        assert!(
            text.contains(wanted),
            "the dump does not carry {wanted:?}:\n{text}"
        );
    }

    // The depth limit is announced where it bites, under the node whose
    // children were not printed, and not under a token that has none.
    assert!(
        text.contains("... (depth limit reached)"),
        "a tree deeper than the limit was printed without saying so:\n{text}"
    );
    assert!(
        !text.contains("integer_literal"),
        "the dump went past its own limit:\n{text}"
    );
    // In this file exactly one node at the limit has children, and the marker
    // sits directly under it. The tokens beside it have none and get no marker.
    let lines: Vec<&str> = text.lines().collect();
    let at = lines
        .iter()
        .position(|line| line.contains("let_declaration ["))
        .expect("no let_declaration in the dump");
    assert!(
        lines[at + 1].contains("... (depth limit reached)"),
        "the marker is not under the node whose children were dropped:\n{text}"
    );
    assert_eq!(
        text.matches("... (depth limit reached)").count(),
        1,
        "a token with no children was marked as cut short:\n{text}"
    );
}

/// A markdown file has no tree-sitter grammar behind it, so `--sexp` dumps the
/// CommonMark tree instead. The dump has to read as a guide to the same two
/// things: the node names, and the text a leaf carries — including the text of
/// an inline code span, which is a node of its own rather than part of the
/// paragraph's words.
#[test]
fn a_markdown_dump_carries_the_node_names_and_the_text_at_the_leaves() {
    let file = scratch(
        "dumped.md",
        "# Title\n\nA line with `inline code` in it.\n\n- item one\n- item two\n",
    );
    let out = ast_editor("mdsexp", &["outline", file.to_str().unwrap(), "--sexp"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout).to_string();

    // A node at each level, indented by its depth. The root is at the margin,
    // so a child that is not indented is a child the dump lost track of.
    for wanted in [
        "Document [1:1",
        "  Heading [1:1 - 1:7]",
        "    Text [1:3 - 1:7] \"Title\"",
        "  Paragraph [3:1",
        "    Code [3:14 - 3:24] \"inline code\"",
    ] {
        assert!(
            text.contains(wanted),
            "the dump does not carry {wanted:?}:\n{text}"
        );
    }

    // The limit bites at the paragraph inside a list item, which is the only
    // node here deep enough to have children it may not print.
    assert!(
        text.contains("... (depth limit reached)"),
        "a tree deeper than the limit was printed without saying so:\n{text}"
    );
    assert!(
        !text.contains("\"item one\""),
        "the dump went past its own limit:\n{text}"
    );
}

/// A read says what encloses the lines it printed, and the name carries the
/// kind as well as the identifier: a line inside a struct is inside
/// `struct:S`, not inside something unnamed. The kinds are spelled per
/// language, so each needs a language that has it.
#[test]
fn an_enclosing_context_names_the_kind_it_is() {
    // (file, source, the line to read, what should enclose it)
    let cases: &[(&str, &str, &str, &str)] = &[
        ("ctx_fn.rs", "fn f() {\n    let a = 1;\n}\n", "2,2", "fn:f"),
        (
            "ctx_struct.rs",
            "struct S {\n    a: u8,\n}\n",
            "2,2",
            "struct:S",
        ),
        (
            "ctx_trait.rs",
            "trait T {\n    fn m(&self);\n}\n",
            "2,2",
            "trait:T",
        ),
        // A line in the class body rather than in a method: a line is given the
        // innermost context that names it, and a method is inside the class.
        ("ctx_class.py", "class C:\n    x = 1\n", "2,2", "class:C"),
        (
            "ctx_method.py",
            "class C:\n    def m(self):\n        pass\n",
            "3,3",
            "fn:m",
        ),
    ];

    for (name, source, range, wanted) in cases {
        let file = scratch(name, source);
        let out = ast_editor(
            &format!("ctx_{name}"),
            &["view", file.to_str().unwrap(), range, "--only-ids"],
        );
        assert!(
            out.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let block = json_block(&out.stdout);
        let names: Vec<&str> = block["enclosing_contexts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|ctx| ctx["name"].as_str().unwrap())
            .collect();
        assert!(
            names.contains(wanted),
            "{name} line {range} is inside {wanted:?} and the answer said {names:?}"
        );
    }
}

/// A refusal carries the lines around the one the parser objected to, so a
/// caller can see the error without reading the file. Which line a grammar
/// blames is the grammar's business; what the context owes is that it is a
/// window of the file — consecutive lines, each numbered as the file numbers
/// it, at most two on either side, and exactly one of them marked.
#[test]
fn a_parse_error_carries_a_window_of_the_file_around_it() {
    let body = "fn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\nfn e() {}\nfn f() {}\n";
    let file = scratch("windowed.rs", body);
    let ids = line_ids("window", &file);
    let out = run_script_with(
        "window",
        &file,
        &format!("replace {} ```\nfn d( {{\n```\n", ids[3]),
        &[],
    );
    assert!(!out.status.success(), "a broken edit was accepted");

    // A refusal answers in the shape of a dry run, on stderr.
    let diagnostics = json_block(&out.stderr)["diagnostics"].clone();
    let context: Vec<String> = diagnostics[0]["context"]
        .as_array()
        .expect("no context in the diagnostic")
        .iter()
        .map(|line| line.as_str().unwrap().to_string())
        .collect();

    assert!(
        (1..=5).contains(&context.len()),
        "a window of two either side is at most five lines, and this is {}: {context:?}",
        context.len()
    );
    assert_eq!(
        context.iter().filter(|line| line.contains("-->")).count(),
        1,
        "exactly one line is the one objected to: {context:?}"
    );

    // Each line says which line of the file it is, and it is that line.
    let written: Vec<&str> = std::fs::read_to_string(&file)
        .unwrap()
        .lines()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .leak()
        .iter()
        .map(|s| s.as_str())
        .collect();
    let mut numbers = Vec::new();
    for line in &context {
        let (head, text) = line.split_once(": ").expect("no line number");
        let number: usize = head.trim().parse().expect("line number is not a number");
        numbers.push(number);
        let text = text.trim_start_matches("--> ").trim_start();
        assert_eq!(
            text,
            written[number - 1].trim_start(),
            "the context says line {number} is {text:?} and the file says {:?}",
            written[number - 1]
        );
    }
    for pair in numbers.windows(2) {
        assert_eq!(pair[1], pair[0] + 1, "the window skips a line: {numbers:?}");
    }

    // A window is around the line, so when the file continues past it, some of
    // what follows is in there. Otherwise the context is only what came before.
    let marked = context
        .iter()
        .position(|line| line.contains("-->"))
        .expect("checked above");
    let at = numbers[marked];
    if at < written.len() {
        assert!(
            marked + 1 < context.len(),
            "line {at} of {} was blamed and the context stopped there: {context:?}",
            written.len()
        );
    }

    // Two broken lines are two diagnostics, one each, because the refusal is
    // read to find what to fix: a line named twice is one fix reported twice,
    // and a line not named at all is a fix nobody knows about.
    let file = scratch("windowed_two.rs", body);
    let ids = line_ids("window2", &file);
    let out = run_script_with(
        "window2",
        &file,
        &format!(
            "replace {} ```\nfn b( {{\n```\nreplace {} ```\nfn e) }}\n```\n",
            ids[1], ids[4]
        ),
        &[],
    );
    assert!(!out.status.success(), "two broken edits were accepted");
    let diagnostics = json_block(&out.stderr)["diagnostics"]
        .as_array()
        .expect("no diagnostics")
        .clone();
    let blamed: Vec<String> = diagnostics
        .iter()
        .flat_map(|entry| {
            entry["context"]
                .as_array()
                .expect("no context")
                .iter()
                .filter_map(|line| line.as_str())
                .filter(|line| line.contains("-->"))
                .map(|line| line.split(':').next().unwrap().trim().to_string())
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(
        blamed,
        vec!["2".to_string(), "5".to_string()],
        "the refusal blames {blamed:?} rather than each broken line once"
    );
}

/// A tool's help is a column of flags and a column of prose beside it. The
/// prose column starts past the longest flag, every description begins in that
/// same column, and a line is broken only when the next word would not fit —
/// a wrap that breaks earlier than it has to costs a reader rows for nothing.
#[test]
fn a_tool_help_lays_its_prose_in_one_column() {
    for name in tool_names() {
        let out = ast_editor(&format!("col_{name}"), &[&name, "--help"]);
        assert!(
            out.status.success(),
            "{name} --help failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8_lossy(&out.stdout).to_string();

        let flags: Vec<&str> = text
            .lines()
            .filter(|line| line.starts_with("  --"))
            .collect();
        assert!(!flags.is_empty(), "{name} --help lists no flags:\n{text}");
        let longest_flag = flags
            .iter()
            .map(|line| line.split_whitespace().next().unwrap().len())
            .max()
            .unwrap();

        // Description lines are the indented ones that are not flags.
        let prose: Vec<&str> = text
            .lines()
            .filter(|line| line.starts_with("    ") && !line.starts_with("  --"))
            .filter(|line| !line.trim().is_empty())
            .collect();
        assert!(!prose.is_empty(), "{name} --help carries no prose:\n{text}");

        let columns: std::collections::HashSet<usize> = prose
            .iter()
            .map(|line| line.len() - line.trim_start().len())
            .collect();
        assert_eq!(
            columns.len(),
            1,
            "{name}: prose begins in {} different columns: {columns:?}",
            columns.len()
        );
        let column = *columns.iter().next().unwrap();
        assert!(
            column > longest_flag + 2,
            "{name}: prose begins at {column} and the longest flag reaches {}",
            longest_flag + 2
        );

        // The widest line is as wide as the wrap allows, so no earlier line of
        // the same description had room for the word that begins the next. The
        // lines are grouped per flag: two options' prose is two wraps, and the
        // last line of one had no reason to reach the first line of the other.
        let width = prose.iter().map(|line| line.len()).max().unwrap();
        let mut group: Vec<&str> = Vec::new();
        let mut groups: Vec<Vec<&str>> = Vec::new();
        for line in text.lines() {
            if line.starts_with("  --") || line.trim().is_empty() {
                if !group.is_empty() {
                    groups.push(std::mem::take(&mut group));
                }
            } else if line.starts_with("    ") {
                group.push(line);
            }
        }
        if !group.is_empty() {
            groups.push(group);
        }

        // And the prose is wrapped at all: some option's description is longer
        // than one line, or nothing here is doing any wrapping.
        assert!(
            groups.iter().any(|group| group.len() > 1),
            "{name}: no description in this help takes more than one line, so \
             nothing was wrapped:\n{text}"
        );

        for group in &groups {
            for pair in group.windows(2) {
                let (this, next) = (pair[0], pair[1]);
                let word = next.split_whitespace().next().unwrap_or("");
                if word.is_empty() {
                    continue;
                }
                assert!(
                    this.len() + 1 + word.len() > width,
                    "{name}: {this:?} had room for {word:?} and broke anyway (width {width})"
                );
            }
        }
    }
}

/// A dry run whose result parses with complaints answers `syntax_valid: true`
/// and carries the complaints beside a `preview_id`. The warnings are advice,
/// so the batch behind the id is committable: a caller who reads them and
/// decides they do not matter applies what it already validated rather than
/// resending it.
#[test]
fn a_dry_run_that_warns_still_hands_back_its_batch() {
    // A heading two levels below the one above it, which is what the markdown
    // grammar complains about without calling the file broken.
    let file = scratch("dry_warn.md", "# Title\n\nbody\n");
    let ids = line_ids("dry_warn", &file);
    let out = run_script_with(
        "dry_warn",
        &file,
        &format!("replace {} ```\n### Skipped\n```\n", ids[2]),
        &["--dry-run"],
    );
    assert!(
        out.status.success(),
        "a dry run that only warns exited non-zero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let answer = json_block(&out.stdout);
    assert_eq!(
        answer["syntax_valid"], true,
        "a warning was reported as a broken parse: {answer}"
    );
    assert!(
        !answer["warnings"].as_array().expect("warnings").is_empty(),
        "the skipped heading level was not reported: {answer}"
    );
    assert!(
        answer["preview_id"].is_string(),
        "a warned batch was not named, so it cannot be applied: {answer}"
    );
}

/// A dry run whose result does not parse answers `syntax_valid: false` with the
/// diagnostics — and a `preview_id` all the same. The parser reports rather
/// than vetoes, so a caller who judges it wrong applies the batch instead of
/// retyping it.
#[test]
fn a_dry_run_that_does_not_parse_still_names_its_batch() {
    let file = scratch("dry_broken.rs", "fn main() {\n    let x = 1;\n}\n");
    let ids = line_ids("dry_broken", &file);
    let out = run_script_with(
        "dry_broken",
        &file,
        &format!("replace {} ```\n    let x = (1;\n```\n", ids[1]),
        &["--dry-run"],
    );
    let answer = json_block(&out.stdout);
    assert_eq!(
        answer["syntax_valid"], false,
        "a broken payload was reported as parsing: {answer}"
    );
    assert!(
        !answer["diagnostics"]
            .as_array()
            .expect("diagnostics")
            .is_empty(),
        "nothing said what was wrong: {answer}"
    );
    assert!(
        answer["preview_id"].is_string(),
        "a refused batch was not named, so it cannot be applied: {answer}"
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "fn main() {\n    let x = 1;\n}\n",
        "a dry run wrote to the file"
    );
}

/// An op that acts at one line is given a span. The script form refuses it and
/// the JSON form used to take the field and drop it, so the same batch wrote
/// something different depending on which door it arrived through. README says
/// the ops that do not span say so if given two ids; both doors say it now.
#[test]
fn an_insert_given_a_span_is_refused_at_either_door() {
    let content = "fn main() {\n    let a = 1;\n    let b = 2;\n}\n";
    for (test, op) in [
        ("span_after", "insert_after"),
        ("span_before", "insert_before"),
    ] {
        let file = scratch(&format!("{test}.rs"), content);
        let ids = line_ids(test, &file);
        let path = file.to_str().unwrap();

        let out = ast_editor(
            test,
            &[
                "edit",
                path,
                "--json",
                &format!(
                    r#"{{"filepath":"{path}","edits":[{{"op":"{op}","start_id":"{}","end_id":"{}","content":"    // x"}}]}}"#,
                    ids[1], ids[2]
                ),
            ],
        );
        assert!(
            !out.status.success(),
            "{op} took a span through the json form: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            content,
            "{op} wrote to the file before refusing the span"
        );

        // The script form has always refused it, and is the other half of the
        // claim: one door enforcing it is what let this through.
        let refused = run_script(
            test,
            &file,
            &format!("{op} {},{} ```\n    // x\n```\n", ids[1], ids[2]),
        );
        assert!(
            !refused.status.success(),
            "{op} took a span through the script form: {}",
            String::from_utf8_lossy(&refused.stdout)
        );
    }
}

/// Every kind of definition a language declares is in its outline. The check is
/// against what each fixture declares rather than against a recorded answer: an
/// outline built from the `classes` and `functions` templates passed a fixture
/// test for as long as both existed while leaving out a Rust `enum`, a
/// TypeScript `interface` and a C `typedef`, because the fixture had been
/// written from what the code already did.
#[test]
fn an_outline_names_every_definition_its_file_declares() {
    // Each entry is the file, its source, and the signatures that have to come
    // back — one per definition the source declares, in no particular order.
    let cases: &[(&str, &str, &[&str])] = &[
        (
            "kinds.rs",
            "struct S {\n    x: i32,\n}\nenum E {\n    A,\n}\ntrait T {\n    fn m(&self);\n}\ntype Alias = i32;\nconst C: i32 = 1;\nfn f() {}\n",
            &["struct S", "enum E", "trait T", "type Alias", "const C", "fn f"],
        ),
        (
            "kinds.ts",
            "interface I {\n    a: string;\n}\ntype Al = string;\nenum En {\n    A,\n}\nclass C {\n    m() {}\n}\nfunction f() {}\n",
            &["interface I", "type Al", "enum En", "class C", "function f"],
        ),
        (
            "kinds.go",
            "package p\n\ntype I interface {\n\tM()\n}\n\ntype S struct {\n\tx int\n}\n\nfunc F() {}\n",
            &["type I interface", "type S struct", "func F"],
        ),
        (
            "kinds.c",
            "struct S {\n    int x;\n};\nenum E {\n    A\n};\ntypedef int Alias;\nvoid f(void) {}\n",
            &["struct S", "enum E", "typedef int Alias", "void f"],
        ),
        (
            "kinds.java",
            "interface I {\n    void m();\n}\nenum E {\n    A\n}\nclass C {\n    void g() {}\n}\n",
            &["interface I", "enum E", "class C", "void g"],
        ),
    ];

    for (name, source, wanted) in cases {
        let test = name.replace('.', "_");
        let file = scratch(name, source);
        let out = ast_editor(&test, &["outline", file.to_str().unwrap()]);
        assert!(
            out.status.success(),
            "outline {name} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let listed: Vec<String> = json_block(&out.stdout)["outline"]
            .as_array()
            .expect("an outline")
            .iter()
            .map(|entry| entry["signature"].as_str().unwrap_or("").to_string())
            .collect();
        for definition in *wanted {
            assert!(
                listed
                    .iter()
                    .any(|signature| signature.contains(definition)),
                "{name} declares {definition:?} and its outline does not name it: {listed:?}"
            );
        }
    }
}
