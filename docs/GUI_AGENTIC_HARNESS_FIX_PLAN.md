# Plan: make Dory launch into a usable agentic workspace

**Written:** 2026-11-04.
**Status:** Implementation executed; release acceptance is partially unverified (see §7).
**Scope:** The existing Rust desktop application (`dory-gui`) and its application controller (`dory-app`).

## 1. Intended result

Launching Dory from a project with `DORY_MODEL` set should open the Agent view, load the configured model in the background, resolve the project workspace, and create an agent session automatically. The user should be able to type while initialization runs and send once the model and session are ready. Selecting a model and pressing a separate session-start button must disappear from the normal launch workflow.

The conversation should occupy the available window area, with readable messages and a full-width composer anchored at the bottom. Workspace, permission mode, sandbox selection, effective sandbox status, and model status should be visible and usable in the conversation view. A user should be able to change permission and sandbox settings without recreating the conversation or reloading the model.

Retain the existing local engine, pinned model/fork verification, agent tools, policy semantics, transcript persistence, cancellation, and approval implementation. Plain Chat remains an explicit mode for conversations without workspace tools.

## 2. Findings in the current implementation

These findings describe the working tree inspected for this plan. It contains existing changes; implementation must preserve unrelated work.

| Reported problem | Evidence in the current code | Required correction |
|---|---|---|
| Model must be loaded manually | `UserSettings::default()` reads `DORY_MODEL`, but `DoryApp::new()` only constructs a controller. `start_model_load()` is invoked by the setup button. | Resolve the startup model and schedule one background load automatically. |
| Environment path may be overridden by old settings | `UserSettings::load()` deserializes the saved `model_path`; an existing empty or stale value can supersede the environment-derived default. | Separate persisted preferences from launch-time path resolution, with documented precedence. |
| Agent requires manual initialization | `DoryApp::new()` selects Chat, creates `chat-1`, and leaves `workspace` empty. `submit_turn()` refuses Agent requests until `agent_session_started` is true. | Default to Agent, resolve a workspace, and create the session as part of startup. |
| Session readiness is unreliable | The Start button marks the session started when the command is queued, before `SessionCreated`. Clicking Agent in navigation clears that flag again. | Track pending and acknowledged session state; navigation must preserve it. |
| Large unused window area and cramped controls | The root layout nests horizontal/vertical content inside a frame. The transcript has no explicit allocation relative to the composer, messages use two-row text edits, and the composer uses three rows. | Allocate the complete content rectangle deliberately, reserve the composer, and render messages at readable heights. Confirm the exact geometry with layout measurements. |
| Policy settings are hard to find and appear ineffective | Settings has permission/sandbox selectors inside a collapsed section, while the Agent view hard-codes `Permission: ask · Sandbox: auto`. Actual `AgentStatus` is appended only to activity. | Put selectors in the Agent view and render acknowledged configuration and effective status. |
| Settings may be silently lost | `WorkerCommand::UpdateSettings` only applies when the worker is `Ready`; other states discard it without an event. | Validate, retain, and acknowledge settings updates independently of model readiness. |
| Conversation actions are inconsistent | Two New conversation buttons exist: one clears visible messages; the other creates a Chat session even from Agent. | Provide one action that creates the correct session and updates the view only after success. |
| Readiness copy is misleading | The composer always says “Load a model before sending” outside demo mode, including after a successful load. | Derive send availability and explanatory text from actual runtime/session state. |

Primary files:

- `crates/dory-gui/src/main.rs`: launch arguments and native window setup.
- `crates/dory-gui/src/settings.rs`: persisted settings and configuration conversion.
- `crates/dory-gui/src/app.rs`: startup, event handling, navigation, conversation layout, and controls.
- `crates/dory-app/src/config.rs`: load, generation, and agent configuration.
- `crates/dory-app/src/controller.rs`: worker commands, model loading, session creation, and per-turn policy/tool assembly.
- `crates/dory-app/src/protocol.rs`: runtime states and application events.
- `crates/dory-agent/src/policy.rs` and `sandbox.rs`: existing behavior to expose accurately.
- `docs/GUI.md`, `docs/GUI_DECISIONS.md`, and `scripts/package_gui.sh`: documented workflow and packaged launch behavior.

This plan supersedes the manual model/workspace/session startup workflow described in the earlier GUI plan and decisions. It does not replace their engine or persistence design.

## 3. Step-by-step implementation

