//! The lines of a file being edited, in memory, and the edits applied to them.
//!
//! A buffer is loaded from the store, edited, and written back in one
//! transaction. It is the only thing that mints a sequence number, so the
//! number that names a line is assigned in one place.

use anyhow::{bail, Context, Result};
use std::fs;

use rusqlite::Connection;

use super::session_db::{
    compute_line_hash, missing_field, parse_line_id, reconcile_index, EditOp, LineEdit,
    MovePosition,
};

/// One line of a file being edited, paired with the sequence number that
/// identifies it. Position in `LineBuffer::lines` is the line's order.
#[derive(Clone)]
pub struct BufLine {
    pub seq: i64,
    pub content: String,
    pub parent_context: Option<String>,
}

/// A file's lines held in memory for the duration of one edit batch. Edits
/// mutate the buffer; nothing reaches the database or the disk until the
/// caller decides the result is good.
pub struct LineBuffer {
    pub lines: Vec<BufLine>,
    next_seq: i64,
}

/// The lines an op's `content` field carries. Content is line-terminated text
/// (D-01M280Y0JPPWBG): empty content is no lines at all, "\n" is one empty
/// line, and a trailing newline terminates the last line rather than starting
/// another.
fn split_insert_content(content: &str) -> Vec<String> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut parts: Vec<&str> = content.split('\n').collect();
    if parts.last() == Some(&"") {
        parts.pop();
    }
    parts
        .iter()
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_string())
        .collect()
}

impl LineBuffer {
    /// `next_seq` is the file's monotonic counter. Deriving it from the lines
    /// that happen to survive would hand a deleted line's number to the next
    /// one inserted.
    pub(crate) fn new(lines: Vec<BufLine>, next_seq: i64) -> Self {
        Self { lines, next_seq }
    }

