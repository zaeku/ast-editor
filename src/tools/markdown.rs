//! Reading a markdown file's structure, which no grammar does.
//!
//! Every other language the binary carries is parsed by tree-sitter and
//! searched with a query. Markdown is parsed by comrak, so a template names a
//! kind of node to walk for rather than an s-expression to run, and the walk is
//! here. What comes back is the shape `inspect` answers with, because this is
//! the `inspect` tool for one language rather than a tool of its own.

use crate::tools::inspect::{
    line_id_at, InspectArgs, InspectDefinition, InspectMatch, InspectResult,
};
use crate::tools::repository::FileStore;
use anyhow::Result;
use std::hash::{Hash, Hasher};

pub(crate) fn run_markdown_inspect(
    code: &str,
    args: &InspectArgs,
    repository: &impl FileStore,
    file_key_opt: &Option<String>,
) -> Result<String> {
    let arena = comrak::Arena::new();
    let mut options = comrak::Options::default();
    options.extension.table = true;
    options.extension.tasklist = true;
    options.extension.strikethrough = true;
    options.extension.autolink = true;

    let root = comrak::parse_document(&arena, code, &options);
    let mut matches = Vec::new();

    collect_markdown_matches(root, code, args, repository, file_key_opt, &mut matches);

    let mut status: Option<String> = None;
    let mut hint = None;

    if let Some(ref template) = args.template {
        match template.as_str() {
            "headings" | "headers" | "codeblocks" | "code_blocks" | "links" | "tables"
            | "lists" => {}
            _ => {
                status = Some("warning".to_string());
                hint = Some(format!(
                    "Template '{}' is not supported for language 'markdown'. Supported templates: headings, headers, codeblocks, code_blocks, links, tables, lists.",
                    template
                ));
            }
        }
    }

    let result = InspectResult {
        status,
        query: None,
        filepath: args.filepath.clone(),
        language: "markdown".to_string(),
        has_syntax_errors: false,
        match_count: matches.len(),
        matches,
        hint: hint.clone(),
    };

    serde_json::to_string_pretty(&result).map_err(anyhow::Error::from)
}

fn collect_markdown_matches<'a>(
    node: &'a comrak::nodes::AstNode<'a>,
    code: &str,
    args: &InspectArgs,
    repository: &impl FileStore,
    file_key_opt: &Option<String>,
    matches: &mut Vec<InspectMatch>,
) {
    let data = node.data.borrow();

    // Extract variant name from Debug representation of NodeValue
    let debug_str = format!("{:?}", data.value);
    let kind = debug_str.split(['(', '{', ' ']).next().unwrap_or("Unknown");

    let mut is_match = false;
    if let Some(ref template) = args.template {
        match template.as_str() {
            "headings" | "headers" => {
                if kind == "Heading" {
                    is_match = true;
                }
            }
            "codeblocks" | "code_blocks" => {
                if kind == "CodeBlock" {
                    is_match = true;
                }
            }
            "links" => {
                if kind == "Link" || kind == "Image" {
                    is_match = true;
                }
            }
            "tables" => {
                if kind == "Table" {
                    is_match = true;
                }
            }
            "lists" if kind == "List" => {
                is_match = true;
            }
            _ => {}
        }
    } else if let Some(ref q) = args.query {
        let term_filtered: String = q
            .chars()
            .filter(|c| c.is_alphabetic() || *c == '_')
            .collect();
        let query_normalized = term_filtered.to_lowercase().replace('_', "");
        let kind_normalized = kind.to_lowercase().replace('_', "");
        if query_normalized == kind_normalized {
            is_match = true;
        }
    }

    if is_match {
        let pos = data.sourcepos;
        let start_line = pos.start.line;
        let start_column = pos.start.column;
        let end_line = pos.end.line;
        let end_column = pos.end.column;

        let node_text = extract_text(code, start_line, start_column, end_line, end_column);

        // Generate a structural definition block
        let is_def_node =
            kind == "Heading" || kind == "CodeBlock" || kind == "Table" || kind == "List";
        let definition = if is_def_node {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            node_text.hash(&mut hasher);
            let block_hash = format!("{:x}", hasher.finish());

            Some(InspectDefinition {
                r#type: kind.to_string(),
                start_line,
                end_line,
                block_hash,
            })
        } else {
            None
        };

        matches.push(InspectMatch {
            capture_name: kind.to_string(),
            start_line,
            end_line,
            start_id: line_id_at(repository, file_key_opt, start_line),
            end_id: line_id_at(repository, file_key_opt, end_line),
            text: node_text,
            definition,
        });
    }

    // Recurse into children
    let mut child = node.first_child();
    while let Some(c) = child {
        collect_markdown_matches(c, code, args, repository, file_key_opt, matches);
        child = c.next_sibling();
    }
}