### Step 1 — Define launch configuration and path resolution

**Files:** `main.rs`, `settings.rs`, and a small new startup/configuration module if needed.

1. Introduce a `LaunchOptions` value parsed once before creating the app. Preserve `--demo` and `--version`; add `--model <path>` and `--workspace <directory>` as explicit overrides. Reject malformed arguments with a useful error instead of silently ignoring them.
2. Resolve the startup model in this order: explicit `--model`, nonempty `DORY_MODEL`, saved nonempty model preference, then a model-selection recovery view. A model chosen interactively becomes the current process's explicit choice; repainting or revisiting Settings must not reapply the environment over that choice.
3. Keep the configured preference, selected effective path, and source (`argument`, `environment`, `saved`, or `picker`) separate. Show the source in model details. Do not overwrite the saved preference merely because the environment supplied this launch's path.
4. Use path-native values for filesystem operations. Avoid trimming meaningful path characters or converting a non-UTF-8 environment path to an empty string. Treat an empty environment value as absent. Do not evaluate shell expressions embedded in a path.
5. Resolve the workspace in this order: explicit `--workspace`; a meaningful inherited working directory; a valid saved workspace; otherwise a folder chooser. Treat the home directory and filesystem root as requiring a project choice unless explicitly supplied. A directory need not be a Git repository.
6. Canonicalize and validate the workspace using the same directory rules as `Workspace::new`. Display the full canonical path in details, and persist the last successfully selected workspace. Explicit invalid arguments should show their error; do not silently select another project.
7. Add persistence fields with backward-compatible defaults. Preserve existing user preferences when adding fields; introduce migration only if the schema actually requires it. Report malformed settings and recover usable defaults rather than silently hiding the problem.
8. Record the process working directory before any future launcher changes. Keep the package launcher from changing into its installation directory. Document that desktop launches may not inherit shell environment variables or a project working directory.

**Acceptance:** With an old settings file containing an empty or different model path, a nonempty `DORY_MODEL` still supplies the startup model. Launching from this repository resolves this repository as the workspace. Explicit overrides win, and an unavailable model/workspace has an actionable recovery view.

### Step 2 — Establish an explicit application lifecycle

**Files:** `app.rs`, `protocol.rs`, and `controller.rs`.

1. Replace the overlapping `runtime_ready`, `running`, and `agent_session_started` readiness decisions with explicit state owned by a small reducer/view model. Separate model state, session state, and turn state; a model can be ready while a session is still being created.
2. Represent at least: model missing/verifying/loading/ready/failed; workspace missing/validating/valid/failed; session absent/creating/ready/read-only/failed; turn idle/running/awaiting approval/cancelling.
3. Derive one `can_submit` result shared by Send and Ctrl+Enter. Agent submission requires a ready model, an acknowledged writable Agent session for the current workspace, valid settings, no pending continuation review, and no active turn. Chat uses equivalent checks without a project requirement.
4. Add correlation identifiers to new startup/configuration operations. Increment the model generation on each distinct load attempt. Ignore stale load, session, settings, and status responses rather than allowing an earlier operation to overwrite the current selection.
5. Use `SessionCreated` as confirmation of creation. A successful queue send means “creating session,” not “session ready.” Keep the current usable session until a replacement is acknowledged.
6. Retain drafts through load failures, invalid settings, queue failures, and rejected submission. Clear a submitted draft only once its text is retained by an accepted pending/active turn. Restore it if the worker rejects the turn before recording it.
7. Wake or poll the event receiver reliably during initialization as well as generation. Prefer a worker-to-UI repaint notification; if a bounded repaint interval is used, enable it only while work is pending. Avoid a stalled loading screen that updates only after mouse movement.

**Acceptance:** There is a single explanation for why Send is disabled. Repainting cannot enqueue duplicate loads or sessions. A late event cannot reactivate an obsolete project or model.

### Step 3 — Automatically load the model

**Files:** startup/configuration module, `app.rs`, and controller event handling.

