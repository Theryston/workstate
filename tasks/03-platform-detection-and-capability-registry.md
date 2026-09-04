# Task 03 — Platform detection and capability registry

## Objective

Implement automatic runtime detection and the compatibility gate for the initial supported profile: Linux + Pop!_OS + COSMIC + tmux. Create the capability registry and support descriptors so future backends can be added without changing the reconciliation engine.

## Scope

This task implements:

- typed platform identity;
- Linux and Pop!_OS detection;
- COSMIC detection;
- tmux capability detection;
- support profiles;
- capability descriptors;
- registry construction;
- the preflight compatibility gate;
- unsupported-platform diagnostics.

The detector must be silent on compatible systems and must reject every Workstate command on unsupported systems before mutations.

## Files to create or change

Create or update:

~~~
src/application/ports/platform.rs
src/application/ports/desktop.rs
src/platform/detection/mod.rs
src/platform/detection/detector.rs
src/platform/detection/support.rs
src/platform/linux/mod.rs
src/platform/linux/detector.rs
src/platform/desktop/mod.rs
src/platform/desktop/cosmic.rs
src/integrations/registry.rs
src/integrations/mod.rs
src/application/context.rs
~~~

## Implementation plan

### 1. Define typed detection results

Create typed values for:

~~~
OperatingSystem
Distribution
DesktopEnvironment
TerminalCapability
DetectedPlatform
~~~

Avoid passing raw strings from detection into application logic. Preserve unknown values as an explicit Unknown variant with the original safe display value where useful.

### 2. Implement Linux detection

Implement a Linux detector behind PlatformDetector.

Use stable, read-only sources such as:

- the operating system reported by the runtime;
- /etc/os-release for distribution identity;
- desktop-session environment variables;
- executable availability checks.

Do not require the current working directory. Do not mutate files or start services during detection.

### 3. Detect Pop!_OS

Parse distribution metadata into a typed result. Match Pop!_OS deliberately rather than treating every Ubuntu-derived distribution as Pop!_OS.

Handle:

- missing /etc/os-release;
- malformed fields;
- unknown distributions;
- case normalization;
- missing version data.

Detection failures must become typed diagnostics, not panics.

### 4. Detect COSMIC

Detect COSMIC using the strongest available read-only signals. Keep the detector independent from the later COSMIC window/workspace adapter.

The detector may verify the presence of the required COSMIC control capability, but it must not change workspace state or issue a mutating desktop command.

### 5. Detect tmux capability

Detect whether tmux is available and usable for the initial support profile. Do not create a session during detection.

Keep tool availability separate from the base platform identity. A compatible host may still fail an environment-specific preflight when that environment requires a missing tool.

### 6. Define support profiles

Create a support profile model containing:

~~~
operating system predicate
distribution predicate
desktop predicate
required base capabilities
human-readable description
~~~

Register the initial profile:

~~~
Linux + Pop!_OS + COSMIC + tmux
~~~

The registry must be additive. A future Ubuntu + GNOME profile should be another descriptor, not a rewrite of the initial profile.

### 7. Define capability descriptors

Create typed capability identifiers for:

~~~
desktop_workspaces
desktop_windows
desktop_tiling
terminal_sessions
background_processes
docker_engine
docker_desktop
docker_compose
zed
android_emulator
adb
~~~

An environment should request capabilities based on its actions. Do not require Docker or Android when the environment does not use them.

### 8. Implement the registry

Create IntegrationRegistry as an explicit dependency owned by AppContext.

The registry must expose:

- registered support profiles;
- available capability descriptors;
- action-handler lookup;
- backend selection after compatibility validation.

Do not use hidden global registration. Do not place all platform behavior in one large match.

### 9. Implement the compatibility gate

Every public command must pass through one application-level preflight function before it:

- opens the TUI;
- loads a mutable execution state;
- starts a process;
- changes a desktop setting;
- creates a configuration directory;
- changes an environment resource.

On a compatible system, return success without printing detection information.

On an unsupported system, return a typed error containing:

- detected operating system;
- detected distribution;
- detected desktop environment;
- relevant terminal capability;
- supported profile descriptions.

The CLI will later render the error in English.

### 10. Make detection testable

The detector must accept injected read-only dependencies for filesystem metadata and executable availability. Tests must be able to simulate Linux, Pop!_OS, COSMIC, tmux, Ubuntu, GNOME, missing metadata, and missing tools without changing the host.

## Tests

Add tests for:

- a supported Pop!_OS + COSMIC + tmux result;
- Ubuntu + GNOME being unsupported;
- missing distribution metadata;
- unknown desktop environment;
- missing tmux;
- case normalization;
- compatible detection producing no user-facing output;
- unsupported detection producing structured diagnostics;
- capability-specific missing-tool errors;
- registry lookup and additive registration;
- compatibility gating before mutation.

Use fake detector dependencies and recording output sinks.

## Acceptance criteria

- Supported detection is silent.
- Unsupported detection is actionable and English at the rendering boundary.
- All commands use the same compatibility gate.
- No unsupported command can mutate configuration or runtime state.
- The initial support profile is represented by data, not scattered conditionals.
- New profiles can be added without changing the reconciliation engine.
- Detection never starts or stops a process.
- Detection never changes desktop state.
- No global registry or global mutable state exists.

## Non-goals

- no COSMIC window manipulation;
- no tmux session creation;
- no Docker, Zed, or Android execution;
- no user-facing TUI;
- no compile-time platform feature selection;
- no legacy support.

