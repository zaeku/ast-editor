use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use tree_sitter::{Node, Query, QueryCursor, StreamingIterator};

use crate::config::{language_name, outline_query};
use crate::parser::ParserManager;
use crate::tools::formatter::line_id_at;
use crate::tools::inspect::InspectArgs;
use crate::tools::markdown::run_markdown_inspect;
use crate::tools::repository::{FileStore, SqliteFileStore};

#[derive(Debug, Deserialize)]
pub(crate) struct OutlineArgs {
    pub filepath: String,
    pub sexp: Option<bool>,
}

fn format_node(
    node: Node,
    field_name: Option<&str>,
    code: &str,
    depth: usize,
    max_depth: usize,
    out: &mut String,
) {
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

pub(crate) async fn run_outline(
    args: OutlineArgs,
    parser_manager: &Arc<ParserManager>,
) -> Result<String> {
    if !args.sexp.unwrap_or(false) {
        return outline_report(&args.filepath, parser_manager).await;
    }
    dump_tree(&args.filepath, parser_manager).await
}

/// The whole parse tree as s-expression text, for writing a query against a
/// grammar whose node names are not yet known.
async fn dump_tree(filepath: &str, parser_manager: &Arc<ParserManager>) -> Result<String> {
    let file_path = Path::new(filepath);
    if !file_path.exists() {
        bail!("File not found: {:?}", file_path);
    }

    let code = fs::read_to_string(file_path)
        .with_context(|| format!("Failed to read file: {:?}", file_path))?;

    let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");

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
        format_comrak_node(root, 0, max_depth, &mut formatted);

        let root_kind = {
            let data = root.data.borrow();
            let debug_str = format!("{:?}", data.value);
            debug_str
                .split(['(', '{', ' '])
                .next()
                .unwrap_or("Document")
                .to_string()
        };
        (formatted, root_kind, line_range_end)
    } else {
        // 1. Delegated parsing via sticky session cache in ParserManager
        let (tree, _language) = parser_manager
            .parse_code(ext, &code)
            .await
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
        max_depth, root_kind, line_range_end
    );

    Ok(format!("{}{}", header, formatted_tree))
}

fn format_comrak_node<'a>(
    node: &'a comrak::nodes::AstNode<'a>,
    depth: usize,
    max_depth: usize,
    out: &mut String,
) {
    let indent = "  ".repeat(depth);
    let data = node.data.borrow();

    // Extract variant name from Debug representation of NodeValue
    let debug_str = format!("{:?}", data.value);
    let kind = debug_str.split(['(', '{', ' ']).next().unwrap_or("Unknown");

    let pos = data.sourcepos;
    out.push_str(&format!(
        "{}{} [{}:{} - {}:{}]\n",
        indent, kind, pos.start.line, pos.start.column, pos.end.line, pos.end.column
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
            format_comrak_node(c, depth + 1, max_depth, out);
            child = c.next_sibling();
        }
    } else if node.first_child().is_some() {
        out.push_str(&format!("{}  ... (depth limit reached)\n", indent));
    }
}

