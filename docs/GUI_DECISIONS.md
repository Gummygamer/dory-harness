# Desktop GUI implementation decisions

Recorded 2026-11-04 while executing `GUI_PLAN.md`.

## Revised startup workflow

The launch and session choices below supersede the earlier manual-start items in this document. The earlier entries are retained as historical implementation notes.

- Open Agent by default. When a model and workspace resolve, load the model in the worker and create a fresh Agent session after model readiness is acknowledged.
- Resolve model paths in order: `--model`, nonempty `DORY_MODEL`, saved preference, then model picker. Keep the launch source separate from the saved preference.
- Resolve workspaces in order: `--workspace`, meaningful inherited working directory, valid saved workspace, then folder chooser. The home directory and filesystem root require an explicit project choice unless passed directly.
- Chat remains an explicit mode without workspace tools. Navigation to Settings and other pages preserves the active transcript; switching modes retains each mode's current session. New conversation creates a new transcript for the selected mode and workspace.
- Permission and sandbox selections are visible in Agent. Settings changes use revision acknowledgments and apply to the next eligible turn. The view reports actual sandbox enforcement before the first tool command.
- Keep the package launcher in the caller's working directory and pass through all arguments and environment values.

## First release

- Target Linux desktop first. Windows and macOS are not advertised until their native runtime and sandbox behavior are implemented and checked.
- Use Rust with `egui`/`eframe`, with the OpenGL (`glow`) renderer and explicit X11/Wayland support. Enable AccessKit, clipboard, and bundled default fonts.
- Keep one loaded model and one active turn. Chat has no workspace tools. Agent requires an explicitly selected workspace, starts in `ask` mode, and reports the effective sandbox state.
- Keep the engine, agent, transcript and tool implementations in existing libraries. A toolkit-independent `dory-app` owns background runtime state; `dory-gui` owns presentation and input.

## Baseline

- Repository baseline: `9936472` (`dory-cli` and `dory-serve` version stamping).
- Toolchain: `rustc 1.95.0`, `cargo 1.95.0`.
- Fork: `../llama.cpp-dory`, commit `1c3cdd23787750814aa21a070dc4d6dc451af871`; `build/bin/libllama.so` exists.
- Model: `/run/media/darabat/24f15d44-9691-4cf4-8b52-bc03f1e3693f/models/dory/NVIDIA-Nemotron-Labs-Dory1.0-4B-Q5_K_M.gguf`.
- `bwrap` and `/usr/lib/x86_64-linux-gnu/libclang-21.so.1` are available. The desktop session exposes both `DISPLAY=:0` and `WAYLAND_DISPLAY=wayland-0`.
- NVML reports a driver/library version mismatch. The pinned model nevertheless loaded and all nine ignored engine GPU tests passed on the RTX 3060 Laptop GPU (5,806 MiB reported).
- `cargo test --workspace -- --test-threads=1` passed with `DORY_LLAMA_DIR` and `LD_LIBRARY_PATH` set. CLI and chat acceptance passed; agent fixtures passed, including the fake-provider web-search case. Paid live web-search tests were not run.

## Toolkit check

Pinned eframe 0.37.0 (minimum Rust 1.95), with default features disabled and `glow`, `accesskit`, `clipboard`, `default_fonts`, `wayland`, and `x11` selected explicitly. The renderer is OpenGL; the UI uses standard egui text controls for keyboard input, selection, IME, and clipboard rather than custom widgets. The demo and extracted package launch, but keyboard, IME, clipboard, high-DPI, and screen-reader behavior still need hands-on validation on desktop.
