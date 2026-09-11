# Agent Guidelines

Guidelines for an agent working on ast-editor. It holds rules, the boundaries
this project defends, and where work goes. It is not a description of the
product — read `README.md` for that, and read it first if you have not.

**Terms.** *Line id* means the `n#hash` pair a tool prints beside a line: a
sequence number and a hash of that line's content. *Session* means the cached
database under the user's cache directory that maps a file's lines to their ids.
*Grammar* means a tree-sitter `.wasm` the runtime compiles in process on first
use and caches outside this repository. *Script* means the edit-directive format
`ast-editor edit` reads from stdin. *Tool* means one of the five the binary
serves: `inspect`, `outline`, `view`, `edit`, `create`.

## Three layers

This project is three repositories, and which one a change belongs in is the
first question, not a detail of filing it.

| Layer | Holds | Changes |
|---|---|---|
| This one | The implementation, the grammars it ships, and the documents it publishes | Per work item |
| `decisions/` | The rules that stand, and the fences that fail when the code breaks one | When a rule does |
| `kanban/` | What is next | Hourly |

Both of the others are ignored here and carry their own history, so a commit in
one never drags the others along. The rhythms are the reason: a rule that had to
be committed alongside the code it governs would be revised whenever the code
was, which is exactly the coupling the split exists to prevent.

## Where code goes

`src/main.rs` dispatches, `src/cli.rs` declares the command line, `src/skill.rs`
serves the skill documents, `src/parser.rs` loads a grammar and runs a query, and
`src/config.rs` resolves settings. The five tools are one file each under
`src/tools/`, over three shared ones: `session_db.rs` owns the line-id store,
`formatter.rs` owns what output looks like, and `metadata.rs` owns every string
the binary prints. New work joins the file whose subject it already is.

Two things stay outside the binary. Grammar compilation belongs to the bundled
`wasmtime` compiler, because a host that could compile would carry a code
generator into every install; and the cache lives under the user's cache
directory rather than in a tree ast-editor edits, because a tool that writes into
its subject cannot be pointed at itself safely.

## Decision layer

`decisions/` is a separate repository carrying its own toolchain, so that a clone
of it alone can verify itself. `decisions/SPEC.md` is its charter: it defines
what a decision document is, what a fence is, and the checks that enforce both.
Read §7 and §12 before you add a decision or a fence. It carries its own
`AGENTS.md`, so nothing about working inside it is repeated here.

**What belongs there rather than here.** A rule in this file is a rule someone
has to remember. A decision in `live/` with a fence in `fences/` is a rule that
fails a check when it is broken. A rule a check can reach belongs there, and this
file should end up holding only what no check reaches.

The criterion for moving is not only whether a fence can be written. It is
whether the code layer could be rebuilt without the rule. Decisions are expensive
and code is cheap, and the layer's purpose is that this tree could be lost and
stood up again from what `decisions/` holds. A rule that would have to be
rediscovered is load-bearing whether or not anything can execute it, and §4
provides `fence: none` with `verified:` for exactly that case.

**Adopting a rule means writing its fence in the same change** (§8 Adopt), and
deleting the rule from this file. A rule kept in both places drifts, and the copy
without the fence is the one that goes stale.

**A rule is identified by its opening sentence, not by a number.** Rules leave
this file one at a time as they are adopted, so a position would shift under
every adoption and every reference to one would rot silently.

**No list of decisions is kept here.** `live/` is the list, and
`cargo run --bin check` derives the rest. §2 allows references in one direction
only — the faster layer points at the slower one — so a table of decisions in
this file would be a stored reverse reference, and it would be wrong the first
time a decision moved. To find which rule a decision came from, run
`jj log -r 'diff_lines(substring:"D-...")' -p` in whichever layer you are
standing in; the change that deletes the rule is the mapping.

**Before you decide a rule is out of reach, ask which surface it lives on.** §5
lets a fence read whatever the code layer exposes to anyone else, and ast-editor
exposes two things: the `ast-editor` command on `PATH` — its subcommands, its
output, its exit codes, and the files it rewrites — and the documents it
publishes, which are `README.md`, `agent_skill/SKILL.md`, `CHANGELOG.md`, this
file, and the strings under `resources/` that the binary prints back to a
caller. There is no `docs/` any more: every rule it held names a decision, and
the narrative around them is in the history of the changes that removed it. What
§5 forbids is implementation internals: `src/`, `tests/`, a Cargo target, a
`just` recipe. A rule about what this project's documents say is reachable.

