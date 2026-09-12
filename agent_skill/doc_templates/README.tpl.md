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

Under `{{prefix}}` by default:

| | |
|---|---|
| `{{bin_dir}}/ast-editor` | the binary |
| `{{skill_dir}}/SKILL.md` | the skill document |

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

{{view_description}}

Each line is printed as `<id>|<line>: <text>`; a line too long for one row is
broken at a fixed character count onto `│:` and `└:` rows that join back to it
exactly. A range can follow the path the way `sed -n '40,80p'` takes one.

* A range is capped at {{line_cap}} lines per call.
* Returned text is capped at {{response_cap}} bytes.
* A line over {{segment_length}} characters is truncated in the view and its id
  is suffixed `#TRUNC`. An edit to such a line is refused, to prevent the
  truncation being written back; run a formatter over the file first.

#### Input Schema
{{view_schema}}

#### Usage Examples

##### Default Example (`only_ids = false`)
###### Input
{{view_input_default}}

###### Output
{{view_output_default}}

##### Example with IDs Only (`only_ids = true`)
###### Input
{{view_input_only_ids}}

###### Output
{{view_output_only_ids}}

### `outline`

{{outline_description}}

With `sexp` it returns the AST as S-expression text instead, up to a fixed
depth, which is what a custom `inspect` query is written against.

### `inspect`

{{inspect_description}}

A matched definition's code comes back as its own block, printed as `view`
prints lines; `include_code: false` leaves it out. A query that does not compile
is an error rather than zero matches.

`ast-editor inspect --help` lists the templates each language has.

### `edit`

{{edit_description}}

* `replace`, `replace_range`, `replace_substring`, `insert_before`,
  `insert_after`, `delete`, `move`.
* The answer is `modified_lines`, each entry a line as `[id, line number]`.
* Content is line-terminated text: an empty payload is no lines, so a `replace`
  with one deletes the line.

#### Input Schema
{{edit_schema}}

#### Usage Examples

##### Compact Example
###### Input
{{edit_input_compact}}

###### Output
{{edit_output_compact}}

##### Dry Run
###### Input
{{edit_input_dry_run}}

###### Output
{{edit_output_dry_run}}

### `create`

{{create_description}}

The refusal is `FILE_ALREADY_EXISTS`, and it is deliberate: rewriting a whole
file to change part of it is how a file gets truncated when something goes
wrong halfway.

#### Input Schema
{{create_schema}}

#### Usage Examples

##### Default Example (`return_ids = false`)
###### Input
{{create_input_default}}

###### Output
{{create_output_default}}

##### Example with Line IDs (`return_ids = true`)
###### Input
{{create_input_ids}}

###### Output
{{create_output_ids}}

## Languages

Each grammar is a dependency compiled into the binary at the version
`Cargo.lock` pins, and `ast-editor --version` reports them with their versions.

{{languages}}

A file whose extension is not listed is read, edited and written without a
syntax check.

## Building

Rust {{msrv}} or newer.

```bash
just build       # or: cargo build --release
just test
```

The binary is at `target/release/ast-editor`.
