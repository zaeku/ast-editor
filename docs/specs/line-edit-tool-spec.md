# Line-Level Code Editing Specification for tree-sitter-inspector (Rust)

## 1. 개요 및 목적
본 명세서는 LLM 에이전트가 코드 파일을 편집할 때 발생하는 대용량 입출력 토큰 낭비와 재작성 시 유입되는 문법적/포맷 오류(들여쓰기, 오타 등)를 최소화하기 위해 고안된 **라인 단위 트랜잭셔널 편집 도구**의 설계를 정의합니다. 

본 도구는 Rust 기반의 `tree-sitter-inspector-rs` 프로젝트에 통합되어, 코드 구조 분석(Inspection)과 코드 수정(Editing)을 단일 Rust MCP 서버 환경에서 유기적으로 처리할 수 있도록 돕습니다.

---

## 2. 핵심 설계 개념 (Core Concepts)

### A. SQLite 기반 단일 캐시 DB 및 라이프사이클 관리
*   **단일 파일 기반 캐시**: 개별 파일 세션마다 프로세스 메모리(RAM)에 독립적인 인메모리 DB를 띄우는 대신, **단일 로컬 SQLite 데이터베이스 파일(`~/.cache/line-editor/sessions.db`)**을 생성하여 모든 세션 정보를 통합 관리합니다.
*   **프로세스 간 세션 유지**: MCP(Model Context Protocol) 도구 호출은 기본적으로 비상태성(Stateless) 독립 프로세스 실행 구조를 가집니다. 따라서 영속적인 단일 SQLite 파일을 활용함으로써 도구 호출이 끝난 뒤나 프로세스가 재시작되더라도 편집 세션 상태를 안정적으로 보존할 수 있습니다.
*   **성능 최적화**: 디스크 I/O 병목을 줄이기 위해 SQLite에 `PRAGMA journal_mode = WAL;` 및 `PRAGMA synchronous = NORMAL;`, 적절한 캐싱 Pragma를 설정하여 실질적으로 **인메모리 DB 수준의 초고속(Sub-millisecond) 연산**을 구현합니다.
*   **세션 재사용 (Reuse)**: `init_edit_session` 요청 시점에 이미 캐시 DB에 해당 파일의 세션이 존재하고, 디스크 파일의 수정 시각(`mtime`) 및 해시가 일치한다면 기존 세션 데이터를 그대로 재사용하여 중복 파싱 오버헤드를 제거합니다.
*   **세션 만료 정책 (Garbage Collection)**: 세션 테이블에 외래키(`ON DELETE CASCADE`)와 `last_accessed_at` 타임스탬프를 유지합니다. 세션을 새로 열 때마다 마지막 접근 시각 기준 **30분이 초과된 미사용 세션**을 단일 SQL 쿼리로 자동 폐기(GC)하여 디스크 공간을 관리합니다. 30분 TTL은 활발한 작업 흐름과 유휴 리소스 정리 간의 균형점입니다.

### B. 고유 라인 식별자 (Line ID)
*   **ID 포맷**: `<Sequence ID (HEX)>#<Line Content Hash (앞 4글자)>`
*   **유일성 보장**: 라인별로 순차 증가하는 고유 ID가 앞부분에 오므로 해시 충돌 대응 로직이 필요치 않으며 완벽한 유일성이 보장됩니다.
*   **환각 방지용 체크섬 (Safety Checksum)**: 뒷부분의 라인 해시 값은 LLM이 잘못된 대상을 지정하거나 환각(Hallucination)하는 것을 막아주는 안전장치 역할을 합니다. 에이전트가 편집 요청 시 제시한 해시와 DB 내 실제 라인의 해시가 일치할 때만 수정을 허용합니다.
*   **불변성**: 라인이 삽입되거나 삭제되더라도 다른 라인들의 고유 식별자 ID는 영향을 받지 않고 그대로 유지됩니다.

