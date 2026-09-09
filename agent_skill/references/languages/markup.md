# Markup & Configuration Reference - `ast-editor`

This document details the AST queries, templates, and parser integration details for markup, document, and configuration formats: **HTML** (`.html`), **JSON** (`.json`), **YAML** (`.yaml`), **TOML** (`.toml`), and **Markdown** (`.md`).

---

## 1. HTML
*   **List All Imports (Links & Scripts)**: `[(element (start_tag (tag_name) @tag (#eq? @tag "link"))) @import (script_element) @import]`

## 2. JSON
*   **List All Keys**: `(pair key: (string) @key)`

## 3. YAML
*   **List All Mapping Keys**: `(block_mapping_pair key: (flow_node) @key)`

## 4. TOML
*   **List All Tables and Keys**: `[(table (bare_key) @table) (pair (bare_key) @key)]`

## 5. Markdown
*   **Predefined templates**: `"headings"`, `"headers"`, `"codeblocks"`, `"code_blocks"`, `"links"`, `"tables"`, or `"lists"` are supported via `inspect`.
