# Plan: visible loop selection and reliable return to Agent

## Goal and scope

Make the number of model recurrence loops easy to configure in the desktop app, and let users return from Settings to their existing Agent conversation while work is in progress. This document is an implementation plan; it does not change application behavior.

“Loops” means the Dory model's recurrence count, supported from **1 through 4**, with the current default of **2**. It is separate from the agent's maximum tool/generation rounds (`max_rounds`). Keep that distinction explicit in the interface.

The work primarily affects `dory-gui`, with small `dory-app` changes if needed to report the loaded loop count reliably. Reuse the existing engine support, settings format, and model-loading worker. CLI flags, server behavior, and the engine's supported range are outside this change.

## Findings in the current working tree

| Area | Existing behavior | Implication |
| --- | --- | --- |
| Loop control | `DoryApp::settings_view` already renders a `DragValue` for `self.settings.loops`, restricted to `1..=4`, inside the collapsed “Model loading · requires reload” section. | Improve the existing option's visibility and application flow rather than introducing a second setting. |
| Persistence | `UserSettings` already serializes `loops`, defaults it to 2, and copies it into `LoadConfig`. | Preserve the existing JSON key and default; no schema migration is needed for this field. |
| Engine support | `LoadConfig::engine_options_with_vocab_only` and `dory_engine::model::Model::load` validate the range. The engine applies `dory.num_loops` when loading. | A changed value requires a model reload, not merely an update to next-turn agent settings. |
| Settings actions | “Save settings” writes preferences; “Apply next turn settings” sends generation and agent settings only. | Neither action currently reloads the model with a new loop count. Explain those boundaries and provide an explicit reload action. |
| Navigation | The sidebar disables every destination during `session_pending`; it also disables Agent and Chat during `running` or `pending_settings_revision`. `switch_mode` repeats those busy guards. | A user can open Settings during a turn, but cannot immediately return to the active Agent view. |
| Session preservation | `current_session_kind`, `mode_sessions`, and `switch_mode` already preserve existing conversations. The same-kind early return happens after the busy guards. | Treat returning to the current mode as a view change that does not switch or create a session. |
| Load acknowledgment | `AppEvent::ModelReady` carries a generation identifier and human-readable details. The worker already has `next.info.loops`. | Report the effective value as structured data rather than assuming the current settings value is loaded or parsing a status string. |

The navigation conditions explain a concrete route to the reported greyed-out Agent button. Confirm the exact reproduction during implementation; an idle Agent → Settings → Agent round trip should already work unless a pending flag remains set. Investigate stale flags if the issue also occurs when no work is pending.

Relevant files:

- [GUI state, navigation, settings, and event handling](../crates/dory-gui/src/app.rs)
- [Saved settings and configuration conversion](../crates/dory-gui/src/settings.rs)
- [Controller and model-loading worker](../crates/dory-app/src/controller.rs)
- [Load configuration validation](../crates/dory-app/src/config.rs)
- [Application event types](../crates/dory-app/src/protocol.rs)
- [Existing model-free loop validation tests](../crates/dory-engine/tests/loops.rs)
- [Desktop user guide](GUI.md)

## Intended user experience

1. Open Settings and immediately see a clearly labeled **Model loops** selector with choices **1, 2, 3, 4**. Keep other advanced loading options collapsible.
2. Show concise help: “Number of model recurrence loops. Default: 2. Changes require a model reload.” Do not promise a particular speed or quality improvement.
3. Show the effective loaded count separately from the selected value. For example, after selecting 3 while a two-loop model is loaded, show “Loaded: 2 · Selected: 3 · Reload required.” Before loading, show “No model loaded.”
4. Provide **Reload model with selected settings** in Settings. Disable it during a running turn, pending settings acknowledgment, session creation, or another model load, and explain the blocking operation. If no model is selected, direct the user to the existing model chooser.
5. Saving preferences stores the selected count for future loads. Applying next-turn settings updates generation and agent options; it does not clear the loop reload notice.
6. Opening Settings and returning to Agent preserves the session ID, transcript, workspace, draft, turn ID, approval request, and ongoing work. Return is available even while that same conversation is busy.
7. A real change from Agent to Chat or Chat to Agent keeps the existing restrictions while busy. Navigation to the active conversation and operations that replace a conversation use different eligibility rules.

