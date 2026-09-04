# Task 06 — Reconciliation, ownership, rollback, and lifecycle

## Objective

Implement the safe lifecycle engine around the scheduler. The result must reconcile desired state, track ownership, compensate partial setup, restore changed configuration, stop only owned resources, and allow delete to stop and remove an environment safely.

## Scope

This task implements:

- observation-to-mutation reconciliation;
- transactional runtime state;
- ownership records;
- mutation journaling;
- rollback;
- stop;
- delete;
- shared-resource protection;
- tiling restoration contracts;
- idempotent cleanup;
- confirmation-independent delete application logic.

Concrete integrations may still be fakes. The engine must be complete before real integrations are connected.

## Dependencies

Complete Tasks 02, 03, and 05. Task 04 may provide the UI entrypoints, but lifecycle behavior must remain usable from application-level tests without a TUI.

## Files to create or change

Create or update:

~~~
src/application/reconciliation/engine.rs
src/application/reconciliation/rollback.rs
src/application/reconciliation/ownership.rs
src/application/use_cases/run.rs
src/application/use_cases/stop.rs
src/application/use_cases/delete.rs
src/application/ports/persistence.rs
src/application/ports/desktop.rs
src/domain/ownership.rs
src/domain/runtime_state.rs
~~~

## Implementation plan

### 1. Define lifecycle states

Use typed lifecycle states:

~~~
stopped
planning
active
ready
partial
rolling_back
rollback_failed
stopping
deleting
~~~

Define legal transitions and reject invalid transitions with typed errors. Do not represent lifecycle status with arbitrary strings in application logic.

### 2. Build the reconciliation engine

The run use case must:

1. pass compatibility preflight;
2. load environment.toml;
3. validate the complete configuration;
4. load existing state.toml if present;
5. observe current resources;
6. build a plan;
7. execute required actions;
8. persist ownership after every successful mutation;
9. run final verification;
10. mark the environment ready;
11. print the final summary after the TUI is restored.

If every required action is already correct, mark the environment ready without restarting anything.

### 3. Define ownership classification

Every observed resource must be classified before a mutation:

~~~
pre_existing
created_by_current_run
created_by_environment
reused_existing
shared
unknown
~~~

The classification must be persisted whenever it affects future cleanup.

Do not infer ownership from a broad name match. Use stable identifiers, recorded metadata, and active environment state.

### 4. Journal mutations

Before changing a resource, record the relevant previous state. After a successful change, persist:

~~~
action ID
resource identity
previous state
new state
ownership
compensation operation
cleanup policy
~~~

Persist the journal atomically after meaningful mutations. If persistence fails, stop scheduling further work and report a typed persistence error so cleanup can use the most recent valid state.

### 5. Implement rollback

On the first required action failure:

1. stop scheduling new dependent actions;
2. cancel or safely finish in-flight work;
3. mark the runtime state rolling_back;
4. traverse completed mutations in reverse dependency and mutation order;
5. execute compensations only for resources created or changed by this run;
6. skip pre-existing and shared resources;
7. restore previous configuration values;
8. persist each compensation result;
9. mark the state stopped or rollback_failed;
10. return the primary error and cleanup errors;
11. exit with code 1 at the CLI boundary.

Rollback must not blindly call stop on every visible resource.

### 6. Implement shared-resource protection

When deciding whether a resource can be stopped, inspect state files for other active environments under ~/.workstate. Treat a resource as shared when another active environment records a dependency on the same stable identity.

At minimum, protect:

- Docker Desktop;
- Docker containers;
- Compose projects or stacks;
- Zed windows;
- any future resource with a stable shared identity.

If shared ownership cannot be determined safely, preserve the resource and report that cleanup was skipped.

### 7. Implement stop

The stop use case must:

1. pass compatibility preflight;
2. load state.toml;
3. inspect current resource state;
4. stop owned background sessions and resources;
5. preserve pre-existing and shared resources;
6. restore recorded desktop settings;
7. tolerate already-stopped or already-removed resources;
8. persist cleanup progress;
9. remove state.toml only after cleanup is complete;
10. keep environment.toml.

Stop must be idempotent. Running it twice must not cause new destructive effects.

### 8. Implement delete

The application-level delete operation receives an already-confirmed request from the CLI. It must:

1. pass compatibility preflight;
2. stop the environment if state indicates it is active or partial;
3. retain state when cleanup fails;
4. remove only the environment directory after successful cleanup;
5. never delete configured project paths;
6. never delete resources outside the environment-owned runtime scope;
7. return a typed result suitable for the CLI summary.

The confirmation UI and --yes flag belong at the CLI boundary. The application layer must still enforce all safety checks.

### 9. Restore configuration mutations

Every mutating desktop configuration action must record:

~~~
target
previous value
applied value
restoration status
~~~

For tiling, restore the exact previous value. If tiling was already in the requested state, do not record a change.

### 10. Handle stale state

State may refer to a resource that the user stopped manually. Each cleanup handler must re-observe the resource before acting.

If the resource is gone:

- mark it cleaned or absent;
- do not fail only because it is already stopped;
- preserve enough information for diagnostics;
- continue cleanup of independent owned resources.

If identity is ambiguous, do not guess. Preserve the resource and return a safe diagnostic.

### 11. Keep cleanup policy explicit

Do not infer cleanup behavior from action kind alone. Each action handler must provide or resolve a cleanup policy that answers:

- whether the resource is stoppable;
- whether it can be shared;
- whether it should be closed or merely detached;
- whether previous configuration must be restored;
- what to do when the resource is already absent.

## Tests

Add tests for:

- complete successful reconciliation;
- already-correct reconciliation;
- partial setup;
- first failure triggering rollback;
- reverse compensation order;
- rollback failure preserving state;
- pre-existing resource preservation;
- shared resource preservation;
- stale resource handling;
- idempotent stop;
- tiling restoration;
- delete stopping before removal;
- delete refusing to remove state after failed cleanup;
- delete scope never including project directories;
- concurrent environment ownership checks.

Use fake handlers and a temporary Workstate store. Verify every mutation and compensation through a recording fake.

## Acceptance criteria

- Setup is transactional from the user's perspective.
- Every successful mutation is journaled.
- Rollback runs after a required setup failure.
- Rollback exits with code 1 through the CLI boundary.
- Pre-existing and shared resources are never stopped by this environment.
- Stop restores configuration changes and keeps desired configuration.
- Stop is idempotent.
- Delete stops first and removes only the environment directory after successful cleanup.
- Stale state does not cause unsafe guesses.
- Cleanup failures remain diagnosable.
- The lifecycle engine does not contain concrete COSMIC, tmux, Docker, Zed, or Android commands.

## Non-goals

- no real integration commands;
- no final TUI styling;
- no support for previous formats;
- no compatibility migration.