1. After constructing the controller, schedule exactly one load for the resolved startup model. Do not perform model hashing, file verification, model initialization, or unload on the UI thread.
2. Reuse the existing `load_model` worker path so automatic loading performs the same fork, checksum, options, and template checks as manual loading.
3. Show stages such as “Checking model,” “Loading model,” and “Model ready,” with the selected filename and expandable path/source details. Show indeterminate progress unless the worker actually supplies measurable progress.
4. Keep the composer editable during startup. Disable sending until initialization succeeds, with a state-specific reason. Do not automatically send a draft merely because the model became ready.
5. On a missing path, offer Browse and Retry in the conversation area or model controls. On verification, native-library, or GPU failure, show the actual error and relevant configuration actions. Retry only after a user action or a changed configuration; avoid a failing reload loop.
6. Make picking a replacement model start loading automatically when idle. Disable or defer replacement during a turn with an explicit reason. Preserve the known limitation that native initialization cannot currently be interrupted safely.
7. Replace the always-visible manual setup section with compact model status and a model-details/settings surface. An empty conversation should no longer bring the setup wizard back after every new conversation.
8. Keep `--demo` isolated from automatic real model loading and real tool execution.

**Acceptance:** `DORY_MODEL=/absolute/path/to/model.gguf target/release/dory-gui` starts verification/loading without a click, and the window remains responsive. A model error leaves the draft editable and the recovery controls usable.

### Step 4 — Automatically create and preserve Agent sessions

**Files:** `app.rs`, `controller.rs`, and session-related events.

1. Open Agent by default. Remove unconditional creation of the startup Chat session.
2. Once the current model is ready and the resolved workspace is valid, enqueue one Agent session creation using the current acknowledged settings. If either dependency is missing, show its recovery controls and finish automatically when it becomes available.
3. Remove the normal Start agent session requirement. Selecting a valid workspace should be sufficient to initialize its session; no extra Start click is needed.
4. Validate the workspace as a directory before creating its transcript, and use the canonical root consistently for the system prompt, `AGENTS.md`, transcript storage, and tool context. Creation failures must not leave a misleading ready flag.
5. Preserve active session identity, messages, draft, and turn counter when navigating to Settings, History, Diagnostics, or back to Agent. Navigation alone must neither create a session nor clear its state.
6. Consolidate New conversation into one command. In Agent, create an Agent session in the same workspace with the selected settings. In Chat, create a Chat session. Replace visible messages only after creation succeeds; never merely clear the UI while retaining the old model conversation.
7. Make switching between Chat and Agent explicit and preserve each mode's session state. Ensure a Chat session's data-storage root is not accidentally treated as the next Agent workspace; currently `SessionCreated` includes a workspace/root for Chat too.
8. When changing workspace, preserve the previous transcript, create a session for the new root, and close the old writer when no longer needed. Do not silently attach existing messages to a different project.
9. Use collision-resistant session/request identifiers for a long-running GUI. Audit worker session cleanup so repeated New conversation actions do not retain unnecessary transcript writers indefinitely.
10. Keep history resume explicit. Automatic startup should create a fresh session, not resume unfinished tool calls. Preserve read-only ownership and continuation-review behavior when the user opens history.

**Acceptance:** With a valid model and project, one launch produces one writable Agent session. The first prompt runs without a session-start action. Visiting Settings and returning preserves the same session; New conversation creates a genuinely fresh Agent transcript.

### Step 5 — Make settings changes reliable and observable

**Files:** `config.rs`, `protocol.rs`, `controller.rs`, and GUI settings state.

1. Validate agent settings in the application layer, including permission, sandbox mode, limits, writable directory entries, and generation values. Replace the controller's invalid-sandbox fallback to `auto` with a reported validation error.
2. Add configuration revision IDs and applied/rejected acknowledgments. Queue acceptance must not be displayed as successful application.
3. Retain valid generation and agent settings while unloaded, loading, or failed, then apply them to the next eligible turn. Never silently discard `UpdateSettings` outside `Ready`.
4. Define immutable settings for each active turn. An edit while running or awaiting approval applies to the next turn; it must not change permissions halfway through an existing tool call or approval request. The direct Stop/approval channels remain available.
5. Ensure submission uses one validated, acknowledged settings revision. Either submit a complete immutable turn configuration or enforce revision ordering in the controller so separate update/submit queue operations cannot leave the UI and worker disagreeing.
6. Keep model-load settings distinct: context, GPU layers, cache, and similar changes require a reload. Permission, sandbox, network, and generation changes require no model reload or session reset.
7. Separate selected next-turn configuration from the active turn's effective configuration. Persist valid user changes consistently, using the existing atomic save approach; expose save failures without losing the in-memory choice.
8. Record the applied permission/sandbox configuration with each turn or configuration-change record. Session headers alone are insufficient after settings change in an existing transcript.

