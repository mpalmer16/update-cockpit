use std::collections::VecDeque;

use anyhow::{Result, bail};

use crate::catalog::{Catalog, ExecutionPlan, TaskDefinition};
use crate::persistence::{HistoryEntry, MAX_HISTORY_ENTRIES, PersistedProfile, PersistedState};
use crate::profiles::{CUSTOM_PROFILE_ID, ProfileDefinition, built_in_profiles};
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
pub struct CompletedRun {
    pub started_at_unix_secs: u64,
    pub duration_ms: u64,
    pub profile_id: String,
    pub selected_tasks: Vec<String>,
    pub result: std::result::Result<RunSummary, String>,
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
    built_in_profiles: Vec<ProfileDefinition>,
    custom_profile: ProfileDefinition,
    active_profile_id: String,
    history: Vec<HistoryEntry>,
    dirty: bool,
}

impl AppState {
    pub fn new(catalog: Catalog, launch_options: RunOptions, persisted: PersistedState) -> Self {
        let mut tasks = catalog
            .tasks()
            .map(TaskItem::from_definition)
            .collect::<Vec<_>>();
        let default_selected = tasks
            .iter()
            .filter(|task| task.selected)
            .map(|task| task.id.clone())
            .collect::<Vec<_>>();

        let custom_profile = ProfileDefinition::custom(
            if persisted.custom_profile.selected_tasks.is_empty() {
                default_selected
            } else {
                persisted.custom_profile.selected_tasks.clone()
            },
            merge_options(persisted.custom_profile.options, launch_options),
        );
        let built_in_profiles = built_in_profiles();

        let active_profile_id = if built_in_profiles
            .iter()
            .any(|profile| profile.id == persisted.active_profile_id)
        {
            persisted.active_profile_id
        } else {
            CUSTOM_PROFILE_ID.to_string()
        };

        let mut state = Self {
            catalog,
            tasks: std::mem::take(&mut tasks),
            selected_index: 0,
            screen: Screen::Select,
            options: RunOptions::default(),
            logs: VecDeque::new(),
            summary: None,
            status_message: None,
            pending_plan: None,
            built_in_profiles,
            custom_profile: custom_profile.clone(),
            active_profile_id,
            history: persisted.history,
            dirty: false,
        };

        state.history.truncate(MAX_HISTORY_ENTRIES);
        if state.active_profile_id == CUSTOM_PROFILE_ID {
            state.apply_profile(&custom_profile);
        } else {
            state.apply_active_profile();
        }
        state.custom_profile = custom_profile;

        state
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

    pub fn set_status_message(&mut self, message: impl Into<String>) {
        self.status_message = Some(message.into());
    }

    pub fn selected_task(&self) -> &TaskItem {
        &self.tasks[self.selected_index]
    }

    pub fn active_profile(&self) -> &ProfileDefinition {
        if self.active_profile_id == CUSTOM_PROFILE_ID {
            &self.custom_profile
        } else {
            self.built_in_profiles
                .iter()
                .find(|profile| profile.id == self.active_profile_id)
                .unwrap_or(&self.custom_profile)
        }
    }

    pub fn profiles(&self) -> Vec<&ProfileDefinition> {
        let mut profiles = Vec::with_capacity(self.built_in_profiles.len() + 1);
        profiles.push(&self.custom_profile);
        for profile in &self.built_in_profiles {
            profiles.push(profile);
        }
        profiles
    }

    pub fn history(&self) -> &[HistoryEntry] {
        &self.history
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn snapshot(&self) -> PersistedState {
        PersistedState {
            active_profile_id: self.active_profile_id.clone(),
            custom_profile: PersistedProfile {
                selected_tasks: self.custom_profile.selected_tasks.clone(),
                options: self.custom_profile.options,
            },
            history: self.history.clone(),
            ..PersistedState::default()
        }
    }

    pub fn mark_clean(&mut self) {
        self.dirty = false;
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
        self.capture_custom_state();
    }

    pub fn select_all(&mut self) {
        if self.screen != Screen::Select {
            return;
        }

        for task in &mut self.tasks {
            task.selected = true;
        }
        self.capture_custom_state();
    }

    pub fn clear_selection(&mut self) {
        if self.screen != Screen::Select {
            return;
        }

        for task in &mut self.tasks {
            task.selected = false;
        }
        self.capture_custom_state();
    }

    pub fn toggle_dry_run(&mut self) {
        if self.screen == Screen::Select {
            self.options.dry_run = !self.options.dry_run;
            self.capture_custom_state();
        }
    }

    pub fn toggle_verbose(&mut self) {
        if self.screen == Screen::Select {
            self.options.verbose = !self.options.verbose;
            self.capture_custom_state();
        }
    }

    pub fn toggle_brew_cleanup(&mut self) {
        if self.screen == Screen::Select {
            self.options.brew_cleanup = !self.options.brew_cleanup;
            self.capture_custom_state();
        }
    }

    pub fn toggle_npm_audit(&mut self) {
        if self.screen == Screen::Select {
            self.options.npm_audit = !self.options.npm_audit;
            self.capture_custom_state();
        }
    }

    pub fn cycle_profile_next(&mut self) {
        if self.screen != Screen::Select {
            return;
        }
        self.cycle_profile(1);
    }

    pub fn cycle_profile_previous(&mut self) {
        if self.screen != Screen::Select {
            return;
        }
        self.cycle_profile(-1);
    }

    pub fn prepare_run(&mut self) -> Result<Option<ExecutionPlan>> {
        self.status_message = None;
        let selected_ids = self.selected_task_ids();

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

    pub fn rerun_failed(&mut self) -> Result<Option<ExecutionPlan>> {
        let failed_ids = self
            .tasks
            .iter()
            .filter(|task| task.state == TaskState::Fail)
            .map(|task| task.id.clone())
            .collect::<Vec<_>>();

        if failed_ids.is_empty() {
            bail!("no failed tasks to rerun");
        }

        self.screen = Screen::Select;
        for task in &mut self.tasks {
            task.selected = failed_ids.contains(&task.id);
        }
        self.capture_custom_state();
        self.prepare_run()
    }

    pub fn rerun_last_profile(&mut self) -> Result<Option<ExecutionPlan>> {
        let Some(last_profile_id) = self.history.last().map(|entry| entry.profile_id.clone())
        else {
            bail!("no run history available");
        };

        self.screen = Screen::Select;
        if last_profile_id == CUSTOM_PROFILE_ID {
            self.apply_profile_by_id(CUSTOM_PROFILE_ID);
        } else {
            self.apply_profile_by_id(&last_profile_id);
        }
        self.prepare_run()
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

    pub fn finish_run(&mut self, completed_run: CompletedRun) {
        match completed_run.result {
            Ok(summary) => {
                self.screen = Screen::Summary;
                self.status_message = Some("Run complete.".to_string());
                self.history.push(HistoryEntry::from_run_summary(
                    completed_run.started_at_unix_secs,
                    completed_run.duration_ms,
                    completed_run.profile_id,
                    completed_run.selected_tasks,
                    &summary,
                ));
                if self.history.len() > MAX_HISTORY_ENTRIES {
                    let keep_from = self.history.len() - MAX_HISTORY_ENTRIES;
                    self.history.drain(0..keep_from);
                }
                self.summary = Some(summary);
                self.dirty = true;
            }
            Err(error) => {
                self.screen = Screen::Summary;
                self.status_message = Some(format!("Run failed: {error}"));
                self.summary = None;
                self.push_log(format!("fatal: {error}"));
            }
        }
    }

    pub fn active_profile_id(&self) -> &str {
        &self.active_profile_id
    }

    pub fn selected_task_ids(&self) -> Vec<String> {
        self.tasks
            .iter()
            .filter(|task| task.selected)
            .map(|task| task.id.clone())
            .collect()
    }

    fn cycle_profile(&mut self, step: isize) {
        let profile_ids = self
            .profiles()
            .into_iter()
            .map(|profile| profile.id.clone())
            .collect::<Vec<_>>();
        let current_index = profile_ids
            .iter()
            .position(|id| id == &self.active_profile_id)
            .unwrap_or(0);

        let len = profile_ids.len() as isize;
        let next_index = (current_index as isize + step).rem_euclid(len) as usize;
        self.apply_profile_by_id(&profile_ids[next_index]);
    }

    fn apply_profile_by_id(&mut self, profile_id: &str) {
        if profile_id == CUSTOM_PROFILE_ID {
            let profile = self.custom_profile.clone();
            self.active_profile_id = CUSTOM_PROFILE_ID.to_string();
            self.apply_profile(&profile);
        } else if let Some(profile) = self
            .built_in_profiles
            .iter()
            .find(|profile| profile.id == profile_id)
            .cloned()
        {
            self.active_profile_id = profile.id.clone();
            self.apply_profile(&profile);
        }

        self.status_message = Some(format!("Profile: {}", self.active_profile().label));
        self.dirty = true;
    }

    fn apply_active_profile(&mut self) {
        let profile = self.active_profile().clone();
        self.apply_profile(&profile);
    }

    fn apply_profile(&mut self, profile: &ProfileDefinition) {
        self.options = profile.options;
        for task in &mut self.tasks {
            task.selected = profile.selected_tasks.contains(&task.id);
        }
    }

    fn capture_custom_state(&mut self) {
        self.active_profile_id = CUSTOM_PROFILE_ID.to_string();
        self.custom_profile = ProfileDefinition::custom(self.selected_task_ids(), self.options);
        self.status_message = Some("Profile: Custom".to_string());
        self.dirty = true;
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

fn merge_options(saved: RunOptions, launch: RunOptions) -> RunOptions {
    RunOptions {
        dry_run: saved.dry_run || launch.dry_run,
        verbose: saved.verbose || launch.verbose,
        brew_cleanup: saved.brew_cleanup || launch.brew_cleanup,
        npm_audit: saved.npm_audit || launch.npm_audit,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::catalog::{Catalog, TaskDefinition, TaskRunner};
    use crate::persistence::{HistoryEntry, HistorySummary, PersistedProfile, PersistedState};
    use crate::profiles::CUSTOM_PROFILE_ID;
    use crate::runner::{
        OutcomeStatus, RunOptions, RunSummary, RunnerEvent, StreamKind, TaskOutcome,
    };

    use super::{AppState, CompletedRun, Screen, TaskState};

    #[test]
    fn starts_from_persisted_profile() {
        let state = AppState::new(
            catalog_fixture(),
            RunOptions::default(),
            PersistedState {
                active_profile_id: "safe".to_string(),
                ..PersistedState::default()
            },
        );
        let selected: Vec<_> = state
            .tasks()
            .iter()
            .filter(|task| task.selected)
            .map(|task| task.id.as_str())
            .collect();

        assert_eq!(state.active_profile().id, "safe");
        assert!(!selected.contains(&"flutter"));
    }

    #[test]
    fn manual_changes_switch_to_custom_profile() {
        let mut state = AppState::new(
            catalog_fixture(),
            RunOptions::default(),
            PersistedState::default(),
        );
        state.apply_profile_by_id("safe");
        state.toggle_current();

        assert_eq!(state.active_profile_id(), CUSTOM_PROFILE_ID);
        assert!(state.is_dirty());
    }

    #[test]
    fn cycles_through_profiles() {
        let mut state = AppState::new(
            catalog_fixture(),
            RunOptions::default(),
            PersistedState::default(),
        );
        state.cycle_profile_next();
        assert_eq!(state.active_profile().id, "full");
        state.cycle_profile_previous();
        assert_eq!(state.active_profile().id, CUSTOM_PROFILE_ID);
    }

    #[test]
    fn prepares_plan_and_requires_confirmation_for_dangerous_tasks() {
        let mut state = AppState::new(
            catalog_fixture(),
            RunOptions::default(),
            PersistedState::default(),
        );
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
        let mut state = AppState::new(
            catalog_fixture(),
            RunOptions::default(),
            PersistedState::default(),
        );
        state.prepare_run().expect("prepare run");
        state.cancel_confirmation();

        assert_eq!(state.screen(), Screen::Select);
        assert_eq!(state.status_message(), Some("Run cancelled."));
    }

    #[test]
    fn rejects_empty_selection() {
        let mut state = AppState::new(
            catalog_fixture(),
            RunOptions::default(),
            PersistedState::default(),
        );
        state.clear_selection();

        let error = state.prepare_run().expect_err("should fail");
        assert!(error.to_string().contains("select at least one task"));
    }

    #[test]
    fn handles_runner_events_and_summary() {
        let mut state = AppState::new(
            catalog_fixture(),
            RunOptions::default(),
            PersistedState::default(),
        );
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
        state.finish_run(CompletedRun {
            started_at_unix_secs: 10,
            duration_ms: 500,
            profile_id: "custom".to_string(),
            selected_tasks: vec!["brew".to_string()],
            result: Ok(RunSummary {
                outcomes: vec![TaskOutcome {
                    id: "brew".to_string(),
                    label: "Homebrew".to_string(),
                    status: OutcomeStatus::Warn,
                }],
                ok_count: 0,
                warn_count: 1,
                fail_count: 0,
            }),
        });

        assert_eq!(state.screen(), Screen::Summary);
        assert_eq!(state.tasks()[0].state, TaskState::Warn);
        assert!(state.logs().iter().any(|line| line.contains("updating")));
        assert_eq!(state.summary().expect("summary").warn_count, 1);
        assert_eq!(state.history().len(), 1);
    }

    #[test]
    fn reruns_only_failed_tasks() {
        let mut state = AppState::new(
            catalog_fixture(),
            RunOptions::default(),
            PersistedState::default(),
        );
        state.finish_run(CompletedRun {
            started_at_unix_secs: 10,
            duration_ms: 500,
            profile_id: "custom".to_string(),
            selected_tasks: vec!["brew".to_string()],
            result: Ok(RunSummary {
                outcomes: vec![
                    TaskOutcome {
                        id: "brew".to_string(),
                        label: "Homebrew".to_string(),
                        status: OutcomeStatus::Fail,
                    },
                    TaskOutcome {
                        id: "node".to_string(),
                        label: "Node".to_string(),
                        status: OutcomeStatus::Ok,
                    },
                ],
                ok_count: 1,
                warn_count: 0,
                fail_count: 1,
            }),
        });
        state.tasks[0].state = TaskState::Fail;
        state.tasks[2].state = TaskState::Ok;

        let plan = state.rerun_failed().expect("rerun").expect("plan");
        let ids: Vec<_> = plan.tasks.iter().map(|task| task.id.as_str()).collect();
        assert_eq!(ids, vec!["brew"]);
    }

    #[test]
    fn reruns_last_profile() {
        let mut state = AppState::new(
            catalog_fixture(),
            RunOptions::default(),
            PersistedState {
                history: vec![HistoryEntry {
                    started_at_unix_secs: 1,
                    duration_ms: 2,
                    profile_id: "safe".to_string(),
                    selected_tasks: vec!["brew".to_string()],
                    summary: HistorySummary {
                        ok_count: 1,
                        warn_count: 0,
                        fail_count: 0,
                        outcome_labels: vec!["Homebrew:OK".to_string()],
                    },
                }],
                ..PersistedState::default()
            },
        );

        let plan = state.rerun_last_profile().expect("rerun").expect("plan");
        let ids: Vec<_> = plan.tasks.iter().map(|task| task.id.as_str()).collect();

        assert_eq!(state.active_profile_id(), "safe");
        assert!(!ids.contains(&"flutter"));
    }

    #[test]
    fn snapshots_custom_profile_and_history() {
        let state = AppState::new(
            catalog_fixture(),
            RunOptions::default(),
            PersistedState {
                active_profile_id: CUSTOM_PROFILE_ID.to_string(),
                custom_profile: PersistedProfile {
                    selected_tasks: vec!["brew".to_string()],
                    options: RunOptions {
                        verbose: true,
                        ..RunOptions::default()
                    },
                },
                history: vec![HistoryEntry {
                    started_at_unix_secs: 1,
                    duration_ms: 2,
                    profile_id: "custom".to_string(),
                    selected_tasks: vec!["brew".to_string()],
                    summary: HistorySummary {
                        ok_count: 1,
                        warn_count: 0,
                        fail_count: 0,
                        outcome_labels: vec!["Homebrew:OK".to_string()],
                    },
                }],
                ..PersistedState::default()
            },
        );

        let snapshot = state.snapshot();
        assert_eq!(snapshot.active_profile_id, CUSTOM_PROFILE_ID);
        assert!(snapshot.custom_profile.options.verbose);
        assert_eq!(snapshot.history.len(), 1);
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
            task("rust", "Rust", false, false, Vec::new()),
            task("julia", "Julia", false, false, Vec::new()),
            task("sdkman", "SDKMAN", false, false, Vec::new()),
        ])
        .expect("catalog")
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
