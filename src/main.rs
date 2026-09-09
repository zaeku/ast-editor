use ast_editor::parser::ParserManager;
use ast_editor::tools::ToolDispatcher;
use std::sync::Arc;
use anyhow::Context;
use tracing_subscriber::EnvFilter;

const USAGE: &str = "\
ast-editor — line-precise editing over tree-sitter

    ast-editor edit <file> < script  apply an edit script read from stdin
    ast-editor skill [topic]         the skill document, or one of its references
    ast-editor <tool> <file> [opts]  call one tool and print its output
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

Options are the tool's own parameters, so whatever `ast-editor skill api`
lists can be passed as one. A tool is named by any unambiguous prefix, so the
`_lines` and `_ast` suffixes can be left off. Paths are relative to the
working directory:

    ast-editor view src/main.rs --start-line 40 --end-line 80
    ast-editor inspect src/main.rs --template functions
    ast-editor edit src/main.rs --apply p1f

A shape no option can carry, such as an array of edits, goes in as JSON:

    ast-editor edit src/main.rs --json '{\"edits\":[...]}'
";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("edit") => {
            init_tracing(tracing::Level::WARN);
            match run_edit(&args[1..]).await {
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
            match call_once(tool, &args[1..]).await {
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

/// The one edit command. Its options are `edit_lines`'s own parameters; when
/// none of them says what to change, the edit script on stdin does.
async fn run_edit(args: &[String]) -> anyhow::Result<String> {
    use std::io::Read;

    // `--strict` is how the script form has always spelled strict_validation.
    let args: Vec<String> = args.iter()
        .map(|arg| if arg == "--strict" { "--strict-validation".to_string() } else { arg.clone() })
        .collect();

    let mut arguments = ast_editor::cli::arguments("edit_lines", &args)?;
    let given = arguments.as_object_mut().context("edit takes options, not a bare value")?;
    if !given.contains_key("filepath") {
        anyhow::bail!("edit needs a file: ast-editor edit <file> < script");
    }

    // Reading stdin is what makes this command the script form, so it happens
    // only when nothing on the command line already carries the edits.
    if !given.contains_key("edits") && !given.contains_key("apply") {
        let mut script = String::new();
        std::io::stdin().read_to_string(&mut script)
            .context("Failed to read the edit script from stdin")?;
        if script.trim().is_empty() {
            anyhow::bail!("The edit script is empty. It is read from stdin, so pass it as a heredoc.");
        }
        let edits = ast_editor::tools::script::parse(&script)?;
        given.insert("edits".to_string(), serde_json::to_value(edits)?);
    }

    call_with("edit_lines", arguments).await
}

/// Run one tool and return what it printed, so the shell sees the tool's own
/// output rather than the JSON-RPC envelope around it.
async fn call_once(tool: &str, args: &[String]) -> anyhow::Result<String> {
    let tool = ast_editor::cli::resolve(tool)?;
    let arguments = ast_editor::cli::arguments(&tool, args)?;
    call_with(&tool, arguments).await
}

/// Dispatch one tool call that is already built.
async fn call_with(tool: &str, arguments: serde_json::Value) -> anyhow::Result<String> {
    let parser_manager = Arc::new(ParserManager::new()?);
    ToolDispatcher::new().call_tool(tool, arguments, &parser_manager).await
}
