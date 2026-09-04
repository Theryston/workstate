# Task 02 — Domain, configuration, and persistence

## Objective

Define the pure domain model for environments and implement safe TOML persistence under ~/.workstate/<environment-slug>/. The result must represent a multi-project environment as a dependency graph, preserve user-entered paths, and keep desired configuration separate from runtime ownership state.

## Scope

This task implements:

- environment identity;
- desktop workspace specifications;
- action specifications;
- execution modes;
- readiness checks;
- dependency references;
- graph validation primitives;
- runtime state records;
- path resolution;
- TOML stores;
- atomic writes;
- environment directory creation.

Do not call COSMIC, tmux, Docker, Zed, Android, or any other external system.

## Files to create or change

Create or update:

~~~
src/domain/environment.rs
src/domain/action.rs
src/domain/graph.rs
src/domain/workspace.rs
src/domain/resource.rs
src/domain/ownership.rs
src/domain/runtime_state.rs
src/domain/error.rs
src/infrastructure/filesystem/mod.rs
src/infrastructure/filesystem/local.rs
src/infrastructure/persistence/mod.rs
src/infrastructure/persistence/paths.rs
src/infrastructure/persistence/toml_store.rs
src/infrastructure/persistence/atomic_write.rs
src/application/ports/filesystem.rs
src/application/ports/persistence.rs
~~~

Update module declarations and AppContext wiring as necessary.

## Implementation plan

### 1. Define environment identity

Create typed environment identity rather than passing arbitrary strings throughout the application.

The model must contain:

~~~
EnvironmentName
EnvironmentSlug
EnvironmentConfig
~~~

The display name may contain spaces and user-friendly characters. The slug must be deterministic, lowercase, filesystem-safe, non-empty, and stable once persisted.

Reject:

- an empty name;
- a slug that becomes empty;
- path separators;
- . and ..;
- a slug that escapes the Workstate root;
- two environments resolving to the same slug.

Do not silently rename an existing environment during an edit.

### 2. Define workspace specifications

Create WorkspaceSpec with:

~~~
id
target
name, when required
tiling
~~~

Represent workspace target modes as a typed enum:

~~~
current
existing
next_empty
create
none
~~~

The tiling value must distinguish an explicit enabled or disabled desired state from the absence of a requested change if that distinction is required by the implementation. When a workspace is explicitly changed, runtime state must later record its previous value.

### 3. Define action specifications

Create ActionSpec with:

~~~
id
kind
depends_on
working_directory
desktop_workspace
execution_mode
parameters
readiness_checks
timeout
retry_policy
cleanup_policy
~~~

Use typed action kinds for built-in behavior while leaving a structured path for future custom actions. Do not represent an environment with parallel arrays.

Use stable action IDs as the identity used by planning, runtime state, logs, and rollback. Editing a label must not silently change an action ID.

### 4. Define execution modes and checks

Represent:

~~~
run_once
background
~~~

as a typed enum. Only command-like actions need an execution mode; other actions may use an explicit lifecycle field when their semantics differ.

Represent the initial readiness checks:

~~~
none
tcp
http
command
delay
container
compose
~~~

Every check must carry enough typed data to validate itself and an explicit timeout where waiting is possible.

### 5. Validate the graph

Implement pure graph validation and traversal helpers:

- unique action IDs;
- existing dependency IDs;
- no self-dependencies;
- no cycles;
- deterministic traversal order for equal-priority nodes;
- workspace references that point to configured workspaces;
- required action parameters;
- valid timeouts and retry counts.

Graph validation must return structured errors containing the affected action IDs. The UI and CLI will later render those errors in English.

Do not execute any action during validation.

### 6. Define runtime ownership state

Create serializable runtime types for:

~~~
RuntimeState
ResourceRecord
MutationRecord
RunStatus
CleanupStatus
~~~

The runtime state must be able to represent:

- active and ready environments;
- partial setup;
- rollback in progress;
- rollback failure;
- stopped environments;
- pre-existing resources;
- resources created by the current run;
- resources created by the environment;
- reused resources;
- shared resources;
- previous values of changed settings;
- resource-specific integration metadata.

Do not store desired configuration in state.toml.

### 7. Implement Workstate paths

Create a path service that resolves:

~~~
~/.workstate
~/.workstate/<slug>
~/.workstate/<slug>/environment.toml
~/.workstate/<slug>/state.toml
~/.workstate/<slug>/logs
~/.workstate/<slug>/runtime
~~~

The root must be derived from the user's home directory, not the current working directory. Keep path construction in one infrastructure module.

The service must reject path traversal and must never return a deletion target outside the selected environment directory.

### 8. Resolve configured paths at runtime

Preserve user-entered path forms such as:

~~~
~/Projects/blog/api
$HOME/Projects/blog/frontend
~~~

Implement expansion and validation at execution time. Do not rewrite the saved representation to an absolute path during normal saves.

The resolver must:

- expand ~ and $HOME;
- reject unresolved or malformed values;
- optionally canonicalize only for observation and process execution;
- return a typed error when a required directory does not exist;
- never use the process current directory as a fallback.

### 9. Implement TOML persistence

Use Serde and TOML for typed serialization. Include a schema version in both configuration and state.

environment.toml is the desired-state source of truth. state.toml contains runtime observations and ownership.

Implement:

- load;
- validate;
- save;
- atomic replace;
- missing-file handling;
- malformed-file errors;
- schema-version errors.

Write a complete temporary file in the same directory, flush it, and atomically replace the target. Never truncate the only valid file before a replacement is ready.

### 10. Define edit and cancellation behavior

The persistence layer must support editing by loading an existing configuration into memory, applying changes, validating the complete result, and saving only after the caller confirms.

Canceling an edit must leave the original file byte-for-byte unchanged.

## Tests

Add unit and integration tests for:

- name and slug validation;
- path traversal rejection;
- path expansion;
- current-directory independence;
- TOML round trips;
- missing and malformed files;
- atomic-write failure behavior;
- schema-version handling;
- graph cycles and missing dependencies;
- deterministic graph ordering;
- runtime state ownership records;
- canceled edits preserving the original configuration;
- safe environment directory deletion targets.

Use tempfile for filesystem tests. Do not use the real home directory.

## Acceptance criteria

- A complete environment can be serialized and deserialized from TOML.
- Desired configuration and runtime state are separate types and files.
- Every saved environment is scoped to ~/.workstate/<slug>/.
- User-entered ~ and $HOME paths remain preserved.
- No code depends on the current process working directory.
- Writes are atomic.
- Graph validation is pure and deterministic.
- Runtime state can represent ownership and restoration data.
- No external process or platform API is called.
- All tests pass without modifying the user's machine.

## Non-goals

- no platform detection;
- no CLI or TUI;
- no real process execution;
- no integrations;
- no support for previous configuration formats;
- no migration code.

