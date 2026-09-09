pub mod inspect;
pub mod dump;
pub mod session_db;
pub mod view;
pub mod edit;
pub mod metadata;
pub mod formatter;
pub mod script;

use serde::{Serialize, Deserialize};
use serde_json::Value;
use std::sync::Arc;
use anyhow::{Result, bail, Context};

use crate::parser::ParserManager;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct McpTextContent {
    #[serde(rename = "type")]
    pub content_type: String,
    pub text: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct McpToolResult {
    pub content: Vec<McpTextContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
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
                "name": "inspect_ast",
                "description": metadata::get_tool_description("inspect_ast"),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "file": {
                            "type": "string",
                            "description": "Path to the file to inspect (deprecated, use filepath instead)"
                        },
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
                            "description": "Predefined query template to run. Supported templates: functions, classes, imports (rust, python, go, javascript, typescript, tsx, java, c, cpp); traits, impls (rust); interfaces, structs (go); macros (c, cpp); functions (bash); headings, headers, codeblocks, code_blocks, links, tables, lists (markdown)."
                        },
                        "include_code": {
                            "type": "boolean",
                            "description": "Whether to include the source code of the enclosing definition (default: true)"
                        },
                        "code_format": {
                            "type": "string",
                            "enum": ["lines", "raw"],
                            "description": "Format of the returned code (default: 'lines')"
                        },
                        "output_file": {
                            "type": "boolean",
                            "description": "If true, saves the inspect output JSON to a unique file in the plugin's outputs directory and returns its absolute path. Recommended for large source files to bypass token limits."
                        }
                    }
                }
            }),
            serde_json::json!({
                "name": "dump_ast",
                "description": metadata::get_tool_description("dump_ast"),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "file": {
                            "type": "string",
                            "description": "Path to the file to inspect (deprecated, use filepath instead)"
                        },
                        "filepath": {
                            "type": "string",
                            "description": "Absolute path to the file to inspect"
                        }
                    }
                }
            }),
            serde_json::json!({
                "name": "view_lines",
                "description": metadata::get_tool_description("view_lines"),
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
                            "description": "Optional search term to filter lines matching this keyword."
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
                "name": "edit_lines",
                "description": metadata::get_tool_description("edit_lines"),
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
                "name": "create_lines",
                "description": metadata::get_tool_description("create_lines"),
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
            })
        ]
    }

    /// Invokes the appropriate tool based on name and deserialized arguments.
    pub async fn call_tool(&self, name: &str, arguments: Value, parser_manager: &Arc<ParserManager>) -> Result<Value> {
        match name {
            "inspect_ast" => {
                let args = serde_json::from_value(arguments)?;
                inspect::run_inspect(args, parser_manager).await
            }
            "dump_ast" => {
                let args = serde_json::from_value(arguments)?;
                dump::run_dump(args, parser_manager).await
            }
            "view_lines" => {
                let filepath = arguments.get("filepath").and_then(|v| v.as_str()).context("Missing filepath")?;
                let start_line = arguments.get("start_line").and_then(|v| v.as_u64()).map(|v| v as usize);
                let end_line = arguments.get("end_line").and_then(|v| v.as_u64()).map(|v| v as usize);
                let only_ids = arguments.get("only_ids").and_then(|v| v.as_bool());
                let query = arguments.get("query").and_then(|v| v.as_str()).map(|s| s.to_string());
                let context_lines = arguments.get("context_lines").and_then(|v| v.as_u64()).map(|v| v as usize);
                let repository = session_db::SqliteSessionRepository;
                let res = view::view_lines(&repository, filepath, start_line, end_line, only_ids, query, context_lines)?;
                
                let mut content = Vec::new();
                if let Some(code) = res.lines_text {
                    let ext = std::path::Path::new(filepath)
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("");
                    let lang = match ext {
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
                    };
                    let formatted_code = format!("```{}\n{}\n```", lang, code);
                    content.push(McpTextContent {
                        content_type: "text".to_string(),
                        text: formatted_code,
                    });
                }
                
                content.push(McpTextContent {
                    content_type: "text".to_string(),
                    text: res.metadata_json,
                });
                
                Ok(serde_json::to_value(McpToolResult {
                    content,
                    is_error: None,
                })?)
            }
            "edit_lines" => {
                let filepath = arguments.get("filepath").and_then(|v| v.as_str()).context("Missing filepath")?;
                let strict_validation = arguments.get("strict_validation").and_then(|v| v.as_bool()).unwrap_or(false);
                let dry_run = arguments.get("dry_run").and_then(|v| v.as_bool()).unwrap_or(false);
                let repository = session_db::SqliteSessionRepository;

                let text = if let Some(preview_id) = arguments.get("apply").and_then(|v| v.as_str()) {
                    edit::apply_preview(&repository, filepath, preview_id, strict_validation, parser_manager).await?
                } else {
                    let edits_val = arguments.get("edits").context("Missing edits array")?;
                    let edits: Vec<session_db::LineEdit> = serde_json::from_value(edits_val.clone())
                        .map_err(|err| {
                            // `update` was this operation's name until it was
                            // renamed for symmetry with replace_range and
                            // replace_substring. Say so once rather than
                            // accepting both names forever.
                            if err.to_string().contains("update") {
                                anyhow::anyhow!("{}. The 'update' operation is now called 'replace'.", err)
                            } else {
                                anyhow::Error::from(err)
                            }
                        })?;
                    if dry_run {
                        edit::edit_lines_dry_run(&repository, filepath, edits, parser_manager).await?
                    } else {
                        edit::edit_lines_with_validation(&repository, filepath, edits, strict_validation, parser_manager).await?
                    }
                };
                Ok(serde_json::to_value(McpToolResult {
                    content: vec![McpTextContent { content_type: "text".to_string(), text }],
                    is_error: None,
                })?)
            }
            "create_lines" => {
                let filepath = arguments.get("filepath").and_then(|v| v.as_str()).context("Missing filepath")?;
                let content = arguments.get("content").and_then(|v| v.as_str()).context("Missing content")?;
                let return_ids = arguments.get("return_ids").and_then(|v| v.as_bool());
                let repository = session_db::SqliteSessionRepository;
                let text = view::create_lines(&repository, filepath, content, return_ids)?;
                Ok(serde_json::to_value(McpToolResult {
                    content: vec![McpTextContent { content_type: "text".to_string(), text }],
                    is_error: None,
                })?)
            }
            _ => bail!("Unknown tool: {}", name),
        }
    }
}

pub static TEST_DB_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

