# Spec: Skill Refactoring and Template Isolation (`ast-editor`)

This specification defines the migration of the `SKILL.md` document and its associated template/reference files from the project root into a dedicated `agent_skill/` directory.

## 1. Directory Structure

All skill-related files will be encapsulated under `[project-root]/agent_skill/`:

```
[ast-editor]/
  └── agent_skill/
        ├── SKILL.md                 # Thin core hub + Value Propositions (Generated)
        ├── doc_templates/           # Source templates for generation
        │     ├── README.tpl.md
        │     ├── api_specification.tpl.md
        │     └── SKILL.tpl.md
        └── references/              # Detailed guides
              ├── api_specification.md  # Generated tool schemas & JSON outputs
              ├── usage_guides.md       # Detailed workflows & resync mechanisms
              └── languages/         # Split language guides
                    ├── rust.md
                    ├── nix.md
                    ├── python.md
                    └── ...
```

## 2. Target Output Mapping & Build Flow
- **README.md**: Regenerated from `agent_skill/doc_templates/README.tpl.md` ➡️ Output: `[project-root]/README.md`.
- **api_specification.md**: Regenerated from `agent_skill/doc_templates/api_specification.tpl.md` (containing schema placeholders) ➡️ Output: `agent_skill/references/api_specification.md`.
- **SKILL.md**: Regenerated from `agent_skill/doc_templates/SKILL.tpl.md` (retaining Core Value Propositions and Rules, without schemas placeholders) ➡️ Output: `agent_skill/SKILL.md`.

## 3. Test/Generator Script Refactoring
- The test suite `tests/readme_generator.rs` must be modified to point to the new paths:
  - Input template 1: `agent_skill/doc_templates/README.tpl.md` ➡️ Output: `README.md`
  - Input template 2: `agent_skill/doc_templates/api_specification.tpl.md` ➡️ Output: `agent_skill/references/api_specification.md`
  - Input template 3: `agent_skill/doc_templates/SKILL.tpl.md` ➡️ Output: `agent_skill/SKILL.md`
