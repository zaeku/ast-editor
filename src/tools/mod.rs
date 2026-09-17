pub(crate) mod buffer;
pub(crate) mod create;
pub(crate) mod edit;
pub(crate) mod file_entry;
pub(crate) mod formatter;
pub(crate) mod inspect;
pub(crate) mod line_id;
pub(crate) mod markdown;
pub(crate) mod metadata;
pub(crate) mod outline;
pub(crate) mod repository;
pub(crate) mod script;
pub(crate) mod store;
pub(crate) mod syntax;
pub(crate) mod view;

// Drives the tools through the crate rather than through the command, which is
// what lets a caller reach a path no CLI invocation reaches deterministically —
// the index falling out of step with a file being written under it. Inside the
// crate rather than under tests/ so that nothing has to be `pub` to be tested.
#[cfg(test)]
mod file_entry_tests;
#[cfg(test)]
mod line_edit_tests;

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::sync::Arc;

use crate::parser::ParserManager;
use crate::tools::formatter::{fence_language, fenced};

/// A temporary directory for a test, named so that no two processes share one.
/// A mutation run builds and tests many copies of this library at once, and
/// every copy resolves the same names; a directory they had in common made one
/// test's cleanup another test's failure, and the run read that failure as the
/// mutant being caught. The process id alone is reused, so the time goes in
/// beside it.
pub(crate) fn test_temp_dir(name: &str) -> std::path::PathBuf {
    use std::sync::OnceLock;
    static SUFFIX: OnceLock<String> = OnceLock::new();
    let suffix = SUFFIX.get_or_init(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        format!("{}-{}", std::process::id(), nanos)
    });
    std::env::temp_dir().join(format!("{name}-{suffix}"))
}

pub(crate) struct ToolDispatcher;

impl ToolDispatcher {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl Default for ToolDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolDispatcher {
    /// Returns the JSON schema array of available tools.
    pub(crate) fn list_tools(&self) -> Vec<Value> {
        metadata::tools()
            .iter()
            .map(|tool| {
                serde_json::json!({
                    "name": tool.name,
                    "description": tool.description,
                    "inputSchema": tool.input_schema,
                })
            })
            .collect()
    }

    /// Invokes the appropriate tool based on name and deserialized arguments.
    /// Run one tool and return the text it prints.
    pub(crate) async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        parser_manager: &Arc<ParserManager>,
    ) -> Result<String> {
        match name {
            "inspect" => {
                let args: inspect::InspectArgs = serde_json::from_value(arguments)?;
                let language = fence_language(&args.filepath);
                let answer = inspect::run_inspect(args, parser_manager).await?;
                let mut blocks: Vec<String> = answer
                    .blocks
                    .iter()
                    .map(|block| fenced(language, block))
                    .collect();
                blocks.push(fenced("json", &answer.report));
                Ok(blocks.join("\n"))
            }
            "outline" => {
                let args: outline::OutlineArgs = serde_json::from_value(arguments)?;
                let sexp = args.sexp.unwrap_or(false);
                let text = outline::run_outline(args, parser_manager).await?;
                Ok(fenced(if sexp { "text" } else { "json" }, &text))
            }
            "view" => {
                let filepath = arguments
                    .get("filepath")
                    .and_then(|v| v.as_str())
                    .context("Missing filepath")?;
                let start_line = arguments
                    .get("start_line")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as usize);
                let end_line = arguments
                    .get("end_line")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as usize);
                let only_ids = arguments.get("only_ids").and_then(|v| v.as_bool());
                let query = arguments
                    .get("query")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let context_lines = arguments
                    .get("context_lines")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as usize);
                let fixed_string = arguments.get("fixed_string").and_then(|v| v.as_bool());

                // One file answers as it always has. Several answer with one
                // block each and a json block that says which is which, so
                // skimming a directory is one call (card #8).
                let mut paths = vec![filepath.to_string()];
                if let Some(more) = arguments.get("filepaths").and_then(|v| v.as_array()) {
                    paths.extend(more.iter().filter_map(|v| v.as_str()).map(str::to_string));
                }

                let repository = repository::SqliteFileStore;
                let mut blocks = Vec::new();
                let mut per_file = Vec::new();

                for path in &paths {
                    let res = view::view_lines(
                        &repository,
                        path,
                        start_line,
                        end_line,
                        only_ids,
                        query.clone(),
                        context_lines,
                        fixed_string,
                    )
                    .with_context(|| format!("reading {}", path))?;

                    if let Some(code) = res.lines_text {
                        blocks.push(fenced(fence_language(path), &code));
                    }
                    if paths.len() == 1 {
                        blocks.push(fenced("json", &res.metadata_json));
                    } else {
                        let mut entry: serde_json::Value =
                            serde_json::from_str(&res.metadata_json)?;
                        if let Some(object) = entry.as_object_mut() {
                            object.insert(
                                "filepath".to_string(),
                                serde_json::Value::String(path.clone()),
                            );
                        }
                        per_file.push(entry);
                    }
                }

                if paths.len() > 1 {
                    blocks.push(fenced(
                        "json",
                        &serde_json::to_string_pretty(&serde_json::json!({ "files": per_file }))?,
                    ));
                }
                Ok(blocks.join("\n"))
            }
            "edit" => {
                let filepath = arguments
                    .get("filepath")
                    .and_then(|v| v.as_str())
                    .context("Missing filepath")?;
                let dry_run = arguments
                    .get("dry_run")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let repository = repository::SqliteFileStore;

                let text = if let Some(preview_id) = arguments.get("apply").and_then(|v| v.as_str())
                {
                    edit::apply_preview(&repository, filepath, preview_id, parser_manager).await?
                } else {
                    let edits_val = arguments.get("edits").context("Missing edits array")?;
                    let edits: Vec<line_id::LineEdit> = serde_json::from_value(edits_val.clone())?;
                    if dry_run {
                        // The diff is the point of a dry run, so it is a block
                        // to read rather than a string to unescape.
                        let preview =
                            edit::edit_lines_dry_run(&repository, filepath, edits, parser_manager)
                                .await?;
                        return Ok(format!(
                            "{}\n{}",
                            fenced("diff", preview.diff.trim_end()),
                            fenced("json", &preview.report)
                        ));
                    }
                    edit::edit_lines(&repository, filepath, edits, parser_manager).await?
                };
                Ok(fenced("json", &text))
            }
            "create" => {
                let filepath = arguments
                    .get("filepath")
                    .and_then(|v| v.as_str())
                    .context("Missing filepath")?;
                let content = arguments
                    .get("content")
                    .and_then(|v| v.as_str())
                    .context("Missing content")?;
                let return_ids = arguments.get("return_ids").and_then(|v| v.as_bool());
                let repository = repository::SqliteFileStore;
                let text = create::create_lines(&repository, filepath, content, return_ids)?;
                Ok(fenced("json", &text))
            }
            _ => bail!("Unknown tool: {}", name),
        }
    }
}

/// Serialises the tests that reach the store, which share one process and one
/// cache directory under `cfg(test)`.
#[cfg(test)]
pub(crate) static TEST_DB_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
