use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::Arc;
use tree_sitter::{Query, QueryCursor, StreamingIterator};

use crate::parser::ParserManager;
use crate::tools::session_db::{SessionRepository, SqliteSessionRepository};

#[derive(Debug, Deserialize)]
pub struct InspectArgs {
    pub filepath: String,
    pub query: Option<String>,
    pub template: Option<String>,
    pub include_code: Option<bool>,
}

/// What `inspect` has to say: the code of each definition it matched, ready
/// to print as its own block, and the report naming what was found. The code
/// is kept out of the report because code inside a JSON string is code
/// nobody can read and nothing can copy.
#[derive(Debug)]
pub struct Inspected {
    pub blocks: Vec<String>,
    pub report: String,
}

#[derive(Debug, Serialize)]
pub struct InspectResult {
    /// Absent when the search ran and answered. A name here means something
    /// else happened: a template the language does not have, or a query the
    /// grammar refused.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    pub filepath: String,
    pub language: String,
    pub has_syntax_errors: bool,
    /// The s-expression that was run. Null means none was asked for, which is
    /// a different thing from a search that matched nothing.
    pub query: Option<String>,
    pub match_count: usize,
    pub matches: Vec<InspectMatch>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct InspectMatch {
    pub capture_name: String,
    pub start_line: usize,
    pub end_line: usize,
    /// The line ids `edit` targets, so a match found by structure can be
    /// edited without a second call to locate it by number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_id: Option<String>,
    pub text: String,
    pub definition: Option<InspectDefinition>,
}

/// One definition a file declares, as `outline` lists it.
#[derive(Debug, Serialize)]
pub struct OutlineEntry {
    pub kind: String,
    pub signature: String,
    pub start_line: usize,
    pub end_line: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_id: Option<String>,
}

/// The id `edit` would take for a line number, if a session exists.
fn line_id_at(
    repository: &impl SessionRepository,
    session_id: &Option<String>,
    line: usize,
) -> Option<String> {
    let session_id = session_id.as_ref()?;
    let row = repository
        .fetch_lines_range(session_id, line, line)
        .ok()?
        .into_iter()
        .next()?;
    let (seq, hash, _) = row;
    Some(format!("{:x}#{}", seq, hash?))
}

#[derive(Debug, Serialize)]
pub struct InspectDefinition {
    pub r#type: String,
    pub start_line: usize,
    pub end_line: usize,
    pub block_hash: String,
}

/// The s-expression a named template stands for, or None when the language
/// has no such template.
fn template_query(lang: &str, template: &str) -> Option<String> {
    match (lang, template) {
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
        ("go", "functions") => {
            Some("[(function_declaration) (method_declaration)] @function".to_string())
        }
        ("go", "classes") => Some("(type_declaration) @class".to_string()),
        ("go", "imports") => Some("(import_declaration) @import".to_string()),
        ("go", "interfaces") => {
            Some("(type_declaration (type_spec type: (interface_type))) @interface".to_string())
        }
        ("go", "structs") => {
            Some("(type_declaration (type_spec type: (struct_type))) @struct".to_string())
        }

        // JavaScript / TypeScript / TSX
        ("javascript" | "typescript" | "tsx", "functions") => Some(
            "[(function_declaration) (arrow_function) (method_definition)] @function".to_string(),
        ),
        ("javascript" | "typescript" | "tsx", "classes") => {
            Some("(class_declaration) @class".to_string())
        }
        ("javascript" | "typescript" | "tsx", "imports") => {
            Some("(import_statement) @import".to_string())
        }

        // Java
        ("java", "functions") => Some("(method_declaration) @function".to_string()),
        ("java", "classes") => {
            Some("[(class_declaration) (interface_declaration)] @class".to_string())
        }
        ("java", "imports") => Some("(import_declaration) @import".to_string()),

        // C / C++
        ("c" | "cpp", "functions") => Some("(function_definition) @function".to_string()),
        ("c" | "cpp", "classes") => {
            Some("[(struct_specifier) (class_specifier)] @class".to_string())
        }
        ("c" | "cpp", "imports") => Some("(preproc_include) @import".to_string()),
        ("c" | "cpp", "macros") => {
            Some("[(preproc_def) (preproc_function_def)] @macro".to_string())
        }

        // Bash
        ("bash", "functions") => Some("(function_definition) @function".to_string()),

        // Swift
        ("swift", "functions") => Some("(function_declaration) @function".to_string()),
        ("swift", "classes") => Some("(class_declaration) @class".to_string()),
        ("swift", "imports") => Some("(import_declaration) @import".to_string()),

        _ => None,
    }
}

/// The file's top-level definitions, using whichever templates the language
/// declares. Each entry carries the line ids `edit` takes, so an outline is
/// enough to act on.
fn outline_of(
    language: &tree_sitter::Language,
    root: tree_sitter::Node,
    code: &str,
    repository: &impl SessionRepository,
    session_id: &Option<String>,
    lang_name: &str,
) -> Vec<OutlineEntry> {
    let mut entries = Vec::new();

    for template in ["classes", "functions"] {
        let Some(query_str) = template_query(lang_name, template) else {
            continue;
        };
        let Ok(query) = Query::new(language, &query_str) else {
            continue;
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
                    start_id: line_id_at(repository, session_id, start_line),
                    end_id: line_id_at(repository, session_id, end_line),
                });
            }
        }
    }

    entries.sort_by_key(|entry| entry.start_line);
    entries
}

