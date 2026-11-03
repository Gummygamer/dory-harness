//! Adds an rpath to the llama.cpp library directory so binaries run without LD_LIBRARY_PATH.

fn main() {
    if let Ok(dir) = std::env::var("DEP_LLAMA_LIB_DIR") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{dir}");
    }
}