**Acceptance:** A permission change before model readiness survives loading. A change during a turn is visibly pending and takes effect on the following turn. Invalid input is rejected with a specific reason, and the last valid configuration remains active.

### Step 6 — Expose permission and sandbox controls in Agent

**Files:** Agent header/view, Settings view, `protocol.rs`, and controller status reporting.

1. Add visible permission and sandbox selectors beside the workspace/status controls. Reuse the same settings state as Settings; do not introduce a second independent configuration copy.
2. Label the existing permission choices clearly: Read only (`read-only`), Ask (`ask`), Auto edit (`auto-edit`), and Full (`full`). Explain their existing behavior in short help text: reads are automatic; Ask requests writes/commands; Auto edit permits writes but normally asks for commands; Full permits tool execution subject to sandbox rules. Command allow rules and web permissions retain their existing exceptions.
3. Expose sandbox choices Auto (`auto`), Bubblewrap required (`bwrap`), and None (`none`). Preserve `ask`/`auto` defaults; automatic startup must not imply Full permission or disabling the sandbox.
4. Replace the hard-coded header and session-ready strings. Show selected permission, requested sandbox mode, effective sandbox result, and any pending next-turn changes using structured state.
5. Probe sandbox capability in the worker and report it when the session/settings become ready, before the first command. Reuse the same setup/status logic for actual execution so the header and runtime cannot drift.
6. Distinguish “bwrap active,” “Auto fallback: unconfined,” “None: unconfined,” and “bwrap unavailable: commands blocked.” The current status assembly calls an unavailable explicit `bwrap` mode an unconfined fallback; correct that wording to match command refusal behavior.
7. Preserve the existing Full-mode refusal rules when a sandbox is required but unavailable. Selecting `none` remains an available explicit choice. Do not invent sandbox options unsupported by the backend.
8. Put command network access and extra writable directories in a nearby details surface. Explain that command-network isolation is provided by the active sandbox; when unconfined, the checkbox cannot promise isolation. Keep web-search permissions separate from sandboxed command network access.
9. Handle `AgentStatus` by session/revision, store it in the view model, and render it directly. Activity can also record the event, but must not be the only place to learn effective policy.

**Acceptance:** Changing a selector updates the acknowledged configuration and the next turn's behavior. The header describes actual enforcement, including unavailable sandbox cases, without requiring the activity drawer to be opened.

### Step 7 — Rebuild the conversation layout around available space

**Files:** `app.rs`; extract focused view modules where useful.

1. Replace the content-sized root arrangement with explicit full-window allocation: navigation at the left, a compact header at the top of the main area, a composer reserved at the bottom, and the transcript using the remaining rectangle. Use the pinned egui version's supported layout APIs.
2. Measure allocated rectangles during development. Verify the main panel reaches the viewport edges and that transcript/composer heights add up to the available area. Address the source of the screenshot's unused region rather than filling it with fixed-height placeholders.
3. Reserve composer/action space before laying out the transcript scroll area. An empty conversation, a long conversation, and an expanded activity drawer must all leave input visible and usable.
4. Make the composer full width, initially about four readable text lines, growing with content up to a bounded portion of the window. Support vertical resizing or internal scrolling beyond that limit; preserve multiline entry, selection, clipboard, and Ctrl+Enter.
5. Render messages as selectable, wrapping document content rather than two-row disabled text inputs. Size ordinary prose to its content. Use readable monospace, copy actions, and bounded expandable regions for long code/tool output. Do not add a full editor or terminal as part of this fix.
6. Group or collapse reasoning and tool details so they do not hide the final answer. Keep the active tool/approval status visible. Follow streaming output only while the user is already at the bottom; preserve their position when reading older messages.
7. Give workspace/model paths available width and a full-path tooltip or details view. Size Browse and action buttons first, then allocate remaining width to the path. Avoid a tiny path field beside unused horizontal space.
8. Make navigation/status text wrap or truncate deliberately. Use one New conversation button and one Stop action; show Stop only when applicable.
9. Put errors in a wrapping, bounded, dismissible banner with a relevant action. Do not render a short error as a fixed three-row text-edit panel.
10. Keep Activity in a bounded drawer or resizable region. Make Settings and secondary pages scroll independently, so expanded sections and save/apply controls remain reachable on small windows.
11. Set a readable default text/control scale and spacing. Respect saved zoom and test it across the existing range, including high-DPI scaling; do not force a theme change to mask layout defects.

