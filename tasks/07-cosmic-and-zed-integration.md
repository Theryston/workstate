# Task 07: COSMIC and Zed Integration

## Objective

Implement the first concrete desktop integration for Pop!_OS with COSMIC and the first concrete editor integration for Zed.

The integration must let an environment:

- Resolve an action to the current workspace, a specific existing workspace, the next empty workspace, or a newly created workspace.
- Open or reuse a Zed window for an explicitly configured project directory.
- Move an owned window to the workspace selected by the environment.
- Enable tiling only when the environment requests it for that workspace.
- Record every desktop mutation needed to restore the previous state during stop or rollback.
- Distinguish resources that already existed from resources created by Workstate.

The rest of Workstate must interact with these capabilities through application ports. No use case may know how COSMIC IPC, a desktop command, a window identifier, or a Zed launch command works.

## Dependencies

This task starts after:

- Task 03: platform detection and capability registry.
- Task 05: dependency graph planning and scheduling.
- Task 06: reconciliation, ownership, rollback, and lifecycle.

The platform registry must reject unsupported desktops before this integration is constructed. The lifecycle layer must already be able to persist ownership and compensating actions.

## Scope

### In scope

- COSMIC workspace observation and mutation.
- COSMIC window observation and movement.
- COSMIC tiling observation and mutation.
- Zed process discovery and launch.
- Reusing an already-open matching Zed project window when it can be identified safely.
- Waiting for a launched Zed window to become observable.
- Ownership-aware cleanup and restoration.
- Typed parsing of all external command or IPC output.

### Out of scope

- GNOME, KDE, Windows, WSL, or any other desktop backend.
- General-purpose window management for applications other than resources modeled by Workstate.
- Reproducing or migrating the earlier shell scripts.
- Guessing a project directory from the caller's current directory.

## Files and module responsibilities

Create or update these modules:

~~~text
src/platform/desktop/cosmic.rs
src/integrations/cosmic/mod.rs
src/integrations/cosmic/backend.rs
src/integrations/cosmic/models.rs
src/integrations/cosmic/errors.rs
src/integrations/zed/mod.rs
src/integrations/zed/backend.rs
src/integrations/zed/errors.rs
src/application/ports/desktop.rs
src/application/ports/editor.rs
tests/fakes/fake_desktop.rs
tests/fakes/fake_editor.rs
tests/fixtures/cosmic/
~~~

Keep the port definitions independent from COSMIC and Zed. The concrete modules may depend on the generic process runner, path resolver, clock, and logging facilities, but they must not call another integration directly.

## Detailed implementation plan

### 1. Define desktop and editor port contracts

1. Expose an observation API that returns a typed snapshot of:
   - desktop workspaces and stable identifiers;
   - workspace names and positions when available;
   - windows, application identity, title, stable window identifier, and workspace;
   - tiling state for each relevant workspace.
2. Expose mutations for:
   - creating a workspace when supported;
   - moving a window;
   - enabling or disabling tiling;
   - opening or focusing an application resource.
3. Return structured outcomes such as created, already present, reused, changed, unchanged, unavailable, and ambiguous.
4. Make destructive or irreversible behavior explicit in the port contract. A caller must never infer ownership from a window title or from a process name alone.
5. Define the editor port around a project identity and path, not around a hard-coded Zed command. The Zed adapter owns command-line flags and process detection.

### 2. Implement typed COSMIC observation

1. Encapsulate the supported COSMIC IPC or command mechanism inside the COSMIC adapter.
2. Route every external invocation through the process port. Do not spawn commands from the domain or application layers.
3. Parse output into dedicated serde models first, then convert those models into integration-neutral desktop models.
4. Reject malformed, incomplete, or ambiguous output with a typed error containing the operation and a short actionable explanation.
5. Never parse output with scattered string searches in several modules. There must be one conversion boundary per external format.
6. Cache only immutable capability information. Workspace and window snapshots must be refreshed whenever a mutation could have changed them.
7. Keep observation bounded and fast. Use one batched query when the backend supports it rather than one process invocation per window.

### 3. Resolve workspace destinations

Implement a deterministic resolver for the workspace target declared by an action:

1. The current target maps to the workspace containing the focused window at the start of the operation.
2. An existing target must match exactly one workspace. Report an ambiguity instead of choosing arbitrarily.
3. The next-empty target selects the first empty workspace according to the observed desktop order.
4. A create-new target creates a workspace only when the backend supports it, then waits until it appears in a fresh snapshot.
5. A none target leaves the resource in its current location and must not cause a desktop mutation.
6. If a target disappears between planning and execution, refresh the snapshot and retry the safe resolution once before returning an error.
7. Store the resolved workspace identifier in runtime state so rollback and stop do not depend on a later name lookup.
8. Treat each configured workspace ID as a virtual binding. Resolve that binding once during run preparation and reuse its concrete workspace identity for every referencing action, including dependent actions.
9. Reserve identities selected for distinct next-empty bindings in deterministic configuration order. Never resolve next-empty independently after another action has changed workspace occupancy.
10. When an active runtime state contains the concrete identity previously assigned to a `next_empty` binding and that workspace still exists, reuse it before searching for another empty workspace.

