# Nix Language Reference - `ast-editor`

Queries for **Nix** (`.nix`). There are no templates; write the query.

## Queries

- **Attribute names**: `(binding attrpath: (attrpath (identifier) @attr))`
- **Let bindings**: `(let_expression (binding_set (binding attrpath: (attrpath (identifier) @name))))`
- **Function arguments**: `(formals (formal name: (identifier) @arg))`
