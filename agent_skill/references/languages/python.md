# Python Language Reference - `ast-editor`

Templates and queries for **Python** (`.py`).

## Templates

`functions`, `classes` and `imports`, via `inspect --template`.

## Queries

- **Functions and methods**: `(function_definition name: (identifier) @function)`
- **Classes**: `(class_definition name: (identifier) @class)`
- **Imports**: `[(import_statement) @import (import_from_statement) @import]`
