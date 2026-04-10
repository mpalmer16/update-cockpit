use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "upgrade-cockpit")]
#[command(about = "A terminal-first control panel for running and managing system update tasks.")]
pub struct Cli {
    #[arg(long, global = true)]
    pub root: Option<PathBuf>,

    #[arg(long, global = true)]
    pub dry_run: bool,

    #[arg(long, global = true)]
    pub verbose: bool,

    #[arg(long, global = true)]
    pub brew_cleanup: bool,

    #[arg(long, global = true)]
    pub npm_audit: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

impl Cli {
    pub fn parse_args() -> Self {
        Self::parse()
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    List,
    Plan {
        #[arg(long)]
        all: bool,
        #[arg(value_name = "TASK")]
        tasks: Vec<String>,
    },
    Run {
        #[arg(long)]
        all: bool,
        #[arg(value_name = "TASK")]
        tasks: Vec<String>,
    },
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Cli, Command};

    #[test]
    fn defaults_to_no_explicit_subcommand() {
        let cli = Cli::try_parse_from(["upgrade-cockpit"]).expect("parse cli");
        assert!(cli.command.is_none());
        assert!(!cli.dry_run);
    }

    #[test]
    fn parses_run_with_global_flags() {
        let cli = Cli::try_parse_from([
            "upgrade-cockpit",
            "--dry-run",
            "--brew-cleanup",
            "run",
            "brew",
        ])
        .expect("parse cli");

        assert!(cli.dry_run);
        assert!(cli.brew_cleanup);
        match cli.command.expect("command") {
            Command::Run { all, tasks } => {
                assert!(!all);
                assert_eq!(tasks, vec!["brew".to_string()]);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }
}