## Step-by-step implementation

### Step 1 — Reproduce the navigation failure and record its state

1. Launch the current GUI against a disposable workspace. Start with a normal idle Agent session and test Agent → Settings → Agent.
2. Repeat while a response is streaming, while an approval is pending, and immediately after applying next-turn settings. Also inspect behavior during session initialization.
3. Observe `selected`, `current_session_kind`, `session_kind_pending`, `running`, `session_pending`, and `pending_settings_revision` at the disabled button. Use a debugger or temporary diagnostic logging; remove temporary logging before finishing.
4. Verify that streaming and worker acknowledgments continue to be processed while Settings is visible through `drain_events`.
5. Record which busy condition causes the failure. If a flag remains set after a matching completion or error, fix that transition alongside the navigation policy; do not simply remove submission or session-switching safeguards.

**Completion condition:** There is a reproducible scenario and an identified navigation condition or stale lifecycle flag behind the disabled Agent button.

### Step 2 — Separate returning to a conversation from switching modes

1. Extract a small, testable navigation decision that distinguishes an ordinary page, the current conversation kind, and another conversation kind. Use an enum/helper local to the GUI rather than a broad application rewrite.
2. Allow showing the current conversation regardless of `running`, `session_pending`, or `pending_settings_revision`. If initialization has no acknowledged current kind yet, allow showing its pending conversation kind as a loading view without creating a second session.
3. Keep actual mode changes blocked during those busy states. Keep existing model/readiness checks on operations that create conversations.
4. In `switch_mode`, handle the current-kind view-only return before the busy guards. Returning to a pending initialization view must also stop before `new_conversation` or `maybe_create_agent_session` can enqueue another request.
5. Use the same decision for sidebar enablement and the click handler. Avoid assigning `self.selected = item` before an actual mode switch has been accepted; the existing handler currently does that unconditionally.
6. Allow ordinary pages such as Settings and Diagnostics to remain accessible during session creation. Their individual mutation controls should retain their own eligibility checks.
7. Preserve session IDs, messages, composer text, counters, cached sessions, workspace, and pending approvals during view-only navigation. A navigation click must not queue a model load, session creation, settings update, or turn submission.
8. Keep Send, New conversation, and workspace-changing operations subject to their existing lifecycle requirements. When the user returns during a turn, show the active transcript and Stop action immediately.

**Completion condition:** Returning from Settings to the active Agent is always a view-only operation, while an unsafe change to another conversation mode remains blocked.

### Step 3 — Expose the existing loop option clearly

1. Move the loop control out of the collapsed advanced loading section into a visible model configuration group near the top of Settings. Remove the old control to avoid duplicate widgets for the same setting.
2. Use an integer selector with only values 1 through 4, bound to the existing `UserSettings.loops`. Label it “Model loops” and include the reload/default help text described above.
3. Keep `max_rounds` under Agent settings and label/help it as an agent round limit so users do not confuse it with model recurrence.
4. Keep the default at 2 and retain the `loops` JSON field. Older settings documents that omit the field should continue to deserialize with that default.
5. Validate the selected count before saving or requesting a load. Handle an existing settings file containing an out-of-range integer with a clear correction message; do not allow a clamped widget value to silently conceal invalid persisted input. Malformed JSON continues through the existing warning/default path.
6. Continue using `UserSettings::load_config` and the existing backend validation. Do not duplicate engine-specific recurrence logic in the UI.

**Completion condition:** Users can find and select the loop count without expanding advanced settings, and invalid preferences cannot be presented as an effective loaded value.

