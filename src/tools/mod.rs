pub mod buffer;
pub mod edit;
pub mod formatter;
pub(crate) mod inspect;
pub mod metadata;
pub(crate) mod outline;
pub mod repository;
pub mod script;
pub mod session_db;
pub mod view;

// Drives the tools through the crate rather than through the command, which is
// what lets a caller reach a path no CLI invocation reaches deterministically —
// the index falling out of step with a file being written under it. Inside the
// crate rather than under tests/ so that nothing has to be `pub` to be tested.
#[cfg(test)]
mod line_edit_tests;

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::sync::Arc;

use crate::parser::ParserManager;

/// A temporary directory for a test, named so that no two processes share one.
/// A mutation run builds and tests many copies of this library at once, and
/// every copy resolves the same names; a directory they had in common made one
/// test's cleanup another test's failure, and the run read that failure as the
/// mutant being caught. The process id alone is reused, so the time goes in
/// beside it.
pub(crate) fn test_temp_dir(name: &str) -> std::path::PathBuf {
    use std::sync::OnceLock;
    static SUFFIX: OnceLock<String> = OnceLock::new();
    let suffix = SUFFIX.get_or_init(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        format!("{}-{}", std::process::id(), nanos)
    });
    std::env::temp_dir().join(format!("{name}-{suffix}"))
}

/// One block of a response, fenced and named by what it holds, so a reader —
/// or an `awk` one-liner — can tell the code from the data without knowing
/// which tool answered.
///
/// The fence is longer than any run of backticks that *starts a line* inside
/// it, the same rule the edit script reads, and only those can be mistaken
/// for a fence. Bounding it that way is what keeps the length predictable: no
/// line of pretty-printed JSON can begin with a backtick, so a `json` block
/// is always fenced with exactly three and `^```json$` matches it exactly.
pub fn fenced(language: &str, body: &str) -> String {
    let longest = body
        .lines()
        .map(|line| line.len() - line.trim_start_matches('`').len())
        .max()
        .unwrap_or(0);
    let fence = "`".repeat(std::cmp::max(3, longest + 1));
    format!("{}{}\n{}\n{}", fence, language, body, fence)
}

/// What a block of this file's lines is called on its fence.
pub fn fence_language(filepath: &str) -> &'static str {
    let ext = std::path::Path::new(filepath)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    crate::config::language_for_extension(ext).unwrap_or("text")
}

pub struct ToolDispatcher;

