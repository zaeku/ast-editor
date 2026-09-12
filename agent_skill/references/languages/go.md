# Go Language Reference - `ast-editor`

Templates and queries for **Go** (`.go`).

## Templates

`functions`, `classes` and `imports`, and two Go has of its own: `interfaces`
and `structs`.

## Queries

- **Functions**: `(function_declaration name: (identifier) @func)`
- **Named types**: `(type_spec name: (type_identifier) @type)`
- **Imports**: `(import_spec) @import`
