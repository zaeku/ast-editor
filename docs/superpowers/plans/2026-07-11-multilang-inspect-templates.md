# Implementation Plan: inspect_ast Multi-Language & Specialized Template Expansion

**Goal:**
1. Support standard templates (`functions`, `classes`, `imports`) across all core programming, config, and script languages.
2. Implement language-specific specialized templates to extract distinct syntax structures:
   - **Rust**: `traits`, `impls`
   - **Go**: `structs`, `interfaces`
   - **C / C++**: `macros`
3. Update configuration schema in `mod.rs` and document everything in `SKILL.md` and `README.md`.

---

## Proposed Changes

### 1. Template Resolution Expansion
#### [MODIFY] [inspect.rs](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/src/tools/inspect.rs)
- Expand the template matching logic inside `run_inspect` to resolve:
  - **Go (`go`)**:
    - `functions` ➡️ `[(function_declaration) (method_declaration)] @function`
    - `classes` ➡️ `(type_declaration) @class`
    - `imports` ➡️ `(import_declaration) @import`
    - `interfaces` ➡️ `(type_declaration (type_spec type: (interface_type))) @interface`
    - `structs` ➡️ `(type_declaration (type_spec type: (struct_type))) @struct`
  - **JavaScript / TypeScript / TSX (`javascript`, `typescript`, `tsx`)**:
    - `functions` ➡️ `[(function_declaration) (arrow_function) (method_definition)] @function`
    - `classes` ➡️ `(class_declaration) @class`
    - `imports` ➡️ `(import_statement) @import`
  - **Java (`java`)**:
    - `functions` ➡️ `(method_declaration) @function`
    - `classes` ➡️ `[(class_declaration) (interface_declaration)] @class`
    - `imports` ➡️ `(import_declaration) @import`
  - **C / C++ (`c`, `cpp`)**:
    - `functions` ➡️ `(function_definition) @function`
    - `classes` ➡️ `[(struct_specifier) (class_specifier)] @class`
    - `imports` ➡️ `(preproc_include) @import`
    - `macros` ➡️ `[(preproc_def) (preproc_function_def)] @macro`
  - **Bash / Shell (`bash`)**:
    - `functions` ➡️ `(function_definition) @function`
  - **Rust (`rust`)**:
    - Keep existing (`functions`, `classes`, `imports`) and add:
      - `traits` ➡️ `(trait_item) @trait`
      - `impls` ➡️ `(impl_item) @impl`

### 2. Schema Update
#### [MODIFY] [mod.rs](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/src/tools/mod.rs)
- Update the `inspect_ast` tool schema input properties description for the `template` parameter to include the new standard languages and specialized template options (`traits`, `impls`, `structs`, `interfaces`, `macros`).

### 3. Documentation
#### [MODIFY] [SKILL.md](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/SKILL.md)
- Update `SKILL.md` to document the new templates.
#### [MODIFY] [README.tpl.md](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/README.tpl.md)
- Document the new templates and their mappings.

---

## Verification Plan

### Automated Tests
- Create unit tests in `src/tools/inspect.rs` verifying that the new templates match correctly for their respective languages.
- Run `cargo test` to execute all tests.
- Re-run `cargo test --test readme_generator` to rebuild `README.md`.
- Compile final release binary `cargo build --release`.
