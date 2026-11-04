# Plan: a desktop GUI for dory-harness

**Written:** 2026-11-04.
**Status:** Implementation delivered; see §8 for verification evidence and remaining acceptance work.
**Repository baseline:** `9936472`. Source code is the authority where older plans describe an earlier implementation.

## 1. Goal and scope

Add a Linux desktop application, `dory-gui`, that lets a user load the pinned Dory model, chat with it, and run the existing workspace agent without using a terminal. Keep inference, parsing, permissions, tools, sandboxing, and transcript handling in the existing Rust libraries.

The first release should support this complete workflow:

1. Launch the application and select the model file.
2. See verification and loading status, then the actual loaded configuration.
3. Start a plain conversation, or select a project and start an agent session.
4. Watch the reply stream; expand reasoning, tool calls, results, and generation statistics.
5. Approve or decline a proposed edit, command, or web search in the application.
6. Stop an active turn without freezing the window.
7. Reopen a previous session and continue it without repeating unfinished tool calls.
8. Close the application with session data saved and active work shut down deliberately.

Assumptions for this plan: a local desktop application is the desired GUI; Linux is the initial supported platform; one model and one active turn are sufficient. These choices fit the existing `.so` linking, Unix process execution, and `bwrap` sandbox. Windows and macOS require separate runtime and sandbox work before they can be advertised as supported.

### Release boundaries

- **First vertical slice:** model selection, background loading, plain streaming chat, Stop, and useful errors.
- **First usable agent release:** workspace selection, native approvals, tool activity, compatible sessions, settings, and recovery.
- **Later:** benchmark views, prompt/token inspection, external-tool configuration UI, and optional server controls.

Defer remote access, browser hosting, concurrent generations, model downloads, a full code editor, terminal emulation, automatic Git commits, and automatic application updates. A GUI can be useful before it replaces every CLI diagnostic command.

## 2. What already exists and what must change

| Existing code | Reuse in the GUI | Work needed |
|---|---|---|
| `crates/dory-engine/src/engine.rs` | `Engine`, `EngineOptions`, streamed generation, `GenStats`, cancellation | Add an application controller around the blocking APIs; retain one engine owner |
| `crates/dory-engine/src/verify.rs` | Pinned model checksum and linked-fork checks | Run off the UI thread; present stages and actionable errors |
| `crates/dory-engine/src/chat.rs` and `parse.rs` | Existing template renderer and parsed output | Preserve the exact chat path and model markup handling |
| `crates/dory-agent/src/agent.rs` | `Agent::run_turn`, `AgentEvent`, compaction, stop reasons | Translate events into stable application state; add only the metadata the GUI needs |
| `crates/dory-agent/src/policy.rs` | Permission decisions and the `Approver` trait | Implement a channel-based GUI approver; terminal approval cannot serve this frontend |
| `crates/dory-agent/src/tool.rs`, `builtin/`, `workspace.rs`, `sandbox.rs` | Registry, file tools, command execution, path checks, sandbox | Assemble these through shared setup instead of duplicating CLI behavior |
| `crates/dory-agent/src/session.rs` | JSONL recording, resume, incomplete-call repair | Add safe listing/read-only inspection, single-writer ownership, and desktop lifecycle handling |
| `crates/dory-cli/src/agent_cmd.rs` | Working setup and event-persistence flow | Extract frontend-independent setup and configuration; leave terminal I/O in the CLI |
| `crates/dory-agent/src/scripted.rs` | `ScriptedBackend`, `ScriptedApprover` | Use for deterministic application tests and a clearly labeled demo mode |
| `crates/dory-agent/src/web/` and `builtin/web.rs` | Tavily/Brave providers, cancellation, quota state, secret filtering | Expose existing configuration and status without storing keys in ordinary settings |
| `crates/dory-serve/src/main.rs` | Existing OpenAI-style server | Keep separate for the first release; it returns tool calls and does not run the workspace agent |
| `build_version.rs`, crate build scripts | Version stamping and native-library linkage | Apply the same rules to the GUI binary and its packaging |

Important implementation constraints found in the source:

