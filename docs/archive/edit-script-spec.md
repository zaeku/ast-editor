# Spec: The `edit` Script Format (`ast-editor`)

> Delivered. Kept as the record of what was decided and why. Section 3 is still
> the grammar the parser implements, and section 5 is the test suite in
> `tests/edit_script_tests.rs`.

A line-oriented format for `edit_lines`, read from stdin, so that an edit batch
can be written in a shell heredoc with no escaping.

## 1. Why

`edit_lines` is reached today by passing a JSON object as an argv string, which
puts arbitrary code through two escaping layers — JSON, then the shell:

```
source :     let msg = format!("can't parse {:?}: {}", path, err);
as passed: \"    let msg = format!(\\"can't parse {:?}: {}\\", path, err);\"
```

Code contains quotes, braces and backslashes, so this is the normal case rather
than an awkward one. The tax is paid on every edit, it is a recurring source of
malformed calls, and it is the reason a caller who knows the tool still reaches
for a `python3 - <<'PY'` heredoc instead.

That the format is a transport and nothing more — no operation the JSON schema
lacks, a line named only by its id, and `replace_substring` left where it lives
— is `D-01M27VV9X5P9J9` in the decision layer, with a fence, since 2026-09-11.
What follows is the grammar that carries it.

## 2. Invocation

```bash
ast-editor edit <filepath> [--dry-run] [--strict] < script
```

The script is read from stdin. In a shell that means a quoted heredoc, whose
delimiter suppresses every expansion:

```bash
ast-editor edit src/config.rs <<'EOF'
replace_range 3f#a1b2 4c#d3e4 ```
    let a = 1;
    let b = 2;
```
replace 5a#7788 ```
    let c = 3;
```
delete 6b#99aa
EOF
```

The heredoc delimiter is consumed by the shell and never reaches the tool, so
the format needs no terminator of its own: end of input closes the last block.

`--dry-run` and `--strict` mean what `dry_run` and `strict_validation` mean in
the JSON form. A dry run prints the same preview, `preview_id` included, so the
batch can be applied afterwards by id without being sent again.

## 3. Grammar

A script is a sequence of **directives**. A directive is one line:

```
<op> <arg>... [<fence>]
```

- `<op>` and each `<arg>` are whitespace-separated words. Every argument is a
  line id or a keyword, so none of them can contain whitespace and none needs
  quoting.
- `<fence>` is three or more backticks at the end of the line. Its presence
  means the directive takes a payload.

A directive with a fence is followed by its **payload**: every subsequent line,
verbatim, until a line whose only content is backticks, at least as many as the
opening fence. Leading and trailing whitespace on that closing line is ignored;
nothing else on it is allowed.

A directive with no fence takes no payload, and the next line is the next
directive.

Blank lines and lines beginning with `#` are ignored **between** directives.
Inside a payload they are content like any other line.

### Fence length

The closing fence must be at least as long as the opening one, so content that
itself contains a fence is written inside a longer one:

````
replace 5a#7788 ````
```bash
ast-editor --version
```
````
````

This is markdown's own rule. It matters because a line of exactly three
backticks is common — 73 of them in this repository, in Markdown and inside
Rust source — while the format needs a delimiter that content cannot forge.
Choosing the fence length is the author's way of saying how deep the content
goes, and it removes the collision by construction rather than by a flag.

### Operations

| Directive | Payload |
|---|---|
| `replace <id>` | the new lines |
| `replace_range <start_id> <end_id>` | the new lines |
| `insert_after <id>` | the new lines |
| `insert_before <id>` | the new lines |
| `append` | the new lines |
| `prepend` | the new lines |
| `delete <id>` | none |
| `move <start_id> [<end_id>] <before\|after> <dest_id>` | none |
| `move <start_id> [<end_id>] <prepend\|append>` | none |

A payload of zero lines is allowed and means empty content, which is how
`replace_range` deletes a span.

The batch is applied exactly as the JSON form applies it: in order, as one
transaction, with one validation pass at the end.

One consequence is worth stating, because a script makes it easy to write: a
line id carries a hash of that line's content, so a directive cannot target a
line an earlier directive in the same batch changed. The id captured before the
batch no longer describes the line, and the edit is refused rather than applied
to whatever is there now. Target distinct lines, or split the batch.

## 4. Failing

Every one of these fails the whole batch, writes nothing, and names the line
number in the script:

- An unknown operation, or an operation given the wrong number of arguments.
- A fence that is never closed. The remainder of the input is **not** taken as
  the payload; a script that forgot a closing fence is a script whose intent is
  unknown.
- A payload on an operation that takes none, or a missing payload on one that
  requires it.
- `replace_substring`, which is named only to say where it lives instead.

Errors carry the script line number, because a heredoc has no other landmark.

## 5. Verification Plan

1. Each operation applies exactly as its JSON equivalent does, checked by
   running both forms over the same file and comparing the result.
2. Content containing a line of three backticks survives inside a four-backtick
   fence, unchanged.
3. An unclosed fence fails, names the operation and the line, and leaves the
   file untouched.
4. An unknown operation fails and names the line.
5. A payload whose lines begin with `#` or are blank is preserved verbatim.
6. `--dry-run` returns a `preview_id` that the JSON form's `apply` then commits,
   showing the two forms share one engine.
7. A batch of several directives is one transaction: if the result fails
   validation under `--strict`, none of it is written.
8. A directive targeting a line an earlier directive changed is refused, and
   the file is left alone.
