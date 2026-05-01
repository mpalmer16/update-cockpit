# upgrade-cockpit

A Rust-powered terminal cockpit for running scripted system updates without turning your shell history into a maintenance log.

## Overview

`upgrade-cockpit` keeps the update logic simple and the orchestration interesting:

- update tasks are defined as manifests in `tasks/`
- each task runs a script or command from `scripts/`
- Rust handles planning, dependency ordering, profiles, preflight checks, summaries, history, and the terminal UI
- the default interactive experience is a ratatui control panel with grouped tasks, recent runs, per-task status, and task safety notes
- task runs temporarily reclaim the terminal so interactive tools can prompt directly before returning to the summary screen

## Project Shape

- `src/runner.rs` executes tasks and classifies `OK`, `WARN`, and `FAIL` outcomes
- `src/profiles.rs` defines the built-in maintenance profiles
- `src/persistence.rs` stores saved defaults and recent run history
- `src/catalog.rs` loads task metadata, tags, notes, and preflight requirements from manifests
- `src/tui/` contains the interactive shell and state model
- `tasks/` declares what exists and how tasks depend on each other
- `scripts/` preserves the shell-native behavior for tools like `nvm`, `sdkman`, `brew`, and `flutter`

## Commands

```bash
cargo run
cargo run -- tui
cargo run -- list
cargo run -- plan npm-tools
cargo run -- --dry-run run
cargo test
```

## TUI Controls

- `j` / `k` or arrow keys move through tasks
- `space` toggles a task
- `a` selects all tasks
- `x` clears the selection
- `p` cycles to the next profile
- `Shift+P` cycles to the previous profile
- `f` cycles task filter scope
- `g` toggles a category filter from the selected task
- `t` toggles a tag filter from the selected task
- `z` clears active task filters
- `d`, `v`, `c`, `u` toggle runtime options
- `enter` starts a run
- `r` reruns failed tasks from the summary screen
- `l` reruns the last profile from the summary screen
- `q` quits

## State

The TUI saves its current profile, custom selection, flags, and recent run history to `.upgrade-cockpit/state.toml` in the project root.

## Terminal Behavior

During a run, `upgrade-cockpit` temporarily leaves the TUI and gives the terminal back to the task process. That keeps interactive tools like SDKMAN, Homebrew, and language managers usable when they need to ask a question.

When the run finishes, the TUI returns to a summary screen. The Activity panel records cockpit-level events such as task starts, task finishes, preflight warnings, and fatal errors. Full stdout/stderr is shown directly in the terminal during the run instead of being captured into the TUI.

## Task Metadata

Tasks can declare categories, tags, notes, danger messages, and preflight requirements directly in their `tasks/*.toml` manifests. The runner uses that metadata to warn or fail early when required tools are missing, and the TUI surfaces the same information before you launch a task.

Example:

```toml
id = "rust"
label = "Rust"
description = "Update Rust toolchains with rustup."
category = "toolchain"
tags = ["rust", "runtime"]
default_selected = true

[preflight]
requires_commands = ["rustup"]
on_missing = "warn"

[runner]
kind = "script"
path = "scripts/rust.zsh"
```

## Testing

The test suite focuses on the stable contracts that make the cockpit safe to evolve:

- catalog loading, manifest validation, dependency ordering, and cycle detection
- runner outcome classification, preflight behavior, event emission, and interactive task execution
- persisted state, profile selection, run history, and history trimming
- TUI state transitions for selection, filters, dangerous confirmations, summaries, and rerun flows

Run the suite with:

```bash
cargo test
```