**Acceptance:** At the attached screenshot's approximately 1285×703 size and the configured 1050×760 size, the transcript occupies the main area and the composer sits near the bottom. At 650×480, controls wrap/reflow and secondary content scrolls without hiding required actions. There is no large unused band caused by an undersized content container.

### Step 8 — Integrate recovery, cancellation, and normal interaction

1. Derive status copy from lifecycle state: selecting workspace, verifying model, loading, creating session, ready, running, awaiting approval, cancelling, or failed. Remove stale “Load a model before sending” copy after readiness.
2. Ensure Send and Ctrl+Enter follow identical guards, and repeated key presses cannot create duplicate sessions or turns. Focus the composer when the conversation becomes usable without stealing focus from an open settings control or dialog.
3. Preserve draft input when initialization or session creation fails. Browse/Retry should finish startup without requiring an application restart.
4. Keep Stop responsive during tool execution and approval waits. Retain approval identity checks so an old approval response cannot authorize a later request.
5. Apply session/turn filtering consistently to tool, approval, stats, and error events as well as text deltas. An obsolete session must not populate the current view's approval or status state.
6. Exercise close-during-load and close-during-turn behavior. Keep initialization's current noninterruptible limitation visible, and prevent shutdown/event delivery from deadlocking when the UI stops consuming events.
7. Preserve live drafts and current transcript identity across recoverable configuration failures. Do not silently restart the model or replay a user turn on error.

**Acceptance:** The full sequence launch → ready → prompt → tool approval → completion → next-turn policy change → new conversation works through the normal Agent view.

### Step 9 — Add focused verification

Add tests where they protect lifecycle and policy behavior; use layout measurements and real desktop checks for presentation. Do not require a GPU model load for every state-transition test.

| Area | Required checks |
|---|---|
| Startup resolution | Environment vs saved empty/stale values; explicit override; absent/empty environment; missing file; meaningful working directory; unavailable saved project; explicit home/root selection. Inject environment/path inputs into pure resolver tests instead of mutating global environment concurrently. |
| Lifecycle reducer | Exactly one automatic load/session; ready only after acknowledgment; model/session failure and retry; stale event ignored; navigation preserves session; readiness requires the matching workspace. |
| Configuration | Settings before loading are retained; invalid sandbox rejected; revision acknowledgment; active-turn configuration immutable; next turn receives the new settings; persisted preferences survive migration. |
| Session actions | Agent New conversation creates Agent; Chat creates Chat; view is not cleared on failed creation; workspace changes preserve the old transcript; Chat storage paths do not become Agent workspace defaults. |
| Policy and sandbox | Existing permission decisions remain intact; explicit `bwrap` failure blocks commands; Auto fallback is reported accurately; Full retains its enforcement rules; None remains selectable; command network and web settings stay independent. |
| Submission and events | Button/shortcut guards agree; duplicate send prevented; rejected submission preserves draft; stale approval/tool/status events do not affect the active session. |
| Layout | Empty, streaming, long prose, multiline code, large tool output, visible error, expanded activity, long paths, and expanded Settings at 650×480, 1050×760, and approximately 1285×703; repeat at representative zoom/DPI settings. |
| End to end | Valid `DORY_MODEL` + project → automatic model load → automatic Agent session → one prompt with a controlled read/edit/command approval path → completion → changed policy on the next turn. |

Implementation sequence for verification:

1. Add pure startup/reducer tests in `dory-gui` and controller tests in `dory-app`. Use a small injected model/runtime test boundary or scripted fixture for ordering and failure tests; do not duplicate the production controller in tests.
2. Use sandbox capability injection or existing fixtures to make available/unavailable cases deterministic. Real sandbox setup stays off the UI thread.
3. Run `cargo fmt --all --check`, focused `cargo test -p dory-app -p dory-gui`, then relevant `dory-agent` policy/command tests after shared behavior changes. Run the workspace suite before release. Supply the existing pinned native-library build environment as documented.
4. Build the GUI and inspect actual desktop geometry, keyboard input, copy/paste, multiline entry, and high-DPI behavior. Capture before/after screenshots at the reported size. Demo mode can verify presentation, but cannot prove real model loading or real sandbox enforcement.
5. Run a real-model smoke test with the pinned model. Verify startup requires no model/session clicks, and validate a harmless tool workflow in a disposable project rather than editing the user's active repository.
6. Launch the packaged binary from a project and from a desktop-style environment. Verify inherited environment/CWD behavior and the fallback workspace chooser. No live paid web-search request is necessary for this fix.

