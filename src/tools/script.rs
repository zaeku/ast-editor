//! The `edit` script format: a line-oriented way to write an edit batch, so a
//! shell heredoc can carry code with no escaping.
//!
//! The grammar is D-01M27ZZNWK431A in decisions/. This module only turns a script
//! into the same `LineEdit` values the JSON form produces; everything after
//! that is shared.

use anyhow::{bail, Result};

use crate::tools::session_db::{EditOp, LineEdit, MovePosition};

/// Parse a script into the batch it describes.
pub(crate) fn parse(script: &str) -> Result<Vec<LineEdit>> {
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
    // Line-terminated, so that a payload of no lines and a payload of one empty
    // line do not arrive as the same string (D-01M280Y0JPPWBG).
    let content = payload
        .as_ref()
        .map(|body| body.iter().map(|line| format!("{line}\n")).collect());

    // Name an operation nobody has before complaining about its payload: an
    // unknown op with a block used to be told it takes no content, which reads
    // as if the op were real and the block were the mistake.
    if !is_known(op_name) && op_name != "replace_substring" {
        bail!(
            "line {}: unknown operation '{}'. Known: replace, insert_after, \
             insert_before, append, prepend, delete, move.",
            line,
            op_name
        );
    }

    let takes_payload = matches!(
        op_name,
        "replace" | "insert_after" | "insert_before" | "append" | "prepend"
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
        ("replace", 1) => {
            let (start, end) = span(args[0], line, "replace")?;
            LineEdit {
                op: EditOp::Replace,
                start_id: Some(start),
                end_id: end,
                content,
                ..Default::default()
            }
        }
        ("insert_after", 1) => LineEdit {
            op: EditOp::InsertAfter,
            start_id: Some(single(args[0], line, "insert_after")?),
            content,
            ..Default::default()
        },
        ("insert_before", 1) => LineEdit {
            op: EditOp::InsertBefore,
            start_id: Some(single(args[0], line, "insert_before")?),
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
        ("delete", 1) => {
            let (start, end) = span(args[0], line, "delete")?;
            LineEdit {
                op: EditOp::Delete,
                start_id: Some(start),
                end_id: end,
                ..Default::default()
            }
        }
        ("move", _) => parse_move(args, line)?,

        ("replace_substring", _) => bail!(
            "line {}: 'replace_substring' takes a pattern and a replacement, which this format \
             cannot carry. Use the JSON form for it.",
            line
        ),
        // Anything unknown was named as unknown before the payload was read, and
        // replace_substring has the arm above, so what is left here is a known
        // operation given the wrong number of arguments.
        (op, _) => bail!(
            "line {}: '{}' was given {} argument(s). {}",
            line,
            op,
            args.len(),
            usage_of(op)
        ),
    };

    Ok(edit)
}

/// An address argument: one line id, or two separated by a comma for a span.
/// `view` reads a line-number range the same way, so this is the same comma.
fn span(arg: &str, line: usize, op: &str) -> Result<(String, Option<String>)> {
    match arg.split_once(',') {
        None => Ok((arg.to_string(), None)),
        Some((start, end)) if !start.is_empty() && !end.is_empty() && !end.contains(',') => {
            Ok((start.to_string(), Some(end.to_string())))
        }
        Some(_) => bail!(
            "line {line}: '{op}' was given '{arg}' as an address. A span is \
             <start_id>,<end_id>, with one comma and an id on each side."
        ),
    }
}

/// An address for an op that acts at a point rather than over a span.
fn single(arg: &str, line: usize, op: &str) -> Result<String> {
    let (start, end) = span(arg, line, op)?;
    if end.is_some() {
        bail!(
            "line {line}: '{op}' acts at one line, so it takes one id rather than a \
             span. {}",
            usage_of(op)
        );
    }
    Ok(start)
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

    if position_at != 1 {
        bail!(
            "line {}: 'move' takes one address before the position. {}",
            line,
            usage_of("move")
        );
    }
    let (start, end) = span(args[0], line, "move")?;

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
        start_id: Some(start),
        end_id: end,
        dest_id: dest,
        move_position: Some(position),
        ..Default::default()
    })
}

fn is_known(op: &str) -> bool {
    matches!(
        op,
        "replace" | "insert_after" | "insert_before" | "append" | "prepend" | "delete" | "move"
    )
}

fn usage_of(op: &str) -> &'static str {
    match op {
        "replace" => "Usage: replace <id> ``` or replace <start_id>,<end_id> ```",
        "insert_after" => "Usage: insert_after <id> ```",
        "insert_before" => "Usage: insert_before <id> ```",
        "append" => "Usage: append ```",
        "prepend" => "Usage: prepend ```",
        "delete" => "Usage: delete <id> or delete <start_id>,<end_id>",
        "move" => "Usage: move <start_id>[,<end_id>] before|after <dest_id>, or move <start_id>[,<end_id>] prepend|append",
        _ => "",
    }
}
