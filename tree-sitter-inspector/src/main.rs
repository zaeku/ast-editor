use tree_sitter_inspector::parser::ParserManager;
use tree_sitter_inspector::mcp::{McpServer, JsonRpcRequest};
use tracing::{info, error, debug};
use tracing_subscriber::EnvFilter;
use tokio::io::{stdin, stdout, AsyncBufReadExt, BufReader, AsyncWriteExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing to stderr so it doesn't pollute stdout (since stdout is used for JSON-RPC transport)
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .init();

    info!("Bootstrapping tree-sitter-inspector-rs...");
    let parser_manager = ParserManager::new()?;
    let mcp_server = McpServer::new(parser_manager);
    info!("McpServer & Headless Wasmtime initialized successfully.");

    let stdin = stdin();
    let mut reader = BufReader::new(stdin).lines();
    let mut stdout = stdout();

    info!("Ready for JSON-RPC messages via Stdin.");
    while let Some(line) = reader.next_line().await? {
        let line_trimmed = line.trim();
        if line_trimmed.is_empty() {
            continue;
        }

        debug!("Received raw message: {}", line_trimmed);
        
        let request: JsonRpcRequest = match serde_json::from_str(line_trimmed) {
            Ok(req) => req,
            Err(e) => {
                error!("Failed to parse JSON-RPC request: {:?}", e);
                let err_response = serde_json::json!({
                    "jsonrpc": "2.0",
                    "error": {
                        "code": -32700,
                        "message": format!("Parse error: {}", e)
                    },
                    "id": null
                });
                let resp_str = serde_json::to_string(&err_response)? + "\n";
                stdout.write_all(resp_str.as_bytes()).await?;
                stdout.flush().await?;
                continue;
            }
        };

        // Handle request and generate response
        let response = mcp_server.handle_request(request).await;
        
        // Write response back to stdout
        if response.id.is_some() || response.error.is_some() || response.result.is_some() {
            let resp_str = serde_json::to_string(&response)? + "\n";
            stdout.write_all(resp_str.as_bytes()).await?;
            stdout.flush().await?;
        }
    }

    info!("tree-sitter-inspector-rs Stdio stream closed.");
    Ok(())
}
