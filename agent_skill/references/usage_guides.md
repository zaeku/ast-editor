# Usage Guides - `ast-editor`

This document outlines key editing concepts, best practices, and transactional workflows for the `ast-editor` tool suite.

---

## 1. Shift-Invariant Targeting

Unlike standard search-and-replace tools, the `edit` tool targets specific lines using stable sequence/hash IDs (e.g., `"1#dfca"`).
*   **Immune to Line Shifting**: Inserting or deleting lines in one part of a file does not shift the Line IDs of other lines. Any line shifts will not invalidate your references or write changes to incorrect positions.
*   **Safer than Content Matching**: If a file contains duplicate lines of code, targeting precise, unique Line IDs guarantees that the exact intended line is modified, eliminating duplicate matching errors.
*   **Hash Reuse Optimization (Double-turn Avoidance)**: If the content of a line has not changed, its Line ID remains stable and invariant. You can reuse previous Line IDs directly in subsequent edits without calling `view` again.

---

## 2. Stateless Local Cache & Lock-Free Design

To track Line IDs across editing operations, a SQLite database is maintained in the user directory as a stateless local cache.
*   **No File Locks**: The database functions as a lightweight cache. Source files on disk are read, written, and closed instantly. There are no persistent file locks held on workspace files.
*   **Markdown & Plain Text Compatibility**: Syntax validation is completely skipped for Markdown, plain text, and unsupported configuration files. Syntax validation omission or failure on these files **never blocks** updates.

---

## 3. When a File Changes Underneath

When a file has changed outside the tool, its line index is reconciled against what is now on disk rather than rebuilt. A patience diff over the stored line hashes decides what survived: unchanged lines keep their IDs, a line that only moved keeps its ID, and a line that was merely respaced by a formatter keeps its ID too. Only genuinely new lines draw a new one, and a retired ID is never handed out again.

That happens silently for lines you are not editing. If a line **your edit targets** did not survive — its content changed or it was deleted — the edit is refused with a `CONCURRENCY_ERROR` naming it, rather than being applied to whatever is at that position now. Re-read the file to get current IDs and try again.

---

## 4. Editing Long Lines (`replace_substring`)

Surgical updates on long lines (lines exceeding 2,048 characters) are supported via the `"replace_substring"` operation. This avoids token explosion while still permitting precise edits inside long lines (e.g., minified JS, long strings, large JSON arrays, or HTML files).
*   **Workflow**: Use `view` (which soft-wraps the lines for display), locate the target Line ID, and apply edits with the `"replace_substring"` operation, providing the search `pattern` and target `replacement`.

---

## 5. `"replace_range"` Guidelines & Examples

The `"replace_range"` operation is designed to replace a continuous block of lines in a single atomic transaction. It is highly recommended over sending multiple single-line `"replace"` or `"delete"` operations in a loop.

### Advantages:
1. **Safety & Atomicity**: The entire range replacement is validated as a single block. If any syntax error is introduced, the entire replacement is rolled back.
2. **Token Efficiency**: Bypasses multiple round-trips and reduces overhead by sending a single edit payload.
3. **No Line Shifting Collision**: Replacing a range at once guarantees that Line IDs remain stable, avoiding potential issues with shifted indices during sequential updates.

### Example: Replacing a multi-line function definition
```json
{
  "filepath": "/path/to/project/src/main.rs",
  "edits": [
    {
      "op": "replace_range",
      "target_id": "1a#b029",
      "end_target_id": "22#f8c3",
      "content": "pub fn execute() -> Result<()> {\n    println!(\"Updated content!\");\n    Ok(())\n}"
    }
  ]
}
```

---

## 6. Agent-Native Positioning Workflow (Token Savings)

When creating large files via `create`, passing `return_ids: false` saves significant token costs by bypassing line serialization and hashing.
Since the file has just been initialized:
1. **Implicit Mapping**: The agent knows the 1-indexed line numbers correspond 1-to-1 to the indices of the input content split by `\n`.
2. **Local ID Construction**: For a newly created file, the agent can locally construct the Line ID for line number $N$ using the hexadecimal format of $N$ and the first 4 characters of the SHA-1 hex hash of the line content (e.g., `<hex_N>#<sha1_prefix>`).
3. **Querying as Needed**: If the agent wants to edit a specific range later, it can call `view` with `only_ids: true` for just that narrow range. This retrieves only the necessary Line IDs and line numbers, keeping the payload content-free.
