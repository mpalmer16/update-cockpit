use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunnerEvent {
    TaskStarted {
        task_id: String,
        label: String,
    },
    OutputLine {
        task_id: String,
        stream: StreamKind,
        line: String,
    },
    TaskFinished {
        task_id: String,
        label: String,
        status: OutcomeStatus,
    },
}

pub trait EventSink {
    fn handle(&mut self, event: RunnerEvent);
}

impl<F> EventSink for F
where
    F: FnMut(RunnerEvent),
{
    fn handle(&mut self, event: RunnerEvent) {
        self(event);
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

    pub fn run_with_events<S>(
        &self,
        plan: &ExecutionPlan,
        options: &RunOptions,
        sink: &mut S,
    ) -> Result<RunSummary>
    where
        S: EventSink,
    {
        let mut outcomes = Vec::new();
        let mut ok_count = 0;
        let mut warn_count = 0;
        let mut fail_count = 0;

        for task in &plan.tasks {
            sink.handle(RunnerEvent::TaskStarted {
                task_id: task.id.clone(),
                label: task.label.clone(),
            });

            let status = self.run_task_with_events(task, options, sink)?;
            match status {
                OutcomeStatus::Ok => ok_count += 1,
                OutcomeStatus::Warn => warn_count += 1,
                OutcomeStatus::Fail => fail_count += 1,
            }

            sink.handle(RunnerEvent::TaskFinished {
                task_id: task.id.clone(),
                label: task.label.clone(),
                status,
            });

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
        let mut command = build_command(task, &self.root, options);

        let status = command
            .status()
            .with_context(|| format!("failed to execute task {}", task.id))?;
        let code = status.code().unwrap_or(1);

        let outcome = classify_exit_code(code);

        if matches!(outcome, OutcomeStatus::Fail) {
            eprintln!("{} failed (exit {}).", task.label, code);
        }

        Ok(outcome)
    }

    fn run_task_with_events<S>(
        &self,
        task: &TaskDefinition,
        options: &RunOptions,
        sink: &mut S,
    ) -> Result<OutcomeStatus>
    where
        S: EventSink,
    {
        let mut command = build_command(task, &self.root, options);
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());

        let mut child = command
            .spawn()
            .with_context(|| format!("failed to execute task {}", task.id))?;
        let stdout = child.stdout.take().context("child stdout not captured")?;
        let stderr = child.stderr.take().context("child stderr not captured")?;
        let (tx, rx) = mpsc::channel();

        let stdout_tx = tx.clone();
        let stdout_task_id = task.id.clone();
        let stdout_thread = thread::spawn(move || {
            read_stream(stdout, StreamKind::Stdout, stdout_task_id, stdout_tx)
        });

        let stderr_tx = tx;
        let stderr_task_id = task.id.clone();
        let stderr_thread = thread::spawn(move || {
            read_stream(stderr, StreamKind::Stderr, stderr_task_id, stderr_tx)
        });

        for event in rx {
            sink.handle(event);
        }

        stdout_thread.join().expect("stdout reader panicked")?;
        stderr_thread.join().expect("stderr reader panicked")?;

        let status = child.wait().context("failed to wait on task process")?;
        Ok(classify_exit_code(status.code().unwrap_or(1)))
    }
}

fn build_command(task: &TaskDefinition, root: &PathBuf, options: &RunOptions) -> Command {
    let mut command = match &task.runner {
        TaskRunner::Script { path, shell, args } => {
            let mut command = Command::new(shell);
            command.arg(root.join(path));
            command.args(args);
            command
        }
        TaskRunner::Command { program, args } => {
            let mut command = Command::new(program);
            command.args(args);
            command
        }
    };

    command.current_dir(root);
    command.env("UC_REPO_ROOT", root);
    command.env("UC_DRY_RUN", bool_env(options.dry_run));
    command.env("UC_VERBOSE", bool_env(options.verbose));
    command.env("UC_BREW_CLEANUP", bool_env(options.brew_cleanup));
    command.env("UC_NPM_AUDIT", bool_env(options.npm_audit));
    command.env("UC_TASK_ID", &task.id);
    for (key, value) in &task.env {
        command.env(key, value);
    }

    command
}

fn bool_env(value: bool) -> &'static str {
    if value { "1" } else { "0" }
}

fn classify_exit_code(code: i32) -> OutcomeStatus {
    if code == 0 {
        OutcomeStatus::Ok
    } else if code == WARN_EXIT_CODE {
        OutcomeStatus::Warn
    } else {
        OutcomeStatus::Fail
    }
}

fn read_stream<R: std::io::Read + Send + 'static>(
    reader: R,
    stream: StreamKind,
    task_id: String,
    sender: mpsc::Sender<RunnerEvent>,
) -> Result<()> {
    for line in BufReader::new(reader).lines() {
        sender
            .send(RunnerEvent::OutputLine {
                task_id: task_id.clone(),
                stream,
                line: line?,
            })
            .ok();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::TempDir;

    use crate::catalog::{ExecutionPlan, TaskDefinition, TaskRunner};

    use super::{OutcomeStatus, RunOptions, Runner, RunnerEvent, StreamKind};

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

    #[test]
    fn emits_runner_events() {
        let root = TempDir::new().expect("tempdir");
        let scripts_dir = root.path().join("scripts");
        fs::create_dir(&scripts_dir).expect("create scripts dir");

        write_script(
            &scripts_dir.join("events.sh"),
            r#"#!/bin/sh
echo "line 1"
echo "line 2"
exit 0
"#,
        );

        let plan = ExecutionPlan {
            tasks: vec![task("events", "scripts/events.sh")],
        };
        let mut events = Vec::new();

        let summary = Runner::new(root.path().to_path_buf())
            .run_with_events(&plan, &default_options(), &mut |event| events.push(event))
            .expect("run task");

        assert_eq!(summary.ok_count, 1);
        assert!(matches!(
            events.first(),
            Some(RunnerEvent::TaskStarted { task_id, .. }) if task_id == "events"
        ));
        assert!(events.iter().any(|event| {
            matches!(
                event,
                RunnerEvent::OutputLine {
                    task_id,
                    stream: StreamKind::Stdout,
                    line,
                } if task_id == "events" && line == "line 1"
            )
        }));
        assert!(matches!(
            events.last(),
            Some(RunnerEvent::TaskFinished {
                task_id,
                status: OutcomeStatus::Ok,
                ..
            }) if task_id == "events"
        ));
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
