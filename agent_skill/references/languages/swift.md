# Swift Language Reference - `ast-editor`

Templates and queries for **Swift** (`.swift`).

## Templates

`functions`, `classes` and `imports`. `outline` lists what a file declares from
the first two.

## Queries

- **Classes and protocols**: `[(class_declaration name: (type_identifier) @class) (protocol_declaration name: (type_identifier) @protocol)]`
- **Functions**: `(function_declaration name: (simple_identifier) @function)`
- **Imports**: `(import_declaration) @import`
