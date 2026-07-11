use std::sync::Arc;
use std::fs;
use std::path::Path;
use anyhow::{Result, Context, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tree_sitter::{Query, QueryCursor, StreamingIterator};
use std::hash::{Hash, Hasher};

use crate::parser::ParserManager;
use crate::tools::session_db::{SessionRepository, SqliteSessionRepository};
use crate::tools::{McpTextContent, McpToolResult};

#[derive(Debug, Deserialize)]
pub struct InspectArgs {
    pub file: String,
    pub query: Option<String>,
    pub template: Option<String>,
    pub include_code: Option<bool>,
    pub code_format: Option<String>,
    pub output_file: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct InspectResult {
    pub status: String,
    pub filepath: String,
    pub language: String,
    pub has_syntax_errors: bool,
    pub match_count: usize,
    pub matches: Vec<InspectMatch>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct InspectSummary {
    pub status: String,
    pub filepath: String,
    pub language: String,
    pub has_syntax_errors: bool,
    pub match_count: usize,
    pub saved_to_file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
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
        "ts" => "typescript",
        "tsx" => "tsx",
        "go" => "go",
        "java" => "java",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" => "cpp",
        "html" | "htm" => "html",
        "json" => "json",
        "lua" => "lua",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "md" | "markdown" => "markdown",
        "sh" | "bash" | "zsh" | "ksh" => "bash",
        _ => bail!("Unsupported extension: {}", ext),
    };

    // JIT edit session pre-caching
    let repository = SqliteSessionRepository;
    let mut session_id_opt = None;
    match repository.init_session(&args.file, false) {
        Ok(meta) => {
            crate::tools::session_db::start_background_hash_worker(meta.session_id.clone());
            session_id_opt = Some(meta.session_id);
        }
        Err(e) => {
            tracing::warn!("Failed to initialize edit session for inspect pre-caching: {:?}", e);
        }
    }

    if lang_name == "markdown" {
        return run_markdown_inspect(&code, &args, &repository, &session_id_opt);
    }

    // 1. Delegated parsing via sticky session cache in ParserManager
    let (tree, language) = parser_manager.parse_code(ext, &code).await
        .context("Failed to parse code via ParserManager delegation")?;

    let root_node = tree.root_node();
    let has_syntax_errors = root_node.has_error();

    // 3. Perform Query matching if requested
    let mut matches = Vec::new();
    let mut status = "success".to_string();
    let mut hint = None;

    if has_syntax_errors {
        hint = Some("Warning: Syntax errors detected in source file. Tree-sitter query matching might be incomplete or fail to find some symbols due to structural errors.".to_string());
    }
    
    // Choose query S-expression based on template
    let query_str = if let Some(ref q) = args.query {
        Some(q.clone())
    } else if let Some(ref temp) = args.template {
        match (lang_name, temp.as_str()) {
            // Rust
            ("rust", "functions") => Some("(function_item) @function".to_string()),
            ("rust", "classes") => Some("(struct_item) @class".to_string()),
            ("rust", "imports") => Some("(use_declaration) @import".to_string()),
            ("rust", "traits") => Some("(trait_item) @trait".to_string()),
            ("rust", "impls") => Some("(impl_item) @impl".to_string()),

            // Python
            ("python", "functions") => Some("(function_definition) @function".to_string()),
            ("python", "classes") => Some("(class_definition) @class".to_string()),
            ("python", "imports") => Some("(import_statement) @import".to_string()),

            // Go
            ("go", "functions") => Some("[(function_declaration) (method_declaration)] @function".to_string()),
            ("go", "classes") => Some("(type_declaration) @class".to_string()),
            ("go", "imports") => Some("(import_declaration) @import".to_string()),
            ("go", "interfaces") => Some("(type_declaration (type_spec type: (interface_type))) @interface".to_string()),
            ("go", "structs") => Some("(type_declaration (type_spec type: (struct_type))) @struct".to_string()),

            // JavaScript / TypeScript / TSX
            ("javascript" | "typescript" | "tsx", "functions") => Some("[(function_declaration) (arrow_function) (method_definition)] @function".to_string()),
            ("javascript" | "typescript" | "tsx", "classes") => Some("(class_declaration) @class".to_string()),
            ("javascript" | "typescript" | "tsx", "imports") => Some("(import_statement) @import".to_string()),

            // Java
            ("java", "functions") => Some("(method_declaration) @function".to_string()),
            ("java", "classes") => Some("[(class_declaration) (interface_declaration)] @class".to_string()),
            ("java", "imports") => Some("(import_declaration) @import".to_string()),

            // C / C++
            ("c" | "cpp", "functions") => Some("(function_definition) @function".to_string()),
            ("c" | "cpp", "classes") => Some("[(struct_specifier) (class_specifier)] @class".to_string()),
            ("c" | "cpp", "imports") => Some("(preproc_include) @import".to_string()),
            ("c" | "cpp", "macros") => Some("[(preproc_def) (preproc_function_def)] @macro".to_string()),

            // Bash
            ("bash", "functions") => Some("(function_definition) @function".to_string()),

            _ => None,
        }
    } else {
        None
    };

    if let (Some(ref template), None) = (&args.template, &query_str) {
        status = "warning".to_string();
        hint = Some(format!(
            "Template '{}' is not supported for language '{}'. Supported templates: functions, classes, imports (rust, python, go, javascript, typescript, tsx, java, c, cpp), traits, impls (rust), interfaces, structs (go), macros (c, cpp), functions (bash).",
            template,
            lang_name
        ));
    }

    if let Some(ref q_str) = query_str {
        match Query::new(&language, q_str) {
            Ok(query) => {
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
                            let mut text_val = "".to_string();
                            let start_line = start_position.row + 1;
                            let end_line = end_position.row + 1;

                            if args.include_code.unwrap_or(true) {
                                if let Some(ref session_id) = session_id_opt {
                                    match format_definition_table(&repository, session_id, start_line, end_line) {
                                        Ok(table_text) => {
                                            text_val = table_text;
                                        }
                                        Err(e) => {
                                            tracing::warn!("Failed to format definition table: {:?}", e);
                                            text_val = node_text.clone();
                                        }
                                    }
                                } else {
                                    text_val = node_text.clone();
                                }
                            }
                            
                            let mut hasher = std::collections::hash_map::DefaultHasher::new();
                            node_text.hash(&mut hasher);
                            let block_hash = format!("{:x}", hasher.finish());

                            Some(InspectDefinition {
                                r#type: node.kind().to_string(),
                                start_line,
                                start_column: start_position.column + 1,
                                end_line,
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
            Err(e) => {
                status = "error".to_string();
                hint = Some(format!("Invalid Tree-sitter query S-expression: {}. Error: {:?}", q_str, e));
            }
        }
    }

    // Footnote JIT tips recommending edit_lines or view_lines
    let mut jit_footnote = None;
    if !matches.is_empty() {
        let footnote = if args.include_code.unwrap_or(true) {
            "Tip: You can apply edits to this file using the 'edit_lines' tool with the line IDs shown in the definition block."
        } else {
            "Tip: You can view line IDs for this file using the 'view_lines' tool."
        };
        jit_footnote = Some(footnote.to_string());
    }

    let mut final_hint = hint;
    if let Some(footnote) = jit_footnote {
        final_hint = match final_hint {
            Some(h) => Some(format!("{}\n\n{}", h, footnote)),
            None => Some(footnote),
        };
    }

    // Handle output_file option if specified
    let mut saved_to_file = None;
    if args.output_file.unwrap_or(false) {
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(parent) = exe_path.parent().and_then(|p| p.parent()) {
                let outputs_dir = parent.join("outputs");
                if !outputs_dir.exists() {
                    let _ = fs::create_dir_all(&outputs_dir);
                }
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::SystemTime::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                args.file.hash(&mut hasher);
                let hash_val = format!("{:x}", hasher.finish());
                let filename = format!(
                    "inspect_output_{}_{}.json",
                    now,
                    &hash_val[..std::cmp::min(8, hash_val.len())]
                );
                let file_path = outputs_dir.join(filename);
                let target_path_str = file_path.to_string_lossy().to_string();
                saved_to_file = Some(target_path_str);
            }
        }
    }

    let result = InspectResult {
        status: status.clone(),
        filepath: args.file,
        language: lang_name.to_string(),
        has_syntax_errors,
        match_count: matches.len(),
        matches,
        hint: final_hint.clone(),
    };

    let pretty_json = serde_json::to_string_pretty(&result)?;

    if let Some(ref path_str) = saved_to_file {
        let _ = fs::write(path_str, &pretty_json);
    }

    // Probabilistic Stateless Garbage Collector (1% trigger rate)
    let is_gc_turn = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_micros() % 100 == 0)
        .unwrap_or(false);

    if is_gc_turn {
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(parent) = exe_path.parent().and_then(|p| p.parent()) {
                let outputs_dir = parent.join("outputs");
                if outputs_dir.exists() {
                    tokio::spawn(run_gc(outputs_dir));
                }
            }
        }
    }

    let returned_text = if let Some(ref path_str) = saved_to_file {
        let summary = InspectSummary {
            status: status.clone(),
            filepath: result.filepath.clone(),
            language: result.language.clone(),
            has_syntax_errors: result.has_syntax_errors,
            match_count: result.match_count,
            saved_to_file: path_str.clone(),
            hint: Some(format!(
                "The full query result has been saved to the file specified in 'saved_to_file'. You can analyze it using jq, jc, or ripgrep.{}",
                final_hint.as_ref().map(|h| format!("\n\n{}", h)).unwrap_or_default()
            )),
        };
        serde_json::to_string_pretty(&summary)?
    } else {
        pretty_json
    };

    let is_err = if status == "error" { Some(true) } else { None };

    Ok(serde_json::to_value(McpToolResult {
        content: vec![McpTextContent {
            content_type: "text".to_string(),
            text: returned_text,
        }],
        is_error: is_err,
    })?)
}

pub(crate) async fn run_gc(outputs_dir: std::path::PathBuf) {
    if let Ok(mut entries) = tokio::fs::read_dir(outputs_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            if let Ok(metadata) = entry.metadata().await {
                if metadata.is_file() {
                    if let Ok(modified) = metadata.modified() {
                        if let Ok(elapsed) = modified.elapsed() {
                            // Deletes files older than 12 hours (43200 seconds)
                            if elapsed.as_secs() > 43200 {
                                let _ = tokio::fs::remove_file(entry.path()).await;
                            }
                        }
                    }
                }
            }
        }
    }
}

fn format_definition_table(
    repository: &impl crate::tools::session_db::SessionRepository,
    session_id: &str,
    start_line: usize,
    end_line: usize,
) -> Result<String> {
    let config = crate::tools::metadata::get_config();
    let formatted_res = crate::tools::formatter::retrieve_and_format_lines(
        repository,
        session_id,
        start_line,
        end_line,
        false,
        config.only_ids_wrap_trigger_length,
    )?;

    let output = format!(
        "{{\n  \"columns\": [\n    \"id\",\n    \"n\",\n    \"content\"\n  ],\n  \"lines\": {}\n}}",
        formatted_res.lines_json
    );
    Ok(output)
}

pub fn run_markdown_inspect(
    code: &str,
    args: &InspectArgs,
    repository: &impl SessionRepository,
    session_id_opt: &Option<String>,
) -> Result<Value> {
    let arena = comrak::Arena::new();
    let mut options = comrak::Options::default();
    options.extension.table = true;
    options.extension.tasklist = true;
    options.extension.strikethrough = true;
    options.extension.autolink = true;

    let root = comrak::parse_document(&arena, code, &options);
    let mut matches = Vec::new();

    collect_markdown_matches(root, code, args, repository, session_id_opt, &mut matches);

    let mut status = "success".to_string();
    let mut hint = None;

    let mut jit_footnote = None;
    if !matches.is_empty() {
        let footnote = if args.include_code.unwrap_or(true) {
            "Tip: You can apply edits to this file using the 'edit_lines' tool with the line IDs shown in the definition block."
        } else {
            "Tip: You can view line IDs for this file using the 'view_lines' tool."
        };
        jit_footnote = Some(footnote.to_string());
    }

    if let Some(ref template) = args.template {
        match template.as_str() {
            "headings" | "headers" | "codeblocks" | "code_blocks" | "links" | "tables" | "lists" => {}
            _ => {
                status = "warning".to_string();
                hint = Some(format!(
                    "Template '{}' is not supported for language 'markdown'. Supported templates: headings, headers, codeblocks, code_blocks, links, tables, lists.",
                    template
                ));
            }
        }
    }

    let mut final_hint = hint;
    if let Some(footnote) = jit_footnote {
        final_hint = match final_hint {
            Some(h) => Some(format!("{}\n\n{}", h, footnote)),
            None => Some(footnote),
        };
    }

    // Handle output_file option if specified
    let mut saved_to_file = None;
    if args.output_file.unwrap_or(false) {
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(parent) = exe_path.parent().and_then(|p| p.parent()) {
                let outputs_dir = parent.join("outputs");
                if !outputs_dir.exists() {
                    let _ = fs::create_dir_all(&outputs_dir);
                }
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::SystemTime::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                args.file.hash(&mut hasher);
                let hash_val = format!("{:x}", hasher.finish());
                let filename = format!(
                    "inspect_output_{}_{}.json",
                    now,
                    &hash_val[..std::cmp::min(8, hash_val.len())]
                );
                let file_path = outputs_dir.join(filename);
                let target_path_str = file_path.to_string_lossy().to_string();
                saved_to_file = Some(target_path_str);
            }
        }
    }

    let result = InspectResult {
        status: status.clone(),
        filepath: args.file.clone(),
        language: "markdown".to_string(),
        has_syntax_errors: false,
        match_count: matches.len(),
        matches,
        hint: final_hint.clone(),
    };

    let pretty_json = serde_json::to_string_pretty(&result)?;

    if let Some(ref path_str) = saved_to_file {
        let _ = fs::write(path_str, &pretty_json);
    }

    // Probabilistic Stateless Garbage Collector (1% trigger rate)
    let is_gc_turn = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_micros() % 100 == 0)
        .unwrap_or(false);

    if is_gc_turn {
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(parent) = exe_path.parent().and_then(|p| p.parent()) {
                let outputs_dir = parent.join("outputs");
                if outputs_dir.exists() {
                    tokio::spawn(run_gc(outputs_dir));
                }
            }
        }
    }

    let returned_text = if let Some(ref path_str) = saved_to_file {
        let summary = InspectSummary {
            status: status.clone(),
            filepath: result.filepath.clone(),
            language: result.language.clone(),
            has_syntax_errors: result.has_syntax_errors,
            match_count: result.match_count,
            saved_to_file: path_str.clone(),
            hint: Some(format!(
                "The full query result has been saved to the file specified in 'saved_to_file'. You can analyze it using jq, jc, or ripgrep.{}",
                final_hint.as_ref().map(|h| format!("\n\n{}", h)).unwrap_or_default()
            )),
        };
        serde_json::to_string_pretty(&summary)?
    } else {
        pretty_json
    };

    let is_err = if status == "error" { Some(true) } else { None };

    Ok(serde_json::to_value(McpToolResult {
        content: vec![McpTextContent {
            content_type: "text".to_string(),
            text: returned_text,
        }],
        is_error: is_err,
    })?)
}