    fn take_seq(&mut self) -> i64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        seq
    }

    pub(crate) fn join(&self, line_ending: &str) -> String {
        self.lines
            .iter()
            .map(|l| l.content.as_str())
            .collect::<Vec<_>>()
            .join(line_ending)
            + line_ending
    }

    /// Resolve a target id to a position, verifying the caller's view of the
    /// line still matches.
    fn position_of(&self, start_id: &str, role: &str, field: &str) -> Result<usize> {
        let (seq, hash) = parse_line_id(start_id)?;
        let idx = self
            .lines
            .iter()
            .position(|l| l.seq == seq)
            .with_context(|| {
                crate::tools::metadata::get_config()
                    .error_target_gone
                    .replacen("{}", &format!("{field} {start_id}"), 1)
            })?;
        let actual = compute_line_hash(&self.lines[idx].content);
        if actual != hash {
            // Which rejection this is decides what the caller should do, so
            // each says it (card #6). A batch carries several ids, so which
            // field this one came in under is part of locating it.
            let _ = role;
            bail!(
                "{}",
                crate::tools::metadata::get_config()
                    .error_target_changed
                    .replacen("{}", &format!("{field} {start_id}"), 1)
            );
        }
        Ok(idx)
    }

    /// Each of `ids` with the 1-indexed line it now sits on, in file order.
    /// An id the buffer no longer holds is dropped: a delete mints nothing and
    /// leaves nothing to point at.
    pub(crate) fn locate(&self, ids: &[String]) -> Vec<(String, usize)> {
        let wanted: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
        self.lines
            .iter()
            .enumerate()
            .filter_map(|(idx, _)| {
                let id = self.id_at(idx);
                wanted.contains(id.as_str()).then_some((id, idx + 1))
            })
            .collect()
    }

    fn id_at(&self, idx: usize) -> String {
        format!(
            "{:x}#{}",
            self.lines[idx].seq,
            compute_line_hash(&self.lines[idx].content)
        )
    }

    fn insert_at(&mut self, idx: usize, contents: Vec<String>, modified: &mut Vec<String>) {
        for (offset, content) in contents.into_iter().enumerate() {
            let seq = self.take_seq();
            self.lines.insert(
                idx + offset,
                BufLine {
                    seq,
                    content,
                    parent_context: None,
                },
            );
            modified.push(self.id_at(idx + offset));
        }
    }

    /// Apply one batch of edits in order, returning the ids the batch touched.
    pub(crate) fn apply(&mut self, edits: &[LineEdit]) -> Result<Vec<String>> {
        let mut modified = Vec::new();

        for edit in edits {
            match edit.op {
                EditOp::InsertAfter | EditOp::InsertBefore | EditOp::Append | EditOp::Prepend => {
                    let contents = split_insert_content(edit.content.as_deref().unwrap_or(""));
                    let no_target = edit.start_id.as_ref().is_none_or(|s| s.is_empty());

                    let at = if edit.op == EditOp::Append
                        || (edit.op == EditOp::InsertAfter && no_target)
                    {
                        self.lines.len()
                    } else if edit.op == EditOp::Prepend
                        || (edit.op == EditOp::InsertBefore && no_target)
                    {
                        0
                    } else {
                        let start_id = edit
                            .start_id
                            .as_ref()
                            .filter(|s| !s.is_empty())
                            .ok_or_else(|| missing_field(edit, "start_id"))?;
                        let idx = self.position_of(start_id, "Start", "start_id")?;
                        if edit.op == EditOp::InsertAfter {
                            idx + 1
                        } else {
                            idx
                        }
                    };

                    self.insert_at(at, contents, &mut modified);
                }

                EditOp::Replace => {
                    let start_id = edit
                        .start_id
                        .as_ref()
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| missing_field(edit, "start_id"))?;
                    let content = edit
                        .content
                        .as_ref()
                        .ok_or_else(|| missing_field(edit, "content"))?;
                    let start_idx = self.position_of(start_id, "Start", "start_id")?;
                    let end_idx = match edit.end_id.as_ref().filter(|s| !s.is_empty()) {
                        Some(end_id) => {
                            let idx = self.position_of(end_id, "End", "end_id")?;
                            if start_idx > idx {
                                bail!("VALIDATION_ERROR: start_id sort order is after end_id sort order.");
                            }
                            idx
                        }
                        None => start_idx,
                    };

                    // A payload is the lines it has (D-01M280Y0JPPWBG), and an
                    // address is a span of one or more. The span's first line
                    // keeps its sequence number and takes the first of the
                    // payload; the rest of the span goes; what is left of the
                    // payload follows as new lines. No lines is no lines: an
                    // empty payload takes the span out.
                    let mut contents = split_insert_content(content);
                    if contents.is_empty() {
                        self.lines.drain(start_idx..=end_idx);
                    } else {
                        let rest = contents.split_off(1);
                        self.lines[start_idx].content = contents.pop().unwrap_or_default();
                        modified.push(self.id_at(start_idx));
                        // A one-line span drains `start_idx + 1..=start_idx`,
                        // which is empty, so the span needs no test of its own.
                        self.lines.drain(start_idx + 1..=end_idx);
                        if !rest.is_empty() {
                            self.insert_at(start_idx + 1, rest, &mut modified);
                        }
                    }
                }

                EditOp::ReplaceSubstring => {
                    let start_id = edit
                        .start_id
                        .as_ref()
                        .ok_or_else(|| missing_field(edit, "start_id"))?;
                    let pattern = edit
                        .pattern
                        .as_ref()
                        .ok_or_else(|| missing_field(edit, "pattern"))?;
                    let replacement = edit
                        .replacement
                        .as_ref()
                        .ok_or_else(|| missing_field(edit, "replacement"))?;
                    let occurrence = edit.occurrence.unwrap_or(1);
                    if occurrence < 1 {
                        bail!("Invalid occurrence number: {}. Must be >= 1.", occurrence);
                    }
                    let idx = self.position_of(start_id, "Start", "start_id")?;

                    let current = &self.lines[idx].content;
                    let start = current
                        .match_indices(pattern.as_str())
                        .nth(occurrence - 1)
                        .map(|(pos, _)| pos)
                        .with_context(|| {
                            crate::tools::metadata::get_config()
                                .error_pattern_not_found
                                .replacen("{}", &occurrence.to_string(), 1)
                                .replacen("{}", pattern, 1)
                        })?;

                    let mut updated = String::with_capacity(current.len());
                    updated.push_str(&current[..start]);
                    updated.push_str(replacement);
                    updated.push_str(&current[start + pattern.len()..]);
                    self.lines[idx].content = updated;
                    modified.push(self.id_at(idx));
                }

                EditOp::Delete => {
                    let start_id = edit
                        .start_id
                        .as_ref()
                        .ok_or_else(|| missing_field(edit, "start_id"))?;
                    let idx = self.position_of(start_id, "Start", "start_id")?;
                    let end_idx = match edit.end_id.as_ref().filter(|s| !s.is_empty()) {
                        Some(end_id) => {
                            let last = self.position_of(end_id, "End", "end_id")?;
                            if idx > last {
                                bail!("VALIDATION_ERROR: start_id sort order is after end_id sort order.");
                            }
                            last
                        }
                        None => idx,
                    };
                    self.lines.drain(idx..=end_idx);

                    // Report the line now nearest the hole, so the agent has a
                    // live id to anchor its next edit on.
                    if !self.lines.is_empty() {
                        let neighbour = if idx < self.lines.len() {
                            idx
                        } else {
                            self.lines.len() - 1
                        };
                        modified.push(self.id_at(neighbour));
                    }
                }

                EditOp::Move => {
                    let start_id = edit
                        .start_id
                        .as_ref()
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| missing_field(edit, "start_id"))?;
                    let end_id = edit
                        .end_id
                        .as_ref()
                        .filter(|s| !s.is_empty())
                        .unwrap_or(start_id);
                    let move_pos = edit.move_position.unwrap_or(MovePosition::After);

                    let start_idx = self.position_of(start_id, "Start", "start_id")?;
                    let end_idx = self.position_of(end_id, "End", "end_id")?;
                    if start_idx > end_idx {
                        bail!("VALIDATION_ERROR: start_id sort order is after end_id sort order for move.");
                    }

                    // Where the block lands is decided against the lines that
                    // stay put, so resolve the destination before lifting it.
                    let dest_idx = match move_pos {
                        MovePosition::Prepend | MovePosition::Append => None,
                        MovePosition::Before | MovePosition::After => {
                            let dest_id = edit
                                .dest_id
                                .as_ref()
                                .filter(|s| !s.is_empty())
                                .ok_or_else(|| missing_field(edit, "dest_id"))?;
                            let idx = self.position_of(dest_id, "Destination", "dest_id")?;
                            if idx >= start_idx && idx <= end_idx {
                                bail!("VALIDATION_ERROR: Cannot move a range into itself (dest_id lies within source range).");
                            }
                            Some(idx)
                        }
                    };

                    let block: Vec<BufLine> = self.lines.drain(start_idx..=end_idx).collect();
                    let moved = block.len();

                    let at = match (move_pos, dest_idx) {
                        (MovePosition::Prepend, _) => 0,
                        (MovePosition::Append, _) => self.lines.len(),
                        (pos, Some(dest)) => {
                            // The drain shifted every position after the block.
                            let dest = if dest > end_idx { dest - moved } else { dest };
                            if pos == MovePosition::Before {
                                dest
                            } else {
                                dest + 1
                            }
                        }
                        (_, None) => unreachable!("before/after always resolve a destination"),
                    };

                    for (offset, line) in block.into_iter().enumerate() {
                        self.lines.insert(at + offset, line);
                        modified.push(self.id_at(at + offset));
                    }
                }
            }
        }

        Ok(modified)
    }
}

