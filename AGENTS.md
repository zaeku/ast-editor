# Agent Guidelines & Project Rules

This document outlines project-specific rules that all AI coding agents must follow when modifying the `ast-editor` codebase.

## 1. No Hardcoded User-Facing Strings
All user-facing strings, tool tips, error footers, and hints must be stored in compile-time embedded JSON files under `resources/` (e.g., `resources/tool_metadata.json` or `resources/tool_config.json`).
- **NEVER** write literal strings like `"tip": "Edit these lines..."` or hardcoded error tips in the Rust source code files directly.
- Instead, read them using the `metadata` module (e.g., `crate::tools::metadata::get_tool_tip(...)` or custom configuration getters) which embeds and parses the JSON at compile-time via `once_cell::sync::Lazy`.
