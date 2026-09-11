use once_cell::sync::Lazy;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Deserialize)]
struct ToolMeta {
    description: String,
}

static METADATA: Lazy<HashMap<String, ToolMeta>> = Lazy::new(|| {
    let json_str = include_str!("../../resources/tool_metadata.json");
    serde_json::from_str(json_str).unwrap_or_default()
});

#[derive(Deserialize)]
pub struct ToolConfig {
    pub only_ids_wrap_trigger_length: usize,
    pub warning_cumulative_limit: String,
    pub warning_line_cap: String,
    pub error_no_query_match: String,
    pub error_query_does_not_compile: String,
    pub warning_line_limit_exceeded: String,
    pub warning_header_hierarchy: String,
    pub warning_malformed_link: String,
    pub warning_html_syntax: String,
    pub warning_no_grammar: String,
    pub help_footer: String,
    pub error_preview_unknown: String,
    pub error_preview_other_file: String,
    pub error_preview_stale: String,
    pub error_preview_id_shape: String,
}

static CONFIG: Lazy<ToolConfig> = Lazy::new(|| {
    let json_str = include_str!("../../resources/tool_config.json");
    serde_json::from_str(json_str).expect("Failed to parse tool_config.json")
});

pub fn get_config() -> &'static ToolConfig {
    &CONFIG
}

pub fn get_tool_description(name: &str) -> String {
    METADATA
        .get(name)
        .map(|meta| meta.description.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metadata_retrieval() {
        let view_desc = get_tool_description("view");
        assert!(view_desc.contains("Retrieve file lines"));
        assert!(view_desc.contains("Line IDs"));

        let edit_desc = get_tool_description("edit");
        assert!(edit_desc.contains("Apply edits"));
    }

    #[test]
    fn test_config_retrieval() {
        let config = get_config();
        assert_eq!(config.only_ids_wrap_trigger_length, 1000);
        assert!(config.warning_line_cap.contains("800"));
    }
}
