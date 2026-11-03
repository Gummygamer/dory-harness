//! Model-free checks: argument validation that must fail before any file is opened.

use dory_engine::{CacheMode, Error, LoadOptions};

fn opts(loops: u32) -> LoadOptions {
    LoadOptions {
        path: "/nonexistent/model.gguf".into(),
        loops,
        cache_mode: CacheMode::LastLoop,
        n_gpu_layers: 0,
        vocab_only: true,
    }
}

#[test]
fn zero_and_five_loops_are_rejected_before_loading() {
    for bad in [0u32, 5, 8] {
        match dory_engine::model::Model::load(&opts(bad)) {
            Err(Error::InvalidLoops(n)) => assert_eq!(n, bad),
            other => panic!("loops={bad}: expected InvalidLoops, got {other:?}"),
        }
    }
}

#[test]
fn checksum_mismatch_is_reported_with_both_hashes() {
    let dir = std::env::temp_dir().join(format!("dory-verify-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("not-the-model.gguf");
    std::fs::write(&file, b"hello").unwrap();
    match dory_engine::verify::verify_model(&file) {
        Err(Error::Checksum { expected, actual, .. }) => {
            assert_eq!(expected, dory_engine::verify::PINNED_MODEL_SHA256);
            assert_eq!(actual, "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824");
        }
        other => panic!("expected checksum error, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fork_commit_matches_pin() {
    dory_engine::verify::verify_fork().expect("linked libllama must be the pinned fork commit");
}
