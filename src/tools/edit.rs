pub use crate::tools::session_db::LineEdit;
use crate::tools::session_db::{check_language_supported, SessionRepository};
use anyhow::Result;
use std::fs;

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

                if start_idx < lines.len() && end_idx < lines.len() {
                    let start_line_str = lines[start_idx];
                    let trimmed = start_line_str.trim_start();
                    let start_lead_spaces =
                        start_line_str.len() - start_line_str.trim_start().len();
                    let fence_char = if trimmed.starts_with('`') {
                        Some('`')
                    } else if trimmed.starts_with('~') {
                        Some('~')
                    } else {
                        None
                    };

                    if let Some(fc) = fence_char {
                        let fence_len = trimmed.chars().take_while(|&c| c == fc).count();
                        if fence_len >= 3 {
                            let end_line_str = lines[end_idx];
                            let end_trimmed = end_line_str.trim_end_matches('\r').trim_start();
                            let end_lead_spaces =
                                end_line_str.len() - end_line_str.trim_start().len();

                            let is_valid_closing_fence = if end_lead_spaces <= start_lead_spaces + 3
                                && end_trimmed.starts_with(fc)
                            {
                                let end_fence_len =
                                    end_trimmed.chars().take_while(|&c| c == fc).count();
                                let remainder = &end_trimmed[end_fence_len..];
                                end_fence_len >= fence_len && remainder.trim().is_empty()
                            } else {
                                false
                            };

                            if !is_valid_closing_fence {
                                anyhow::bail!("Validation error: Unclosed fenced code block starting at line {}", start_line);
                            }
                        }
                    }
                } else {
                    anyhow::bail!("Validation error: Fenced code block source position is out of bounds. Start: {}, End: {}", start_line, end_line);
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
                        .replace("{}", &(line_num + 1).to_string())
                        .replace("{}", &level.to_string())
                        .replace("{}", &last_level.to_string())
                        .replace("{}", &(last_level + 1).to_string());
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
                .replace("{}", &(line_num + 1).to_string());
            warnings.push(msg);
        }
    }

    warnings
}

enum SyntaxValidationResult {
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
    for idx in start..=end {
        let line_num = idx + 1;
        let prefix = if idx == error_row {
            format!("{:>4}: --> ", line_num)
        } else {
            format!("{:>4}:     ", line_num)
        };
        if idx < lines.len() {
            context.push(format!("{}{}", prefix, lines[idx]));
        }
    }
    context
}