/// The grammar an extension is read with.
pub(crate) fn language_name(ext: &str) -> Result<&'static str> {
    Ok(match ext {
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
        "swift" => "swift",
        "nix" => "nix",
        _ => bail!("Unsupported extension: {}", ext),
    })
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
pub(crate) async fn outline_report(
    filepath: &str,
    parser_manager: &Arc<ParserManager>,
) -> Result<String> {
    let file_path = Path::new(filepath);
    if !file_path.exists() {
        bail!("File not found: {:?}", file_path);
    }
    let code = fs::read_to_string(file_path)
        .with_context(|| format!("Failed to read file: {:?}", file_path))?;
    let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let lang_name = language_name(ext)?;

    let repository = SqliteSessionRepository;
    let session_id_opt = repository
        .init_session(filepath, false)
        .ok()
        .map(|meta| meta.session_id);

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
            &session_id_opt,
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
            &session_id_opt,
            lang_name,
        ),
    };
    Ok(serde_json::to_string_pretty(&report)?)
}

pub async fn run_inspect(
    args: InspectArgs,
    parser_manager: &Arc<ParserManager>,
) -> Result<Inspected> {
    let file_path = Path::new(&args.filepath);
    if !file_path.exists() {
        bail!("File not found: {:?}", file_path);
    }

    let code = fs::read_to_string(file_path)
        .with_context(|| format!("Failed to read file: {:?}", file_path))?;

    let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");

    let lang_name = language_name(ext)?;

    if args.query.is_none() && args.template.is_none() {
        bail!(
            "inspect searches, so it needs --query or --template. \
             For the file's definitions, run: ast-editor outline {}",
            args.filepath
        );
    }

    // JIT edit session pre-caching
    let repository = SqliteSessionRepository;
    let mut session_id_opt = None;
    match repository.init_session(&args.filepath, false) {
        Ok(meta) => {
            session_id_opt = Some(meta.session_id);
        }
        Err(e) => {
            tracing::warn!(
                "Failed to initialize edit session for inspect pre-caching: {:?}",
                e
            );
        }
    }

    if lang_name == "markdown" {
        return Ok(Inspected {
            blocks: Vec::new(),
            report: run_markdown_inspect(&code, &args, &repository, &session_id_opt)?,
        });
    }

    // 1. Delegated parsing via sticky session cache in ParserManager
    let (tree, language) = parser_manager
        .parse_code(ext, &code)
        .await
        .context("Failed to parse code via ParserManager delegation")?;

    let root_node = tree.root_node();
    let has_syntax_errors = root_node.has_error();

    // 3. Perform Query matching if requested
    let mut matches = Vec::new();
    let mut blocks: Vec<String> = Vec::new();
    let mut status: Option<String> = None;
    let mut hint = None;

    if has_syntax_errors {
        hint = Some("Warning: Syntax errors detected in source file. Tree-sitter query matching might be incomplete or fail to find some symbols due to structural errors.".to_string());
    }

    // What to search for: an explicit query, a named template, or — when
    // neither was given — nothing. The last case is reported as such rather
    // than as a search that found no matches.
    let query_str = match (&args.query, &args.template) {
        (Some(q), _) => Some(q.clone()),
        (None, Some(temp)) => template_query(lang_name, temp),
        (None, None) => None,
    };

    if let (Some(ref template), None) = (&args.template, &query_str) {
        status = Some("warning".to_string());
        hint = Some(format!(
            "Template '{}' is not supported for language '{}'. Supported templates: functions, classes, imports (rust, python, go, javascript, typescript, tsx, java, c, cpp, swift), traits, impls (rust), interfaces, structs (go), macros (c, cpp), functions (bash).",
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
                        let capture_name =
                            query.capture_names()[capture.index as usize].to_string();

                        let start_position = node.start_position();
                        let end_position = node.end_position();
                        let node_text = node.utf8_text(code.as_bytes()).unwrap_or("").to_string();

                        // A definition's code becomes a block of its own,
                        // read as `view` reads lines.
                        let definition = if capture_name == "function" || capture_name == "class" {
                            let start_line = start_position.row + 1;
                            let end_line = end_position.row + 1;

                            if args.include_code.unwrap_or(true) {
                                blocks.push(match session_id_opt {
                                    Some(ref session_id) => definition_lines(
                                        &repository,
                                        session_id,
                                        start_line,
                                        end_line,
                                    )
                                    .unwrap_or_else(|err| {
                                        tracing::warn!("Failed to read the definition: {:?}", err);
                                        node_text.clone()
                                    }),
                                    None => node_text.clone(),
                                });
                            }

                            let mut hasher = std::collections::hash_map::DefaultHasher::new();
                            node_text.hash(&mut hasher);
                            let block_hash = format!("{:x}", hasher.finish());

                            Some(InspectDefinition {
                                r#type: node.kind().to_string(),
                                start_line,
                                end_line,
                                block_hash,
                            })
                        } else {
                            None
                        };

                        let start_line = start_position.row + 1;
                        let end_line = end_position.row + 1;
                        matches.push(InspectMatch {
                            capture_name,
                            start_line,
                            end_line,
                            start_id: line_id_at(&repository, &session_id_opt, start_line),
                            end_id: line_id_at(&repository, &session_id_opt, end_line),
                            text: if definition.is_some() {
                                node_text.lines().next().unwrap_or("").to_string()
                            } else {
                                node_text
                            },
                            definition,
                        });
                    }
                }
            }
            Err(e) => {
                // Nothing ran, so "no matches" would not be an answer to what
                // was asked: a typo in a query and a query that found nothing
                // would read the same (D-01M28HCSAMTEFS).
                bail!(
                    "{}",
                    crate::tools::metadata::get_config()
                        .error_query_does_not_compile
                        .replacen("{}", q_str, 1)
                        .replacen("{}", &format!("{:?}", e), 1)
                );
            }
        }
    }

    let result = InspectResult {
        status,
        filepath: args.filepath,
        language: lang_name.to_string(),
        has_syntax_errors,
        query: query_str.clone(),
        match_count: matches.len(),
        matches,
        hint: hint.clone(),
    };

    Ok(Inspected {
        blocks,
        report: serde_json::to_string_pretty(&result)?,
    })
}

