use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, error};
use std::sync::Arc;

use crate::parser::ParserManager;
use crate::tools::ToolDispatcher;

#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub method: String,
    pub params: Option<Value>,
    pub id: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcError {
    pub fn new(code: i32, message: &str) -> Self {
        Self {
            code,
            message: message.to_string(),
            data: None,
        }
    }

    pub fn with_data(code: i32, message: &str, data: Value) -> Self {
        Self {
            code,
            message: message.to_string(),
            data: Some(data),
        }
    }

    pub fn method_not_found(method: &str) -> Self {
        Self::new(-32601, &format!("Method not found: {}", method))
    }

    pub fn invalid_params(msg: &str) -> Self {
        Self::new(-32602, msg)
    }

    pub fn internal_error(msg: &str) -> Self {
        Self::new(-32603, msg)
    }
}

pub struct McpServer {
    parser_manager: Arc<ParserManager>,
    dispatcher: ToolDispatcher,
}

impl McpServer {
    pub fn new(parser_manager: ParserManager) -> Self {
        Self {
            parser_manager: Arc::new(parser_manager),
            dispatcher: ToolDispatcher::new(),
        }
    }

    pub async fn handle_request(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        let req_id = request.id.clone();
        
        // Ensure protocol version is 2.0
        if request.jsonrpc != "2.0" {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                result: None,
                error: Some(JsonRpcError::new(-32600, "Invalid JSON-RPC version. Expected '2.0'.")),
                id: req_id,
            };
        }

