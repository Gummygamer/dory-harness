//! Shared by the build scripts of dory-cli and dory-serve (`#[path]` include, not a crate).
//!
//! Sets `DORY_VERSION` to `MAJOR.MINOR.COMMITS+HASH[.dirty]`, for example `0.1.19+d70498f`:
//! MAJOR.MINOR come from the crate's Cargo.toml version (bump them by hand for a real release),
//! COMMITS is the number of commits reachable from HEAD, so the patch number rises with every commit
//! and nothing has to be edited. `.dirty` marks a build with uncommitted changes to tracked files.
//! Without git (a source tarball) it falls back to the plain Cargo.toml version plus `+nogit`.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn emit() {
    let cargo_version = std::env::var("CARGO_PKG_VERSION").expect("cargo sets CARGO_PKG_VERSION");
    let mut parts = cargo_version.split('.');
    let (major, minor) = (parts.next().unwrap_or("0"), parts.next().unwrap_or("0"));

    let version = match (
        git(&["rev-list", "--count", "HEAD"]),
        git(&["rev-parse", "--short=7", "HEAD"]),
    ) {
        (Some(count), Some(hash)) => {
            let dirty = git(&["status", "--porcelain", "--untracked-files=no"])
                .is_some_and(|s| !s.is_empty());
            format!(
                "{major}.{minor}.{count}+{hash}{}",
                if dirty { ".dirty" } else { "" }
            )
        }
        _ => format!("{cargo_version}+nogit"),
    };
    println!("cargo:rustc-env=DORY_VERSION={version}");

    // Re-run when HEAD moves, the index changes, or any source changes (so `.dirty` stays accurate).
    for path in ["HEAD", "index", "packed-refs"] {
        if let Some(p) = git(&["rev-parse", "--git-path", path]) {
            println!("cargo:rerun-if-changed={p}");
        }
    }
    if let Some(head_ref) = git(&["symbolic-ref", "-q", "HEAD"])
        && let Some(p) = git(&["rev-parse", "--git-path", &head_ref])
    {
        println!("cargo:rerun-if-changed={p}");
    }
    if let Some(root) = git(&["rev-parse", "--show-toplevel"]) {
        println!("cargo:rerun-if-changed={root}/crates");
        println!("cargo:rerun-if-changed={root}/Cargo.toml");
    }
}
