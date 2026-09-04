# Task 01 — Foundation and architecture

## Objective

Create the Rust application foundation without implementing product integrations yet. The result must compile, expose the intended module boundaries, initialize Tokio, and provide a composition root that later tasks can extend without moving business logic into main.rs.

## Scope

This task establishes:

- the single-crate Rust structure;
- Rust stable and edition 2024;
- production and development dependencies;
- main.rs and lib.rs;
- the initial AppContext;
- top-level typed error plumbing;
- port and adapter module boundaries;
- the first quality checks.

Do not implement COSMIC, tmux, Docker, Zed, Android, environment persistence, or real CLI behavior in this task.

## Files to create or change

Create or update:

~~~
Cargo.toml
src/main.rs
src/lib.rs
src/cli/mod.rs
src/ui/mod.rs
src/domain/mod.rs
src/application/mod.rs
src/application/context.rs
src/application/ports/mod.rs
src/infrastructure/mod.rs
src/platform/mod.rs
src/integrations/mod.rs
~~~

Create the remaining directories from the canonical tree in AGENTS.md when needed by module declarations. Empty directories are not required to be committed; every declared module must have a valid Rust file.

## Implementation plan

### 1. Configure the crate

Keep the existing package name workstate, version, description, and edition unless a deliberate product decision changes them.

Add only the dependencies needed by the architecture:

~~~
clap
crossterm
dialoguer
ratatui
tokio
serde
toml
thiserror
tracing
tracing-subscriber
serde_json
which
~~~

Add development dependencies only when the first tests use them:

~~~
insta
tempfile
assert_cmd
predicates
criterion
~~~

Configure Tokio for the runtime, process coordination, synchronization, timers, and signal handling needed by the application. Do not enable unrelated Tokio features.

Do not add anyhow. Errors must remain typed.

### 2. Keep main.rs thin

src/main.rs must only:

1. initialize the Tokio runtime;
2. construct the concrete AppContext;
3. call the library-level CLI runner;
4. map the final result to the documented exit code.

It must not parse TOML, call an external command, render a widget, or contain a product rule.

### 3. Establish library modules

src/lib.rs should declare the top-level modules and expose only the library surface required by integration tests and the binary entrypoint.

Keep modules private by default. Use pub only for deliberate boundaries such as application entrypoints, domain types needed by tests, and port traits.

### 4. Create AppContext

Create application::context::AppContext as the composition root container. It should have named fields or accessors for the future dependencies:

~~~
ConfigStore
StateStore
FileSystem
ProcessRunner
Clock
PlatformDetector
DesktopBackend
TerminalBackend
ContainerBackend
EditorBackend
EmulatorBackend
IntegrationRegistry
~~~

Use temporary placeholder types only when necessary to make the crate compile. Do not put concrete subprocess construction inside application use cases.

### 5. Create the top-level error boundary

Create a typed application error path that can later represent domain, persistence, platform, process, integration, UI, and CLI failures.

The error boundary must preserve source errors and structured context. The CLI layer will later render English messages and choose exit codes.

### 6. Create port module boundaries

Create application/ports/mod.rs and declarations for the narrow port modules listed in AGENTS.md. The traits may start with minimal signatures, but do not create a universal System trait or a catch-all Utils trait.

Each port must describe one capability:

~~~
FileSystem
ProcessRunner
Clock
ConfigStore
StateStore
PlatformDetector
DesktopBackend
TerminalBackend
ContainerBackend
EditorBackend
EmulatorBackend
~~~

### 7. Add tracing initialization

Set up a small tracing subscriber in the composition root. It must be quiet by default and compatible with the future TUI. Do not print raw tracing output over the interactive interface.

Keep user-facing messages separate from internal diagnostics.

### 8. Add initial quality configuration

Add repository configuration only when it is needed by the task:

- rustfmt configuration should stay close to Rust defaults;
- Clippy must be compatible with warnings denied;
- CI configuration may be introduced in Task 11;
- do not add a formatter or linter that duplicates Rust tooling.

## Tests

Add a minimal test that:

- constructs an AppContext with fake or placeholder dependencies;
- invokes the library entrypoint without an external side effect;
- verifies that the crate can be used as a library by integration tests.

The test must not depend on the host's desktop, Docker daemon, tmux server, Zed installation, or Android SDK.

## Acceptance criteria

- cargo check succeeds.
- cargo test --all-targets succeeds.
- cargo fmt --all -- --check succeeds.
- cargo clippy --all-targets -- -D warnings succeeds.
- src/main.rs contains no business logic.
- src/lib.rs exposes the intended crate boundary.
- AppContext is the only composition root.
- No global mutable state or singleton is introduced.
- No external process is started.
- No source comments are added unless required by an unavoidable constraint.
- No forbidden panic-based operation is added.

## Non-goals

- no real command parsing;
- no TUI screens;
- no TOML schema;
- no platform detection;
- no integration implementation;
- no legacy format support or migration.

