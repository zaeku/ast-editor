pub mod edit;
pub mod formatter;
pub mod inspect;
pub mod metadata;
pub mod outline;
pub mod script;
pub mod session_db;
pub mod view;

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::sync::Arc;

use crate::parser::ParserManager;

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
    match ext {
        "rs" => "rust",
        "py" => "python",
        "js" => "javascript",
        "ts" => "typescript",
        "tsx" => "tsx",
        "jsx" => "jsx",
        "go" => "go",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "html" => "html",
        "md" => "markdown",
        "sh" => "bash",
        _ => "text",
    }
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
                            "description": "Absolute path to the file to inspect"
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
                        },
                        "output_file": {
                            "type": "boolean",
                            "description": "If true, saves the inspect output JSON to a unique file in the plugin's outputs directory and returns its absolute path. Recommended for large source files to bypass token limits."
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
                            "description": "Absolute path to the file to read"
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
                            "description": "Absolute path to the target file"
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
                            "description": "If true, only returns Line IDs and line numbers, omitting text content."
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
                            "description": "Absolute path to the file to modify"
                        },
                        "edits": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "op": {
                                        "type": "string",
                                        "enum": ["replace", "insert_after", "insert_before", "delete", "replace_range", "move", "replace_substring"],
                                        "description": "The edit operation to perform."
                                    },
                                    "target_id": {
                                        "type": "string",
                                        "description": "Optional target line ID (e.g. 1#a5c7). Required for replace, delete, replace_range, move, replace_substring. Optional/omitted for insert_before (prepends) and insert_after (appends)."
                                    },
                                    "end_target_id": {
                                        "type": "string",
                                        "description": "Optional ending target line ID for block range (e.g. 5#7f1c). Required for replace_range, optional for move."
                                    },
                                    "dest_target_id": {
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
                                        "description": "The new content to insert or replace with. Omitted/ignored for delete, move."
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
                            "description": "If true, returns the unified diff and syntax validation result the edits would produce, without writing to disk or assigning line IDs. When the result is syntactically valid the response also carries a preview_id; pass it back as 'apply' to commit that exact batch without resending it."
                        },
                        "apply": {
                            "type": "string",
                            "description": "A preview_id from an earlier dry_run, e.g. 'p1f'. Applies the batch that preview validated and returns its modified_ids. Supply 'filepath' with it; 'edits' is not needed and is ignored. A preview id is single-use, and is refused once the file has changed under it."
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
                            "description": "Absolute path to the file."
                        },
                        "content": {
                            "type": "string",
                            "description": "Initial text content of the file."
                        },
                        "return_ids": {
                            "type": "boolean",
                            "default": false,
                            "description": "If true, returns the flat array of generated Line IDs. Set to false to omit IDs and save tokens."
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
                let repository = session_db::SqliteSessionRepository;
                let res = view::view_lines(
                    &repository,
                    filepath,
                    start_line,
                    end_line,
                    only_ids,
                    query,
                    context_lines,
                    fixed_string,
                )?;

                let mut blocks = Vec::new();
                if let Some(code) = res.lines_text {
                    blocks.push(fenced(fence_language(filepath), &code));
                }
                blocks.push(fenced("json", &res.metadata_json));
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
                let repository = session_db::SqliteSessionRepository;

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
                let repository = session_db::SqliteSessionRepository;
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
