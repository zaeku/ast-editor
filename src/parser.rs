//! Parsing source with the grammar for its extension.
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

pub struct ParserManager {
    /// The language the kept parser is set to, and the parser itself.
    active_session: Arc<TokioMutex<Option<(&'static str, Parser, Language)>>>,
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
