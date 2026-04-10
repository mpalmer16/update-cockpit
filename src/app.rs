use anyhow::{Result, bail};

use crate::catalog::Catalog;
use crate::cli::{Cli, Command};
use crate::runner::{RunOptions, Runner};
use crate::workspace::discover_root;

pub fn run() -> Result<()> {
    let cli = Cli::parse_args();
    let root = discover_root(cli.root.as_deref())?;
    let catalog = Catalog::load_from_root(&root)?;

    match cli.command.unwrap_or(Command::Run {
        all: false,
        tasks: Vec::new(),
    }) {
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
    }

    Ok(())
}
