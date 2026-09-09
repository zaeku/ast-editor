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

Run the `ast-editor` command with a tool name and its arguments as one JSON
object:

```bash
ast-editor view_lines '{"filepath":"/abs/path/file.rs","start_line":40,"end_line":80}'
ast-editor --help
```

**Editing uses a script on stdin, not JSON.** One directive per line, each
block fenced with three or more backticks, so code goes in exactly as written —
no quoting, no `\n`, no escaping of any kind:

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

The arguments are exactly the tool schema, so anything
[api_specification.md](references/api_specification.md) describes works here
unchanged. Output goes to stdout with no wrapper and pipes normally; failures
go to stderr with a non-zero exit status.

Tools: `view_lines`, `edit_lines`, `create_lines`, `inspect_ast`, `dump_ast`.
`ast-editor --version` reports the grammar set the binary is paired with, which
is the first thing to check when a file will not parse.

The same binary serves MCP over JSON-RPC on stdin when asked — `ast-editor mcp`
— for clients that expect that. Prefer the command form: it keeps no tool
schemas resident in context.

---

## 🔎 Finding What to Edit

You never need a line number, and never need `grep` first. Two ways in, both
returning the line IDs `edit_lines` takes:

**By content** — `view_lines` takes a `query`, with optional `context_lines`:

```bash
ast-editor view_lines '{"filepath":"/abs/path/f.rs","query":"get_wasm_dir","context_lines":2}'
```

**By structure** — `inspect_ast` matches carry `start_id` and `end_id`, so a
whole definition can be replaced in the next call:

```bash
ast-editor inspect_ast '{"filepath":"/abs/path/f.rs","template":"functions"}'
# → matches[].start_id / end_id
ast-editor edit_lines '{"filepath":"/abs/path/f.rs","edits":[
  {"op":"replace_range","target_id":"<start_id>","end_target_id":"<end_id>","content":"…"}]}'
```

Called with neither `query` nor `template`, `inspect_ast` searches for nothing
and says so: `"query": null`, an empty `matches`, and an `outline` of the
file's top-level definitions with their IDs. An empty `matches` alongside a
null `query` means nothing was asked for — not that the file is empty.

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
   ID=$(ast-editor edit_lines '{"filepath":"/abs/path/f.rs","edits":[…],"dry_run":true}' | jq -r .preview_id)
   ast-editor edit_lines "{\"filepath\":\"/abs/path/f.rs\",\"apply\":\"$ID\"}"
   ```

5. **Graceful Degradation** — Markdown, text, and configuration files are
   written with warnings rather than rejected, and edits still apply when no
   parser is available for the language.

---

## 🧭 Reference Index

Load only the document your immediate task needs; none of these belong in
context by default.

- **JSON schemas, parameters, payloads** — [api_specification.md](references/api_specification.md)
- **Editing workflows** — [usage_guides.md](references/usage_guides.md)
  (concurrent-edit resync, `replace_range`, long lines)
- **S-expression queries per language** —
  [rust](references/languages/rust.md),
  [python](references/languages/python.md),
  [javascript & typescript](references/languages/javascript.md),
  [nix](references/languages/nix.md),
  [swift](references/languages/swift.md),
  [shell](references/languages/shell.md),
  [markup & configuration](references/languages/markup.md)
