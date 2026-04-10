# upgrade-cockpit

A terminal-first control panel for running and managing system update tasks.

## Current Status

The first implementation slice focuses on the non-TUI execution core:

- task manifests in `tasks/`
- shell-based task implementations in `scripts/`
- a Rust runner with dependency-aware planning
- unit coverage for CLI parsing, manifest validation, planning, and execution status handling

## Commands

```bash
cargo run -- list
cargo run -- plan npm-tools
cargo run -- --dry-run run
```

## Next Step

Build the TUI on top of the existing runner instead of mixing terminal rendering into the execution layer.
