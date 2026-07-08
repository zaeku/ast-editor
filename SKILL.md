---
name: tree-sitter-inspector
description: >
  Inspects code structure and finds target lines using Tree-sitter S-expression queries. Resilient to syntax errors.
---

# Tree-Sitter Code Inspector Skill

This skill allows agents to analyze source code structures and identify specific lines of interest using Tree-sitter S-expression queries. It is highly resilient to syntax errors, supports multiple languages, and operates as a Model Context Protocol (MCP) server to eliminate terminal command execution warnings.

## Supported Languages

The following languages and file extensions are currently supported out-of-the-box:
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

## Setup and Dependencies

This skill is powered by a Rust-based MCP server using `wasmtime` (WebAssembly) and precompiled `.wasm` grammars with native AOT caching for optimal performance.

### Registering the MCP Server
To enable this skill, you must register it in your `mcp_config.json` configuration file:

```json
{
  "mcpServers": {
    "tree-sitter-inspector": {
      "command": "/Users/zaeku/.gemini/config/plugins/custom-developer-plugin/skills/tree-sitter-inspector/scripts/tree-sitter-inspector",
      "args": []
    }
  }
}
```

Once registered, the MCP server will run in the background and expose the `tree_sitter_inspect` tool directly to the agent without requiring command approval prompts.

## Execution Instructions

This skill exposes two primary tools: `tree_sitter_inspect` and `tree_sitter_dump_tree`.

### 1. `tree_sitter_inspect`

Use this tool to find targeted syntax structures using Tree-sitter queries.

#### Arguments
*   `file` (string, required): Absolute or relative path to the file to inspect.
*   `query` (string, optional): Tree-sitter S-expression query. If omitted, falls back to the default outline query (classes and functions).
*   `template` (string, optional): Predefined query template. Choose from:
    *   `functions`: Extract only functions and methods.
    *   `classes`: Extract classes, structs, enums, interfaces, etc.
    *   `imports`: Extract all import statements.
*   `include_code` (boolean, optional, default: `true`): Whether to include the source code of the enclosing definition.
*   `code_format` (string, optional, default: `"lines"`): Format of the returned code. Options:
    *   `"lines"`: Prefixes each line with its 1-indexed line number (e.g. `40: def my_func():`), compatible with `view_file`.
    *   `"raw"`: Returns the raw code string without line number prefixes.
*   `output_file` (boolean, optional, default: `false`): If `true`, saves the full matches JSON payload to a file in the plugin's outputs directory and returns only a lightweight summary. Highly recommended for large source files to avoid context/token overflow and bypass platform redirects.

#### Common Tree-sitter S-Expression Queries (for manual query override)

##### Python
*   **List All Functions/Methods**: `(function_definition name: (identifier) @function)`
*   **List All Classes**: `(class_definition name: (identifier) @class)`

##### JavaScript / TypeScript
*   **List All Functions**: `[(function_declaration) @func (arrow_function) @func (method_definition) @func]`

##### HTML
*   **List All Imports**: `[(element (start_tag (tag_name) @tag (#eq? @tag "link"))) @import (script_element) @import]`

##### JSON
*   **List All Keys**: `(pair key: (string) @key)`

##### YAML
*   **List All Mapping Keys**: `(block_mapping_pair key: (flow_node) @key)`

##### TOML
*   **List All Tables and Keys**: `[(table (bare_key) @table) (pair (bare_key) @key)]`

##### Swift
*   **List All Classes/Structs/Enums/Protocols**: `[(class_declaration name: (type_identifier) @class) (protocol_declaration name: (type_identifier) @protocol)]`
*   **List All Functions**: `(function_declaration name: (simple_identifier) @function)`
*   **List All Imports**: `(import_declaration) @import`

### 2. `tree_sitter_dump_tree`

Use this tool to view the hierarchical AST structure of a file. It is especially useful for inspecting node types before writing custom queries.

#### Arguments
*   `file` (string, required): Path to the file.
*   `max_depth` (integer, optional): Maximum depth to traverse (default: `3`).
*   `start_line` (integer, optional): Start line number (1-indexed) to locate a specific node.
*   `end_line` (integer, optional): End line number (1-indexed) to locate a specific node. If omitted, defaults to `start_line`.

## Output Interpretation

