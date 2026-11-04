//! Generates raw bindings for the llama.cpp fork and links against its `libllama`.
//!
//! The fork location defaults to `../../../llama.cpp-dory` relative to this crate
//! (i.e. `<drive>/src/llama.cpp-dory`) and can be overridden with `DORY_LLAMA_DIR`.
//! The fork must already be built (`build/bin/libllama.so`).

use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-env-changed=RILLWRIGHT_LLAMA_DIR");
    println!("cargo:rerun-if-env-changed=DORY_LLAMA_DIR");
    println!("cargo:rerun-if-env-changed=LIBCLANG_PATH");

    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let fork = match env::var("RILLWRIGHT_LLAMA_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| env::var("DORY_LLAMA_DIR").ok().filter(|s| !s.is_empty()))
    {
        Some(p) => PathBuf::from(p),
        None => manifest.join("../../../llama.cpp-dory"),
    };
    let fork = fork.canonicalize().unwrap_or_else(|e| {
        panic!(
            "llama.cpp fork not found at {}: {e} (set RILLWRIGHT_LLAMA_DIR; DORY_LLAMA_DIR is supported)",
            fork.display()
        )
    });

    let lib_dir = fork.join("build/bin");
    let lib = lib_dir.join("libllama.so");
    assert!(
        lib.exists(),
        "{} missing: build the fork first",
        lib.display()
    );

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=dylib=llama");
    // Only this crate's own targets get the rpath from rustc-link-arg, so the library directory
    // is also exported for dependents (DEP_LLAMA_LIB_DIR), which add the rpath themselves.
    println!("cargo:lib_dir={}", lib_dir.display());
    println!(
        "cargo:rerun-if-changed={}",
        fork.join("include/llama.h").display()
    );

    // Stamp the commit the library was built from; the engine compares it with the pin.
    let head = Command::new("git")
        .arg("-C")
        .arg(&fork)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=DORY_FORK_COMMIT={head}");

    let bindings = bindgen::Builder::default()
        .header(fork.join("include/llama.h").to_string_lossy())
        .clang_arg(format!("-I{}", fork.join("include").display()))
        .clang_arg(format!("-I{}", fork.join("ggml/include").display()))
        .allowlist_function("llama_.*")
        .allowlist_type("llama_.*")
        .allowlist_var("LLAMA_.*")
        .generate_comments(false)
        .derive_debug(true)
        .derive_default(true)
        .layout_tests(true)
        .generate()
        .expect("bindgen failed to generate llama.h bindings");

    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out.join("bindings.rs"))
        .expect("could not write bindings.rs");
}
