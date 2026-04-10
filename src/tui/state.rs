use std::collections::VecDeque;

use anyhow::{Result, bail};

use crate::catalog::{Catalog, ExecutionPlan, TaskDefinition};
use crate::runner::{OutcomeStatus, RunOptions, RunSummary, RunnerEvent, StreamKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Select,
    ConfirmDangerous,
    Running,
    Summary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Pending,
    Running,
    Ok,
    Warn,
    Fail,
}

impl TaskState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskItem {
    pub id: String,
    pub label: String,
    pub description: String,
    pub dangerous: bool,
    pub dependencies: Vec<String>,
    pub selected: bool,
    pub state: TaskState,
}

#[derive(Debug, Clone)]
pub struct AppState {
    catalog: Catalog,
    tasks: Vec<TaskItem>,
    selected_index: usize,
    screen: Screen,
    options: RunOptions,
    logs: VecDeque<String>,
    summary: Option<RunSummary>,
    status_message: Option<String>,
    pending_plan: Option<ExecutionPlan>,
}

impl AppState {
    pub fn new(catalog: Catalog, options: RunOptions) -> Self {
        let tasks = catalog
            .tasks()
            .map(TaskItem::from_definition)
            .collect::<Vec<_>>();

        Self {
            catalog,
            tasks,
            selected_index: 0,
            screen: Screen::Select,
            options,
            logs: VecDeque::new(),
            summary: None,
            status_message: None,
            pending_plan: None,
        }
    }

    pub fn tasks(&self) -> &[TaskItem] {
        &self.tasks
    }

    pub fn selected_index(&self) -> usize {
        self.selected_index
    }

    pub fn screen(&self) -> Screen {
        self.screen
    }

    pub fn options(&self) -> RunOptions {
        self.options
    }

    pub fn logs(&self) -> &VecDeque<String> {
        &self.logs
    }

    pub fn summary(&self) -> Option<&RunSummary> {
        self.summary.as_ref()
    }

    pub fn status_message(&self) -> Option<&str> {
        self.status_message.as_deref()
    }

    pub fn selected_task(&self) -> &TaskItem {
        &self.tasks[self.selected_index]
    }

    pub fn move_next(&mut self) {
        if self.tasks.is_empty() {
            return;
        }

        self.selected_index = (self.selected_index + 1) % self.tasks.len();
    }

    pub fn move_previous(&mut self) {
        if self.tasks.is_empty() {
            return;
        }

        self.selected_index = if self.selected_index == 0 {
            self.tasks.len() - 1
        } else {
            self.selected_index - 1
        };
    }

    pub fn toggle_current(&mut self) {
        if self.screen != Screen::Select || self.tasks.is_empty() {
            return;
        }

        let task = &mut self.tasks[self.selected_index];
        task.selected = !task.selected;
    }

    pub fn select_all(&mut self) {
        if self.screen != Screen::Select {
            return;
        }

        for task in &mut self.tasks {
            task.selected = true;
        }
    }

    pub fn clear_selection(&mut self) {
        if self.screen != Screen::Select {
            return;
        }

        for task in &mut self.tasks {
            task.selected = false;
        }
    }

    pub fn toggle_dry_run(&mut self) {
        if self.screen == Screen::Select {
            self.options.dry_run = !self.options.dry_run;
        }
    }

    pub fn toggle_verbose(&mut self) {
        if self.screen == Screen::Select {
            self.options.verbose = !self.options.verbose;
        }
    }

    pub fn toggle_brew_cleanup(&mut self) {
        if self.screen == Screen::Select {
            self.options.brew_cleanup = !self.options.brew_cleanup;
        }
    }

    pub fn toggle_npm_audit(&mut self) {
        if self.screen == Screen::Select {
            self.options.npm_audit = !self.options.npm_audit;
        }
    }