/// The path a session tracks. Content lives on disk, so most operations need
/// to get back to it from a session id alone.
fn session_filepath(conn: &Connection, session_id: &str) -> Result<String> {
    conn.query_row(
        "SELECT filepath FROM sessions WHERE session_id = ?1",
        [session_id],
        |row| row.get(0),
    )
    .with_context(|| format!("No session found for session_id={}", session_id))
}

/// Read a session's file and pair each line with the identity the index holds
/// for it. The index stores order and identity; the text comes from disk.
pub(crate) fn load_buffer(conn: &mut Connection, session_id: &str) -> Result<LineBuffer> {
    let filepath = session_filepath(conn, session_id)?;

    // Callers reach here through init_edit_session, which reconciles, so the
    // index normally matches. It can still be short or long if the file changed
    // in the window between the two. Reconciling again keeps the ids an agent
    // is holding, where renumbering the file 1..N would discard every one of
    // them without saying so — and persist that through the next commit.
    let mut reconciled = false;
    loop {
        let content = fs::read_to_string(&filepath)
            .with_context(|| format!("Failed to read file for line buffer: {}", filepath))?;
        let mut parts: Vec<&str> = content.split('\n').collect();
        if parts.last() == Some(&"") {
            parts.pop();
        }
        let disk: Vec<String> = parts
            .iter()
            .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
            .collect();

        let index: Vec<(i64, Option<String>)> = {
            let mut stmt = conn.prepare(
                "SELECT sequence_id, parent_context FROM lines WHERE session_id = ?1 ORDER BY sort_order"
            )?;
            let rows = stmt.query_map([session_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };

        if index.len() != disk.len() {
            if reconciled {
                bail!(
                    "{}",
                    crate::tools::metadata::get_config()
                        .error_file_written_while_read
                        .replacen("{}", &filepath, 1)
                );
            }
            reconcile_index(conn, session_id, &filepath, &[])?;
            reconciled = true;
            continue;
        }

        let lines: Vec<BufLine> = disk
            .into_iter()
            .zip(index)
            .map(|(content, (seq, parent_context))| BufLine {
                seq,
                content,
                parent_context,
            })
            .collect();

        let stored_next: i64 = conn
            .query_row(
                "SELECT next_line_id FROM sessions WHERE session_id = ?1",
                [session_id],
                |row| row.get(0),
            )
            .unwrap_or(1);
        let highest_live = lines.iter().map(|line| line.seq).max().unwrap_or(0);
        return Ok(LineBuffer::new(
            lines,
            std::cmp::max(stored_next, highest_live + 1),
        ));
    }
}
