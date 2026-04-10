# upgrade-cockpit

A terminal-first control panel for running and managing system update tasks.

## Current Status

The project now has a working runner plus an initial interactive TUI shell:

- task manifests in `tasks/`
- shell-based task implementations in `scripts/`
- a Rust runner with dependency-aware planning
- a ratatui-based control panel with selection, confirmations, live logs, and summaries
- unit coverage for CLI parsing, manifest validation, planning, execution status handling, and TUI state transitions

## Commands

```bash
cargo run
cargo run -- tui
cargo run -- list
cargo run -- plan npm-tools
cargo run -- --dry-run run
```

## Next Step

Deepen the TUI with richer profiles, saved preferences, and better history.