### C. 논리적 정렬 가중치 (Index-Shifting 방지)
*   DB 내부 테이블에 `sort_order` (`REAL` 타입) 컬럼을 관리합니다.
*   두 라인 사이에 새로운 라인을 삽입할 때, 전체 라인의 인덱스를 재정렬(Shift)하는 대신 앞 라인의 정렬 값과 뒷 라인의 정렬 값의 중간값(예: `(A.sort_order + B.sort_order) / 2.0`)을 새 라인에 부여하여 논리적 순서를 보존합니다.

### D. 읽기 선행 우회 및 컨텍스트 프리뷰 반환 (No-Read Optimization)
*   에이전트가 `apply_line_edits` 호출을 통해 편집을 성공적으로 마치면, 툴은 변경 내역을 파일에 커밋한 직후 **수정 또는 삽입이 일어난 줄 전후 2라인의 컨텍스트를 포함하는 변경 결과 프리뷰**를 반환합니다.
*   이때 반환 포맷은 `view_session_lines`와 완전히 동일한 `LINE | LINE ID | CODE` 표 형식을 취하며, 줄 번호는 DB 상의 전체 정렬 상태를 기준으로 **완벽히 재계산된 1-indexed 라인 번호**를 출력합니다.
*   이를 통해 에이전트는 줄 삽입/삭제로 인해 물리적 줄 번호가 어떻게 밀렸는지(Index shifting) 및 수정된 줄의 고유 ID가 무엇인지 즉시 인지하여, 다시 파일을 조회하지 않고 다음 연쇄 편집을 이어나갈 수 있습니다.

### E. 핀포인트 뷰 모드 (Pinpoint View Mode)
*   **컨텍스트 최소화**: 세션이 생성될 때 파일의 전체 텍스트와 ID를 한 번에 LLM에게 노출하지 않습니다.
*   에이전트는 사전에 `tree-sitter-inspector` 등을 사용하여 알아낸 편집 대상 범위(StartLine ~ EndLine)만 `view_session_lines` 도구를 통해 핀포인트로 조회하여 ID 목록을 획득함으로써 토큰을 크게 절약합니다.

### F. 지연(Lazy) 및 비동기 라인 해싱 (Asymmetric Hashing)
*   **지연 해싱**: 대용량 파일 파싱 시 발생하는 연산 지연을 제거하기 위해, `init_edit_session` 시점에는 라인의 해시를 계산하지 않고 `NULL` 상태로 데이터베이스에 삽입합니다.
*   **온디맨드 계산**: 에이전트가 `view_session_lines` 또는 `apply_line_edits`로 특정 범위를 요청하는 시점에, 해당 영역의 라인들만 우선적으로 해시를 계산해 캐시를 채우고 응답합니다.
*   **비동기 병렬 계산**: 세션 생성 직후, 백그라운드 스레드에서 파일의 나머지 잔여 라인들의 해시를 순차적으로 계산하여 DB에 지속 반영해 둡니다.

### G. 도구 응답 내 후속 행동 가이드(Footnote JIT Tip) 탑재
*   **행동 제어**: 에이전트가 도구 결과를 수신했을 때, 매 턴마다 다음에 어떤 행동이나 도구를 실행해야 할지 명시적인 힌트(Markdown 인용 블록 팁)를 함께 출력하여 최적의 편집 경로 이탈을 막아줍니다.
*   **적용 기준**:
    1. `tree_sitter_inspect` / `tree_sitter_dump_tree` (코드 내용이 포함된 경우):
       *   `> [TIP] You can edit these lines directly using the 'apply_line_edits' tool with the line IDs shown above.`
    2. `tree_sitter_inspect` (아웃라인/구조만 포함된 경우):
       *   `> [TIP] Use 'view_session_lines' with a target line range to view their code contents and line IDs before editing.`
    3. `view_session_lines` (라인 목록):
       *   `> [TIP] Edit these lines by calling 'apply_line_edits' with the line IDs (e.g. 1a#f8c9) shown above.`

---

## 3. 도구 API 사양 (MCP Tools Spec)

### A. `init_edit_session`
*   **설명**: 대상 파일을 읽어 캐시 DB에 라인별로 적재하고 세션을 등록합니다. 파일 내용 전체를 출력하지 않고 요약 정보만 응답하여 토큰 낭비를 예방합니다.
*   **인자**:
    *   `filepath` (string, required): 읽을 파일의 절대 경로
    *   `create_if_not_exists` (boolean, optional, default: false): 해당 경로에 파일이 없을 경우 신규 생성 여부
