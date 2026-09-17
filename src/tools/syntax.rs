//! What a parser says about content, and nothing about the file it came from.
//!
//! An edit is written only if the result parses, so something has to read the
//! result and answer. This file is that answer: it takes text and a path to
//! name the language by, and says whether a grammar accepts it, where it does
//! not, and what a prose file should be warned about. It opens nothing and
//! stores nothing, so what it says depends on the content alone.

use anyhow::Result;

fn validate_markdown(content: &str) -> Result<()> {
    let arena = comrak::Arena::new();
    let mut options = comrak::Options::default();
    options.extension.table = true;
    options.extension.tasklist = true;
    options.extension.strikethrough = true;

    let root = comrak::parse_document(&arena, content, &options);
    let lines: Vec<&str> = content.split('\n').collect();

    for node in root.descendants() {
        let data = node.data.borrow();
        if let comrak::nodes::NodeValue::CodeBlock(ref cb) = data.value {
            if cb.fenced {
                let start_line = data.sourcepos.start.line;
                let end_line = data.sourcepos.end.line;

                let start_idx = start_line.saturating_sub(1);
                let end_idx = end_line.saturating_sub(1);

                // comrak numbers lines from one and never reports a node
                // ending past the last line it was given, so the refusal below
                // is unreachable and is here instead of an index that panics.
                let (Some(start_line_str), Some(end_line_str)) =
                    (lines.get(start_idx), lines.get(end_idx))
                else {
                    anyhow::bail!("Validation error: Fenced code block source position is out of bounds. Start: {}, End: {}", start_line, end_line);
                };
                let trimmed = start_line_str.trim_start();
                let fence_char = if trimmed.starts_with('`') {
                    Some('`')
                } else if trimmed.starts_with('~') {
                    Some('~')
                } else {
                    None
                };

                if let Some(fc) = fence_char {
                    // comrak ends a fenced block either at a closing fence or at
                    // the end of the input, and it is the one applying
                    // CommonMark's rules — same character, at least as long as
                    // the opening run, indented no further than three, nothing
                    // after it — when it decides where the block ends. So the
                    // only question left is which of the two it did, and the
                    // line it ended on answers that. Re-deriving those rules
                    // here could agree with comrak and nothing else.
                    let end_trimmed = end_line_str.trim_end_matches('\r').trim_start();
                    let is_valid_closing_fence = end_trimmed.starts_with(fc)
                        && end_trimmed.trim_end().chars().all(|c| c == fc);

                    if !is_valid_closing_fence {
                        anyhow::bail!(
                            "Validation error: Unclosed fenced code block starting at line {}",
                            start_line
                        );
                    }
                }
            }
        }
    }

    Ok(())
}

fn lint_markdown(content: &str) -> Vec<String> {
    let mut warnings = Vec::new();

    // 1. Check unclosed fenced code block via validate_markdown
    if let Err(err) = validate_markdown(content) {
        warnings.push(format!("{}", err));
    }

    // 2. Check header hierarchy
    let mut last_level = 0;
    for (line_num, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            let level = trimmed.chars().take_while(|&c| c == '#').count();
            let is_header = trimmed
                .chars()
                .nth(level)
                .is_some_and(|c| c.is_whitespace());
            if is_header {
                if level > last_level + 1 && last_level > 0 {
                    let config = crate::tools::metadata::get_config();
                    let msg = config
                        .warning_header_hierarchy
                        .replacen("{}", &(line_num + 1).to_string(), 1)
                        .replacen("{}", &level.to_string(), 1)
                        .replacen("{}", &last_level.to_string(), 1)
                        .replacen("{}", &(last_level + 1).to_string(), 1);
                    warnings.push(msg);
                }
                last_level = level;
            }
        }
    }

    for (line_num, line) in content.lines().enumerate() {
        if line.contains('[') && !line.contains(']') && line.contains('(') {
            let config = crate::tools::metadata::get_config();
            let msg = config
                .warning_malformed_link
                .replacen("{}", &(line_num + 1).to_string(), 1);
            warnings.push(msg);
        }
    }

    warnings
}

