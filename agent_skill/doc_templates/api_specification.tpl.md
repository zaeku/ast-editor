# API Specification - `ast-editor`

This document defines the interface, parameters, and return formats for the `ast-editor` MCP tool suite.

---

## 1. `create_lines`
Creates a brand-new file with the initial content and JIT-initializes its line editing session.

### Parameters (JSON Schema)
{{create_lines_schema}}

### Usage Examples

#### Default Example (`return_ids = false`)
##### Input
{{create_lines_input_default}}

##### Output
{{create_lines_output_default}}

#### Example with Line IDs (`return_ids = true`)
##### Input
{{create_lines_input_ids}}

##### Output
{{create_lines_output_ids}}

---

## 2. `view_lines`
Retrieves a range of lines for any text file along with their persistent unique Line IDs.

### Parameters (JSON Schema)
{{view_lines_schema}}

### Usage Examples

#### Default Example (`only_ids = false`)
##### Input
{{view_lines_input_default}}

##### Output
{{view_lines_output_default}}

#### Example with IDs Only (`only_ids = true`)
##### Input
{{view_lines_input_only_ids}}

##### Output
{{view_lines_output_only_ids}}

---

## 3. `edit_lines`
Applies a transactional batch of operations to lines using their unique IDs.

### Parameters (JSON Schema)
{{edit_lines_schema}}

### Usage Examples

#### Compact Example
##### Input
{{edit_lines_input_compact}}

##### Output
{{edit_lines_output_compact}}
