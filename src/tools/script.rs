//! The `edit` script format: a line-oriented way to write an edit batch, so a
//! shell heredoc can carry code with no escaping.
//!
//! The grammar is D-01M27ZZNWK431A in decisions/. This module only turns a script
//! into the same `LineEdit` values the JSON form produces; everything after
//! that is shared.

use anyhow::{bail, Result};

use crate::tools::session_db::{EditOp, LineEdit, MovePosition};

/// Parse a script into the batch it describes.
pub fn parse(script: &str) -> Result<Vec<LineEdit>> {
    let lines: Vec<&str> = script.lines().collect();
    let mut edits = Vec::new();
    let mut idx = 0;

    while idx < lines.len() {
        let line = lines[idx];
        let number = idx + 1;
        idx += 1;

        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let (directive, fence) = split_fence(line);
        let words: Vec<&str> = directive.split_whitespace().collect();

        let payload = match fence {
            None => None,
            Some(len) => {
                let (body, next) = read_payload(&lines, idx, len, &words, number)?;
                idx = next;
                Some(body)
            }
        };

        edits.push(directive_to_edit(&words, payload, number)?);
    }

    Ok(edits)
}

/// Split a directive line into its words and the length of the fence that ends
/// it, if any. A fence is three or more backticks at the end of the line.
fn split_fence(line: &str) -> (&str, Option<usize>) {
    let trimmed = line.trim_end();
    let backticks = trimmed.len() - trimmed.trim_end_matches('`').len();
    if backticks >= 3 {
        (&trimmed[..trimmed.len() - backticks], Some(backticks))
    } else {
        (trimmed, None)
    }
}

/// Collect a payload up to its closing fence, returning it and the index of the
/// line after it. A fence that is never closed is an error: the rest of the
/// input is not what the author meant by it.
fn read_payload(
    lines: &[&str],
    start: usize,
    fence_len: usize,
    words: &[&str],
    directive_line: usize,
) -> Result<(Vec<String>, usize)> {
    let mut body = Vec::new();
    let mut idx = start;

    while idx < lines.len() {
        let line = lines[idx];
        let trimmed = line.trim();
        if trimmed.len() >= fence_len && trimmed.chars().all(|c| c == '`') {
            return Ok((body, idx + 1));
        }
        body.push(line.to_string());
        idx += 1;
    }

    bail!(
        "line {}: the '{}' block opened with {} backticks is never closed. \
         Close it with a line of at least {} backticks, or use a longer fence \
         if the content contains one.",
        directive_line,
        words.first().unwrap_or(&"?"),
        fence_len,
        fence_len
    )
}

fn directive_to_edit(
    words: &[&str],
    payload: Option<Vec<String>>,
    line: usize,
) -> Result<LineEdit> {
    let op_name = *words.first().unwrap_or(&"");
    let args = &words[1.min(words.len())..];
    let content = payload.as_ref().map(|body| body.join("\n"));

    let takes_payload = matches!(
        op_name,
        "replace" | "replace_range" | "insert_after" | "insert_before" | "append" | "prepend"
    );
    if takes_payload && payload.is_none() {
        bail!(
            "line {}: '{}' needs content. End the line with ``` and put the new lines under it.",
            line,
            op_name
        );
    }
    if !takes_payload && payload.is_some() {
        bail!(
            "line {}: '{}' takes no content, so it must not open a block.",
            line,
            op_name
        );
    }

    let edit = match (op_name, args.len()) {
        ("replace", 1) => LineEdit {
            op: EditOp::Replace,
            target_id: Some(args[0].to_string()),
            content,
            ..Default::default()
        },
        ("replace_range", 2) => LineEdit {
            op: EditOp::ReplaceRange,
            target_id: Some(args[0].to_string()),
            end_target_id: Some(args[1].to_string()),
            content,
            ..Default::default()
        },
        ("insert_after", 1) => LineEdit {
            op: EditOp::InsertAfter,
            target_id: Some(args[0].to_string()),
            content,
            ..Default::default()
        },
        ("insert_before", 1) => LineEdit {
            op: EditOp::InsertBefore,
            target_id: Some(args[0].to_string()),
            content,
            ..Default::default()
        },
        ("append", 0) => LineEdit {
            op: EditOp::Append,
            content,
            ..Default::default()
        },
        ("prepend", 0) => LineEdit {
            op: EditOp::Prepend,
            content,
            ..Default::default()
        },
        ("delete", 1) => LineEdit {
            op: EditOp::Delete,
            target_id: Some(args[0].to_string()),
            ..Default::default()
        },
        ("move", _) => parse_move(args, line)?,

        ("replace_substring", _) => bail!(
            "line {}: 'replace_substring' takes a pattern and a replacement, which this format \
             cannot carry. Use the JSON form for it.",
            line
        ),
        (op, _) if is_known(op) => bail!(
            "line {}: '{}' was given {} argument(s). {}",
            line,
            op,
            args.len(),
            usage_of(op)
        ),
        (op, _) => bail!(
            "line {}: unknown operation '{}'. Known: replace, replace_range, insert_after, \
             insert_before, append, prepend, delete, move.",
            line,
            op
        ),
    };

    Ok(edit)
}

/// `move <start> [<end>] <before|after> <dest>` or
/// `move <start> [<end>] <prepend|append>`.
fn parse_move(args: &[&str], line: usize) -> Result<LineEdit> {
    let position_at = args
        .iter()
        .position(|word| matches!(*word, "before" | "after" | "prepend" | "append"))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "line {}: 'move' needs a position. {}",
                line,
                usage_of("move")
            )
        })?;

    if position_at == 0 || position_at > 2 {
        bail!(
            "line {}: 'move' takes one or two line ids before the position. {}",
            line,
            usage_of("move")
        );
    }

    let (position, needs_dest) = match args[position_at] {
        "before" => (MovePosition::Before, true),
        "after" => (MovePosition::After, true),
        "prepend" => (MovePosition::Prepend, false),
        _ => (MovePosition::Append, false),
    };

    let rest = &args[position_at + 1..];
    let dest = match (needs_dest, rest.len()) {
        (true, 1) => Some(rest[0].to_string()),
        (true, _) => bail!(
            "line {}: '{}' needs one destination line id. {}",
            line,
            args[position_at],
            usage_of("move")
        ),
        (false, 0) => None,
        (false, _) => bail!(
            "line {}: '{}' takes no destination. {}",
            line,
            args[position_at],
            usage_of("move")
        ),
    };

    Ok(LineEdit {
        op: EditOp::Move,
        target_id: Some(args[0].to_string()),
        end_target_id: if position_at == 2 {
            Some(args[1].to_string())
        } else {
            None
        },
        dest_target_id: dest,
        move_position: Some(position),
        ..Default::default()
    })
}

fn is_known(op: &str) -> bool {
    matches!(
        op,
        "replace"
            | "replace_range"
            | "insert_after"
            | "insert_before"
            | "append"
            | "prepend"
            | "delete"
            | "move"
    )
}

fn usage_of(op: &str) -> &'static str {
    match op {
        "replace" => "Usage: replace <id> ```",
        "replace_range" => "Usage: replace_range <start_id> <end_id> ```",
        "insert_after" => "Usage: insert_after <id> ```",
        "insert_before" => "Usage: insert_before <id> ```",
        "append" => "Usage: append ```",
        "prepend" => "Usage: prepend ```",
        "delete" => "Usage: delete <id>",
        "move" => "Usage: move <start_id> [<end_id>] before|after <dest_id>, or move <start_id> [<end_id>] prepend|append",
        _ => "",
    }
}