impl ToolDispatcher {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ToolDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolDispatcher {
    /// Returns the JSON schema array of available tools.
    pub fn list_tools(&self) -> Vec<Value> {
        vec![
            serde_json::json!({
                "name": "inspect",
                "description": metadata::get_tool_description("inspect"),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "filepath": {
                            "type": "string",
                            "description": "Path to the file, relative to the working directory or absolute"
                        },
                        "query": {
                            "type": "string",
                            "description": "Optional Tree-sitter S-expression query"
                        },
                        "template": {
                            "type": "string",
                            "enum": ["functions", "classes", "imports", "headings", "headers", "codeblocks", "code_blocks", "links", "tables", "lists", "traits", "impls", "structs", "interfaces", "macros"],
                            "description": "Predefined query template to run. Supported templates: functions, classes, imports (rust, python, go, javascript, typescript, tsx, java, c, cpp, swift); traits, impls (rust); interfaces, structs (go); macros (c, cpp); functions (bash); headings, headers, codeblocks, code_blocks, links, tables, lists (markdown)."
                        },
                        "include_code": {
                            "type": "boolean",
                            "description": "Whether to include the source code of the enclosing definition (default: true)"
                        }
                    }
                }
            }),
            serde_json::json!({
                "name": "outline",
                "description": metadata::get_tool_description("outline"),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "filepath": {
                            "type": "string",
                            "description": "Path to the file, relative to the working directory or absolute"
                        },
                        "sexp": {
                            "type": "boolean",
                            "default": false,
                            "description": "If true, returns the whole parse tree as s-expression text instead of the outline. For writing a query against a grammar whose node names are not yet known."
                        }
                    },
                    "required": ["filepath"]
                }
            }),
            serde_json::json!({
                "name": "view",
                "description": metadata::get_tool_description("view"),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "filepath": {
                            "type": "string",
                            "description": "Path to the file, relative to the working directory or absolute"
                        },
                        "filepaths": {
                            "type": "array",
                            "description": "More files to read in the same call, answered with one block each. A path after the first on the command line lands here."
                        },
                        "start_line": {
                            "type": "integer",
                            "description": "1-indexed starting line number (inclusive)"
                        },
                        "end_line": {
                            "type": "integer",
                            "description": "1-indexed ending line number (inclusive)"
                        },
                        "only_ids": {
                            "type": "boolean",
                            "description": "If true, answers with [id, line number] pairs instead of the lines themselves."
                        },
                        "query": {
                            "type": "string",
                            "description": "Optional regular expression; only lines matching it are returned. Matched against the file's own lines, so it is unaffected by how the response is printed. Prefix with (?i) to ignore case."
                        },
                        "fixed_string": {
                            "type": "boolean",
                            "default": false,
                            "description": "If true, 'query' is searched for literally rather than as a regular expression."
                        },
                        "context_lines": {
                            "type": "integer",
                            "description": "Optional number of surrounding context lines to return around query matches. Defaults to 5."
                        }
                    },
                    "required": ["filepath"]
                }
            }),
            serde_json::json!({
                "name": "edit",
                "description": metadata::get_tool_description("edit"),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "filepath": {
                            "type": "string",
                            "description": "Path to the file, relative to the working directory or absolute"
                        },
                        "edits": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "op": {
                                        "type": "string",
                                        "enum": ["replace", "insert_after", "insert_before", "delete", "move", "replace_substring"],
                                        "description": "The edit operation to perform."
                                    },
                                    "start_id": {
                                        "type": "string",
                                        "description": "The first line the op acts on (e.g. 1#a5c7). Required for replace, delete, move and replace_substring. Omitted for insert_before (prepends) and insert_after (appends)."
                                    },
                                    "end_id": {
                                        "type": "string",
                                        "description": "The last line of the span, where the op acts on more than one (e.g. 5#7f1c). Omitted addresses the start line alone. Taken by replace, delete and move."
                                    },
                                    "dest_id": {
                                        "type": "string",
                                        "description": "Optional destination target line ID (e.g. 10#e9c4). Required for move operations with 'before' or 'after' move_position."
                                    },
                                    "move_position": {
                                        "type": "string",
                                        "enum": ["before", "after", "prepend", "append"],
                                        "description": "Optional relative position for move operations ('before', 'after', 'prepend', 'append')."
                                    },
                                    "content": {
                                        "type": "string",
                                        "description": "The new content to insert or replace with, as line-terminated text: \"\" is no lines at all, \"\\n\" is one empty line, and a trailing newline ends the last line rather than starting another. Omitted/ignored for delete, move."
                                    },
                                    "pattern": {
                                        "type": "string",
                                        "description": "The substring pattern to find. Required for replace_substring."
                                    },
                                    "replacement": {
                                        "type": "string",
                                        "description": "The replacement string. Required for replace_substring."
                                    },
                                    "occurrence": {
                                        "type": "integer",
                                        "description": "Optional 1-indexed occurrence count of the pattern (default: 1) for replace_substring."
                                    }
                                },
                                "required": ["op"]
                            }
                        },
                        "strict_validation": {
                            "type": "boolean",
                            "default": false,
                            "description": "If true, rolls back edits on syntax or parser error. If false, saves changes anyway and returns warnings/errors."
                        },
                        "dry_run": {
                            "type": "boolean",
                            "default": false,
                            "description": "If true, returns the unified diff and the syntax result the edits would produce, without writing to disk or assigning line IDs. The response carries a preview_id whatever the verdict; pass it back as 'apply' to commit that exact batch without resending it."
                        },
                        "apply": {
                            "type": "string",
                            "description": "A preview_id from an earlier dry_run, e.g. 'p1f'. Applies the batch that preview validated and returns its modified_lines. Supply 'filepath' with it; 'edits' is not needed and is ignored. A preview id is single-use, and is refused once the file has changed under it."
                        }
                    },
                    "required": ["filepath"]
                }
            }),
            serde_json::json!({
                "name": "create",
                "description": metadata::get_tool_description("create"),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "filepath": {
                            "type": "string",
                            "description": "Path to the file to write, relative to the working directory or absolute"
                        },
                        "content": {
                            "type": "string",
                            "description": "Initial text content of the file."
                        },
                        "return_ids": {
                            "type": "boolean",
                            "default": false,
                            "description": "If true, answers with the new file's lines, each as [id, line number]. Set to false to omit them and save tokens."
                        }
                    },
                    "required": ["filepath", "content"]
                }
            }),
        ]
    }

    /// Invokes the appropriate tool based on name and deserialized arguments.
    /// Run one tool and return the text it prints.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        parser_manager: &Arc<ParserManager>,
    ) -> Result<String> {
        match name {
            "inspect" => {
                let args: inspect::InspectArgs = serde_json::from_value(arguments)?;
                let language = fence_language(&args.filepath);
                let answer = inspect::run_inspect(args, parser_manager).await?;
                let mut blocks: Vec<String> = answer
                    .blocks
                    .iter()
                    .map(|block| fenced(language, block))
                    .collect();
                blocks.push(fenced("json", &answer.report));
                Ok(blocks.join("\n"))
            }
            "outline" => {
                let args: outline::OutlineArgs = serde_json::from_value(arguments)?;
                let sexp = args.sexp.unwrap_or(false);
                let text = outline::run_outline(args, parser_manager).await?;
                Ok(fenced(if sexp { "text" } else { "json" }, &text))
            }
            "view" => {
                let filepath = arguments
                    .get("filepath")
                    .and_then(|v| v.as_str())
                    .context("Missing filepath")?;
                let start_line = arguments
                    .get("start_line")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as usize);
                let end_line = arguments
                    .get("end_line")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as usize);
                let only_ids = arguments.get("only_ids").and_then(|v| v.as_bool());
                let query = arguments
                    .get("query")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let context_lines = arguments
                    .get("context_lines")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as usize);
                let fixed_string = arguments.get("fixed_string").and_then(|v| v.as_bool());

                // One file answers as it always has. Several answer with one
                // block each and a json block that says which is which, so
                // skimming a directory is one call (card #8).
                let mut paths = vec![filepath.to_string()];
                if let Some(more) = arguments.get("filepaths").and_then(|v| v.as_array()) {
                    paths.extend(more.iter().filter_map(|v| v.as_str()).map(str::to_string));
                }

                let repository = repository::SqliteSessionRepository;
                let mut blocks = Vec::new();
                let mut per_file = Vec::new();

                for path in &paths {
                    let res = view::view_lines(
                        &repository,
                        path,
                        start_line,
                        end_line,
                        only_ids,
                        query.clone(),
                        context_lines,
                        fixed_string,
                    )
                    .with_context(|| format!("reading {}", path))?;

                    if let Some(code) = res.lines_text {
                        blocks.push(fenced(fence_language(path), &code));
                    }
                    if paths.len() == 1 {
                        blocks.push(fenced("json", &res.metadata_json));
                    } else {
                        let mut entry: serde_json::Value =
                            serde_json::from_str(&res.metadata_json)?;
                        if let Some(object) = entry.as_object_mut() {
                            object.insert(
                                "filepath".to_string(),
                                serde_json::Value::String(path.clone()),
                            );
                        }
                        per_file.push(entry);
                    }
                }

                if paths.len() > 1 {
                    blocks.push(fenced(
                        "json",
                        &serde_json::to_string_pretty(&serde_json::json!({ "files": per_file }))?,
                    ));
                }
                Ok(blocks.join("\n"))
            }
            "edit" => {
                let filepath = arguments
                    .get("filepath")
                    .and_then(|v| v.as_str())
                    .context("Missing filepath")?;
                let strict_validation = arguments
                    .get("strict_validation")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let dry_run = arguments
                    .get("dry_run")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let repository = repository::SqliteSessionRepository;

                let text = if let Some(preview_id) = arguments.get("apply").and_then(|v| v.as_str())
                {
                    edit::apply_preview(
                        &repository,
                        filepath,
                        preview_id,
                        strict_validation,
                        parser_manager,
                    )
                    .await?
                } else {
                    let edits_val = arguments.get("edits").context("Missing edits array")?;
                    let edits: Vec<session_db::LineEdit> =
                        serde_json::from_value(edits_val.clone())?;
                    if dry_run {
                        // The diff is the point of a dry run, so it is a block
                        // to read rather than a string to unescape.
                        let preview =
                            edit::edit_lines_dry_run(&repository, filepath, edits, parser_manager)
                                .await?;
                        return Ok(format!(
                            "{}\n{}",
                            fenced("diff", preview.diff.trim_end()),
                            fenced("json", &preview.report)
                        ));
                    }
                    edit::edit_lines_with_validation(
                        &repository,
                        filepath,
                        edits,
                        strict_validation,
                        parser_manager,
                    )
                    .await?
                };
                Ok(fenced("json", &text))
            }
            "create" => {
                let filepath = arguments
                    .get("filepath")
                    .and_then(|v| v.as_str())
                    .context("Missing filepath")?;
                let content = arguments
                    .get("content")
                    .and_then(|v| v.as_str())
                    .context("Missing content")?;
                let return_ids = arguments.get("return_ids").and_then(|v| v.as_bool());
                let repository = repository::SqliteSessionRepository;
                let text = view::create_lines(&repository, filepath, content, return_ids)?;
                Ok(fenced("json", &text))
            }
            _ => bail!("Unknown tool: {}", name),
        }
    }
}

pub static TEST_DB_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::fenced;

    #[test]
    fn test_a_fence_outgrows_the_backticks_a_line_starts_with() {
        assert_eq!(fenced("json", "{}"), "```json\n{}\n```");

        // A json block is always three: no line of pretty-printed json can
        // begin with a backtick, so `^```json$` is an exact match for one.
        assert!(fenced("json", "{\n  \"tip\": \"write ``` to fence\"\n}").starts_with("```json\n"));

        // A markdown file read raw can, and then the fence has to be longer.
        let wrapped = fenced("markdown", "text\n```rust\nfn a() {}\n```");
        assert!(wrapped.starts_with("````markdown\n"), "{}", wrapped);
        assert!(wrapped.ends_with("\n````"), "{}", wrapped);
    }
}
