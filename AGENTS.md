# Agent Guidelines & Project Rules

Project-specific rules that all AI coding agents must follow when modifying the
`ast-editor` codebase.

The rules in force live in `decisions/`, a separate repository holding the
decisions that stand and the executable fences that fail when the code breaks
them; read its `SPEC.md` and `AGENTS.md` before adopting one. What belongs in
this file is what no check can reach. Nothing does today: the one rule that used
to be written here is `D-01M27G4E6GTCA8`, and `decisions/fences/` now catches it
being broken.

A rule adopted there is deleted from here in the same change, because a rule
kept in both places drifts and the copy without the fence is the one that goes
stale.

```sh
cd decisions && cargo run --bin check
```
