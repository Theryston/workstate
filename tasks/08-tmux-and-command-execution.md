# Task 08: tmux and Command Execution

## Objective

Implement the safe, asynchronous command execution layer and the tmux integration used by persistent background actions.

The result must support two clearly different action lifecycles:

- One-shot commands run directly during setup, stream their output to Workstate logs, wait for completion, and leave no tmux window.
- Persistent commands run in one tmux session per environment, with one tmux window per persistent command, and continue after the main Workstate process exits.

The main process must remain a setup and reconciliation controller only. It must not become a daemon merely because an environment contains background commands.

## Dependencies

This task starts after:

- Task 03: capability registry confirms that tmux is available.
- Task 05: graph planner and scheduler classify command actions and dependencies.
- Task 06: ownership, rollback, and lifecycle rules are available.

Task 02 supplies the persisted command model and Task 04 supplies the TUI event sink.

## Scope

### In scope

- Structured asynchronous process execution with Tokio.
- Explicit working directories and environment variables.
- One-shot command execution.
- tmux session and window observation, creation, reuse, and cleanup.
- Background command ownership and runtime status.
- Output and log routing.
- Attach instructions in the completion summary.
- Failure and rollback behavior.

### Out of scope

- Running arbitrary commands during configuration without an explicit action.
- Starting a shell for every command by default.
- A Workstate daemon or background supervisor process.
- Compatibility with the previous script's state format or session naming.

## Files and module responsibilities

Create or update these modules:

~~~text
src/infrastructure/process/mod.rs
src/infrastructure/process/tokio_runner.rs
src/infrastructure/process/command_spec.rs
src/infrastructure/process/errors.rs
src/integrations/tmux/mod.rs
src/integrations/tmux/backend.rs
src/integrations/tmux/models.rs
src/integrations/tmux/errors.rs
src/application/ports/process.rs
src/application/ports/tmux.rs
tests/fakes/fake_process.rs
tests/fakes/fake_tmux.rs
tests/fixtures/tmux/
~~~

The process runner is generic infrastructure. The tmux adapter is the only layer allowed to know tmux commands, sessions, windows, target syntax, or attach syntax.

## Detailed implementation plan

### 1. Define a structured command specification

1. Represent every executable action with:
   - a stable action identifier;
   - an executable or explicit command text;
   - arguments when using argv mode;
   - an explicit execution mode;
   - a resolved working directory;
   - optional environment variables;
   - a timeout policy;
   - an output policy;
   - a persistent or one-shot lifecycle.
2. Prefer argv mode for known executables and use shell mode only when the user explicitly chooses a shell command.
3. In shell mode, invoke the configured shell with an explicit command string. Never concatenate untrusted values into a command silently.
4. Validate that the working directory exists and is a directory before launch.
5. Normalize environment variable names and preserve user values without logging secrets.
6. Keep the persisted representation stable and independent from Tokio process types.

### 2. Implement the Tokio process runner

1. Use Tokio's process API and cancellation-aware tasks.
2. Capture stdout and stderr separately.
3. Attach action id, environment id, process id when available, start time, exit status, and duration to runtime events.
4. Stream output incrementally to the logging and event channel without blocking the scheduler.
5. Apply explicit action timeouts when configured; otherwise use the shared `180-second` external-operation default. External startup waits must remain bounded and cancellable.
6. Kill only the process tree owned by the action when cancellation or rollback requires it.
7. Drain output after termination so no final error or diagnostic is lost.
8. Return a typed error for spawn failures, invalid working directories, timeout, cancellation, non-zero exit, and unavailable executable.
9. Never call unwrap, expect, panic, or silently discard a process result.

### 3. Implement tmux naming and identity

1. Derive the session name as workstate-<environment-slug>.
2. Use a stable, validated slug and reject names that can collide after normalization.
3. Derive each window name from the stable action id, not from a truncated command string.
4. Enforce that each environment owns at most one session with its exact canonical name.
5. Do not kill sessions or windows based on a loose prefix, title match, or substring.
6. Store the observed tmux session id, window id, and creation metadata in runtime state when tmux exposes them.

### 4. Observe and reconcile tmux state

