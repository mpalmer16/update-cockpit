use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};

use crate::catalog::{ExecutionPlan, TaskDefinition, TaskRunner};

const WARN_EXIT_CODE: i32 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunOptions {
    pub dry_run: bool,
    pub verbose: bool,
    pub brew_cleanup: bool,
    pub npm_audit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSummary {
    pub outcomes: Vec<TaskOutcome>,
    pub ok_count: usize,
    pub warn_count: usize,
    pub fail_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskOutcome {
    pub id: String,
    pub label: String,
    pub status: OutcomeStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutcomeStatus {
    Ok,
    Warn,
    Fail,
}

impl OutcomeStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Warn => "WARN",
            Self::Fail => "FAIL",
        }
    }
}

pub struct Runner {
    root: PathBuf,
}

impl Runner {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn run(&self, plan: &ExecutionPlan, options: &RunOptions) -> Result<RunSummary> {
        let mut outcomes = Vec::new();
        let mut ok_count = 0;
        let mut warn_count = 0;
        let mut fail_count = 0;

        for task in &plan.tasks {
            println!();
            println!("=== {} ===", task.label);

            let status = self.run_task(task, options)?;
            match status {
                OutcomeStatus::Ok => ok_count += 1,
                OutcomeStatus::Warn => warn_count += 1,
                OutcomeStatus::Fail => fail_count += 1,
            }

            outcomes.push(TaskOutcome {
                id: task.id.clone(),
                label: task.label.clone(),
                status,
            });
        }

        Ok(RunSummary {
            outcomes,
            ok_count,
            warn_count,
            fail_count,
        })
    }

    fn run_task(&self, task: &TaskDefinition, options: &RunOptions) -> Result<OutcomeStatus> {
        let mut command = match &task.runner {
            TaskRunner::Script { path, shell, args } => {
                let mut command = Command::new(shell);
                command.arg(self.root.join(path));
                command.args(args);
                command
            }
            TaskRunner::Command { program, args } => {
                let mut command = Command::new(program);
                command.args(args);
                command
            }
        };

        command.current_dir(&self.root);
        command.env("UC_REPO_ROOT", &self.root);
        command.env("UC_DRY_RUN", bool_env(options.dry_run));
        command.env("UC_VERBOSE", bool_env(options.verbose));
        command.env("UC_BREW_CLEANUP", bool_env(options.brew_cleanup));
        command.env("UC_NPM_AUDIT", bool_env(options.npm_audit));
        command.env("UC_TASK_ID", &task.id);
        for (key, value) in &task.env {
            command.env(key, value);
        }

        let status = command
            .status()
            .with_context(|| format!("failed to execute task {}", task.id))?;
        let code = status.code().unwrap_or(1);

        let outcome = if code == 0 {
            OutcomeStatus::Ok
        } else if code == WARN_EXIT_CODE {
            OutcomeStatus::Warn
        } else {
            OutcomeStatus::Fail
        };

        if matches!(outcome, OutcomeStatus::Fail) {
            eprintln!("{} failed (exit {}).", task.label, code);
        }

        Ok(outcome)
    }
}

fn bool_env(value: bool) -> &'static str {
    if value { "1" } else { "0" }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::TempDir;

    use crate::catalog::{ExecutionPlan, TaskDefinition, TaskRunner};

    use super::{OutcomeStatus, RunOptions, Runner};

    #[test]
    fn classifies_task_outcomes() {
        let root = TempDir::new().expect("tempdir");
        let scripts_dir = root.path().join("scripts");
        fs::create_dir(&scripts_dir).expect("create scripts dir");

        write_script(
            &scripts_dir.join("ok.sh"),
            r#"#!/bin/sh
exit 0
"#,
        );
        write_script(
            &scripts_dir.join("warn.sh"),
            r#"#!/bin/sh
exit 10
"#,
        );
        write_script(
            &scripts_dir.join("fail.sh"),
            r#"#!/bin/sh
exit 3
"#,
        );

        let plan = ExecutionPlan {
            tasks: vec![
                task("ok", "scripts/ok.sh"),
                task("warn", "scripts/warn.sh"),
                task("fail", "scripts/fail.sh"),
            ],
        };

        let summary = Runner::new(root.path().to_path_buf())
            .run(&plan, &default_options())
            .expect("run tasks");

        let statuses: Vec<_> = summary
            .outcomes
            .iter()
            .map(|outcome| outcome.status)
            .collect();
        assert_eq!(
            statuses,
            vec![OutcomeStatus::Ok, OutcomeStatus::Warn, OutcomeStatus::Fail]
        );
        assert_eq!(summary.ok_count, 1);
        assert_eq!(summary.warn_count, 1);
        assert_eq!(summary.fail_count, 1);
    }

    #[test]
    fn forwards_runtime_environment() {
        let root = TempDir::new().expect("tempdir");
        let scripts_dir = root.path().join("scripts");
        fs::create_dir(&scripts_dir).expect("create scripts dir");
        let output_path = root.path().join("env-output.txt");

        write_script(
            &scripts_dir.join("env.sh"),
            &format!(
                r#"#!/bin/sh
printf "%s:%s:%s:%s" "$UC_DRY_RUN" "$UC_VERBOSE" "$UC_BREW_CLEANUP" "$UC_NPM_AUDIT" > "{}"
exit 0
"#,
                output_path.display()
            ),
        );

        let plan = ExecutionPlan {
            tasks: vec![task("env", "scripts/env.sh")],
        };
        let options = RunOptions {
            dry_run: true,
            verbose: true,
            brew_cleanup: true,
            npm_audit: true,
        };

        Runner::new(root.path().to_path_buf())
            .run(&plan, &options)
            .expect("run task");

        let contents = fs::read_to_string(output_path).expect("read output");
        assert_eq!(contents, "1:1:1:1");
    }

    fn task(id: &str, path: &str) -> TaskDefinition {
        TaskDefinition {
            id: id.to_string(),
            label: id.to_string(),
            description: String::new(),
            default_selected: true,
            dangerous: false,
            dependencies: Vec::new(),
            env: Default::default(),
            runner: TaskRunner::Script {
                path: path.into(),
                shell: "sh".to_string(),
                args: Vec::new(),
            },
        }
    }

    fn default_options() -> RunOptions {
        RunOptions {
            dry_run: false,
            verbose: false,
            brew_cleanup: false,
            npm_audit: false,
        }
    }

    fn write_script(path: &Path, contents: &str) {
        fs::write(path, contents).expect("write script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = fs::metadata(path).expect("metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(path, permissions).expect("chmod");
        }
    }
}
