# Task 09: Docker Engine, Docker Desktop, and Compose Integration

## Objective

Implement Docker support for the first release, covering Docker Engine, Docker Desktop, Docker Compose, container checks, startup readiness, ownership, and safe cleanup.

An environment must be able to express that a Docker resource is required before another action runs. For example, a Compose stack can become ready before a Zed project or one-shot command is started. Direct container actions are managed by Workstate at the container level. Compose actions are different: Docker Compose is the source of truth for the project and its services, so Workstate must delegate project reconciliation to the configured Compose `up` and `down` operations while retaining project-level ownership and sharing safety.

## Dependencies

This task starts after:

- Task 03: capability detection and support registry.
- Task 05: dependency graph planning and scheduling.
- Task 06: ownership-aware lifecycle and rollback.
- Task 08: generic process execution and output events.

Task 02 supplies persistent Docker action configuration and Task 04 supplies the TUI.

## Scope

### In scope

- Detecting usable Docker Engine access.
- Starting or opening Docker Desktop when configured and supported.
- Waiting for the Docker daemon to become responsive.
- Running explicit Docker commands and Docker Compose commands.
- Starting and checking containers.
- Health, running-state, command, port, and HTTP/TCP readiness checks.
- Tracking containers, Compose projects, and Docker Desktop ownership.
- Reconciliation, rollback, stop, and delete behavior.

### Out of scope

- Managing arbitrary containers that are not referenced by an environment.
- Removing images, volumes, networks, or data unless the environment explicitly owns and requests them.
- Replacing Docker's own orchestration.
- Supporting Docker backends other than the detected supported Linux path in this task.
- Migrating configuration from the earlier shell scripts.

## Files and module responsibilities

Create or update these modules:

~~~text
src/integrations/docker/mod.rs
src/integrations/docker/backend.rs
src/integrations/docker/engine.rs
src/integrations/docker/desktop.rs
src/integrations/docker/compose.rs
src/integrations/docker/models.rs
src/integrations/docker/checks.rs
src/integrations/docker/errors.rs
src/application/ports/docker.rs
tests/fakes/fake_docker.rs
tests/fixtures/docker/
~~~

Keep engine, desktop, and Compose mechanisms behind one integration-neutral Docker port. The application layer must request capabilities such as ensure engine ready, ensure stack, inspect container, and stop owned resources; it must not construct Docker CLI arguments.

## Detailed implementation plan

### 1. Define the Docker port

1. Provide typed operations for:
   - observing engine availability and version;
   - starting or opening Docker Desktop when requested;
   - waiting for engine readiness;
   - observing containers and Compose projects;
   - ensuring a container or Compose project;
   - evaluating readiness checks;
   - stopping only owned resources.
2. Represent resource identity independently from display names:
   - container id where available;
   - observed Compose project name and working directory;
   - configured service name;
   - Docker Desktop process identity when applicable.
3. Return explicit outcomes for healthy, repaired, created, unchanged, not found, unavailable, ambiguous, and failed.
4. Make ownership part of every mutation result. The caller must not have to infer it from a name.

### 2. Detect and prepare the engine

1. Ask the capability registry whether Docker Engine access is supported and available.
2. Inspect the daemon before launching Docker Desktop or a service.
3. If the engine is already responsive, record it as pre-existing and do not claim ownership.
4. If the environment requests Docker Desktop and the supported desktop launcher is available, launch it through the process port only when needed.
5. Poll a bounded engine readiness check after launch.
6. If the engine remains unavailable, return an error containing:
   - the operation that failed;
   - the detection result;
   - the last safe diagnostic from Docker;
   - a concise next action for the user.
7. Never retry indefinitely and never launch duplicate Docker Desktop processes.
8. Keep desktop-process ownership separate from engine availability. A running engine does not prove Workstate owns the desktop process.

### 3. Implement direct container actions

1. Validate image, container name, working directory, environment values, mounts, ports, and command arguments before mutation.
2. Inspect the exact container identity before creating or starting anything.
3. Reuse a healthy matching container when its immutable configuration matches the action.
4. If a matching container exists but its configuration is incompatible, report a conflict instead of deleting or replacing it silently.
5. Create and start a container only when no safe match exists.
6. Record container id, creation ownership, requested checks, and the action id in runtime state.
7. Wait for configured checks after starting.
8. Treat a container that exits during readiness as a failure with its exit status and relevant log tail.
9. Do not remove volumes or images during ordinary stop or rollback.
10. Do not apply the scheduler's short default action timeout to container creation or startup. A missing explicit action timeout means that image pulls may run until Docker completes or cancellation is requested. Preserve bounded Docker Engine readiness and readiness-check timeouts.

### 4. Implement Docker Compose actions