1. Query sessions and windows in as few invocations as the backend allows.
2. Convert tmux output into typed models at one adapter boundary.
3. For each persistent action:
   - find the exact owned window;
   - verify its command identity and working directory when available;
   - classify it as healthy, missing, changed, exited, ambiguous, or unmanaged.
4. Reuse an exact healthy window.
5. Recreate a missing owned window.
6. Refuse to take over an ambiguous or unmanaged window without an explicit safe rule.
7. Treat a session that exists but contains no owned windows as pre-existing unless Workstate can prove it created the session.
8. Record whether a session or window existed before the current run.

### 5. Start persistent commands

1. Ensure tmux is available before scheduling a persistent action.
2. Ensure the exact environment session exists, creating it only when needed.
3. Create one window for each persistent command.
4. Start the command in the action's resolved working directory.
5. Pass configured environment variables through the tmux command safely.
6. Wait until tmux reports the window and command as started, subject to a bounded readiness timeout.
7. Do not wait for a persistent command to terminate.
8. Persist enough identity and ownership state for stop, delete, and a later reconcile.
9. Report the canonical attach command: tmux attach-session -t workstate-<environment-slug>.
10. Exit the main Workstate process after all setup actions are complete while leaving the tmux session running.

### 6. Run one-shot commands

1. Execute one-shot commands through the generic process runner, never through tmux.
2. Stream stdout and stderr as action-scoped events.
3. Wait for the command to finish before unblocking dependent actions.
4. Treat a non-zero exit status as an action failure.
5. Preserve the command's output in the environment log directory according to the runtime retention policy.
6. Do not create a session or window for commands that are not persistent.
7. If a one-shot command has already completed successfully and its check says it is still satisfied, skip it during reconcile.

### 7. Implement rollback and stop

1. Register a compensating action only for a process, session, or window created by this run.
2. On setup failure, terminate newly created persistent windows before terminating their owned session.
3. Never terminate a pre-existing tmux session or a window used by another active environment.
4. On stop, stop only persistent actions owned by the selected environment.
5. Remove the canonical session only when no owned windows remain and Workstate created the session.
6. Persist cleanup failures and show them without hiding the original failure.
7. Make cleanup idempotent: a second stop must report already absent or already stopped rather than failing because the resource is gone.

### 8. Route logs and completion information

Emit structured English events for:

- command validation;
- command start;
- stdout and stderr chunks;
- tmux session and window creation or reuse;
- readiness checks;
- command failure;
- cleanup and rollback;
- attach instructions.

Keep event messages concise. Include detailed diagnostics in log files and structured error context, not in repeated TUI lines.

## Required tests

Add unit and integration tests with fake process and tmux ports:

1. argv mode preserves executable and argument boundaries.
2. shell mode is opt-in and receives the exact configured command.
3. Invalid working directories fail before a process is spawned.
4. A one-shot command runs outside tmux and waits for its exit status.
5. A one-shot non-zero exit blocks dependents and triggers rollback.
6. Two persistent actions use one exact session and two distinct windows.
7. A healthy existing window is reused without ownership being claimed.
8. A missing owned window is recreated.
9. A prefix-collision session is ignored.
10. An ambiguous window identity produces a safe error.
11. A persistent process survives the controller completion event.
12. Stop removes only environment-owned windows and sessions.
13. A shared or pre-existing session is preserved.
14. Output chunks retain stdout and stderr attribution.
15. Timeout and cancellation are deterministic and do not leak tasks.
16. Attach instructions use the canonical session name.

No default test may require a live tmux server. Live smoke tests must be opt-in.

## Acceptance criteria

- Persistent commands continue after Workstate exits.
- One-shot commands never create tmux state.
- There is exactly one canonical session per environment and one window per persistent action.
- Reconciliation does not duplicate healthy commands.
- Stop and rollback are ownership-aware and idempotent.
- Command execution is independent from the caller's PWD.
- Process failures are typed, actionable, and rendered in English.
- The implementation passes fmt, clippy with warnings denied, fake-backend tests, and lifecycle tests.

## Non-goals

- Do not add a daemon, service unit, or long-running Workstate supervisor.
- Do not implement a compatibility layer for the previous tmux session format.
- Do not silently execute user text through a shell when argv execution is appropriate.
- Do not kill all sessions belonging to a user.
