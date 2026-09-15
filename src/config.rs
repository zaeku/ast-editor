//! Which languages the tool reads, and what reads them.
//!
//! Every grammar is compiled in at a version `Cargo.lock` pins
//! (D-01M28RAGW19ZZC), so this is one table rather than a file to find: an
//! extension names a language, a language carries its parser, and nothing has
//! to be installed beside the binary.

use anyhow::Result;
use tree_sitter::Language;
use tree_sitter_language::LanguageFn;

include!(concat!(env!("OUT_DIR"), "/grammar_versions.rs"));

/// A language the tool reads: what it is called, what parses it, and which
/// crate the parser came from.
pub(crate) struct Grammar {
    pub name: &'static str,
    pub extensions: &'static [&'static str],
    pub crate_name: &'static str,
    language: Option<LanguageFn>,
}

impl Grammar {
    /// The parser for this language, or `None` where the language has one that
    /// is not a tree-sitter grammar.
    pub(crate) fn language(&self) -> Option<Language> {
        self.language.map(Language::from)
    }

    /// The version of the grammar crate compiled in, as Cargo.lock pinned it.
    pub(crate) fn version(&self) -> &'static str {
        GRAMMAR_VERSIONS
            .iter()
            .find(|(package, _)| *package == self.crate_name)
            .map(|(_, version)| *version)
            .unwrap_or("unknown")
    }
}

/// One table, so that the language a file is read as and the parser that reads
/// it cannot disagree. Markdown is here without a grammar: comrak parses it.
pub(crate) static GRAMMARS: &[Grammar] = &[
    Grammar {
        name: "bash",
        extensions: &["sh", "bash", "zsh", "ksh"],
        crate_name: "tree-sitter-bash",
        language: Some(tree_sitter_bash::LANGUAGE),
    },
    Grammar {
        name: "c",
        extensions: &["c", "h"],
        crate_name: "tree-sitter-c",
        language: Some(tree_sitter_c::LANGUAGE),
    },
    Grammar {
        name: "cpp",
        extensions: &["cpp", "cc", "cxx", "hpp"],
        crate_name: "tree-sitter-cpp",
        language: Some(tree_sitter_cpp::LANGUAGE),
    },
    Grammar {
        name: "go",
        extensions: &["go"],
        crate_name: "tree-sitter-go",
        language: Some(tree_sitter_go::LANGUAGE),
    },
    Grammar {
        name: "html",
        extensions: &["html", "htm"],
        crate_name: "tree-sitter-html",
        language: Some(tree_sitter_html::LANGUAGE),
    },
    Grammar {
        name: "java",
        extensions: &["java"],
        crate_name: "tree-sitter-java",
        language: Some(tree_sitter_java::LANGUAGE),
    },
    Grammar {
        name: "javascript",
        extensions: &["js", "jsx", "mjs", "cjs"],
        crate_name: "tree-sitter-javascript",
        language: Some(tree_sitter_javascript::LANGUAGE),
    },
    Grammar {
        name: "json",
        extensions: &["json"],
        crate_name: "tree-sitter-json",
        language: Some(tree_sitter_json::LANGUAGE),
    },
    Grammar {
        name: "lua",
        extensions: &["lua"],
        crate_name: "tree-sitter-lua",
        language: Some(tree_sitter_lua::LANGUAGE),
    },
    Grammar {
        name: "markdown",
        extensions: &["md", "markdown"],
        crate_name: "comrak",
        language: None,
    },
    Grammar {
        name: "nix",
        extensions: &["nix"],
        crate_name: "tree-sitter-nix",
        language: Some(tree_sitter_nix::LANGUAGE),
    },
    Grammar {
        name: "python",
        extensions: &["py"],
        crate_name: "tree-sitter-python",
        language: Some(tree_sitter_python::LANGUAGE),
    },
    Grammar {
        name: "rust",
        extensions: &["rs"],
        crate_name: "tree-sitter-rust",
        language: Some(tree_sitter_rust::LANGUAGE),
    },
    Grammar {
        name: "swift",
        extensions: &["swift"],
        crate_name: "tree-sitter-swift",
        language: Some(tree_sitter_swift::LANGUAGE),
    },
    Grammar {
        name: "toml",
        extensions: &["toml"],
        crate_name: "tree-sitter-toml-ng",
        language: Some(tree_sitter_toml_ng::LANGUAGE),
    },
    Grammar {
        name: "tsx",
        extensions: &["tsx"],
        crate_name: "tree-sitter-typescript",
        language: Some(tree_sitter_typescript::LANGUAGE_TSX),
    },
    Grammar {
        name: "typescript",
        extensions: &["ts", "mts", "cts"],
        crate_name: "tree-sitter-typescript",
        language: Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT),
    },
    Grammar {
        name: "yaml",
        extensions: &["yaml", "yml"],
        crate_name: "tree-sitter-yaml",
        language: Some(tree_sitter_yaml::LANGUAGE),
    },
];

/// The language an extension is read as, with or without a leading dot.
pub(crate) fn grammar_for_extension(ext: &str) -> Option<&'static Grammar> {
    let wanted = ext.trim_start_matches('.').to_ascii_lowercase();
    GRAMMARS
        .iter()
        .find(|grammar| grammar.extensions.contains(&wanted.as_str()))
}

/// The name of the language an extension is read as.
pub(crate) fn language_for_extension(ext: &str) -> Option<&'static str> {
    grammar_for_extension(ext).map(|grammar| grammar.name)
}

/// Every language the binary carries, in the order `--version` reports them:
/// what it is called, the grammar's version, and the extensions it claims.
pub(crate) fn describe_languages() -> Result<Vec<(String, String, String)>> {
    Ok(GRAMMARS
        .iter()
        .map(|grammar| {
            (
                grammar.name.to_string(),
                grammar.version().to_string(),
                grammar
                    .extensions
                    .iter()
                    .map(|extension| format!(".{extension}"))
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        })
        .collect())
}
