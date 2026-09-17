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

| parameter | type | required | description |
|---|---|---|---|
| `filepath` | `string` | yes | Path to the file, relative to the working directory or absolute |
| `sexp` | `boolean` |  | If true, returns the whole parse tree as s-expression text instead of the outline. For writing a query against a grammar whose node names are not yet known. |

## `inspect`

Search a file's structure with a Tree-sitter query or a named template, and answer with each match's line range and the ids that edit it.

| parameter | type | required | description |
|---|---|---|---|
| `filepath` | `string` |  | Path to the file, relative to the working directory or absolute |
| `include_code` | `boolean` |  | Whether to include the source code of the enclosing definition (default: true) |
| `query` | `string` |  | Optional Tree-sitter S-expression query |
| `template` | `functions` \| `classes` \| `imports` \| `headings` \| `headers` \| `codeblocks` \| `code_blocks` \| `links` \| `tables` \| `lists` \| `traits` \| `impls` \| `structs` \| `interfaces` \| `macros` |  | Predefined query template to run. Supported templates: functions, classes, imports (rust, python, go, javascript, typescript, tsx, java, c, cpp, swift); traits, impls (rust); interfaces, structs (go); macros (c, cpp); functions (bash); headings, headers, codeblocks, code_blocks, links, tables, lists (markdown). |

## `view`

Print a file's lines with the ids that edit them, or with only_ids the ids and line numbers alone.

| parameter | type | required | description |
|---|---|---|---|
| `context_lines` | `integer` |  | Optional number of surrounding context lines to return around query matches. Defaults to 5. |
| `end_line` | `integer` |  | 1-indexed ending line number (inclusive) |
| `filepath` | `string` | yes | Path to the file, relative to the working directory or absolute |
| `filepaths` | `array` |  | More files to read in the same call, answered with one block each. A path after the first on the command line lands here. |
| `fixed_string` | `boolean` |  | If true, 'query' is searched for literally rather than as a regular expression. |
| `only_ids` | `boolean` |  | If true, answers with [id, line number] pairs instead of the lines themselves. |
| `query` | `string` |  | Optional regular expression; only lines matching it are returned. Matched against the file's own lines, so it is unaffected by how the response is printed. Prefix with (?i) to ignore case. |
| `start_line` | `integer` |  | 1-indexed starting line number (inclusive) |

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

Apply edits transactionally to a file. Answers with the lines it wrote, each as [id, line number], and with the runs of lines it left at a new number, so a following edit needs no second read.

| parameter | type | required | description |
|---|---|---|---|
| `apply` | `string` |  | A preview_id from an earlier dry_run, e.g. 'p1f'. Applies the batch that preview validated and returns its modified_lines. Supply 'filepath' with it; 'edits' is not needed and is ignored. A preview id is single-use, and is refused once the file has changed under it. |
| `dry_run` | `boolean` |  | If true, returns the unified diff and the syntax result the edits would produce, without writing to disk or assigning line IDs. The response carries a preview_id whatever the verdict; pass it back as 'apply' to commit that exact batch without resending it. |
| `edits` | `array` |  |  |
| `filepath` | `string` | yes | Path to the file, relative to the working directory or absolute |

Each entry of `edits`:

| field | type | description |
|---|---|---|
| `content` | `string` | The new content to insert or replace with, as line-terminated text: "" is no lines at all, "\n" is one empty line, and a trailing newline ends the last line rather than starting another. Omitted/ignored for delete, move. |
| `dest_id` | `string` | Optional destination target line ID (e.g. 10#e9c4). Required for move operations with 'before' or 'after' move_position. |
| `end_id` | `string` | The last line of the span, where the op acts on more than one (e.g. 5#7f1c). Omitted addresses the start line alone. Taken by replace, delete and move. |
| `move_position` | `before` \| `after` \| `prepend` \| `append` | Optional relative position for move operations ('before', 'after', 'prepend', 'append'). |
| `occurrence` | `integer` | Optional 1-indexed occurrence count of the pattern (default: 1) for replace_substring. |
| `op` | `replace` \| `insert_after` \| `insert_before` \| `delete` \| `move` \| `replace_substring` | The edit operation to perform. |
| `pattern` | `string` | The substring pattern to find. Required for replace_substring. |
| `replacement` | `string` | The replacement string. Required for replace_substring. |
| `start_id` | `string` | The first line the op acts on (e.g. 1#a5c7). Required for replace, delete, move and replace_substring. Omitted for insert_before (prepends) and insert_after (appends). |

### An example

A batch that replaces a span of two lines with one, and inserts after another:

```json
{
  "edits": [
    {
      "content": "    let y = 200;",
      "op": "insert_after",
      "start_id": "1#77cf"
    },
    {
      "content": "    let x = 100;",
      "end_id": "3#8d90",
      "op": "replace",
      "start_id": "2#bcb4"
    }
  ],
  "filepath": "/path/to/project/create_ids.rs"
}
```

```json
{
  "modified_lines": [
    ["5#b7a3",2], ["2#9639",3]
  ],
  "renumbered": [
    {"from": ["2#9639", 3], "to": ["2#9639", 3]}
  ]
}
```

Every id the batch minted comes back in `modified_lines`, each entry a line as
`[id, line number]`, so a following edit needs no second read.

A batch that changes how many lines a file has moves every line after it, and
`renumbered` names those runs: `{"from": [id, line], "to": [id, line]}` for
each. Lines inside a run stay contiguous and in order, so its two ends give the
number of every line between them and nothing in the middle is listed. A
`delete` writes no line, so it answers with an empty `modified_lines` and the
run that moved up into the hole; a `replace` given no lines answers the same
way, because it is the same edit.

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
      "start_id": "1#77cf"
    },
    {
      "content": "    let x = 100;",
      "end_id": "3#8d90",
      "op": "replace",
      "start_id": "2#bcb4"
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
+    let y = 200;
+    let x = 100;
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

A refusal carries a `preview_id` too, so a batch the parser disliked can be
committed with `apply` when you judge the parser wrong.

## `create`

Write a new file and answer with its lines, each as [id, line number] when asked. Refuses to overwrite a file that exists, so an existing file is changed with edit.

| parameter | type | required | description |
|---|---|---|---|
| `content` | `string` | yes | Initial text content of the file. |
| `filepath` | `string` | yes | Path to the file to write, relative to the working directory or absolute |
| `return_ids` | `boolean` |  | If true, answers with the new file's lines, each as [id, line number]. Set to false to omit them and save tokens. |

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