async fn validate_syntax(
    filepath: &str,
    content: &str,
    parser_manager: &crate::parser::ParserManager,
) -> SyntaxValidationResult {
    let ext = std::path::Path::new(filepath)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    if ext == "md" || ext == "markdown" {
        let warnings = lint_markdown(content);
        if warnings.is_empty() {
            return SyntaxValidationResult::Success;
        } else {
            return SyntaxValidationResult::Warnings(warnings);
        }
    }

    if !check_language_supported(filepath) {
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

/// Reconcile the session with disk when the file changed under us, so that an
/// edit or a preview operates on the current state.
fn resync_if_stale(
    repository: &impl SessionRepository,
    filepath: &str,
    edits: &[LineEdit],
) -> Result<()> {
    // Reconcile before the read path can, so the lines this batch targets are
    // checked for having survived rather than silently re-identified.
    let Some(session_id) = repository.get_session_id(filepath)? else {
        return Ok(());
    };

    let mut start_ids = Vec::new();
    for edit in edits {
        for id in [&edit.start_id, &edit.end_id, &edit.dest_id]
            .into_iter()
            .flatten()
        {
            start_ids.push(id.clone());
        }
    }

    repository.smart_resync(filepath, &session_id, &start_ids)
}

/// Preview an edit batch without touching disk or the session store.
///
/// The batch is applied to the session, the resulting content is validated the
/// same way a real commit is, and the session is then restored to its previous
/// state. No line IDs are minted: the caller obtains those from a real
/// `edit` call.
/// What a dry run has to say: the diff to read, and the report to act on.
/// They are separate because a diff inside a JSON string is a diff nobody can
/// read.
pub struct DryRun {
    pub diff: String,
    pub report: String,
}

pub async fn edit_lines_dry_run(
    repository: &impl SessionRepository,
    filepath: &str,
    edits: Vec<LineEdit>,
    parser_manager: &crate::parser::ParserManager,
) -> Result<DryRun> {
    resync_if_stale(repository, filepath, &edits)?;

    let meta = repository.init_session(filepath, false)?;
    let session_id = meta.session_id;

    let line_ending = if repository.get_session_crlf(&session_id)? {
        "\r\n"
    } else {
        "\n"
    };
    let original_content = fs::read_to_string(filepath)?;

    // A preview is a plan that is never committed, so there is nothing to undo.
    let (buffer, _) = repository.plan_line_edits(&session_id, &edits)?;
    let preview_content = buffer.join(line_ending);

    let diff = similar::TextDiff::from_lines(&original_content, &preview_content)
        .unified_diff()
        .context_radius(3)
        .header(filepath, filepath)
        .to_string();

    let output = match validate_syntax(filepath, &preview_content, parser_manager).await {
        SyntaxValidationResult::Success => serde_json::json!({
            "syntax_valid": true,
            "preview_id": repository.create_preview(filepath, &edits)?,
        }),
        SyntaxValidationResult::NotChecked => serde_json::json!({
            "syntax_valid": serde_json::Value::Null,
            "message": crate::tools::metadata::get_config().message_not_checked,
            "preview_id": repository.create_preview(filepath, &edits)?,
        }),
        SyntaxValidationResult::Warnings(warnings) => serde_json::json!({
            "syntax_valid": true,
            "warnings": warnings,
            "preview_id": repository.create_preview(filepath, &edits)?,
        }),
        SyntaxValidationResult::SyntaxErrors {
            errors,
            contexts,
            _raw_ast: _,
        } => {
            let diagnostics: Vec<_> = errors.iter().zip(contexts.iter())
                .map(|(message, context)| serde_json::json!({ "message": message, "context": context }))
                .collect();
            // An id even here: the verdict travels beside it, and a caller who
            // judges the parser wrong applies the batch rather than retyping it
            // (D-01M28NM3ECNY08).
            serde_json::json!({
                "syntax_valid": false,
                "diagnostics": diagnostics,
                "preview_id": repository.create_preview(filepath, &edits)?,
            })
        }
        SyntaxValidationResult::InfrastructureFailure(reason) => serde_json::json!({
            "syntax_valid": serde_json::Value::Null,
            "message": format!("validation could not run: {}", reason),
            "preview_id": repository.create_preview(filepath, &edits)?,
        }),
    };

    Ok(DryRun {
        diff,
        report: serde_json::to_string_pretty(&output)?,
    })
}

/// Apply the edit batch a previous dry run validated, addressed by its preview
/// id instead of resent in full.
pub async fn apply_preview(
    repository: &impl SessionRepository,
    filepath: &str,
    preview_id: &str,
    strict_validation: bool,
    parser_manager: &crate::parser::ParserManager,
) -> Result<String> {
    let edits = repository.take_preview(filepath, preview_id)?;
    edit_lines_with_validation(
        repository,
        filepath,
        edits,
        strict_validation,
        parser_manager,
    )
    .await
}

pub async fn edit_lines_with_validation(
    repository: &impl SessionRepository,
    filepath: &str,
    edits: Vec<LineEdit>,
    strict_validation: bool,
    parser_manager: &crate::parser::ParserManager,
) -> Result<String> {
    resync_if_stale(repository, filepath, &edits)?;

    let meta = repository.init_session(filepath, false)?;
    let session_id = meta.session_id;

    let line_ending = if repository.get_session_crlf(&session_id)? {
        "\r\n"
    } else {
        "\n"
    };

    // Plan the batch without persisting it. Nothing is committed until the
    // content it produces has been accepted, so a rejected edit needs no undo.
    let (buffer, newly_modified_lines) = repository.plan_line_edits(&session_id, &edits)?;
    // Each id with the line it is now, walked off the planned buffer: an id
    // alone does not say where its line went (card #5).
    let newly_modified_lines = buffer.locate(&newly_modified_lines);
    let final_content = buffer.join(line_ending);

    // Validate syntax
    let validation = validate_syntax(filepath, &final_content, parser_manager).await;

    let mut checked = true;
    let warnings = match validation {
        SyntaxValidationResult::Success => None,
        SyntaxValidationResult::NotChecked => {
            checked = false;
            None
        }
        SyntaxValidationResult::Warnings(warns) => Some(warns),
        SyntaxValidationResult::SyntaxErrors {
            errors,
            contexts,
            _raw_ast: _,
        } => {
            if strict_validation {
                // A refusal in the shape of a dry run: the same verdict, the
                // same diagnostics, and the batch kept under an id, so a caller
                // who judges the parser wrong applies it rather than sending it
                // again (D-01M28NM3ECNY08). The exit code still says nothing
                // happened (D-01M28HCSAMTEFS).
                let diagnostics: Vec<_> = errors
                    .iter()
                    .zip(contexts.iter())
                    .map(|(message, context)| {
                        serde_json::json!({ "message": message, "context": context })
                    })
                    .collect();
                let kept = repository.create_preview(filepath, &edits)?;
                let report = serde_json::json!({
                    "syntax_valid": false,
                    "diagnostics": diagnostics,
                    "preview_id": kept,
                    "hint": crate::tools::metadata::get_config()
                        .error_strict_refused
                        .replacen("{}", filepath, 1)
                        .replacen("{}", &kept, 1),
                });
                anyhow::bail!("```json\n{}\n```", serde_json::to_string_pretty(&report)?);
            } else {
                // Permissive Mode: Write to disk and save, but return status "saved_with_errors" with details
                fs::write(filepath, &final_content)?;
                repository.commit_buffer(&session_id, &buffer)?;
                repository.smart_resync(filepath, &session_id, &[])?;

                let config = crate::tools::metadata::get_config();
                let only_ids_wrap_trigger_length = config.only_ids_wrap_trigger_length;
                let formatted_ids = crate::tools::formatter::format_lines(
                    &newly_modified_lines,
                    only_ids_wrap_trigger_length,
                );
                let mut indented_ids = String::new();
                for (i, line) in formatted_ids.lines().enumerate() {
                    if i == 0 {
                        indented_ids.push_str(line);
                    } else {
                        indented_ids.push_str("\n  ");
                        indented_ids.push_str(line);
                    }
                }

                // Construct diagnostic JSON for permissive output
                let mut diagnostics = Vec::new();
                for (msg, ctx) in errors.iter().zip(contexts.iter()) {
                    diagnostics.push(serde_json::json!({
                        "message": msg,
                        "context": ctx,
                    }));
                }

                let output = format!(
                    "{{\n  \"status\": \"saved_with_errors\",\n  \"modified_lines\": {},\n  \"syntax_valid\": false,\n  \"diagnostics\": {}\n}}",
                    indented_ids,
                    serde_json::to_string_pretty(&diagnostics)?
                );
                return Ok(output);
            }
        }
        SyntaxValidationResult::InfrastructureFailure(reason) => {
            if strict_validation {
                let kept = repository.create_preview(filepath, &edits)?;
                let report = serde_json::json!({
                    "syntax_valid": serde_json::Value::Null,
                    "preview_id": kept,
                    "hint": crate::tools::metadata::get_config()
                        .error_strict_unevaluable
                        .replacen("{}", &reason, 1)
                        .replacen("{}", filepath, 1)
                        .replacen("{}", &kept, 1),
                });
                anyhow::bail!("```json\n{}\n```", serde_json::to_string_pretty(&report)?);
            } else {
                // Permissive Mode: Write to disk, but return "saved" with error reason message
                fs::write(filepath, &final_content)?;
                repository.commit_buffer(&session_id, &buffer)?;
                repository.smart_resync(filepath, &session_id, &[])?;

                let config = crate::tools::metadata::get_config();
                let only_ids_wrap_trigger_length = config.only_ids_wrap_trigger_length;
                let formatted_ids = crate::tools::formatter::format_lines(
                    &newly_modified_lines,
                    only_ids_wrap_trigger_length,
                );
                let mut indented_ids = String::new();
                for (i, line) in formatted_ids.lines().enumerate() {
                    if i == 0 {
                        indented_ids.push_str(line);
                    } else {
                        indented_ids.push_str("\n  ");
                        indented_ids.push_str(line);
                    }
                }

                let message = serde_json::to_string(&format!(
                    "saved (validation failed because: {})",
                    reason
                ))?;
                let output = format!(
                    "{{\n  \"status\": \"saved\",\n  \"modified_lines\": {},\n  \"message\": {}\n}}",
                    indented_ids, message
                );
                return Ok(output);
            }
        }
    };

    // Save to disk
    fs::write(filepath, &final_content)?;
    repository.commit_buffer(&session_id, &buffer)?;

    // Resync database session to update parent contexts, line hashes, and mtime/file_hash metadata
    repository.smart_resync(filepath, &session_id, &[])?;

    let config = crate::tools::metadata::get_config();
    let only_ids_wrap_trigger_length = config.only_ids_wrap_trigger_length;
    let formatted_ids =
        crate::tools::formatter::format_lines(&newly_modified_lines, only_ids_wrap_trigger_length);
    let mut indented_ids = String::new();
    for (i, line) in formatted_ids.lines().enumerate() {
        if i == 0 {
            indented_ids.push_str(line);
        } else {
            indented_ids.push_str("\n  ");
            indented_ids.push_str(line);
        }
    }

    // A field appears when it has something to say: an edit that parsed says
    // nothing, and one nothing could read says so (card #1).
    let mut fields = vec![format!("\"modified_lines\": {}", indented_ids)];
    if let Some(warns) = warnings {
        fields.push(format!("\"warnings\": {}", serde_json::to_string(&warns)?));
    }
    if !checked {
        fields.push("\"syntax_valid\": null".to_string());
        fields.push(format!(
            "\"message\": {}",
            serde_json::to_string(&crate::tools::metadata::get_config().message_not_checked)?
        ));
    }
    Ok(format!("{{\n  {}\n}}", fields.join(",\n  ")))
}

pub async fn edit_lines(
    repository: &impl SessionRepository,
    filepath: &str,
    edits: Vec<LineEdit>,
    parser_manager: &crate::parser::ParserManager,
) -> Result<String> {
    edit_lines_with_validation(repository, filepath, edits, false, parser_manager).await
}

#[cfg(test)]
#[allow(clippy::await_holding_lock)]
mod tests {
    use super::*;
    use crate::parser::ParserManager;
    use crate::tools::session_db::{
        init_edit_session, parse_line_id, EditOp, MovePosition, SqliteSessionRepository,
    };
    use crate::tools::TEST_DB_LOCK as DB_LOCK;

    /// A temporary directory to put fixtures in. The grammars are compiled in
    /// (D-01M28RAGW19ZZC), so nothing else has to be arranged.
    struct TestEnvironment {
        dir: std::path::PathBuf,
        pm: ParserManager,
    }

    impl TestEnvironment {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("tree_sitter_edit_tests_{}", name));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self {
                dir,
                pm: ParserManager::new().unwrap(),
            }
        }
    }

    impl Drop for TestEnvironment {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    /// The id of the line whose text matches, read off the line that carries it.
    fn find_line_id(view_res: &crate::tools::view::ViewLinesOutput, pattern: &str) -> String {
        let lines_text = view_res.lines_text.as_ref().unwrap();
        let mut current = String::new();
        for row in lines_text.lines() {
            if let Some((head, _)) = row.split_once(": ") {
                if let Some((id, _)) = head.split_once('|') {
                    current = id.trim().to_string();
                }
            }
            if row.contains(pattern) {
                return current;
            }
        }
        String::new()
    }

    fn test_view_lines(
        repository: &impl crate::tools::session_db::SessionRepository,
        filepath: &str,
        start_line: usize,
        end_line: usize,
        only_ids: Option<bool>,
    ) -> Result<crate::tools::view::ViewLinesOutput> {
        crate::tools::view::view_lines(
            repository,
            filepath,
            Some(start_line),
            Some(end_line),
            only_ids,
            None,
            None,
            None,
        )
    }

    #[tokio::test]
    async fn test_apply_line_edits_insert_update_delete() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let env = TestEnvironment::new("basic_ops");
        let repository = SqliteSessionRepository;

        let file_path = env.dir.join("code.rs");
        fs::write(&file_path, "fn main() {\n    let a = 1;\n}\n")?;
        let filepath_str = file_path.to_str().unwrap();

        let metadata = init_edit_session(filepath_str, false)?;
        assert_eq!(metadata.total_lines, 3);

        // Get the line IDs by viewing
        let lines_view = test_view_lines(&repository, filepath_str, 1, 3, None)?;
        let start_id = find_line_id(&lines_view, "let a = 1;");
        assert!(!start_id.is_empty());

        // 1. Test update
        let edits = vec![LineEdit {
            op: EditOp::Replace,
            start_id: Some(start_id.clone()),
            content: Some("    let a = 42;".to_string()),
            ..Default::default()
        }];
        let preview = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
        let res: serde_json::Value = serde_json::from_str(&preview)?;
        assert!(res["status"].is_null(), "success is the exit code: {}", res);
        let modified = res["modified_lines"].as_array().unwrap();
        let (seq, _) = parse_line_id(&start_id)?;
        let expected_prefix = format!("{:x}#", seq);
        // Each entry is [id, line] (card #5).
        assert!(modified
            .iter()
            .any(|entry| entry[0].as_str().unwrap().starts_with(&expected_prefix)));
        assert_eq!(
            fs::read_to_string(&file_path)?,
            "fn main() {\n    let a = 42;\n}\n"
        );

        // Refresh metadata/session
        let _metadata = init_edit_session(filepath_str, false)?;
        let lines_view = test_view_lines(&repository, filepath_str, 1, 3, None)?;
        let new_start_id = find_line_id(&lines_view, "let a = 42;");
        assert!(!new_start_id.is_empty());

        // 2. Test insert_after
        let edits = vec![LineEdit {
            op: EditOp::InsertAfter,
            start_id: Some(new_start_id.clone()),
            content: Some("    let b = 2;".to_string()),
            ..Default::default()
        }];
        let preview = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
        let res: serde_json::Value = serde_json::from_str(&preview)?;
        assert!(res["status"].is_null(), "success is the exit code: {}", res);
        let modified = res["modified_lines"].as_array().unwrap();
        assert_eq!(
            fs::read_to_string(&file_path)?,
            "fn main() {\n    let a = 42;\n    let b = 2;\n}\n"
        );

        // Refresh session to get latest target IDs
        let _ = init_edit_session(filepath_str, false)?;
        let lines_view = test_view_lines(&repository, filepath_str, 1, 4, None)?;
        let b_id = find_line_id(&lines_view, "let b = 2;");
        assert!(!b_id.is_empty());
        assert!(modified
            .iter()
            .any(|entry| entry[0].as_str().unwrap() == b_id));

        // 3. Test delete
        let edits = vec![LineEdit {
            op: EditOp::Delete,
            start_id: Some(b_id.clone()),
            content: None,
            ..Default::default()
        }];
        let _preview = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
        assert_eq!(
            fs::read_to_string(&file_path)?,
            "fn main() {\n    let a = 42;\n}\n"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_concurrency_smart_resync() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let env = TestEnvironment::new("concurrency");
        let repository = SqliteSessionRepository;

        let file_path = env.dir.join("code.rs");
        fs::write(&file_path, "fn main() {}\n")?;
        let filepath_str = file_path.to_str().unwrap();

        let _metadata = init_edit_session(filepath_str, false)?;

        // Modify file externally on disk, changing its mtime
        tokio::time::sleep(tokio::time::Duration::from_millis(1100)).await;
        fs::write(&file_path, "fn main() {\n    // changed externally\n}\n")?;

        let edits = vec![LineEdit {
            op: EditOp::InsertAfter,
            start_id: None,
            content: Some("// success after resync\n".to_string()),
            ..Default::default()
        }];

        let res = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
        assert!(res.contains("modified_lines"), "{}", res);

        // Verify disk content includes both the external change and our new edit!
        let content = fs::read_to_string(&file_path)?;
        assert!(content.contains("changed externally"));
        assert!(content.contains("success after resync"));

        Ok(())
    }

    #[tokio::test]
    async fn test_checksum_error() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let env = TestEnvironment::new("checksum");
        let repository = SqliteSessionRepository;

        let file_path = env.dir.join("code.rs");
        fs::write(&file_path, "fn main() {}\n")?;
        let filepath_str = file_path.to_str().unwrap();

        let _metadata = init_edit_session(filepath_str, false)?;

        let edits = vec![LineEdit {
            op: EditOp::Replace,
            start_id: Some("1#9999".to_string()), // Invalid hash prefix
            content: Some("fn main() { // updated }".to_string()),
            ..Default::default()
        }];

        let res = edit_lines(&repository, filepath_str, edits, &env.pm).await;
        assert!(res.is_err());
        let err_msg = res.unwrap_err().to_string();
        assert!(
            err_msg.contains("changed since you read it"),
            "Expected checksum error, got: {}",
            err_msg
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_syntax_validation_error_rolls_back() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let env = TestEnvironment::new("syntax_error");
        let repository = SqliteSessionRepository;

        let file_path = env.dir.join("code.rs");
        let initial_content = "fn main() {\n    let a = 1;\n}\n";
        fs::write(&file_path, initial_content)?;
        let filepath_str = file_path.to_str().unwrap();

        let _metadata = init_edit_session(filepath_str, false)?;

        // Find the line 2 ID
        let lines_view = test_view_lines(&repository, filepath_str, 1, 3, None)?;
        let start_id = find_line_id(&lines_view, "let a = 1;");
        assert!(!start_id.is_empty());

        // Apply edit that introduces syntax error (e.g. mismatched braces / parsing error)
        let edits = vec![LineEdit {
            op: EditOp::Replace,
            start_id: Some(start_id),
            content: Some("    let a = {;".to_string()), // Syntax error
            ..Default::default()
        }];

        // 1. Test Permissive Mode (strict_validation: false by default when calling edit_lines)
        let res = edit_lines(&repository, filepath_str, edits.clone(), &env.pm).await?;
        let val: serde_json::Value = serde_json::from_str(&res)?;
        assert_eq!(val["status"], "saved_with_errors");
        assert_eq!(val["syntax_valid"], false);
        assert!(val["diagnostics"].is_array());

        // Verify disk content was saved (i.e. changed)
        assert_eq!(
            fs::read_to_string(&file_path)?,
            "fn main() {\n    let a = {;\n}\n"
        );

        // Restore initial content for strict validation test
        fs::write(&file_path, initial_content)?;
        let _metadata_strict = init_edit_session(filepath_str, false)?;
        let lines_view_strict = test_view_lines(&repository, filepath_str, 1, 3, None)?;
        let start_id_strict = find_line_id(&lines_view_strict, "let a = 1;");
        let edits_strict = vec![LineEdit {
            op: EditOp::Replace,
            start_id: Some(start_id_strict),
            content: Some("    let a = {;".to_string()),
            ..Default::default()
        }];

        // 2. Test Strict Mode (strict_validation: true)
        let res_strict =
            edit_lines_with_validation(&repository, filepath_str, edits_strict, true, &env.pm)
                .await;
        assert!(res_strict.is_err());
        let err_msg = res_strict.unwrap_err().to_string();
        assert!(
            err_msg.contains("\"syntax_valid\": false"),
            "Expected the refusal to carry its verdict, got: {}",
            err_msg
        );
        assert!(
            err_msg.contains("preview_id"),
            "Expected the refusal to name the batch it kept, got: {}",
            err_msg
        );

        // Verify disk content was rolled back (i.e. remains unchanged)
        assert_eq!(fs::read_to_string(&file_path)?, initial_content);

        // Verify DB content was rolled back (re-query content of line 2)
        let lines_strict = repository.fetch_lines_range(&_metadata_strict.session_id, 2, 2)?;
        assert_eq!(lines_strict.len(), 1);
        assert_eq!(lines_strict[0].2.trim(), "let a = 1;");

        fs::remove_dir_all(&env.dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn test_insert_into_empty_file() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let env = TestEnvironment::new("empty_file");
        let repository = SqliteSessionRepository;

        let file_path = env.dir.join("code.rs");
        fs::write(&file_path, "")?;
        let filepath_str = file_path.to_str().unwrap();

        let metadata = init_edit_session(filepath_str, false)?;
        assert_eq!(metadata.total_lines, 0);

        let edits = vec![LineEdit {
            op: EditOp::Append,
            start_id: None,
            content: Some("fn main() {\n}".to_string()),
            ..Default::default()
        }];

        let preview = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
        let res: serde_json::Value = serde_json::from_str(&preview)?;
        assert!(res["status"].is_null(), "success is the exit code: {}", res);
        let modified = res["modified_lines"].as_array().unwrap();
        assert!(!modified.is_empty());
        assert_eq!(fs::read_to_string(&file_path)?, "fn main() {\n}\n");

        Ok(())
    }

    #[tokio::test]
    async fn test_case_insensitive_validation_and_deletion_preview() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let env = TestEnvironment::new("case_insensitive_and_delete");
        let repository = SqliteSessionRepository;

        // Use uppercase extension: .RS
        let file_path = env.dir.join("code.RS");
        fs::write(
            &file_path,
            "fn main() {\n    let a = 1;\n    let b = 2;\n    let c = 3;\n}\n",
        )?;
        let filepath_str = file_path.to_str().unwrap();

        let metadata = init_edit_session(filepath_str, false)?;
        assert_eq!(metadata.total_lines, 5);

        // Get the line IDs by viewing
        let lines_view = test_view_lines(&repository, filepath_str, 1, 5, None)?;
        let b_id = find_line_id(&lines_view, "let b = 2;");
        assert!(!b_id.is_empty());

        // Test delete on .RS file (verifies lowercase lookup/delegation works for uppercase extensions too)
        let edits = vec![LineEdit {
            op: EditOp::Delete,
            start_id: Some(b_id.clone()),
            content: None,
            ..Default::default()
        }];

        let preview = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
        let res: serde_json::Value = serde_json::from_str(&preview)?;
        assert!(res["status"].is_null(), "success is the exit code: {}", res);
        let modified = res["modified_lines"].as_array().unwrap();
        assert!(!modified.is_empty());

        let disk_content = fs::read_to_string(&file_path)?;
        assert!(disk_content.contains("let a = 1;"));
        assert!(disk_content.contains("let c = 3;"));
        assert!(!disk_content.contains("let b = 2;"));

        Ok(())
    }

    #[tokio::test]
    async fn test_append_operation_empty_file() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let env = TestEnvironment::new("append_empty");
        let repository = SqliteSessionRepository;

        let file_path = env.dir.join("code.rs");
        fs::write(&file_path, "")?;
        let filepath_str = file_path.to_str().unwrap();

        let metadata = init_edit_session(filepath_str, false)?;
        assert_eq!(metadata.total_lines, 0);

        let edits = vec![LineEdit {
            op: EditOp::Append,
            start_id: None,
            content: Some("pub fn foo() {}".to_string()),
            ..Default::default()
        }];

        let preview = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
        let res: serde_json::Value = serde_json::from_str(&preview)?;
        assert!(res["status"].is_null(), "success is the exit code: {}", res);
        let modified = res["modified_lines"].as_array().unwrap();
        assert!(!modified.is_empty());
        assert_eq!(fs::read_to_string(&file_path)?, "pub fn foo() {}\n");

        Ok(())
    }

    #[tokio::test]
    async fn test_append_operation_with_content() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = TestEnvironment::new("append_content");
        let repository = SqliteSessionRepository;

        let file_path = env.dir.join("code.rs");
        fs::write(&file_path, "pub fn foo() {}\n")?;
        let filepath_str = file_path.to_str().unwrap();

        let metadata = init_edit_session(filepath_str, false)?;
        assert_eq!(metadata.total_lines, 1);

        let edits = vec![LineEdit {
            op: EditOp::Append,
            start_id: None,
            content: Some("pub fn bar() -> i32 {\n    42\n}".to_string()),
            ..Default::default()
        }];

        let preview = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
        let res: serde_json::Value = serde_json::from_str(&preview)?;
        assert!(res["status"].is_null(), "success is the exit code: {}", res);
        let modified = res["modified_lines"].as_array().unwrap();
        assert!(!modified.is_empty());
        assert_eq!(
            fs::read_to_string(&file_path)?,
            "pub fn foo() {}\npub fn bar() -> i32 {\n    42\n}\n"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_strict_start_id_validation() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = TestEnvironment::new("strict_validation");
        let repository = SqliteSessionRepository;

        let file_path = env.dir.join("code.rs");
        fs::write(&file_path, "pub fn foo() {}\n")?;
        let filepath_str = file_path.to_str().unwrap();

        let metadata = init_edit_session(filepath_str, false)?;
        assert_eq!(metadata.total_lines, 1);

        // Call update with start_id = None
        let edits = vec![LineEdit {
            op: EditOp::Replace,
            start_id: None,
            content: Some("pub fn bar() {}".to_string()),
            ..Default::default()
        }];

        let result = edit_lines(&repository, filepath_str, edits, &env.pm).await;
        assert!(result.is_err());
        let err_msg = result.err().unwrap().to_string();
        assert!(err_msg.contains("Missing start_id for replace op"));

        Ok(())
    }

    #[tokio::test]
    async fn test_prepend_operation() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = TestEnvironment::new("prepend");
        let repository = SqliteSessionRepository;

        let file_path = env.dir.join("code.rs");
        fs::write(&file_path, "pub fn hello() {}\n")?;
        let filepath_str = file_path.to_str().unwrap();

        let _metadata = init_edit_session(filepath_str, false)?;

        let edits = vec![LineEdit {
            op: EditOp::Prepend,
            start_id: None,
            content: Some("use std::collections::HashMap;\n\n".to_string()),
            ..Default::default()
        }];

        let preview = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
        let res: serde_json::Value = serde_json::from_str(&preview)?;
        assert!(res["status"].is_null(), "success is the exit code: {}", res);
        let modified = res["modified_lines"].as_array().unwrap();
        assert!(!modified.is_empty());
        assert_eq!(
            fs::read_to_string(&file_path)?,
            "use std::collections::HashMap;\n\npub fn hello() {}\n"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_replace_range_operation() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = TestEnvironment::new("replace_range");
        let repository = SqliteSessionRepository;

        let file_path = env.dir.join("code.rs");
        fs::write(
            &file_path,
            "pub fn foo() {\n    let a = 1;\n    let b = 2;\n}\n",
        )?;
        let filepath_str = file_path.to_str().unwrap();

        let _metadata = init_edit_session(filepath_str, false)?;

        // Find IDs for lines 2 and 3
        let lines_view = test_view_lines(&repository, filepath_str, 2, 3, None)?;
        let id_2 = find_line_id(&lines_view, "let a = 1;");
        let id_3 = find_line_id(&lines_view, "let b = 2;");

        let edits = vec![LineEdit {
            op: EditOp::Replace,
            start_id: Some(id_2),
            end_id: Some(id_3),
            content: Some("    let val = 42;".to_string()),
            ..Default::default()
        }];

        let preview = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
        let res: serde_json::Value = serde_json::from_str(&preview)?;
        assert!(res["status"].is_null(), "success is the exit code: {}", res);
        let modified = res["modified_lines"].as_array().unwrap();
        assert!(!modified.is_empty());
        assert_eq!(
            fs::read_to_string(&file_path)?,
            "pub fn foo() {\n    let val = 42;\n}\n"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_move_operation() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = TestEnvironment::new("move_ops");
        let repository = SqliteSessionRepository;

        let file_path = env.dir.join("code.rs");
        fs::write(
            &file_path,
            "pub fn main() {\n    foo();\n}\npub fn foo() {}\n",
        )?;
        let filepath_str = file_path.to_str().unwrap();

        let _metadata = init_edit_session(filepath_str, false)?;

        // Get target ID for lines 4 (pub fn foo() {})
        let lines_view = test_view_lines(&repository, filepath_str, 4, 4, None)?;
        let foo_id = find_line_id(&lines_view, "pub fn foo() {}");

        // Get target ID for line 1 (pub fn main() {)
        let lines_view_main = test_view_lines(&repository, filepath_str, 1, 1, None)?;
        let main_id = find_line_id(&lines_view_main, "pub fn main() {");

        // Move 'foo' function before 'main' function
        let edits = vec![LineEdit {
            op: EditOp::Move,
            start_id: Some(foo_id),
            dest_id: Some(main_id),
            move_position: Some(MovePosition::Before),
            ..Default::default()
        }];

        let preview = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
        let res: serde_json::Value = serde_json::from_str(&preview)?;
        assert!(res["status"].is_null(), "success is the exit code: {}", res);
        let modified = res["modified_lines"].as_array().unwrap();
        assert!(!modified.is_empty());
        assert_eq!(
            fs::read_to_string(&file_path)?,
            "pub fn foo() {}\npub fn main() {\n    foo();\n}\n"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_edit_lines_jit_initialization() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = TestEnvironment::new("test_jit_edit");

        let file_path = env.dir.join("code.txt");
        fs::write(&file_path, "line 1\nline 2\n")?;
        let filepath_str = file_path.to_str().unwrap();

        // Ensure no stale session exists in DB from previous test runs
        let repository = SqliteSessionRepository;
        let _ = repository.delete_session(filepath_str);

        // Call edit_lines directly without calling init_edit_session.
        // We can append a line. Since it's append, start_id is ignored.
        let edits = vec![LineEdit {
            op: EditOp::Append,
            content: Some("line 3".to_string()),
            ..Default::default()
        }];

        let preview = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
        let res: serde_json::Value = serde_json::from_str(&preview)?;
        assert!(res["status"].is_null(), "success is the exit code: {}", res);
        let modified = res["modified_lines"].as_array().unwrap();
        assert!(!modified.is_empty());

        let content = fs::read_to_string(&file_path)?;
        assert_eq!(content, "line 1\nline 2\nline 3\n");

        fs::remove_dir_all(&env.dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn test_long_line_replace_substring() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = TestEnvironment::new("long_line_substring_test");
        let repository = SqliteSessionRepository;

        let file_path = env.dir.join("code.rs");
        let long_line = "a".repeat(2050);
        let file_content = format!("fn main() {{\n    // {}\n}}\n", long_line);
        fs::write(&file_path, &file_content)?;
        let filepath_str = file_path.to_str().unwrap();

        let metadata = init_edit_session(filepath_str, false)?;
        assert_eq!(metadata.total_lines, 3);

        // Get the lines view
        let lines_view = test_view_lines(&repository, filepath_str, 1, 3, None)?;
        let lines_text = lines_view.lines_text.as_ref().unwrap();

        // Verify that the long line is soft-wrapped in the output
        assert!(lines_text.contains("2:     // aaaaa"));
        assert!(lines_text.contains("└: aaaaa"));

        // Get target ID (which should NOT have #TRUNC suffix)
        let start_id = find_line_id(&lines_view, "    // aaaaa");
        assert!(!start_id.is_empty());
        assert!(!start_id.contains("#TRUNC"));

        // 1. Perform replace_substring on the long line
        let edits = vec![LineEdit {
            op: EditOp::ReplaceSubstring,
            start_id: Some(start_id.clone()),
            pattern: Some("aaaaa".to_string()),
            replacement: Some("bbbbb".to_string()),
            occurrence: Some(1),
            ..Default::default()
        }];

        let result = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
        assert!(result.contains("modified_lines"), "{}", result);

        // Verify that the file content was updated on disk
        let disk_content = fs::read_to_string(&file_path)?;
        assert!(disk_content.contains("bbbbbaaaaa")); // Only the first match was replaced

        fs::remove_dir_all(&env.dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn test_hybrid_policy_markdown_warning() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = TestEnvironment::new("hybrid_md_warning");
        let repository = SqliteSessionRepository;

        let file_path = env.dir.join("doc.md");
        let md_content = "# Title\nSome content\n";
        fs::write(&file_path, md_content)?;
        let filepath_str = file_path.to_str().unwrap();

        let metadata = init_edit_session(filepath_str, false)?;
        assert_eq!(metadata.total_lines, 2);

        // Get the lines view
        let lines_view = test_view_lines(&repository, filepath_str, 1, 2, None)?;
        let start_id = find_line_id(&lines_view, "Some content");
        assert!(!start_id.is_empty());

        // Make an edit that introduces a lint warning (unclosed fence, header hierarchy gap, and malformed link)
        let edits = vec![LineEdit {
            op: EditOp::Replace,
            start_id: Some(start_id),
            content: Some("### Heading Gap\n\n[text(url)\n\n```rust\nlet x = 1;\n".to_string()),
            ..Default::default()
        }];

        let result = edit_lines(&repository, filepath_str, edits, &env.pm).await?;

        // The edit should succeed (status success) but return warnings!
        assert!(
            !result.contains("\"status\""),
            "success is the exit code: {}",
            result
        );
        assert!(result.contains("\"warnings\":"));
        assert!(result.contains("Header hierarchy mismatch"));
        assert!(result.contains("Possible malformed link syntax"));
        assert!(result.contains("Unclosed fenced code block"));

        // Verify that the file was indeed updated on disk
        let disk_content = fs::read_to_string(&file_path)?;
        assert!(disk_content.contains("### Heading Gap"));
        assert!(disk_content.contains("[text(url)"));

        fs::remove_dir_all(&env.dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn test_hybrid_policy_html_warning() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = TestEnvironment::new("hybrid_html_warning");
        let repository = SqliteSessionRepository;

        let file_path = env.dir.join("index.html");
        let html_content = "<div>\n  <p>Hello</p>\n</div>\n";
        fs::write(&file_path, html_content)?;
        let filepath_str = file_path.to_str().unwrap();

        let metadata = init_edit_session(filepath_str, false)?;
        assert_eq!(metadata.total_lines, 3);

        // Get the lines view
        let lines_view = test_view_lines(&repository, filepath_str, 1, 3, None)?;
        let start_id = find_line_id(&lines_view, "<p>Hello</p>");
        assert!(!start_id.is_empty());

        // Make an edit that has invalid HTML syntax
        let edits = vec![LineEdit {
            op: EditOp::Replace,
            start_id: Some(start_id),
            content: Some("<p>Hello <span class=</p>".to_string()),
            ..Default::default()
        }];

        let result = edit_lines(&repository, filepath_str, edits, &env.pm).await?;

        // The edit should succeed (status success) but return HTML warnings!
        assert!(
            !result.contains("\"status\""),
            "success is the exit code: {}",
            result
        );
        assert!(result.contains("\"warnings\":"));
        assert!(result.contains("HTML syntax warning"));

        fs::remove_dir_all(&env.dir)?;
        Ok(())
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
      "session_id": "8f3a8b23",
      "total_lines": 420,
      "file_hash": "a1b2c3d4",
      "mtime": "2026-07-08T22:07:07Z",
      "is_supported": true
    }
    ```
"#;
        assert!(validate_markdown(nested_md).is_ok());
    }

    #[tokio::test]
    async fn test_edit_lines_markdown_success() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = TestEnvironment::new("markdown_success");
        let repository = SqliteSessionRepository;

        let file_path = env.dir.join("test.md");
        let file_content = "# Title\n\n```rust\nfn main() {}\n```\n";
        fs::write(&file_path, file_content)?;

        let filepath_str = file_path.to_str().unwrap();
        let _init_res = init_edit_session(filepath_str, false)?;

        let lines_view = test_view_lines(&repository, filepath_str, 1, 100, None)?;
        let start_line_id = find_line_id(&lines_view, "fn main()");

        let edits = vec![LineEdit {
            op: EditOp::Replace,
            start_id: Some(start_line_id),
            content: Some("fn main() { println!(\"x\"); }".to_string()),
            ..Default::default()
        }];

        let result = edit_lines(&repository, filepath_str, edits, &env.pm).await;
        assert!(result.is_ok());

        let disk_content = fs::read_to_string(&file_path)?;
        assert_eq!(
            disk_content,
            "# Title\n\n```rust\nfn main() { println!(\"x\"); }\n```\n"
        );

        fs::remove_dir_all(&env.dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn test_edit_lines_markdown_soft_warning() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = TestEnvironment::new("markdown_failure");
        let repository = SqliteSessionRepository;

        let file_path = env.dir.join("test.md");
        let file_content = "# Title\n\n```rust\nfn main() {}\n````\n";
        fs::write(&file_path, file_content)?;

        let filepath_str = file_path.to_str().unwrap();
        let _init_res = init_edit_session(filepath_str, false)?;

        let lines_view = test_view_lines(&repository, filepath_str, 1, 100, None)?;
        let target_line_id = find_line_id(&lines_view, "````");

        let edits = vec![LineEdit {
            op: EditOp::Delete,
            start_id: Some(target_line_id),
            ..Default::default()
        }];

        let result = edit_lines(&repository, filepath_str, edits, &env.pm).await?;
        assert!(
            !result.contains("\"status\""),
            "success is the exit code: {}",
            result
        );
        assert!(result.contains("Unclosed fenced code block"));

        let disk_content = fs::read_to_string(&file_path)?;
        assert_eq!(disk_content, "# Title\n\n```rust\nfn main() {}\n");

        fs::remove_dir_all(&env.dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn test_edit_lines_success_response_formatting() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = TestEnvironment::new("response_formatting");
        let repository = SqliteSessionRepository;

        // A file a grammar covers: an edit that parses says nothing but its
        // ids, which is what this checks. The other case is its own test.
        let file_path = env.dir.join("test.rs");
        let file_content = "fn a() {}\nfn b() {}\nfn c() {}";
        fs::write(&file_path, file_content)?;

        let filepath_str = file_path.to_str().unwrap();
        let _init_res = init_edit_session(filepath_str, false)?;

        let lines_view = test_view_lines(&repository, filepath_str, 1, 3, None)?;
        let line_1_id = find_line_id(&lines_view, "fn a() {}");

        let edits = vec![LineEdit {
            op: EditOp::Replace,
            start_id: Some(line_1_id.clone()),
            content: Some("fn renamed() {}".to_string()),
            ..Default::default()
        }];

        let result = edit_lines(&repository, filepath_str, edits, &env.pm).await?;

        let val: serde_json::Value = serde_json::from_str(&result)?;
        assert!(val["status"].is_null(), "success is the exit code: {}", val);
        let modified_lines = val["modified_lines"].as_array().unwrap();
        assert_eq!(modified_lines.len(), 1);
        let new_id = modified_lines[0][0].as_str().unwrap();
        assert_eq!(
            modified_lines[0][1].as_u64().unwrap(),
            1,
            "the id says which line it is: {}",
            val
        );
        let (old_seq, _) = crate::tools::session_db::parse_line_id(&line_1_id)?;
        let (new_seq, _) = crate::tools::session_db::parse_line_id(new_id)?;
        assert_eq!(old_seq, new_seq);

        let expected_json = format!(
            "{{\n  \"modified_lines\": [\n    [\"{}\",1]\n  ]\n}}",
            new_id
        );
        assert_eq!(result, expected_json);

        fs::remove_dir_all(&env.dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn test_bash_syntax_validation_error_rolls_back() -> Result<()> {
        let _lock = DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = TestEnvironment::new("bash_syntax_error");
        let repository = SqliteSessionRepository;

        let file_path = env.dir.join("script.sh");
        let initial_content = "if [ \"$x\" = \"1\" ]; then\n    echo \"one\"\nfi\n";
        fs::write(&file_path, initial_content)?;
        let filepath_str = file_path.to_str().unwrap();

        let _metadata = init_edit_session(filepath_str, false)?;

        // Find the line 3 ID (which is "fi")
        let lines_view = test_view_lines(&repository, filepath_str, 1, 3, None)?;
        let start_id = find_line_id(&lines_view, "fi");
        assert!(!start_id.is_empty());

        // Apply edit that introduces syntax error (e.g. replacing "fi" with "else")
        let edits = vec![LineEdit {
            op: EditOp::Replace,
            start_id: Some(start_id),
            content: Some("else".to_string()), // Syntax error since mismatched if/else without fi
            ..Default::default()
        }];

        // 1. Test Permissive Mode (strict_validation: false by default when calling edit_lines)
        let res = edit_lines(&repository, filepath_str, edits.clone(), &env.pm).await?;
        let val: serde_json::Value = serde_json::from_str(&res)?;
        assert_eq!(val["status"], "saved_with_errors");
        assert_eq!(val["syntax_valid"], false);
        assert!(val["diagnostics"].is_array());

        // Verify disk content was saved (i.e. changed)
        assert_eq!(
            fs::read_to_string(&file_path)?,
            "if [ \"$x\" = \"1\" ]; then\n    echo \"one\"\nelse\n"
        );

        // Restore initial content for strict validation test
        fs::write(&file_path, initial_content)?;
        let _metadata_strict = init_edit_session(filepath_str, false)?;
        let lines_view_strict = test_view_lines(&repository, filepath_str, 1, 3, None)?;
        let start_id_strict = find_line_id(&lines_view_strict, "fi");
        let edits_strict = vec![LineEdit {
            op: EditOp::Replace,
            start_id: Some(start_id_strict),
            content: Some("else".to_string()),
            ..Default::default()
        }];

        // 2. Test Strict Mode (strict_validation: true)
        let res_strict =
            edit_lines_with_validation(&repository, filepath_str, edits_strict, true, &env.pm)
                .await;
        assert!(res_strict.is_err());
        let err_msg = res_strict.unwrap_err().to_string();
        assert!(
            err_msg.contains("\"syntax_valid\": false"),
            "Expected the refusal to carry its verdict, got: {}",
            err_msg
        );
        assert!(
            err_msg.contains("preview_id"),
            "Expected the refusal to name the batch it kept, got: {}",
            err_msg
        );

        // Verify disk content was rolled back (i.e. remains unchanged)
        assert_eq!(fs::read_to_string(&file_path)?, initial_content);

        // Verify DB content was rolled back (re-query content of line 3)
        let lines_strict = repository.fetch_lines_range(&_metadata_strict.session_id, 3, 3)?;
        assert_eq!(lines_strict.len(), 1);
        assert_eq!(lines_strict[0].2.trim(), "fi");

        fs::remove_dir_all(&env.dir)?;
        Ok(())
    }
}
