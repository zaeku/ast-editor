# Specification: Rust-based tree-sitter-inspector with Lazy Local AOT Compilation

This document specifies the technical design, architectural flow, and deployment strategy for migrating the `tree-sitter-inspector` MCP server to a Rust-based host utilizing a headless WebAssembly (WASM) runtime with lazy local AOT (Ahead-of-Time) compilation.

---

## 1. Project Goal

The primary goals of this sub-project are:
*   **Startup Speed**: Achieve instant cold starts ($<10\text{ms}$) by bypassing JS runtime boot times and runtime JIT compilation overhead.
*   **Minimal Footprint**: Deliver a highly optimized, lightweight host binary ($<5\text{MB}$) without embedding heavy compiler codebases.
*   **Offline/Air-gapped Reliability**: Package the platform-specific Wasm compiler natively within platform-specific distribution bundles to support networks without internet access.

---

## 2. System Architecture Overview

The system implements the **Pattern C: Headless Host with Lazy Local Compilation (On-Demand AOT)** model. 

```
+--------------------------------------------------------+
|                      MCP Client                        |
+--------------------------------------------------------+
                           ^
                           | Stdio (JSON-RPC)
                           v
+--------------------------------------------------------+
|             tree-sitter-inspector (Host)               |
|            [Rust Native Binary - Headless]             |
+--------------------------------------------------------+
      |                                            |
      | 1. Query Cache (~/.cache)                  | 2. Exec compiler (Subprocess)
      v                                            v
+-----------+    No (Miss)                   +-----------+
|  .cwasm   | -----------------------------> | Compiler  | (Bundled Utility)
| Cache file|                                |  Engine   |
+-----------+                                +-----------+
      |                                            |
      | Yes (Hit)                                  | 3. Compiles .wasm to .cwasm
      |                                            v
      |                                      +-----------+
      |                                      |   .wasm   | (Original grammar)
      |                                      +-----------+
      v                                            |
[Wasmtime Headless Load] <-------------------------+
      |
      v
[Execute Parser & Query AST]
```

### Components
1.  **Headless Host (`tree-sitter-inspector`)**: A Rust native binary compiled without the Wasmtime `cranelift` compilation backend. It reads JSON-RPC via stdio, controls AST parsing, runs queries, and deserializes pre-compiled `.cwasm` grammars.
2.  **Original Grammars (`wasm/*.wasm`)**: Platform-independent raw WebAssembly files for each supported language.
3.  **Local Compiler Utility (`wasmtime-compiler`)**: A platform-appropriate compiled executable capable of AOT-compiling `.wasm` into `.cwasm` for the host platform.
4.  **Local Cache Store (`~/.cache/tree-sitter-inspector/`)**: Local storage location where compiled `.cwasm` files are persisted.

---

## 3. Detailed Runtime Workflow

### Step 1: Initialize Server
The host starts, registers MCP tools, and establishes Stdio transport. No grammars are loaded at startup.

### Step 2: Tool Call Received
An inspect request for `main.py` is received.
1.  The host identifies the file extension (`.py`) and maps it to `tree-sitter-python.wasm`.
2.  It calculates the expected cache path for the compiled grammar:
    `~/.cache/tree-sitter-inspector/<wasmtime-version>/tree-sitter-python.cwasm`

### Step 3: Cache Verification & Lazy Compilation
*   **Scenario A: Cache Hit**
    *   The host loads the serialized `tree-sitter-python.cwasm` file.
    *   It deserializes the module instantly into the headless Wasmtime store.
    *   Execution proceeds.
*   **Scenario B: Cache Miss**
    *   The host locates the bundled compiler utility (`wasmtime-compiler`) and the raw grammar `wasm/tree-sitter-python.wasm`.
    *   The host invokes a subprocess:
        ```bash
        wasmtime-compiler compile -o <cache_path>/tree-sitter-python.cwasm wasm/tree-sitter-python.wasm
        ```
    *   The subprocess blocks until compilation completes (expected duration: $100\text{ms} - 300\text{ms}$).
    *   The host loads and deserializes the newly generated `tree-sitter-python.cwasm`.
    *   The raw `wasm/tree-sitter-python.wasm` file remains intact or is deleted depending on configuration.

### Step 4: Parse & Inspect
The host feeds the source code buffer and the loaded `Language` struct into the `tree-sitter` parser engine, runs S-expression queries, and formats output.

---

## 4. Project Directory Structure

The sub-project is structured inside the main repository under the `rust/` folder:

```
tree-sitter-inspector/
├── package.json
├── src/                # Current TypeScript implementation
├── wasm/               # Raw WebAssembly grammar files (.wasm)
└── rust/               # [NEW] Sub-project directory
    ├── Cargo.toml      # Project manifest (declares headless features)
    ├── spec.md         # This specification document
    ├── src/
    │   ├── main.rs     # MCP Stdio Server entry point
    │   ├── parser.rs   # Wasmtime headless loader & lazy compilation launcher
    │   ├── config.rs   # Extension mapping and template paths
    │   └── tools/      # Inspect and Dump AST implementations
    │       ├── inspect.rs
    │       └── dump.rs
    └── scripts/
        └── build-releases.sh  # Cross-platform packaging script
```

---

## 5. Crate Configurations (`Cargo.toml`)

To enforce the headless nature of Wasmtime, we disable default features and explicitly exclude compilation components:

```toml
[package]
name = "tree-sitter-inspector-rust"
version = "0.1.0"
edition = "2021"

[dependencies]
# We use tree-sitter bindings but might need manual integration with 
# headless wasmtime if the official crate's high-level wrapper pulls in cranelift.
tree-sitter = "0.22"

# Wasmtime configured to be headless (exclude compiler backend)
wasmtime = { version = "22", default-features = false, features = ["runtime", "async", "cache"] }

# Stdio JSON-RPC MCP Server implementation
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
tokio = { version = "1.0", features = ["full"] }
```

---

## 6. Distribution & Packaging

To support target-specific distribution with minimal overhead, the packaging pipeline compiles and bundles assets per target platform:

### Target Matrix
*   `aarch64-apple-darwin` (Apple Silicon macOS)
*   `x86_64-apple-darwin` (Intel macOS)
*   `x86_64-unknown-linux-gnu` (Linux)
*   `x86_64-pc-windows-msvc` (Windows)

### Bundle Anatomy (Per Target)
Each target package (e.g. `tree-sitter-inspector-darwin-arm64.tar.gz` or npm optional dependency package) contains:
1.  **`tree-sitter-inspector`**: The OS-specific headless host binary (~3-4MB).
2.  **`wasmtime-compiler`**: The OS-specific compiler binary (~15-20MB).
3.  **`wasm/*.wasm`**: Platform-independent raw grammar files (~10MB total).

By distributing these together, the installation is self-contained, fully offline-functional, and requires no external compiler dependencies.
