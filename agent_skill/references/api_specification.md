# Parameters

Every tool's parameters, as the binary declares them. Each one can be passed on
the command line as `--kebab-case`, or the whole object can be passed at once
with `--json '{...}'`:

```bash
ast-editor view src/main.rs --start-line 1 --end-line 3
ast-editor view --json '{"filepath": "src/main.rs", "start_line": 1, "end_line": 3}'
```

`filepath` is the first positional argument, so it is rarely written out. A
path is relative to the working directory unless it is absolute.

A tool prints its answer to stdout as fenced blocks — the file's own language
for code, `diff` for a diff, `json` for the data. A failure prints to stderr and
exits non-zero.

## `outline`

Lists the definitions a file declares, each with its line range and the line IDs that edit it. The first look at an unfamiliar file.

```json
{
  "properties": {
    "filepath": {
      "description": "Path to the file, relative to the working directory or absolute",
      "type": "string"
    },
    "sexp": {
      "default": false,
      "description": "If true, returns the whole parse tree as s-expression text instead of the outline. For writing a query against a grammar whose node names are not yet known.",
      "type": "boolean"
    }
  },
  "required": [
    "filepath"
  ],
  "type": "object"
}
```

## `inspect`

Search a file's structure with a Tree-sitter query or a named template, and answer with each match's line range and the ids that edit it.

```json
{
  "properties": {
    "filepath": {
      "description": "Path to the file, relative to the working directory or absolute",
      "type": "string"
    },
    "include_code": {
      "description": "Whether to include the source code of the enclosing definition (default: true)",
      "type": "boolean"
    },
    "query": {
      "description": "Optional Tree-sitter S-expression query",
      "type": "string"
    },
    "template": {
      "description": "Predefined query template to run. Supported templates: functions, classes, imports (rust, python, go, javascript, typescript, tsx, java, c, cpp, swift); traits, impls (rust); interfaces, structs (go); macros (c, cpp); functions (bash); headings, headers, codeblocks, code_blocks, links, tables, lists (markdown).",
      "enum": [
        "functions",
        "classes",
        "imports",
        "headings",
        "headers",
        "codeblocks",
        "code_blocks",
        "links",
        "tables",
        "lists",
        "traits",
        "impls",
        "structs",
        "interfaces",
        "macros"
      ],
      "type": "string"
    }
  },
  "type": "object"
}
```

## `view`

Print a file's lines with the ids that edit them, or with only_ids the ids and line numbers alone.

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

### An example

Input:

```json
{
  "end_line": 3,
  "filepath": "/path/to/project/create_ids.rs",
  "only_ids": false,
  "start_line": 1
}
```

Output:

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

With `only_ids`:

```json
{
  "end_line": 3,
  "filepath": "/path/to/project/create_ids.rs",
  "only_ids": true,
  "start_line": 1
}
```

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

## `edit`

Apply edits transactionally to a file. Answers with the lines it changed, each as [id, line number], so a following edit needs no second read.

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

### An example

A batch that replaces a line, inserts after it, and deletes another:

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

```json
{
  "modified_lines": [
    ["2#9639",2], ["5#b7a3",3], ["4#c2b7",4]
  ]
}
```

Every id the batch minted comes back in `modified_lines`, each entry a line as
`[id, line number]`, so a following edit needs no second read.

### Previewing a batch

`dry_run` runs the same batch against a copy. The answer carries the unified
diff and the syntax result; the file and the line ids are untouched, and no
`modified_lines` come back because nothing was written.

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

The answer carries a short single-use `preview_id`. Pass it back as `apply` with
the same `filepath` to commit exactly that batch, without resending `edits`:

```json
{"filepath": "/path/to/file.rs", "apply": "p1f"}
```

An id is refused if it was already applied, if it is addressed at another file,
or if the file changed since the preview was taken — in that last case the diff
and the syntax result no longer describe the outcome, so preview again.

A refusal under `--strict` carries a `preview_id` too, so a batch the parser
disliked can be committed with `apply` when you judge the parser wrong.

## `create`

Write a new file and answer with its lines, each as [id, line number] when asked. Refuses to overwrite a file that exists, so an existing file is changed with edit.

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

### An example

```json
{
  "content": "fn main() {\n    let x = 42;\n    let scratch = 0;\n}\n",
  "filepath": "/path/to/project/create_ids.rs",
  "return_ids": true
}
```

```json
{
  "lines": [
    ["1#77cf",1], ["2#bcb4",2], ["3#8d90",3], ["4#c2b7",4]
  ],
  "total_bytes": 51,
  "total_lines": 4
}
```