### Step 10 — Update documentation and deliver

1. Update `docs/GUI.md` with automatic startup, `DORY_MODEL` precedence, launch arguments, workspace selection, visible policy controls, pending next-turn settings, and recovery behavior.
2. Amend `docs/GUI_DECISIONS.md` to replace the prior manual workspace/session decision and record the new defaults. Preserve the historical implementation notes as historical evidence.
3. Update the README launch example to show opening directly in a project. Keep the model external to the Linux bundle.
4. Rebuild the package after runtime changes and verify its launcher preserves arguments, environment, and working directory.
5. Record passing tests, the real-model smoke result, desktop layouts checked, and any remaining platform limits. Mark this plan complete only when the acceptance criteria below have evidence.

## 4. Suggested implementation slices

Implement each slice completely enough to review and exercise before proceeding:

1. **Startup resolution and state:** launch options, path precedence, workspace default, compatible persistence, lifecycle reducer, and focused tests.
2. **Automatic initialization and sessions:** background load, acknowledged automatic Agent creation, recovery, navigation preservation, and correct New conversation behavior.
3. **Configuration and policy controls:** retained/acknowledged revisions, per-turn settings, visible selectors, effective sandbox status, and policy regression checks.
4. **Conversation layout:** full-window allocation, bottom composer, readable messages, bounded details/errors, and desktop sizing checks.
5. **Integrated acceptance and release:** real model/tool smoke test, packaged launch checks, and updated documentation.

Steps 1–2 establish the state contract used by later work. Settings reliability must precede claiming policy controls work. Complete automatic startup and configuration behavior before treating visual polish as acceptance of the fix.

## 5. Definition of done

- [ ] A valid `DORY_MODEL` loads automatically, including when older saved settings contain an empty or different model path.
- [ ] A project launch opens Agent and creates one usable session automatically; no manual Start action is required.
- [x] Missing model/workspace information has visible Browse/Retry recovery controls, and a corrected selection can continue initialization.
- [ ] The conversation uses the available window height and width; the composer remains visible at the bottom and text is readable.
- [x] Permission and sandbox selectors are available in Agent, and their effects use revision acknowledgments, persistence, and the next eligible turn.
- [x] Requested sandbox settings and actual enforcement are distinguishable before tool execution; sandbox status labels have focused tests.
- [x] Settings/navigation preserve the active session and draft, and New conversation keeps the selected mode/workspace; each mode retains its session state.
- [x] Stop, approvals, transcript persistence, history ownership, and model verification remain on the existing worker/session paths; regression suites pass.
- [ ] Focused automated checks, actual desktop checks, and a real-model Agent smoke test pass with evidence recorded.
- [x] The packaged app and documentation describe the same startup workflow; the package rebuilt and argument/version paths were checked.

## 6. Scope boundaries

Complete the launch, session, layout, and configuration fixes in the existing desktop stack. Remote providers, multiple simultaneous models/turns, a terminal emulator, an IDE editor, automatic Git operations, and unrelated agent-tool features are separate projects. Keeping these out makes the reported workflow problems independently verifiable.

## 7. Execution and verification record

- Implemented launch argument parsing, path precedence, startup Agent selection, worker-side automatic model loading, acknowledged session creation, next-turn settings revisions, sandbox status reporting, per-mode session retention, transcript-safe new conversations, composer recovery, responsive transcript layout, and documentation updates.
- Added startup-resolution, settings migration, settings validation, and sandbox-status tests. `cargo fmt --all --check`, focused `dory-app`/`dory-gui` tests, `dory-agent` tests, and the workspace suite passed with the pinned native library path configured.
- Rebuilt `dist/dory-harness-linux-x86_64.tar.gz`; the packaged version and invalid-argument paths work. The launcher forwards arguments and environment and does not change the caller's working directory.
- The packaged real-model launch reached CUDA initialization and memory-mapped the configured model, but was stopped after 4:34 without an Agent transcript. NVML reported a driver/library mismatch. Automatic session creation with the real model therefore remains unverified.
- The X11 demo window reported a 1050×760 client size, but the captured desktop image was black. Visual layout checks at 650×480, 1050×760, and approximately 1285×703, high-DPI checks, and the full approval/tool end-to-end interaction remain unverified in this environment.

The implementation work is in place, but this plan is not marked complete because its real-model and visual acceptance criteria do not yet have evidence.
