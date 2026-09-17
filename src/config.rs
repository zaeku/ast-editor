//! Which languages the tool reads, what reads them, and what it asks them.
//!
//! Every grammar is compiled in at a version `Cargo.lock` pins
//! (D-01M28RAGW19ZZC), so this is one table rather than a file to find: an
//! extension names a language, a language carries its parser, and nothing has
//! to be installed beside the binary.
//!
//! The queries are here for the same reason. A template name and an outline
//! both stand for an s-expression written against one grammar's own node
//! names, so adding a language means one file rather than two that have to be
//! kept in step.

use anyhow::{Context, Result};
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

/// The s-expression a named template stands for, or None when the language
/// has no such template.
pub(crate) fn template_query(lang: &str, template: &str) -> Option<String> {
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
        // C has no class_specifier, and naming a node a grammar does not have
        // is a query that will not compile rather than one that finds nothing.
        ("c", "classes") => Some("(struct_specifier) @class".to_string()),
        ("cpp", "classes") => Some("[(struct_specifier) (class_specifier)] @class".to_string()),
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

/// Every kind of definition a language declares, as one query with a capture
/// per kind. `outline` used to borrow the `classes` and `functions` templates,
/// which name what several languages have in common rather than what any one
/// of them declares: a Rust `enum`, a TypeScript `interface` and a C `typedef`
/// were each absent from a listing that says it holds the file's definitions.
///
/// Node names are the grammar's own, read from `outline --sexp` rather than
/// guessed. A name a grammar does not have is a query that will not compile,
/// and the caller below passes over a query that will not compile, so a wrong
/// name here is a language that silently lists nothing.
pub(crate) fn outline_query(lang: &str) -> Option<&'static str> {
    Some(match lang {
        "rust" => {
            "(function_item) @function
             (struct_item) @struct
             (union_item) @struct
             (enum_item) @enum
             (trait_item) @trait
             (type_item) @type
             (const_item) @const
             (static_item) @const
             (macro_definition) @macro
             (mod_item) @module"
        }
        "python" => {
            "(function_definition) @function
             (class_definition) @class"
        }
        "go" => {
            "(function_declaration) @function
             (method_declaration) @function
             (type_declaration (type_spec type: (interface_type))) @interface
             (type_declaration (type_spec type: (struct_type))) @struct
             (const_declaration) @const"
        }
        // TypeScript's own declarations on top of what JavaScript has. `tsx`
        // is the same grammar with JSX, so it takes the same list.
        "typescript" | "tsx" => {
            "(function_declaration) @function
             (method_definition) @function
             (class_declaration) @class
             (abstract_class_declaration) @class
             (interface_declaration) @interface
             (type_alias_declaration) @type
             (enum_declaration) @enum"
        }
        "javascript" => {
            "(function_declaration) @function
             (method_definition) @function
             (class_declaration) @class"
        }
        "java" => {
            "(method_declaration) @function
             (constructor_declaration) @function
             (class_declaration) @class
             (interface_declaration) @interface
             (enum_declaration) @enum
             (record_declaration) @record"
        }
        "c" => {
            "(function_definition) @function
             (struct_specifier) @struct
             (union_specifier) @struct
             (enum_specifier) @enum
             (type_definition) @type"
        }
        "cpp" => {
            "(function_definition) @function
             (struct_specifier) @struct
             (class_specifier) @class
             (union_specifier) @struct
             (enum_specifier) @enum
             (type_definition) @type
             (namespace_definition) @module"
        }
        // A struct and an enum are `class_declaration` in this grammar, so one
        // capture covers all three and the signature line tells them apart.
        "swift" => {
            "(function_declaration) @function
             (class_declaration) @class
             (protocol_declaration) @protocol"
        }
        "bash" => "(function_definition) @function",
        "lua" => "(function_declaration) @function",
        _ => return None,
    })
}

/// The language an extension is read as, from the one table that also carries
/// the parser for it (D-01M28RAGW19ZZC).
pub(crate) fn language_name(ext: &str) -> Result<String> {
    crate::config::language_for_extension(ext)
        .map(str::to_string)
        .with_context(|| format!("Unsupported extension: {}", ext))
}
