# API Specification - `ast-editor`

This document defines the interface, parameters, and return formats for the `ast-editor` tool suite.

---

## 1. `create`
Creates a brand-new file with the initial content and JIT-initializes its line editing session.

### Parameters (JSON Schema)
```json
{
  "properties": {
    "content": {
      "description": "Initial text content of the file.",
      "type": "string"
    },
    "filepath": {
      "description": "Path to the file to write, relative to the working directory or absolute",
      "type": "string"
    },
    "return_ids": {
      "default": false,
      "description": "If true, returns the flat array of generated Line IDs. Set to false to omit IDs and save tokens.",
      "type": "boolean"
    }
  },
  "required": [
    "filepath",
    "content"
  ],
  "type": "object"
}
```

### Usage Examples

#### Default Example (`return_ids = false`)
##### Input
```json
{
  "content": "fn main() {\n    let x = 42;\n    let scratch = 0;\n}\n",
  "filepath": "/path/to/project/create_default.rs",
  "return_ids": false
}
```

##### Output
```json
{
  "total_bytes": 51,
  "total_lines": 4
}
```

#### Example with Line IDs (`return_ids = true`)
##### Input
```json
{
  "content": "fn main() {\n    let x = 42;\n    let scratch = 0;\n}\n",
  "filepath": "/path/to/project/create_ids.rs",
  "return_ids": true
}
```

##### Output
```json
{
  "lines": [
    ["1#77cf",1], ["2#bcb4",2], ["3#8d90",3], ["4#c2b7",4]
  ],
  "total_bytes": 51,
  "total_lines": 4
}
```

---

## 2. `view`
Retrieves a range of lines for any text file along with their persistent unique Line IDs. Each line is printed as `<id>|<line>: <text>`; a line too long for one row is broken at a fixed character count onto `│:` and `└:` rows that join back to it exactly.

### Parameters (JSON Schema)
```json
{
  "properties": {
    "context_lines": {
      "description": "Optional number of surrounding context lines to return around query matches. Defaults to 5.",
      "type": "integer"
    },
    "end_line": {
      "description": "1-indexed ending line number (inclusive)",
      "type": "integer"
    },
    "filepath": {
      "description": "Path to the file, relative to the working directory or absolute",
      "type": "string"
    },
    "filepaths": {
      "description": "More files to read in the same call, answered with one block each. A path after the first on the command line lands here.",
      "type": "array"
    },
    "fixed_string": {
      "default": false,
      "description": "If true, 'query' is searched for literally rather than as a regular expression.",
      "type": "boolean"
    },
    "only_ids": {
      "description": "If true, answers with [id, line number] pairs instead of the lines themselves.",
      "type": "boolean"
    },
    "query": {
      "description": "Optional regular expression; only lines matching it are returned. Matched against the file's own lines, so it is unaffected by how the response is printed. Prefix with (?i) to ignore case.",
      "type": "string"
    },
    "start_line": {
      "description": "1-indexed starting line number (inclusive)",
      "type": "integer"
    }
  },
  "required": [
    "filepath"
  ],
  "type": "object"
}
```

### Usage Examples

#### Default Example (`only_ids = false`)
##### Input
```json
{
  "end_line": 3,
  "filepath": "/path/to/project/create_ids.rs",
  "only_ids": false,
  "start_line": 1
}
```

##### Output
```rust
1#77cf|1: fn main() {
2#bcb4|2:     let x = 42;
3#8d90|3:     let scratch = 0;
```

```json
{
  "enclosing_contexts": [{"end":4,"name":"fn:main","start":1}],
  "showing_end": 3,
  "showing_start": 1,
  "total_bytes": 51,
  "total_lines": 4
}
```

#### Example with IDs Only (`only_ids = true`)
##### Input
```json
{
  "end_line": 3,
  "filepath": "/path/to/project/create_ids.rs",
  "only_ids": true,
  "start_line": 1
}
```

##### Output
```json
{
  "enclosing_contexts": [{"end":4,"name":"fn:main","start":1}],
  "lines": [["1#77cf",1],["2#bcb4",2],["3#8d90",3]],
  "showing_end": 3,
  "showing_start": 1,
  "total_bytes": 51,
  "total_lines": 4
}
```

---

## 3. `edit`
Applies a transactional batch of operations to lines using their unique IDs.

