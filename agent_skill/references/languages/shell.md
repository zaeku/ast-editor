# Shell Script Language Reference - `ast-editor`

This document details the AST queries, templates, and parser details for **Shell Scripts (POSIX shell, Bash, Zsh, Ksh)** (`.sh`, `.bash`, `.zsh`, `.ksh`).

---

## 1. Syntax Inspection Template
Bash supports `"functions"` template via `inspect_ast`.

## 2. Common S-Expression Queries
*   **List All Functions**: `(function_definition name: (word) @function)`
*   **List Command Calls**: `(command_name) @command`
