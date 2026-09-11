use anyhow::Context;
use ast_editor::parser::ParserManager;
use ast_editor::tools::ToolDispatcher;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

/// What the tool says about itself lives in resources/ (D-01M27G4E6GTCA8), and
/// the first thirty lines are budgeted for a reader who pipes this to `head`.
const USAGE: &str = include_str!("../resources/usage.txt");

/// Write to stdout, treating a closed reader as the end of the work rather than
/// a failure. An agent reading `--help | head -30` closes the pipe on line 31,
/// and Rust's `println!` panics on that.
fn say(text: &str) {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => std::process::exit(0),
        Err(err) => {
            eprintln!("failed writing to stdout: {}", err);
            std::process::exit(1);
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("edit") => {
            init_tracing(tracing::Level::WARN);
            match run_edit(&args[1..]).await {
                Ok(output) => {
                    say(&format!("{}\n", output));
                    Ok(())
                }
                Err(err) => {
                    eprintln!("{:#}", err);
                    std::process::exit(1);
                }
            }
        }
        Some("skill") => match ast_editor::skill::document(args.get(1).map(String::as_str)) {
            Ok(text) => {
                say(&text);
                Ok(())
            }
            Err(err) => {
                eprintln!("{:#}", err);
                std::process::exit(2);
            }
        },
        Some("--version") | Some("-V") | Some("version") => {
            say(&version_report());
            Ok(())
        }
        Some("--help") | Some("-h") | Some("help") => {
            say(USAGE);
            say(&format!("\nTools:  {}\n", tool_names().join(", ")));
            say(&format!(
                "Topics: {}\n",
                ast_editor::skill::topics().join(", ")
            ));
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
                    say(&format!("{}\n", output));
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

/// The binary's version and the grammars compiled into it, each with the
/// version Cargo.lock pinned (D-01M28RAGW19ZZC), so that a file parsing
/// differently than expected can be traced to a grammar without opening the
/// tree it was built from.
fn version_report() -> String {
    let mut out = format!("ast-editor {}\n", env!("CARGO_PKG_VERSION"));
    match ast_editor::config::describe_languages() {
        Ok(languages) => {
            out.push_str(&format!("grammars {} compiled in\n", languages.len()));
            for (name, version) in languages {
                out.push_str(&format!("         {name} {version}\n"));
            }
        }
        Err(err) => out.push_str(&format!("grammars unreadable: {:#}\n", err)),
    }
    out
}

fn tool_names() -> Vec<String> {
    ToolDispatcher::new()
        .list_tools()
        .iter()
        .filter_map(|tool| tool.get("name")?.as_str().map(str::to_string))
        .collect()
}

/// The one edit command. Its options are `edit`'s own parameters; when
/// none of them says what to change, the edit script on stdin does.
async fn run_edit(args: &[String]) -> anyhow::Result<String> {
    use std::io::Read;

    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        return ast_editor::cli::help_for("edit");
    }
    // `--strict` is how the script form has always spelled strict_validation.
    let args: Vec<String> = args
        .iter()
        .map(|arg| {
            if arg == "--strict" {
                "--strict-validation".to_string()
            } else {
                arg.clone()
            }
        })
        .collect();

    let mut arguments = ast_editor::cli::arguments("edit", &args)?;
    let given = arguments
        .as_object_mut()
        .context("edit takes options, not a bare value")?;
    if !given.contains_key("filepath") {
        anyhow::bail!("edit needs a file: ast-editor edit <file> < script");
    }

    // Reading stdin is what makes this command the script form, so it happens
    // only when nothing on the command line already carries the edits.
    if !given.contains_key("edits") && !given.contains_key("apply") {
        let mut script = String::new();
        std::io::stdin()
            .read_to_string(&mut script)
            .context("Failed to read the edit script from stdin")?;
        if script.trim().is_empty() {
            anyhow::bail!(
                "The edit script is empty. It is read from stdin, so pass it as a heredoc."
            );
        }
        let edits = ast_editor::tools::script::parse(&script)?;
        given.insert("edits".to_string(), serde_json::to_value(edits)?);
    }

    call_with("edit", arguments).await
}

/// Run one tool and return what it printed, so the shell sees the tool's own
/// output rather than the JSON-RPC envelope around it.
async fn call_once(tool: &str, args: &[String]) -> anyhow::Result<String> {
    let tool = ast_editor::cli::resolve(tool)?;
    // Asking what a tool takes is a question, not a mistake: answer it rather
    // than refusing the call and listing the options in the complaint.
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        return ast_editor::cli::help_for(&tool);
    }
    let arguments = ast_editor::cli::arguments(&tool, args)?;
    call_with(&tool, arguments).await
}

/// Dispatch one tool call that is already built.
async fn call_with(tool: &str, arguments: serde_json::Value) -> anyhow::Result<String> {
    let parser_manager = Arc::new(ParserManager::new()?);
    ToolDispatcher::new()
        .call_tool(tool, arguments, &parser_manager)
        .await
}
