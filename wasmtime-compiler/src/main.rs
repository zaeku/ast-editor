use std::env;
use std::fs;
use wasmtime::{Engine, Config};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 5 || args[1] != "compile" || args[2] != "-o" {
        eprintln!("Usage: wasmtime-compiler compile -o <output> <input>");
        std::process::exit(1);
    }
    let output_path = &args[3];
    let input_path = &args[4];
    
    // Wasmtime engine with Cranelift enabled (default configuration)
    let mut config = Config::new();
    config.strategy(wasmtime::Strategy::Cranelift);
    let engine = Engine::new(&config)?;
    
    // Compile to AOT byte code (cwasm)
    let cwasm_bytes = engine.precompile_module(&fs::read(input_path)?)?;
    
    // Write serialized cwasm
    fs::write(output_path, cwasm_bytes)?;
    Ok(())
}