*   **파일 중복 및 안전성 정책**:
    *   `create_if_not_exists`에 `true`를 전달했더라도 **해당 경로에 이미 파일이 존재하면 기존 파일을 덮어쓰거나(Overwrite) 비우지(Truncate) 않고**, 기존 파일 내용을 안전하게 읽어서 세션을 시작(O_CREAT 동작 모델)합니다.
*   **지원 외 파일 대체 작동(Fallback)**:
    *   지원 대상 21개 확장자 외의 파일(예: `.gitignore`, `.env`, `.md` 등)에 대한 요청도 **에러 없이 정상적으로 수용**하여 라인 편집기 기능을 쓸 수 있게 합니다.
    *   단, 세션 응답에 `warning: "Unsupported file format. Syntax validation is disabled."` 메타데이터를 함께 반환하고, 해당 세션에 대해서는 문법 검사 단계를 우회합니다.
*   **바이너리 파일 차단 정책**:
    *   이미지, PDF, 압축파일, 컴파일된 목적코드/실행파일 등 비텍스트성 바이너리 파일임이 판별되면(첫 수 킬로바이트 데이터에서 null 바이트 `\0`를 탐색하는 등의 방식으로 검출) 세션 조회를 거절하고 즉시 `BINARY_FILE_ERROR` 에러를 반환하여 소스 오염을 사전에 방지합니다.
*   **반환 포맷 (JSON)**:
    ```json
    {
      "status": "success",
      "session_id": "8f3a8b23",
      "total_lines": 420,
      "file_hash": "a1b2c3d4",
      "mtime": "2026-07-08T22:07:07Z",
      "is_supported": true
    }
    ```

### B. `view_session_lines`
*   **설명**: 세션이 활성화된 파일의 특정 줄 범위만 타겟팅하여 ID가 부여된 코드 내용을 반환합니다. (`view_file` 도구와 동일한 형태의 입출력 방식을 가집니다.)
*   **인자**:
    *   `filepath` (string, required): 대상 파일의 절대 경로
    *   `start_line` (integer, required, 1-indexed): 조회를 시작할 줄 번호
    *   `end_line` (integer, required, 1-indexed): 조회를 마칠 줄 번호 (포함)
*   **반환 포맷**:
    *   최상단에 `LINE | LINE ID | CODE` 헤더를 1회 출력하고, 각 라인은 토큰 절약 및 가독성을 위해 간결한 3열 표 형태를 유지합니다.
    *   *예시*:
        ```text
        LINE | LINE ID | CODE
        120 | 1f#a1b2 | const express = require('express');
        121 | 20#f8c9 | const app = express();
        122 | 21#3d1e | app.listen(3000);
        
        > [TIP] Edit these lines by calling 'apply_line_edits' with the line IDs (e.g. 1a#f8c9) shown above.
        ```

### C. `apply_line_edits`
*   **설명**: 여러 개의 편집 작업을 하나의 트랜잭션으로 처리하고, 문법 무결성 검증 통과 시 디스크에 반영합니다.
*   **인자**:
    *   `filepath` (string, required): 편집할 파일의 절대 경로
    *   `edits` (array, required): 편집 명령어 목록
        *   `op` (enum: `"update"`, `"insert_after"`, `"insert_before"`, `"delete"`): 연산 종류
        *   `target_id` (string, nullable): 연산의 기준이 되는 라인의 고유 ID (예: `"1b#f8c9"`). 대상 파일이 비어있는 경우 `null` 또는 `""` 허용.
        *   `content` (string, optional): 삽입 또는 수정할 텍스트 내용 (여러 줄 가능)
*   **빈 파일 및 첫 줄 삽입 지원**:
    *   세션을 연 대상 파일이 완전히 비어있어 기준 라인이 없을 경우, `target_id`를 `null` 혹은 빈 문자열(`""`)로 설정하여 `insert_after` 또는 `insert_before` 연산을 요청하면 파일의 첫 줄로 데이터가 삽입됩니다.