pub(crate) enum SyntaxValidationResult {
    Success,
    /// Nothing checked it: no grammar covers this file type. Distinct from
    /// Success, because a response that cannot tell them apart says the same
    /// thing about a file that parsed and a file nobody read (card #1).
    NotChecked,
    Warnings(Vec<String>),
    SyntaxErrors {
        errors: Vec<String>,
        contexts: Vec<Vec<String>>,
        _raw_ast: String,
    },
    InfrastructureFailure(String),
}

fn gather_syntax_errors(
    node: tree_sitter::Node,
    source_code: &str,
    errors: &mut Vec<(usize, String)>,
) {
    if node.is_error() {
        let start_pos = node.start_position();
        let snippet = source_code
            .lines()
            .nth(start_pos.row)
            .unwrap_or("")
            .to_string();
        errors.push((
            start_pos.row,
            format!(
                "Syntax error at line {}: {:?}",
                start_pos.row + 1,
                snippet.trim()
            ),
        ));
        return;
    } else if node.is_missing() {
        let start_pos = node.start_position();
        errors.push((
            start_pos.row,
            format!("Missing element at line {}", start_pos.row + 1),
        ));
        return;
    }

    if node.has_error() {
        let count = node.child_count();
        for i in 0..count {
            if let Some(child) = node.child(i as u32) {
                gather_syntax_errors(child, source_code, errors);
            }
        }
    }
}

fn format_error_context(source_code: &str, error_row: usize) -> Vec<String> {
    let lines: Vec<&str> = source_code.lines().collect();
    let start = error_row.saturating_sub(2);
    let end = (error_row + 2).min(lines.len().saturating_sub(1));
    let mut context = Vec::new();
    for (idx, line) in lines.iter().enumerate().take(end + 1).skip(start) {
        let line_num = idx + 1;
        let prefix = if idx == error_row {
            format!("{:>4}: --> ", line_num)
        } else {
            format!("{:>4}:     ", line_num)
        };
        context.push(format!("{prefix}{line}"));
    }
    context
}

