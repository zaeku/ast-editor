# JavaScript / TypeScript Language Reference - `ast-editor`

This document details the AST queries, templates, and compiler/linter integration details for **JavaScript / TypeScript / TSX** (`.js`, `.ts`, `.tsx`).

---

## 1. Syntax Inspection Template
The `"functions"`, `"classes"`, and `"imports"` templates are supported via `inspect_ast`.

## 2. Common S-Expression Queries
*   **List All Functions**: `[(function_declaration) @func (arrow_function) @func (method_definition) @func]`
*   **List Imports**: `(import_paragraph) @import`
*   **List Interface Names**: `(interface_declaration name: (type_identifier) @interface)`
