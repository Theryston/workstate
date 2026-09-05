# Task 04 — CLI and Ratatui interface

## Objective

Implement the user-facing command contract and the full-screen terminal interface. The result must let a programmer select, create, and edit environments without knowing the internal Rust module layout or having to use the current shell directory.

## Scope

This task implements:

- Clap command parsing;
- positional environment execution;
- the no-argument environment selector;
- new-create and edit behavior;
- the dynamic environment editor;
- workspace target configuration;
- action configuration;
- dependency editing;
- path selection and validation feedback;
- delete confirmation;
- terminal restoration;
- English human-readable output;
- TUI snapshots.

The interface may use fake application services while the execution engine and integrations are being completed.

## Dependencies

Complete Tasks 01, 02, and 03 first. The UI must consume domain and application types rather than inventing a second configuration model.

## Files to create or change

Create or update:

~~~
src/cli/mod.rs
src/cli/args.rs
src/cli/command.rs
src/cli/output.rs
src/ui/mod.rs
src/ui/app.rs
src/ui/event.rs
src/ui/state.rs
src/ui/theme.rs
src/ui/editor.rs
src/ui/progress.rs
src/ui/widgets/mod.rs
~~~

Add or update snapshot files in the location selected by the test setup.

## Implementation plan

### 1. Model the command grammar

The top-level parser must support:

~~~
workstate
workstate <environment>
workstate run [environment]
workstate start [environment]
workstate new [environment]
workstate edit [environment]
workstate stop [environment]
workstate delete [environment]
~~~

Reserve known subcommand names so a name such as new is not interpreted as an environment. Preserve the positional environment invocation as the primary start operation. Accept `run` and `start` as hidden aliases that normalize to the positional flow, but do not expose either alias in root help or document them as public subcommands.

When `edit`, `stop`, or `delete` omit the environment argument, resolve it through the shared saved-environment selector. When `new` omits the argument, resolve it through the shared validated text field. Resolve and validate the target before opening the editor or beginning the requested lifecycle operation.

Support the agreed global flags when they are implemented:

~~~
--yes
--dry-run
--json
--quiet
--verbose
--no-color
--config <path>
~~~

Keep parsing separate from command execution. A parsed command must be a typed value that can be passed to an application use case.

### 2. Route every command through compatibility preflight

The CLI runner must call the shared compatibility gate before opening an interactive editor or starting a lifecycle operation.

On a compatible host, do not print a detection banner.

On an unsupported host, render the structured error in English and exit with code 1. Do not create a Workstate directory, modify a state file, or start an external process before the gate succeeds.

### 3. Build the terminal lifecycle guard

Create a guard that:

- switches the terminal into the mode required by Ratatui;
- restores the previous terminal mode on normal completion;
- restores the terminal on user cancellation;
- restores the terminal when an application error occurs;
- restores the terminal when a Tokio task returns an error;
- does not leave an alternate screen, raw mode, cursor state, or mouse mode active.

The guard must be owned by the TUI runner, not by individual widgets.

### 4. Build the environment selector

The no-argument command must display saved environments with:

- display name;
- slug or another non-confusing identifier;
- current state such as ready, partial, stopped, or unknown;
- selection highlight;
- keyboard navigation;
- an empty-state message that points to workstate new <environment>.

Selecting an environment must invoke the same application execution use case as workstate <environment>.

Do not make the selector depend on a numeric prompt. A numeric index may be shown as secondary information, but keyboard navigation is the primary interaction.

### 5. Build the shared new/edit editor

The editor must support both creation and editing:

- load an existing environment when the directory exists;
- start from a valid empty configuration when it does not;
- preserve unsaved edits while navigating;
- show the environment identity;
- show an action palette;
- show the current action graph;
- show a property inspector;
- show validation errors only after an explicit save attempt fails;
- render validation errors in the footer instead of the contextual inspector;
- identify action-related validation errors with the action's current display name;
- revalidate a displayed invalid field after it changes and remove its error immediately when fixed;
- show a context-sensitive footer legend for the focused action list, inspector, and modal editors;
- show one canonical shortcut per operation, with visually distinct key tokens and operation labels;
- keep action-list mutations (`a` and `d`) unavailable while the inspector is focused;
- show a review and save screen.

The user must be able to add actions in any order and attach dependencies after creating them. Do not implement a fixed sequence of technology-specific questions.

The action palette should use capability-oriented labels such as:

~~~
Open application
Open Project with Zed
Run command
Start service
Configure tiling
Start Docker container
Start Docker Compose stack
Start Android Emulator
Wait for condition
Verify resource
Custom action
~~~

Backend-specific fields may appear after the user selects a relevant action. For example, tmux details may be shown for a persistent terminal action, and Zed details may be shown for an open-project action.

### 6. Implement action forms

Each form must expose only fields relevant to the selected action while preserving the common fields:

~~~
Action ID
Display label
Working directory
Desktop workspace
Execution mode
Dependencies
Readiness checks
Timeout
Retry policy
Cleanup policy
~~~