pub(crate) async fn validate_syntax(
    filepath: &str,
    content: &str,
    parser_manager: &crate::parser::ParserManager,
) -> SyntaxValidationResult {
    let ext = std::path::Path::new(filepath)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    // What reads a file is the table's business, so this asks the table rather
    // than the extension. An entry carrying no tree-sitter grammar is read by
    // something else — comrak reads markdown — and must not reach the parser.
    // Keyed on "md" instead, a second such entry would pass the support check,
    // miss this branch, and fail inside the parser, which answers a plain file
    // with a refusal. The support check is derived from the table for the same
    // reason (D-01M27WE1VYAKPP).
    let Some(grammar) = crate::config::grammar_for_extension(&ext) else {
        return SyntaxValidationResult::NotChecked;
    };
    if grammar.language().is_none() {
        if grammar.name == "markdown" {
            let warnings = lint_markdown(content);
            return if warnings.is_empty() {
                SyntaxValidationResult::Success
            } else {
                SyntaxValidationResult::Warnings(warnings)
            };
        }
        // In the table and read by nothing this knows how to check.
        return SyntaxValidationResult::NotChecked;
    }

    let (tree, _language) = match parser_manager.parse_code(&ext, content).await {
        Ok(val) => val,
        Err(err) => {
            return SyntaxValidationResult::InfrastructureFailure(format!("{:?}", err));
        }
    };

    let root = tree.root_node();
    if root.has_error() {
        let ast = root.to_sexp();
        if ext == "html" || ext == "htm" {
            let config = crate::tools::metadata::get_config();
            let msg = config.warning_html_syntax.replace("{}", &ast);
            return SyntaxValidationResult::Warnings(vec![msg]);
        }

        let mut errors = Vec::new();
        gather_syntax_errors(root, content, &mut errors);

        errors.sort_by_key(|e| e.0);
        errors.dedup_by(|a, b| a.0 == b.0);

        let mut contexts = Vec::new();
        let mut error_messages = Vec::new();
        for (row, msg) in errors {
            error_messages.push(msg);
            contexts.push(format_error_context(content, row));
        }

        return SyntaxValidationResult::SyntaxErrors {
            errors: error_messages,
            contexts,
            _raw_ast: ast,
        };
    }

    SyntaxValidationResult::Success
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The window around an error is two lines either side, and the error's
    /// own line is the one with the arrow.
    ///
    /// Read as `error_row * 2` the far edge agrees at row two and nowhere else,
    /// so the rows here avoid it. The ends clamp: a row near the top has no
    /// two lines above it and one near the bottom has none below.
    #[test]
    fn an_error_is_shown_with_two_lines_either_side() {
        let source = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\n";

        // Row 4 counts from zero, so the arrow is on line 5 and the window is
        // lines 3 through 7.
        let middle = format_error_context(source, 4);
        assert_eq!(middle.len(), 5, "{middle:?}");
        assert!(middle[0].starts_with("   3:     "), "{middle:?}");
        assert!(middle[2].starts_with("   5: --> "), "{middle:?}");
        assert!(middle[4].starts_with("   7:     "), "{middle:?}");

        // The top clamps to the first line rather than counting below it.
        let top = format_error_context(source, 0);
        assert_eq!(top.len(), 3, "{top:?}");
        assert!(top[0].starts_with("   1: --> "), "{top:?}");
        assert!(top[2].starts_with("   3:     "), "{top:?}");

        // The bottom clamps to the last line.
        let bottom = format_error_context(source, 8);
        assert!(bottom.last().unwrap().starts_with("   9:"), "{bottom:?}");
        assert_eq!(bottom.len(), 3, "{bottom:?}");
    }

    /// A line closes a fence when it starts with the fence character and
    /// carries nothing else. Both halves are needed: `\u0060\u0060\u0060rust` starts with
    /// one and opens a block rather than closing it, and a line of tildes
    /// closes nothing a backtick opened.
    #[test]
    fn a_closing_fence_is_the_character_and_nothing_else() {
        // Opened, then a line that only looks like a fence. Still unclosed.
        let looks_closed = "# H\n\n```rust\nfn a() {}\n```rust\n";
        let refused = validate_markdown(looks_closed);
        assert!(
            refused.is_err(),
            "a line with text after the fence closed the block"
        );

        // The same block closed properly.
        let closed = "# H\n\n```rust\nfn a() {}\n```\n";
        assert!(
            validate_markdown(closed).is_ok(),
            "a closed block was refused"
        );
    }

    #[test]
    fn test_validate_markdown_valid() {
        let valid_md = r#"# Hello World
Some text here.

```rust
fn main() {
    println!("Hello");
}
```

Other text.
~strikethrough~

| A | B |
|---|---|
| 1 | 2 |
"#;
        assert!(validate_markdown(valid_md).is_ok());
    }

    #[test]
    fn test_validate_markdown_invalid() {
        let invalid_md = r#"# Hello World

```rust
fn main() {
    println!("Hello");
}
"#;
        let res = validate_markdown(invalid_md);
        assert!(res.is_err());
        assert!(res
            .unwrap_err()
            .to_string()
            .contains("Unclosed fenced code block"));
    }

    #[test]
    fn test_validate_markdown_nested_list_codeblock() {
        let nested_md = r#"# Nested Markdown List
*   **반환 포맷 (JSON)**:
    ```json
    {
      "status": "success",
      "file_key": "8f3a8b23",
      "total_lines": 420,
      "file_hash": "a1b2c3d4",
      "mtime": "2026-07-08T22:07:07Z",
      "is_supported": true
    }
    ```
"#;
        assert!(validate_markdown(nested_md).is_ok());
    }
}
