use once_cell::sync::Lazy;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Deserialize)]
struct ToolMeta {
    description: String,
    tip: Option<String>,
}

static METADATA: Lazy<HashMap<String, ToolMeta>> = Lazy::new(|| {
    let json_str = include_str!("../../resources/tool_metadata.json");
    serde_json::from_str(json_str).unwrap_or_default()
});

pub fn get_tool_description(name: &str) -> String {
    METADATA
        .get(name)
        .map(|meta| meta.description.clone())
        .unwrap_or_default()
}

pub fn get_tool_tip(name: &str) -> String {
    METADATA
        .get(name)
        .and_then(|meta| meta.tip.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metadata_retrieval() {
        let view_desc = get_tool_description("view_lines");
        assert!(view_desc.contains("Retrieves lines"));
        assert!(view_desc.contains("Line IDs"));

        let edit_desc = get_tool_description("edit_lines");
        assert!(edit_desc.contains("Applies a structured batch"));

        let dump_tip = get_tool_tip("dump_ast");
        assert!(dump_tip.contains("Tip:"));
    }
}
