# JavaScript / TypeScript Language Reference - `ast-editor`

Templates and queries for **JavaScript / TypeScript / TSX** (`.js`, `.jsx`,
`.mjs`, `.cjs`, `.ts`, `.mts`, `.cts`, `.tsx`).

## Templates

`functions`, `classes` and `imports`, via `inspect --template`.

## Queries

- **Every function**: `[(function_declaration) @func (arrow_function) @func (method_definition) @func]`
- **Imports**: `(import_statement) @import`
- **Interface names** (TypeScript): `(interface_declaration name: (type_identifier) @interface)`
