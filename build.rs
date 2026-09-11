//! The version of every grammar compiled in, read out of `Cargo.lock` so that
//! `--version` can name them (D-01M28RAGW19ZZC). Cargo tells a crate its own
//! version and nothing about its dependencies, and a table written by hand is
//! the thing that went stale last time.

use std::fmt::Write as _;

fn main() {
    println!("cargo:rerun-if-changed=Cargo.lock");

    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let lock = std::path::Path::new(&manifest).join("Cargo.lock");
    let text = std::fs::read_to_string(&lock)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", lock.display()));

    // Cargo.lock is a sequence of [[package]] tables, each with a name and a
    // version line. Only the grammars are wanted, and the parser stays this
    // small on purpose: a build script that needs a TOML dependency to report
    // versions is worse than the problem.
    let mut versions: Vec<(String, String)> = Vec::new();
    let mut name: Option<String> = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("name = ") {
            name = Some(value.trim_matches('"').to_string());
        } else if let Some(value) = line.strip_prefix("version = ") {
            if let Some(package) = name.take() {
                let carries_a_grammar = (package.starts_with("tree-sitter-")
                    && package != "tree-sitter-language")
                    || package == "comrak";
                if carries_a_grammar {
                    versions.push((package, value.trim_matches('"').to_string()));
                }
            }
        }
    }
    versions.sort();

    let mut generated = String::from(
        "/// Every grammar crate compiled in, with the version Cargo.lock pinned.\n\
         pub static GRAMMAR_VERSIONS: &[(&str, &str)] = &[\n",
    );
    for (package, version) in &versions {
        writeln!(generated, "    ({package:?}, {version:?}),").unwrap();
    }
    generated.push_str("];\n");

    let out = std::path::Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR"))
        .join("grammar_versions.rs");
    std::fs::write(&out, generated).expect("failed to write the grammar version table");
}
