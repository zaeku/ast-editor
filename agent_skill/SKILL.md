---
name: ast-editor
description: >
  Edits lines by id, validated as one transaction, and answers structural queries with Tree-sitter. Reads files a parser rejects. Invoked as the `ast-editor` command.
---

# AST-based Code Editor and Inspector Skill

Edits any text file line by line, and answers structural queries and syntax
checks for the languages it carries a grammar for, which `--version` lists.

## 🚀 Invocation

Run the `ast-editor` command with a tool name, a path, and the tool's own
parameters as options. Paths are relative to the working directory:

```bash
ast-editor view src/config.rs 40,80
ast-editor inspect src/config.rs --template functions
ast-editor --help
```

A tool is named by any unambiguous prefix, so `ins` is `inspect` and `cr` is
`create`. A line range can follow the path the way `sed -n '40,80p'` takes one:
`40,80`, `40` alone, `40,` to the end, `,80` from the start. The
`--start-line` and `--end-line` options mean the same thing.

`view` takes several paths and answers with one block per file, followed by a
`json` block whose `files` array carries each file's own metadata under its
`filepath`. One path answers as it always has.

The options are the schema. Pass any parameter `ast-editor skill api` lists as
`--kebab-case`. A boolean option takes `--flag`, `--flag false` or `--no-flag`.
A shape no option can carry, such as an array of edits, goes in as
`--json '{...}'`; a whole argument object still works as one JSON string.

**Editing uses a script on stdin, not JSON.** What `edit` reads depends on the
options it was given:

- No option says what to change → the script on stdin does.
- `--apply <preview_id>` or `--json '{"edits":[...]}'` → `edit` does not read
  stdin at all.

Write one directive per line, and fence each payload with three or more
backticks. If the payload itself contains a line of three backticks, open with
four: the closing fence must be at least as long, exactly as in Markdown. Code
goes in as written — no quoting, no `\n`, no escaping of any kind:

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

Directives taking a payload: `replace <id>`, `replace_range <start> <end>`,
`insert_after <id>`, `insert_before <id>`, `append`, `prepend`. Directives
taking none: `delete <id>`, `move <start> [<end>] before|after <dest>`.

Add `--dry-run` or `--strict` after the file. `--strict` is the
`strict_validation` parameter the API reference names.

The syntax check is a parser's opinion and tree-sitter is sometimes wrong about
valid code, so nothing a check refuses is thrown away: a refusal names the batch
it kept, and `--apply <preview_id>` commits it if you judge the parser wrong.

Within one batch, do not target a line an earlier directive changed. The id
carries that line's content hash, so it no longer matches and `edit` refuses
the whole batch.

Failures go to stderr with a non-zero exit status, so a response says nothing
about having succeeded. A field appears when it has something to say: a
`status` names a state that is not plain success, such as an `edit` `saved`
past a validation it failed; `syntax_valid` is `null` where no grammar covers
the file, so the edit was written without being checked at all; and a
`message` or a `warnings` array carries what was worth saying.

A `lines` or `modified_lines` array holds one entry per line, `[id, line
number]`, in file order — so the id and where it landed arrive together and a
following edit needs no second read.

**A response is a sequence of fenced blocks**, named by what they hold: the
file's own language for code, `diff` for a diff, `json` for the data. So code
arrives unescaped and the data is still machine-readable:

````bash
ast-editor view src/config.rs | awk '/^```json$/{f=1;next} /^```/{f=0} f' | jq .ids
````

A fence is longer than any run of backticks that starts a line inside it,
which is the rule the edit script reads. Two cases follow:

- A `json` block is always exactly three, since no line of JSON can begin with
  a backtick. The pattern above is an exact match for one.
- A code block holding a markdown file can be longer. Match a run of three or
  more, or take everything between the first fence and the last.

Five tools, narrowing by scale: `outline` reads a whole file, `inspect` finds
a block in it, `view` reads that block's lines, `edit` changes them, and
`create` makes a file that does not exist yet. `create` refuses a path that
does exist, so an existing file is only ever edited:

```bash
ast-editor create src/new.rs --content 'fn main() {}
' --return-ids
```

This document and its references are carried inside the binary, so they are
readable wherever it is: `ast-editor skill` prints this page, `ast-editor skill
api` the full parameter reference rendered from the live schemas, and
`ast-editor skill <topic>` any of the rest — `--help` lists them.
`ast-editor --version` reports the grammar set the binary is paired with, which
is the first thing to check when a file will not parse.

---

## 🔎 Finding What to Edit

You never need a line number, and never need `grep` first. Three ways in, all
returning the line ids `edit` takes.

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
`rg`. `view` matches the query against the file's own lines, while a pipe sees
the printed rows: there the id prefix defeats `^`, and a wrapped line hides a
match that straddles the break. `(?i)` at the front ignores case, and
`--fixed-string` takes the query literally.

```bash
ast-editor view src/config.rs --query grammar_for_extension --context-lines 2
ast-editor view src/config.rs --query '^\s*pub fn' --context-lines 0
```

**By structure** — `inspect` searches, so give it a `query` or a `template`;
called with neither, it says to run `outline` instead. Its matches carry
`start_id` and `end_id`, so the next call can replace a whole definition:

```bash
ast-editor inspect src/config.rs --template functions
# → matches[].start_id / end_id
ast-editor edit src/config.rs <<'EOF'
replace_range <start_id> <end_id> ```
    …
```
EOF
```

An empty `matches` therefore always means the search found nothing, never that
nothing was asked for.

`view` puts the id on the line it names — `<id>|<line>: <text>`, padded so the
ids, the numbers and the text each stand in one column. Nothing has to be
cross-referenced against a table, and the text column shows a line's own
indentation. `view` breaks a line too long for one row at a fixed count of
characters onto `│:` and `└:` rows; those rows join back to exactly what the
file holds, because the break adds and removes nothing. `--only-ids` answers
with ids and line numbers as JSON instead.

---

## 🌟 What a Line Id Holds

1. **After an edit, do not read the same lines again.** The ids you already
   hold still address them. An edit mints ids for the lines it writes and
   leaves every other id alone, so re-reading a file you just edited buys
   nothing and costs a call:

       before   1#fe05  2#ad78  3#b802  4#9f8f
                        insert_after 2#ad78
       after    1#fe05  2#ad78  6#7722  3#b802  4#9f8f

   The number locates the line and the hash guards its content, which is why
   `3#b802` still names the line it named before, at a line number one
   further down.
2. **An id stays valid while its line is unchanged.** It survives edits
   elsewhere in the file, restarts, and reformatting by an external formatter,
   so an id held from an earlier turn still edits the line it named. An id
   whose own line changed underneath is refused, never applied to whatever
   sits there now.
3. **`--dry-run` answers with the diff and the syntax result an edit batch
   would produce, and writes nothing.** A batch that validates also mints a
   short `preview_id`. Pass it back with `--apply` and the same file to commit
   that exact batch without resending the script:

   ````bash
   ID=$(ast-editor edit src/config.rs --dry-run < edits.txt |
        awk '/^```json$/{f=1;next} /^```/{f=0} f' | jq -r .preview_id)
   ast-editor edit src/config.rs --apply "$ID"
   ````

4. **`edit` writes Markdown, text and configuration files with a warning
   rather than rejecting them**, and applies edits to a language it has no
   parser for.

---

## 🧭 Reference Index

Each reference is a command, so it is readable wherever the binary is. Load
only what the immediate task needs.

```bash
ast-editor skill api      # every tool and parameter, from the live schemas
ast-editor skill usage    # editing workflows: resync, replace_range, long lines
```

S-expression queries per language:

```bash
ast-editor skill rust        ast-editor skill go
ast-editor skill python      ast-editor skill swift
ast-editor skill javascript  ast-editor skill shell
ast-editor skill nix         ast-editor skill markup
```