### 4. Implement per-workspace tiling

1. Read the current tiling state before changing it.
2. Change tiling only when the action explicitly requests enabled or disabled behavior.
3. Record the previous state, the new state, the workspace identifier, and the owning environment in the mutation journal.
4. If tiling is already in the requested state, record an unchanged observation and no inverse mutation.
5. During rollback or stop, restore the recorded previous state only if Workstate still owns the mutation and the workspace still exists.
6. If another active environment owns a later mutation on the same workspace, leave that state untouched and report why it was preserved.
7. Do not restore a user's focused workspace or rewrite unrelated workspace settings.

### 5. Implement Zed project launch and reuse

1. Require the project path to come from the saved action configuration after path resolution and validation.
2. Observe existing Zed windows before launching anything.
3. Reuse a matching window only when the adapter can establish a stable project identity. A title-only match is insufficient.
4. If no safe match exists, launch Zed with the resolved path through the process port.
5. Mark a launch as owned only after the process or window can be associated with the requested project.
6. Poll the desktop and editor observation ports with a bounded timeout and cancellation support until the new window is visible.
7. Move a newly launched or otherwise newly provisioned window to the resolved workspace.
8. If a matching Zed project window already exists anywhere on the desktop, classify the action as already correct from its project key alone; a different current workspace must not trigger a move or a duplicate launch.
9. If a user manually moved a reused window after Workstate observed it, stop must not close it or move it back.
10. Focus a window only when the action requests focus; opening an environment must not steal focus unnecessarily.
11. Support any number of Zed project actions in one environment. Derive the action's identity key from its resolved project directory, never from `ActionKind` alone.
12. If desktop observation does not expose project metadata, serialize the launch-and-observe handoff, refresh the pre-launch snapshot after acquiring the coordination, and correlate only windows that appeared during that handoff. On later runs, a persisted canonical project key may be matched to its persisted stable window identity after verifying that the current window is still a Zed window; a title-only match is never sufficient.

### 6. Integrate with reconciliation and rollback

1. Make each desktop and editor action idempotent.
2. On rerun, observe each action's resource key independently from its workspace placement and return unchanged or repaired outcomes instead of blindly launching duplicate Zed windows or toggling tiling.
3. Register inverse operations only for mutations performed by the current run.
4. Preserve pre-existing windows and desktop settings.
5. If a later action fails, roll back moves, launches, workspace creation, and tiling changes in reverse dependency order.
6. If rollback is incomplete, return the original error plus a structured cleanup warning and persist enough state for a later stop attempt.
7. Shared-resource checks must happen before closing a Zed window or restoring a workspace setting.

### 7. Add user-facing operation events

Emit concise English events for:

- inspecting workspaces and windows;
- resolving a workspace destination;
- reusing or launching Zed;
- waiting for a window;
- moving a window;
- enabling or preserving tiling;
- restoring a previous state;
- skipping cleanup because the resource was pre-existing or shared.

The TUI decides how to render these events. The integration must not print directly to stdout or stderr.

## Required tests

Add unit and integration tests with fake ports:

1. Exact workspace matching succeeds.
2. Duplicate workspace names return an ambiguity error.
3. Current, next-empty, create-new, and none targets resolve correctly.
4. A missing workspace is refreshed and safely retried once.
5. Existing enabled tiling produces no mutation.
6. Requested tiling changes create a reversible journal entry.
7. A reused Zed project is not marked as owned.
8. A newly launched Zed project is marked as owned only after observation confirms it.
9. A title collision does not cause unsafe reuse.
10. An editor timeout produces a typed error and rollback.
11. Stop restores only tiling and windows owned by the environment.
12. Shared Zed usage prevents premature close.
13. Malformed COSMIC output is rejected at the adapter boundary.
14. No test depends on a real COSMIC session or a real Zed process.

Add opt-in live smoke tests only if a developer explicitly enables them. They must skip cleanly when COSMIC or Zed is unavailable and must never run in the default test command.

## Acceptance criteria

- The application layer contains no COSMIC command names, Zed flags, or window-manager parsing.
- A supported environment can place Docker Desktop, Zed, and an emulator in different requested workspaces.
- Zed opens in the configured directory without depending on the caller's PWD.
- Multiple Zed project actions with different configured directories complete independently, including when the scheduler starts them concurrently.
- Tiling is independently configurable per workspace.
- Re-running an already correct environment does not create duplicates or toggle desktop state.
- Rollback and stop preserve resources that were not created or changed by Workstate.
- All external failures become actionable typed errors and all user-facing text is English.
- The implementation passes formatting, clippy with warnings denied, unit tests, fake-backend integration tests, and UI event tests.

## Non-goals

- Do not add a migration layer for the previous script-based project.
- Do not add compatibility aliases for old config or state paths.
- Do not introduce a compile-time platform feature matrix for desktop selection.
- Do not add a generic window manager abstraction that is not required by the current product model.
