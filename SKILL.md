---
name: ast-editor
description: >
  Inspects code structure, finds target lines, and performs transactionally-validated line-level code edits using Tree-sitter. Resilient to syntax errors.
---

# AST-based Code Editor and Inspector Skill

This skill provides a powerful **general-purpose line-level text editing framework** for all text files (including Markdown, plain text, etc.), while **additionally** providing robust Abstract Syntax Tree (AST) query and syntax validation tools for 21 supported programming and configuration languages. It operates as a Model Context Protocol (MCP) server to eliminate terminal command execution warnings.

---

## 1. General-Purpose Line-Level Editing

The tool can be used to view and edit **any text file** on the filesystem. When editing files that are not in the supported languages list, the tool functions as a general line-level editor (skipping AST syntax validation but preserving transactional and concurrency safety).

### The Editing Lifecycle
The editing workflow follows a structured 2-step transaction cycle:
$$\text{View Lines} \rightarrow \text{Edit Lines}$$

*Note: Session initialization is completely handled JIT (Just-in-Time) behind the scenes, so there is no need for a manual session opening step.*

#### A. `view_lines`
Retrieves lines along with their persistent unique Line IDs for a given file range. If no session exists for the file, it automatically JIT-initializes the session.
*   **Arguments**:
    *   `filepath` (string, required): Absolute path to the file.
    *   `start_line` (integer, required): 1-indexed starting line.
    *   `end_line` (integer, required): 1-indexed ending line.

#### B. `edit_lines`
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
Tools returning line lists (`view_lines` and `edit_lines` previews) return a structured, type-safe, self-documenting JSON array format with explicit columns metadata:

```json
{
  "columns": ["n", "id", "content"],
  "lines": [
    [1, "1#9d33", "use anyhow::{Result, Context};"],
    [2, "2#c3b3", "use crate::tools::session_db::{get_db_connection, ensure_hashes_for_range, compute_line_hash};"],
    [3, "3#da39", ""]
  ],
  "tip": "Edit these lines by calling 'edit_lines' with the line IDs (e.g. 1a#f8c9) shown above."
}
```
*   `columns`: Describes the array schema (`"n"` is line number, `"id"` is line ID, `"content"` is code content).
*   `lines`: An array of JSON arrays, where each entry matches the columns order.

---

### Concurrency Protection
When a session is initialized JIT, the file's modification time (`mtime`) is cached. During `edit_lines`, the current filesystem `mtime` is checked. If it is different from the cached `mtime`, it indicates that the file was modified externally. The edit is rejected to prevent overwriting third-party or concurrent changes.

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
