# Changelog

What changed for someone who uses `ast-editor`, newest first. Why it changed is
in the commit that changed it, and what the project holds true is in
`decisions/`.

## 0.3.0 — 2026-09-12

An address is a span now, which removes an operation and renames two fields.
**A script or a JSON batch written against 0.2 has to change.**

### Changed

- **`replace_range` is gone. `replace` takes the span.** An address is one id
  or two separated by a comma, the way `view src/main.rs 40,80` already reads a
  range: `replace 1a#b029,22#f8c3` is what `replace_range 1a#b029 22#f8c3` was.
  A span of one line is one line, so `replace 1a#b029` is unchanged.
- **`delete` takes a span too**, so `delete 1a#b029,22#f8c3` removes a block.
  It used to take one line, and removing several meant replacing the range with
  an empty payload.
- **`move` spells its range with the comma**: `move a,b before c`, where it
  took `move a b before c`.
- **`target_id` is `start_id`, `end_target_id` is `end_id`, and
  `dest_target_id` is `dest_id`.** These are the keys `inspect` and `outline`
  already answer with, so a match they found pastes into an edit without being
  renamed — which is what prompted the change. There are no aliases: the old
  names are refused.
- **An op that acts at one line says so when given a span.** `insert_after a,b`
  is refused by name rather than silently taking the first id.
- The first line of a replaced span keeps its id, where a replaced range used
  to mint a new one for every line. The lines after it are new.

## 0.2.1 — 2026-09-12

### Fixed

- **A `replace` given several lines answers with all of them.** It wrote them
  all and reported one, and the id it reported named no line: the sequence
  number the edit had just retired, carrying the hash of the whole payload. A
  following edit addressed with it was refused — "That line is gone" — about a
  line that was there under a different id. `replace_range` and the insert
  operations were unaffected.

## 0.2.0 — 2026-09-12

A minor release rather than a patch because several answers changed shape, and
one of them changes what an identical script does.

### Changed

- **An empty payload writes nothing.** `replace_range 1#a 2#b ``` ``` ` removes
  the span, where it used to leave one blank line; `replace` with an empty
  payload removes the line. Content is line-terminated text now: `""` is no
  lines, `"\n"` is one empty line, and a trailing newline ends the last line
  rather than starting another. The JSON form reads it the same way. **This is
  the one change that alters what an existing script does without saying so** —
  a script that relied on an empty fence to leave a blank line now deletes.
- **`modified_ids` is `modified_lines`, and `view --only-ids` answers with
  `lines` rather than `ids`.** Each entry is a line as `[id, line number]`:
  `[["2#9639",2], ["4#b7a3",3]]` where `edit` used to answer `["2#9639",
  "4#b7a3"]`. The name says the shape, so nothing has to be read elsewhere to
  know it. The list is in file order, and an id a batch touched twice appears
  once.
- **A refused edit says what to do next**, and the two refusals differ because
  the remedies do: an id naming no line says the line is gone and there is
  nothing to re-read for it; a line that changed says to read that line again.
  Both add that the ids held for other lines are unaffected. The
  `CHECKSUM_ERROR` and `CONCURRENCY_ERROR` prefixes are gone.
- **A `--strict` refusal answers in the shape of a dry run** — `syntax_valid`,
  the diagnostics, and a `preview_id` — on stderr, still exiting non-zero. The
  batch it refused is kept, so `--apply <id>` commits it if you judge the parser
  wrong. A dry run whose result does not parse now returns an id too.
- **An `inspect` whose query does not compile is an error**, where it used to
  report zero matches with a hint and exit 0.
- **An edit to a file no grammar covers says so**, with `syntax_valid: null` and
  a message. An edit that parsed still answers with its ids alone.
- **`--version` reports the grammars compiled in**, each with its version, in
  place of the grammar directory it used to name.
- **`AST_EDITOR_WASM_DIR` does nothing**, and the install places no
  `share/ast-editor/wasm`. The install is the binary and the skill.
- **`.nix` files are syntax-checked.** A `--strict` edit that breaks one is now
  refused, where it used to be written.
- The preview refusals no longer tell you to run `edit_lines` with `dry_run`,
  which have not existed since the MCP server was removed.

### Added

- **`view` takes several paths**, answering with one block per file and a `json`
  block whose `files` array carries each file's metadata. One path answers
  exactly as before.
- **A line range can follow the path**, the way `sed -n '40,80p'` takes one:
  `40,80`, `40`, `40,`, `,40`. The options still work.
- **`<tool> --help`** prints that tool's options with their types and
  descriptions, instead of refusing the flag.

### Fixed

- **The first `edit` on a machine worked.** It failed with `no such table:
  sessions` unless something had read a file first, which the skill happens to
  tell you to do.
- **Grammars are found by where the binary is.** `CARGO_MANIFEST_DIR` was
  consulted first, so an `ast-editor` called from any other crate's `cargo run`
  or `cargo test` lost every language.
- **Valid Rust is no longer reported as broken.** The bundled grammar read
  `&raw` as the start of a raw borrow, so an ordinary borrow of a variable named
  `raw` failed to parse and `--strict` refused valid edits to such files.
- **`--help | head -30` no longer panics** on the closed pipe.
- **Reading a file with no grammar is quiet.** Every such read logged an error
  twice.
- `inspect` stops advertising `output_file`, which it accepted and ignored.

### Inside

- The grammars are Cargo dependencies compiled into the binary, at versions
  `Cargo.lock` pins, in place of WebAssembly modules loaded at run time.
  wasmtime is gone with them. The test suite runs in about a third of the time
  and the release build in half.
- The first thirty lines of `--help` carry what a first reader needs: the
  invocation, the tools, the id model, the directives, and the response shape.
