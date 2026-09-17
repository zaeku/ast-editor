use crate::tools::line_id::LineEdit;
use crate::tools::repository::FileStore;
use crate::tools::syntax::{validate_syntax, SyntaxValidationResult};
use anyhow::Result;
use std::fs;

/// Reconcile the entry with disk when the file changed under us, so that an
/// edit or a preview operates on the current state.
fn resync_if_stale(repository: &impl FileStore, filepath: &str, edits: &[LineEdit]) -> Result<()> {
    // Reconcile before the read path can, so the lines this batch targets are
    // checked for having survived rather than silently re-identified.
    let Some(file_key) = repository.get_file_key(filepath)? else {
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

    repository.smart_resync(filepath, &file_key, &start_ids)
}

/// Preview an edit batch without touching disk or the store.
///
/// The batch is applied to the entry, the resulting content is validated the
/// same way a real commit is, and the entry is then restored to its previous
/// state. No line IDs are minted: the caller obtains those from a real
/// `edit` call.
/// What a dry run has to say: the diff to read, and the report to act on.
/// They are separate because a diff inside a JSON string is a diff nobody can
/// read.
pub(crate) struct DryRun {
    pub diff: String,
    pub report: String,
}

pub(crate) async fn edit_lines_dry_run(
    repository: &impl FileStore,
    filepath: &str,
    edits: Vec<LineEdit>,
    parser_manager: &crate::parser::ParserManager,
) -> Result<DryRun> {
    resync_if_stale(repository, filepath, &edits)?;

    let meta = repository.init_session(filepath, false)?;
    let file_key = meta.file_key;

    let line_ending = if repository.get_file_crlf(&file_key)? {
        "\r\n"
    } else {
        "\n"
    };
    let original_content = fs::read_to_string(filepath)?;

    // A preview is a plan that is never committed, so there is nothing to undo.
    let (buffer, _, _) = repository.plan_line_edits(&file_key, &edits)?;
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

/// What an edit does with a result the parser rejects.
#[derive(Clone, Copy, PartialEq)]
enum OnRejectedParse {
    /// Roll back and hand the batch back under an id. What a caller who has
    /// not seen a verdict gets, so that a broken file is never the answer to
    /// an edit they did not know was broken.
    Refuse,
    /// Write it. What applying a preview id means: the verdict came back with
    /// that id and the caller sent it anyway (D-01M27KKNNRRZ96).
    Write,
}

/// Apply the edit batch a previous dry run validated, addressed by its preview
/// id instead of resent in full. It does not refuse what it is given: the
/// batch was checked when the preview was taken and the caller read the answer
/// before naming it here.
pub(crate) async fn apply_preview(
    repository: &impl FileStore,
    filepath: &str,
    preview_id: &str,
    parser_manager: &crate::parser::ParserManager,
) -> Result<String> {
    let edits = repository.take_preview(filepath, preview_id)?;
    apply_batch(
        repository,
        filepath,
        edits,
        OnRejectedParse::Write,
        parser_manager,
    )
    .await
}

/// Apply a batch and answer with the lines it changed. A result that does not
/// parse is rolled back and refused, and the batch is kept under an id so a
/// caller who judges the parser wrong applies it (D-01M28NM3ECNY08).
pub(crate) async fn edit_lines(
    repository: &impl FileStore,
    filepath: &str,
    edits: Vec<LineEdit>,
    parser_manager: &crate::parser::ParserManager,
) -> Result<String> {
    apply_batch(
        repository,
        filepath,
        edits,
        OnRejectedParse::Refuse,
        parser_manager,
    )
    .await
}

async fn apply_batch(
    repository: &impl FileStore,
    filepath: &str,
    edits: Vec<LineEdit>,
    on_rejected: OnRejectedParse,
    parser_manager: &crate::parser::ParserManager,
) -> Result<String> {
    resync_if_stale(repository, filepath, &edits)?;

    let meta = repository.init_session(filepath, false)?;
    let file_key = meta.file_key;

    let line_ending = if repository.get_file_crlf(&file_key)? {
        "\r\n"
    } else {
        "\n"
    };

    // Plan the batch without persisting it. Nothing is committed until the
    // content it produces has been accepted, so a rejected edit needs no undo.
    let (buffer, newly_modified_lines, renumbered) =
        repository.plan_line_edits(&file_key, &edits)?;
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
            // A refusal in the shape of a dry run: the same verdict, the same
            // diagnostics, and the batch kept under an id, so a caller who
            // judges the parser wrong applies it rather than sending it again
            // (D-01M28NM3ECNY08). The exit code still says nothing happened
            // (D-01M28HCSAMTEFS).
            if on_rejected == OnRejectedParse::Refuse {
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
                // Applying a preview id: the verdict came back with that id
                // and the caller sent it anyway, so the work happens and the
                // answer says what it knows (D-01M27KKNNRRZ96).
                fs::write(filepath, &final_content)?;
                repository.commit_buffer(&file_key, &buffer)?;
                repository.smart_resync(filepath, &file_key, &[])?;

                let config = crate::tools::metadata::get_config();
                let only_ids_wrap_trigger_length = config.only_ids_wrap_trigger_length;
                let formatted_ids = crate::tools::formatter::format_lines(
                    &newly_modified_lines,
                    only_ids_wrap_trigger_length,
                );
                let indented_ids = formatted_ids.replace('\n', "\n  ");

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
            if on_rejected == OnRejectedParse::Refuse {
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
                // Applying a preview id: the check could not run when the
                // preview was taken either, and the caller sent the id anyway.
                fs::write(filepath, &final_content)?;
                repository.commit_buffer(&file_key, &buffer)?;
                repository.smart_resync(filepath, &file_key, &[])?;

                let config = crate::tools::metadata::get_config();
                let only_ids_wrap_trigger_length = config.only_ids_wrap_trigger_length;
                let formatted_ids = crate::tools::formatter::format_lines(
                    &newly_modified_lines,
                    only_ids_wrap_trigger_length,
                );
                let indented_ids = formatted_ids.replace('\n', "\n  ");

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
    repository.commit_buffer(&file_key, &buffer)?;

    // Resync the entry to update parent contexts, line hashes, and mtime/file_hash metadata
    repository.smart_resync(filepath, &file_key, &[])?;

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
    if !renumbered.is_empty() {
        // Every line after an edit that changed the file's length is at a new
        // number. The ends of each run say so, and what is between them is
        // contiguous, so nothing in the middle has to be listed
        // (D-01M2NTVZGAP3VT).
        let ranges = renumbered
            .iter()
            .map(|range| {
                Ok(format!(
                    "    {{\"range\": [{}, {}], \"line_numbers\": [{}, {}]}}",
                    serde_json::to_string(&range.range.0)?,
                    serde_json::to_string(&range.range.1)?,
                    range.line_numbers.0,
                    range.line_numbers.1,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        // One run to a row, the way the ids are printed, so a long answer is
        // read down a column rather than across a line.
        fields.push(format!("\"renumbered\": [\n{}\n  ]", ranges.join(",\n")));
    }
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
