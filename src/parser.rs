use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use wasmtime::{Engine, Config, Cache};
use tokio::sync::Mutex as TokioMutex;
use anyhow::{Context, Result, bail};
use tree_sitter::{WasmStore, Parser, Language};

use crate::config;

pub struct ParserManager {
    engine: Engine,
    wasm_dir: PathBuf,
    wasm_bytes_cache: Arc<TokioMutex<HashMap<String, Vec<u8>>>>,
    active_session: Arc<TokioMutex<Option<(String, Parser, Language)>>>,
}

impl ParserManager {
    pub fn new() -> Result<Self> {
        let mut config = Config::new();
        if let Ok(cache) = Cache::from_file(None) {
            config.cache(Some(cache));
        }
        let engine = Engine::new(&config)
            .context("Failed to initialize Wasmtime engine")?;
        
        Ok(Self {
            engine,
            wasm_dir: config::get_wasm_dir(),
            wasm_bytes_cache: Arc::new(TokioMutex::new(HashMap::new())),
            active_session: Arc::new(TokioMutex::new(None)),
        })
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn wasm_dir(&self) -> &Path {
        &self.wasm_dir
    }

    /// Custom paths constructor for testing
    pub fn with_paths(_cache_dir: PathBuf, _compiler_path: PathBuf, wasm_dir: PathBuf) -> Result<Self> {
        let mut config = Config::new();
        if let Ok(cache) = Cache::from_file(None) {
            config.cache(Some(cache));
        }
        let engine = Engine::new(&config)?;
        Ok(Self {
            engine,
            wasm_dir,
            wasm_bytes_cache: Arc::new(TokioMutex::new(HashMap::new())),
            active_session: Arc::new(TokioMutex::new(None)),
        })
    }

    /// Sticky Session-enabled parser execution that caches instantiation.
    pub async fn parse_code(&self, ext: &str, code: &str) -> Result<(tree_sitter::Tree, Language)> {
        let wasm_file = config::get_wasm_file(&self.wasm_dir, ext)
            .with_context(|| format!("Unsupported file extension: {}", ext))?;
        
        let wasm_name = wasm_file.strip_suffix(".wasm").unwrap_or(&wasm_file);
        
        let mut session_lock = self.active_session.lock().await;
        
        // 1. Cache Hit: Reuse active Parser session
        if let Some((ref active_name, ref mut parser, ref lang)) = *session_lock {
            if active_name == wasm_name {
                let tree = parser.parse(code, None)
                    .context("Failed to parse code in cached session")?;
                return Ok((tree, lang.clone()));
            }
        }
        
        // 2. Cache Miss: Fresh load and compilation
        let mut wasm_store = WasmStore::new(&self.engine)
            .context("Failed to create WasmStore")?;

        // Load WASM bytes
        let wasm_bytes = {
            let mut cache = self.wasm_bytes_cache.lock().await;
            if let Some(bytes) = cache.get(wasm_name) {
                bytes.clone()
            } else {
                let raw_wasm_path = self.wasm_dir.join(format!("{}.wasm", wasm_name));
                if !raw_wasm_path.exists() {
                    bail!("Raw Wasm file not found for {}", wasm_name);
                }
                let bytes = fs::read(&raw_wasm_path)
                    .with_context(|| format!("Failed to read raw wasm module at {:?}", raw_wasm_path))?;
                cache.insert(wasm_name.to_string(), bytes.clone());
                bytes
            }
        };
        
        let lang_symbol = wasm_name.strip_prefix("tree-sitter-").unwrap_or(wasm_name).replace("-", "_");
        let language = wasm_store.load_language(&lang_symbol, &wasm_bytes)
            .context("Failed to load language into WasmStore")?;
            
        let mut parser = Parser::new();
        parser.set_wasm_store(wasm_store)
            .context("Failed to set WasmStore on Parser")?;
            
        parser.set_language(&language)
            .context("Failed to set language on Parser")?;

        let _tree = parser.parse(code, None)
            .context("Failed to parse code after fresh instantiation")?;
            
        // Store session
        *session_lock = Some((wasm_name.to_string(), parser, language.clone()));
        
        // Fetch reference from session
        if let Some((_, ref mut parser, ref lang)) = *session_lock {
            let tree = parser.parse(code, None)
                .context("Re-parsing inside session storage failed")?;
            return Ok((tree, lang.clone()));
        }
        
        bail!("Failed to store parser session")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct TempTestFixture {
        dir: PathBuf,
    }

    impl TempTestFixture {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("tree_sitter_inspector_parser_tests_{}", name));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }
    }

    impl Drop for TempTestFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    #[tokio::test]
    async fn test_parse_code_success() {
        let fixture = TempTestFixture::new("parse_success");
        let wasm_dir = fixture.dir.join("wasm");
        fs::create_dir_all(&wasm_dir).unwrap();

        // Write a config maps for test
        let config_path = wasm_dir.join("languages.json");
        let mock_config = serde_json::json!({
            "rust": {
                "extensions": [".rs"],
                "wasm_file": "tree-sitter-rust.wasm"
            }
        });
        fs::write(&config_path, serde_json::to_string(&mock_config).unwrap()).unwrap();

        let pm = ParserManager::with_paths(
            fixture.dir.join("cache"),
            fixture.dir.join("compiler"),
            wasm_dir
        ).unwrap();

        // Calling parse_code with missing wasm file should return error cleanly
        let res = pm.parse_code("rs", "fn main() {}").await;
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("Raw Wasm file not found"));
    }

    #[tokio::test]
    async fn test_parse_nix_code() {
        let pm = ParserManager::new().unwrap();
        let res = pm.parse_code("nix", "{ x = 1; }").await;
        assert!(res.is_ok());
        let (tree, _) = res.unwrap();
        assert!(!tree.root_node().has_error());
    }
}
