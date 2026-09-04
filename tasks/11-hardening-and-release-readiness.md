# Task 11: Hardening, End-to-End Validation, and Release Readiness

## Objective

Integrate all completed vertical slices into a reliable first release and verify that Workstate behaves as one coherent product.

This task is complete only when a new developer or AI can build the binary, understand the supported architecture, configure an environment, start it, reconcile it, stop it, delete it, and diagnose failures without depending on the earlier shell scripts.

## Dependencies

This task starts after Tasks 01 through 10:

- foundation and dependency wiring;
- domain configuration and persistence;
- platform detection and capability registry;
- CLI and Ratatui interface;
- graph planner and scheduler;
- reconciliation and lifecycle;
- COSMIC and Zed;
- tmux and command execution;
- Docker;
- Android emulator.

## Scope

### In scope

- Final composition-root wiring.
- End-to-end behavior with fake integrations.
- Unsupported-platform behavior for every command.
- Transaction and rollback scenario coverage.
- Persistence recovery and atomic-write verification.
- TUI snapshot and interaction verification.
- Performance benchmarks and startup profiling.
- Security and failure-mode review.
- CI quality gates and release build verification.
- Synchronizing implementation documentation with AGENTS.md.

### Out of scope

- Migration from old configuration, state, or session formats.
- Runtime compatibility with the previous project.
- New product integrations not already described by the MVP.
- Choosing a package distribution service or publishing a release without a separate product decision.

## Files and module responsibilities

Create or update these files as needed:

~~~text
tests/scenarios/start_environment.rs
tests/scenarios/stop_environment.rs
tests/scenarios/delete_environment.rs
tests/scenarios/unsupported_platform.rs
tests/scenarios/rollback.rs
tests/scenarios/shared_resources.rs
tests/snapshots/
benches/startup.rs
benches/planning.rs
.github/workflows/ci.yml
README.md
Cargo.toml
AGENTS.md
~~~

Keep test scenarios independent from the live user's desktop. Use fake ports and deterministic clocks for normal CI. Live integration checks must be explicit opt-in commands.

## Detailed implementation plan

### 1. Finalize composition and capability gating

1. Build a single explicit AppContext in the composition root.
2. Detect the runtime OS, distribution, desktop, and required tools before constructing platform-dependent adapters.
3. Reject every Workstate command on an unsupported runtime before:
   - opening the TUI;
   - opening the add editor;
   - reading or writing an environment;
   - starting a process;
   - changing a desktop setting.
4. Ensure supported runtimes remain silent about successful detection.
5. Ensure unsupported errors show:
   - what Workstate detected;
   - the currently supported combination, Linux plus Pop!_OS plus COSMIC plus tmux;
   - that the command cannot run on the current system.
6. Verify that command dispatch, including future command placeholders, passes through the same compatibility gate.
7. Confirm there are no global mutable registries or hidden singleton clients.

### 2. Verify the complete command surface

Test the public behavior:

1. Workstate with no arguments opens the selector.
2. Workstate with an environment name starts or reconciles it directly.
3. A missing environment returns an actionable error suggesting workstate add followed by the name.
4. Workstate add followed by a name opens the dynamic editor and saves one environment directory.
5. Adding an existing name edits the environment through the same MVP flow.
6. Workstate stop followed by a name restores and cleans only owned resources.
7. Workstate delete followed by a name stops first and then removes only that environment's Workstate data.
8. Delete confirmation is shown by default and the yes flag bypasses only confirmation.
9. All user-facing output, prompts, errors, help, logs, and documentation are English.
10. Every command returns a predictable non-zero exit status on failure.

### 3. Verify the complete environment scenario

Create a deterministic fake-backend scenario containing:

- a Docker Desktop requirement;
- a Compose or container service;
- a one-shot preparation command;
- a persistent tmux command for a frontend;
- a persistent tmux command for an API;
- a Zed project in one workspace;
- a second Zed project in another workspace;
- an Android emulator in a selected workspace;
- independent tiling preferences.

Verify that:

1. The planner produces the expected dependency order.
2. Independent actions can execute concurrently.
3. Engine and emulator readiness unblock their dependents exactly once.
4. One-shot commands finish and are not placed in tmux.
5. Persistent commands remain in one environment session with one window each.
6. Zed projects open in their saved directories.
7. Windows and emulator are placed in the requested workspaces.
8. A successful run exits after the setup summary and leaves persistent resources alive.
9. A second run reports already-correct resources instead of duplicating them.

### 4. Verify transactional failure and rollback

Add scenarios that fail at each major boundary:

1. platform compatibility;
2. configuration validation;
3. one-shot command;
4. tmux session or window creation;
5. Docker engine readiness;
6. container or Compose readiness;
7. Zed launch or window observation;
8. workspace mutation;
9. emulator boot or window observation;
10. persistence commit.

For each scenario:

- confirm the first failure is returned;
- confirm no later graph node starts;
- confirm completed mutations are rolled back in reverse order;
- confirm pre-existing resources remain unchanged;
- confirm cleanup failures are preserved as secondary diagnostics;
- confirm the process exits with status 1;
- confirm runtime state remains sufficient for a later stop when rollback is incomplete.

### 5. Verify stop, shared resources, and delete

1. Start two environments that reference the same pre-existing Docker resource or Zed process.
2. Stop the first environment and confirm the shared resource remains available.
3. Stop the second environment and confirm only resources that Workstate actually owns are cleaned.
4. Re-run stop after manual deletion of one resource and verify idempotent success.
5. Delete an environment after a successful stop and verify only its directory is removed.
6. Exercise delete after a failed start and verify cleanup is attempted before deletion.
7. Confirm no command deletes data outside the selected environment directory without an explicit future product rule.

