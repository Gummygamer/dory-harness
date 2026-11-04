# Plan: rename dory-harness to Rillwright

**Status:** Implemented locally as Rillwright per the user’s instruction to execute this plan.
**Prepared:** 2026-11-04.
**Recommended product name:** **Rillwright**.
**Repository slug:** `rillwright`.
**Short command name:** `rillwright`.

## 1. Establish the name and scope

Use **Rillwright** as the proposed application and project name. It combines “rill” (a small stream) with “wright” (a maker), suggesting a tool for building things while giving the coding agent, desktop app, CLI, and local inference service one identity independent of a model vendor or architecture. The name contains no NVIDIA, Nemotron, Dory, model version, or parameter count. Keep the current model in the supported-model documentation so future backend changes do not require another product rename.

The naming criteria are: distinctive enough to identify this project, easy to say and type, usable as a lowercase executable/package prefix, and independent of the currently pinned model. Rillwright is a concrete proposal for this plan, not a final name selected by the user. Alternatives can replace it throughout the mapping before implementation.

Use this description in the README and application metadata:

> A local coding agent and desktop workspace, built in Rust.

Follow that product description with a separate implementation note:

> Currently supports NVIDIA Nemotron Labs Dory 1.0 4B through a pinned llama.cpp fork.

The upstream model identifier is confirmed by [NVIDIA's GGUF model card](https://huggingface.co/nvidia/NVIDIA-Nemotron-Labs-Dory1.0-4B-GGUF). The repository already pins a specific model checksum and llama.cpp fork commit. This rename must preserve those pins and the current inference behavior. A model-independent product name does not mean the current implementation supports arbitrary models; do not advertise broader model support as part of the rename.

### Collision check performed on 2026-11-04

The earlier **Alderwork** proposal is withdrawn. There are two concrete reasons to avoid that naming direction:

- **Aider already exists in this category.** Its [official site](https://aider.chat/) and [repository](https://github.com/Aider-AI/aider) describe an AI coding assistant. Avoid `Aider`, `AIder`, and visually similar branding; letter case does not create a distinct identity.
- **Alderwork has an exact relevant collision.** [Alderwork/eme](https://github.com/Alderwork/eme) is an existing tool for managing AI coding agents. This is stronger evidence against the proposed name than similarity to Aider alone.

For the replacement **Rillwright**, these checks were actually run:

- A public [GitHub repository-name query](https://api.github.com/search/repositories?q=rillwright%20in%3Aname&per_page=10) returned HTTP 200 with `total_count: 0`. The [GitHub user/organization lookup](https://api.github.com/users/rillwright) returned HTTP 404.
- Exact crates.io lookups returned HTTP 404 for the root name and every planned package: [rillwright](https://crates.io/api/v1/crates/rillwright), [rillwright-sys](https://crates.io/api/v1/crates/rillwright-sys), [rillwright-engine](https://crates.io/api/v1/crates/rillwright-engine), [rillwright-agent](https://crates.io/api/v1/crates/rillwright-agent), [rillwright-app](https://crates.io/api/v1/crates/rillwright-app), [rillwright-cli](https://crates.io/api/v1/crates/rillwright-cli), [rillwright-gui](https://crates.io/api/v1/crates/rillwright-gui), and [rillwright-serve](https://crates.io/api/v1/crates/rillwright-serve).
- Exact package lookups on [PyPI](https://pypi.org/pypi/rillwright/json) and [npm](https://registry.npmjs.org/rillwright) returned HTTP 404. These are cross-ecosystem collision checks; this project does not need to publish Python or npm packages.
- General web searches for the exact name and software/coding-agent variants found no relevant software product. Unrelated personal-name and historical text matches appeared, so the claim is **no relevant software collision found**, not that the word has never been used.

These results support Rillwright as a lower-conflict proposal within the checked public software namespaces. They do not reserve a name or prove every unpublished/private project is absent. Domain availability and trademark registries were not checked. Repeat repository and registry checks immediately before adopting/publishing the name, including close spellings and hyphen/underscore variants. If a new relevant conflict appears, revise the complete mapping below before editing code.

Use the project's own attribution; describe NVIDIA as the model provider without suggesting NVIDIA publishes this application.

**Scope:** project identity, Rust package and module names, executables, GUI branding, distribution names, documentation, and compatibility for existing local data. Model support, dependency upgrades, inference settings, permissions, and tool behavior remain separate work.

**Done when:** one approved naming map covers every surface, with an explicit list of compatibility names to retain.

## 2. Record the starting state and protect existing work

1. Read applicable `AGENTS.md` instructions and inspect `git status --short` and `git diff` before implementation.
2. At planning time, the workspace already contains changes to `Cargo.lock`, `crates/dory-gui/Cargo.toml`, and `crates/dory-gui/src/main.rs`, plus untracked `crates/dory-gui/assets/` and `dist/`. Recheck this list; integrate the rename with that work without reverting, overwriting, or accidentally committing unrelated changes.
3. Inventory tracked occurrences and untracked application assets separately:

   ```sh
   git grep -n -i -E 'dory|nemotron|harness'
   rg --files --hidden -g '!.git' -g '!target' -g '!dist'
   ```

4. Classify each occurrence as product branding, a Rust identifier, a command/path, a persisted contract, an upstream model identifier, or historical evidence. Do not perform a global replacement of `dory`.
5. Record baseline formatting, compilation, and relevant test results using the existing pinned native-library checkout. Record environment failures separately from code failures.
6. Use temporary configuration/data/workspace directories for migration checks. Before any manual test against real user data, make a local backup of settings and relevant transcripts; do not add these files to Git.

**Done when:** the initial diff and baseline are understood, and every replacement belongs to an identified category.

## 3. Apply one naming map

| Surface | Current name | Planned name |
|---|---|---|
| Project/display name | `dory-harness` / `Dory` | `Rillwright` |
| Repository/folder slug | `dory-harness` | `rillwright` |
| FFI crate/directory | `dory-sys` | `rillwright-sys` |
| Engine crate/directory | `dory-engine` | `rillwright-engine` |
| Agent crate/directory | `dory-agent` | `rillwright-agent` |
| Shared app crate/directory | `dory-app` | `rillwright-app` |
| CLI crate/directory | `dory-cli` | `rillwright-cli` |
| GUI crate/directory | `dory-gui` | `rillwright-gui` |
| Server crate/directory | `dory-serve` | `rillwright-serve` |
| CLI executable | `dory-cli` (help currently says `dory`) | `rillwright` |
| GUI executable | `dory-gui` | `rillwright-gui` |
| Server executable | `dory-serve` | `rillwright-serve` |
| Linux GUI bundle | `dory-harness-linux-x86_64` | `rillwright-linux-x86_64` |
| New GUI settings directory | `dory-harness` | `rillwright` |
| New plain-chat data directory | `dory-harness-chat` | `rillwright-chat` |
| Default diagnostics export | `dory-diagnostics.json` | `rillwright-diagnostics.json` |
| Canonical model variable | `DORY_MODEL` | `RILLWRIGHT_MODEL` (legacy alias retained) |
| Canonical native-fork variable | `DORY_LLAMA_DIR` | `RILLWRIGHT_LLAMA_DIR` (legacy alias retained) |
| Debug web-provider override | `DORY_WEB_BASE_URL` | `RILLWRIGHT_WEB_BASE_URL` (legacy alias retained) |

Rust imports replace hyphens with underscores: for example, `dory_engine` becomes `rillwright_engine`. Rename the GUI's internal `DoryApp` type to `RillwrightApp` for consistency; this does not change any saved-data schema.

Retain these compatibility and upstream identifiers in the first renamed release:

- `DORY_MODEL` and `DORY_LLAMA_DIR`: legacy aliases for the canonical `RILLWRIGHT_*` variables. New documentation uses the canonical names; existing setups continue working.
- `DORY_WEB_BASE_URL`: legacy alias for `RILLWRIGHT_WEB_BASE_URL`; preserve the existing debug-only override and ignore both names in release builds.
- `DORY_VERSION` and `DORY_FORK_COMMIT`: internal build-time variables, updated consistently only if a later change needs it.
- `<workspace>/.dory/`, session IDs, transcript schemas, session locks, and web-provider quota state.
- Server model ID `dory`, `/v1/models`, completion response model fields, and existing API request behavior.
- GGUF metadata keys such as `dory.num_loops`, `dory.recurrent_kv_cache_mode`, and `dory.block_count`; model filenames, template hashes, checksum pins, and upstream URLs.
- External checkout name `llama.cpp-dory`, native library names, and the `llama` Cargo native-link identity.
- Evaluation artifacts ending in `.dory.json`, script option `--dory`, and saved reference fixtures. Update their executable defaults, not their data contract.

These retained names are intentional. The goal is a recognizable application identity without silently changing model formats, external integrations, or stored sessions.

For environment aliases, the canonical variable takes precedence when nonempty; otherwise use the nonempty legacy value. Preserve explicit `--model` precedence over either environment variable, and saved-model fallback after them in the GUI. Share the runtime model-resolution helper across CLI and GUI where practical. Apply the same canonical/legacy rule in the native build script and packaging script, and emit `cargo:rerun-if-env-changed` for both native-fork variable names. Diagnostics and errors should mention the canonical names and document legacy aliases. Keep build-time `DORY_VERSION`/`DORY_FORK_COMMIT` internal for this release.

## 4. Rename the Rust workspace as one coherent change

1. Move all seven crate directories according to the map. Move the current GUI assets with the GUI crate, including untracked assets that are needed by its existing `include_bytes!` reference.
2. Update root `Cargo.toml` workspace members and each crate's `[package].name`.
3. Update every local dependency key and relative `path` across the seven manifests. Keep dependency versions and features unchanged, including the existing GUI image dependency.
4. Update Rust imports, qualified module paths, doctest examples, integration tests, and investigation utilities under `investigation/tools/`.
5. Add an explicit `[[bin]]` declaration to the CLI manifest with `name = "rillwright"` and `path = "src/main.rs"`. Its package is `rillwright-cli`; Cargo commands select that package while users run `rillwright`. The GUI/server default executable names follow their renamed packages.
6. Check build-script paths. The shared `../../build_version.rs` include and `../../../llama.cpp-dory` fallback retain the same depth after moving crate directories. Add canonical/legacy native-fork environment resolution as specified above. Preserve `DEP_LLAMA_LIB_DIR`, rpath handling, and fork verification.
7. Rename product-related worker-thread labels and their error labels in the engine and app controller. Preserve labels that identify model metadata.
8. Let Cargo update `Cargo.lock` from the renamed manifests, then inspect the diff. Expect local workspace package names/dependency references to change; do not use `cargo update` or introduce unrelated dependency upgrades.
9. Verify that workspace metadata lists seven renamed packages and the expected executable targets:

   ```sh
   cargo metadata --no-deps --format-version 1
   cargo fmt --all --check
   cargo check --workspace --all-targets --locked
   ```

**Done when:** all seven packages resolve, all source imports use the new crate names, and the pinned native integration still builds.

## 5. Update CLI, server, and GUI identity

1. In the renamed CLI `src/main.rs`, change Clap's `name = "dory"` to `rillwright`; update the description, CLI module documentation, and interactive `dory>` prompt in both normal chat and `agent_cmd.rs`.
2. In the server, change the command name and help description to the new server executable. Keep `MODEL_ID = "dory"` and all existing API behavior. Application branding and a served model identifier have different purposes.
3. In the GUI `src/main.rs`, update version output, argument-error prefixes, usage text, and the `eframe::run_native` title to the mapped names.
4. In GUI `src/app.rs`, update the application heading, message placeholder, close confirmation, and default diagnostics filename to Rillwright. Present the loaded model name in model selection, status, and diagnostics rather than as the application title.
5. Keep assistant/message roles such as `Dory` when they describe the model. If changing a visible role label, update all corresponding string comparisons together; current rendering logic compares role strings, so an isolated replacement could change formatting.
6. Preserve the existing PNG icon and update its filename/reference only if needed for consistency. Review it for embedded old project text before deciding whether a new asset is needed; generating a new logo is separate work.
7. Audit window identity and any framework-managed persistence affected by the changed application title. If it creates a new framework storage location, preserve relevant old preferences or document the cosmetic reset separately from application settings.
8. Keep the agent system prompt, model template, tool names, schemas, and generation parameters unchanged. Branding should not alter the prompt or model outputs.

**Done when:** help, version output, errors, window titles, and headings use the new identity while model-specific text remains accurate.

## 6. Preserve settings and existing conversation history

Current settings live under `${XDG_CONFIG_HOME:-~/.config}/dory-harness/settings.json`; plain chat uses `${XDG_DATA_HOME:-~/.local/share}/dory-harness-chat`. Their path builders are in GUI `src/settings.rs` and app `src/controller.rs`, respectively.

### Settings

1. Introduce explicit canonical and legacy settings paths, retaining the existing XDG/HOME/temp-directory fallback behavior.
2. Read canonical settings first. Read legacy settings only when the canonical file is absent; a malformed or unreadable canonical file must produce its own warning rather than silently resurrecting stale preferences.
3. Continue validating settings version 1 and use the existing deserialization defaults. A directory rename does not require a schema-version bump.
4. After loading valid legacy settings, save a copy to the canonical location using the existing temporary-file/rename approach. Leave the original intact. Report a migration write failure without discarding the loaded settings or erasing old files.
5. Write future settings only to the canonical path. Preserve model/workspace paths, loop count, permissions, sandbox/network preferences, theme, and web configuration exactly. Do not start storing API keys.

### Plain chat

1. Resolve one chat storage root when starting the application/controller and reuse it for listing, loading, and creating conversations.
2. If the legacy chat directory exists, keep using it for this compatibility release so existing history, session locks, and new conversations stay together. Fresh installations use `rillwright-chat`.
3. If both roots exist, report the conflict and use the legacy root consistently for this release; leave the other root untouched. Do not merge, duplicate, or silently move live transcript trees. Document how a user can consolidate them later with the application closed.
4. Handle inaccessible storage as an error/warning, not as an empty history that silently starts writing somewhere else.
5. Keep workspace-agent history in `.dory/sessions/`. Preserve `.dory` in `PROTECTED_DIRS`, `.gitignore`, secret/path checks, command-output paths, and web-state paths. Keeping the same location preserves existing writer-lock behavior across old and new frontends.

**Done when:** a fresh installation uses the new settings/chat identity, and an existing installation opens its saved preferences and history without losing or duplicating sessions.

## 7. Update scripts and local packaging

1. Replace hardcoded executable/package defaults in `cli_acceptance.sh`, `chat_acceptance.sh`, `agent_acceptance.sh`, `bench_grid.sh`, `loop_matrix.sh`, `compare_reference.py`, `compare_generation.py`, `logit_probe.py`, `gen_long_case.py`, `serve_openai_check.py`, and `scripts/agent_eval/run_eval.py`.
2. Update both release `BIN` and debug `WEB_BIN` defaults. Move first-party script configuration to `RILLWRIGHT_MODEL`, `RILLWRIGHT_LLAMA_DIR`, and `RILLWRIGHT_WEB_BASE_URL`, with legacy fallbacks wherever scripts consume the old names. The fake web provider must still use a debug build; do not let an acceptance check accidentally use live search.
3. Preserve existing caller overrides (`BIN`, `WEB_BIN`, `--bin`, `--dory`, and others), loop/cache settings, reference output names, and expected JSON/event schemas.
4. Change `scripts/package_gui.sh` to build `rillwright-gui`, stage `rillwright-linux-x86_64`, copy `rillwright-gui.bin`, write launcher `rillwright-gui`, and produce the correspondingly named `.tar.gz`.
5. Keep the bundle's native libraries and launcher `LD_LIBRARY_PATH` behavior unchanged. Keep the model external.
6. Include a small compatibility launcher named `dory-gui` in the GUI bundle for the transition release, forwarding all arguments to `rillwright-gui`. Document opt-in `dory-cli`/`dory-serve` symlinks for existing CLI installations; do not overwrite installed executables automatically. Old Cargo package names are not compatibility aliases.
7. Regenerate the new bundle from source. Do not hand-edit or delete the existing untracked `dist/` tree; distinguish old and new artifacts when checking the result.
8. Inspect any existing machine-local desktop entry or launcher as a separate installation concern. If updated, point `Name`, `Exec`, `TryExec`, and `Icon` at actual new bundle paths, preserve argument forwarding, and avoid assuming a desktop file is currently tracked in this repository.

**Done when:** scripts find the renamed executables and an extracted bundle launches from outside the repository, including from a directory containing spaces.

## 8. Update documentation without rewriting evidence

1. Change the README heading, description, crate list, build/run commands, executable examples, and packaging instructions. Correct the existing discrepancy between the `dory` name in CLI help and the actual executable name.
2. Add a concise “Renamed from dory-harness” note with old/new executable names, retained environment variables, settings compatibility, chat storage behavior, and `.dory` session location.
3. Update `docs/GUI.md` with the new title, commands, package paths, diagnostics name, settings precedence, and history compatibility behavior.
4. Review the other files under `docs/`. Update instructions that are still intended to be run. Keep historical plans, measured results, recorded command output, and upstream references intact; add a short rename note pointing to this plan where old crate paths could confuse a reader.
5. Leave historical investigation logs, saved generation outputs, model filenames, upstream PR references, and GGUF keys unchanged. Rename a source-file import in an investigation tool if needed to compile it, while preserving its recorded evidence.
6. Rerun the name inventory. Classify every remaining old product-name occurrence as a compatibility path, historical note, or stale reference to fix. Exclude the mapping/history in this plan from any zero-occurrence requirement.

**Done when:** a new contributor can build, run, and package the renamed project using only current documentation.

## 9. Verify the implementation in layers

Run these checks during implementation; none are claimed as completed by this planning document. Supply `RILLWRIGHT_LLAMA_DIR` (or the legacy alias) and the matching native library search path as required by the existing build.

### Compile and command identity

```sh
cargo fmt --all --check
cargo check --workspace --all-targets --locked
cargo test --workspace --locked
cargo build --release --locked -p rillwright-cli -p rillwright-serve -p rillwright-gui
target/release/rillwright --help
target/release/rillwright --version
target/release/rillwright-serve --help
target/release/rillwright-serve --version
target/release/rillwright-gui --version
```

The default test run leaves ignored model tests unexecuted. Check that help/version operations work without loading a model and preserve the `MAJOR.MINOR.COMMITS+HASH[.dirty]` version scheme; binary output names should match the map.

### Compatibility checks with temporary data

Add focused tests for the new path-resolution/migration behavior, where silent failures could hide history or reset policy. Cover fresh paths, legacy-only settings, canonical precedence, malformed canonical settings, unsupported settings versions, failed canonical writes, legacy chat selection, both chat roots present, inaccessible directories, and unchanged `.dory` session resume/locking. Pass base paths into helpers so tests do not race by mutating process-wide XDG variables.

Test environment resolution with canonical-only, legacy-only, both set, empty canonical values, and explicit CLI overrides. Check native-fork resolution through both names. Verify that both web override aliases work in debug builds and neither affects release builds. Prefer helpers receiving values over tests that mutate the process environment.

Reuse the existing settings, agent-session, workspace-protection, and web-state checks. Avoid adding tests that merely repeat every renamed string or import.

### Desktop and bundle checks

Run `rillwright-gui --demo` and inspect the title, heading, prompts, Settings, History, diagnostics export, and close dialog. Verify that an existing profile still exposes its model/workspace preferences and conversations. Extract the generated bundle into a temporary location and test the canonical and legacy GUI launchers with `--version`, `--demo`, and forwarded workspace/model arguments.

### Real-model checks

With the existing pinned GGUF and native fork available, run the relevant CLI/chat/agent acceptance scripts, the OpenAI server compatibility check, and a GUI load/resume smoke check. Run ignored GPU integration tests serially to avoid competing model loads:

```sh
cargo test -p rillwright-engine --test engine_gpu --locked -- --ignored --test-threads=1
```

Confirm unchanged server model ID `dory`, tool/JSON schemas, permission behavior, and session resume. Reuse saved renderer/generation cases for a representative before/after comparison with identical flags. Serialize model-loading checks; do not run the CLI, GUI, server, or reference server against limited GPU memory simultaneously. If a model or native dependency is unavailable, record exactly which checks remain unrun rather than calling the rename fully validated.

**Done when:** build and default tests pass, compatibility cases pass, visible branding is reviewed, and real-model verification has a clear result or a recorded blocker.

## 10. Rename repository hosting and the local checkout last

1. Complete source changes and verification while the current repository directory still exists. A local folder rename during edits could invalidate editor/task paths.
2. Inventory the actual Git remote and any hosting integrations before preparing host-specific instructions. This plan does not assume a particular remote, existing release workflow, or published crate.
3. If repository hosting is being renamed, change its slug to `rillwright`, update its description, and update local remote URLs. Check the hosting provider's redirect behavior and update known clone/release/documentation links instead of relying solely on redirects.
4. Check external consumers: pinned checkout paths, scripts, desktop entries, CI settings, release download URLs, and any locally installed executable aliases. Update only integrations that exist.
5. Rename the local checkout folder from its parent after closing affected processes. Keep `llama.cpp-dory` in place. Check ignored `.cargo/` configuration for an absolute `target-dir` that still points at the old checkout.
6. Reopen the project at the new path and verify Cargo metadata, native-fork discovery, a build, and a normal launch. The relative fork fallback should still resolve when only the repository's sibling name changes.

Hosting changes, publishing, and machine-wide installation are later execution steps, not actions performed by writing this plan.

**Done when:** source, hosting links where applicable, local paths, and installed launchers point to the same identity.

## 11. Keep the rollout reviewable and reversible

Use separately reviewable changes in this order: (1) workspace/package/import rename, (2) executable and visible identity plus script/package updates, (3) settings/history compatibility and focused tests, (4) documentation and release notes. Keep each committed state coherent; fold coupled edits together when needed to avoid broken intermediate commits. Change hosting and local checkout paths after source verification.

Rollback source with targeted reverts of the rename changes, preserving unrelated work. Retain the original bundle and legacy settings. Because agent history stays in `.dory` and existing plain chat stays in its legacy root, rolling back does not require moving those histories. Canonical settings saved after the rename can diverge from the preserved legacy copy; reconcile them explicitly while both application versions are closed. Fresh-install chat data in the new root must also be retained if rolling back to an older build that cannot discover it.

Final implementation checklist:

- [x] Initial public collision research is recorded; Aider and Alderwork are excluded and Rillwright's planned Rust package names were checked.
- [ ] Recheck the proposed name and package/binary namespace before adoption, and finalize the naming map.
- [x] Seven crate directories, package names, dependencies, and Rust imports agree.
- [x] CLI help matches the actual executable; GUI and server show the new product identity.
- [x] Public configuration uses `RILLWRIGHT_*`; legacy aliases and canonical-over-legacy helper precedence are implemented and unit checked.
- [x] Packaging and scripts use the new commands; the GUI compatibility launcher was exercised from a path containing spaces.
- [x] Existing settings migrate by copy; existing chat history selects its legacy root, and agent history remains under `.dory/`.
- [x] Model pins, metadata keys, prompt bytes, API identity, permissions, and debug-only web behavior are preserved.
- [x] Current documentation is updated; historical evidence and retained names are explained.
- [x] Validation results and unrun checks are recorded below.
- [x] GitHub repository, local remote, checkout folder, and ignored Cargo target path were renamed and checked after the move.


## Execution record (2026-11-04)

The user directed execution of this plan, so the Rillwright proposal was adopted. The seven workspace packages and source directories were renamed, the repository was renamed to `Gummygamer/rillwright`, and the checkout is now `src/rillwright`. The uncommitted GUI image dependency, icon asset, and existing `dist/` contents were retained; the new bundle is `dist/rillwright-linux-x86_64.tar.gz`. The ignored `.cargo/config.toml` target directory was updated to the new absolute checkout path. No commit or push was made.

Validation completed: Cargo metadata lists seven renamed packages and expected binary targets; `cargo fmt --all --check`, workspace checks, and workspace tests passed with the pinned native library directory available; release help/version commands passed; settings migration and chat-root compatibility tests passed; the new and legacy GUI bundle launchers both printed the expected version when extracted beneath a path containing spaces. The initial workspace test attempt without `LD_LIBRARY_PATH` failed to load `libllama.so.0`; it passed when run with the pinned fork’s `build/bin` directory.

Still unverified: a visible GUI `--demo` review, real-model/GPU acceptance and ignored GPU tests, exhaustive canonical/legacy environment precedence cases, trademark/domain checks, and publication availability of all planned crate names (the fresh crates.io API lookup returned HTTP 403). The GUI title was not inspected in a live window, no model GGUF was used, and the GPU acceptance suite was not run. The plan’s exact package/binary namespace recheck, exhaustive `--model`/environment matrix, trademark/domain checks, and host machine installation/desktop-entry inspection remain unverified.
