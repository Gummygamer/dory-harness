//! Adds an rpath to the llama.cpp library directory so binaries run without LD_LIBRARY_PATH,
//! and sets DORY_VERSION from git (see build_version.rs).

#[path = "../../build_version.rs"]
mod build_version;

fn main() {
    if let Ok(dir) = std::env::var("DEP_LLAMA_LIB_DIR") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{dir}");
    }
    build_version::emit();
}