- `Engine::start` creates an internal worker but waits for readiness before returning. Calling it from a window callback still blocks the window.
- `Engine::drop` joins its worker. Model unload and shutdown also belong off the UI thread.
- `Agent::run_turn` and `Approver::ask` are synchronous. Put the agent on an application worker and let approvals block that worker while the window remains responsive.
- `Policy`, `Registry`, and `ToolCtx` contain non-thread-safe state or trait objects without `Send` bounds. Construct and use them inside their owning worker; do not try to share the entire agent with the UI through a mutex.
- `AgentEvent::MessageAdded` carries authoritative messages, while `Content` and `Reasoning` carry streaming deltas. Appending both would duplicate replies.
- `AgentEvent::ToolResult` currently contains a name and text, not structured exit/truncation data. The UI must not invent status by interpreting arbitrary result text.
- Approval requests currently carry only `tool` and `summary`. `edit_file` summaries include a diff; `write_file` summaries contain counts rather than a full preview.
- Session IDs currently use seconds plus process ID. A long-lived GUI can create multiple sessions in one second, so unique-ID allocation needs strengthening.
- The current session reader skips malformed JSON lines, then reopens the file for append. A torn final line without a newline needs explicit recovery before appending, or the next record can become part of the damaged line.
- A scripted backend avoids model loading and GPU use, but crates depending on `dory-engine` still build/link `dory-sys`. Demo mode alone does not remove the fork, compiler, or native-library build prerequisites.

## 3. Architecture decision

