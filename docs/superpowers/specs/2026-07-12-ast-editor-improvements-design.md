# Design Spec: Substring Replacement, Concurrency Resync, and Custom Lints

This design spec outlines the implementation of improvements to the `ast-editor` tool. These enhancements improve the usability of line-level editing, particularly for long lines, multi-agent concurrency, and custom markup/document linting.

---

## Detailed Requirements

### 1. `"replace_substring"` Operation & Long Line Editing
- **Remove Truncation Restrictions**: Remove the restriction that labels lines $> 2048$ chars as `#TRUNC` and blocks surgical updates.
- **Soft-Wrapped display in `view_lines`**: In `view_lines`, format long lines into visual soft-wrapped segments of 2,048 characters using dynamic spacing alignment:
  - First segment: `N: content` (where `N` is the 1-indexed line number)
  - Subsequent middle segments: `(L-1) spaces + │: content` (where `L` is the digit/character length of `N`)
  - Final segment: `(L-1) spaces + └: content` (where `L` is the digit/character length of `N`)
  - Each segment counts as 1 line towards the 800-line capping limit in `view_lines`.
  - The actual line IDs in `metadata_json` are generated normally without `#TRUNC` suffix.
- **Add `"replace_substring"` to `edit_lines`**:
  - `op`: `"replace_substring"`
  - `target_id` (string, required): target line's unique ID.
  - `pattern` (string, required): string to search for in the line.
  - `replacement` (string, required): replacement string.
  - `occurrence` (integer, optional, default: 1): 1-indexed count of which match of the pattern to replace.

### 2. Concurrency Smart Resync
- **Smart Validation**: When `edit_lines` detects a modified time (`mtime`) mismatch:
  - Load the current file from disk.
  - For each targeted line ID in the request, verify if that line (by unique hash/content) still exists in the latest file on disk.
  - If all targeted lines are unchanged and present, update the session DB to match the new file state (updating line numbers, offsets, hashes, and cached `mtime`), then apply the edits.
  - If a targeted line ID's content has changed or is deleted, fail with a concurrency conflict error.

### 3. Hybrid Verification & Custom Lints
- **Hard Fail & Rollback**: Guarantees syntax correctness of programming and configuration languages. Any syntax parse crash rolls back changes.
- **Soft Warning & Commit**: Non-critical structural rules for Markdown and HTML.
  - Validates parsed structures (e.g. unclosed fence blocks, incorrect link syntax like `[text(url)`, header hierarchy gaps, or invalid inline HTML tags).
  - Rather than rejecting the edit, writes it to disk and includes the warnings in the response message.
