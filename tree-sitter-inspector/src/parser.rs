use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tree_sitter::wasmtime::{Engine, Config};
use tokio::sync::Mutex as TokioMutex;
use sha2::{Sha256, Digest};
use tracing::{info, warn, debug};
use anyhow::{Context, Result, bail};
use tree_sitter::{WasmStore, Parser};

use crate::config;

pub struct ParserManager {
    engine: Engine,
    compile_locks: Arc<TokioMutex<HashMap<String, Arc<TokioMutex<()>>>>>,
    cache_dir: PathBuf,
    compiler_path: PathBuf,
    wasm_dir: PathBuf,
    wasm_bytes_cache: Arc<TokioMutex<HashMap<String, Vec<u8>>>>,
    active_session: Arc<TokioMutex<Option<(String, Parser, tree_sitter::Language)>>>,
}

impl ParserManager {
    pub fn new() -> Result<Self> {
        let mut config = Config::new();
        config.wasm_tail_call(true);
        config.wasm_threads(true);
        let engine = Engine::new(&config)
            .context("Failed to initialize Wasmtime engine")?;
        
        Ok(Self {
            engine,
            compile_locks: Arc::new(TokioMutex::new(HashMap::new())),
            cache_dir: config::get_cache_dir(),
            compiler_path: config::get_compiler_path(),
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
    #[cfg(test)]
    pub fn with_paths(cache_dir: PathBuf, compiler_path: PathBuf, wasm_dir: PathBuf) -> Result<Self> {
        let mut config = Config::new();
        config.wasm_tail_call(true);
        config.wasm_threads(true);
        let engine = Engine::new(&config)?;
        Ok(Self {
            engine,
            compile_locks: Arc::new(TokioMutex::new(HashMap::new())),
            cache_dir,
            compiler_path,
            wasm_dir,
            wasm_bytes_cache: Arc::new(TokioMutex::new(HashMap::new())),
            active_session: Arc::new(TokioMutex::new(None)),
        })
    }

    /// Sticky Session-enabled parser execution that caches instantiation.
    pub async fn parse_code(&self, ext: &str, code: &str) -> Result<(tree_sitter::Tree, tree_sitter::Language)> {
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
        debug!("Active session miss for {}. Re-instantiating parser...", wasm_name);
        
        if !self.cache_dir.exists() {
            fs::create_dir_all(&self.cache_dir)
                .context("Failed to create cache directory")?;
        }

        let existing_cache = self.find_cached_cwasm(wasm_name)?;
        
        let mut wasm_store = WasmStore::new(self.engine.clone())
            .context("Failed to create WasmStore")?;

        let _cwasm_path = if let Some(cache_path) = existing_cache {
            debug!("Cache hit for {}: {:?}", wasm_name, cache_path);
            
            let pm_clone = self.clone_manager();
            let wasm_name_str = wasm_name.to_string();
            let cache_path_clone = cache_path.clone();
            tokio::spawn(async move {
                if let Err(e) = pm_clone.revalidate_and_compile(&wasm_name_str, Some(cache_path_clone)).await {
                    warn!("Background revalidation failed for {}: {:?}", wasm_name_str, e);
                }
            });

            cache_path
        } else {
            info!("Cache miss for {}. Compiling synchronously...", wasm_name);
            self.revalidate_and_compile(wasm_name, None).await?
        };

        // Load WASM bytes
        let wasm_bytes = {
            let mut cache = self.wasm_bytes_cache.lock().await;
            if let Some(bytes) = cache.get(wasm_name) {
                bytes.clone()
            } else {
                let raw_wasm_path = self.wasm_dir.join(format!("{}.wasm", wasm_name));
                let bytes = fs::read(&raw_wasm_path)
                    .with_context(|| format!("Failed to read raw wasm module at {:?}", raw_wasm_path))?;
                cache.insert(wasm_name.to_string(), bytes.clone());
                bytes
            }
        };
        
        let lang_symbol = wasm_name.strip_prefix("tree-sitter-").unwrap_or(wasm_name);
        let language = wasm_store.load_language(lang_symbol, &wasm_bytes)
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

    /// Internal helper to clone manager configurations
    fn clone_manager(&self) -> Self {
        Self {
            engine: self.engine.clone(),
            compile_locks: self.compile_locks.clone(),
            cache_dir: self.cache_dir.clone(),
            compiler_path: self.compiler_path.clone(),
            wasm_dir: self.wasm_dir.clone(),
            wasm_bytes_cache: self.wasm_bytes_cache.clone(),
            active_session: self.active_session.clone(),
        }
    }

    /// Finds any precompiled cwasm module under the cache directory starting with target prefix.
    fn find_cached_cwasm(&self, grammar_prefix: &str) -> Result<Option<PathBuf>> {
        if !self.cache_dir.exists() {
            return Ok(None);
        }

        for entry in fs::read_dir(&self.cache_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
                if let Some(filename) = path.file_name().and_then(|f| f.to_str()) {
                    if filename.starts_with(grammar_prefix) && filename.ends_with(".cwasm") {
                        return Ok(Some(path));
                    }
                }
            }
        }
        Ok(None)
    }

    /// Calculates SHA256 of raw WASM binary
    fn calculate_wasm_hash(&self, wasm_path: &Path) -> Result<String> {
        let mut file = File::open(wasm_path)
            .with_context(|| format!("Failed to open raw wasm file: {:?}", wasm_path))?;
        let mut hasher = Sha256::new();
        let mut buffer = [0; 4096];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 { break; }
            hasher.update(&buffer[..count]);
        }
        Ok(hex::encode(hasher.finalize()))
    }

    /// Revalidates cache and compiles via external Cranelift compiler
    async fn revalidate_and_compile(&self, grammar_name: &str, existing_cache: Option<PathBuf>) -> Result<PathBuf> {
        let lock = self.get_compile_lock(grammar_name).await;
        let _guard = lock.lock().await;

        let raw_wasm_path = self.wasm_dir.join(format!("{}.wasm", grammar_name));
        if !raw_wasm_path.exists() {
            bail!("Raw Wasm file not found for {}", grammar_name);
        }

        let current_hash = self.calculate_wasm_hash(&raw_wasm_path)?;

        if let Some(ref cache_path) = existing_cache {
            if let Some(filename) = cache_path.file_name().and_then(|f| f.to_str()) {
                let parts: Vec<&str> = filename.split('.').collect();
                if parts.len() >= 3 && parts[parts.len() - 2] == current_hash {
                    return Ok(cache_path.clone());
                }
            }
        }

        if let Some(ref cache_path) = existing_cache {
            let _ = fs::remove_file(cache_path);
        }

        let new_cwasm_filename = format!("{}.{}.cwasm", grammar_name, current_hash);
        let target_path = self.cache_dir.join(new_cwasm_filename);

        info!("Compiling {} to cwasm using wasmtime-compiler...", grammar_name);

        let tmp_cwasm_path = self.cache_dir.join(format!("{}.tmp.cwasm", grammar_name));
        let _ = fs::remove_file(&tmp_cwasm_path);

        let output = tokio::process::Command::new(&self.compiler_path)
            .arg("compile")
            .arg("-o")
            .arg(&tmp_cwasm_path)
            .arg(&raw_wasm_path)
            .output()
            .await
            .context("Failed to spawn wasmtime-compiler process")?;

        if !output.status.success() {
            let err_msg = String::from_utf8_lossy(&output.stderr);
            let _ = fs::remove_file(&tmp_cwasm_path);
            bail!("Compilation process failed: {}", err_msg);
        }

        fs::rename(&tmp_cwasm_path, &target_path)
            .context("Failed to finalize compilation to cwasm path")?;

        info!("Compiled successfully: {:?}", target_path);
        Ok(target_path)
    }

    async fn get_compile_lock(&self, grammar_name: &str) -> Arc<TokioMutex<()>> {
        let mut locks = self.compile_locks.lock().await;
        locks
            .entry(grammar_name.to_string())
            .or_insert_with(|| Arc::new(TokioMutex::new(())))
            .clone()
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

    #[test]
    fn test_calculate_wasm_hash() {
        let fixture = TempTestFixture::new("calc_hash");
        let wasm_path = fixture.dir.join("test.wasm");
        fs::write(&wasm_path, b"test bytecode").unwrap();

        let manager = ParserManager::with_paths(
            fixture.dir.join("cache"),
            fixture.dir.join("compiler"),
            fixture.dir.clone()
        ).unwrap();

        let hash = manager.calculate_wasm_hash(&wasm_path).unwrap();
        assert_eq!(hash.len(), 64);
    }

    #[test]
    fn test_find_cached_cwasm() {
        let fixture = TempTestFixture::new("find_cache");
        let cache_dir = fixture.dir.join("cache");
        fs::create_dir_all(&cache_dir).unwrap();

        let manager = ParserManager::with_paths(
            cache_dir.clone(),
            fixture.dir.join("compiler"),
            fixture.dir.clone()
        ).unwrap();

        // No cache file initially
        assert!(manager.find_cached_cwasm("tree-sitter-test").unwrap().is_none());

        // Create mock cached file
        let hash = "a".repeat(64);
        let cache_file = cache_dir.join(format!("tree-sitter-test.{}.cwasm", hash));
        fs::write(&cache_file, "compiled").unwrap();

        let found = manager.find_cached_cwasm("tree-sitter-test").unwrap().unwrap();
        assert_eq!(found, cache_file);
    }

    #[tokio::test]
    async fn test_revalidate_and_compile_success() {
        let fixture = TempTestFixture::new("compile_success");
        let cache_dir = fixture.dir.join("cache");
        let wasm_dir = fixture.dir.join("wasm");
        fs::create_dir_all(&cache_dir).unwrap();
        fs::create_dir_all(&wasm_dir).unwrap();

        let wasm_path = wasm_dir.join("tree-sitter-test.wasm");
        fs::write(&wasm_path, b"test wasm bytecode").unwrap();

        // Create a mock compiler script that writes mock output
        let compiler_path = fixture.dir.join("mock-compiler.sh");
        let script = r#"#!/bin/sh
# $3 is target path, $4 is input path
echo "mock cwasm output" > "$3"
"#;
        fs::write(&compiler_path, script).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&compiler_path).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&compiler_path, perms).unwrap();
        }

        let manager = ParserManager::with_paths(
            cache_dir,
            compiler_path,
            wasm_dir
        ).unwrap();

        let cwasm_path = manager.revalidate_and_compile("tree-sitter-test", None).await.unwrap();
        assert!(cwasm_path.exists());
        let content = fs::read_to_string(&cwasm_path).unwrap();
        assert_eq!(content.trim(), "mock cwasm output");
    }

    #[tokio::test]
    async fn test_revalidate_and_compile_failure() {
        let fixture = TempTestFixture::new("compile_fail");
        let cache_dir = fixture.dir.join("cache");
        let wasm_dir = fixture.dir.join("wasm");
        fs::create_dir_all(&cache_dir).unwrap();
        fs::create_dir_all(&wasm_dir).unwrap();

        let wasm_path = wasm_dir.join("tree-sitter-fail.wasm");
        fs::write(&wasm_path, b"wasm bytecode").unwrap();

        let compiler_path = fixture.dir.join("mock-compiler-fail.sh");
        let script = r#"#!/bin/sh
exit 1
"#;
        fs::write(&compiler_path, script).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&compiler_path).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&compiler_path, perms).unwrap();
        }

        let manager = ParserManager::with_paths(
            cache_dir.clone(),
            compiler_path,
            wasm_dir
        ).unwrap();

        let res = manager.revalidate_and_compile("tree-sitter-fail", None).await;
        assert!(res.is_err());

        let entries = fs::read_dir(cache_dir).unwrap().count();
        assert_eq!(entries, 0);
    }
}
