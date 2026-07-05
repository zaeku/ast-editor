use std::path::{Path, PathBuf};
use std::env;
use std::collections::HashMap;
use std::fs;
use serde::Deserialize;
use anyhow::{Result, Context, bail};

#[derive(Debug, Deserialize, Clone)]
pub struct LanguageConfig {
    pub extensions: Vec<String>,
    pub wasm_file: String,
}

/// Decouples file extension mappings from source code using languages.json.
/// If file is missing or invalid, throws a descriptive suggestion error.
fn init_languages_map(wasm_dir: &Path) -> Result<HashMap<String, String>> {
    let config_path = wasm_dir.join("languages.json");
    
    if !config_path.exists() {
        bail!(
            "Configuration file 'languages.json' is missing.\n\
             - Expected Location: {:?}\n\
             - Format Requirement (JSON):\n  {{\n    \"rust\": {{\n      \"extensions\": [\".rs\"],\n      \"wasm_file\": \"tree-sitter-rust.wasm\"\n    }}\n  }}",
            config_path
        );
    }

    let content = fs::read_to_string(&config_path)
        .with_context(|| format!("Failed to read languages config file at {:?}", config_path))?;

    let parsed: HashMap<String, LanguageConfig> = serde_json::from_str(&content)
        .context(
            "Failed to parse languages.json config file.\n\
             - Expected Format (JSON):\n  {{\n    \"rust\": {{\n      \"extensions\": [\".rs\"],\n      \"wasm_file\": \"tree-sitter-rust.wasm\"\n    }}\n  }}"
        )?;

    let mut map = HashMap::new();
    for (_lang, cfg) in parsed {
        for ext in cfg.extensions {
            map.insert(ext, cfg.wasm_file.clone());
        }
    }
    
    Ok(map)
}

/// Dynamically lookup wasm grammar file name for a given file extension under a custom raw wasm directory.
pub fn get_wasm_file(wasm_dir: &Path, ext: &str) -> Result<String> {
    let map = init_languages_map(wasm_dir)?;
    let lookup_key = if ext.starts_with('.') {
        ext.to_string()
    } else {
        format!(".{}", ext)
    };
    if let Some(wasm_file) = map.get(&lookup_key) {
        Ok(wasm_file.clone())
    } else {
        bail!("Unsupported file extension: {}", ext)
    }
}

/// Clear caching for test isolation (noop after removing cache)
#[cfg(test)]
pub fn clear_langs_map_for_testing() {}



/// Returns the path to the WebAssembly grammar files directory (resources/wasm).
pub fn get_wasm_dir() -> PathBuf {
    if let Ok(dir) = env::var("TREE_SITTER_WASM_DIR") {
        PathBuf::from(dir)
    } else {
        PathBuf::from("./resources/wasm")
    }
}
