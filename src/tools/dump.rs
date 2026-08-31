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
    #[serde(alias = "filepath")]
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

    let max_depth = 3; // Align with Node.js default limit
    let is_markdown = ext == "md" || ext == "markdown";

    let (formatted_tree, root_kind, line_range_end) = if is_markdown {
        let arena = comrak::Arena::new();
        let mut options = comrak::Options::default();
        options.extension.table = true;
        options.extension.tasklist = true;
        options.extension.strikethrough = true;
        options.extension.autolink = true;

        let root = comrak::parse_document(&arena, &code, &options);
        let line_range_end = code.lines().count();
        let mut formatted = String::new();
        format_comrak_node(root, &code, 0, max_depth, &mut formatted);
        
        let root_kind = {
            let data = root.data.borrow();
            let debug_str = format!("{:?}", data.value);
            debug_str
                .split(|c| c == '(' || c == '{' || c == ' ')
                .next()
                .unwrap_or("Document")
                .to_string()
        };
        (formatted, root_kind, line_range_end)
    } else {
        // 1. Delegated parsing via sticky session cache in ParserManager
        let (tree, _language) = parser_manager.parse_code(ext, &code).await
            .context("Failed to parse code via ParserManager delegation")?;

        let root_node = tree.root_node();
        let line_range_end = std::cmp::max(1, root_node.end_position().row + 1);
        let mut formatted = String::new();
        format_node(root_node, None, &code, 0, max_depth, &mut formatted);
        (formatted, root_node.kind().to_string(), line_range_end)
    };

    let header = format!(
        "[tree-sitter-dump-tree]\n\
         Max Depth Limit: {}\n\
         Target Node Type: {}\n\
         Target Node Line Range: 1-{}\n\
         Searched by range parameter: no\n\
         ========================================================================\n",
        max_depth,
        root_kind,
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

fn format_comrak_node<'a>(
    node: &'a comrak::nodes::AstNode<'a>,
    code: &str,
    depth: usize,
    max_depth: usize,
    out: &mut String,
) {
    let indent = "  ".repeat(depth);
    let data = node.data.borrow();
    
    // Extract variant name from Debug representation of NodeValue
    let debug_str = format!("{:?}", data.value);
    let kind = debug_str
        .split(|c| c == '(' || c == '{' || c == ' ')
        .next()
        .unwrap_or("Unknown");
        
    let pos = data.sourcepos;
    out.push_str(&format!(
        "{}{} [{}:{} - {}:{}]\n",
        indent,
        kind,
        pos.start.line,
        pos.start.column,
        pos.end.line,
        pos.end.column
    ));
    
    // For leaves with content (Text, Code, CodeBlock), print their string contents
    if node.first_child().is_none() {
        let mut leaf_text = None;
        match &data.value {
            comrak::nodes::NodeValue::Text(s) => {
                leaf_text = Some(s.as_str());
            }
            comrak::nodes::NodeValue::Code(c) => {
                leaf_text = Some(c.literal.as_str());
            }
            comrak::nodes::NodeValue::CodeBlock(cb) => {
                leaf_text = Some(cb.literal.as_str());
            }
            _ => {}
        }
        if let Some(text) = leaf_text {
            let cleaned = text.trim().replace("\n", " ");
            if !cleaned.is_empty() {
                out.pop(); // remove '\n'
                out.push_str(&format!(" \"{}\"\n", cleaned));
            }
        }
    }
    
    if depth < max_depth {
        let mut child = node.first_child();
        while let Some(c) = child {
            format_comrak_node(c, code, depth + 1, max_depth, out);
            child = c.next_sibling();
        }
    } else if node.first_child().is_some() {
        out.push_str(&format!("{}  ... (depth limit reached)\n", indent));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Arc;
    use crate::parser::ParserManager;

    #[tokio::test]
    async fn test_markdown_ast_dump() {
        let temp_dir = std::env::temp_dir().join("ast-editor-markdown-tests");
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();

        let file_path = temp_dir.join("test.md");
        let md_content = "# Hello World\n\nSome text content\n\n```rust\nfn main() {}\n```\n";
        fs::write(&file_path, md_content).unwrap();

        let pm = Arc::new(ParserManager::new().unwrap());
        let args = DumpArgs {
            file: file_path.to_string_lossy().to_string(),
        };

        let res = run_dump(args, &pm).await.unwrap();
        let value: McpToolResult = serde_json::from_value(res).unwrap();
        let text = &value.content[0].text;

        assert!(text.contains("[tree-sitter-dump-tree]"));
        assert!(text.contains("Document [1:1 - 7:3]"));
        assert!(text.contains("Heading [1:1 - 1:13]"));
        assert!(text.contains("Text [1:3 - 1:13] \"Hello World\""));
        assert!(text.contains("Paragraph [3:1 - 3:17]"));
        assert!(text.contains("Text [3:1 - 3:17] \"Some text content\""));
        assert!(text.contains("CodeBlock"));
        assert!(text.contains("fn main() {}"));

        let _ = fs::remove_dir_all(&temp_dir);
    }
}