1. Require an explicit Compose working directory and one compose file or command configuration.
2. Resolve paths before execution; never use the caller's PWD implicitly.
3. Use the generic command runner with a structured argv specification where possible.
4. Identify the Compose project by the observed Compose project name and resolved working directory. The project name is runtime metadata, not user-editable configuration.
5. Ensure Docker Engine readiness before invoking Compose.
6. Invoke the configured up operation on every run. Docker Compose decides whether services must be created, started, recreated, or left unchanged.
7. Use a post-up project observation only to verify that the project became available and to evaluate configured readiness checks.
8. Record the project identity and project-level ownership. Do not require individual service-container identities for normal Compose cleanup.
9. On stop, invoke the configured safe down operation once for an owned project. Do not compare container IDs or remove Compose services individually.
10. Avoid destructive flags by default. Data removal must never occur unless a future explicit product capability enables it.
11. If the project is pre-existing, shared, or its ownership is ambiguous, preserve it and do not invoke down.
12. Do not apply the scheduler's short default action timeout to Compose startup. A missing explicit action timeout means that image pulls may run until Docker Compose completes or cancellation is requested. Preserve bounded Docker Engine readiness and readiness-check timeouts.

### 5. Implement readiness checks

Support the checks defined by the domain model:

1. Container running state.
2. Container health state.
3. Expected command or process state.
4. TCP port connectivity.
5. HTTP response status and optional response condition.
6. A bounded command check executed through the process port.
7. Optional fixed delay only as an explicit fallback, never as a replacement for a check that can observe readiness.

For every check:

- apply an explicit timeout and polling interval;
- emit progress without flooding the TUI;
- include the last observed value in errors;
- cancel cleanly during rollback;
- avoid logging credentials, tokens, or full secret-bearing environment values.

### 6. Integrate with graph execution

1. Mark engine readiness as a prerequisite for container and Compose actions.
2. Allow independent Docker actions to run concurrently when their resource identities do not conflict.
3. Serialize operations that can mutate the same engine resource or Compose project.
4. Do not let a Docker check mutate a resource.
5. Make a successful ensure operation idempotent and safe to rerun.
6. Publish structured action events rather than printing directly.
7. Return a readiness result that downstream actions can consume without re-running the same expensive inspection unnecessarily during one execution.

### 7. Implement rollback and lifecycle cleanup

1. On a later setup failure, stop direct containers created by the current run and invoke the configured Compose cleanup operation for an owned project in reverse dependency order.
2. Preserve pre-existing containers, projects, engine state, and Docker Desktop.
3. If Workstate started Docker Desktop and no other active environment requires it, stop it only when the configured lifecycle policy permits.
4. If another active environment uses the same Docker resource, leave it running.
5. Persist cleanup failures so stop can retry them.
6. Make stop safe when containers or services were already manually stopped.
7. Make delete call the same ownership-aware stop path before deleting only the selected environment's files.

## Required tests

Add unit and integration tests with a fake Docker port:

1. An already-ready engine is reused without ownership.
2. An unavailable engine triggers the configured desktop launch once.
3. Engine readiness timeout returns a typed diagnostic.
4. A matching healthy container is reused.
5. A configuration conflict is reported without deletion.
6. A newly created container is owned and removed or stopped according to policy.
7. A pre-existing container survives stop and rollback.
8. Compose project identity includes the resolved working directory.
9. A healthy Compose project still receives the configured up operation, and Docker Compose remains responsible for deciding whether anything changes.
10. A Compose failure rolls back through the configured Compose cleanup operation while preserving pre-existing or shared projects.
11. Each readiness check succeeds, times out, and cancels deterministically.
12. HTTP and TCP checks use bounded timeouts.
13. Shared Docker resources are not stopped by one environment.
14. Secrets are absent from emitted logs and error strings.
15. No default test requires a running Docker daemon.
16. Direct container creation and Compose startup are not interrupted by the scheduler's default action timeout, while an explicitly configured action timeout remains enforced.

## Acceptance criteria

- Docker Engine, Docker Desktop, direct containers, and Compose are available through the same application-facing abstraction.
- The environment can wait for Docker readiness before opening dependent tools.
- Existing healthy direct containers are reused without blind restart; Compose projects delegate reconciliation to Docker Compose.
- Docker failures stop the setup transaction and participate in reverse-order rollback.
- Stop and delete are ownership-aware and do not remove unrelated data.
- All paths and commands are explicit and independent from PWD.
- All user-facing text is English.
- The implementation passes fake-backend tests, lifecycle tests, error tests, fmt, and clippy with warnings denied.

## Non-goals

- Do not implement migration from old Docker state or config locations.
- Do not use broad container-name prefix matching.
- Do not issue destructive volume or image cleanup by default.
- Do not hide daemon or container failures behind generic messages.
