use std::sync::Arc;
use std::fs;
use std::path::Path;
use anyhow::{Result, Context, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tree_sitter::{Query, QueryCursor, StreamingIterator};
use std::hash::{Hash, Hasher};

use crate::parser::ParserManager;

#[derive(Debug, Deserialize)]
pub struct InspectArgs {
    pub file: String,
    pub query: Option<String>,
    pub template: Option<String>,
    pub include_code: Option<bool>,
    pub code_format: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct InspectResult {
    pub status: String,
    pub filepath: String,
    pub language: String,
    pub has_syntax_errors: bool,
    pub match_count: usize,
    pub matches: Vec<InspectMatch>,
}

#[derive(Debug, Serialize)]
pub struct InspectMatch {
    pub pattern_index: usize,
    pub capture_name: String,
    pub start_line: usize,
    pub start_column: usize,
    pub end_line: usize,
    pub end_column: usize,
    pub text: String,
    pub definition: Option<InspectDefinition>,
}

#[derive(Debug, Serialize)]
pub struct InspectDefinition {
    pub r#type: String,
    pub start_line: usize,
    pub start_column: usize,
    pub end_line: usize,
    pub end_column: usize,
    pub block_hash: String,
    pub text: String,
}

pub async fn run_inspect(args: InspectArgs, parser_manager: &Arc<ParserManager>) -> Result<Value> {
    let file_path = Path::new(&args.file);
    if !file_path.exists() {
        bail!("File not found: {:?}", file_path);
    }

    let code = fs::read_to_string(file_path)
        .with_context(|| format!("Failed to read file: {:?}", file_path))?;
    
    let ext = file_path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    let lang_name = match ext {
        "py" => "python",
        "rs" => "rust",
        "js" | "jsx" => "javascript",
        "ts" | "tsx" => "typescript",
        "go" => "go",
        "html" | "htm" => "html",
        "json" => "json",
        "lua" => "lua",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        _ => bail!("Unsupported extension: {}", ext),
    };

    // 1. Delegated parsing via sticky session cache in ParserManager
    let (tree, language) = parser_manager.parse_code(ext, &code).await
        .context("Failed to parse code via ParserManager delegation")?;

    let root_node = tree.root_node();
    let has_syntax_errors = root_node.has_error();

    // 3. Perform Query matching if requested
    let mut matches = Vec::new();
    
    // Choose query S-expression based on template
    let query_str = if let Some(ref q) = args.query {
        Some(q.clone())
    } else if let Some(ref temp) = args.template {
        match (lang_name, temp.as_str()) {
            ("rust", "functions") => Some("(function_item) @function".to_string()),
            ("rust", "classes") => Some("(struct_item) @class".to_string()),
            ("rust", "imports") => Some("(use_declaration) @import".to_string()),
            ("python", "functions") => Some("(function_definition) @function".to_string()),
            ("python", "classes") => Some("(class_definition) @class".to_string()),
            ("python", "imports") => Some("(import_statement) @import".to_string()),
            _ => None,
        }
    } else {
        None
    };

    if let Some(ref q_str) = query_str {
        if let Ok(query) = Query::new(&language, q_str) {
            let mut cursor = QueryCursor::new();
            let mut matches_iter = cursor.matches(&query, root_node, code.as_bytes());
            
            while let Some(m) = matches_iter.next() {
                for capture in m.captures {
                    let node = capture.node;
                    let capture_name = query.capture_names()[capture.index as usize].to_string();
                    
                    let start_position = node.start_position();
                    let end_position = node.end_position();
                    let node_text = node.utf8_text(code.as_bytes()).unwrap_or("").to_string();
                    
                    // Generate a structural definition block
                    let definition = if capture_name == "function" || capture_name == "class" {
                        let text_val = if args.include_code.unwrap_or(true) {
                            node_text.clone()
                        } else {
                            "".to_string()
                        };
                        
                        let mut hasher = std::collections::hash_map::DefaultHasher::new();
                        node_text.hash(&mut hasher);
                        let block_hash = format!("{:x}", hasher.finish());

                        Some(InspectDefinition {
                            r#type: node.kind().to_string(),
                            start_line: start_position.row + 1,
                            start_column: start_position.column + 1,
                            end_line: end_position.row + 1,
                            end_column: end_position.column + 1,
                            block_hash,
                            text: text_val,
                        })
                    } else {
                        None
                    };

                    matches.push(InspectMatch {
                        pattern_index: m.pattern_index,
                        capture_name,
                        start_line: start_position.row + 1,
                        start_column: start_position.column + 1,
                        end_line: end_position.row + 1,
                        end_column: end_position.column + 1,
                        text: node_text,
                        definition,
                    });
                }
            }
        }
    }

    let result = InspectResult {
        status: "success".to_string(),
        filepath: args.file,
        language: lang_name.to_string(),
        has_syntax_errors,
        match_count: matches.len(),
        matches,
    };

    Ok(serde_json::to_value(result)?)
}
