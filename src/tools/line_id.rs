//! A line's identity, and the shapes an edit is asked for in. Nothing here
//! opens the store: an id is computed from the line's own content, and an edit
//! describes itself before anything has been read.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone)]
pub(crate) struct FileEntry {
    pub file_key: String,
    pub total_lines: usize,
    pub file_hash: String,
    pub mtime: i64,
    pub is_supported: bool,
    pub warning_message: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EditOp {
    #[default]
    InsertAfter,
    InsertBefore,
    Append,
    Prepend,
    Replace,
    Delete,
    ReplaceSubstring,
    Move,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub(crate) enum MovePosition {
    Prepend,
    Append,
    Before,
    #[default]
    After,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub(crate) struct LineEdit {
    pub op: EditOp,
    pub start_id: Option<String>,
    pub end_id: Option<String>,
    pub dest_id: Option<String>,
    pub move_position: Option<MovePosition>,
    pub content: Option<String>,
    pub pattern: Option<String>,
    pub replacement: Option<String>,
    pub occurrence: Option<usize>,
}

/// An op's name as a caller spells it, which is what `serde` renamed it to.
pub(crate) fn op_name(op: EditOp) -> String {
    serde_json::to_value(op)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{op:?}"))
}

/// What to say when an edit does not carry a field its op needs. The op and the
/// field are named as a caller spells them, and so is what the edit did carry,
/// because the mistake is usually a field in the wrong place rather than a
/// field nobody thought of.
pub(crate) fn missing_field(edit: &LineEdit, needs: &str) -> anyhow::Error {
    let mut carried: Vec<&str> = Vec::new();
    let filled = |value: &Option<String>| value.as_deref().is_some_and(|s| !s.is_empty());
    if filled(&edit.start_id) {
        carried.push("start_id");
    }
    if filled(&edit.end_id) {
        carried.push("end_id");
    }
    if filled(&edit.dest_id) {
        carried.push("dest_id");
    }
    if edit.content.is_some() {
        carried.push("content");
    }
    if filled(&edit.pattern) {
        carried.push("pattern");
    }
    if filled(&edit.replacement) {
        carried.push("replacement");
    }
    let carried = if carried.is_empty() {
        "no other field".to_string()
    } else {
        carried.join(", ")
    };
    let op = op_name(edit.op);
    anyhow::anyhow!(
        "{}",
        crate::tools::metadata::get_config()
            .error_edit_missing_field
            .replacen("{}", &op, 1)
            .replacen("{}", needs, 1)
            .replacen("{}", &carried, 1)
    )
}

pub(crate) fn parse_line_id(id_str: &str) -> Result<(i64, String)> {
    let parts: Vec<&str> = id_str.split('#').collect();
    if parts.len() == 1 {
        anyhow::bail!(crate::tools::metadata::get_config()
            .error_address_needs_a_number
            .replacen("{}", id_str, 1));
    }
    if parts.len() != 2 {
        anyhow::bail!("Invalid Line ID format: {}", id_str);
    }
    let seq = i64::from_str_radix(parts[0], 16).context("Failed to parse sequence ID hex")?;
    Ok((seq, parts[1].to_string()))
}

/// The hash kept in the index. Reconciliation aligns disk lines against stored
/// rows before any sequence number is known, so this is the only key available
/// there and needs enough width that a file's lines do not collide.
pub(crate) fn compute_stored_hash(content: &str) -> String {
    use sha1::{Digest, Sha1};
    let mut hasher = Sha1::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())[..16].to_string()
}

/// A hash of the line with every space removed, so respacing it does not
/// change the result. Collapsing runs of whitespace instead would not be
/// enough: formatters add and remove spaces around operators and delimiters,
/// not just at the margin.
pub(crate) fn compute_normalized_hash(content: &str) -> String {
    compute_stored_hash(
        &content
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>(),
    )
}

/// The hash surfaced to agents inside `{sequence_id:x}#{hash}`. It is a prefix
/// of the stored hash, so the short form can be checked against a stored row
/// without keeping a second column. The sequence number does the identifying
/// here, which is why four characters are enough.
pub(crate) fn compute_line_hash(content: &str) -> String {
    compute_stored_hash(content)[..4].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `script.rs` builds a `LineEdit` for the insert directives and names the
    /// op beside `..Default::default()`, which supplies the same one. Deleting
    /// either half leaves the value alone, so no test can tell them apart and
    /// `mutants.toml` says so — on the condition asserted here.
    #[test]
    fn the_default_op_is_the_one_an_insert_names() {
        assert_eq!(EditOp::default(), EditOp::InsertAfter);
    }
}