fn collect_markdown_matches<'a>(
    node: &'a comrak::nodes::AstNode<'a>,
    code: &str,
    args: &InspectArgs,
    repository: &impl SessionRepository,
    session_id_opt: &Option<String>,
    matches: &mut Vec<InspectMatch>,
) {
    let data = node.data.borrow();
    
    // Extract variant name from Debug representation of NodeValue
    let debug_str = format!("{:?}", data.value);
    let kind = debug_str
        .split(|c| c == '(' || c == '{' || c == ' ')
        .next()
        .unwrap_or("Unknown");

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
            "lists" => {
                if kind == "List" {
                    is_match = true;
                }
            }
            _ => {}
        }
    } else if let Some(ref q) = args.query {
        let term_filtered: String = q.chars()
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
        let is_def_node = kind == "Heading" || kind == "CodeBlock" || kind == "Table" || kind == "List";
        let definition = if is_def_node && args.include_code.unwrap_or(true) {
            let text_val = if let Some(ref session_id) = session_id_opt {
                match format_definition_table(repository, session_id, start_line, end_line) {
                    Ok(table_text) => table_text,
                    Err(e) => {
                        tracing::warn!("Failed to format definition table: {:?}", e);
                        node_text.clone()
                    }
                }
            } else {
                node_text.clone()
            };

            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            node_text.hash(&mut hasher);
            let block_hash = format!("{:x}", hasher.finish());

            Some(InspectDefinition {
                r#type: kind.to_string(),
                start_line,
                start_column,
                end_line,
                end_column,
                block_hash,
                text: text_val,
            })
        } else {
            None
        };

        matches.push(InspectMatch {
            pattern_index: 0,
            capture_name: kind.to_string(),
            start_line,
            start_column,
            end_line,
            end_column,
            text: node_text,
            definition,
        });
    }

    // Recurse into children
    let mut child = node.first_child();
    while let Some(c) = child {
        collect_markdown_matches(c, code, args, repository, session_id_opt, matches);
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
    use crate::tools::session_db::SqliteSessionRepository;

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

        let repository = SqliteSessionRepository;
        let session_id_opt = None;

        // Test Headings
        let args = InspectArgs {
            file: "test.md".to_string(),
            query: None,
            template: Some("headings".to_string()),
            include_code: Some(true),
            code_format: None,
            output_file: None,
        };
        let res_val = run_markdown_inspect(md_content, &args, &repository, &session_id_opt).unwrap();
        let res: McpToolResult = serde_json::from_value(res_val).unwrap();
        let text = &res.content[0].text;
        assert!(text.contains(r#""capture_name": "Heading""#));
        assert!(text.contains(r##""text": "# Heading 1"##));

        // Test Codeblocks
        let args = InspectArgs {
            file: "test.md".to_string(),
            query: None,
            template: Some("code_blocks".to_string()),
            include_code: Some(true),
            code_format: None,
            output_file: None,
        };
        let res_val = run_markdown_inspect(md_content, &args, &repository, &session_id_opt).unwrap();
        let res: McpToolResult = serde_json::from_value(res_val).unwrap();
        let text = &res.content[0].text;
        assert!(text.contains(r#""capture_name": "CodeBlock""#));
        assert!(text.contains("fn main()"));

        // Test Links
        let args = InspectArgs {
            file: "test.md".to_string(),
            query: None,
            template: Some("links".to_string()),
            include_code: Some(true),
            code_format: None,
            output_file: None,
        };
        let res_val = run_markdown_inspect(md_content, &args, &repository, &session_id_opt).unwrap();
        let res: McpToolResult = serde_json::from_value(res_val).unwrap();
        let text = &res.content[0].text;
        assert!(text.contains(r#""capture_name": "Link""#));
        assert!(text.contains(r#""capture_name": "Image""#));

        // Test Tables
        let args = InspectArgs {
            file: "test.md".to_string(),
            query: None,
            template: Some("tables".to_string()),
            include_code: Some(true),
            code_format: None,
            output_file: None,
        };
        let res_val = run_markdown_inspect(md_content, &args, &repository, &session_id_opt).unwrap();
        let res: McpToolResult = serde_json::from_value(res_val).unwrap();
        let text = &res.content[0].text;
        assert!(text.contains(r#""capture_name": "Table""#));

        // Test Lists
        let args = InspectArgs {
            file: "test.md".to_string(),
            query: None,
            template: Some("lists".to_string()),
            include_code: Some(true),
            code_format: None,
            output_file: None,
        };
        let res_val = run_markdown_inspect(md_content, &args, &repository, &session_id_opt).unwrap();
        let res: McpToolResult = serde_json::from_value(res_val).unwrap();
        let text = &res.content[0].text;
        assert!(text.contains(r#""capture_name": "List""#));

        // Test Query matching case-insensitive
        let args = InspectArgs {
            file: "test.md".to_string(),
            query: Some("paragraph".to_string()),
            template: None,
            include_code: Some(true),
            code_format: None,
            output_file: None,
        };
        let res_val = run_markdown_inspect(md_content, &args, &repository, &session_id_opt).unwrap();
        let res: McpToolResult = serde_json::from_value(res_val).unwrap();
        let text = &res.content[0].text;
        assert!(text.contains(r#""capture_name": "Paragraph""#));
    }
    #[tokio::test]
    async fn test_inspect_templates_all_languages() {
        let _lock = crate::tools::TEST_DB_LOCK.lock().unwrap();
        let temp_dir = std::env::temp_dir().join("test_inspect_templates_all_languages");
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let wasm_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("resources").join("wasm");
        let pm = Arc::new(ParserManager::with_paths(
            temp_dir.join("cache"),
            temp_dir.join("compiler"),
            wasm_dir,
        ).unwrap());

        async fn test_one(pm: &Arc<ParserManager>, temp_dir: &std::path::Path, filename: &str, code: &str, template: &str, expected_contains: &str) {
            let file_path = temp_dir.join(filename);
            std::fs::write(&file_path, code).unwrap();
            let args = InspectArgs {
                file: file_path.to_string_lossy().to_string(),
                query: None,
                template: Some(template.to_string()),
                include_code: Some(true),
                code_format: None,
                output_file: None,
            };
            let res_val = run_inspect(args, pm).await.unwrap();
            let res: McpToolResult = serde_json::from_value(res_val).unwrap();
            let text = &res.content[0].text;
            assert!(
                text.contains(expected_contains),
                "Expected text to contain '{}' for template '{}' of file '{}'. Got: {}",
                expected_contains,
                template,
                filename,
                text
            );
        }

        // 1. Rust
        test_one(&pm, &temp_dir, "test.rs", "fn my_rust_func() {}\nstruct MyRustStruct {}\ntrait MyRustTrait {}", "functions", "my_rust_func").await;
        test_one(&pm, &temp_dir, "test.rs", "fn my_rust_func() {}\nstruct MyRustStruct {}\ntrait MyRustTrait {}", "classes", "MyRustStruct").await;
        test_one(&pm, &temp_dir, "test.rs", "fn my_rust_func() {}\nstruct MyRustStruct {}\ntrait MyRustTrait {}", "traits", "MyRustTrait").await;

        // 2. Python
        test_one(&pm, &temp_dir, "test.py", "def my_py_func():\n    pass\nclass MyPyClass:\n    pass", "functions", "my_py_func").await;

        // 3. Go
        test_one(&pm, &temp_dir, "test.go", "package main\nfunc myGoFunc() {}\ntype MyGoStruct struct {}", "functions", "myGoFunc").await;

        // 4. JS/TS/TSX
        test_one(&pm, &temp_dir, "test.ts", "function myTsFunc() {}\nclass MyTsClass {}", "functions", "myTsFunc").await;
        test_one(&pm, &temp_dir, "test.tsx", "const MyComponent = () => { return <div />; };", "functions", "MyComponent").await;

        // 5. Java
        test_one(&pm, &temp_dir, "test.java", "class MyClass {\n    public void myJavaMethod() {}\n}", "functions", "myJavaMethod").await;

        // 6. C/C++
        test_one(&pm, &temp_dir, "test.cpp", "int myCppFunc() { return 0; }\n#define MY_MACRO 42", "functions", "myCppFunc").await;
        test_one(&pm, &temp_dir, "test.cpp", "int myCppFunc() { return 0; }\n#define MY_MACRO 42", "macros", "MY_MACRO").await;

        // 7. Bash
        test_one(&pm, &temp_dir, "test.sh", "my_bash_func() {\n  echo 'hello'\n}", "functions", "my_bash_func").await;
    }

}
