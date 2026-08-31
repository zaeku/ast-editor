# API Specification - `ast-editor`

This document defines the interface, parameters, and return formats for the `ast-editor` MCP tool suite.

---

## 1. `create_lines`
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
      "description": "Absolute path to the file.",
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
  "content": "fn main() {\n    let x = 42;\n}\n",
  "filepath": "/path/to/project/create_default.rs",
  "return_ids": false
}
```

##### Output
```json
{
  "message": "File successfully created and line editing session initialized.",
  "status": "success",
  "total_bytes": 30,
  "total_lines": 3
}
```

#### Example with Line IDs (`return_ids = true`)
##### Input
```json
{
  "content": "fn main() {\n    let x = 42;\n}\n",
  "filepath": "/path/to/project/create_ids.rs",
  "return_ids": true
}
```

##### Output
```json
{
  "ids": [
    "1#77cf", "2#bcb4", "3#c2b7"
  ],
  "message": "File successfully created and line editing session initialized.",
  "status": "success",
  "total_bytes": 30,
  "total_lines": 3
}
```

---

## 2. `view_lines`
Retrieves a range of lines for any text file along with their persistent unique Line IDs.

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
      "description": "Absolute path to the target file",
      "type": "string"
    },
    "only_ids": {
      "description": "If true, only returns Line IDs and line numbers, omitting text content.",
      "type": "boolean"
    },
    "query": {
      "description": "Optional search term to filter lines matching this keyword.",
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
1: fn main() {
2:     let x = 42;
3: }
```

```json
{
  "enclosing_contexts": [],
  "ids": [["1#77cf",1],["2#bcb4",2],["3#c2b7",3]],
  "showing_end": 3,
  "showing_start": 1,
  "tip": "Edit these lines by calling 'edit_lines' with the line IDs (e.g. 1a#f8c9) shown above.",
  "total_bytes": 30,
  "total_lines": 3
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
  "enclosing_contexts": [],
  "ids": [["1#77cf",1],["2#bcb4",2],["3#c2b7",3]],
  "showing_end": 3,
  "showing_start": 1,
  "tip": "Edit these lines by calling 'edit_lines' with the line IDs (e.g. 1a#f8c9) shown above.",
  "total_bytes": 30,
  "total_lines": 3
}
```

---

## 3. `edit_lines`
Applies a transactional batch of operations to lines using their unique IDs.

### Parameters (JSON Schema)
```json
{
  "properties": {
    "edits": {
      "items": {
        "properties": {
          "content": {
            "description": "The new content to insert/update/replace. Omitted/ignored for delete, move.",
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
              "update",
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
            "description": "Optional target line ID (e.g. 1#a5c7). Required for update, delete, replace_range, move, replace_substring. Optional/omitted for insert_before (prepends) and insert_after (appends).",
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
      "description": "Absolute path to the file to modify",
      "type": "string"
    },
    "strict_validation": {
      "default": false,
      "description": "If true, rolls back edits on syntax or parser error. If false, saves changes anyway and returns warnings/errors.",
      "type": "boolean"
    }
  },
  "required": [
    "filepath",
    "edits"
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
      "op": "update",
      "target_id": "2#bcb4"
    },
    {
      "op": "delete",
      "target_id": "3#c2b7"
    }
  ],
  "filepath": "/path/to/project/create_ids.rs"
}
```

##### Output
```json
{
  "status": "success",
  "modified_ids": [
    "4#b7a3", "2#9639", "4#b7a3"
  ]
}
```
