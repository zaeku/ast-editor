use ast_editor::parser::ParserManager;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let wasm_dir = manifest_dir.join("resources").join("wasm");
    let tmp = std::env::temp_dir().join("ast_editor_scratch_test");
    let cache_dir = tmp.join("cache");
    let compiler_path = tmp.join("compiler");
    
    let pm = ParserManager::with_paths(cache_dir, compiler_path, wasm_dir)?;
    println!("Loading bash parser...");
    let (tree, _lang) = pm.parse_code("sh", "hello() {\n  echo \"World\"\n}\n").await?;
    println!("Success! Tree: {:?}", tree.root_node().to_sexp());
    Ok(())
}