        match request.method.as_str() {
            "initialize" => {
                debug!("Received initialize handshake request");
                // Reply with server capabilities
                let response_result = serde_json::json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {
                        "tools": {}
                    },
                    "serverInfo": {
                        "name": "ast-editor",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                });
                JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    result: Some(response_result),
                    error: None,
                    id: req_id,
                }
            }
            "notifications/initialized" | "initialized" => {
                // Initialized notification has no response (Notification)
                debug!("Received initialized notification");
                JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    result: None,
                    error: None,
                    id: None,
                }
            }
            "tools/list" => {
                debug!("Received tools/list request");
                let tools_list = self.dispatcher.list_tools();
                JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    result: Some(serde_json::json!({ "tools": tools_list })),
                    error: None,
                    id: req_id,
                }
            }
            "tools/call" => {
                debug!("Received tools/call request");
                let params = match request.params {
                    Some(p) => p,
                    None => {
                        return JsonRpcResponse {
                            jsonrpc: "2.0".to_string(),
                            result: None,
                            error: Some(JsonRpcError::invalid_params("Missing params for tools/call")),
                            id: req_id,
                        };
                    }
                };

                let name = match params.get("name").and_then(|v| v.as_str()) {
                    Some(n) => n,
                    None => {
                        return JsonRpcResponse {
                            jsonrpc: "2.0".to_string(),
                            result: None,
                            error: Some(JsonRpcError::invalid_params("Missing or invalid 'name' in tools/call params")),
                            id: req_id,
                        };
                    }
                };

                let arguments = params.get("arguments").cloned().unwrap_or(Value::Object(serde_json::Map::new()));

                match self.dispatcher.call_tool(name, arguments, &self.parser_manager).await {
                    Ok(result) => JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        result: Some(result),
                        error: None,
                        id: req_id,
                    },
                    Err(e) => {
                        error!("Tool call failed for '{}': {:?}", name, e);
                        let err_str = format!("{:?}", e);
                        let mut err_data = serde_json::Map::new();
                        err_data.insert("cause".to_string(), Value::String(err_str.clone()));
                        
                        if err_str.contains("languages.json") || err_str.contains("Configuration file 'languages.json' is missing") {
                            let expected_path = crate::config::get_wasm_dir().join("languages.json");
                            err_data.insert("missing_file".to_string(), Value::String(expected_path.to_string_lossy().to_string()));
                            err_data.insert("suggestion".to_string(), Value::String(
                                "Configuration file 'languages.json' is missing or corrupted.\n\
                                 Please create a valid 'languages.json' in your raw wasm directory.\n\
                                 Format (JSON):\n\
                                 {\n\
                                   \"rust\": {\n\
                                     \"extensions\": [\".rs\"],\n\
                                     \"wasm_file\": \"tree-sitter-rust.wasm\"\n\
                                   }\n\
                                 }".to_string()
                            ));
                        } else if err_str.contains("Failed to open raw wasm file") || err_str.contains("Raw Wasm file not found") || (err_str.contains("tree-sitter-") && err_str.contains(".wasm")) {
                            err_data.insert("wasm_dir".to_string(), Value::String(crate::config::get_wasm_dir().to_string_lossy().to_string()));
                            err_data.insert("suggestion".to_string(), Value::String(
                                "Please ensure the raw .wasm parser files exist in the raw WASM directory.".to_string()
                            ));
                        } else {
                            err_data.insert("suggestion".to_string(), Value::String("Check files, environment paths, and compilation logs.".to_string()));
                        }

                        JsonRpcResponse {
                            jsonrpc: "2.0".to_string(),
                            result: None,
                            error: Some(JsonRpcError::with_data(
                                -32603,
                                &format!("Tool execution failed for '{}': {}", name, e),
                                Value::Object(err_data)
                            )),
                            id: req_id,
                        }
                    }
                }
            }
            _ => {
                // If it is a notification (no id), we do not return an error, just ignore
                if req_id.is_none() {
                    return JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        result: None,
                        error: None,
                        id: None,
                    };
                }
                
                JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    result: None,
                    error: Some(JsonRpcError::method_not_found(&request.method)),
                    id: req_id,
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::await_holding_lock, clippy::bool_assert_comparison)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    struct TempTestFixture {
        dir: PathBuf,
    }

    impl TempTestFixture {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join("tree_sitter_inspector_mcp_tests").join(name);
            if dir.exists() {
                let _ = fs::remove_dir_all(&dir);
            }
            fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }
    }

    impl Drop for TempTestFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn create_test_server(fixture: &TempTestFixture) -> McpServer {
        crate::config::clear_langs_map_for_testing();
        let cache_dir = fixture.dir.join("cache");
        let compiler_path = fixture.dir.join("compiler");
        let wasm_dir = fixture.dir.join("wasm");
        fs::create_dir_all(&cache_dir).unwrap();
        fs::create_dir_all(&wasm_dir).unwrap();

        let langs_json_path = wasm_dir.join("languages.json");
        let mock_config = serde_json::json!({
            "rust": {
                "extensions": [".rs"],
                "wasm_file": "tree-sitter-rust.wasm"
            },
            "python": {
                "extensions": [".py"],
                "wasm_file": "tree-sitter-python.wasm"
            }
        });
        fs::write(&langs_json_path, serde_json::to_string(&mock_config).unwrap()).unwrap();

        let pm = ParserManager::with_paths(cache_dir, compiler_path, wasm_dir).unwrap();
        McpServer::new(pm)
    }

    #[tokio::test]
    async fn test_mcp_initialize() {
        let fixture = TempTestFixture::new("test_initialize");
        let server = create_test_server(&fixture);

        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "initialize".to_string(),
            params: Some(serde_json::json!({
                "protocolVersion": "2024-11-05",
                "clientInfo": { "name": "test-client", "version": "1.0.0" }
            })),
            id: Some(serde_json::json!(1)),
        };

        let resp = server.handle_request(req).await;
        assert_eq!(resp.jsonrpc, "2.0");
        assert!(resp.error.is_none());
        assert_eq!(resp.id, Some(serde_json::json!(1)));

        let result = resp.result.unwrap();
        assert_eq!(result.get("protocolVersion").unwrap().as_str().unwrap(), "2024-11-05");
        assert!(result.get("capabilities").unwrap().get("tools").is_some());
    }

    #[tokio::test]
    async fn test_mcp_tools_list() {
        let fixture = TempTestFixture::new("test_tools_list");
        let server = create_test_server(&fixture);

        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "tools/list".to_string(),
            params: None,
            id: Some(serde_json::json!("abc")),
        };

        let resp = server.handle_request(req).await;
        assert_eq!(resp.id, Some(serde_json::json!("abc")));
        assert!(resp.error.is_none());

        let result = resp.result.unwrap();
        let tools = result.get("tools").unwrap().as_array().unwrap();
        assert_eq!(tools.len(), 5);
        
        let tool_names: Vec<&str> = tools.iter().map(|t| t.get("name").unwrap().as_str().unwrap()).collect();
        assert!(tool_names.contains(&"inspect_ast"));
        assert!(tool_names.contains(&"dump_ast"));
        assert!(tool_names.contains(&"view_lines"));
        assert!(tool_names.contains(&"edit_lines"));
        assert!(tool_names.contains(&"create_lines"));

        // Verify updated template enum in inspect_ast tool schema
        let inspect_tool = tools.iter().find(|t| t.get("name").unwrap().as_str().unwrap() == "inspect_ast").unwrap();
        let schema = inspect_tool.get("inputSchema").unwrap();
        let props = schema.get("properties").unwrap();
        let template = props.get("template").unwrap();
        let enum_vals = template.get("enum").unwrap().as_array().unwrap();
        let enum_strs: Vec<&str> = enum_vals.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(enum_strs.contains(&"headings"));
        assert!(enum_strs.contains(&"headers"));
        assert!(enum_strs.contains(&"codeblocks"));
        assert!(enum_strs.contains(&"code_blocks"));
        assert!(enum_strs.contains(&"links"));
        assert!(enum_strs.contains(&"tables"));
        assert!(enum_strs.contains(&"lists"));
    }

    #[tokio::test]
    async fn test_mcp_tools_call_inspect_missing_wasm() {
        let _lock = crate::tools::TEST_DB_LOCK.lock().unwrap();
        let fixture = TempTestFixture::new("test_tools_call_inspect_fail");
        let server = create_test_server(&fixture);

        // Create a mock source file to inspect
        let test_file = fixture.dir.join("test.rs");
        fs::write(&test_file, "fn main() {}\n").unwrap();

        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": "inspect_ast",
                "arguments": {
                    "file": test_file.to_str().unwrap()
                }
            })),
            id: Some(serde_json::json!(42)),
        };

        let resp = server.handle_request(req).await;
        assert_eq!(resp.id, Some(serde_json::json!(42)));
        assert!(resp.result.is_none());
        assert!(resp.error.is_some());

        let error = resp.error.unwrap();
        assert_eq!(error.code, -32603);
        assert!(error.message.contains("Tool execution failed for 'inspect_ast'"));

        let data = error.data.unwrap();
        println!("DEBUG: inspect_missing_wasm data = {:?}", data);
        assert!(data.get("cause").is_some());
        
        let suggestion = data.get("suggestion").unwrap().as_str().unwrap();
        assert!(suggestion.contains("Please ensure the raw .wasm parser files exist"));
        
        let wasm_dir = data.get("wasm_dir").unwrap().as_str().unwrap();
        assert!(wasm_dir.contains("wasm"));
    }

    #[tokio::test]
    async fn test_mcp_tools_call_inspect_success() {
        let _lock = crate::tools::TEST_DB_LOCK.lock().unwrap();
        let fixture = TempTestFixture::new("test_inspect_success");
        let server = create_test_server(&fixture);

        // Copy real rust wasm so that compilation succeeds
        let manifest_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
        let real_wasm_path = manifest_dir.join("resources").join("wasm").join("tree-sitter-rust.wasm");
        let target_wasm_path = fixture.dir.join("wasm").join("tree-sitter-rust.wasm");
        if real_wasm_path.exists() {
            fs::copy(&real_wasm_path, &target_wasm_path).unwrap();
        }

        let test_file = fixture.dir.join("test.rs");
        fs::write(&test_file, "pub fn hello() {}\npub fn world() {}\n").unwrap();

        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": "inspect_ast",
                "arguments": {
                    "file": test_file.to_str().unwrap(),
                    "template": "functions"
                }
            })),
            id: Some(serde_json::json!(101)),
        };

        let resp = server.handle_request(req).await;
        assert_eq!(resp.id, Some(serde_json::json!(101)));
        assert!(resp.error.is_none());
        
        let result = resp.result.unwrap();
        let content = result.get("content").unwrap().as_array().unwrap();
        assert_eq!(content.len(), 1);
        
        let content_item = &content[0];
        assert_eq!(content_item.get("type").unwrap().as_str().unwrap(), "text");
        
        let text_raw = content_item.get("text").unwrap().as_str().unwrap();
        let inspect_res: serde_json::Value = serde_json::from_str(text_raw).unwrap();
        
        assert_eq!(inspect_res.get("status").unwrap().as_str().unwrap(), "success");
        assert_eq!(inspect_res.get("language").unwrap().as_str().unwrap(), "rust");
        assert_eq!(inspect_res.get("match_count").unwrap().as_u64().unwrap(), 2);
        
        let matches = inspect_res.get("matches").unwrap().as_array().unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].get("capture_name").unwrap().as_str().unwrap(), "function");
    }

    #[tokio::test]
    async fn test_mcp_tools_call_inspect_unsupported_template_warning() {
        let _lock = crate::tools::TEST_DB_LOCK.lock().unwrap();
        let fixture = TempTestFixture::new("test_inspect_warning");
        let server = create_test_server(&fixture);

        // Copy real rust wasm
        let manifest_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
        let real_wasm_path = manifest_dir.join("resources").join("wasm").join("tree-sitter-rust.wasm");
        let target_wasm_path = fixture.dir.join("wasm").join("tree-sitter-rust.wasm");
        if real_wasm_path.exists() {
            fs::copy(&real_wasm_path, &target_wasm_path).unwrap();
        }

        let test_file = fixture.dir.join("test.rs");
        fs::write(&test_file, "pub fn hello() {}\n").unwrap();

        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": "inspect_ast",
                "arguments": {
                    "file": test_file.to_str().unwrap(),
                    "template": "invalid_template_name"
                }
            })),
            id: Some(serde_json::json!(102)),
        };

        let resp = server.handle_request(req).await;
        assert_eq!(resp.id, Some(serde_json::json!(102)));
        assert!(resp.error.is_none());
        
        let result = resp.result.unwrap();
        let content = result.get("content").unwrap().as_array().unwrap();
        let text_raw = content[0].get("text").unwrap().as_str().unwrap();
        let inspect_res: serde_json::Value = serde_json::from_str(text_raw).unwrap();
        
        assert_eq!(inspect_res.get("status").unwrap().as_str().unwrap(), "warning");
        assert!(inspect_res.get("hint").unwrap().as_str().unwrap().contains("Template 'invalid_template_name' is not supported"));
    }

    #[tokio::test]
    async fn test_mcp_tools_call_inspect_invalid_query_error() {
        let _lock = crate::tools::TEST_DB_LOCK.lock().unwrap();
        let fixture = TempTestFixture::new("test_inspect_error");
        let server = create_test_server(&fixture);

        // Copy real rust wasm
        let manifest_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
        let real_wasm_path = manifest_dir.join("resources").join("wasm").join("tree-sitter-rust.wasm");
        let target_wasm_path = fixture.dir.join("wasm").join("tree-sitter-rust.wasm");
        if real_wasm_path.exists() {
            fs::copy(&real_wasm_path, &target_wasm_path).unwrap();
        }

        let test_file = fixture.dir.join("test.rs");
        fs::write(&test_file, "pub fn hello() {}\n").unwrap();

        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": "inspect_ast",
                "arguments": {
                    "file": test_file.to_str().unwrap(),
                    "query": "(invalid_s_expression_syntax"
                }
            })),
            id: Some(serde_json::json!(103)),
        };

        let resp = server.handle_request(req).await;
        assert_eq!(resp.id, Some(serde_json::json!(103)));
        assert!(resp.error.is_none());
        
        let result = resp.result.unwrap();
        assert_eq!(result.get("is_error").unwrap().as_bool().unwrap(), true);
        
        let content = result.get("content").unwrap().as_array().unwrap();
        let text_raw = content[0].get("text").unwrap().as_str().unwrap();
        let inspect_res: serde_json::Value = serde_json::from_str(text_raw).unwrap();
        
        assert_eq!(inspect_res.get("status").unwrap().as_str().unwrap(), "error");
        assert!(inspect_res.get("hint").unwrap().as_str().unwrap().contains("Invalid Tree-sitter query S-expression"));
    }

    #[tokio::test]
    async fn test_mcp_tools_call_missing_languages_config() {
        crate::config::clear_langs_map_for_testing();
        // Create a fixture without writing a languages.json in the wasm directory
        let fixture = TempTestFixture::new("test_tools_call_missing_langs");
        let server = create_test_server(&fixture);

        // Explicitly remove languages.json to trigger configuration missing error
        let config_path = fixture.dir.join("wasm").join("languages.json");
        if config_path.exists() {
            fs::remove_file(&config_path).unwrap();
        }
        crate::config::clear_langs_map_for_testing();

        let test_file = fixture.dir.join("test.rs");
        fs::write(&test_file, "struct Point {}\n").unwrap();

        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": "dump_ast",
                "arguments": {
                    "file": test_file.to_str().unwrap()
                }
            })),
            id: Some(serde_json::json!(44)),
        };

        let resp = server.handle_request(req).await;
        assert_eq!(resp.id, Some(serde_json::json!(44)));
        assert!(resp.result.is_none());
        assert!(resp.error.is_some());

        let error = resp.error.unwrap();
        let data = error.data.unwrap();
        
        let suggestion = data.get("suggestion").unwrap().as_str().unwrap();
        assert!(suggestion.contains("Configuration file 'languages.json' is missing or corrupted"));
        
        let missing_file = data.get("missing_file").unwrap().as_str().unwrap();
        assert!(missing_file.contains("languages.json"));
    }

    #[tokio::test]
    async fn test_mcp_tools_call_inspect_output_file_success() {
        let _lock = crate::tools::TEST_DB_LOCK.lock().unwrap();
        let fixture = TempTestFixture::new("test_inspect_output_file");
        let server = create_test_server(&fixture);

        // Copy real rust wasm so that compilation succeeds
        let manifest_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
        let real_wasm_path = manifest_dir.join("resources").join("wasm").join("tree-sitter-rust.wasm");
        let target_wasm_path = fixture.dir.join("wasm").join("tree-sitter-rust.wasm");
        if real_wasm_path.exists() {
            fs::copy(&real_wasm_path, &target_wasm_path).unwrap();
        }

        let test_file = fixture.dir.join("test.rs");
        fs::write(&test_file, "pub fn hello() {}\n").unwrap();

        // Let's call the tool with output_file: true
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": "inspect_ast",
                "arguments": {
                    "file": test_file.to_str().unwrap(),
                    "template": "functions",
                    "output_file": true
                }
            })),
            id: Some(serde_json::json!(201)),
        };

        let resp = server.handle_request(req).await;
        assert_eq!(resp.id, Some(serde_json::json!(201)));
        assert!(resp.error.is_none());

        let result = resp.result.unwrap();
        let content = result.get("content").unwrap().as_array().unwrap();
        let text_raw = content[0].get("text").unwrap().as_str().unwrap();
        let inspect_res: serde_json::Value = serde_json::from_str(text_raw).unwrap();

        assert_eq!(inspect_res.get("status").unwrap().as_str().unwrap(), "success");

        // The summary returned over stdio should NOT contain the detailed matches array
        assert!(inspect_res.get("matches").is_none());

        // Verify the saved_to_file path exists and contains JSON
        let saved_to_file_path_str = inspect_res.get("saved_to_file").unwrap().as_str().unwrap();
        let saved_path = std::path::Path::new(saved_to_file_path_str);
        assert!(saved_path.exists());

        let file_content = fs::read_to_string(saved_path).unwrap();
        let file_json: serde_json::Value = serde_json::from_str(&file_content).unwrap();
        assert_eq!(file_json.get("status").unwrap().as_str().unwrap(), "success");

        // The actual saved file MUST contain the detailed matches
        assert!(file_json.get("matches").is_some());
        // The actual saved file MUST NOT contain the self-referential saved_to_file field
        assert!(file_json.get("saved_to_file").is_none());

        // Clean up the output file to keep build folder clean
        let _ = fs::remove_file(saved_path);
    }

    #[tokio::test]
    async fn test_mcp_tools_call_inspect_jit_caching_and_formatting() {
        let _lock = crate::tools::TEST_DB_LOCK.lock().unwrap();
        let fixture = TempTestFixture::new("test_inspect_jit");
        let server = create_test_server(&fixture);

        // Copy real rust wasm
        let manifest_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
        let real_wasm_path = manifest_dir.join("resources").join("wasm").join("tree-sitter-rust.wasm");
        let target_wasm_path = fixture.dir.join("wasm").join("tree-sitter-rust.wasm");
        if real_wasm_path.exists() {
            fs::copy(&real_wasm_path, &target_wasm_path).unwrap();
        }

        let test_file = fixture.dir.join("test.rs");
        fs::write(&test_file, "pub fn hello() {\n    let x = 1;\n}\n").unwrap();

        // 1. Call inspect_ast with include_code: true
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": "inspect_ast",
                "arguments": {
                    "file": test_file.to_str().unwrap(),
                    "template": "functions",
                    "include_code": true
                }
            })),
            id: Some(serde_json::json!(301)),
        };

        let resp = server.handle_request(req).await;
        assert!(resp.error.is_none());

        let result = resp.result.unwrap();
        let content = result.get("content").unwrap().as_array().unwrap();
        let text_raw = content[0].get("text").unwrap().as_str().unwrap();
        let inspect_res: serde_json::Value = serde_json::from_str(text_raw).unwrap();

        // Check for JIT tips footnote in the hint field recommending 'edit_lines'
        let hint = inspect_res.get("hint").unwrap().as_str().unwrap();
        assert!(hint.contains("Tip: You can apply edits to this file using the 'edit_lines' tool"));

        // Check formatting of the definition text
        let matches = inspect_res.get("matches").unwrap().as_array().unwrap();
        assert_eq!(matches.len(), 1);
        let def = matches[0].get("definition").unwrap();
        let def_text = def.get("text").unwrap().as_str().unwrap();
        let val: serde_json::Value = serde_json::from_str(def_text).unwrap();
        let lines = val["lines"].as_array().unwrap();
        assert_eq!(lines.len(), 3);
        let line0 = lines[0].as_array().unwrap();
        let line1 = lines[1].as_array().unwrap();
        let line2 = lines[2].as_array().unwrap();
        assert!(line0[0].as_str().unwrap().starts_with("1#"));
        assert!(line1[0].as_str().unwrap().starts_with("2#"));
        assert!(line2[0].as_str().unwrap().starts_with("3#"));
        assert_eq!(line0[2].as_str().unwrap(), "pub fn hello() {");
        assert_eq!(line1[2].as_str().unwrap(), "    let x = 1;");

        // 2. Call inspect_ast with include_code: false
        let req_no_code = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": "inspect_ast",
                "arguments": {
                    "file": test_file.to_str().unwrap(),
                    "template": "functions",
                    "include_code": false
                }
            })),
            id: Some(serde_json::json!(302)),
        };

        let resp_no_code = server.handle_request(req_no_code).await;
        let result_no_code = resp_no_code.result.unwrap();
        let content_no_code = result_no_code.get("content").unwrap().as_array().unwrap();
        let text_raw_no_code = content_no_code[0].get("text").unwrap().as_str().unwrap();
        let inspect_res_no_code: serde_json::Value = serde_json::from_str(text_raw_no_code).unwrap();

        // Check for JIT tips footnote in the hint field recommending 'view_lines'
        let hint_no_code = inspect_res_no_code.get("hint").unwrap().as_str().unwrap();
        assert!(hint_no_code.contains("Tip: You can view line IDs for this file using the 'view_lines' tool"));
    }

    #[tokio::test]
    async fn test_mcp_stateless_gc_behavior() {
        let fixture = TempTestFixture::new("test_gc_behavior");
        let outputs_dir = fixture.dir.join("outputs");
        fs::create_dir_all(&outputs_dir).unwrap();

        let old_file = outputs_dir.join("old_output.json");
        let new_file = outputs_dir.join("new_output.json");

        fs::write(&old_file, "{}").unwrap();
        fs::write(&new_file, "{}").unwrap();

        // Adjust old_file's modified time to 24 hours ago (Mac/Unix touch command)
        let status = std::process::Command::new("touch")
            .args(["-m", "-t", "202001010000", old_file.to_str().unwrap()])
            .status()
            .expect("failed to execute touch command for testing");
        assert!(status.success());

        // Run the GC logic directly on outputs_dir
        crate::tools::inspect::run_gc(outputs_dir.clone()).await;

        // old_file should be deleted (older than 12 hours)
        assert!(!old_file.exists(), "Old file was not cleaned up by GC");
        // new_file should still exist
        assert!(new_file.exists(), "New file was prematurely deleted by GC");
    }
}

