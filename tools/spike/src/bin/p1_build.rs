//! P1: prove llama.cpp builds with Metal and a model loads.
use std::path::PathBuf;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::model::LlamaModel;
use llama_cpp_2::model::params::LlamaModelParams;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from("models/qwen2.5-0.5b-instruct-q4_k_m.gguf");
    println!("loading {}", path.display());
    let backend = LlamaBackend::init()?;
    let params = LlamaModelParams::default().with_n_gpu_layers(999);
    let model = LlamaModel::load_from_file(&backend, &path, &params)?;
    println!("OK: model loaded. n_ctx_train={}", model.n_ctx_train());
    Ok(())
}