### Step 4 — Track selected, loading, and loaded loop counts separately

1. Add the actual loop count to `AppEvent::ModelReady` as a typed field, populated from `next.info.loops` in the controller. Update every producer, match, and test fixture for that event.
2. Add GUI state for the acknowledged loaded count and the count requested by the current load generation. Capture the requested count when queuing a load, not when its completion arrives.
3. On a matching `ModelReady`, update the effective count from the event. Continue ignoring stale model-generation events.
4. If the user edits the selection while loading, leave that new value as a pending preference. Completion of an earlier request must not overwrite it or incorrectly clear “Reload required.”
5. Derive the pending-loop notice by comparing the selected value with the acknowledged value. Selecting the loaded value again should remove the notice without a reload.
6. During a load, show the requested count and loading state. After a failure, show that the runtime is unavailable and retain the selected count for correction/retry. The existing loader may already have dropped the previous engine, so do not claim it remains usable or promise automatic rollback.
7. Use the effective count for any new loaded-model label in the conversation header or Diagnostics. Existing diagnostic exports currently use selected settings; label selected versus loaded values if adding effective values there.

**Completion condition:** Saving, editing during a load, failures, and stale events cannot make the UI claim an unacknowledged loop count is active.

### Step 5 — Add an explicit model reload flow in Settings

1. Add “Reload model with selected settings” beside the visible model settings and loop status. The button should describe that it applies all selected load options, not just loops.
2. Reuse `start_model_load` and `ControllerHandle::load_model`. Keep expensive verification and native initialization on the existing worker thread.
3. Validate the candidate configuration before beginning a destructive reload. In the worker, move `config.engine_options()` validation before dropping the current engine; retain the existing fork/model verification. A validation error must not destroy a usable engine unnecessarily.
4. Block repeated reload clicks while loading, and block this Settings action during turns, approvals, pending settings updates, and session creation. Show why it is unavailable. Keep existing deferred loading behavior for other model-selection paths unless the implementation reveals a directly related defect.
5. Preserve existing conversations and the draft across a successful reload. The worker already retains its session map during `WorkerCommand::Load`; navigation and `ModelReady` handling must not create a replacement Agent session for the same workspace.
6. Leave the conversation view accessible during reload. Disable Send until the matching model-ready acknowledgment and normal session readiness conditions are satisfied.
7. Keep “Save settings” and “Apply next turn settings” explicit about their effects. Do not mark a saved loop value as loaded and do not send it through `update_settings`, which accepts only generation and agent configuration.
8. On queue failure, load failure, or worker disconnection, clear the appropriate loading/pending indicators, display the error, and allow recovery. Reuse existing error handling and add only the missing transitions. A settings rejection or disconnected worker must not trap the user in Settings.

**Completion condition:** Selecting and reloading a valid count changes the actual model configuration, keeps the same conversation, and exposes a recoverable failure state when loading fails.

### Step 6 — Add focused regression coverage

Add behavior tests around the navigation decision and lifecycle transitions; avoid tests that only assert widget labels or mirror boolean expressions.

1. **Navigation while busy:** Agent → Settings → Agent succeeds during streaming, pending settings, pending approval, and session creation. Verify that it preserves session identity, messages, draft, turn counter, workspace, and approval ID, and emits no new session/load commands.
2. **Real mode switches:** Agent → Chat remains blocked during those busy states and works when idle. Verify that sidebar eligibility and handler outcomes agree. Cover Chat → Settings → Chat as the symmetric case.
3. **Initialization:** Returning to the pending Agent loading view does not enqueue a duplicate session. A matching session failure clears pending flags and leaves navigation usable.
4. **Settings compatibility:** Round-trip loop counts 1, 2, 3, and 4; verify that an older JSON document without `loops` defaults to 2. Use temporary files or pure serialization tests rather than modifying the user's settings file.
5. **Validation:** Reject loop values 0 and 5 before reload. Keep the existing engine rejection tests and add focused app/GUI validation coverage only where new behavior is introduced.
6. **Load acknowledgment:** Selecting 3 while 2 is loaded shows a pending change. Save and next-turn apply do not clear it. A matching ready event for 3 clears it; a stale event does not.
7. **Edits during load:** Request 3, change the selection to 4, then acknowledge 3. Verify that loaded remains 3, selected remains 4, and reload is still required.
8. **Failure and retry:** Exercise queue failure, load failure, settings rejection, and event-channel disconnection. Navigation stays usable; unavailable runtime state blocks submission; retry can use the retained loop preference.