/// The lines of a definition, rendered the way `view` renders them, so a
/// block read here and a block read there are the same thing.
fn definition_lines(
    repository: &impl crate::tools::session_db::SessionRepository,
    session_id: &str,
    start_line: usize,
    end_line: usize,
) -> Result<String> {
    let formatted = crate::tools::formatter::retrieve_and_format_lines(
        repository,
        session_id,
        start_line,
        end_line,
        false,
        crate::tools::metadata::get_config().only_ids_wrap_trigger_length,
    )?;
    Ok(formatted.lines_text.unwrap_or_default())
}

pub fn run_markdown_inspect(
    code: &str,
    args: &InspectArgs,
    repository: &impl SessionRepository,
    session_id_opt: &Option<String>,
) -> Result<String> {
    let arena = comrak::Arena::new();
    let mut options = comrak::Options::default();
    options.extension.table = true;
    options.extension.tasklist = true;
    options.extension.strikethrough = true;
    options.extension.autolink = true;

    let root = comrak::parse_document(&arena, code, &options);
    let mut matches = Vec::new();

    collect_markdown_matches(root, code, args, repository, session_id_opt, &mut matches);

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
    repository: &impl SessionRepository,
    session_id_opt: &Option<String>,
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
            start_id: line_id_at(repository, session_id_opt, start_line),
            end_id: line_id_at(repository, session_id_opt, end_line),
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
#[allow(clippy::await_holding_lock)]
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
            filepath: "test.md".to_string(),
            query: None,
            template: Some("headings".to_string()),
            include_code: Some(true),
        };
        let res_val =
            run_markdown_inspect(md_content, &args, &repository, &session_id_opt).unwrap();
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
        let res_val =
            run_markdown_inspect(md_content, &args, &repository, &session_id_opt).unwrap();
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
        let res_val =
            run_markdown_inspect(md_content, &args, &repository, &session_id_opt).unwrap();
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
        let res_val =
            run_markdown_inspect(md_content, &args, &repository, &session_id_opt).unwrap();
        let text = &res_val;
        assert!(text.contains(r#""capture_name": "Table""#));

        // Test Lists
        let args = InspectArgs {
            filepath: "test.md".to_string(),
            query: None,
            template: Some("lists".to_string()),
            include_code: Some(true),
        };
        let res_val =
            run_markdown_inspect(md_content, &args, &repository, &session_id_opt).unwrap();
        let text = &res_val;
        assert!(text.contains(r#""capture_name": "List""#));

        // Test Query matching case-insensitive
        let args = InspectArgs {
            filepath: "test.md".to_string(),
            query: Some("paragraph".to_string()),
            template: None,
            include_code: Some(true),
        };
        let res_val =
            run_markdown_inspect(md_content, &args, &repository, &session_id_opt).unwrap();
        let text = &res_val;
        assert!(text.contains(r#""capture_name": "Paragraph""#));
    }
    #[tokio::test]
    async fn test_inspect_templates_all_languages() {
        let _lock = crate::tools::TEST_DB_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let temp_dir = std::env::temp_dir().join("test_inspect_templates_all_languages");
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let wasm_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("resources")
            .join("wasm");
        let pm = Arc::new(
            ParserManager::with_paths(temp_dir.join("cache"), temp_dir.join("compiler"), wasm_dir)
                .unwrap(),
        );

        async fn test_one(
            pm: &Arc<ParserManager>,
            temp_dir: &std::path::Path,
            filename: &str,
            code: &str,
            template: &str,
            expected_contains: &str,
        ) {
            let file_path = temp_dir.join(filename);
            std::fs::write(&file_path, code).unwrap();
            let args = InspectArgs {
                filepath: file_path.to_string_lossy().to_string(),
                query: None,
                template: Some(template.to_string()),
                include_code: Some(true),
            };
            // A definition's code is a block now, and the report names what
            // was found, so a template is proved by the two together.
            let answer = run_inspect(args, pm).await.unwrap();
            let res_val = format!("{}\n{}", answer.blocks.join("\n"), answer.report);
            let text = &res_val;
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
        test_one(
            &pm,
            &temp_dir,
            "test.rs",
            "fn my_rust_func() {}\nstruct MyRustStruct {}\ntrait MyRustTrait {}",
            "functions",
            "my_rust_func",
        )
        .await;
        test_one(
            &pm,
            &temp_dir,
            "test.rs",
            "fn my_rust_func() {}\nstruct MyRustStruct {}\ntrait MyRustTrait {}",
            "classes",
            "MyRustStruct",
        )
        .await;
        test_one(
            &pm,
            &temp_dir,
            "test.rs",
            "fn my_rust_func() {}\nstruct MyRustStruct {}\ntrait MyRustTrait {}",
            "traits",
            "MyRustTrait",
        )
        .await;

        // 2. Python
        test_one(
            &pm,
            &temp_dir,
            "test.py",
            "def my_py_func():\n    pass\nclass MyPyClass:\n    pass",
            "functions",
            "my_py_func",
        )
        .await;

        // 3. Go
        test_one(
            &pm,
            &temp_dir,
            "test.go",
            "package main\nfunc myGoFunc() {}\ntype MyGoStruct struct {}",
            "functions",
            "myGoFunc",
        )
        .await;

        // 4. JS/TS/TSX
        test_one(
            &pm,
            &temp_dir,
            "test.ts",
            "function myTsFunc() {}\nclass MyTsClass {}",
            "functions",
            "myTsFunc",
        )
        .await;
        test_one(
            &pm,
            &temp_dir,
            "test.tsx",
            "const MyComponent = () => { return <div />; };",
            "functions",
            "MyComponent",
        )
        .await;

        // 5. Java
        test_one(
            &pm,
            &temp_dir,
            "test.java",
            "class MyClass {\n    public void myJavaMethod() {}\n}",
            "functions",
            "myJavaMethod",
        )
        .await;

        // 6. C/C++
        test_one(
            &pm,
            &temp_dir,
            "test.cpp",
            "int myCppFunc() { return 0; }\n#define MY_MACRO 42",
            "functions",
            "myCppFunc",
        )
        .await;
        test_one(
            &pm,
            &temp_dir,
            "test.cpp",
            "int myCppFunc() { return 0; }\n#define MY_MACRO 42",
            "macros",
            "MY_MACRO",
        )
        .await;

        // 7. Bash
        test_one(
            &pm,
            &temp_dir,
            "test.sh",
            "my_bash_func() {\n  echo 'hello'\n}",
            "functions",
            "my_bash_func",
        )
        .await;
    }

    #[tokio::test]
    async fn test_inspect_nix_custom_query() {
        let _lock = crate::tools::TEST_DB_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let temp_dir = std::env::temp_dir().join("test_inspect_nix_custom_query");
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let wasm_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("resources")
            .join("wasm");
        let pm = Arc::new(
            ParserManager::with_paths(temp_dir.join("cache"), temp_dir.join("compiler"), wasm_dir)
                .unwrap(),
        );

        let file_path = temp_dir.join("test.nix");
        std::fs::write(&file_path, "{ x = 1; }").unwrap();

        // 1. Test using 'file' field
        let args_file = InspectArgs {
            filepath: file_path.to_string_lossy().to_string(),
            query: Some("(binding attrpath: (attrpath (identifier) @attr))".to_string()),
            template: None,
            include_code: Some(true),
        };
        let text1 = run_inspect(args_file, &pm).await.unwrap().report;
        assert!(text1.contains("x"));

        // 2. Test using 'filepath' alias field (deserialized manually in code or via serde)
        let json_input = serde_json::json!({
            "filepath": file_path.to_string_lossy().to_string(),
            "query": "(binding attrpath: (attrpath (identifier) @attr))"
        });
        let args_filepath: InspectArgs = serde_json::from_value(json_input).unwrap();
        let text2 = run_inspect(args_filepath, &pm).await.unwrap().report;
        assert!(text2.contains("x"));
    }
}