Path fields must allow the user to enter or select a directory. Preserve forms such as ~/Projects/app and $HOME/Projects/app. Validate the resolved path without rewriting the saved representation.

Do not use the process current directory as an implicit value.

### 7. Implement workspace configuration

The editor must expose:

~~~
Current workspace
Specific existing workspace
Next empty workspace
Create a named workspace
No workspace movement
~~~

Each configured workspace must expose a simple tiling enabled or disabled setting. Do not add manual pixel placement to the MVP.

### 8. Implement dependency editing

The editor must allow the user to select existing action IDs as dependencies. It must:

- prevent an action from depending on itself;
- show the dependency path clearly;
- show missing references immediately;
- show cycles before save;
- retain dependency order deterministically;
- allow removing a dependency without deleting the action.

The editor must not permit saving an invalid graph.

### 9. Implement save and cancel behavior

The save flow must:

1. run complete domain validation;
2. show unresolved errors;
3. show a review of the actions and workspaces;
4. require explicit save confirmation when creating or changing an environment;
5. write through the injected configuration store;
6. leave the previous file untouched if saving fails;
7. return to the terminal cleanly.

Canceling must be a successful no-op and must not change environment.toml.

### 10. Implement delete confirmation

The delete flow must show:

- the environment name;
- the environment directory;
- whether it is active;
- the fact that active resources will be stopped first;
- the fact that the environment directory will be removed;
- a confirmation action.

The --yes flag may skip only the confirmation. The UI must still perform compatibility checks, ownership checks, and cleanup.

### 11. Implement execution progress rendering

Create one reusable lifecycle progress view for both `run` and `stop`. It must consume application events through a bounded channel rather than polling integration internals directly.

Render one row for every configured action before the first action starts, preserving configuration order. Each row must expose the action's display label and a visual state marker:

- a pending marker while it is waiting for dependencies or scheduler capacity;
- an animated spinner while it is running;
- a success check when it reaches `ready` during `run` or `stopped` during `stop`;
- distinct markers for skipped, failed, cancelled, and rolling-back states.

The view must keep updating its elapsed time and spinner on a periodic tick even when external integrations do not emit an event. It must visibly support concurrent branches by allowing more than one action row to be running at once. Use a starting/stopping operation title and an activity panel for action output and ownership-preserving cleanup messages.

Render:

- pending actions;
- running actions;
- ready actions;
- skipped dependent actions;
- failed actions;
- rollback actions;
- logs;
- elapsed time;
- timeouts;
- final summary;
- stop cleanup progress with the same component and event mapping.

The progress view must close only after run setup succeeds, rollback finishes, or stop cleanup completes or fails. `--quiet` and `--json` must bypass interactive progress so their output remains script-safe.

### 12. Define keyboard behavior

Use stable, discoverable navigation. At minimum, support:

~~~
Up and Down      move the selection in the focused pane
Right            enter the inspector from the action list
Left             return to the action list from the inspector
Enter            inspect, edit, or confirm according to context
Esc              go back, cancel, or exit according to context
Tab              move between panels
a                add an action from the action list
d                delete the selected action from the action list
s                save
q                exit when safe
~~~

If a key has a destructive meaning, the UI must show the current action and require confirmation where appropriate.

## Tests

Add:

- Clap parsing tests for every public command;
- tests for positional environment disambiguation;
- selector tests with zero, one, and many environments;
- new-create tests;
- edit tests;
- omitted-environment selector tests for edit, stop, and delete;
- omitted-environment text-field tests for new;
- cancel-preserves-file tests;
- path input tests;
- workspace target form tests;
- dependency validation tests;
- delete confirmation tests;
- --yes behavior tests;
- TUI snapshots for the states listed in AGENTS.md;
- terminal restoration tests using a testable terminal abstraction.

All tests must use fake application services and temporary storage.

## Acceptance criteria

- The documented command grammar parses correctly.
- workstate without arguments opens a selector.
- workstate <environment> runs the selected environment use case.
- workstate new [environment] creates only through the shared dynamic editor, using the reusable name field when the argument is omitted.
- workstate edit [environment] edits only through the same shared dynamic editor, using the reusable environment selector when the argument is omitted.
- workstate stop [environment] and workstate delete [environment] use the same reusable environment selector when the argument is omitted.
- workstate <environment> and workstate stop [environment] display the reusable lifecycle progress view with action rows, pending markers, animated running spinners, completion checks, failure states, and live activity.
- Every project and command can receive an explicit working directory.
- Every visual action can receive a desktop workspace target.
- Workspace tiling is configured as a boolean desired state.
- Invalid graphs cannot be saved.
- Delete confirms by default and --yes bypasses only confirmation.
- Supported systems do not show an unnecessary detection banner.
- Unsupported systems fail before the TUI mutates anything.
- The terminal is restored on every exit path.
- All UI text and snapshots are English.

## Non-goals

- no real COSMIC calls;
- no real tmux sessions;
- no Docker or Android startup;
- no final reconciliation behavior;
- no support for previous configuration formats.
