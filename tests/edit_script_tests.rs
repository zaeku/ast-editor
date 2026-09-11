//! The edit script format, driven through the binary as a shell would drive it.
//! The grammar is D-01M27ZZNWK431A in decisions/, which fences it from outside;
//! these drive the same rules through the module.

use std::io::Write;
use std::process::{Command, Stdio};

fn store_for(test: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("ast-editor-script-{}-{}", std::process::id(), test))
}

fn scratch(test: &str, name: &str, content: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ast-editor-script-files-{}-{}",
        std::process::id(),
        test
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, content).unwrap();
    path
}

/// Run `ast-editor edit <file> [flags]` with `script` on stdin.
fn edit(test: &str, file: &std::path::Path, flags: &[&str], script: &str) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ast-editor"))
        .arg("edit")
        .arg(file)
        .args(flags)
        .env("AST_EDITOR_CACHE_DIR", store_for(test))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    child.stdin.take();
    child.wait_with_output().unwrap()
}

/// The block a response fences as `json`, which is how a caller reads the
/// data half of one.
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

/// The ids of a file's lines, in order.
fn ids(test: &str, file: &std::path::Path) -> Vec<String> {
    let out = Command::new(env!("CARGO_BIN_EXE_ast-editor"))
        .args([
            "view",
            &format!(r#"{{"filepath":"{}","only_ids":true}}"#, file.display()),
        ])
        .env("AST_EDITOR_CACHE_DIR", store_for(test))
        .output()
        .unwrap();
    let meta = json_block(&out.stdout);
    meta["ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pair| pair[0].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn test_every_operation_lands() {
    let file = scratch(
        "ops",
        "ops.rs",
        "fn main() {\n    let a = 1;\n    let b = 2;\n    let c = 3;\n}\n",
    );
    let id = ids("ops", &file);

    let script = format!(
        "replace {} ```\n    let a = 10;\n```\ninsert_after {} ```\n    let inserted = 0;\n```\ndelete {}\n",
        id[1], id[2], id[3]
    );
    let out = edit("ops", &file, &[], &script);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "fn main() {\n    let a = 10;\n    let b = 2;\n    let inserted = 0;\n}\n"
    );
}

#[test]
fn test_a_line_changed_earlier_in_the_batch_cannot_be_targeted_again() {
    let body = "fn main() {\n    let a = 1;\n}\n";
    let file = scratch("restage", "r.rs", body);
    let id = ids("restage", &file);

    // The id carries a hash of the line's content. Changing the line changes
    // the hash, so an id captured before the batch no longer describes it.
    let script = format!(
        "replace {} ```\n    let a = 2;\n```\ninsert_after {} ```\n    let b = 3;\n```\n",
        id[1], id[1]
    );
    let out = edit("restage", &file, &[], &script);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("CHECKSUM_ERROR"));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), body);
}

#[test]
fn test_content_may_contain_a_shorter_fence() {
    let file = scratch("fence", "doc.md", "# Title\n\nbody\n");
    let id = ids("fence", &file);

    // A four-backtick fence carries content whose own fence is three.
    let script = format!(
        "replace {} ````\n```bash\nast-editor --version\n```\n````\n",
        id[2]
    );
    let out = edit("fence", &file, &[], &script);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "# Title\n\n```bash\nast-editor --version\n```\n"
    );
}

#[test]
fn test_an_unclosed_fence_fails_and_writes_nothing() {
    let body = "fn main() {\n    let a = 1;\n}\n";
    let file = scratch("unclosed", "u.rs", body);
    let id = ids("unclosed", &file);

    let script = format!("replace {} ```\n    let a = 2;\n", id[1]);
    let out = edit("unclosed", &file, &[], &script);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("never closed"), "{}", err);
    assert!(
        err.contains("line 1"),
        "the script line is not named: {}",
        err
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), body);
}

#[test]
fn test_an_unknown_operation_names_the_line() {
    let body = "fn main() {}\n";
    let file = scratch("unknown", "k.rs", body);

    let out = edit("unknown", &file, &[], "delete 1#77cf\nfrobnicate 2#0759\n");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("line 2"), "{}", err);
    assert!(err.contains("frobnicate"), "{}", err);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), body);
}

#[test]
fn test_a_payload_keeps_comments_and_blank_lines() {
    let file = scratch("verbatim", "v.rs", "fn main() {\n    old();\n}\n");
    let id = ids("verbatim", &file);

    // '#' and blank lines are directive-level syntax, but inside a block they
    // are content like anything else.
    let script = format!(
        "replace {} ````\n# not a comment here\n\n    kept();\n````\n",
        id[1]
    );
    let out = edit("verbatim", &file, &[], &script);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "fn main() {\n# not a comment here\n\n    kept();\n}\n"
    );
}

#[test]
fn test_a_dry_run_yields_a_preview_the_json_form_applies() {
    let file = scratch("preview", "p.rs", "fn main() {\n    let a = 1;\n}\n");
    let id = ids("preview", &file);

    let script = format!("replace {} ```\n    let a = 42;\n```\n", id[1]);
    let out = edit("preview", &file, &["--dry-run"], &script);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let preview = json_block(&out.stdout);
    assert_eq!(preview["syntax_valid"], true);
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "fn main() {\n    let a = 1;\n}\n"
    );

    // The two forms share one engine, so the id crosses between them.
    let apply = Command::new(env!("CARGO_BIN_EXE_ast-editor"))
        .args([
            "edit",
            &format!(
                r#"{{"filepath":"{}","apply":"{}"}}"#,
                file.display(),
                preview["preview_id"].as_str().unwrap()
            ),
        ])
        .env("AST_EDITOR_CACHE_DIR", store_for("preview"))
        .output()
        .unwrap();
    assert!(
        apply.status.success(),
        "{}",
        String::from_utf8_lossy(&apply.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "fn main() {\n    let a = 42;\n}\n"
    );
}

#[test]
fn test_a_batch_that_fails_validation_writes_none_of_itself() {
    let body = "fn main() {\n    let a = 1;\n    let b = 2;\n}\n";
    let file = scratch("atomic", "a.rs", body);
    let id = ids("atomic", &file);

    // The first directive is fine; the second breaks the syntax.
    let script = format!(
        "replace {} ```\n    let a = 10;\n```\nreplace {} ```\n    let b = ;\n```\n",
        id[1], id[2]
    );
    let out = edit("atomic", &file, &["--strict"], &script);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("Validation error"));
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        body,
        "part of the batch was written"
    );
}
