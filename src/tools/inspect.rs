use std::sync::Arc;
use std::fs;
use std::path::Path;
use anyhow::{Result, Context, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tree_sitter::{Query, QueryCursor, StreamingIterator};
use std::hash::{Hash, Hasher};

use crate::parser::ParserManager;
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

    // JIT edit session pre-caching
    let mut session_conn = None;
    let mut session_id_opt = None;
    match crate::tools::session_db::init_edit_session(&args.file, false) {
        Ok(meta) => {
            crate::tools::session_db::start_background_hash_worker(meta.session_id.clone());
            session_id_opt = Some(meta.session_id);
            match crate::tools::session_db::get_db_connection() {
                Ok(conn) => {
                    session_conn = Some(conn);
                }
                Err(e) => {
                    tracing::warn!("Failed to open DB connection for inspect pre-caching: {:?}", e);
                }
            }
        }
        Err(e) => {
            tracing::warn!("Failed to initialize edit session for inspect pre-caching: {:?}", e);
        }
    }

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

    if args.template.is_some() && query_str.is_none() {
        status = "warning".to_string();
        hint = Some(format!(
            "Template '{}' is not supported for language '{}'. Supported templates: functions, classes, imports (rust, python).",
            args.template.as_ref().unwrap(),
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
                                if let (Some(ref conn), Some(ref session_id)) = (&session_conn, &session_id_opt) {
                                    match format_definition_table(conn, session_id, start_line, end_line) {
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
    conn: &rusqlite::Connection,
    session_id: &str,
    start_line: usize,
    end_line: usize,
) -> Result<String> {
    crate::tools::session_db::ensure_hashes_for_range(conn, session_id, start_line, end_line)?;

    let limit = if end_line >= start_line { end_line - start_line + 1 } else { 0 };
    let offset = start_line.saturating_sub(1);

    let mut stmt = conn.prepare(
        "SELECT sequence_id, line_hash, content FROM lines WHERE session_id = ?1 ORDER BY sort_order LIMIT ?2 OFFSET ?3"
    )?;

    let mut rows = stmt.query(rusqlite::params![session_id, limit, offset])?;
    let mut items = Vec::new();

    let mut current_idx = start_line;
    while let Some(row) = rows.next()? {
        let seq_id: i64 = row.get(0)?;
        let line_hash_opt: Option<String> = row.get(1)?;
        let content: String = row.get(2)?;
        
        let line_hash = line_hash_opt.unwrap_or_else(|| crate::tools::session_db::compute_line_hash(&content));
        let hex_seq = format!("{:x}", seq_id);
        items.push(serde_json::json!([
            format!("{}#{}", hex_seq, line_hash),
            current_idx,
            content
        ]));
        current_idx += 1;
    }

    let result_val = serde_json::json!({
        "columns": ["id", "n", "content"],
        "lines": items
    });
    let output = serde_json::to_string_pretty(&result_val)?;
    Ok(output)
}
