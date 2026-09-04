# Workstate implementation tasks

This directory contains the implementation plan for the workstate MVP. Each file is one macro-task. A macro-task may contain several commits and internal checklists, but it should produce one coherent milestone that can be reviewed and tested independently.

The tasks deliberately avoid compatibility work for earlier shell scripts. The current product starts with a new Rust configuration format under ~/.workstate.

## Recommended order

| Task | Name | Depends on |
| --- | --- | --- |
| 01 | Foundation and architecture | None |
| 02 | Domain, configuration, and persistence | 01 |
| 03 | Platform detection and capability registry | 01, 02 |
| 04 | CLI and Ratatui interface | 01, 02, 03 |
| 05 | Dependency graph planner and Tokio scheduler | 01, 02, 03, 04 |
| 06 | Reconciliation, ownership, rollback, and lifecycle | 02, 03, 05 |
| 07 | COSMIC and Zed integration | 03, 05, 06 |
| 08 | tmux and command execution | 03, 05, 06 |
| 09 | Docker integration | 03, 05, 06 |
| 10 | Android Emulator integration | 03, 05, 06, 07 |
| 11 | Hardening and release readiness | 01-10 |

Tasks 07, 08, 09, and 10 may be developed in parallel after the core engine is stable, but they should be integrated one at a time into the end-to-end flow.

## Definition of completion

Every task must:

- keep all source, UI, test, and documentation text in English;
- obey the source-comment restriction in AGENTS.md;
- avoid unwrap, expect, panic!, unreachable!, and todo! in shipped code;
- use injected ports for side effects;
- add tests at the appropriate boundary;
- pass formatting and Clippy checks;
- preserve the existing product contract;
- document any deliberate public behavior introduced by the task.

The final implementation must satisfy every acceptance criterion in these files and the normative rules in the repository root AGENTS.md.

