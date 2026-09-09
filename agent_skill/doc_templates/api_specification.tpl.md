# API Specification - `ast-editor`

This document defines the interface, parameters, and return formats for the `ast-editor` tool suite.

---

## 1. `create`
Creates a brand-new file with the initial content and JIT-initializes its line editing session.

### Parameters (JSON Schema)
{{create_schema}}

### Usage Examples

#### Default Example (`return_ids = false`)
##### Input
{{create_input_default}}

##### Output
{{create_output_default}}

#### Example with Line IDs (`return_ids = true`)
##### Input
{{create_input_ids}}

##### Output
{{create_output_ids}}

---

## 2. `view`
Retrieves a range of lines for any text file along with their persistent unique Line IDs.

### Parameters (JSON Schema)
{{view_schema}}

### Usage Examples

#### Default Example (`only_ids = false`)
##### Input
{{view_input_default}}

##### Output
{{view_output_default}}

#### Example with IDs Only (`only_ids = true`)
##### Input
{{view_input_only_ids}}

##### Output
{{view_output_only_ids}}

---

## 3. `edit`
Applies a transactional batch of operations to lines using their unique IDs.

### Parameters (JSON Schema)
{{edit_schema}}

### Usage Examples

#### Compact Example
##### Input
{{edit_input_compact}}

##### Output
{{edit_output_compact}}

#### Dry-Run Example (`dry_run = true`)
Previews the same batch. The response carries the unified diff and the syntax
validation result; the file and the line IDs are left untouched, and no
`modified_ids` are returned.

A batch that validates also returns a short single-use `preview_id`. Pass it
back as `apply` with the same `filepath` to commit exactly that batch and
receive the new IDs, without resending `edits`:

```json
{"filepath": "/path/to/file.rs", "apply": "p1f"}
```

The id is refused if it was already applied, if it is addressed at another
file, or if the file changed since the preview was taken — in that last case
the diff and syntax result no longer describe the outcome, so preview again.

##### Input
{{edit_input_dry_run}}

##### Output
{{edit_output_dry_run}}
