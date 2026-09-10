//! Turning a command line into the arguments a tool takes.
//!
//! The flags are not written out anywhere: they are derived from the tool's own
//! schema, so a parameter added to a schema gains a flag with no second place
//! to remember. `--start-line 40` is the `start_line` property, typed by what
//! the schema says it is.
//!
//! The JSON form still works — a bare `{...}`, or `--json '{...}'` — because
//! the schema describes shapes a flag cannot carry, such as `edit`'s
//! array of edits.

use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};

use crate::tools::ToolDispatcher;

/// The tool a command-line name means. On the command line the binary name
/// already says these are about a syntax tree, so the `_lines` and `_ast`
/// suffixes carry nothing: any unambiguous prefix names the tool, and `view`
/// is `view`.
pub fn resolve(tool: &str) -> Result<String> {
    let names: Vec<String> = ToolDispatcher::new()
        .list_tools()
        .iter()
        .filter_map(|candidate| candidate["name"].as_str().map(str::to_string))
        .collect();

    if names.iter().any(|name| name == tool) {
        return Ok(tool.to_string());
    }
    let matched: Vec<&String> = names.iter().filter(|name| name.starts_with(tool)).collect();
    match matched.as_slice() {
        [one] => Ok((*one).clone()),
        [] => bail!("Unknown tool '{}'. Available: {}", tool, names.join(", ")),
        many => bail!(
            "'{}' could be any of: {}.",
            tool,
            many.iter()
                .map(|name| name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// The schema of one tool, or an error naming the ones that exist.
fn schema_of(tool: &str) -> Result<Value> {
    let name = resolve(tool)?;
    ToolDispatcher::new()
        .list_tools()
        .into_iter()
        .find(|candidate| candidate["name"].as_str() == Some(name.as_str()))
        .map(|candidate| candidate["inputSchema"].clone())
        .with_context(|| format!("Unknown tool '{}'", name))
}

/// An absolute path, so that a session is keyed the same however the caller
/// spelled it. The file need not exist yet: `create` makes one.
pub fn absolute(path: &str) -> Result<String> {
    let path = std::path::Path::new(path);
    if path.is_absolute() {
        return Ok(path.to_string_lossy().into_owned());
    }
    let cwd = std::env::current_dir().context("Failed to read the working directory")?;
    Ok(cwd.join(path).to_string_lossy().into_owned())
}

/// Build a tool's arguments from what followed its name on the command line.
pub fn arguments(tool: &str, args: &[String]) -> Result<Value> {
    let schema = schema_of(tool)?;
    let empty = Map::new();
    let properties = schema["properties"].as_object().unwrap_or(&empty);

    let mut out = Map::new();
    let mut positional: Option<String> = None;
    let mut idx = 0;

    while idx < args.len() {
        let arg = &args[idx];
        idx += 1;

        if arg == "--json" {
            let raw = args
                .get(idx)
                .context("--json needs a JSON object after it")?;
            idx += 1;
            let parsed: Value = serde_json::from_str(raw).map_err(|err| {
                anyhow::anyhow!("--json is not valid JSON: {}\n  given: {}", err, raw)
            })?;
            let object = parsed
                .as_object()
                .context("--json takes an object, such as --json '{\"filepath\":\"x.rs\"}'")?;
            for (key, value) in object {
                out.insert(key.clone(), value.clone());
            }
            continue;
        }

        if let Some(flag) = arg.strip_prefix("--") {
            // `--no-x` is how a boolean is turned off without a value.
            let (flag, negated) = match flag.strip_prefix("no-") {
                Some(rest) if properties.contains_key(&rest.replace('-', "_")) => (rest, true),
                _ => (flag, false),
            };
            let name = flag.replace('-', "_");
            let field = properties.get(&name).with_context(|| {
                format!(
                    "'{}' takes no option '{}'. {}",
                    tool,
                    arg,
                    options_of(properties)
                )
            })?;

            let value = match field["type"].as_str() {
                // A boolean is true by being named, but `--flag false` is what
                // a caller writes when transcribing the JSON form, so take it.
                Some("boolean") => {
                    if negated {
                        Value::Bool(false)
                    } else {
                        match args.get(idx).map(String::as_str) {
                            Some("true") => {
                                idx += 1;
                                Value::Bool(true)
                            }
                            Some("false") => {
                                idx += 1;
                                Value::Bool(false)
                            }
                            _ => Value::Bool(true),
                        }
                    }
                }
                _ if negated => bail!("'{}' is not a switch, so '--no-' does not apply.", flag),
                _ => {
                    let raw = args
                        .get(idx)
                        .with_context(|| format!("'{}' needs a value after it", arg))?;
                    idx += 1;
                    coerce(raw, field, arg)?
                }
            };
            out.insert(name, value);
            continue;
        }

        // The old form: the whole argument object as one JSON string.
        if arg.trim_start().starts_with('{') {
            let parsed: Value = serde_json::from_str(arg).map_err(|err| {
                anyhow::anyhow!("Arguments are not valid JSON: {}\n  given: {}", err, arg)
            })?;
            let object = parsed
                .as_object()
                .context("Arguments must be a JSON object")?;
            for (key, value) in object {
                out.insert(key.clone(), value.clone());
            }
            continue;
        }

        if positional.is_some() {
            bail!("'{}' takes one file; also given '{}'.", tool, arg);
        }
        positional = Some(arg.clone());
    }

    if let Some(path) = positional {
        out.insert("filepath".to_string(), Value::String(absolute(&path)?));
    }
    if let Some(Value::String(path)) = out.get("filepath") {
        let resolved = absolute(path)?;
        out.insert("filepath".to_string(), Value::String(resolved));
    }

    Ok(Value::Object(out))
}

/// Coerce a flag's text to what the schema says the property holds.
fn coerce(raw: &str, field: &Value, flag: &str) -> Result<Value> {
    if let Some(allowed) = field["enum"].as_array() {
        let names: Vec<&str> = allowed.iter().filter_map(|v| v.as_str()).collect();
        if !names.contains(&raw) {
            bail!(
                "'{}' does not take '{}'. One of: {}.",
                flag,
                raw,
                names.join(", ")
            );
        }
        return Ok(Value::String(raw.to_string()));
    }

    match field["type"].as_str() {
        Some("integer") => raw
            .parse::<i64>()
            .map(Value::from)
            .map_err(|_| anyhow::anyhow!("'{}' takes a whole number, not '{}'.", flag, raw)),
        Some("number") => raw
            .parse::<f64>()
            .map(Value::from)
            .map_err(|_| anyhow::anyhow!("'{}' takes a number, not '{}'.", flag, raw)),
        Some("array") | Some("object") => bail!(
            "'{}' takes a {}, which an option cannot carry. Pass it with --json, \
             or for edits use: ast-editor edit <file> < script",
            flag,
            field["type"].as_str().unwrap_or("value")
        ),
        _ => Ok(Value::String(raw.to_string())),
    }
}

/// The options a tool accepts, named as they are typed.
fn options_of(properties: &Map<String, Value>) -> String {
    let mut names: Vec<String> = properties
        .keys()
        .map(|name| format!("--{}", name.replace('_', "-")))
        .collect();
    names.sort();
    format!("Options: {}, --json.", names.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn test_a_prefix_names_the_tool_it_can_only_mean() {
        assert_eq!(resolve("ins").unwrap(), "inspect");
        assert_eq!(resolve("cr").unwrap(), "create");
        assert_eq!(resolve("outline").unwrap(), "outline");
        assert!(resolve("nope").is_err());
        // A prefix two tools share names neither of them.
        assert!(resolve("").is_err());
    }

    #[test]
    fn test_flags_become_the_properties_the_schema_names() {
        let parsed = arguments(
            "view",
            &args(&["/tmp/x.rs", "--start-line", "40", "--end-line", "80"]),
        )
        .unwrap();
        assert_eq!(parsed["filepath"], "/tmp/x.rs");
        assert_eq!(parsed["start_line"], 40);
        assert_eq!(parsed["end_line"], 80);
    }

    #[test]
    fn test_a_boolean_flag_needs_no_value() {
        let parsed = arguments("view", &args(&["/tmp/x.rs", "--only-ids"])).unwrap();
        assert_eq!(parsed["only_ids"], true);
    }

    #[test]
    fn test_a_boolean_takes_the_written_form_too() {
        // What a caller writes when transcribing `"include_code": false`.
        let explicit =
            arguments("inspect", &args(&["/tmp/x.rs", "--include-code", "false"])).unwrap();
        assert_eq!(explicit["include_code"], false);
        assert!(explicit["filepath"]
            .as_str()
            .unwrap()
            .ends_with("/tmp/x.rs"));

        let negated = arguments("inspect", &args(&["/tmp/x.rs", "--no-include-code"])).unwrap();
        assert_eq!(negated["include_code"], false);

        let bare = arguments("inspect", &args(&["/tmp/x.rs", "--include-code"])).unwrap();
        assert_eq!(bare["include_code"], true);
    }

    #[test]
    fn test_a_relative_path_becomes_absolute() {
        // A session is keyed by the path string, so the same file reached from
        // two directories must not become two sessions.
        let parsed = arguments("view", &args(&["some/where.rs"])).unwrap();
        let path = parsed["filepath"].as_str().unwrap();
        assert!(std::path::Path::new(path).is_absolute(), "{}", path);
        assert!(path.ends_with("some/where.rs"), "{}", path);
    }

    #[test]
    fn test_the_json_form_still_works_both_ways() {
        let bare = arguments(
            "view",
            &args(&[r#"{"filepath":"/tmp/x.rs","only_ids":true}"#]),
        )
        .unwrap();
        assert_eq!(bare["only_ids"], true);

        let flagged = arguments(
            "view",
            &args(&["--json", r#"{"filepath":"/tmp/x.rs"}"#, "--start-line", "3"]),
        )
        .unwrap();
        assert_eq!(flagged["filepath"], "/tmp/x.rs");
        assert_eq!(flagged["start_line"], 3);
    }

    #[test]
    fn test_an_unknown_option_lists_the_ones_that_exist() {
        let err = arguments("view", &args(&["/tmp/x.rs", "--nope", "1"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("--nope"), "{}", err);
        assert!(
            err.contains("--start-line"),
            "the real options are not listed: {}",
            err
        );
    }

    #[test]
    fn test_a_wrong_value_says_what_was_wanted() {
        let err = arguments("view", &args(&["/tmp/x.rs", "--start-line", "forty"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("whole number"), "{}", err);

        let err = arguments("inspect", &args(&["/tmp/x.rs", "--template", "nope"]))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("functions"),
            "the allowed values are not listed: {}",
            err
        );
    }

    #[test]
    fn test_a_shape_no_option_can_carry_says_where_to_put_it() {
        let err = arguments("edit", &args(&["/tmp/x.rs", "--edits", "[]"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("--json"), "{}", err);
        assert!(err.contains("ast-editor edit"), "{}", err);
    }
}
