# Implementation Plan: Markdown inspect_ast Integration

**Goal:**
1. Support `inspect_ast` for Markdown (.md, .markdown) files.
2. Implement Markdown-specific templates to minimize cognitive load:
   - `headings` / `headers` -> Matches Comrak `Heading` nodes.
   - `codeblocks` / `code_blocks` -> Matches Comrak `CodeBlock` nodes.
   - `links` -> Matches Comrak `Link` and `Image` nodes.
   - `tables` -> Matches GFM Comrak `Table` nodes.
   - `lists` -> Matches Comrak `List` nodes.
3. Support simplified node kind queries (e.g. `heading`, `code_block`, `table`, `list`, `item`, `paragraph`, `blockquote`) for custom searches.
4. Document the new templates in `SKILL.md` and `README.md`.

---

## Proposed Changes

### 1. Dedicated Markdown Inspect Handler
#### [MODIFY] [inspect.rs](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/src/tools/inspect.rs)
- Remove the placeholder markdown query matching error.
- Implement `run_markdown_inspect(code: &str, args: &InspectArgs, repository: &impl SessionRepository, session_id_opt: &Option<String>) -> Result<Value>`:
  - Parse the markdown content via Comrak with GFM extensions (tables, tasklist, strikethrough) enabled.
  - Parse the requested filter from `args.template` or `args.query`:
    - **Templates**:
      - `headings` / `headers` -> Matches Comrak `Heading` nodes.
      - `codeblocks` / `code_blocks` -> Matches Comrak `CodeBlock` nodes.
      - `links` -> Matches Comrak `Link` and `Image` nodes.
      - `tables` -> Matches GFM Comrak `Table` nodes.
      - `lists` -> Matches Comrak `List` nodes.
    - **Query Strings**:
      - Extract target word (e.g., `heading` from `(heading) @heading` or `"heading"`).
      - Match against node types (case-insensitive):
        - `"heading"` / `"header"` -> `NodeValue::Heading`
        - `"codeblock"` / `"code_block"` / `"code"` -> `NodeValue::CodeBlock`
        - `"link"` -> `NodeValue::Link`
        - `"image"` -> `NodeValue::Image`
        - `"table"` -> `NodeValue::Table`
        - `"list"` -> `NodeValue::List`
        - `"item"` -> `NodeValue::Item`
        - `"paragraph"` -> `NodeValue::Paragraph`
        - `"blockquote"` / `"block_quote"` -> `NodeValue::BlockQuote`
  - Recursively traverse the Comrak AST tree.
  - For each match, populate `InspectMatch` (and optionally `InspectDefinition` with formatted code tables for headings/codeblocks/tables/lists if `include_code` is not false).
  - Return the final matches wrapped in the standard `McpToolResult` JSON payload.
- Route markdown files in `run_inspect` to `run_markdown_inspect`.

### 2. Schema and Schema Registration
#### [MODIFY] [mod.rs](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/src/tools/mod.rs)
- Update `inspect_ast` tool schema input property descriptions to document the markdown-specific templates (`headings`, `codeblocks`, `links`, `tables`, `lists`).

### 3. Documentation updates
#### [MODIFY] [SKILL.md](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/SKILL.md)
- Document the Markdown-specific templates in `inspect_ast` tool guide.
#### [MODIFY] [README.tpl.md](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/README.tpl.md)
- Update the `inspect_ast` parameter documentation to list Markdown templates.

---

## Verification Plan

### Automated Tests
- Create unit tests in `src/tools/inspect.rs` verifying markdown inspect matching for all 5 templates and custom queries.
- Run `cargo test` to execute all tests.
- Re-run `cargo test --test readme_generator` to rebuild `README.md`.