*   **반환 데이터**:
    *   편집 완료 후, **수정된 영역을 둘러싼 전후 2라인의 컨텍스트 프리뷰**를 `LINE | LINE ID | CODE` 테이블 구조로 즉시 출력합니다. 이 프리뷰의 줄 번호는 최신 상태로 동적 재계산되어 표현됩니다.
*   **트랜잭션 및 검증 흐름**:
    1. **트랜잭션 시작**: SQLite `BEGIN TRANSACTION;`
    2. **식별자 및 해시 검증**: 요청된 `target_id`가 지정된 경우, 앞부분(HEX ID)을 조회한 뒤 뒷부분의 해시 체크섬과 실제 DB 내 라인의 현재 해시가 일치하는지 비교 검증합니다. 불일치 시 트랜잭션을 중단하고 환각 방지 에러를 반환합니다. (단, `target_id`가 null인 신규 삽입은 이 단계를 건너뜁니다.)
    3. **임시 갱신**: 요청된 연산들을 테이블 상에 실행하여 `sort_order` 가중치를 부여하고 내용 수정.
    4. **가상 파일 생성**: 메모리 상에서 정렬된 상태의 가상 파일 전체 텍스트 버퍼를 병합해 냅니다.
    5. **문법 검사**:
        *   세션 파일 형식이 지원되지 않는 형식일 경우 **검사를 우회(Bypass)하여 성공으로 간주**합니다.
        *   지원 형식인 경우, `tree-sitter-inspector`의 Rust 내부 WASM 파서 엔진을 통해 AST 파싱을 검사하여 `ERROR` 또는 `MISSING` 노드가 존재할 시 **검증 실패**로 처리합니다.
    6. **결과 처리**:
        *   **검증 성공**: 디스크 파일에 실제 내용을 기록하고 `COMMIT;` 실행.
        *   **검증 실패**: 트랜잭션을 `ROLLBACK;`하고 구체적인 문법 에러 메시지와 실패한 행 번호를 에러 피드백으로 반환.

---

## 5. tree-sitter-inspector 연동 및 선행 캐싱 (Integration)

*   **배포 모델**: 본 편집 도구는 별도의 MCP 서버를 띄우지 않고, 기존 `tree-sitter-inspector` MCP 서버에 통합하여 배포합니다.
*   **신규 도구 통합**:
    *   기존 도구: `tree_sitter_inspect`, `tree_sitter_dump_tree`
    *   추가 도구: `init_edit_session`, `view_session_lines`, `apply_line_edits`
*   **선행 캐싱 및 ID 노출 연동**:
    *   에이전트가 `tree_sitter_inspect` 또는 `tree_sitter_dump_tree`를 통해 특정 코드 구조를 조회할 때, 서버는 백그라운드에서 해당 파일에 대한 **`init_edit_session` 논리를 자동으로 작동**시켜 SQLite DB 세션을 사전 초기화합니다.
    *   이후 inspector가 결과 소스 코드 행을 화면에 출력할 때, 기존의 일반 라인 넘버 대신 **본 명세의 고유 라인 ID(예: `1b#f8c9`)를 라인 앞에 부착하여 출력**합니다.
    *   이를 통해 에이전트는 조회(Inspect) 단계에서 획득한 고유 라인 ID를 가지고 별도의 세션 초기화 단계를 건너뛴 채 즉시 `apply_line_edits`로 연동 진입할 수 있게 됩니다.
*   **지원 범위 (21개 확장자)**:
    *   `tree-sitter-inspector`가 WASM을 통해 지원하고 있는 모든 언어 규격을 동일하게 공유합니다:
    *   `.py` (Python), `.js`/`.jsx`/`.ts`/`.tsx` (JS/TS), `.go` (Go), `.rs` (Rust), `.java` (Java), `.cpp`/`.cc`/`.cxx`/`.c`/`.h` (C/C++), `.lua` (Lua), `.html`/`.htm` (HTML), `.json` (JSON), `.yaml`/`.yml` (YAML), `.toml` (TOML), `.swift` (Swift).