fn extract_text(
    code: &str,
    start_line: usize,
    start_column: usize,
    end_line: usize,
    end_column: usize,
) -> String {
    let lines: Vec<&str> = code.split('\n').collect();
    if start_line == 0 || start_line > lines.len() {
        return String::new();
    }

    let end_line = std::cmp::min(end_line, lines.len());
    if end_line < start_line {
        return String::new();
    }

    let mut result = Vec::new();
    for l in start_line..=end_line {
        let line_content = lines[l - 1];
        let start_col = if l == start_line {
            start_column.saturating_sub(1)
        } else {
            0
        };
        let end_col = if l == end_line {
            end_column
        } else {
            line_content.len()
        };

        let slice = safe_byte_slice(line_content, start_col, end_col);
        result.push(slice);
    }
    result.join("\n")
}

fn safe_byte_slice(s: &str, mut start: usize, mut end: usize) -> &str {
    start = std::cmp::min(start, s.len());
    end = std::cmp::min(end, s.len());
    while start > 0 && !s.is_char_boundary(start) {
        start -= 1;
    }
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    if start <= end {
        &s[start..end]
    } else {
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::repository::SqliteFileStore;

    /// `extract_text` indexes `lines[l - 1]`, so a line number of zero or one
    /// past the end is an out-of-bounds read rather than an empty answer. The
    /// numbers come from comrak, which counts from one and never reports past
    /// the end — this is the contract with comrak written down, so that a
    /// version of it that breaks the contract is caught here rather than in a
    /// panic somewhere else.
    #[test]
    fn extract_text_refuses_a_line_outside_the_file() {
        let code = "one\ntwo\nthree";
        assert_eq!(extract_text(code, 0, 1, 1, 3), "");
        assert_eq!(extract_text(code, 4, 1, 4, 3), "");
        assert_eq!(extract_text(code, 3, 1, 3, 5), "three");
    }

    #[test]
    fn test_markdown_inspect_templates() {
        let md_content = r#"# Heading 1
Some paragraph text with a [link](https://example.com) and an ![image](img.png).

```rust
fn main() {}
```

| Col 1 | Col 2 |
|---|---|
| A | B |

- Item 1
- Item 2
"#;

        let repository = SqliteFileStore;
        let file_key_opt = None;

        // Test Headings
        let args = InspectArgs {
            filepath: "test.md".to_string(),
            query: None,
            template: Some("headings".to_string()),
            include_code: Some(true),
        };
        let res_val = run_markdown_inspect(md_content, &args, &repository, &file_key_opt).unwrap();
        let text = &res_val;
        assert!(text.contains(r#""capture_name": "Heading""#));
        assert!(text.contains(r##""text": "# Heading 1"##));

        // Test Codeblocks
        let args = InspectArgs {
            filepath: "test.md".to_string(),
            query: None,
            template: Some("code_blocks".to_string()),
            include_code: Some(true),
        };
        let res_val = run_markdown_inspect(md_content, &args, &repository, &file_key_opt).unwrap();
        let text = &res_val;
        assert!(text.contains(r#""capture_name": "CodeBlock""#));
        assert!(text.contains("fn main()"));

        // Test Links
        let args = InspectArgs {
            filepath: "test.md".to_string(),
            query: None,
            template: Some("links".to_string()),
            include_code: Some(true),
        };
        let res_val = run_markdown_inspect(md_content, &args, &repository, &file_key_opt).unwrap();
        let text = &res_val;
        assert!(text.contains(r#""capture_name": "Link""#));
        assert!(text.contains(r#""capture_name": "Image""#));

        // Test Tables
        let args = InspectArgs {
            filepath: "test.md".to_string(),
            query: None,
            template: Some("tables".to_string()),
            include_code: Some(true),
        };
        let res_val = run_markdown_inspect(md_content, &args, &repository, &file_key_opt).unwrap();
        let text = &res_val;
        assert!(text.contains(r#""capture_name": "Table""#));

        // Test Lists
        let args = InspectArgs {
            filepath: "test.md".to_string(),
            query: None,
            template: Some("lists".to_string()),
            include_code: Some(true),
        };
        let res_val = run_markdown_inspect(md_content, &args, &repository, &file_key_opt).unwrap();
        let text = &res_val;
        assert!(text.contains(r#""capture_name": "List""#));

        // Test Query matching case-insensitive
        let args = InspectArgs {
            filepath: "test.md".to_string(),
            query: Some("paragraph".to_string()),
            template: None,
            include_code: Some(true),
        };
        let res_val = run_markdown_inspect(md_content, &args, &repository, &file_key_opt).unwrap();
        let text = &res_val;
        assert!(text.contains(r#""capture_name": "Paragraph""#));
    }
}