Use **Rust with `egui` and `eframe`** for the initial desktop frontend. This is a proposed project choice: it fits the existing Rust workspace and permits direct library integration without adding an HTTP service between the UI and local tools. `eframe` supports native applications, and its UI can be awakened from another thread through `egui::Context::request_repaint`. See the [official eframe application API](https://docs.rs/eframe/latest/eframe/trait.App.html) and [egui repaint API](https://docs.rs/egui/latest/egui/struct.Context.html#method.request_repaint).

Pin compatible versions during the skeleton step and commit `Cargo.lock`; use the API of the pinned versions. Do an early Linux check for keyboard navigation, selection, clipboard, IME input, screen-reader behavior, renderer compatibility, and display scaling. AccessKit integration is documented, but support on the actual target environment must be tested rather than assumed. See [egui's accessibility guidance](https://github.com/emilk/egui/blob/main/docs/accessibility.md). If an essential input or accessibility requirement fails, resolve the toolkit choice before building the feature screens.

Do not make the existing CLI's JSON output the production transport. It lacks an approval-response channel, and spawning it per turn would repeatedly load the model. Do not use `/v1/chat/completions` as a substitute for the local agent lifecycle; `dory-serve` deliberately does not execute tools.

### Proposed crates and ownership

```text
Window / UI thread: dory-gui
  AppState + view models + keyboard input + bounded event draining
       |
       | owned commands and events, tagged with session/turn/request IDs
       v
Application worker: dory-app
  model lifecycle + conversations + Session writer + Registry + Policy
       |                                    |
       | EngineBackend / Agent              | GuiApprover waits for a response
       v                                    v
Existing dory-engine worker          UI approval panel
  llama context + generation
```

The cancellation flag and approval-response channel bypass the normal command queue. A worker blocked in `run_turn` or `ask` cannot process a queued Stop command.

```text
crates/dory-app/                       # new, no GUI toolkit dependency
  Cargo.toml
  src/lib.rs
  src/config.rs                       # typed settings, validation, defaults
  src/setup.rs                        # shared engine/agent assembly
  src/protocol.rs                     # commands, events, IDs, state enums
  src/controller.rs                   # worker lifecycle and dispatch
  src/approval.rs                     # channel-based Approver
  src/sessions.rs                     # listing, metadata, ownership coordination
  tests/                              # scripted controller tests

crates/dory-gui/                       # new native executable
  Cargo.toml
  build.rs
  src/main.rs
  src/app.rs                          # state reducer and event pump
  src/settings.rs                     # user configuration and UI preferences
  src/views/{setup,chat,activity,approval,history,settings,diagnostics}.rs
  assets/                             # icon and any licensed bundled fonts

docs/GUI_PLAN.md
docs/GUI.md                           # eventual user/build guide
```

Keep transcript format and core session recovery in `dory-agent::session`; the application layer coordinates them. `dory-app` depends on the existing agent and engine; `dory-gui` depends on `dory-app`; the CLI may reuse configuration/setup without adopting desktop-specific behavior. The server can stay as it is.

## 4. Step-by-step implementation

Each step should leave a runnable or testable result. The file names and protocol variants below are proposed interfaces, not claims that those APIs already exist.

### Step 1 — Record the baseline and agree on the first-release behavior

1. Read the README, current source, and relevant acceptance scripts. Record the actual defaults rather than deriving them from an older plan.
2. Record the working build prerequisites: Rust toolchain, `DORY_LLAMA_DIR`, built fork libraries, libclang, model path, and available GPU memory.
3. Run the existing non-GPU tests in a configured build environment. Run the CLI/chat/agent acceptance suites once with the real model if available; record skipped prerequisites explicitly.
4. Write a short design decision beside this plan: Linux desktop, `egui`/`eframe`, one loaded model, one active turn, and the first-release workflows above.
5. Define plain Chat as having no workspace tools. Define Agent as using an explicitly selected workspace, existing built-ins, `ask` permissions by default, and the real sandbox status.

**Done when:** the baseline is recorded and a developer can distinguish existing behavior from proposed GUI behavior.

### Step 2 — Add a runnable desktop skeleton and demo mode

1. Add `dory-gui` to the workspace, pin the chosen GUI dependencies, and select the renderer/features explicitly after testing the target Linux driver.
2. Add `build.rs` consistent with the existing version/rpath approach. If reading `DEP_LLAMA_LIB_DIR`, declare the direct `dory-sys` dependency required for that build metadata, as the current binaries do.
3. Open a window with an empty-state setup view, navigation, a multiline composer, and a status area.
4. Add a clearly labeled `--demo` mode with scripted streaming, a tool result, an approval, and a simulated failure. Keep fake data outside production setup.
5. Exercise resizing, keyboard focus, text selection, clipboard, IME, dark/light themes, and high-DPI behavior before committing to custom widgets.
6. Check Linux accessibility against the requirements established in Step 1. Document any gap and resolve blockers before continuing.

**Done when:** the GUI opens without loading a model, the demo is visibly labeled, and basic desktop input works. Building the linked application still requires the current native dependencies.

### Step 3 — Extract shared configuration and setup

1. Introduce `dory-app` with typed load, generation, agent, workspace, sandbox, and web configuration. Keep CLI parsing types out of this crate.
2. Extract reusable setup from `main.rs::engine_options`, `gen_params`, and `agent_cmd.rs::run`: model verification, template checking, workspace validation, tool registration, policy construction, session setup, and web-provider configuration.
3. Keep terminal printing, Ctrl-C installation, `/dev/tty` approval, exit codes, and REPL commands in `dory-cli`.
4. Pass the approver, cancellation handle, and event sink into shared setup. The CLI continues to supply `TerminalApprover`; the desktop supplies `GuiApprover`.
5. Preserve current defaults: 2 loops, `last_loop`, 32,768 context, 2,048 batch, 512 ubatch, GPU layers `-1`, flash attention auto, prefix reuse off, server prefill, seed 42, temperature 1.0, top-p 0.95, and 4,096 maximum output tokens. The agent CLI uses 30 rounds, 1,800 seconds, and a 120-second default command timeout; these differ from the lower-level `AgentConfig` round default.
6. Validate values and paths centrally. Check finite numeric values, supported enum values, sensible batch/context relationships, and nonzero limits; retain engine validation as the final authority.
7. Add focused tests for configuration conversion and the extracted setup, then rerun the existing CLI acceptance paths affected by the refactor.

**Done when:** CLI behavior and JSON schema v1 remain compatible, and the desktop can assemble the same engine/agent without copying the CLI implementation.

### Step 4 — Define the application protocol and state machine

1. Define owned command values such as `LoadModel`, `UnloadModel`, `NewSession`, `OpenSession`, `SubmitTurn`, `UpdateIdleSettings`, and `Shutdown`.
2. Define application events for load stages, readiness, session snapshots, streamed deltas, tool lifecycle, approval requests, round statistics, turn completion, and operational errors.
3. Give events a model-generation ID and, where applicable, session ID, turn ID, sequence number, tool-call ID, and approval-request ID. Reject late events and stale answers from earlier work.
4. Model runtime states explicitly: `Unloaded`, `Verifying`, `Loading`, `Ready`, `Running`, `AwaitingApproval`, `Cancelling`, `Unloading`, `Failed`, and `ShuttingDown`. Model conversation state separately from model state.
5. Define allowed transitions and command rejection behavior. Disable Send outside Ready; reject duplicate submissions in the controller even if the UI sends them.
6. Distinguish a normal `AgentEvent::Finished(Reason)` from an error returned by `run_turn`. An error path may not emit `Finished`; the application must emit exactly one terminal outcome for every submitted turn.
7. Keep the desktop protocol independent of CLI JSON and transcript versions. Add serialization only where persistence or a future transport actually requires it.

**Done when:** reducer/controller tests cover valid transitions, invalid commands, stale events, and error termination without rendering a window.

### Step 5 — Implement the worker and responsive event delivery

1. Spawn one application worker that owns the engine handle, registry, policy, active messages, and transcript writer. Construct non-`Send` agent objects inside that worker.
2. Keep hashing, model startup, tokenization, session I/O, sandbox probing, agent turns, tool execution, and engine destruction off UI callbacks.
3. Use bounded event delivery and coalesce adjacent text deltas with matching IDs. Never drop authoritative messages, approvals, errors, or terminal events.
4. Define backpressure explicitly: bounded text chunks, cancellation-aware send retries, and immediate exit when the UI receiver disconnects. Avoid blocking forever if the window is minimized or closing.
5. Drain events with a per-frame time/size budget and request another repaint while work remains. Wake the UI from the worker using `request_repaint`; avoid a permanent busy repaint loop.
6. Rate-limit statistics and diagnostic updates. Preserve ordering when flushing coalesced text before a tool or terminal event.
7. Add a scripted stress case with many small Unicode deltas, delayed UI consumption, and cancellation while the queue is full.

**Done when:** long streams do not create an unbounded event queue, user input remains responsive, and slow consumption cannot deadlock Stop or shutdown.

### Step 6 — Add model selection, verification, and lifecycle controls

1. Resolve the model path from an explicit launch override, a saved selection, or `DORY_MODEL`, in a documented order. Provide Browse and a typed path field.
2. Show staged status: checking file, verifying fork, hashing model, loading weights/context, checking template, Ready. Use an indeterminate indicator where progress is unavailable.
3. Reuse `verify_fork`, `verify_model`, and the template check. Do not silently accept an arbitrary GGUF or bypass the pinned checksum.
4. Show actionable failures for a missing model, checksum mismatch, fork mismatch, incompatible template, allocation failure, and unavailable workspace. Keep a copyable technical detail panel.
5. Distinguish requested settings from the actual `ModelInfo`. Show context, loops, cache mode, and effective loading settings.
6. Mark settings that require reload. Prevent overlapping loads; unload the old engine on the worker before creating another GPU allocation.
7. Define honest cancellation: hashing can become cancellable through a small progress/cancellation extension; native model initialization currently has no exposed cancellation path. During that stage, show a pending cancellation/close request and wait for the call to return. Do not promise immediate interruption or kill a Rust thread.
8. Verify that retrying after a failed load leaves the controller usable and does not retain an old model accidentally.

**Done when:** a correct model reaches Ready, a bad model produces a recoverable error, and loading/reloading leaves the window responsive.

### Step 7 — Deliver plain streaming chat

1. Run the existing agent loop with an empty tool registry for Chat, following the existing CLI chat pattern. Preserve `generate_chat`, the renderer, and the streaming parser.
2. Implement multiline input, Send, Stop, copy message, and a New Conversation action. Use Ctrl+Enter to send and Escape to stop active work; avoid submitting while an IME composition is active.
3. Display user and assistant messages, code blocks, optional reasoning, and distinct running/completed/cancelled/error states. Start with correct selectable text; add richer Markdown rendering incrementally.
4. Treat deltas as a provisional view. Reconcile with `MessageAdded` exactly once; clear or mark provisional rounds that the agent discarded during length retries. Add a round identifier/discard event if needed rather than concatenating abandoned output into the final answer.
5. Keep reasoning visibility separate from model thinking: hiding a panel does not turn thinking off.
6. Auto-scroll only while the user is already near the bottom. Provide a jump-to-latest action after manual scrolling.
7. Show round token counts and timing from `GenStats`; calculate tokens/second only when time is nonzero. Label aggregate prompt tokens as summed per-round usage, not current context occupancy.
8. Persist Chat using the same transcript machinery under the application's user-data directory, with an explicit chat-kind header and no implicit workspace tools. Agent sessions remain in their workspace.

**Done when:** multi-turn chat streams correctly, including Unicode/code, with no duplicated messages or discarded-round text presented as a final answer.

### Step 8 — Add workspace agent sessions and tool activity

1. Add an explicit Chat/Agent mode selection. Opening Agent requires a valid canonical workspace path and shows that path prominently.
2. Assemble the existing system prompt, root `AGENTS.md`, built-ins, command tool, optional web tool, scratch directory, and policy through shared setup.
3. Display the effective permission mode, sandbox availability, command-network setting, and web-search policy before the first turn. Keep them visible during a run.
4. Freeze workspace identity and permissions for a submitted turn. Changing workspace creates or opens a different session; it must not redirect an active tool call.
5. Display tool cards in call order with arguments, waiting/running/completed status, and expandable output. Store stable call IDs so repeated tool names cannot be confused.
6. Extend the internal event metadata narrowly if needed to expose `ToolOutcome.is_error`, truncation, and structured completion status. Preserve the existing sanitized result text sent to the model and keep CLI JSON v1 stable unless a deliberate schema change is made.
7. Do not claim live command stdout: current command output is returned after completion. Show Running until the result arrives; add streaming process output only as a separate feature.
8. Show compaction stages and every existing stop reason, including round/time/context limits, malformed calls, repeated calls, length cuts, and cancellation.
9. Explain actual limits accurately: the turn time budget is checked between rounds, individual commands have their own timeout, and compaction counts/tokens are not a continuously measured context gauge.

**Done when:** a fixture task can read a project, request an edit, run an approved test, and answer, with each action attributable to the correct session and call.

### Step 9 — Implement native approvals without changing policy meaning

1. Implement `GuiApprover: Approver`. `ask` emits an approval request and waits on a dedicated response receiver with short cancellation/disconnection checks. It never opens `/dev/tty`.
2. Offer Allow Once, Allow This Exact Call for This Session, Decline, and Decline with Reason. Map them to `Answer::Yes`, `Always`, `No`, and `NoWith`. Preserve the existing exact tool-and-arguments scope of `Always`.
3. Tag each request and response; reject responses after cancellation or a session/turn change. Closing the application or losing the response channel must never imply approval.
4. Extend the approval payload with the minimum review data: risk, normalized workspace, arguments, working directory, command, effective sandbox/network state, and file preview where applicable. Retain existing summaries for CLI compatibility and update scripted approvers/tests if `Request` changes.
5. Reuse edit planning and stale-file checks. For `write_file`, add a bounded create/overwrite preview because the existing summary only reports counts. Show when a preview is truncated and allow inspection of the proposed content.
6. Revalidate files before execution and refuse/re-request approval if a displayed plan changes. A diff preview must describe what will actually run, not an earlier version of the file.
7. Render model/tool text as selectable inert text. Display content cannot activate controls, execute shell text, or approve itself; do not use raw HTML rendering.
8. Test approval of one call, remembered approval of the exact same call, a different argument requiring a new request, decline with reason, pending-request cancellation, and late/double responses.

**Done when:** the default `ask` workflow works without a terminal, with the same permission semantics and no stale approval capable of authorizing another call.

### Step 10 — Finish cancellation and orderly shutdown

1. Create a fresh cancellation handle per submitted turn, shared by generation, tool execution, web requests, and the GUI approver. Never reset a flag while the previous turn is still unwinding.
2. Make Stop immediately set that handle, change the UI to Cancelling, and wait for a terminal outcome. Do not mark the turn stopped merely because the button was clicked.
3. Check cancellation before starting a new round and before every tool execution, including after an approval returns. Audit multi-call replies so later writes cannot run after Stop.
4. Reuse the command runner's process-group cancellation and web layer's cancellation behavior. Preserve partial messages and incomplete-call recovery rules.
5. Keep late output attached to its original turn; reject a new turn until the old one is finished.
6. On close during active work, offer Keep Running or Stop and Close. Resolve pending approval as declined/cancelled, flush the transcript, cancel active work, and destroy the engine on its worker.
7. Use a shutdown handshake. Poll completion through the UI event loop; do not block the window by joining workers inside a close callback. Explain native-load delays using Step 6's behavior.

**Done when:** stopping generation, approval, command execution, or web search returns to a usable state, and normal close leaves no active command process group behind.

### Step 11 — Add session history, resume, and crash recovery

1. Add a read-only session reader/listing API; history browsing must not open an append writer. Read large transcripts outside the UI thread and load older messages incrementally.
2. Preserve workspace agent transcripts at `<workspace>/.dory/sessions/<id>/transcript.jsonl`. Use separate lightweight metadata for title, last activity, and UI preferences; transcripts remain authoritative.
3. Replace seconds-plus-PID allocation with collision-resistant creation and retry on `AlreadyExists`. Continue accepting old session IDs. Validate IDs as single path components and guard against traversal/symlink escapes.
4. Enforce one writer per transcript, including GUI/CLI overlap, through a process-safe lock. A locked session may be viewed read-only; it must not be resumed concurrently.
5. Recover a torn final line before append using the byte offset of the last valid record; preserve the damaged tail separately. Distinguish recoverable tail damage from corruption in the middle and unsupported versions.
6. Resume through the existing message repair logic. Display unknown tool outcomes clearly; do not replay those calls. Ensure repaired state is persisted consistently when continuation begins.
7. Compare recorded model/settings/workspace against the requested continuation. Show differences and require an explicit continuation choice for consequential mismatches; never restore more permissive policy from old metadata automatically.
8. Handle missing workspaces, missing output files, read-only disks, malformed headers, and write failures. If persistence fails during an active task, stop before further side effects and preserve/export the in-memory conversation where possible.
9. Provide user-initiated Markdown/JSON export with an explicit destination. Defer destructive history deletion from the first release.

**Done when:** GUI-created and CLI-created agent sessions can be reopened by either frontend, rapid New Session actions do not collide, and interrupted calls remain unknown instead of running twice.

### Step 12 — Add settings and web-search configuration

1. Separate load settings, per-turn generation settings, workspace policy, and presentation preferences. Mark Apply Next Turn versus Requires Model Reload clearly.
2. Save versioned settings atomically in the Linux user configuration directory and Chat data in the user data directory. Do not write machine preferences into the repository.
3. Expose the existing load and generation options, thinking, agent limits, command timeout, permission mode, allow-command rules, sandbox mode, network, writable directories, and environment-variable names in an Advanced section.
4. Preserve the current `ask`/sandbox-auto behavior. Show the effective sandbox result; an unavailable sandbox must remain visible, and `full` mode must retain its existing refusal behavior unless sandbox-none was explicitly selected.
5. Expose Tavily/Brave selection, enabled/disabled status, search budget, and search approval policy. Distinguish web-tool network requests from networking inside sandboxed commands.
6. For the first release, read keys from the existing provider environment variables and show configured/missing status only. Desktop launchers may not inherit shell variables, so document the launch environment. Add in-app key entry only with an explicit memory-only or OS-keyring design.
7. Never place provider keys, inherited environment values, or collected secret values into ordinary settings, event debug dumps, transcripts, or diagnostics. Reuse query filtering and output sanitization.
8. Display persisted workspace quota status using the existing web state file. Use the fake provider fixtures to exercise missing keys, denied searches, timeouts, quota exhaustion, and cancellation without paid live calls.

**Done when:** settings round-trip across restarts, invalid settings cannot start work, and web configuration follows the existing provider and permission behavior.

### Step 13 — Add diagnostics and desktop polish

1. Add a diagnostics panel with application version, pinned/built fork identifiers, selected model, actual context configuration, sandbox status, and the last operational error.
2. Show stats already available from `GenStats`: prompt/generated/reused tokens, prompt/generation time, prefill chunk sizes, and finish reason. Mark unavailable values rather than estimating GPU memory or live context usage from unrelated counters.
3. Route application logs to a bounded local log with redaction. Treat native-library stderr capture as separate work; do not redirect global process output casually.
4. Make diagnostic export user-initiated, show what it contains, and omit keys and transcript contents by default.
5. Add clear empty/error states, font scaling, accessible labels, visible focus, useful shortcuts, copy controls, and contrast in both themes. Test long paths, large code blocks, and wrapped output.
6. Cache rendered completed messages and virtualize/collapse long histories and outputs so every frame does not reprocess the entire conversation.

**Done when:** common failures can be diagnosed from the application and a long session remains usable at normal and high display scaling.

### Step 14 — Verify behavior with focused tests and real-model acceptance

1. Test controller/reducer behavior with `ScriptedBackend` and temporary workspaces: event ordering, round retries, duplicate prevention, stale events, limits, and terminal errors.
2. Test the approval bridge and policy integration with real response channels rather than only the existing canned approver.
3. Test cancellation while the event buffer is full, during approval, between two tool calls, during a command, and during fake web search.
4. Test transcript locking, rapid session creation, torn-tail recovery followed by append/reopen, unsupported versions, disk-write failure, and unknown outcomes.
5. Test UI interactions through the pinned toolkit's suitable test facilities or a small manual checklist: keyboard send/stop, disabled controls, scrolling, long output, resizing, focus, IME, and high DPI.
6. Run existing CLI/chat/agent acceptance after shared-code changes. Compare a deterministic plain chat and fixture-agent turn between frontends using identical settings, prompt/system text, and workspace fixtures.
7. Run real-GPU checks serially. Confirm loading, inference, reload/unload, memory release, cancellation, and a complete edit/test/answer task. Record the hardware, settings, and observed timing rather than universal performance claims.

Suggested commands after the new crates and dependencies exist:

```bash
cargo fmt --all --check
cargo check --workspace --all-targets
cargo test -p dory-engine -p dory-agent -p dory-app
cargo test -p dory-gui
cargo build --release -p dory-cli -p dory-gui -p dory-serve
target/release/dory-gui --demo
```

The checks above still require the native fork build. Model-dependent vocabulary tests need the model even though they do not use the GPU; distinguish those from fully scripted tests. Run the existing acceptance scripts with their documented inputs, and run ignored GPU tests separately, one GPU user at a time:

```bash
cargo test -p dory-engine --test engine_gpu -- --ignored --test-threads=1
```

**Done when:** focused automated checks pass, existing behavior remains compatible, and the real-model desktop workflow has recorded acceptance results. Demo success alone is insufficient.

### Step 15 — Package and document the Linux release

1. Start with a developer build and launcher; then create one documented Linux release artifact appropriate to the tested target environment.
2. Audit all runtime shared libraries, including `libllama` and transitive ggml/CUDA libraries. The current build embeds a developer library path; a relocatable package needs a tested relative runtime search path or launcher configuration.
3. Keep the model external for the first release. Document the required pinned file and hardware; do not bundle or download it implicitly.
4. Handle native dynamic-loader failures through installation checks/documentation: missing libraries may prevent the process from reaching any GUI error screen.
5. Test the release binary from outside the source checkout and without the developer's `LD_LIBRARY_PATH`. Verify desktop-launch environment, file pickers, display server, GPU access, and `bwrap` behavior in the chosen package format.
6. Add `docs/GUI.md` with build/run commands, first launch, permission modes, Stop/close behavior, session locations, web environment setup, troubleshooting, and known platform limits.
7. Update README crate/build sections and link the GUI guide. Report which platform/hardware was actually tested.

**Done when:** a user can launch the packaged application, select the external model, complete an agent task, and resume it after restart without the development checkout.

### Step 16 — Add optional harness views after the agent release

1. Add read-only prompt/template/token inspection using existing renderer/tokenization APIs; show prefill configuration alongside output.
2. Add a benchmark view only if needed, reusing engine measurements and exporting machine-readable results. Reserve the single engine so benchmarking cannot overlap an agent turn.
3. Add external-tool configuration only after it can display executable paths, arguments, timeouts, and policy clearly before enabling a tool set.
4. If requested, add server controls as a separate opt-in feature. Keep loopback defaults, port conflict handling, explicit process ownership, and shutdown behavior. Avoid loading another model alongside the GUI on a memory-constrained GPU; decide explicitly whether server mode replaces local mode or shares a deliberately designed runtime.

**Done when:** optional tools reuse the same configuration and measurements without expanding the first-release critical path.

## 5. Delivery order and dependencies

- **Milestone A — Runnable foundation:** Steps 1–5. Desktop skeleton, demo, shared setup, tested controller.
- **Milestone B — Chat vertical slice:** Steps 6–7 and the cancellation/shutdown work from Step 10. Real model, streaming conversation, Stop, recoverable errors.
- **Milestone C — Usable workspace agent:** Steps 8–12. Tool timeline, native approvals, reliable cancellation, compatible history/resume, settings, and optional web search.
- **Milestone D — Shippable Linux application:** Steps 13–15. Usability, diagnostics, focused acceptance, packaging, and documentation.
- **Milestone E — Optional harness features:** Step 16.

Implement cancellation support with the first controller, then finish its approval/tool cases as those features arrive. Build approval and recovery regression tests alongside their steps; Step 14 is the combined acceptance gate, not the first time tests are written. Keep shared-setup extraction in small changes so CLI regressions can be isolated.

## 6. First-release acceptance checklist

- [ ] The window opens before model verification/loading and remains responsive throughout.
- [ ] Missing dependencies/model paths and pin failures have actionable handling.
- [ ] One loaded engine serves multiple turns; overlapping generations are rejected.
- [ ] Plain chat uses no workspace tools and preserves the existing renderer/prefill behavior.
- [ ] Streaming messages, discarded retries, reasoning, and final messages reconcile correctly.
- [ ] Agent tasks run in the displayed workspace with the displayed policy and sandbox state.
- [ ] File changes, commands, and searches use the existing approval rules without a terminal.
- [ ] Approval previews match the execution plan; stale or disconnected responses cannot allow work.
- [ ] Stop works during inference, approval, commands, and web search; later tool calls do not run.
- [ ] Native loading delays and turn/command time limits are represented accurately.
- [ ] Sessions survive restart, support CLI interoperability, and never replay unknown tool outcomes.
- [ ] Rapid session creation, damaged tails, writer contention, and disk failures are handled.
- [ ] Keys are absent from ordinary settings, transcripts, and diagnostic exports.
- [ ] Large streams/history do not create unbounded queues or freeze the window.
- [ ] Keyboard navigation, selection, IME, scaling, and target-platform accessibility are checked.
- [ ] Existing automated/acceptance checks and a real-model GUI task pass with recorded results.
- [ ] The release runs outside the checkout with its native libraries resolved and external model selected.

## 7. References

- [Project README](../README.md): current build, CLI, agent, session, and sandbox behavior.
- [Agent plan](AGENT_TOOLS_PLAN.md): earlier design and evaluation background; check current source for API details.
- [Web-search plan](WEB_SEARCH_PLAN.md): provider, quota, filtering, and fixture-test background.
- [Official egui repository](https://github.com/emilk/egui): toolkit capabilities and integration guidance.
- [Official eframe App API](https://docs.rs/eframe/latest/eframe/trait.App.html): native application integration.
- [Official egui Context API](https://docs.rs/egui/latest/egui/struct.Context.html#method.request_repaint): waking the UI from background work.
- [Official egui accessibility guidance](https://github.com/emilk/egui/blob/main/docs/accessibility.md): platform-specific accessibility checks.

External documentation was consulted on 2026-11-04. Select and pin dependency versions during implementation; rolling documentation is not a substitute for the locked version's API.

## 8. Execution and verification record

Implementation added `dory-app` and `dory-gui`, shared typed model-load configuration with the CLI, a bounded worker/event controller, native approval and file previews, chat/agent/history/settings/diagnostics views, a demo flow, read-only viewing of locked sessions, a prompt/token inspector, a launcher, and a Linux archive. Session recovery, writer locking, safe IDs, approval metadata, tool error/truncation metadata, and cancellation between tool calls were added in the existing libraries.

Verified on 2026-11-04 with Rust 1.95.0, the pinned fork `1c3cdd23787750814aa21a070dc4d6dc451af871`, and the 5,806 MiB RTX 3060 Laptop GPU:

- `cargo fmt --all --check`, `cargo check --workspace --all-targets`, and `cargo test --workspace -- --test-threads=1` passed. The focused post-integration `dory-agent`, `dory-app`, and `dory-gui` tests also passed.
- All 9 ignored `dory-engine` GPU tests passed serially with the pinned model.
- CLI acceptance passed. Chat acceptance passed deterministic output, tool use, prefix reuse, and session save/restore.
- The agent fixture suite passed all cases across the full run and targeted rerun: answer from file, find symbol, create/edit file, run and report, fix failing test, read-only refusal, outside-workspace refusal, and fake-provider web search. The read-only checker was broadened to accept “wasn’t able” wording after the tool refusal and unchanged workspace were already verified.
- `cargo build --release -p dory-cli -p dory-gui -p dory-serve` passed. The `--demo` process started. The 43 MiB Linux archive was extracted outside the checkout; its launcher printed the GUI version and started demo mode with `LD_LIBRARY_PATH` and `DORY_LLAMA_DIR` unset. `ldd` with the bundle library directory resolved `libllama` and all ggml libraries from the extracted archive.

The following acceptance still needs hands-on desktop coverage: keyboard/IME/clipboard interaction, screen-reader/accessibility behavior, scaling and resizing, model loading and a complete agent task through the GUI, and window-close behavior during real inference. The application controller does not yet have the scripted stress suite described in Step 14; GUI history rendering is not virtualized, and bounded local redacted log capture is not implemented. These are open acceptance items, not claims of tested behavior. Paid live web-search tests were not run; the fake provider fixture passed.
