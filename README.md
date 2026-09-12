# ast-editor

Line-precise editing over tree-sitter. `ast-editor` reads a file, hands back an
id for every line, and applies a batch of edits addressed by those ids — so a
change is made without reproducing the text around it and without the line
numbers moving underneath the next edit.

It is a command, not a service. One call does one thing and leaves nothing
open.

## The id

A line id is `<number>#<hash>`. The number names the line for as long as it
lives, and the hash guards its content. An edit mints ids for the lines it
writes and leaves every other id alone:

```
before   1#fe05  2#ad78  3#b802  4#9f8f
                 insert_after 2#ad78
after    1#fe05  2#ad78  6#7722  3#b802  4#9f8f
```

So an id read before an edit still names its line afterwards, and re-reading a
file to find out where everything moved is work that does not need doing. An id
whose line changed underneath is refused rather than applied to whatever sits
there now, and the refusal says which line to read again.

The ids are kept in a store under the user's cache directory. It holds no copy
of the file: disk is the truth, and losing the store costs the ids of one file
and nothing else.

## Install

```bash
just install     # the binary and the skill
just --list      # the other tasks
```

Under `~/.agents` by default:

| | |
|---|---|
| `bin/ast-editor` | the binary |
| `skills/ast-editor/SKILL.md` | the skill document |

The skill documents are embedded in the binary, so what `install-skill` writes
is what that binary carries. Only the hub is installed: it reaches its
references by naming `ast-editor skill <topic>` rather than a path, which works
in a terminal and in an installed tree alike. `ast-editor --help` lists the
topics.

`AST_EDITOR_PREFIX` chooses a different prefix, and `install-bin` /
`install-skill` install one half. The grammars are compiled into the binary, so
an install is those two files and nothing needs to be exported for a parse to
work. `<prefix>/bin` does need to be on `PATH`, since the skill document calls
the command by name.

## Calling it

One tool, one call. A tool is named by any unambiguous prefix, so `ins` is
`inspect`:

```bash
ast-editor view src/main.rs 40,80
ast-editor inspect src/main.rs --template functions
ast-editor edit src/main.rs --apply p1f
ast-editor --version
ast-editor --help
```

Options are the tool's own parameters, derived from its schema, so anything
`ast-editor skill api` lists can be passed as `--kebab-case`. Paths are relative
to the working directory, and several of them can be given at once. A shape no
option can carry goes in as `--json '{...}'`.

Editing has a second form that takes a script on stdin, so code needs no
escaping at all — `ast-editor skill usage` has the worked examples:

```bash
ast-editor edit src/config.rs <<'EOF'
replace 2#0759 ```
    let msg = format!("can't parse {:?}: {}", path, err);
```
delete 7c#aabb
EOF
```

`view` takes a `query` and `inspect` matches carry `start_id` / `end_id`, so
finding a line by content or by structure already yields the ids that edit it.

A tool prints what it has to say on stdout as fenced blocks — the file's own
language for code, `diff` for a diff, `json` for the data — so code arrives
unescaped and the data still pipes:

````bash
ast-editor edit src/main.rs --dry-run < edits.txt |
  awk '/^```json$/{f=1;next} /^```/{f=0} f' | jq -r .preview_id
````

Failures print to stderr and exit non-zero, so a response that arrived is a
response about work that happened.

## What a syntax check does

An edit is parsed after it is applied, and the verdict is reported rather than
enforced: the answer carries `syntax_valid` and the diagnostics, and the file is
written. A parser is not always right about valid code, and an edit that a
grammar dislikes is often one a person meant.

`--strict` inverts that. The edit is refused, the file is left alone, and the
refusal carries a `preview_id` — so `--apply <preview_id>` commits the batch
unchanged where you judge the parser wrong. A dry run answers the same way and
never writes.

`syntax_valid: null` means no grammar covers the file type and the result was
written without a check.

## The tools

### `view`

Prints lines with their ids. Each line is printed as `<id>|<line>: <text>`; a
line too long for one row is broken at a fixed character count onto `│:` and
`└:` rows that join back to it exactly. A range can follow the path the way
`sed -n '40,80p'` takes one.

* A range is capped at 800 lines per call.
* Returned text is capped at 45,000 bytes.
* A line over 2048 characters is truncated in the view and its id is suffixed
  `#TRUNC`. An edit to such a line is refused, to prevent the truncation being
  written back; run a formatter over the file first.

#### Input Schema
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

#### Usage Examples

##### Default Example (`only_ids = false`)
###### Input
```json
{
  "end_line": 3,
  "filepath": "/path/to/project/create_ids.rs",
  "only_ids": false,
  "start_line": 1
}
```

###### Output
```rust
1#77cf|1: fn main() {
2#bcb4|2:     let x = 42;
3#c2b7|3: }
```

