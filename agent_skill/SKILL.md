---
name: ast-editor
description: >
  Inspects code structure, finds target lines, and performs transactionally-validated line-level code edits using Tree-sitter. Resilient to syntax errors.
---

# AST-based Code Editor and Inspector Skill

This skill provides a powerful **general-purpose line-level text editing framework** for all text files (including Markdown, plain text, etc.), while **additionally** providing robust Abstract Syntax Tree (AST) query and syntax validation tools for 21 supported programming and configuration languages. It operates as a Model Context Protocol (MCP) server to eliminate terminal command execution warnings.

---

## 🌟 Core Value Proposition (Why Use This Tool?)

Unlike standard search-and-replace tools (e.g., `replace_file_content`), the `ast-editor` tool suite offers several distinct advantages for LLM agents:

1. **Shift-Invariant Targeting**:
   - targets specific lines using stable Line IDs (e.g., `"1#dfca"`).
   - Inserting or deleting lines in one part of a file does not shift the Line IDs of other lines. Your edits will never collision or misalign due to index shifting.
2. **Duplicate Safety**:
   - Traditional search-and-replace tools can overwrite the wrong line if duplicate patterns exist. By targeting unique Line IDs, `edit_lines` guarantees that the exact intended line is modified.
3. **Double-turn Avoidance (Token Savings)**:
   - Line IDs remain stable if the content is unchanged. You can reuse previous Line IDs directly in subsequent edits without calling `view_lines` again, saving massive token counts.
4. **Permissive Graceful Degradation**:
   - Bypasses syntax validation on markdown/text files and configuration files, writing edits cleanly to disk even if compiler infrastructure is absent.

---

## 🧭 Document & Reference Index

To keep your context window thin and efficient, do not load large specifications. Instead, reference only the document matching your immediate task:

- **JSON Schemas & API Parameters**: [api_specification.md](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/agent_skill/references/api_specification.md) (All inputs, outputs, and JSON payloads templates)
- **Advanced Editing Workflows**: [usage_guides.md](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/agent_skill/references/usage_guides.md) (Smart resync details, replace_range guidelines, editing long lines)
- **Language Specific Guides (S-Expression Queries & Tools)**:
  - **Rust**: [rust.md](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/agent_skill/references/languages/rust.md)
  - **Python**: [python.md](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/agent_skill/references/languages/python.md)
  - **Nix**: [nix.md](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/agent_skill/references/languages/nix.md)
  - **JavaScript & TypeScript**: [javascript.md](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/agent_skill/references/languages/javascript.md)
  - **Markup & Configurations**: [markup.md](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/agent_skill/references/languages/markup.md)
  - **Swift**: [swift.md](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/agent_skill/references/languages/swift.md)
  - **Shell Scripts**: [shell.md](file:///Users/zaeku/workspace/Tools%20for%20Agents/ast-editor/agent_skill/references/languages/shell.md)
