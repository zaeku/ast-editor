//! The skill documentation, carried inside the binary.
//!
//! Embedding it makes the documentation and the code one artifact: a binary
//! cannot be paired with a skill document describing a different version,
//! because there is only ever the one copy. `just install-skill` writes the
//! installed files from here rather than copying them alongside.
//!
//! The API reference is the exception. It describes the tool schemas, which
//! this binary already holds, so it is rendered from them on demand instead of
//! embedded — a generated file embedded in the thing that generates it would
//! be stale between the build that changes a schema and the one that picks the
//! regenerated file back up.

use crate::tools::ToolDispatcher;

/// The hub document, printed by `ast-editor skill` with no topic.
pub(crate) const SKILL: &str = include_str!("../agent_skill/SKILL.md");

/// Topics that are authored documents, embedded verbatim.
const REFERENCES: &[(&str, &str)] = &[
    (
        "usage",
        include_str!("../agent_skill/references/usage_guides.md"),
    ),
    (
        "javascript",
        include_str!("../agent_skill/references/languages/javascript.md"),
    ),
    (
        "go",
        include_str!("../agent_skill/references/languages/go.md"),
    ),
    (
        "markup",
        include_str!("../agent_skill/references/languages/markup.md"),
    ),
    (
        "nix",
        include_str!("../agent_skill/references/languages/nix.md"),
    ),
    (
        "python",
        include_str!("../agent_skill/references/languages/python.md"),
    ),
    (
        "rust",
        include_str!("../agent_skill/references/languages/rust.md"),
    ),
    (
        "shell",
        include_str!("../agent_skill/references/languages/shell.md"),
    ),
    (
        "swift",
        include_str!("../agent_skill/references/languages/swift.md"),
    ),
];

/// Every topic `ast-editor skill <topic>` accepts, in the order it lists them.
pub fn topics() -> Vec<&'static str> {
    let mut names = vec!["api"];
    names.extend(REFERENCES.iter().map(|(name, _)| *name));
    names.sort_unstable();
    names
}

/// The document for a topic, or the hub when no topic is given.
pub fn document(topic: Option<&str>) -> anyhow::Result<String> {
    match topic {
        None => Ok(SKILL.to_string()),
        Some("api") => Ok(render_api()),
        Some(name) => REFERENCES
            .iter()
            .find(|(topic, _)| *topic == name)
            .map(|(_, text)| text.to_string())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "No skill topic '{}'. Topics: {}.",
                    name,
                    topics().join(", ")
                )
            }),
    }
}

