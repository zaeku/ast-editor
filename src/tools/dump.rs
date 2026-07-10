use std::sync::Arc;
use std::fs;
use std::path::Path;
use anyhow::{Result, Context, bail};
use serde::Deserialize;
use serde_json::Value;
use tree_sitter::Node;

use crate::parser::ParserManager;

use crate::tools::{McpTextContent, McpToolResult};

#[derive(Debug, Deserialize)]
pub struct DumpArgs {
    pub file: String,
}

fn format_node(node: Node, field_name: Option<&str>, code: &str, depth: usize, max_depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    let start = node.start_position();
    let end = node.end_position();
    
    let kind_str = if let Some(field) = field_name {
        format!("{}: {}", field, node.kind())
    } else {
        node.kind().to_string()
    };
    
    out.push_str(&format!(
        "{}{} [{}:{} - {}:{}]\n",
        indent,
        kind_str,
        start.row + 1,
        start.column,
        end.row + 1,
        end.column
    ));
    
    // For leaves (tokens) with text, append value
    if node.child_count() == 0 {
        if let Ok(text) = node.utf8_text(code.as_bytes()) {
            let cleaned = text.trim().replace("\n", " ");
            if !cleaned.is_empty() {
                // Remove last newline and append the text value representation
                out.pop(); // remove '\n'
                out.push_str(&format!(" \"{}\"\n", cleaned));
            }
        }
    }
    
    if depth < max_depth {
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i as u32) {
                let child_field = node.field_name_for_child(i as u32);
                format_node(child, child_field, code, depth + 1, max_depth, out);
            }
        }
    } else if node.child_count() > 0 {
        out.push_str(&format!("{}  ... (depth limit reached)\n", indent));
    }
}

pub async fn run_dump(args: DumpArgs, parser_manager: &Arc<ParserManager>) -> Result<Value> {
    let file_path = Path::new(&args.file);
    if !file_path.exists() {
        bail!("File not found: {:?}", file_path);
    }

    let code = fs::read_to_string(file_path)
        .with_context(|| format!("Failed to read file: {:?}", file_path))?;
    
    let ext = file_path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    // 1. Delegated parsing via sticky session cache in ParserManager
    let (tree, _language) = parser_manager.parse_code(ext, &code).await
        .context("Failed to parse code via ParserManager delegation")?;

    let root_node = tree.root_node();
    let max_depth = 3; // Align with Node.js default limit
    
    // Node.js uses max(1, end_position.row + 1) for the target line range
    let line_range_end = std::cmp::max(1, root_node.end_position().row + 1);
    
    let mut formatted_tree = String::new();
    format_node(root_node, None, &code, 0, max_depth, &mut formatted_tree);

    let header = format!(
        "[tree-sitter-dump-tree]\n\
         Max Depth Limit: {}\n\
         Target Node Type: {}\n\
         Target Node Line Range: 1-{}\n\
         Searched by range parameter: no\n\
         ========================================================================\n",
        max_depth,
        root_node.kind(),
        line_range_end
    );
    
    let tip = crate::tools::metadata::get_tool_tip("dump_ast");
    let final_text = if !tip.is_empty() {
        format!("{}{}\n{}", header, formatted_tree, tip)
    } else {
        format!("{}{}", header, formatted_tree)
    };

    Ok(serde_json::to_value(McpToolResult {
        content: vec![McpTextContent {
            content_type: "text".to_string(),
            text: final_text,
        }],
        is_error: None,
    })?)
}