```json
{
  "enclosing_contexts": [],
  "showing_end": 3,
  "showing_start": 1,
  "total_bytes": 30,
  "total_lines": 3
}
```

##### Example with IDs Only (`only_ids = true`)
###### Input
```json
{
  "end_line": 3,
  "filepath": "/path/to/project/create_ids.rs",
  "only_ids": true,
  "start_line": 1
}
```

###### Output
```json
{
  "enclosing_contexts": [],
  "lines": [["1#77cf",1],["2#bcb4",2],["3#c2b7",3]],
  "showing_end": 3,
  "showing_start": 1,
  "total_bytes": 30,
  "total_lines": 3
}
```

### `outline`

Lists the definitions a file declares — signature, line range, and the ids that
edit them — which is the first look at an unfamiliar file. With `sexp` it
returns the AST as S-expression text instead, up to a fixed depth, which is what
a custom `inspect` query is written against.

### `inspect`

Searches a file's structure with a tree-sitter query or a named template, and
answers with each match's line range and the ids that edit it. A matched
definition's code comes back as its own block, printed as `view` prints lines;
`include_code: false` leaves it out. A query that does not compile is an error
rather than zero matches.

`ast-editor inspect --help` lists the templates each language has.

### `edit`

Applies a batch of operations to lines named by their ids, as one transaction.

* `replace`, `replace_range`, `replace_substring`, `insert_before`,
  `insert_after`, `delete`, `move`.
* The answer is `modified_lines`, each entry a line as `[id, line number]`.
* Content is line-terminated text: an empty payload is no lines, so a `replace`
  with one deletes the line.

#### Input Schema
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
    "filepath"
  ],
  "type": "object"
}
```

#### Usage Examples

##### Compact Example
###### Input
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
      "target_id": "3#c2b7"
    }
  ],
  "filepath": "/path/to/project/create_ids.rs"
}
```

###### Output
```json
{
  "modified_lines": [
    ["2#9639",2], ["4#b7a3",3]
  ],
  "syntax_valid": null,
  "message": "No grammar covers this file type, so the result was written without a syntax check."
}
```

##### Dry Run
###### Input
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
      "target_id": "3#c2b7"
    }
  ],
  "filepath": "/path/to/project/create_ids.rs"
}
```

###### Output
```json
```diff
--- /path/to/project/create_ids.rs
+++ /path/to/project/create_ids.rs
@@ -1,3 +1,3 @@
 fn main() {
-    let x = 42;
-}
+    let x = 100;
+    let y = 200;
```
```json
{
  "message": "No grammar covers this file type, so the result was written without a syntax check.",
  "preview_id": "p1f",
  "syntax_valid": null
}
```
```

### `create`

Writes a new file and returns its lines with their ids in one step. It refuses
to overwrite an existing file (`FILE_ALREADY_EXISTS`), so the way to change a
file that is already there is `edit`, which replaces the lines it was given and
leaves the rest of the file alone.

#### Input Schema
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

#### Usage Examples

##### Default Example (`return_ids = false`)
###### Input
```json
{
  "content": "fn main() {\n    let x = 42;\n}\n",
  "filepath": "/path/to/project/create_default.rs",
  "return_ids": false
}
```

###### Output
```json
{
  "total_bytes": 30,
  "total_lines": 3
}
```

##### Example with Line IDs (`return_ids = true`)
###### Input
```json
{
  "content": "fn main() {\n    let x = 42;\n}\n",
  "filepath": "/path/to/project/create_ids.rs",
  "return_ids": true
}
```

###### Output
```json
{
  "lines": [
    ["1#77cf",1], ["2#bcb4",2], ["3#c2b7",3]
  ],
  "total_bytes": 30,
  "total_lines": 3
}
```

## Languages

Each grammar is a dependency compiled into the binary at the version
`Cargo.lock` pins, and `ast-editor --version` reports them with their versions.

| Language | Extensions |
|---|---|
| bash | `.sh`, `.bash`, `.zsh`, `.ksh` |
| c | `.c`, `.h` |
| cpp | `.cpp`, `.cc`, `.cxx`, `.hpp` |
| go | `.go` |
| html | `.html`, `.htm` |
| java | `.java` |
| javascript | `.js`, `.jsx`, `.mjs`, `.cjs` |
| json | `.json` |
| lua | `.lua` |
| markdown | `.md`, `.markdown` |
| nix | `.nix` |
| python | `.py` |
| rust | `.rs` |
| swift | `.swift` |
| toml | `.toml` |
| tsx | `.tsx` |
| typescript | `.ts`, `.mts`, `.cts` |
| yaml | `.yaml`, `.yml` |

A file whose extension is not listed is read, edited and written without a
syntax check.

## Building

Rust 1.88 or newer.

```bash
just build       # or: cargo build --release
just test
```

The binary is at `target/release/ast-editor`.
