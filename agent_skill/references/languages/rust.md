# Rust Language Reference - `ast-editor`

This document details the AST queries, templates, and compiler/linter integration details for **Rust** (`.rs`).

---

## 1. Syntax Inspection Template
The `"functions"`, `"classes"`, `"imports"`, `"traits"`, and `"impls"` templates are supported via `inspect`.

## 2. Common S-Expression Queries
*   **List All Impls**: `(impl_item trait: (type_identifier) @trait)`
*   **List All Functions**: `(function_item name: (identifier) @function)`
*   **List All Structs**: `(struct_item name: (type_identifier) @struct)`

## 3. Other Compiled Languages
For other standard compiled languages such as **Java** (`.java`), **C/C++** (`.c`, `.cpp`), and **Lua** (`.lua`), standard templates `"functions"`, `"classes"`, and `"imports"` are supported.
