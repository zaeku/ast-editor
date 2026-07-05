pub mod inspect;
pub mod dump;

use serde_json::Value;
use std::sync::Arc;
use anyhow::{Result, bail};

use crate::parser::ParserManager;

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
                "name": "tree_sitter_inspect",
                "description": "Inspects code structure and finds target lines in source files using Tree-sitter.",
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
                        }
                    },
                    "required": ["file"]
                }
            }),
            serde_json::json!({
                "name": "tree_sitter_dump_tree",
                "description": "Dumps the complete AST syntax tree of a file as S-expression text.",
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
            })
        ]
    }

    /// Invokes the appropriate tool based on name and deserialized arguments.
    pub async fn call_tool(&self, name: &str, arguments: Value, parser_manager: &Arc<ParserManager>) -> Result<Value> {
        match name {
            "tree_sitter_inspect" => {
                let args = serde_json::from_value(arguments)?;
                inspect::run_inspect(args, parser_manager).await
            }
            "tree_sitter_dump_tree" => {
                let args = serde_json::from_value(arguments)?;
                dump::run_dump(args, parser_manager).await
            }
            _ => bail!("Unknown tool: {}", name),
        }
    }
}
