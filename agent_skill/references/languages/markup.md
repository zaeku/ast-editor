# Markup & Configuration Reference - `ast-editor`

Queries for **HTML** (`.html`, `.htm`), **JSON** (`.json`), **YAML** (`.yaml`,
`.yml`), **TOML** (`.toml`) and **Markdown** (`.md`, `.markdown`).

## Templates

Markdown has `headings`, `headers`, `codeblocks`, `code_blocks`, `links`,
`tables` and `lists`. The others have none; write the query.

## Queries

- **HTML links and scripts**: `[(element (start_tag (tag_name) @tag (#eq? @tag "link"))) @import (script_element) @import]`
- **JSON keys**: `(pair key: (string) @key)`
- **YAML mapping keys**: `(block_mapping_pair key: (flow_node) @key)`
- **TOML tables and keys**: `[(table (bare_key) @table) (pair (bare_key) @key)]`
