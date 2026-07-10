---
name: ast-editor
description: >
  Inspects code structure, finds target lines, and performs transactionally-validated line-level code edits using Tree-sitter. Resilient to syntax errors.
---

# AST-based Code Editor and Inspector Skill

This skill provides a powerful **general-purpose line-level text editing framework** for all text files (including Markdown, plain text, etc.), while **additionally** providing robust Abstract Syntax Tree (AST) query and syntax validation tools for 21 supported programming and configuration languages. It operates as a Model Context Protocol (MCP) server to eliminate terminal command execution warnings.

---

## 1. General-Purpose Line-Level Editing

The tool can be used to view and edit **any text file** on the filesystem. When editing files that are not in the supported languages list (such as Markdown or plain text), the tool functions as a general line-level editor, skipping AST syntax validation but preserving transactional and concurrency safety.

### Key Editing Concepts

#### A. Shift-Invariant Targeting
Unlike standard search-and-replace tools (e.g., `replace_file_content`), the `edit_lines` tool targets specific lines using stable sequence/hash IDs (e.g., `"1#dfca"`) and floating-point sort orders.
*   **Immune to Line Shifting**: Inserting or deleting lines in one part of a file does not shift the Line IDs of other lines. Any line shifts will not invalidate your references or write changes to incorrect positions.
*   **Safer than Content Matching**: When a file contains duplicate lines of code, search-and-replace tools might match and overwrite the wrong occurrence. By targeting precise, unique Line IDs, `edit_lines` guarantees that the exact intended line is modified, eliminating duplicate matching errors.
*   **Hash Reuse Optimization (Double-turn Avoidance)**: If the content of a line has not changed, its Line ID and hash remain stable and invariant. You can reuse previous Line IDs directly in subsequent edits without calling `view_lines` again. This avoids redundant round-trips and saves context tokens.

#### B. Stateless Local Cache & Lock-Free Design
To track Line IDs across editing operations, a SQLite database is maintained in the user directory as a stateless local cache.
*   **No File Locks**: The database functions as a lightweight cache. Source files on disk are read, written, and closed instantly—exactly like standard stateless tools. There are no persistent file locks held on workspace files.
*   **Markdown & Plain Text Compatibility**: Syntax validation is completely skipped for Markdown, plain text, and unsupported configuration files. Syntax validation omission or failure on these files **never blocks** updates; it simply bypasses the AST parser and writes the line edits cleanly to disk.

#### C. Paragraph & Block-Level Suitability
The tool is designed to be highly suitable for paragraph and block-level text editing.
*   **Multi-Line Content Support**: The `content` field in line edit operations accepts multi-line strings (lines separated by `\n`). You can insert or update entire multi-line blocks of text in a single operation.
*   **Paragraph Replacements**: Use the `"replace_range"` operation with a start `target_id` and an end `end_target_id` to replace an entire paragraph or block of lines securely, without having to calculate line offsets or issue separate line-by-line updates.

---

### The Editing Lifecycle
The editing workflow follows a structured transaction cycle depending on whether the file is new or already exists:
$$\text{(Create Lines } \lor \text{ View Lines)} \rightarrow \text{Edit Lines}$$

*Note: Session initialization is handled automatically either JIT (Just-in-Time) behind the scenes or explicitly during file creation.*

#### A. `create_lines`
Creates a brand-new file with the initial content and initializes its line editing session. It returns the list of lines with unique IDs immediately, avoiding an extra view call.
*   **Safety Features**: To prevent accidental overwriting, the tool fails if the file already exists (returns an error matching `FILE_ALREADY_EXISTS`).
*   **Arguments**:
    *   `filepath` (string, required): Absolute path to the file.
    *   `content` (string, required): Initial text content of the file.
*   **Response Safety Limits**:
    *   **Line Count Cap**: Output lines are capped at a maximum of 800 lines.
    *   **Response Capacity Cap**: Output payload size is limited to 45,000 bytes (approx. 44KB) to protect the context window.
    *   **Line Length Cap**: Lines exceeding 2,048 characters are truncated in the returned view with a truncation notice and are given a `#TRUNC` suffix in their Line ID (e.g., `12#TRUNC`).
*   **Output Format**: Returns a JSON object indicating the status, a success/warning message (containing truncation details if any limits were hit), the column mapping, the list of generated line items, and session metadata (`total_lines`, `total_bytes`, `showing_start`, `showing_end`).

#### B. `view_lines`
Retrieves lines along with their persistent unique Line IDs for a given file range. If no session exists for the file, it automatically JIT-initializes the session.
*   **Arguments**:
    *   `filepath` (string, required): Absolute path to the file.
    *   `start_line` (integer, required): 1-indexed starting line.
    *   `end_line` (integer, required): 1-indexed ending line.
