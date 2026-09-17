use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::Arc;
use tree_sitter::{Query, QueryCursor, StreamingIterator};

use crate::config::{language_name, template_query};
use crate::parser::ParserManager;
use crate::tools::formatter::line_id_at;
use crate::tools::markdown::run_markdown_inspect;
use crate::tools::repository::{FileStore, SqliteFileStore};

#[derive(Debug, Deserialize)]
pub(crate) struct InspectArgs {
    pub filepath: String,
    pub query: Option<String>,
    pub template: Option<String>,
    pub include_code: Option<bool>,
}

/// What `inspect` has to say: the code of each definition it matched, ready
/// to print as its own block, and the report naming what was found. The code
/// is kept out of the report because code inside a JSON string is code
/// nobody can read and nothing can copy.
#[derive(Debug)]
pub(crate) struct Inspected {
    pub blocks: Vec<String>,
    pub report: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct InspectResult {
    /// Absent when the search ran and answered. A name here means something
    /// else happened: a template the language does not have, or a query the
    /// grammar refused.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    pub filepath: String,
    pub language: String,
    pub has_syntax_errors: bool,
    /// The s-expression that was run. Null means none was asked for, which is
    /// a different thing from a search that matched nothing.
    pub query: Option<String>,
    pub match_count: usize,
    pub matches: Vec<InspectMatch>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct InspectMatch {
    pub capture_name: String,
    pub start_line: usize,
    pub end_line: usize,
    /// The line ids `edit` targets, so a match found by structure can be
    /// edited without a second call to locate it by number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_id: Option<String>,
    pub text: String,
    pub definition: Option<InspectDefinition>,
}

#[derive(Debug, Serialize)]
pub(crate) struct InspectDefinition {
    pub r#type: String,
    pub start_line: usize,
    pub end_line: usize,
    pub block_hash: String,
}

pub(crate) async fn run_inspect(
    args: InspectArgs,
    parser_manager: &Arc<ParserManager>,
) -> Result<Inspected> {
    let file_path = Path::new(&args.filepath);
    if !file_path.exists() {
        bail!("File not found: {:?}", file_path);
    }

    let code = fs::read_to_string(file_path)
        .with_context(|| format!("Failed to read file: {:?}", file_path))?;

    let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");

    let lang_name = language_name(ext)?;

    if args.query.is_none() && args.template.is_none() {
        bail!(
            "inspect searches, so it needs --query or --template. \
             For the file's definitions, run: ast-editor outline {}",
            args.filepath
        );
    }

    // JIT entry pre-caching
    let repository = SqliteFileStore;
    let mut file_key_opt = None;
    match repository.init_session(&args.filepath, false) {
        Ok(meta) => {
            file_key_opt = Some(meta.file_key);
        }
        Err(e) => {
            tracing::warn!(
                "Failed to make a file entry for inspect pre-caching: {:?}",
                e
            );
        }
    }

    if lang_name == "markdown" {
        return Ok(Inspected {
            blocks: Vec::new(),
            report: run_markdown_inspect(&code, &args, &repository, &file_key_opt)?,
        });
    }

    // 1. Delegated parsing via sticky session cache in ParserManager
    let (tree, language) = parser_manager
        .parse_code(ext, &code)
        .await
        .context("Failed to parse code via ParserManager delegation")?;

    let root_node = tree.root_node();
    let has_syntax_errors = root_node.has_error();

    // 3. Perform Query matching if requested
    let mut matches = Vec::new();
    let mut blocks: Vec<String> = Vec::new();
    let mut status: Option<String> = None;
    let mut hint = None;

    if has_syntax_errors {
        hint = Some("Warning: Syntax errors detected in source file. Tree-sitter query matching might be incomplete or fail to find some symbols due to structural errors.".to_string());
    }

    // What to search for: an explicit query, a named template, or — when
    // neither was given — nothing. The last case is reported as such rather
    // than as a search that found no matches.
    let query_str = match (&args.query, &args.template) {
        (Some(q), _) => Some(q.clone()),
        (None, Some(temp)) => template_query(&lang_name, temp),
        (None, None) => None,
    };

    if let (Some(ref template), None) = (&args.template, &query_str) {
        status = Some("warning".to_string());
        hint = Some(format!(
            "Template '{}' is not supported for language '{}'. Supported templates: functions, classes, imports (rust, python, go, javascript, typescript, tsx, java, c, cpp, swift), traits, impls (rust), interfaces, structs (go), macros (c, cpp), functions (bash).",
            template,
            lang_name
        ));
    }

    if let Some(ref q_str) = query_str {
        match Query::new(&language, q_str) {
            Ok(query) => {
                let mut cursor = QueryCursor::new();
                let mut matches_iter = cursor.matches(&query, root_node, code.as_bytes());

                while let Some(m) = matches_iter.next() {
                    for capture in m.captures {
                        let node = capture.node;
                        let capture_name =
                            query.capture_names()[capture.index as usize].to_string();

                        let start_position = node.start_position();
                        let end_position = node.end_position();
                        let node_text = node.utf8_text(code.as_bytes()).unwrap_or("").to_string();

                        // A definition's code becomes a block of its own,
                        // read as `view` reads lines.
                        let definition = if capture_name == "function" || capture_name == "class" {
                            let start_line = start_position.row + 1;
                            let end_line = end_position.row + 1;

                            if args.include_code.unwrap_or(true) {
                                blocks.push(match file_key_opt {
                                    Some(ref file_key) => definition_lines(
                                        &repository,
                                        file_key,
                                        start_line,
                                        end_line,
                                    )
                                    .unwrap_or_else(|err| {
                                        tracing::warn!("Failed to read the definition: {:?}", err);
                                        node_text.clone()
                                    }),
                                    None => node_text.clone(),
                                });
                            }

                            let mut hasher = std::collections::hash_map::DefaultHasher::new();
                            node_text.hash(&mut hasher);
                            let block_hash = format!("{:x}", hasher.finish());

                            Some(InspectDefinition {
                                r#type: node.kind().to_string(),
                                start_line,
                                end_line,
                                block_hash,
                            })
                        } else {
                            None
                        };

                        let start_line = start_position.row + 1;
                        let end_line = end_position.row + 1;
                        matches.push(InspectMatch {
                            capture_name,
                            start_line,
                            end_line,
                            start_id: line_id_at(&repository, &file_key_opt, start_line),
                            end_id: line_id_at(&repository, &file_key_opt, end_line),
                            text: if definition.is_some() {
                                node_text.lines().next().unwrap_or("").to_string()
                            } else {
                                node_text
                            },
                            definition,
                        });
                    }
                }
            }
            Err(e) => {
                // Nothing ran, so "no matches" would not be an answer to what
                // was asked: a typo in a query and a query that found nothing
                // would read the same (D-01M28HCSAMTEFS).
                bail!(
                    "{}",
                    crate::tools::metadata::get_config()
                        .error_query_does_not_compile
                        .replacen("{}", q_str, 1)
                        .replacen("{}", &format!("{:?}", e), 1)
                );
            }
        }
    }

    let result = InspectResult {
        status,
        filepath: args.filepath,
        language: lang_name.to_string(),
        has_syntax_errors,
        query: query_str.clone(),
        match_count: matches.len(),
        matches,
        hint: hint.clone(),
    };

    Ok(Inspected {
        blocks,
        report: serde_json::to_string_pretty(&result)?,
    })
}

/// The lines of a definition, rendered the way `view` renders them, so a
/// block read here and a block read there are the same thing.
fn definition_lines(
    repository: &impl crate::tools::repository::FileStore,
    file_key: &str,
    start_line: usize,
    end_line: usize,
) -> Result<String> {
    let formatted = crate::tools::formatter::retrieve_and_format_lines(
        repository,
        file_key,
        start_line,
        end_line,
        false,
        crate::tools::metadata::get_config().only_ids_wrap_trigger_length,
        crate::tools::formatter::LINE_CAP,
    )?;
    Ok(formatted.lines_text.unwrap_or_default())
}

#[cfg(test)]
#[allow(clippy::await_holding_lock)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_inspect_templates_all_languages() {
        let _lock = crate::tools::TEST_DB_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let temp_dir = crate::tools::test_temp_dir("test_inspect_templates_all_languages");
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let pm = Arc::new(ParserManager::new().unwrap());

        async fn test_one(
            pm: &Arc<ParserManager>,
            temp_dir: &std::path::Path,
            filename: &str,
            code: &str,
            template: &str,
            expected_contains: &str,
        ) {
            let file_path = temp_dir.join(filename);
            std::fs::write(&file_path, code).unwrap();
            let args = InspectArgs {
                filepath: file_path.to_string_lossy().to_string(),
                query: None,
                template: Some(template.to_string()),
                include_code: Some(true),
            };
            // A definition's code is a block now, and the report names what
            // was found, so a template is proved by the two together.
            let answer = run_inspect(args, pm).await.unwrap();
            let res_val = format!("{}\n{}", answer.blocks.join("\n"), answer.report);
            let text = &res_val;
            assert!(
                text.contains(expected_contains),
                "Expected text to contain '{}' for template '{}' of file '{}'. Got: {}",
                expected_contains,
                template,
                filename,
                text
            );
        }

        // 1. Rust
        test_one(
            &pm,
            &temp_dir,
            "test.rs",
            "fn my_rust_func() {}\nstruct MyRustStruct {}\ntrait MyRustTrait {}",
            "functions",
            "my_rust_func",
        )
        .await;
        test_one(
            &pm,
            &temp_dir,
            "test.rs",
            "fn my_rust_func() {}\nstruct MyRustStruct {}\ntrait MyRustTrait {}",
            "classes",
            "MyRustStruct",
        )
        .await;
        test_one(
            &pm,
            &temp_dir,
            "test.rs",
            "fn my_rust_func() {}\nstruct MyRustStruct {}\ntrait MyRustTrait {}",
            "traits",
            "MyRustTrait",
        )
        .await;

        // 2. Python
        test_one(
            &pm,
            &temp_dir,
            "test.py",
            "def my_py_func():\n    pass\nclass MyPyClass:\n    pass",
            "functions",
            "my_py_func",
        )
        .await;

        // 3. Go
        test_one(
            &pm,
            &temp_dir,
            "test.go",
            "package main\nfunc myGoFunc() {}\ntype MyGoStruct struct {}",
            "functions",
            "myGoFunc",
        )
        .await;

        // 4. JS/TS/TSX
        test_one(
            &pm,
            &temp_dir,
            "test.ts",
            "function myTsFunc() {}\nclass MyTsClass {}",
            "functions",
            "myTsFunc",
        )
        .await;
        test_one(
            &pm,
            &temp_dir,
            "test.tsx",
            "const MyComponent = () => { return <div />; };",
            "functions",
            "MyComponent",
        )
        .await;

        // 5. Java
        test_one(
            &pm,
            &temp_dir,
            "test.java",
            "class MyClass {\n    public void myJavaMethod() {}\n}",
            "functions",
            "myJavaMethod",
        )
        .await;

        // 6. C/C++
        test_one(
            &pm,
            &temp_dir,
            "test.cpp",
            "int myCppFunc() { return 0; }\n#define MY_MACRO 42",
            "functions",
            "myCppFunc",
        )
        .await;
        test_one(
            &pm,
            &temp_dir,
            "test.cpp",
            "int myCppFunc() { return 0; }\n#define MY_MACRO 42",
            "macros",
            "MY_MACRO",
        )
        .await;

        // 7. Bash
        test_one(
            &pm,
            &temp_dir,
            "test.sh",
            "my_bash_func() {\n  echo 'hello'\n}",
            "functions",
            "my_bash_func",
        )
        .await;
    }

    #[tokio::test]
    async fn test_inspect_nix_custom_query() {
        let _lock = crate::tools::TEST_DB_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let temp_dir = crate::tools::test_temp_dir("test_inspect_nix_custom_query");
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let pm = Arc::new(ParserManager::new().unwrap());

        let file_path = temp_dir.join("test.nix");
        std::fs::write(&file_path, "{ x = 1; }").unwrap();

        // 1. Test using 'file' field
        let args_file = InspectArgs {
            filepath: file_path.to_string_lossy().to_string(),
            query: Some("(binding attrpath: (attrpath (identifier) @attr))".to_string()),
            template: None,
            include_code: Some(true),
        };
        let text1 = run_inspect(args_file, &pm).await.unwrap().report;
        assert!(text1.contains("x"));

        // 2. Test using 'filepath' alias field (deserialized manually in code or via serde)
        let json_input = serde_json::json!({
            "filepath": file_path.to_string_lossy().to_string(),
            "query": "(binding attrpath: (attrpath (identifier) @attr))"
        });
        let args_filepath: InspectArgs = serde_json::from_value(json_input).unwrap();
        let text2 = run_inspect(args_filepath, &pm).await.unwrap().report;
        assert!(text2.contains("x"));
    }
}
