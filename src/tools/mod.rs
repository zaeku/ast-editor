pub mod inspect;
pub mod dump;
pub mod session_db;
pub mod view;
pub mod edit;
pub mod metadata;

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
                            "description": "Path to the file to inspect"
                        },
                        "query": {
                            "type": "string",
                            "description": "Optional Tree-sitter S-expression query"
                        },
                        "template": {
                            "type": "string",
                            "enum": ["functions", "classes", "imports"],
                            "description": "Predefined query template to run"
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
                    },
                    "required": ["file"]
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
                            "description": "Path to the file to inspect"
                        }
                    },
                    "required": ["file"]
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
                        }
                    },
                    "required": ["filepath", "start_line", "end_line"]
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
                                        "enum": ["update", "insert_after", "insert_before", "delete", "replace_range", "move"],
                                        "description": "The edit operation to perform."
                                    },
                                    "target_id": {
                                        "type": "string",
                                        "description": "Optional target line ID (e.g. 1#a5c7). Required for update, delete, replace_range, move. Optional/omitted for insert_before (prepends) and insert_after (appends)."
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
                                        "description": "The new content to insert/update/replace. Omitted/ignored for delete, move."
                                    }
                                },
                                "required": ["op"]
                            }
                        }
                    },
                    "required": ["filepath", "edits"]
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
                let start_line = arguments.get("start_line").and_then(|v| v.as_u64()).context("Missing start_line")? as usize;
                let end_line = arguments.get("end_line").and_then(|v| v.as_u64()).context("Missing end_line")? as usize;
                let only_ids = arguments.get("only_ids").and_then(|v| v.as_bool());
                let repository = session_db::SqliteSessionRepository;
                let text = view::view_lines(&repository, filepath, start_line, end_line, only_ids)?;
                Ok(serde_json::to_value(McpToolResult {
                    content: vec![McpTextContent { content_type: "text".to_string(), text }],
                    is_error: None,
                })?)
            }
            "edit_lines" => {
                let filepath = arguments.get("filepath").and_then(|v| v.as_str()).context("Missing filepath")?;
                let edits_val = arguments.get("edits").context("Missing edits array")?;
                let edits: Vec<session_db::LineEdit> = serde_json::from_value(edits_val.clone())?;
                let repository = session_db::SqliteSessionRepository;
                let text = edit::edit_lines(&repository, filepath, edits, parser_manager).await?;
                Ok(serde_json::to_value(McpToolResult {
                    content: vec![McpTextContent { content_type: "text".to_string(), text }],
                    is_error: None,
                })?)
            }
            "create_lines" => {
                let filepath = arguments.get("filepath").and_then(|v| v.as_str()).context("Missing filepath")?;
                let content = arguments.get("content").and_then(|v| v.as_str()).context("Missing content")?;
                let repository = session_db::SqliteSessionRepository;
                let text = view::create_lines(&repository, filepath, content)?;
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