*   **Response Safety Limits**:
    *   **Line Count Cap**: Output lines are capped at a maximum of 800 lines per call. If the requested range is larger, it will be automatically clamped to 800 lines and a warning will be added.
    *   **Response Capacity Cap**: Output payload size is limited to 45,000 bytes (approx. 44KB) to protect the context window. If the cumulative content length reaches this limit, lines are truncated and a warning is added.
    *   **Line Length Cap**: Lines exceeding 2,048 characters are truncated in the returned view with a truncation notice (e.g., `... [TRUNCATED: Line is too long. DO NOT UPDATE this line directly unless replacing it completely.]`) and are given a `#TRUNC` suffix in their Line ID (e.g., `3f#TRUNC`, where `3f` is the hexadecimal sequence ID).

#### C. `edit_lines`
Transactionally applies one or more line-level edits, runs AST-based syntax validation (if supported), validates file concurrency, updates the file on disk, and returns a +/- 2 line context preview. If no session exists, it JIT-initializes the session.
*   **Arguments**:
    *   `filepath` (string, required): Absolute path to the file.
    *   `edits` (array of objects, required): A list of line edit operations.

---

### LineEdit Operations Schema
Each element in the `edits` array of `edit_lines` is an object representing a single edit operation.

*   `op` (string, required): The operation to perform. Supported values:
    *   `"insert_before"`: Insert new line(s) before a target line. If `target_id` is omitted/empty, it prepends content to the top of the file.
    *   `"insert_after"`: Insert new line(s) after a target line. If `target_id` is omitted/empty, it appends content to the end of the file.
    *   `"update"`: Replace the content of a target line.
    *   `"delete"`: Delete a target line.
    *   `"replace_range"`: Replace a range of lines.
    *   `"move"`: Move a line or a block of lines to a new position.
*   `target_id` (string): The sequence/hash identifier of the target line (e.g. `"1#fa89"`), obtained from `view_lines` or `inspect_ast`. Required for `update`, `delete`, `replace_range`, and `move`. Optional/omitted for `insert_before` and `insert_after`.
*   `content` (string): The text content to insert or update. Required for `insert_before`, `insert_after`, `update`, and `replace_range`. Can contain multiple lines separated by `\n`.
*   `end_target_id` (string): The ending target line ID of the range. Required for `replace_range`.
*   `dest_target_id` (string): The line ID where the moved block should be placed. Required for `move`.
*   `move_position` (string): Position relative to the destination line for moved blocks: `"before"`, `"after"`, `"prepend"`, or `"append"`. Required for `move`.

---

### JSON Output Format
Tools returning line lists return a structured, type-safe, self-documenting JSON format.

#### `create_lines` Output Format
Returns a confirmation status, a success/warning message, lines list, and session metadata:
```json
{
  "status": "success",
  "message": "File successfully created and line editing session initialized.",
  "columns": ["id", "n", "content"],
  "lines": [
    ["1#9d33", 1, "use anyhow::{Result, Context};"],
    ["2#c3b3", 2, "use crate::tools::session_db::{get_db_connection, ensure_hashes_for_range, compute_line_hash};"]
  ],
  "total_lines": 2,
  "total_bytes": 135,
  "showing_start": 1,
  "showing_end": 2
}
```

#### `view_lines` and `edit_lines` Output Format
`view_lines` and `edit_lines` previews return a structured layout with explicit columns metadata, session statistics, and warnings if any capacity/line limits were reached:
```json
{
  "columns": ["id", "n", "content"],
  "lines": [
    ["1#9d33", 1, "use anyhow::{Result, Context};"],
    ["2#c3b3", 2, "use crate::tools::session_db::{get_db_connection, ensure_hashes_for_range, compute_line_hash};"],
    ["3#da39", 3, ""]
  ],
  "total_lines": 3,
  "total_bytes": 135,
  "showing_start": 1,
  "showing_end": 3,
  "tip": "Edit these lines by calling 'edit_lines' with the line IDs (e.g. 1#9d33) shown above."
}
```
*   `columns`: Describes the array schema (`"id"` is line ID, `"n"` is line number, `"content"` is code/text content).
*   `lines`: An array of JSON arrays, where each entry matches the columns order `[id, n, content]`.
*   `total_lines` (integer): The total number of lines in the file's current session.
*   `total_bytes` (integer): The total size of the file on disk in bytes.
*   `showing_start` (integer): The 1-indexed starting line number of the displayed slice.
*   `showing_end` (integer): The 1-indexed actual ending line number of the displayed slice.
*   `message` (string, optional): A warning message populated if limits are exceeded (e.g. `"Line count limit (800 lines max) exceeded. Output capped at 800 lines."` or `"Response truncated: cumulative response size limit (45,000 bytes) was reached."`).

---

### Concurrency Protection
When a session is initialized JIT, the file's modification time (`mtime`) is cached. During `edit_lines`, the current filesystem `mtime` is checked. If it is different from the cached `mtime`, it indicates that the file was modified externally. The edit is rejected to prevent overwriting third-party or concurrent changes.

---

### Long Line Edit Protection

To prevent accidental data corruption and token bloat, the tool prevents surgical updates (via `update` or `replace_range`) on any line whose content exceeds 2,048 characters, or whose Line ID ends with `#TRUNC`.