/// The tool schemas, rendered from the ones this binary serves. Dry on purpose:
/// it is exhaustive and cannot drift, and worked examples live in `usage`.
pub(crate) fn render_api() -> String {
    let mut out = String::from(
        "# API Reference\n\n\
         Every tool, every parameter, rendered from the schemas this binary serves.\n\
         Worked examples are in `ast-editor skill usage`.\n",
    );

    for tool in ToolDispatcher::new().list_tools() {
        let name = tool["name"].as_str().unwrap_or("?");
        out.push_str(&format!("\n## `{}`\n\n", name));
        if let Some(description) = tool["description"].as_str() {
            out.push_str(&format!("{}\n\n", description.trim()));
        }

        let schema = &tool["inputSchema"];
        let required: Vec<&str> = schema["required"]
            .as_array()
            .map(|names| names.iter().filter_map(|n| n.as_str()).collect())
            .unwrap_or_default();

        let Some(properties) = schema["properties"].as_object() else {
            continue;
        };
        let mut names: Vec<&String> = properties.keys().collect();
        names.sort();

        out.push_str("| parameter | type | required | description |\n|---|---|---|---|\n");
        for parameter in names {
            let field = &properties[parameter];
            let kind = match field["enum"].as_array() {
                Some(values) => values
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(|v| format!("`{}`", v))
                    .collect::<Vec<_>>()
                    .join(" \\| "),
                None => format!("`{}`", field["type"].as_str().unwrap_or("any")),
            };
            out.push_str(&format!(
                "| `{}` | {} | {} | {} |\n",
                parameter,
                kind,
                if required.contains(&parameter.as_str()) {
                    "yes"
                } else {
                    ""
                },
                field["description"]
                    .as_str()
                    .unwrap_or("")
                    .replace('|', "\\|"),
            ));
        }

        if let Some(items) = schema["properties"]["edits"]["items"]["properties"].as_object() {
            out.push_str("\nEach entry of `edits`:\n\n");
            out.push_str("| field | type | description |\n|---|---|---|\n");
            let mut fields: Vec<&String> = items.keys().collect();
            fields.sort();
            for field_name in fields {
                let field = &items[field_name];
                let kind = match field["enum"].as_array() {
                    Some(values) => values
                        .iter()
                        .filter_map(|v| v.as_str())
                        .map(|v| format!("`{}`", v))
                        .collect::<Vec<_>>()
                        .join(" \\| "),
                    None => format!("`{}`", field["type"].as_str().unwrap_or("any")),
                };
                out.push_str(&format!(
                    "| `{}` | {} | {} |\n",
                    field_name,
                    kind,
                    field["description"]
                        .as_str()
                        .unwrap_or("")
                        .replace('|', "\\|"),
                ));
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every parameter name a tool accepts, including the fields of an
    /// `edits` entry, paired with the tool it belongs to.
    fn schema_parameters() -> Vec<(String, String)> {
        let mut found = Vec::new();
        for tool in ToolDispatcher::new().list_tools() {
            let tool_name = tool["name"].as_str().unwrap_or("?").to_string();
            let schema = &tool["inputSchema"];
            if let Some(properties) = schema["properties"].as_object() {
                for parameter in properties.keys() {
                    found.push((tool_name.clone(), parameter.clone()));
                }
            }
            if let Some(items) = schema["properties"]["edits"]["items"]["properties"].as_object() {
                for field in items.keys() {
                    found.push((tool_name.clone(), field.clone()));
                }
            }
        }
        found
    }

    /// The script format's directives are not in any JSON schema, so nothing
    /// else would notice one going undocumented. That failure has happened
    /// twice already — `dry_run` and view's `query` both existed for a
    /// while without appearing in any document an agent reads — and the fix
    /// there was to render the reference from the schema. This surface has no
    /// schema to render from, so it gets a test instead.
    #[test]
    fn test_every_edit_script_directive_is_documented() {
        let mut corpus = String::from(SKILL);
        for (_, text) in REFERENCES {
            corpus.push_str(text);
        }

        let directives = [
            "replace",
            "insert_after",
            "insert_before",
            "append",
            "prepend",
            "delete",
            "move",
        ];
        let undocumented: Vec<&str> = directives
            .into_iter()
            .filter(|directive| !corpus.contains(directive))
            .collect();

        assert!(
            undocumented.is_empty(),
            "the edit script accepts these but no document mentions them: {}",
            undocumented.join(", ")
        );
    }

    /// The rendered reference is the schema, so it must cover all of it. This
    /// is the stricter half: SKILL.md need not mention every parameter, but
    /// the API reference has no excuse.
    #[test]
    fn test_the_rendered_reference_covers_every_parameter() {
        let rendered = render_api();
        let missing: Vec<String> = schema_parameters()
            .into_iter()
            .filter(|(_, parameter)| !rendered.contains(&format!("`{}`", parameter)))
            .map(|(tool, parameter)| format!("{}.{}", tool, parameter))
            .collect();
        assert!(missing.is_empty(), "not rendered: {}", missing.join(", "));
    }

    #[test]
    fn test_every_topic_resolves_to_a_document() {
        for topic in topics() {
            let text = document(Some(topic)).expect(topic);
            assert!(text.len() > 200, "topic '{}' is suspiciously short", topic);
        }
        assert!(document(None).unwrap().contains("ast-editor"));
        assert!(document(Some("nope")).is_err());
    }

    /// The hub is printed to a terminal as often as it is read as a file, and
    /// a relative path resolves in only one of those. It names commands, and
    /// every topic has to be among them or it is unreachable.
    #[test]
    fn test_the_hub_reaches_every_topic_by_command() {
        assert!(
            !SKILL.contains("](references/"),
            "SKILL.md links to a relative path, which does not resolve when the document is printed"
        );
        for topic in topics() {
            let invocation = format!("ast-editor skill {}", topic);
            assert!(
                SKILL.contains(&invocation),
                "no way to reach '{}' from the hub: it never says `{}`",
                topic,
                invocation
            );
        }
    }
}