Use lightweight state fixtures and a fake/recording command sink where needed. Extract only enough event/state handling to test these transitions without a GPU, native GUI window, or real model. Real engine loading remains a separate integration check.

### Step 7 — Validate the actual desktop workflow

Run focused checks after implementation, using the repository's pinned fork and shared-library setup:

```sh
cargo fmt --all --check
cargo test -p dory-gui -p dory-app
cargo test -p dory-engine --test loops
cargo check -p dory-gui -p dory-app
```

Set `DORY_LLAMA_DIR` to the pinned fork and make its `build/bin` shared libraries available through `LD_LIBRARY_PATH` as documented by the project. Record environment blockers separately from test failures. A documentation-only plan does not require these build commands to be run now.

Then perform these manual checks:

1. Launch `dory-gui --demo` to check loop-control visibility, keyboard selection, help text, layout, and same-view navigation during scripted activity. Demo mode cannot prove a loop count reached the engine.
2. Launch a real model in a disposable workspace. Select each supported count and reload; confirm the acknowledged count and controller model details agree. Do not infer success from the preference file alone.
3. With an Agent turn streaming, open Settings, then return to Agent immediately. Verify that output continues in the same transcript and Stop remains usable.
4. Repeat during an approval request and while applying next-turn settings. Verify that the original approval can still be answered and is not duplicated.
5. Change the loop selection during an active turn. Verify that reload is explained as temporarily unavailable, Agent remains reachable, and the running turn continues under its existing model configuration.
6. Reload while idle, return to Agent during loading, and confirm that the draft and session survive. Submit a follow-up only after readiness is acknowledged.
7. Save a selected count, restart, and confirm that startup uses that preference. Check missing/invalid model and failed-load recovery without losing navigation.
8. Check the Settings controls at the supported minimum 650×480 size and the default 1050×760 size, with keyboard focus and a larger font scale.

### Step 8 — Update user documentation and review the diff

1. Update [GUI.md](GUI.md) with the visible Model loops option, supported range/default, reload action, persistence behavior, and distinction from agent rounds.
2. Document that users can return to the running Agent conversation from Settings and that mode-changing operations may still be temporarily unavailable.
3. Record which automated and real-model/manual checks actually passed. Do not claim demo checks establish native loading behavior.
4. Review only the implementation's own changes. The working tree already contains unrelated modifications and untracked GUI/app files; do not reset, overwrite, or include unrelated work as part of this task.

## Acceptance checklist

- [ ] Model loops is visible in Settings, accepts only 1–4, defaults to 2, and uses the existing saved field.
- [ ] The UI distinguishes selected, loading, and effective counts and clearly indicates when a reload is required.
- [ ] A successful explicit reload applies the selected count without replacing the conversation or discarding the draft.
- [ ] Settings → Agent works during a running turn, pending approval, pending settings, and session initialization.
- [ ] Returning to the active view queues no duplicate session, load, or submission operations.
- [ ] Actual mode switches and mutation controls retain appropriate busy/readiness restrictions.
- [ ] Failure, rejected settings, stale events, and disconnected workers cannot leave Agent navigation permanently disabled.
- [ ] Focused regression tests pass and real-model verification results or environment blockers are recorded accurately.
- [ ] The desktop guide explains the final behavior.
