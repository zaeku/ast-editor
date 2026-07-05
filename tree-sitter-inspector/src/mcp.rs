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
                        "name": "tree-sitter-inspector-rs",
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
        assert_eq!(tools.len(), 2);
        
        let tool_names: Vec<&str> = tools.iter().map(|t| t.get("name").unwrap().as_str().unwrap()).collect();
        assert!(tool_names.contains(&"tree_sitter_inspect"));
        assert!(tool_names.contains(&"tree_sitter_dump_tree"));
    }

    #[tokio::test]
    async fn test_mcp_tools_call_inspect_missing_wasm() {
        let fixture = TempTestFixture::new("test_tools_call_inspect_fail");
        let server = create_test_server(&fixture);

        // Create a mock source file to inspect
        let test_file = fixture.dir.join("test.rs");
        fs::write(&test_file, "fn main() {}\n").unwrap();

        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": "tree_sitter_inspect",
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
        assert!(error.message.contains("Tool execution failed for 'tree_sitter_inspect'"));

        let data = error.data.unwrap();
        println!("DEBUG: inspect_missing_wasm data = {:?}", data);
        assert!(data.get("cause").is_some());
        
        let suggestion = data.get("suggestion").unwrap().as_str().unwrap();
        assert!(suggestion.contains("Please ensure the raw .wasm parser files exist"));
        
        let wasm_dir = data.get("wasm_dir").unwrap().as_str().unwrap();
        assert!(wasm_dir.contains("wasm"));
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
                "name": "tree_sitter_dump_tree",
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
}

