//! Parsing source with the grammar for its extension, and the name of what
//! encloses each of its lines.
//!
//! Every grammar is compiled in (D-01M28RAGW19ZZC), so this holds no engine, no
//! store and no directory: a parse is a parser with a language set on it. One
//! parser is kept between calls because setting a language costs more than
//! parsing a small file, and most runs stay in one language.

use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::Mutex as TokioMutex;
use tree_sitter::{Language, Parser};

use crate::config;

/// The language a kept parser is set to, the parser, and the grammar it holds.
type ActiveSession = Option<(&'static str, Parser, Language)>;

pub struct ParserManager {
    active_session: Arc<TokioMutex<ActiveSession>>,
}

impl Default for ParserManager {
    fn default() -> Self {
        Self {
            active_session: Arc::new(TokioMutex::new(None)),
        }
    }
}

impl ParserManager {
    pub fn new() -> Result<Self> {
        Ok(Self::default())
    }

    /// Parse `code` with the grammar for `ext`, reusing the parser when the
    /// language has not changed.
    pub async fn parse_code(&self, ext: &str, code: &str) -> Result<(tree_sitter::Tree, Language)> {
        let grammar = config::grammar_for_extension(ext)
            .with_context(|| format!("Unsupported file extension: {}", ext))?;
        let language = grammar
            .language()
            .with_context(|| format!("No tree-sitter grammar for {}", grammar.name))?;

        let mut session = self.active_session.lock().await;

        if let Some((active, ref mut parser, ref kept)) = *session {
            if active == grammar.name {
                let tree = parser
                    .parse(code, None)
                    .context("Failed to parse code in the kept parser")?;
                return Ok((tree, kept.clone()));
            }
        }

        let mut parser = Parser::new();
        parser
            .set_language(&language)
            .with_context(|| format!("Failed to set the {} grammar on a parser", grammar.name))?;
        let tree = parser
            .parse(code, None)
            .context("Failed to parse code after setting the grammar")?;

        *session = Some((grammar.name, parser, language.clone()));
        Ok((tree, language))
    }
}

/// A parse with a parser of its own, for a caller that is not in an async
/// context. `ParserManager` keeps one parser between calls and needs a lock to
/// do it; this pays for a parser instead.
fn parse_code_sync(ext: &str, code: &str) -> Result<tree_sitter::Tree> {
    let grammar = config::grammar_for_extension(ext)
        .with_context(|| format!("Unsupported file extension: {}", ext))?;
    let language = grammar
        .language()
        .with_context(|| format!("No tree-sitter grammar for {}", grammar.name))?;

    let mut parser = Parser::new();
    parser.set_language(&language)?;
    let tree = parser.parse(code, None).context("Failed to parse code")?;
    Ok(tree)
}

fn get_context_name(node: tree_sitter::Node, code: &str) -> Option<String> {
    let kind = node.kind();
    let name_opt = node.child_by_field_name("name").map(|n| {
        let bytes = n.byte_range();
        if bytes.end <= code.len() {
            code[bytes].trim().to_string()
        } else {
            String::new()
        }
    });

    match kind {
        "function_definition"
        | "function_item"
        | "function_declaration"
        | "method_declaration"
        | "method_definition" => {
            let name = name_opt.unwrap_or_else(|| "anonymous".to_string());
            Some(format!("fn:{}", name))
        }
        "class_definition" | "class_declaration" => {
            let name = name_opt.unwrap_or_else(|| "anonymous".to_string());
            Some(format!("class:{}", name))
        }
        "struct_item" => {
            let name = name_opt.unwrap_or_else(|| "anonymous".to_string());
            Some(format!("struct:{}", name))
        }
        "trait_item" => {
            let name = name_opt.unwrap_or_else(|| "anonymous".to_string());
            Some(format!("trait:{}", name))
        }
        "impl_item" => {
            let range = node.byte_range();
            if range.end <= code.len() {
                if let Some(first_line) = code[range].lines().next() {
                    let header = first_line.trim_end_matches('{').trim().to_string();
                    Some(header)
                } else {
                    Some("impl".to_string())
                }
            } else {
                Some("impl".to_string())
            }
        }
        _ => None,
    }
}

pub(crate) fn compute_parent_contexts(filepath: &str, content: &str) -> Vec<Option<String>> {
    let line_count = content.split('\n').count();
    let mut contexts = vec![None; line_count];

    let ext = std::path::Path::new(filepath)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    if ext == "md" || ext == "markdown" {
        let mut current_header = None;
        for (idx, line) in content.split('\n').enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with('#') {
                let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
                if parts.len() == 2 && parts[0].chars().all(|c| c == '#') {
                    contexts[idx] = current_header.clone();
                    current_header = Some(trimmed.to_string());
                    continue;
                }
            }
            contexts[idx] = current_header.clone();
        }
        return contexts;
    }

    // A file no grammar covers has no parent contexts and that is the answer,
    // not a failure: asking anyway logged an error on every view of a text
    // file, twice a call, in the stream a caller watches for real ones.
    if config::grammar_for_extension(&ext).is_none() {
        return contexts;
    }

    match parse_code_sync(&ext, content) {
        Err(e) => {
            tracing::debug!("parse_code_sync failed for .{}: {:?}", ext, e);
        }
        Ok(tree) => {
            let mut visit_stack = vec![(tree.root_node(), None)];
            while let Some((node, active_context)) = visit_stack.pop() {
                let next_context = if let Some(name) = get_context_name(node, content) {
                    Some(name)
                } else {
                    active_context
                };

                if let Some(ref ctx_name) = next_context {
                    let start_row = node.start_position().row;
                    let end_row = node.end_position().row;
                    // Reached through `get_mut`, so the row a node ends on
                    // needs no comparison standing in for the bounds.
                    for r in start_row..=end_row {
                        if let Some(slot) = contexts.get_mut(r) {
                            *slot = Some(ctx_name.clone());
                        }
                    }
                }

                let count = node.child_count();
                for i in (0..count).rev() {
                    if let Some(child) = node.child(i as u32) {
                        visit_stack.push((child, next_context.clone()));
                    }
                }
            }
        }
    }

    contexts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn parses_with_the_grammar_for_the_extension() {
        let manager = ParserManager::new().unwrap();
        let (tree, _language) = manager
            .parse_code("rs", "fn named() {}\n")
            .await
            .expect("rust parses");
        assert_eq!(tree.root_node().kind(), "source_file");
        assert!(!tree.root_node().has_error());
    }

    #[tokio::test]
    async fn keeps_the_parser_across_languages() {
        let manager = ParserManager::new().unwrap();
        manager.parse_code("rs", "fn a() {}\n").await.unwrap();
        let (tree, _) = manager
            .parse_code("py", "def a():\n    pass\n")
            .await
            .expect("python parses after rust");
        assert_eq!(tree.root_node().kind(), "module");
        let (tree, _) = manager.parse_code("rs", "fn b() {}\n").await.unwrap();
        assert_eq!(tree.root_node().kind(), "source_file");
    }

    #[tokio::test]
    async fn refuses_an_extension_it_carries_no_grammar_for() {
        let manager = ParserManager::new().unwrap();
        let result = manager.parse_code("unheardof", "anything\n").await;
        assert!(result.is_err());
    }
}
