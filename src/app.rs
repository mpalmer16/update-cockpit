use std::io::IsTerminal;

use anyhow::{Result, bail};

use crate::catalog::Catalog;
use crate::cli::{Cli, Command};
use crate::runner::{RunOptions, Runner};
use crate::tui;
use crate::workspace::discover_root;

pub fn run() -> Result<()> {
    let cli = Cli::parse_args();
    let root = discover_root(cli.root.as_deref())?;
    let catalog = Catalog::load_from_root(&root)?;
    let command = resolve_command(cli.command, std::io::stdin().is_terminal());

    match command {
        Command::List => {
            for task in catalog.tasks() {
                let suffix = if task.dangerous { " [dangerous]" } else { "" };
                println!("{} - {}{}", task.id, task.label, suffix);
            }
        }
        Command::Plan { all, tasks } => {
            let plan = catalog.plan(all, &tasks)?;
            for task in &plan.tasks {
                println!("{} - {}", task.id, task.label);
            }
        }
        Command::Run { all, tasks } => {
            let plan = catalog.plan(all, &tasks)?;
            let options = RunOptions {
                dry_run: cli.dry_run,
                verbose: cli.verbose,
                brew_cleanup: cli.brew_cleanup,
                npm_audit: cli.npm_audit,
            };
            let summary = Runner::new(root).run(&plan, &options)?;

            println!();
            println!(
                "Summary: {} OK, {} WARN, {} FAIL",
                summary.ok_count, summary.warn_count, summary.fail_count
            );
            for outcome in &summary.outcomes {
                println!("- {}: {}", outcome.label, outcome.status.label());
            }

            if summary.fail_count != 0 {
                bail!("one or more tasks failed");
            }
        }
        Command::Tui => {
            let options = RunOptions {
                dry_run: cli.dry_run,
                verbose: cli.verbose,
                brew_cleanup: cli.brew_cleanup,
                npm_audit: cli.npm_audit,
            };
            tui::run(root, catalog, options)?;
        }
    }

    Ok(())
}

fn resolve_command(command: Option<Command>, interactive_terminal: bool) -> Command {
    command.unwrap_or_else(|| {
        if interactive_terminal {
            Command::Tui
        } else {
            Command::Run {
                all: false,
                tasks: Vec::new(),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use crate::cli::Command;

    use super::resolve_command;

    #[test]
    fn defaults_to_tui_for_interactive_terminals() {
        assert!(matches!(resolve_command(None, true), Command::Tui));
    }

    #[test]
    fn defaults_to_cli_run_for_non_interactive_terminals() {
        assert!(matches!(
            resolve_command(None, false),
            Command::Run { all: false, tasks } if tasks.is_empty()
        ));
    }
}
