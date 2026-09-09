# Nix Language Reference - `ast-editor`

This document details the AST queries, templates, and compiler/linter integration details for **Nix** (`.nix`).

---

## 1. Syntax Inspection Template
Nix supports general-purpose syntax tree traversal via custom S-expression queries under `inspect`.

## 2. Common S-Expression Queries
*   **List Attribute Names**: `(binding attrpath: (attrpath (identifier) @attr))`
*   **List Let Bindings**: `(let_expression (binding attrpath: (attrpath (identifier) @binding_name)))`
*   **List Function Arguments**: `(formals (formal name: (identifier) @arg))`