/// One definition a file declares, as `outline` lists it.
#[derive(Debug, Serialize)]
struct OutlineEntry {
    pub kind: String,
    pub signature: String,
    pub start_line: usize,
    pub end_line: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_id: Option<String>,
}
/// The file's top-level definitions, using whichever templates the language
/// declares. Each entry carries the line ids `edit` takes, so an outline is
/// enough to act on.
fn outline_of(
    language: &tree_sitter::Language,
    root: tree_sitter::Node,
    code: &str,
    repository: &impl FileStore,
    file_key: &Option<String>,
    lang_name: &str,
) -> Vec<OutlineEntry> {
    let mut entries = Vec::new();

    // One query for the language rather than a loop over borrowed templates. A
    // query that will not compile is a programming error in the table above,
    // not a file the caller got wrong, so it says so where whoever edits that
    // table will read it rather than answering with an empty outline.
    {
        let Some(query_str) = outline_query(lang_name) else {
            return entries;
        };
        let query = match Query::new(language, query_str) {
            Ok(query) => query,
            Err(err) => {
                tracing::warn!(
                    language = lang_name,
                    error = %err,
                    "the outline query names a node this grammar does not have"
                );
                return entries;
            }
        };

        let mut cursor = QueryCursor::new();
        let mut found = cursor.matches(&query, root, code.as_bytes());
        while let Some(m) = found.next() {
            for capture in m.captures {
                let node = capture.node;
                let start_line = node.start_position().row + 1;
                let end_line = node.end_position().row + 1;
                let text = node.utf8_text(code.as_bytes()).unwrap_or("");
                entries.push(OutlineEntry {
                    kind: query.capture_names()[capture.index as usize].to_string(),
                    signature: text.lines().next().unwrap_or("").trim().to_string(),
                    start_line,
                    end_line,
                    start_id: line_id_at(repository, file_key, start_line),
                    end_id: line_id_at(repository, file_key, end_line),
                });
            }
        }
    }

    entries.sort_by_key(|entry| entry.start_line);
    entries
}
#[derive(Debug, Serialize)]
struct OutlineReport {
    filepath: String,
    language: String,
    has_syntax_errors: bool,
    total_lines: usize,
    outline: Vec<OutlineEntry>,
}
/// The file's shape, which is what `outline` answers with: the definitions it
/// declares, each carrying the line ids `edit` takes.
async fn outline_report(filepath: &str, parser_manager: &Arc<ParserManager>) -> Result<String> {
    let file_path = Path::new(filepath);
    if !file_path.exists() {
        bail!("File not found: {:?}", file_path);
    }
    let code = fs::read_to_string(file_path)
        .with_context(|| format!("Failed to read file: {:?}", file_path))?;
    let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let lang_name = language_name(ext)?;

    let repository = SqliteFileStore;
    let file_key_opt = repository
        .init_session(filepath, false)
        .ok()
        .map(|meta| meta.file_key);

    // Markdown is read by comrak, not by a grammar, so its headings are the
    // outline. The markdown path already finds them; they come back as
    // matches, which is a search's shape, so they are read into this one.
    if lang_name == "markdown" {
        let args = InspectArgs {
            filepath: filepath.to_string(),
            query: None,
            template: Some("headings".to_string()),
            include_code: Some(false),
        };
        let found: Value = serde_json::from_str(&run_markdown_inspect(
            &code,
            &args,
            &repository,
            &file_key_opt,
        )?)?;
        let headings = found["matches"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|m| OutlineEntry {
                kind: "heading".to_string(),
                signature: m["text"].as_str().unwrap_or("").to_string(),
                start_line: m["start_line"].as_u64().unwrap_or(0) as usize,
                end_line: m["end_line"].as_u64().unwrap_or(0) as usize,
                start_id: m["start_id"].as_str().map(str::to_string),
                end_id: m["end_id"].as_str().map(str::to_string),
            })
            .collect();
        let report = OutlineReport {
            filepath: filepath.to_string(),
            language: lang_name.to_string(),
            has_syntax_errors: false,
            total_lines: code.lines().count(),
            outline: headings,
        };
        return Ok(serde_json::to_string_pretty(&report)?);
    }

    let (tree, language) = parser_manager
        .parse_code(ext, &code)
        .await
        .context("Failed to parse code via ParserManager delegation")?;
    let root_node = tree.root_node();

    let report = OutlineReport {
        filepath: filepath.to_string(),
        language: lang_name.to_string(),
        has_syntax_errors: root_node.has_error(),
        total_lines: code.lines().count(),
        outline: outline_of(
            &language,
            root_node,
            &code,
            &repository,
            &file_key_opt,
            &lang_name,
        ),
    };
    Ok(serde_json::to_string_pretty(&report)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::ParserManager;
    use std::fs;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_the_sexp_form_dumps_the_markdown_tree() {
        let temp_dir = crate::tools::test_temp_dir("ast-editor-markdown-tests");
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();

        let file_path = temp_dir.join("test.md");
        let md_content = "# Hello World\n\nSome text content\n\n```rust\nfn main() {}\n```\n";
        fs::write(&file_path, md_content).unwrap();

        let pm = Arc::new(ParserManager::new().unwrap());
        let args = OutlineArgs {
            filepath: file_path.to_string_lossy().to_string(),
            sexp: Some(true),
        };

        let text = run_outline(args, &pm).await.unwrap();

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
