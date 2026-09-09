# Python Language Reference - `ast-editor`

This document details the AST queries, templates, and compiler/linter integration details for **Python** (`.py`).

---

## 1. Syntax Inspection Template
The `"functions"`, `"classes"`, and `"imports"` templates are supported via `inspect`.

## 2. Common S-Expression Queries
*   **List All Functions/Methods**: `(function_definition name: (identifier) @function)`
*   **List All Classes**: `(class_definition name: (identifier) @class)`
*   **List All Imports**: `[(import_statement) @import (import_from_statement) @import]`
