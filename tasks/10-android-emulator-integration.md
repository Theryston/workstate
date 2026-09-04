# Task 10: Android Emulator Integration

## Objective

Implement Android emulator support as a first-class environment resource.

An environment must be able to select an Android Virtual Device, start it when it is missing, wait until it is genuinely usable, place its window in the requested COSMIC workspace, and stop it later only when Workstate owns that emulator instance.

The emulator is a managed external process, not a user command that should be placed in the environment's tmux session. It must survive the main Workstate process exiting, while its lifecycle remains represented in runtime state for reconciliation and stop.

## Dependencies

This task starts after:

- Task 03: capability registry and platform support checks.
- Task 05: dependency graph planning and scheduling.
- Task 06: ownership, rollback, and lifecycle.
- Task 07: COSMIC workspace, window, and tiling ports.
- Task 08: Tokio process execution and bounded waits.

Task 02 supplies the persisted emulator configuration and Task 04 supplies the interactive editor and TUI events.

## Scope

### In scope

- Detecting the Android SDK emulator and adb capabilities.
- Selecting an AVD during environment configuration.
- Listing available AVDs during the add flow.
- Starting a selected AVD when necessary.
- Detecting an already-running matching emulator.
- Waiting for the emulator and Android boot process.
- Checking adb connectivity and device readiness.
- Moving the emulator window to a selected COSMIC workspace.
- Recording process, device, and window ownership.
- Safe rollback, stop, and delete behavior.

### Out of scope

- Installing the Android SDK, system images, or AVDs.
- Modifying an AVD definition automatically.
- Supporting Android devices connected over USB.
- Supporting non-COSMIC window managers in this task.
- Running the emulator inside tmux.
- Migrating emulator settings from the earlier scripts.

## Files and module responsibilities

Create or update these modules:

~~~text
src/integrations/android/mod.rs
src/integrations/android/emulator.rs
src/integrations/android/adb.rs
src/integrations/android/models.rs
src/integrations/android/checks.rs
src/integrations/android/errors.rs
src/application/ports/android.rs
tests/fakes/fake_android.rs
tests/fixtures/android/
~~~

The Android adapter owns emulator and adb command details. It must use the generic process port and the desktop port; it must not call the COSMIC or tmux implementation directly.

## Detailed implementation plan

### 1. Define the Android port and identities

1. Expose typed operations for:
   - listing AVDs;
   - observing emulator processes and adb devices;
   - starting an AVD;
   - waiting for a device;
   - evaluating boot readiness;
   - stopping an owned emulator;
   - resolving the emulator window for desktop placement.
2. Represent an AVD identity separately from a runtime device identity:
   - configured AVD name;
   - emulator serial;
   - process id when available;
   - window identifier when available.
3. Record whether each identity existed before the current run.
4. Return explicit outcomes for available, already running, started, booting, ready, missing, ambiguous, incompatible, timed out, and failed.
5. Do not identify an emulator solely by a generic window title or by the existence of any adb device.

### 2. Implement configuration-time selection

1. During the add editor, query the Android capability only after platform compatibility has been accepted.
2. List AVD names in a dialoguer selector with a clear refresh or retry path.
3. Store the selected AVD name in environment TOML, not the caller's current device serial.
4. Let the user choose the workspace target and per-workspace tiling preference independently from the emulator selection.
5. Let the user define whether the emulator is required, optional, or disabled for that environment according to the domain model.
6. Validate that the selected AVD still exists before saving.
7. If the SDK is unavailable, show a concise English error explaining how to install or expose the emulator tools; do not write a partially configured action.

### 3. Observe current emulator state

1. Inspect running emulator processes before launching.
2. Query adb devices and obtain the state of each emulator serial.
3. Match a running emulator to the configured AVD using stable emulator metadata when available.
4. Reuse a matching running emulator without claiming ownership.
5. Report a conflict when the configured AVD has an ambiguous match or when a different emulator already occupies the intended identity.
6. Keep adb server ownership separate from emulator process ownership. Do not stop a pre-existing adb server during stop.
7. Refresh observations after every start or stop operation that can change the process or device list.

