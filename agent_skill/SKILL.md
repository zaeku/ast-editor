---
name: ast-editor
description: >
  Inspects code structure, finds target lines, and performs transactionally-validated line-level code edits using Tree-sitter. Resilient to syntax errors. Invoked as the `ast-editor` command.
---

# AST-based Code Editor and Inspector Skill

A **general-purpose line-level editing framework** for any text file (Markdown,
plain text, configuration), which **additionally** provides Abstract Syntax Tree
queries and syntax validation for 21 programming and configuration languages.

## 🚀 Invocation

Run the `ast-editor` command with a tool name, a path, and the tool's own
parameters as options. Paths are relative to the working directory:

```bash
ast-editor view src/config.rs --start-line 40 --end-line 80
ast-editor inspect src/config.rs --template functions
ast-editor --help
```

A tool is named by any unambiguous prefix, so `ins` is `inspect` and `cr` is
`create`.

The options are the schema: every parameter `ast-editor skill api` lists can be
passed as `--kebab-case`, and a switch takes `--flag`, `--flag false` or
`--no-flag`. A shape no option can carry, such as an array of edits, goes in as
`--json '{...}'`, and a whole argument object still works as one JSON string.

**Editing uses a script on stdin, not JSON** — unless an option already says
what to change (`--apply <preview_id>`, or `--json '{"edits":[...]}'`), in
which case stdin is not read. One directive per line, each block fenced with
three or more backticks, so code goes in exactly as written — no quoting, no
`\n`, no escaping of any kind:

```bash
ast-editor edit /abs/path/file.rs <<'EOF'
replace 2#0759 ```
    let msg = format!("can't parse {:?}: {}", path, err);
```
replace_range 5a#7788 6b#99aa ```
    fn replaced() {
    }
```
delete 7c#aabb
EOF
```

Directives: `replace <id>`, `replace_range <start> <end>`, `insert_after <id>`,
`insert_before <id>`, `append`, `prepend` (all with a block); `delete <id>` and
`move <start> [<end>] before|after <dest>` (no block). Add `--dry-run` or
`--strict` after the file.

If the content itself contains a line of three backticks, open with four — the
closing fence must be at least as long, exactly as in Markdown.

Within one batch, do not target a line an earlier directive changed: the id
carries that line's content hash, so it no longer matches and the batch is
refused.

The arguments are exactly the tool schema, so anything `ast-editor skill api`
describes works here unchanged. Failures go to stderr with a non-zero exit
status.

**A response is a sequence of fenced blocks**, named by what they hold: the
file's own language for code, `diff` for a diff, `json` for the data. So code
arrives unescaped and the data is still machine-readable:

````bash
ast-editor view src/config.rs | awk '/^```json$/{f=1;next} /^```/{f=0} f' | jq .ids
````

A fence is longer than any run of backticks that starts a line inside it — the
rule the edit script reads — and no line of the json can begin with one, so a
json block is always fenced with exactly three and the pattern above is exact.
A code block holding a markdown file is not: match a run of three or more
there, or take everything between the first fence and the last.

Tools: `outline`, `inspect`, `view`, `edit`, `create` — file, block, line,
change, new file.

This document and its references are carried inside the binary, so they are
readable wherever it is: `ast-editor skill` prints this page, `ast-editor skill
api` the full parameter reference rendered from the live schemas, and
`ast-editor skill <topic>` any of the rest — `--help` lists them.
`ast-editor --version` reports the grammar set the binary is paired with, which
is the first thing to check when a file will not parse.

---

## 🔎 Finding What to Edit

You never need a line number, and never need `grep` first. Three ways in, all
returning the line IDs `edit` takes.

`view` puts the id on the line it names — `<id>|<line>: <text>`, padded so the
ids, the numbers and the text each stand in one column — so nothing has to be
cross-referenced against a table, and a line's own indentation is what the
text column shows. A line too long to print in one
row is broken at a fixed count of characters onto `│:` and `└:` rows; they
join back to exactly what the file holds, since the break adds and removes
nothing. `--only-ids` answers with ids and line numbers as JSON instead.

**By file** — `outline` lists what the file declares, which is the first look
at one you have not read:

```bash
ast-editor outline src/config.rs
# → outline[].signature with start_id / end_id and line ranges
```

Pass `--sexp` instead to get the whole parse tree, which is what a custom
`inspect` query is written against.

**By content** — `view` takes a `query`, a regular expression, with optional
`context_lines`. Search with it rather than by piping the output to `grep` or
`rg`: the query is matched against the file's own lines, while a pipe sees the
printed rows, where the id prefix defeats `^` and a wrapped line hides a match
that straddles the break. `(?i)` at the front ignores case, and
`--fixed-string` takes the query literally.

```bash
ast-editor view src/config.rs --query get_wasm_dir --context-lines 2
ast-editor view src/config.rs --query '^\s*pub fn' --context-lines 0
```

**By structure** — `inspect` matches carry `start_id` and `end_id`, so a
whole definition can be replaced in the next call:

```bash
ast-editor inspect src/config.rs --template functions
# → matches[].start_id / end_id
ast-editor edit src/config.rs <<'EOF'
replace_range <start_id> <end_id> ```
    …
```
EOF
```

`inspect` searches, so it needs a `query` or a `template`; called with
neither it says to run `outline` instead. An empty `matches` therefore always
means the search found nothing, never that nothing was asked for.

---

## 🌟 Why Use This Tool?

1. **Shift-Invariant Targeting** — lines are addressed by stable IDs
   (`"1#dfca"`), not by number. Inserting or deleting lines elsewhere does not
   move them, so a batch of edits cannot slide out of alignment.
2. **Duplicate Safety** — search-and-replace overwrites the wrong line when a
   pattern repeats. A Line ID names one line.
3. **Reuse IDs Across Turns** — an ID stays valid while its line is unchanged,
   and survives edits elsewhere in the file, restarts, and reformatting by an
   external formatter. Re-reading the file to find a line you already have an
   ID for is wasted context.
4. **Verify Before Committing** — `"dry_run": true` returns the unified diff
   and syntax result an edit batch would produce, without writing. A batch that
   validates also returns a short `preview_id`; pass it back as `apply` with
   the same `filepath` to commit that exact batch without resending `edits`:

   ```bash
   ID=$(ast-editor edit src/config.rs --dry-run < edits.txt | jq -r .preview_id)
   ast-editor edit src/config.rs --apply "$ID"
   ```

5. **Graceful Degradation** — Markdown, text, and configuration files are
   written with warnings rather than rejected, and edits still apply when no
   parser is available for the language.

---

## 🧭 Reference Index

Each is a command, because this document is read wherever the binary is — in a
terminal, in an installed skill tree, or on a machine where nothing was
installed beside it. A relative path would resolve in only one of those.

Load only what the immediate task needs; none of it belongs in context by
default.

```bash
ast-editor skill api      # every tool and parameter, from the live schemas
ast-editor skill usage    # editing workflows: resync, replace_range, long lines
```

S-expression queries per language:

```bash
ast-editor skill rust        ast-editor skill nix
ast-editor skill python      ast-editor skill swift
ast-editor skill javascript  ast-editor skill shell
ast-editor skill markup      # markup and configuration formats
```