#### The `LINE_TOO_LONG_ERROR`
If you attempt to edit a line that is too long, the operation is rejected with the following error:
```
LINE_TOO_LONG_ERROR: Line is too long (N chars) and has been truncated in the view. Surgical updates on truncated lines are disabled to prevent accidental data loss. Please format the file using a code beautifier (e.g. prettier, black, or cargo fmt) to break it into multiple lines, or rewrite the file using create_lines/write_to_file.
```

#### Workflow to Resolve `LINE_TOO_LONG_ERROR`
When this error occurs, you must not attempt to edit the line surgically. Instead, follow this workflow:
1.  **Format the Code**: Run an appropriate code beautifier or formatter on the target file (e.g., `prettier --write` for JS/TS/HTML/JSON, `black` for Python, `cargo fmt` for Rust, etc.) to format and split the single long line into multiple smaller, manageable lines.
2.  **Re-view and Edit**: Call `view_lines` again to obtain the updated Line IDs for the formatted code, and then apply your edits to the newly created multiple lines.
3.  **Alternative (Whole File Re-write)**: If formatting is not suitable, you may rewrite the entire file using `create_lines` (with `Overwrite` if supported, or via standard file write tools like `write_to_file`) to replace the long lines completely.

---

## 2. AST-based Syntax Inspection & Validation

For 21 supported programming and configuration languages, the tool provides additional structural analysis tools and automatic syntax validation.

### Supported Languages
*   **Python** (`.py`)
*   **JavaScript / TypeScript / TSX** (`.js`, `.jsx`, `.ts`, `.tsx`)
*   **Go** (`.go`)
*   **Rust** (`.rs`)
*   **Java** (`.java`)
*   **C / C++** (`.c`, `.h`, `.cpp`, `.cc`, `.cxx`)
*   **Lua** (`.lua`)
*   **HTML** (`.html`, `.htm`)
*   **JSON** (`.json`)
*   **YAML** (`.yaml`, `.yml`)
*   **TOML** (`.toml`)
*   **Swift** (`.swift`)

### Inspection Tools

#### A. `inspect_ast`
Use this tool to find targeted syntax structures using Tree-sitter queries.
*   **Arguments**:
    *   `file` (string, required): Absolute or relative path to the file to inspect.
    *   `query` (string, optional): Tree-sitter S-expression query. If omitted, falls back to outline templates.
    *   `template` (string, optional): Predefined query template: `"functions"`, `"classes"`, or `"imports"`.
    *   `include_code` (boolean, optional, default: `true`): Whether to include the source code of the enclosing definition (returns compact columns + lines format).
    *   `code_format` (string, optional, default: `"lines"`): Format of the returned code.
    *   `output_file` (boolean, optional, default: `false`): If `true`, saves matches payload to a file in outputs folder to bypass token limits.

#### B. `dump_ast`
Use this tool to view the hierarchical AST structure of a file to design custom queries.
*   **Arguments**:
    *   `file` (string, required): Path to the file.
    *   `max_depth` (integer, optional, default: `3`): Maximum depth to traverse.
    *   `start_line` (integer, optional): 1-indexed start line.
    *   `end_line` (integer, optional): 1-indexed end line.

> [!TIP]
> **JIT Edit Session Caching**: 
> Running `inspect_ast` on a file automatically initializes the editing session under the hood in the background. Therefore, after calling `inspect_ast`, you can immediately invoke `view_lines` or `edit_lines` on the same file without needing to call any initialization tool.

---

### Syntax Validation & Automatic Rollback
For supported languages, after applying edits in the database session, the resulting code is passed to the WebAssembly tree-sitter parser. If any new `ERROR` or `MISSING` nodes are detected in the AST, the edit is aborted, the SQLite transaction is rolled back, and the tool returns a validation error detailing the syntax problem.

---

## Common Tree-sitter S-Expression Queries

### Python
*   **List All Functions/Methods**: `(function_definition name: (identifier) @function)`
*   **List All Classes**: `(class_definition name: (identifier) @class)`

### JavaScript / TypeScript
*   **List All Functions**: `[(function_declaration) @func (arrow_function) @func (method_definition) @func]`

### HTML
*   **List All Imports**: `[(element (start_tag (tag_name) @tag (#eq? @tag "link"))) @import (script_element) @import]`

### JSON
*   **List All Keys**: `(pair key: (string) @key)`

### YAML
*   **List All Mapping Keys**: `(block_mapping_pair key: (flow_node) @key)`

### TOML
*   **List All Tables and Keys**: `[(table (bare_key) @table) (pair (bare_key) @key)]`

### Swift
*   **List All Classes/Structs/Enums/Protocols**: `[(class_declaration name: (type_identifier) @class) (protocol_declaration name: (type_identifier) @protocol)]`
*   **List All Functions**: `(function_declaration name: (simple_identifier) @function)`
*   **List All Imports**: `(import_declaration) @import`