### 4. Start and wait for an emulator

1. Launch the emulator in detached mode through the process port, with the selected AVD and only explicitly configured arguments.
2. Do not embed the emulator in tmux.
3. Capture the process identity and launch metadata immediately after spawn.
4. Wait for the expected emulator serial to appear with a bounded polling policy.
5. Wait for adb to report an accepted device state.
6. Check Android boot completion using an observable system property or equivalent supported readiness check.
7. Apply a final bounded device readiness check before unblocking dependent actions.
8. Stream useful emulator startup diagnostics to the environment log without flooding the TUI.
9. If startup times out, include the AVD name, observed serials, last adb state, and a safe diagnostic tail in the typed error.
10. Register rollback only after Workstate has established ownership of the newly spawned process.

### 5. Resolve and place the emulator window

1. Observe COSMIC windows after the emulator process becomes visible.
2. Match the window using a stable process or window identity where available.
3. Never move a window based only on a title that could belong to another emulator.
4. Resolve the configured workspace through the desktop port.
5. Move the owned emulator window to that workspace.
6. Apply tiling only when requested for that workspace.
7. Persist the resolved workspace and window identities for later stop and rollback.
8. If the window does not appear before the bounded timeout, fail the action and roll back the owned emulator process.

### 6. Integrate emulator readiness with the graph

1. Expose emulator boot readiness as the completion condition of the emulator action.
2. Allow actions that only need adb readiness to depend on the emulator without duplicating polling.
3. Ensure actions that open Zed or run commands in parallel do not begin before their declared emulator dependency completes.
4. Permit independent desktop actions to run concurrently when the resource graph and desktop backend allow it.
5. Publish structured events for selection, observation, launch, device connection, boot readiness, window placement, and cleanup.

### 7. Implement rollback and stop

1. On a later setup failure, stop only an emulator process created by the current run.
2. Preserve an already-running matching emulator.
3. If the user closes an owned emulator manually, classify it as already absent during stop rather than failing solely because it is gone.
4. Do not stop unrelated emulators or physical devices.
5. Restore tiling through the desktop mutation journal, subject to ownership and shared-resource checks.
6. Keep runtime state when cleanup is incomplete so a later stop can retry.
7. Delete only the selected environment directory after the common stop path completes.

## Required tests

Add unit and integration tests with fake Android, process, and desktop ports:

1. A configured AVD is listed and selected deterministically.
2. A missing AVD is rejected before configuration is written.
3. An already-running matching emulator is reused without ownership.
4. An unrelated emulator is not mistaken for the configured AVD.
5. A new emulator process receives ownership only after a successful spawn.
6. The serial wait handles offline, booting, ready, and timeout states.
7. Boot readiness succeeds and fails with deterministic diagnostics.
8. An emulator is never started through tmux.
9. The emulator window is matched safely and moved to the requested workspace.
10. A missing emulator window causes process rollback.
11. Stop preserves pre-existing emulators and adb servers.
12. Stop removes an owned emulator and is idempotent.
13. Shared desktop resources are not closed or restored unsafely.
14. No default test requires an Android SDK, adb, an emulator, or COSMIC.

## Acceptance criteria

- The add flow can select and persist an AVD without relying on PWD.
- Starting an environment launches or reuses the correct emulator and waits for actual boot readiness.
- The emulator remains running after the Workstate controller exits.
- Dependent actions do not start before emulator readiness.
- Window placement and tiling use the existing desktop abstractions.
- Rollback and stop affect only emulator processes and desktop mutations owned by the environment.
- Missing tools and startup failures produce actionable English errors.
- The implementation passes fake-backend tests, lifecycle tests, fmt, and clippy with warnings denied.

## Non-goals

- Do not install or update Android tooling automatically.
- Do not add physical-device management.
- Do not add a migration layer for the earlier emulator configuration.
- Do not use fixed sleeps where an adb or boot readiness check is available.