### `tree_sitter_inspect` Output
The tool outputs structured JSON containing:
- `status`: `"success"` or `"error"`.
- `has_syntax_errors`: `true` if tree-sitter detected syntax errors (but it still parsed the file).
- `matches`: A list of matches sorted by line number.
  - `start_line` (1-indexed)
  - `end_line` (1-indexed)
  - `text` (matched code string, typically the identifier/name node)
  - `capture_name` (name assigned in query via `@`)
  - `definition`: Enclosing block definition metadata:
    - `type` (syntax node type, e.g., `function_definition`, `export_statement`)
    - `start_line` (1-indexed start line of definition)
    - `end_line` (1-indexed end line of definition)
    - `block_hash` (8-character short MD5 hash of the raw block text for optimistic concurrency control)
    - `text` (formatted or raw code text of the definition, included if `include_code` is `true`)

Use the `definition` field's `text` directly to view the definition code without requiring a separate `view_file` call.

### Large File Optimization & CLI Pipeline Filtering (`output_file: true`)

If `output_file` is set to `true`, the MCP tool will write the query result to a unique file under the plugin's `outputs/` folder and return a compact summary in the following structure over Stdio:
```json
{
  "status": "success",
  "filepath": "/path/to/target_file.rs",
  "language": "rust",
  "has_syntax_errors": false,
  "match_count": 42,
  "saved_to_file": "/Users/zaeku/.../outputs/inspect_output_1783504233_b7feac63.json",
  "hint": "The full query result has been saved to the file specified in 'saved_to_file'. You can analyze it using jq, jc, or ripgrep."
}
```

Since the full list of matches is saved directly to the disk, calling agents should leverage **high-performance CLI tools** like `jq` or `ripgrep` directly on the `saved_to_file` path to query details rather than loading the entire JSON file into the context window.

#### Filtering Examples (파이프라인 활용 모범 사례)
1. **Find Start/End Lines of a Specific Function (특정 함수의 시작/끝 라인 추출)**
   ```bash
   jq '.matches[] | select(.text == "main") | {start_line, end_line}' <saved_to_file>
   ```
2. **Extract All Matched Function Names (매칭된 함수명 전체 목록화)**
   ```bash
   jq -r '.matches[].text' <saved_to_file>
   ```
3. **Filter Matches for Syntax Errors (구문 에러 유무 단일값 파싱)**
   ```bash
   jq '.has_syntax_errors' <saved_to_file>
   ```
4. **Locate Specific Captures with ripgrep (빠른 문자열 탐색)**
   ```bash
   rg '"definition"' <saved_to_file>
   ```

Using these CLI pipelines ensures **O(1) memory and token consumption** for the agent, letting you query files of arbitrary sizes in milliseconds.

## Handling Unsupported Languages (지원하지 않는 언어 대응 지침)

If you attempt to use this tool on a file extension that is not currently supported, the tool will return an `Unsupported file extension` error. In this case, follow these fallback steps:

1. **Fallback to `grep_search`**: Use regex-based grep to search for declarations (e.g. `class `, `def `, `func ` etc.) to find the starting line numbers.
2. **Fallback to `view_file`**: Read the file in chunks or view key sections to manually locate classes/functions.
3. **Do NOT block the task**: Proceed with the task using standard tools; do not get stuck trying to force tree-sitter queries.

## Requesting Language Support Extension (새로운 언어 지원 추가 요청 방법)

If you need to analyze a language that is not currently supported (e.g., Kotlin, PHP, C#), you do NOT need to recompile the binary or edit the source code.

For reference (for the User to extend support):
1. Obtain the precompiled `tree-sitter-<lang>.wasm` file (compiled with ABI version 13 to 15, ideally using `tree-sitter-cli v0.26` or compatible).
2. Place the `.wasm` file in the plugin's [resources/wasm/](file:///Users/zaeku/.gemini/config/plugins/custom-developer-plugin/skills/tree-sitter-inspector/resources/wasm) directory.
3. Register the file extension and parser mapping in the plugin's [languages.json](file:///Users/zaeku/.gemini/config/plugins/custom-developer-plugin/skills/tree-sitter-inspector/resources/wasm/languages.json) (e.g., `"kotlin": { "extensions": [".kt"], "wasm_file": "tree-sitter-kotlin.wasm" }`).
