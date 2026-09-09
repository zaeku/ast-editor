use ast_editor::parser::ParserManager;
use ast_editor::mcp::{McpServer, JsonRpcRequest};
use ast_editor::tools::ToolDispatcher;
use std::sync::Arc;
use anyhow::Context;
use tracing::{info, error, debug};
use tracing_subscriber::EnvFilter;
use tokio::io::{stdin, stdout, AsyncBufReadExt, BufReader, AsyncWriteExt};

const USAGE: &str = "\
ast-editor — line-precise editing over tree-sitter

    ast-editor edit <file> < script  apply an edit script read from stdin
    ast-editor skill [topic]         the skill document, or one of its references
    ast-editor <tool> '<json args>'  call one tool and print its output
    ast-editor mcp                   serve MCP over JSON-RPC on stdin
    ast-editor --version             version, and the grammars it can reach
    ast-editor --help                this text

An edit script carries code with no escaping, one directive per line, each
block fenced with three or more backticks:

    ast-editor edit src/config.rs <<'EOF'
    replace_range 3f#a1b2 4c#d3e4 ```
        let a = 1;
    ```
    delete 6b#99aa
    EOF

Tool arguments are the same JSON object the MCP call takes, so anything the
tool schemas describe works here unchanged:

    ast-editor view_lines '{\"filepath\":\"/path/to/file.rs\"}'
    ast-editor edit_lines '{\"filepath\":\"/path/to/file.rs\",\"apply\":\"p1f\"}'
";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        // Serving MCP has to be asked for. It used to be what a bare
        // invocation did, which meant the command form was the exception in a
        // tool whose documented interface is the command form.
        Some("mcp") => {
            init_tracing(tracing::Level::INFO);
            serve_mcp().await
        }
        Some("edit") => {
            init_tracing(tracing::Level::WARN);
            match run_edit_script(&args[1..]).await {
                Ok(output) => {
                    println!("{}", output);
                    Ok(())
                }
                Err(err) => {
                    eprintln!("{:#}", err);
                    std::process::exit(1);
                }
            }
        }
        Some("skill") => {
            match ast_editor::skill::document(args.get(1).map(String::as_str)) {
                Ok(text) => {
                    print!("{}", text);
                    Ok(())
                }
                Err(err) => {
                    eprintln!("{:#}", err);
                    std::process::exit(2);
                }
            }
        }
        Some("--version") | Some("-V") | Some("version") => {
            print!("{}", version_report());
            Ok(())
        }
        Some("--help") | Some("-h") | Some("help") => {
            print!("{}", USAGE);
            println!("\nTools:  {}", tool_names().join(", "));
            println!("Topics: {}", ast_editor::skill::topics().join(", "));
            Ok(())
        }
        None => {
            eprint!("{}", USAGE);
            eprintln!("\nTools: {}", tool_names().join(", "));
            std::process::exit(2);
        }
        Some(tool) => {
            init_tracing(tracing::Level::WARN);
            match call_once(tool, args.get(1).map(String::as_str)).await {
                Ok(output) => {
                    println!("{}", output);
                    Ok(())
                }
                Err(err) => {
                    eprintln!("{:#}", err);
                    std::process::exit(1);
                }
            }
        }
    }
}

fn init_tracing(level: tracing::Level) {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(EnvFilter::from_default_env().add_directive(level.into()))
        .init();
}

/// The binary's version and the grammar set it is paired with. A grammar
/// directory that does not match the binary is the likeliest cause of a file
/// that will not parse, so it is reported rather than left to be guessed at.
fn version_report() -> String {
    let mut out = format!("ast-editor {}\n", env!("CARGO_PKG_VERSION"));
    let wasm_dir = ast_editor::config::get_wasm_dir();
    out.push_str(&format!("grammars {}\n", wasm_dir.display()));

    match ast_editor::config::describe_languages(&wasm_dir) {
        Ok(languages) => {
            let missing: Vec<&str> = languages.iter()
                .filter(|(_, present)| !present)
                .map(|(name, _)| name.as_str())
                .collect();
            let names: Vec<&str> = languages.iter().map(|(name, _)| name.as_str()).collect();
            out.push_str(&format!("         {} declared: {}\n", names.len(), names.join(", ")));
            if !missing.is_empty() {
                out.push_str(&format!("         {} MISSING: {}\n", missing.len(), missing.join(", ")));
            }
        }
        Err(err) => out.push_str(&format!("         unreadable: {:#}\n", err)),
    }
    out
}

fn tool_names() -> Vec<String> {
    ToolDispatcher::new().list_tools().iter()
        .filter_map(|tool| tool.get("name")?.as_str().map(str::to_string))
        .collect()
}

/// Apply an edit script read from stdin. The script says what to change; every
/// flag before it is the same option the JSON form takes.
async fn run_edit_script(args: &[String]) -> anyhow::Result<String> {
    use std::io::Read;

    let mut filepath = None;
    let mut dry_run = false;
    let mut strict = false;
    for arg in args {
        match arg.as_str() {
            "--dry-run" => dry_run = true,
            "--strict" => strict = true,
            flag if flag.starts_with('-') => anyhow::bail!("Unknown option '{}' for edit.", flag),
            path if filepath.is_none() => filepath = Some(path.to_string()),
            extra => anyhow::bail!("edit takes one file; also given '{}'.", extra),
        }
    }
    let filepath = filepath.context("edit needs a file: ast-editor edit <file> < script")?;

    let mut script = String::new();
    std::io::stdin().read_to_string(&mut script).context("Failed to read the edit script from stdin")?;
    if script.trim().is_empty() {
        anyhow::bail!("The edit script is empty. It is read from stdin, so pass it as a heredoc.");
    }

    let edits = ast_editor::tools::script::parse(&script)?;

    let parser_manager = ParserManager::new()?;
    let repository = ast_editor::tools::session_db::SqliteSessionRepository;
    if dry_run {
        ast_editor::tools::edit::edit_lines_dry_run(&repository, &filepath, edits, &parser_manager).await
    } else {
        ast_editor::tools::edit::edit_lines_with_validation(&repository, &filepath, edits, strict, &parser_manager).await
    }
}

/// Run one tool and return what it printed, so the shell sees the tool's own
/// output rather than the JSON-RPC envelope around it.
async fn call_once(tool: &str, args: Option<&str>) -> anyhow::Result<String> {
    let names = tool_names();
    if !names.iter().any(|name| name == tool) {
        anyhow::bail!("Unknown tool '{}'. Available: {}", tool, names.join(", "));
    }

    let arguments: serde_json::Value = match args {
        Some(raw) => serde_json::from_str(raw)
            .map_err(|err| anyhow::anyhow!("Arguments are not valid JSON: {}\n  given: {}", err, raw))?,
        None => serde_json::json!({}),
    };

    let parser_manager = Arc::new(ParserManager::new()?);
    let result = ToolDispatcher::new().call_tool(tool, arguments, &parser_manager).await?;

    let texts: Vec<&str> = result.get("content")
        .and_then(|content| content.as_array())
        .map(|blocks| blocks.iter().filter_map(|b| b.get("text")?.as_str()).collect())
        .unwrap_or_default();

    if texts.is_empty() {
        return Ok(serde_json::to_string_pretty(&result)?);
    }
    Ok(texts.join("\n"))
}

async fn serve_mcp() -> anyhow::Result<()> {
    info!("Bootstrapping ast-editor...");
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

        let response = mcp_server.handle_request(request).await;

        if response.id.is_some() || response.error.is_some() || response.result.is_some() {
            let resp_str = serde_json::to_string(&response)? + "\n";
            stdout.write_all(resp_str.as_bytes()).await?;
            stdout.flush().await?;
        }
    }

    info!("ast-editor Stdio stream closed.");
    Ok(())
}