    pub fn prepare_run(&mut self) -> Result<Option<ExecutionPlan>> {
        self.status_message = None;
        let selected_ids = self
            .tasks
            .iter()
            .filter(|task| task.selected)
            .map(|task| task.id.clone())
            .collect::<Vec<_>>();

        if selected_ids.is_empty() {
            bail!("select at least one task");
        }

        let plan = self.catalog.plan(false, &selected_ids)?;
        if plan.tasks.iter().any(|task| task.dangerous) {
            self.pending_plan = Some(plan);
            self.screen = Screen::ConfirmDangerous;
            self.status_message =
                Some("Dangerous tasks selected. Press y to continue or n to cancel.".to_string());
            return Ok(None);
        }

        self.begin_run();
        Ok(Some(plan))
    }

    pub fn confirm_run(&mut self) -> Option<ExecutionPlan> {
        if self.screen != Screen::ConfirmDangerous {
            return None;
        }

        let plan = self.pending_plan.take();
        if plan.is_some() {
            self.begin_run();
        }
        plan
    }

    pub fn cancel_confirmation(&mut self) {
        if self.screen == Screen::ConfirmDangerous {
            self.pending_plan = None;
            self.screen = Screen::Select;
            self.status_message = Some("Run cancelled.".to_string());
        }
    }

    pub fn reset_after_summary(&mut self) {
        self.screen = Screen::Select;
        self.summary = None;
        self.status_message = None;
        self.logs.clear();
        for task in &mut self.tasks {
            task.state = TaskState::Pending;
        }
    }

    pub fn handle_runner_event(&mut self, event: RunnerEvent) {
        match event {
            RunnerEvent::TaskStarted { task_id, label } => {
                self.set_task_state(&task_id, TaskState::Running);
                self.push_log(format!("==> {label}"));
            }
            RunnerEvent::OutputLine {
                task_id,
                stream,
                line,
            } => {
                let prefix = match stream {
                    StreamKind::Stdout => "out",
                    StreamKind::Stderr => "err",
                };
                self.push_log(format!("[{task_id}:{prefix}] {line}"));
            }
            RunnerEvent::TaskFinished {
                task_id,
                label,
                status,
            } => {
                self.set_task_state(&task_id, TaskState::from_outcome(status));
                self.push_log(format!("{label} finished: {}", status.label()));
            }
        }
    }

    pub fn finish_run(&mut self, result: Result<RunSummary>) {
        match result {
            Ok(summary) => {
                self.screen = Screen::Summary;
                self.status_message = Some("Run complete.".to_string());
                self.summary = Some(summary);
            }
            Err(error) => {
                self.screen = Screen::Summary;
                self.status_message = Some(format!("Run failed: {error:#}"));
                self.summary = None;
                self.push_log(format!("fatal: {error:#}"));
            }
        }
    }

    fn begin_run(&mut self) {
        self.logs.clear();
        self.summary = None;
        self.pending_plan = None;
        self.screen = Screen::Running;
        self.status_message = Some("Running selected tasks...".to_string());
        for task in &mut self.tasks {
            task.state = TaskState::Pending;
        }
    }

    fn set_task_state(&mut self, task_id: &str, state: TaskState) {
        if let Some(task) = self.tasks.iter_mut().find(|task| task.id == task_id) {
            task.state = state;
        }
    }

    fn push_log(&mut self, line: String) {
        self.logs.push_back(line);
        while self.logs.len() > 200 {
            self.logs.pop_front();
        }
    }
}

impl TaskItem {
    fn from_definition(task: &TaskDefinition) -> Self {
        Self {
            id: task.id.clone(),
            label: task.label.clone(),
            description: task.description.clone(),
            dangerous: task.dangerous,
            dependencies: task.dependencies.clone(),
            selected: task.default_selected,
            state: TaskState::Pending,
        }
    }
}

