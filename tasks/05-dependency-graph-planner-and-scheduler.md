# Task 05 — Dependency graph planner and Tokio scheduler

## Objective

Implement the application engine that turns a validated environment graph into an observable execution plan and schedules independent actions concurrently with Tokio. The engine must be deterministic, cancelable, and independent of concrete integrations.

## Scope

This task implements:

- execution plan construction;
- action handler contracts;
- observation and readiness abstractions;
- dependency scheduling;
- concurrent execution;
- event streaming;
- timeout handling;
- cancellation;
- run-once and background handoff semantics;
- dry-run planning.

Do not implement concrete COSMIC, tmux, Docker, Zed, or Android commands in this task.

## Dependencies

Complete Tasks 01-04. The scheduler consumes the domain model and emits events that the TUI can render.

## Files to create or change

Create or update:

~~~
src/application/planner/mod.rs
src/application/planner/plan.rs
src/application/reconciliation/mod.rs
src/application/reconciliation/scheduler.rs
src/application/ports/process.rs
src/application/ports/persistence.rs
src/application/ports/clock.rs
src/domain/graph.rs
src/domain/action.rs
src/application/context.rs
~~~

## Implementation plan

### 1. Define the action handler contract

Create a narrow handler contract that lets the engine work with any integration. A handler must be able to:

- identify its supported action kind;
- validate action-specific configuration;
- observe current state;
- determine whether desired state is already satisfied;
- apply the required change;
- wait for readiness checks;
- expose an ownership/resource record;
- provide a compensation operation;
- provide stop behavior.

Keep the handler contract free of TUI types and concrete process APIs.

The contract may use async methods because Tokio is the application runtime. Preserve structured errors and action IDs in every failure.

### 2. Define plan entries

Create a plan entry containing:

~~~
action_id
action_kind
dependencies
execution_mode
working_directory
desktop_workspace
required_capabilities
observation strategy
apply strategy
readiness checks
compensation strategy
cleanup policy
~~~

The plan is derived from desired configuration and available handlers. It must not mutate the computer.

### 3. Validate and topologically order the graph

Use the pure domain graph validation from Task 02. Build a deterministic topological order for diagnostics and a dependency-ready representation for concurrent execution.

For actions that become ready at the same time, use a deterministic tie-breaker such as configuration order followed by action ID. Do not rely on hash-map iteration order.

### 4. Implement observation and planning

Before applying an action, ask its handler for an observation. The planner must classify the action as one of:

~~~
already_correct
requires_change
blocked_by_missing_capability
invalid
unknown
~~~

An already-correct action must not be restarted or recreated. The plan must retain enough information to display that fact in the TUI.

Observation may be performed concurrently for independent actions when it is safe. Do not perform a mutation during planning.

### 5. Implement dependency scheduling

Create a scheduler that:

1. tracks pending, running, ready, failed, and skipped actions;
2. starts only actions whose dependencies succeeded;
3. starts independent actions concurrently through Tokio;
4. limits task creation to the number of runnable actions;
5. emits an event for every state transition;
6. stops scheduling new work after a required failure;
7. coordinates rollback through the lifecycle engine in Task 06.

Use Tokio task coordination such as JoinSet or an equivalent explicit mechanism. Do not create detached tasks that outlive the application context without being represented in runtime state.

### 6. Implement execution modes

For run_once actions:

- run during setup;
- stream output events;
- wait for completion;
- run readiness checks;
- return success or a structured failure;
- do not create tmux resources merely for logging.

For background actions:

- validate the command and working directory during setup;
- delegate process persistence to the terminal backend;
- wait only until the background resource is successfully created and identified;
- record the resource identity;
- allow the main process to exit after the graph is complete.

The scheduler must not assume that every long-lived action is a foreground Tokio task.

### 7. Implement readiness checks

Create a check runner abstraction. It must support:

~~~
none
tcp
http
command
delay
container
compose
~~~

Each check must:

- have an explicit timeout;
- emit progress;
- return a typed result;
- respect cancellation;
- avoid blocking unrelated action branches;
- include the action ID in errors.

### 8. Implement event streaming

Define application events independent of the TUI:

~~~
RunStarted
PlanBuilt
ActionObserved
ActionStarted
ActionOutput
ActionReady
ActionSkipped
ActionFailed
ActionCancelled
RollbackStarted
RollbackActionStarted
RollbackActionCompleted
RunCompleted
RunFailed
~~~

Events must contain enough context for both human rendering and future machine output. Do not put terminal escape sequences in application events.

Use a bounded channel where practical. If the UI cannot consume events fast enough, preserve critical lifecycle events and apply a deliberate output policy rather than allowing unbounded memory growth.

### 9. Implement timeout and cancellation

Wrap external waits and action lifetimes with Tokio timeouts, using the shared `180-second` external-operation default unless the action or integration defines a more specific policy. On timeout:

- produce an action-specific error;
- stop dependent actions from starting;
- emit the timeout event;
- allow the lifecycle engine to compensate completed mutations.

Handle user cancellation and process termination signals through an explicit cancellation path. Cancellation is not permission to skip rollback.

### 10. Implement dry-run planning

When --dry-run is present:

- validate compatibility;
- load and validate configuration;
- observe where observation is read-only;
- build the plan;
- display actions, dependencies, and expected mutations;
- do not start, stop, close, move, or delete resources;
- do not create a runtime ownership state that claims a mutation occurred.

## Tests

Add tests for:

- deterministic topological ordering;
- already-correct actions;
- missing dependencies;
- independent actions running concurrently;
- dependent actions waiting;
- action failure blocking dependents;
- output event ordering;
- readiness timeout;
- cancellation;
- run-once behavior;
- background handoff behavior with a fake terminal backend;
- dry-run producing no mutations;
- bounded event behavior;
- handler lookup through the registry.

Use fake handlers with controllable delays and outcomes. Verify concurrency through recording timestamps or a deterministic test clock rather than sleeps wherever possible.

## Acceptance criteria

- A valid graph produces a deterministic plan.
- Independent actions can run concurrently.
- Dependent actions never run before successful dependencies.
- Already-correct actions are not restarted.
- Every state transition produces an application event.
- Timeouts and cancellation return typed errors.
- No task is silently detached.
- Run-once and background actions have distinct behavior.
- Dry-run never mutates the machine or ownership state.
- The scheduler contains no integration-specific command logic.

## Non-goals

- no concrete external process execution;
- no ownership rollback implementation;
- no live desktop or Docker operations;
- no legacy support.
