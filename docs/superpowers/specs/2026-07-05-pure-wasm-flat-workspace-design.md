# Spec: Pure WASM Parser Engine & Flat Cargo Workspace

이 스펙 문서는 tree-sitter-inspector-rs 프로젝트를 고도의 확장성과 대칭성을 갖춘 아키텍처로 재편하기 위해, 네이티브 파서 의존성을 완전히 제거하고 WebAssembly 런타임 내에서 파싱을 독점적으로 구동하도록 전환하며, 패키지를 격리 및 평탄화(Flat Workspace)하는 설계를 명세합니다.

---

## 1. 아키텍처 목표 (Architectural Goals)

1. **Pure WASM 동적 플러그인화**:
   - `tree-sitter-rust`, `tree-sitter-python` 등의 하드코딩된 Rust 언어 크레이트 바인딩을 100% 제거합니다.
   - 신규 언어 지원 시, `resources/wasm/`에 `.wasm` 파일을 배치하고 `languages.json`에 매핑 규칙을 기록하는 것만으로 **바이너리 재배포 없이 런타임에 즉각 지원**되도록 만듭니다.
2. **평탄한 Cargo 워크스페이스 구조 (Flat Symmetric Workspace)**:
   - 프로젝트 루트 디렉토리는 오직 워크스페이스 매니페스트 및 공용 설정(resources, docs)만을 관리합니다.
   - 호스트 MCP 서버와 AOT 컴파일러를 각각 `tree-sitter-inspector/` 및 `wasmtime-compiler/` 개별 하위 프로젝트로 분리합니다.
3. **Workspace 상속 (Workspace Inheritance) 도입**:
   - 패키지 메타데이터(version, edition, authors) 및 공용 크레이트 의존성 설정을 루트 `Cargo.toml`에서 일괄 관리하여 하위 패키지의 설정을 단순화합니다.

---

## 2. 디렉토리 구조 재편 (Symmetric Flat Layout)

```text
├── Cargo.toml (Workspace Manifest & Inheritance)
├── tree-sitter-inspector/ (MCP Host Crate)
│   ├── Cargo.toml
│   └── src/ (inspect.rs, dump.rs, parser.rs, config.rs, mcp.rs, main.rs)
├── wasmtime-compiler/ (AOT Compiler Crate)
│   ├── Cargo.toml
│   └── src/main.rs
├── resources/wasm/ (WASM Grammars & languages.json)
└── docs/superpowers/specs/ (Architecture Specs)
```

---

## 3. Cargo Workspace & 상속 명세

### 루트 [Cargo.toml](file:///Users/zaeku/workspace/Tools for Agents/tree-sitter-inspector-rs/Cargo.toml)
```toml
[workspace]
members = [
    "tree-sitter-inspector",
    "wasmtime-compiler"
]

[workspace.package]
version = "0.1.0"
edition = "2021"
authors = ["Antigravity Developer"]

[workspace.dependencies]
wasmtime = "22.0.0"
tokio = { version = "1.36", features = ["full"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
anyhow = "1.0"
sha2 = "0.10"
hex = "0.4"
```

### 호스트 MCP [tree-sitter-inspector/Cargo.toml](file:///Users/zaeku/workspace/Tools for Agents/tree-sitter-inspector-rs/tree-sitter-inspector/Cargo.toml)
```toml
[package]
name = "tree-sitter-inspector"
version.workspace = true
edition.workspace = true
authors.workspace = true

[dependencies]
wasmtime = { workspace = true, default-features = false, features = ["runtime", "async", "cache", "gc", "component-model", "threads"] }
tokio = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
anyhow = { workspace = true }
sha2 = { workspace = true }
hex = { workspace = true }
once_cell = "1.21.4"
log = "0.4"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

---

## 4. Pure WASM 파싱 런타임 설계 (FFI Bridge)

가상 머신(Wasmtime) 내부의 가상 메모리 공간 주소 격리로 인해, 문법 파일은 호스트 tree-sitter 파서에 결합될 수 없습니다. 따라서 Wasmtime 인스턴스 안에서 파싱 연산을 전체 수행합니다.

### 1) WASM 모듈 링킹 및 메모리 설계
- 각 언어별 `tree-sitter-*.wasm` 모듈은 문법 테이블을 담고 있으며, C-FFI 함수 `const TSLanguage *tree_sitter_<lang>(void)` 를 export 합니다.
- 호스트는 Wasmtime 가상 머신 구동 시, 파서 코어가 포함된 **WASM 런타임 모듈**을 함께 로드하여 링킹하거나, 가상머신에 적절한 메모리 공유 함수를 주입합니다.

### 2) 파싱 및 S-expression 변환 FFI 흐름
1. **메모리 확보**:
   호스트는 파싱할 소스코드 스트링을 Wasmtime 런타임 인스턴스의 메모리에 할당(write)합니다.
2. **파서 실행**:
   가상 머신 내의 `ts_parser_parse_string` FFI를 구동하여 가상메모리 상에 구문 트리(AST)를 구축합니다.
3. **S-expression 직렬화**:
   WASM 인스턴스의 `ts_node_string` 함수를 호출하여 메모리 내에 S-expression 텍스트 주소값을 반환받습니다.
4. **호스트 복사**:
   반환받은 가상메모리 상의 C-string 주소 영역을 읽어(read) 호스트 Rust String으로 복사해 냅니다.