impl TaskState {
    fn from_outcome(status: OutcomeStatus) -> Self {
        match status {
            OutcomeStatus::Ok => Self::Ok,
            OutcomeStatus::Warn => Self::Warn,
            OutcomeStatus::Fail => Self::Fail,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::catalog::{Catalog, TaskDefinition, TaskRunner};
    use crate::runner::{
        OutcomeStatus, RunOptions, RunSummary, RunnerEvent, StreamKind, TaskOutcome,
    };

    use super::{AppState, Screen, TaskState};

    #[test]
    fn starts_with_default_selected_tasks() {
        let state = AppState::new(catalog_fixture(), default_options());
        let selected: Vec<_> = state
            .tasks()
            .iter()
            .filter(|task| task.selected)
            .map(|task| task.id.as_str())
            .collect();

        assert_eq!(selected, vec!["brew", "flutter", "node", "npm-tools"]);
        assert_eq!(state.screen(), Screen::Select);
    }

    #[test]
    fn prepares_plan_and_requires_confirmation_for_dangerous_tasks() {
        let mut state = AppState::new(catalog_fixture(), default_options());
        let plan = state.prepare_run().expect("prepare run");

        assert!(plan.is_none());
        assert_eq!(state.screen(), Screen::ConfirmDangerous);

        let confirmed = state.confirm_run().expect("confirmed plan");
        let ids: Vec<_> = confirmed
            .tasks
            .iter()
            .map(|task| task.id.as_str())
            .collect();
        assert_eq!(ids, vec!["brew", "flutter", "node", "npm-tools"]);
        assert_eq!(state.screen(), Screen::Running);
    }

    #[test]
    fn can_cancel_dangerous_confirmation() {
        let mut state = AppState::new(catalog_fixture(), default_options());
        state.prepare_run().expect("prepare run");
        state.cancel_confirmation();

        assert_eq!(state.screen(), Screen::Select);
        assert_eq!(state.status_message(), Some("Run cancelled."));
    }

    #[test]
    fn rejects_empty_selection() {
        let mut state = AppState::new(catalog_fixture(), default_options());
        state.clear_selection();

        let error = state.prepare_run().expect_err("should fail");
        assert!(error.to_string().contains("select at least one task"));
    }

    #[test]
    fn handles_runner_events_and_summary() {
        let mut state = AppState::new(catalog_fixture(), default_options());
        state.clear_selection();
        state.tasks[0].selected = true;
        let plan = state.prepare_run().expect("prepare run").expect("plan");
        assert_eq!(plan.tasks.len(), 1);

        state.handle_runner_event(RunnerEvent::TaskStarted {
            task_id: "brew".to_string(),
            label: "Homebrew".to_string(),
        });
        state.handle_runner_event(RunnerEvent::OutputLine {
            task_id: "brew".to_string(),
            stream: StreamKind::Stdout,
            line: "updating".to_string(),
        });
        state.handle_runner_event(RunnerEvent::TaskFinished {
            task_id: "brew".to_string(),
            label: "Homebrew".to_string(),
            status: OutcomeStatus::Warn,
        });
        state.finish_run(Ok(RunSummary {
            outcomes: vec![TaskOutcome {
                id: "brew".to_string(),
                label: "Homebrew".to_string(),
                status: OutcomeStatus::Warn,
            }],
            ok_count: 0,
            warn_count: 1,
            fail_count: 0,
        }));

        assert_eq!(state.screen(), Screen::Summary);
        assert_eq!(state.tasks()[0].state, TaskState::Warn);
        assert!(state.logs().iter().any(|line| line.contains("updating")));
        assert_eq!(state.summary().expect("summary").warn_count, 1);
    }

    fn catalog_fixture() -> Catalog {
        Catalog::from_task_definitions(vec![
            task("brew", "Homebrew", true, false, Vec::new()),
            task("flutter", "Flutter", true, true, Vec::new()),
            task("node", "Node", true, false, Vec::new()),
            task(
                "npm-tools",
                "npm tools",
                true,
                false,
                vec!["node".to_string()],
            ),
        ])
        .expect("catalog")
    }

    fn default_options() -> RunOptions {
        RunOptions {
            dry_run: false,
            verbose: false,
            brew_cleanup: false,
            npm_audit: false,
        }
    }

    fn task(
        id: &str,
        label: &str,
        default_selected: bool,
        dangerous: bool,
        dependencies: Vec<String>,
    ) -> TaskDefinition {
        TaskDefinition {
            id: id.to_string(),
            label: label.to_string(),
            description: format!("{label} description"),
            default_selected,
            dangerous,
            dependencies,
            env: BTreeMap::new(),
            runner: TaskRunner::Command {
                program: "echo".to_string(),
                args: vec![id.to_string()],
            },
        }
    }
}
