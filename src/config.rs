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

static LANGUAGES_MAP_CACHE: once_cell::sync::Lazy<std::sync::Mutex<HashMap<PathBuf, HashMap<String, String>>>> =
    once_cell::sync::Lazy::new(|| std::sync::Mutex::new(HashMap::new()));

/// Dynamically lookup wasm grammar file name for a given file extension under a custom raw wasm directory.
pub fn get_wasm_file(wasm_dir: &Path, ext: &str) -> Result<String> {
    let lookup_key = if ext.starts_with('.') {
        ext.to_string()
    } else {
        format!(".{}", ext)
    };

    let mut cache = LANGUAGES_MAP_CACHE.lock().unwrap();
    if let Some(map) = cache.get(wasm_dir) {
        if let Some(wasm_file) = map.get(&lookup_key) {
            return Ok(wasm_file.clone());
        } else {
            bail!("Unsupported file extension: {}", ext);
        }
    }

    let map = init_languages_map(wasm_dir)?;
    let result = if let Some(wasm_file) = map.get(&lookup_key) {
        Ok(wasm_file.clone())
    } else {
        bail!("Unsupported file extension: {}", ext)
    };
    cache.insert(wasm_dir.to_path_buf(), map);
    result
}

/// Clear caching for test isolation (noop after removing cache)
#[cfg(test)]
pub fn clear_langs_map_for_testing() {
    if let Ok(mut cache) = LANGUAGES_MAP_CACHE.lock() {
        cache.clear();
    }
}



/// Returns the path to the WebAssembly grammar files directory (resources/wasm).
pub fn get_wasm_dir() -> PathBuf {
    if let Ok(manifest_dir) = env::var("CARGO_MANIFEST_DIR") {
        return PathBuf::from(manifest_dir).join("resources").join("wasm");
    }
    if let Ok(exe_path) = env::current_exe() {
        if let Some(parent) = exe_path.parent().and_then(|p| p.parent()) {
            return parent.join("resources").join("wasm");
        }
    }
    PathBuf::from("./resources/wasm")
}