### 6. Verify persistence and recovery

1. Confirm all data is stored under the user's home directory at .workstate.
2. Confirm each environment has its own directory with environment.toml, state.toml, logs, and runtime.
3. Verify desired configuration and runtime state are never merged into one ambiguous document.
4. Verify atomic writes use a temporary file in the same directory, flush or sync according to the persistence policy, then rename.
5. Verify interrupted writes do not replace a valid previous file with partial TOML.
6. Verify malformed TOML returns an actionable error and does not mutate external resources.
7. Verify unknown future TOML fields are handled according to the compatibility policy documented in the domain model.
8. Verify concurrent commands do not corrupt the same environment state. Use a lock or a clearly defined single-writer policy.
9. Verify secrets are not persisted or logged unless a future explicit secret-management feature changes the policy.

### 7. Verify TUI behavior and accessibility

1. Add snapshots for the selector, add editor, validation error, setup progress, rollback failure, already-correct summary, stop summary, delete confirmation, and unsupported-platform error.
2. Ensure long names, long paths, long commands, Unicode paths, empty lists, and terminal resizing render safely.
3. Keep the TUI event-driven and non-blocking.
4. Ensure the main terminal is restored even when a render loop or action fails.
5. Ensure errors remain understandable when color is unavailable.
6. Ensure non-interactive command errors do not wait for input.
7. Verify the completion view contains exact attach and inspection commands when persistent tmux actions exist.
8. Verify no success path mentions compatibility detection when the platform is supported.

### 8. Measure and protect performance

Measure internal Workstate operations separately from external readiness waits:

1. CLI argument parsing and command dispatch.
2. supported-platform detection with cached immutable metadata.
3. TOML loading and validation for a representative environment.
4. graph construction and validation.
5. reconciliation planning against fake snapshots.
6. ownership lookup and runtime-state serialization.
7. initial TUI frame creation.

Use criterion or an equivalent stable benchmark harness. Establish a target below 200 milliseconds for normal internal operations and document any unavoidable exception. Do not count Docker startup, emulator boot, Zed launch, desktop IPC latency, or user typing as internal execution time.

Avoid premature complexity, but remove unnecessary process launches, repeated filesystem scans, duplicate observations, serial waits for independent nodes, and unbounded log buffering. No optimization may weaken rollback or safety.

### 9. Run security and failure-mode review

Review the whole codebase for:

- accidental shell interpolation;
- command injection through paths, names, or arguments;
- unsafe process termination;
- broad tmux session or container matching;
- path traversal in environment names;
- logs containing secrets;
- symlink or path replacement hazards in the state directory;
- partial persistence;
- hidden ignored Results;
- panic-based control flow;
- blocking filesystem or process work on the async runtime;
- terminal cleanup on every early return.

Fix findings within this task when they are required for MVP correctness. Record deliberate limitations in documentation rather than hiding them.

### 10. Enforce quality gates in CI

The default CI pipeline must run:

~~~text
cargo fmt --all -- --check
cargo check --all-targets
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
cargo build --release
~~~

Add benchmark or snapshot checks only when they are deterministic enough for CI. Keep live COSMIC, Docker, tmux, Zed, and Android checks opt-in and clearly separated from the default suite.

Review dependencies for unnecessary additions, feature flags, compile-time cost, and license or maintenance concerns. Keep the binary lean and preserve the single-crate structure unless a demonstrated boundary requires a workspace.

### 11. Update developer documentation

1. Keep AGENTS.md aligned with the actual module tree, commands, persistence format, ports, testing commands, and safety rules.
2. Document the supported runtime without suggesting that unsupported platforms are silently accepted.
3. Document the add flow and the environment model with an English TOML example.
4. Document the distinction between one-shot commands and persistent tmux commands.
5. Document the attach command shown after a successful start.
6. Document stop and delete ownership behavior.
7. Record any implementation deviations as explicit decisions with a reason and test coverage.
8. Do not describe the earlier scripts as an input format, migration source, or compatibility surface.

## Required tests

The task is not complete without:

1. Unit tests for every domain invariant.
2. Fake-backend integration tests covering all action families.
3. End-to-end command-dispatch tests for start, selector, add, stop, and delete.
4. Unsupported-platform tests for every public command.
5. Transaction rollback tests at every integration boundary.
6. Shared-resource ownership tests.
7. Persistence crash-safety and concurrent-writer tests.
8. Ratatui snapshot tests and terminal cleanup tests.
9. Performance benchmarks for internal operations.
10. A release build test.

## Acceptance criteria

- All MVP capabilities work together through ports and explicit dependency injection.
- Unsupported systems fail before any user or filesystem mutation.
- Supported systems do not show unnecessary detection messages.
- A successful environment start is fast to plan, visually clear, and exits after setup while background resources remain available.
- A partially active environment is repaired rather than blindly restarted.
- Any setup failure rolls back completed owned work, returns the original error, and exits with status 1.
- Stop restores only changes made by the environment and preserves shared or pre-existing resources.
- Delete stops first and removes only the selected environment directory.
- Persistence is TOML-based, per environment, atomic, recoverable, and independent from PWD.
- All code, UI, prompts, errors, logs, tests, and documentation are English.
- Comments remain absent unless an unavoidable non-obvious constraint truly requires one.
- The default quality gates pass with no warnings and no panics hidden behind prohibited constructs.
- Internal performance targets are measured and kept below 200 milliseconds where the operation is under Workstate's control.

## Non-goals

- Do not add compatibility or migration support for the old scripts.
- Do not add integrations that are not part of the defined MVP.
- Do not change the user's desktop configuration outside resources explicitly owned by the environment.
- Do not weaken safety, rollback, or diagnostics to improve a benchmark.
