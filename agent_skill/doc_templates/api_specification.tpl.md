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

{{outline_description}}

{{outline_parameters}}

## `inspect`

{{inspect_description}}

{{inspect_parameters}}

## `view`

{{view_description}}

{{view_parameters}}

### An example

Input:

{{view_input_default}}

Output:

{{view_output_default}}

With `only_ids`:

{{view_input_only_ids}}

{{view_output_only_ids}}

## `edit`

{{edit_description}}

{{edit_parameters}}

### An example

A batch that replaces a span of two lines with one, and inserts after another:

{{edit_input_compact}}

{{edit_output_compact}}

Every id the batch minted comes back in `modified_lines`, each entry a line as
`[id, line number]`, so a following edit needs no second read.

A batch that changes how many lines a file has moves every line after it, and
`renumbered` names those runs: `{"range": [first_id, last_id], "line_numbers":
[first, last]}` for each. Lines inside a run stay contiguous and in order, so
its two ends give the
number of every line between them and nothing in the middle is listed. A
`delete` writes no line, so it answers with an empty `modified_lines` and the
run that moved up into the hole; a `replace` given no lines answers the same
way, because it is the same edit.

### Previewing a batch

`dry_run` runs the same batch against a copy. The answer carries the unified
diff and the syntax result; the file and the line ids are untouched, and no
`modified_lines` come back because nothing was written.

{{edit_input_dry_run}}

{{edit_output_dry_run}}

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

{{create_description}}

{{create_parameters}}

### An example

{{create_input_ids}}

{{create_output_ids}}