### Parameters (JSON Schema)
```json
{
  "properties": {
    "apply": {
      "description": "A preview_id from an earlier dry_run, e.g. 'p1f'. Applies the batch that preview validated and returns its modified_lines. Supply 'filepath' with it; 'edits' is not needed and is ignored. A preview id is single-use, and is refused once the file has changed under it.",
      "type": "string"
    },
    "dry_run": {
      "default": false,
      "description": "If true, returns the unified diff and syntax validation result the edits would produce, without writing to disk or assigning line IDs. When the result is syntactically valid the response also carries a preview_id; pass it back as 'apply' to commit that exact batch without resending it.",
      "type": "boolean"
    },
    "edits": {
      "items": {
        "properties": {
          "content": {
            "description": "The new content to insert or replace with, as line-terminated text: \"\" is no lines at all, \"\\n\" is one empty line, and a trailing newline ends the last line rather than starting another. Omitted/ignored for delete, move.",
            "type": "string"
          },
          "dest_target_id": {
            "description": "Optional destination target line ID (e.g. 10#e9c4). Required for move operations with 'before' or 'after' move_position.",
            "type": "string"
          },
          "end_target_id": {
            "description": "Optional ending target line ID for block range (e.g. 5#7f1c). Required for replace_range, optional for move.",
            "type": "string"
          },
          "move_position": {
            "description": "Optional relative position for move operations ('before', 'after', 'prepend', 'append').",
            "enum": [
              "before",
              "after",
              "prepend",
              "append"
            ],
            "type": "string"
          },
          "occurrence": {
            "description": "Optional 1-indexed occurrence count of the pattern (default: 1) for replace_substring.",
            "type": "integer"
          },
          "op": {
            "description": "The edit operation to perform.",
            "enum": [
              "replace",
              "insert_after",
              "insert_before",
              "delete",
              "replace_range",
              "move",
              "replace_substring"
            ],
            "type": "string"
          },
          "pattern": {
            "description": "The substring pattern to find. Required for replace_substring.",
            "type": "string"
          },
          "replacement": {
            "description": "The replacement string. Required for replace_substring.",
            "type": "string"
          },
          "target_id": {
            "description": "Optional target line ID (e.g. 1#a5c7). Required for replace, delete, replace_range, move, replace_substring. Optional/omitted for insert_before (prepends) and insert_after (appends).",
            "type": "string"
          }
        },
        "required": [
          "op"
        ],
        "type": "object"
      },
      "type": "array"
    },
    "filepath": {
      "description": "Path to the file, relative to the working directory or absolute",
      "type": "string"
    },
    "strict_validation": {
      "default": false,
      "description": "If true, rolls back edits on syntax or parser error. If false, saves changes anyway and returns warnings/errors.",
      "type": "boolean"
    }
  },
  "required": [
    "filepath"
  ],
  "type": "object"
}
```

### Usage Examples

#### Compact Example
##### Input
```json
{
  "edits": [
    {
      "content": "    let y = 200;",
      "op": "insert_after",
      "target_id": "2#bcb4"
    },
    {
      "content": "    let x = 100;",
      "op": "replace",
      "target_id": "2#bcb4"
    },
    {
      "op": "delete",
      "target_id": "3#8d90"
    }
  ],
  "filepath": "/path/to/project/create_ids.rs"
}
```

##### Output
```json
{
  "modified_lines": [
    ["2#9639",2], ["5#b7a3",3], ["4#c2b7",4]
  ]
}
```

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
```json
{
  "dry_run": true,
  "edits": [
    {
      "content": "    let y = 200;",
      "op": "insert_after",
      "target_id": "2#bcb4"
    },
    {
      "content": "    let x = 100;",
      "op": "replace",
      "target_id": "2#bcb4"
    },
    {
      "op": "delete",
      "target_id": "3#8d90"
    }
  ],
  "filepath": "/path/to/project/create_ids.rs"
}
```

##### Output
```diff
--- /path/to/project/create_ids.rs
+++ /path/to/project/create_ids.rs
@@ -1,4 +1,4 @@
 fn main() {
-    let x = 42;
-    let scratch = 0;
+    let x = 100;
+    let y = 200;
 }
```
```json
{
  "preview_id": "p1f",
  "syntax_valid": true
}
```
