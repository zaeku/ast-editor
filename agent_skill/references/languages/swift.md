# Swift Language Reference - `ast-editor`

This document details the AST queries, templates, and compiler/linter integration details for **Swift** (`.swift`).

---

## 1. Syntax Inspection Template
Supports standard Traversal and custom queries under `inspect`.

## 2. Common S-Expression Queries
*   **List All Classes/Structs/Enums/Protocols**: `[(class_declaration name: (type_identifier) @class) (protocol_declaration name: (type_identifier) @protocol)]`
*   **List All Functions**: `(function_declaration name: (simple_identifier) @function)`
*   **List All Imports**: `(import_paragraph) @import`
