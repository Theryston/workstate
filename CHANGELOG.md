# Changelog

All notable changes to Workstate are documented in this file.

## [Unreleased]

## [0.1.1](https://github.com/Theryston/workstate/compare/v0.1.0...v0.1.1) - 2026-09-05

### Added

- add Unix bootstrap installer script

### Other

- Update README.md
- add MIT license reference
- Add MIT License to the project
- replace implementation task plan with public README
- Update crate description in Cargo.toml

## [0.1.0](https://github.com/Theryston/workstate/releases/tag/v0.1.0) - 2026-09-05

### Added

- add release automation workflow and error card rendering

### Fixed

- *(editor)* Require single Compose file and add file catalog
- stop moving reused Zed windows between workspaces
- serialize concurrent Zed project launches to correlate windows

### Other

- Run Docker actions through shared environment-aware preflight
- Update action palette ordering and labels
- Remove mutable backend matching in project editor handlers
- Add VS Code and Cursor project editor support
- Implement generic application action handler
- Remove deprecated wait, verify, and custom action kinds
- Environment ready
- Implement Android emulator lifecycle and image pipeline
- Remove mouse capture from terminal session
- Add duplicate command save-time warning
- Add shared 180s timeout and refine Compose lifecycle
- Add FileCatalog port for Compose YAML discovery
- Implement full Docker container and Compose integration
- Remove start_service action kind
- Implement command actions with tmux background support
- ```
- Move text cursor natively through editor input fields
- Implement directory autocomplete port and local catalog
- Add ApplicationCatalog port for application selection
- Resolve workspace targets once per run with reservations
- Implement shared lifecycle progress view for stop and run commands
- Update editor legend and scope mutations to actions panel
- Document optional environment arguments for CLI commands
- Allow subcommands without an environment argument
- Update the editor footer to display a compact, context-sensitive control
- Move validation errors from inspector to editor footer
- Split add command into new and edit commands
- Refactor Zed backend and editor layout
- Add observe_for_cleanup hook to action handlers
- Add COSMIC and Zed integration backends
- Add lifecycle engine for run, stop, and delete use cases
- Add planner and scheduler with action handling
- Add CLI command parsing and TUI lifecycle flows
- Add platform detection and capability registry
- Add typed domain model and TOML persistence
- Add project scaffolding and dependency foundation
- Add engineering plan for Rust workstate MVP
- Add engineering guide for workstate project
- first commit
