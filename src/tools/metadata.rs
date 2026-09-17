use once_cell::sync::Lazy;
use serde::Deserialize;
use serde_json::Value;

/// One tool as the binary publishes it: the name a caller invokes, the
/// description a model reads, and the schema its arguments are checked against.
/// All three are copy rather than logic, so they live beside each other in
/// `resources/` and are changed without opening the code that serves them.
#[derive(Deserialize)]
pub(crate) struct ToolMeta {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

/// The published tools in the order they are answered in. A list rather than a
/// map, because that order reaches the help, the generated documents and the
/// skill, and a map would leave it to chance.
static METADATA: Lazy<Vec<ToolMeta>> = Lazy::new(|| {
    let json_str = include_str!("../../resources/tool_metadata.json");
    serde_json::from_str(json_str).unwrap_or_default()
});

pub(crate) fn tools() -> &'static [ToolMeta] {
    &METADATA
}

#[derive(Deserialize)]
pub(crate) struct ToolConfig {
    pub only_ids_wrap_trigger_length: usize,
    pub warning_cumulative_limit: String,
    pub warning_line_cap: String,
    pub error_no_query_match: String,
    pub error_query_does_not_compile: String,
    pub warning_header_hierarchy: String,
    pub warning_malformed_link: String,
    pub warning_html_syntax: String,
    pub warning_no_grammar: String,
    pub message_not_checked: String,
    pub error_target_gone: String,
    pub error_address_needs_a_number: String,
    pub error_edit_missing_field: String,
    pub error_target_changed: String,
    pub error_target_changed_outside: String,
    pub error_pattern_not_found: String,
    pub error_file_written_while_read: String,
    pub error_op_takes_one_line: String,
    pub help_footer: String,
    pub error_strict_refused: String,
    pub error_strict_unevaluable: String,
    pub error_preview_unknown: String,
    pub error_preview_other_file: String,
    pub error_preview_stale: String,
    pub error_preview_id_shape: String,
}

static CONFIG: Lazy<ToolConfig> = Lazy::new(|| {
    let json_str = include_str!("../../resources/tool_config.json");
    serde_json::from_str(json_str).expect("Failed to parse tool_config.json")
});

pub(crate) fn get_config() -> &'static ToolConfig {
    &CONFIG
}

pub(crate) fn get_tool_description(name: &str) -> String {
    METADATA
        .iter()
        .find(|meta| meta.name == name)
        .map(|meta| meta.description.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metadata_retrieval() {
        for tool in ["outline", "inspect", "view", "edit", "create"] {
            assert!(
                !get_tool_description(tool).is_empty(),
                "{tool} has no description, and the documents render it"
            );
        }
        assert!(get_tool_description("nonesuch").is_empty());
    }

    #[test]
    fn test_config_retrieval() {
        let config = get_config();
        assert_eq!(config.only_ids_wrap_trigger_length, 1000);
        // The warning names both numbers the formatter enforces, and they
        // drift apart from it silently unless something compares them.
        for number in [
            crate::tools::formatter::LINE_CAP,
            crate::tools::formatter::SEGMENT_LENGTH,
        ] {
            assert!(
                config.warning_line_cap.contains(&number.to_string()),
                "warning_line_cap does not name {number}: {}",
                config.warning_line_cap
            );
        }
    }
}
