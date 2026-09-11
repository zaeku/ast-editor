use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

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

static LANGUAGES_MAP_CACHE: once_cell::sync::Lazy<
    std::sync::Mutex<HashMap<PathBuf, HashMap<String, String>>>,
> = once_cell::sync::Lazy::new(|| std::sync::Mutex::new(HashMap::new()));

/// The languages the grammar directory actually provides, and whether each
/// one's wasm file is present. Reported by `--version`, where a missing
/// grammar is the likeliest reason a file will not parse.
pub fn describe_languages(wasm_dir: &Path) -> Result<Vec<(String, bool)>> {
    let mut described: Vec<(String, bool)> = init_languages_map(wasm_dir)?
        .values()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .map(|wasm_file| {
            let name = wasm_file
                .strip_suffix(".wasm")
                .unwrap_or(wasm_file)
                .strip_prefix("tree-sitter-")
                .unwrap_or(wasm_file)
                .to_string();
            (name, wasm_dir.join(wasm_file).is_file())
        })
        .collect();
    described.sort();
    Ok(described)
}

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
    if let Some(dir) = env::var_os("AST_EDITOR_WASM_DIR") {
        return PathBuf::from(dir);
    }
    // Cargo sets this for everything it runs, so an installed ast-editor called
    // from another crate's `cargo test` would be sent looking for grammars in
    // whichever crate cargo is building. It only counts when it is this
    // project's own tree.
    if let Ok(manifest_dir) = env::var("CARGO_MANIFEST_DIR") {
        let in_manifest = PathBuf::from(manifest_dir).join("resources").join("wasm");
        if in_manifest.join("languages.json").is_file() {
            return in_manifest;
        }
    }
    if let Ok(exe_path) = env::current_exe() {
        if let Some(prefix) = exe_path.parent().and_then(|p| p.parent()) {
            // An installed layout: the binary in <prefix>/bin, its grammars in
            // <prefix>/share/ast-editor/wasm, so nothing is dropped in the
            // prefix root that other tools share.
            let installed = prefix.join("share").join("ast-editor").join("wasm");
            if installed.is_dir() {
                return installed;
            }
            // A self-contained deployment: grammars beside the binary's parent.
            let beside = prefix.join("resources").join("wasm");
            if beside.is_dir() {
                return beside;
            }
        }
    }
    PathBuf::from("./resources/wasm")
}