**A decision may be adopted before the code satisfies it.** §5 says a fence that
cannot find its subject exits 2, and that this is the intended direction of work
rather than a defect. So a decision about where this tool should end up can stand
first, with a fence that reports where the work is until the code catches up.
For a product that has to be finished rather than merely working, that is the
useful direction: the fence states the standard, and `cargo run --bin check`
answers how far off it is.

**A decision carries the numbers it relies on and names no file here.** §2
reaches documents, and §1 says why: a path into this layer dangles the moment
this layer is the thing that was lost. Write the figure, how it was measured, and
when, as sentences in the decision's own body.

**`check` tests what you installed, not what you built.** The command surface is
the `ast-editor` on `PATH`, and `target/release/` is not on it. Run
`just install` before `cargo run --bin check` in `decisions/`, or a fence will
correctly report that the binary someone could actually run does not hold the
decision you just wrote.

## Board

`kanban/` is a third repository, ignored here, holding what is next rather than
what is true. Run `kanban-md board` from the repository root. It is local: it
orders one person's work and records what blocks what, and nothing in it is an
agreement with anyone else.

**A card whose work changes a rule stands that rule's fence up first.**
`cargo run --bin check` then says where the work stands, and the fence names the
surface it cannot find yet. The card is done when the fence passes, which is a
different and harder claim than the code compiling.

**A finding does not belong on the board.** What was measured goes in a commit;
what the project now holds true goes in `decisions/`. The board holds only what
has not been done yet.

**A card that is waiting for something says so.** Work that could plausibly
happen sits in `backlog` with `--block` carrying the condition that would make
it worth scheduling — `kanban-md list --blocked` is therefore the list of things
nobody has committed to, and unblocking one is the act of saying its condition
is met. There is no second list of ideas anywhere else; there was one, in
`docs/`, and it went stale in the way a hand-kept list does.

An idea refused outright is not a card at all. It goes to `decisions/` as a
proposal and is rejected there in the next change, which leaves a tripwire
watching for the code drifting into it and a document recoverable by id — see
`D-01M2828G9S60M9`, the versioned content store this tool nearly became.

**Nothing enforces any of this.** No fence reaches the board — a fence tests the
code layer as a black box, and the board is neither the code layer nor
published — so it is a rule someone has to remember, which is what this file is
for.

## Generated documents

`README.md` and `agent_skill/references/api_specification.md` are rendered from
`agent_skill/doc_templates/*.tpl.md` against the schemas the binary serves. Run
`just doc`; the generator is a test, so `just test` covers it too. Do not edit
either output by hand — the next `just doc` discards the edit, and the template
is where the sentence belongs.

`ast-editor skill` prints the skill document from the binary rather than from a
file in this tree, and `just install-skill` writes that output to the installed
location. There is one copy, and it is the one the binary serves.

## Toolchain

Nix declares the tools on this machine. Do not install a tool globally to make a
task work — add it to the flake that needs it, so the next reader gets it.
`flake.nix` declares `rustc`, `cargo`, `clippy`, `rustfmt`, `rust-analyzer` and
`just`. `.envrc` enters that shell and points `CARGO_HOME` at `.cargo-cache/`
inside the tree, so `direnv allow` once and the directory arrives configured.
The decision layer declares its own toolchain, because it has to verify itself
without this one.

`just` is the task list: `just build`, `just test`, `just lint`, `just doc`,
`just install`. Read `justfile` before adding a step elsewhere; the comments
there carry why each task does what it does.

## Version control

This layer is Git, on `master`. `decisions/` and `kanban/` are Jujutsu colocated
with Git, so `jj` and `git` both work inside them. Read the
[`use-jujutsu-safely`](https://github.com/zaeku/skills/tree/main/plugins/version-control/skills/use-jujutsu-safely)
skill before an unfamiliar `jj` command.

**A description is true of the diff or it is false.** Write it from what the
change does rather than from what it was for. The two part company exactly where
some piece turned out harder than expected and was left, which is the moment the
description matters most. Whoever reads a description is choosing not to read the
diff, and that is what makes an untrue one expensive rather than untidy.

**Do not discard existing changes.** They belong to the user unless a task
identifies them as agent changes.

## Language

Write documentation, project artifacts, code comments, and change descriptions in
English. Reply to the user in the language of their prompt.
