use std::collections::{BTreeMap, VecDeque};

use anyhow::{Result, bail};

use crate::catalog::{Catalog, ExecutionPlan, MissingRequirementPolicy, TaskDefinition};
use crate::persistence::{HistoryEntry, MAX_HISTORY_ENTRIES, PersistedProfile, PersistedState};
use crate::profiles::{CUSTOM_PROFILE_ID, ProfileDefinition, built_in_profiles};
use crate::runner::{
    OutcomeStatus, PreflightReport, RunOptions, RunSummary, RunnerEvent, StreamKind,
    inspect_preflight,
};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvailabilityState {
    Available,
    WarnUnavailable,
    FailUnavailable,
}

impl AvailabilityState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::WarnUnavailable => "warn-skip",
            Self::FailUnavailable => "blocked",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScopeFilter {
    #[default]
    All,
    Selected,
    Available,
    Unavailable,
}

impl ScopeFilter {
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Selected => "selected",
            Self::Available => "available",
            Self::Unavailable => "unavailable",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::All => Self::Selected,
            Self::Selected => Self::Available,
            Self::Available => Self::Unavailable,
            Self::Unavailable => Self::All,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TaskFilter {
    pub scope: ScopeFilter,
    pub category: Option<String>,
    pub tag: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskItem {
    pub id: String,
    pub label: String,
    pub description: String,
    pub category: String,
    pub tags: Vec<String>,
    pub notes: Vec<String>,
    pub dangerous: bool,
    pub danger_message: Option<String>,
    pub dependencies: Vec<String>,
    pub requires_commands: Vec<String>,
    pub requires_paths: Vec<String>,
    pub on_missing: MissingRequirementPolicy,
    pub availability: AvailabilityState,
    pub preflight_messages: Vec<String>,
    pub selected: bool,
    pub state: TaskState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskListEntry {
    Header(String),
    Task(usize),
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
    filter: TaskFilter,
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
            filter: TaskFilter::default(),
            dirty: false,
        };

        state.history.truncate(MAX_HISTORY_ENTRIES);
        if state.active_profile_id == CUSTOM_PROFILE_ID {
            state.apply_profile(&custom_profile);
        } else {
            state.apply_active_profile();
        }
        state.custom_profile = custom_profile;
        state.sync_selected_index();

        state
    }

    pub fn tasks(&self) -> &[TaskItem] {
        &self.tasks
    }

    pub fn selected_index(&self) -> usize {
        self.selected_index
    }

    pub fn selected_list_index(&self) -> Option<usize> {
        self.task_list_entries().iter().position(
            |entry| matches!(entry, TaskListEntry::Task(index) if *index == self.selected_index),
        )
    }

    pub fn task_list_entries(&self) -> Vec<TaskListEntry> {
        let mut grouped = BTreeMap::<String, Vec<usize>>::new();
        for index in self.visible_task_indices() {
            grouped
                .entry(self.tasks[index].category.clone())
                .or_default()
                .push(index);
        }

        let mut entries = Vec::new();
        for (category, indices) in grouped {
            entries.push(TaskListEntry::Header(category));
            for index in indices {
                entries.push(TaskListEntry::Task(index));
            }
        }
        entries
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

    pub fn selected_visible_task(&self) -> Option<&TaskItem> {
        self.visible_task_indices()
            .contains(&self.selected_index)
            .then_some(&self.tasks[self.selected_index])
    }

    pub fn filter(&self) -> &TaskFilter {
        &self.filter
    }

    pub fn filter_summary(&self) -> String {
        let mut parts = vec![self.filter.scope.label().to_string()];
        if let Some(category) = &self.filter.category {
            parts.push(format!("category:{category}"));
        }
        if let Some(tag) = &self.filter.tag {
            parts.push(format!("tag:{tag}"));
        }
        parts.join(" | ")
    }

    pub fn pending_danger_messages(&self) -> Vec<(String, String)> {
        let Some(plan) = &self.pending_plan else {
            return Vec::new();
        };

        plan.tasks
            .iter()
            .filter(|task| task.dangerous)
            .map(|task| {
                (
                    task.label.clone(),
                    task.danger_message.clone().unwrap_or_else(|| {
                        "Marked dangerous, but no specific danger message was provided.".to_string()
                    }),
                )
            })
            .collect()
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
        let visible = self.visible_task_indices();
        if visible.is_empty() {
            return;
        }

        let current_position = visible
            .iter()
            .position(|index| *index == self.selected_index)
            .unwrap_or(0);
        self.selected_index = visible[(current_position + 1) % visible.len()];
    }

    pub fn move_previous(&mut self) {
        let visible = self.visible_task_indices();
        if visible.is_empty() {
            return;
        }

        let current_position = visible
            .iter()
            .position(|index| *index == self.selected_index)
            .unwrap_or(0);
        self.selected_index = if current_position == 0 {
            *visible.last().expect("visible task")
        } else {
            visible[current_position - 1]
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

    pub fn cycle_scope_filter(&mut self) {
        if self.screen != Screen::Select {
            return;
        }

        self.filter.scope = self.filter.scope.next();
        self.sync_selected_index();
        self.status_message = Some(format!("Filter: {}", self.filter_summary()));
    }

    pub fn toggle_selected_category_filter(&mut self) {
        if self.screen != Screen::Select {
            return;
        }

        let Some(task) = self.selected_visible_task() else {
            self.status_message = Some("No tasks match the current filters.".to_string());
            return;
        };

        let category = task.category.clone();
        if self.filter.category.as_deref() == Some(category.as_str()) {
            self.filter.category = None;
        } else {
            self.filter.category = Some(category);
        }
        self.sync_selected_index();
        self.status_message = Some(format!("Filter: {}", self.filter_summary()));
    }

    pub fn toggle_selected_tag_filter(&mut self) -> Result<()> {
        if self.screen != Screen::Select {
            return Ok(());
        }

        let Some(task) = self.selected_visible_task() else {
            bail!("no tasks match the current filters");
        };

        let Some(tag) = task.tags.first().cloned() else {
            bail!("selected task has no tags to filter by");
        };

        if self.filter.tag.as_deref() == Some(tag.as_str()) {
            self.filter.tag = None;
        } else {
            self.filter.tag = Some(tag);
        }
        self.sync_selected_index();
        self.status_message = Some(format!("Filter: {}", self.filter_summary()));
        Ok(())
    }

    pub fn clear_filters(&mut self) {
        if self.screen != Screen::Select {
            return;
        }

        self.filter = TaskFilter::default();
        self.sync_selected_index();
        self.status_message = Some("Filters cleared.".to_string());
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
        self.sync_selected_index();
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
        self.sync_selected_index();
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
        self.sync_selected_index();
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

    fn visible_task_indices(&self) -> Vec<usize> {
        let mut grouped = BTreeMap::<String, Vec<usize>>::new();
        for (index, task) in self.tasks.iter().enumerate() {
            if self.matches_filter(task) {
                grouped
                    .entry(task.category.clone())
                    .or_default()
                    .push(index);
            }
        }

        let mut ordered = Vec::new();
        for (_, indices) in grouped {
            ordered.extend(indices);
        }
        ordered
    }

    fn matches_filter(&self, task: &TaskItem) -> bool {
        let scope_match = match self.filter.scope {
            ScopeFilter::All => true,
            ScopeFilter::Selected => task.selected,
            ScopeFilter::Available => task.availability == AvailabilityState::Available,
            ScopeFilter::Unavailable => task.availability != AvailabilityState::Available,
        };

        let category_match = self
            .filter
            .category
            .as_ref()
            .is_none_or(|category| &task.category == category);
        let tag_match = self
            .filter
            .tag
            .as_ref()
            .is_none_or(|tag| task.tags.iter().any(|task_tag| task_tag == tag));

        scope_match && category_match && tag_match
    }

    fn sync_selected_index(&mut self) {
        let visible = self.visible_task_indices();
        if visible.is_empty() {
            self.selected_index = 0;
            return;
        }

        if !visible.contains(&self.selected_index) {
            self.selected_index = visible[0];
        }
    }
}

impl TaskItem {
    fn from_definition(task: &TaskDefinition) -> Self {
        let preflight = inspect_preflight(task).unwrap_or_else(|error| PreflightReport {
            status: Some(OutcomeStatus::Fail),
            messages: vec![format!("Preflight inspection failed: {error:#}")],
        });

        Self {
            id: task.id.clone(),
            label: task.label.clone(),
            description: task.description.clone(),
            category: task.category.clone(),
            tags: task.tags.clone(),
            notes: task.notes.clone(),
            dangerous: task.dangerous,
            danger_message: task.danger_message.clone(),
            dependencies: task.dependencies.clone(),
            requires_commands: task.preflight.requires_commands.clone(),
            requires_paths: task.preflight.requires_paths.clone(),
            on_missing: task.preflight.on_missing,
            availability: AvailabilityState::from_preflight_status(preflight.status),
            preflight_messages: preflight.messages,
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

impl AvailabilityState {
    fn from_preflight_status(status: Option<OutcomeStatus>) -> Self {
        match status {
            None | Some(OutcomeStatus::Ok) => Self::Available,
            Some(OutcomeStatus::Warn) => Self::WarnUnavailable,
            Some(OutcomeStatus::Fail) => Self::FailUnavailable,
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

    use crate::catalog::{
        Catalog, MissingRequirementPolicy, TaskDefinition, TaskPreflight, TaskRunner,
    };
    use crate::persistence::{HistoryEntry, HistorySummary, PersistedProfile, PersistedState};
    use crate::profiles::CUSTOM_PROFILE_ID;
    use crate::runner::{
        OutcomeStatus, RunOptions, RunSummary, RunnerEvent, StreamKind, TaskOutcome,
    };

    use super::{
        AppState, AvailabilityState, CompletedRun, ScopeFilter, Screen, TaskListEntry, TaskState,
    };

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
    fn cycles_scope_filters() {
        let mut state = AppState::new(
            catalog_fixture(),
            RunOptions::default(),
            PersistedState::default(),
        );
        assert_eq!(state.filter().scope, ScopeFilter::All);
        state.cycle_scope_filter();
        assert_eq!(state.filter().scope, ScopeFilter::Selected);
        state.cycle_scope_filter();
        assert_eq!(state.filter().scope, ScopeFilter::Available);
        state.cycle_scope_filter();
        assert_eq!(state.filter().scope, ScopeFilter::Unavailable);
    }

    #[test]
    fn toggles_category_and_tag_filters_from_selected_task() {
        let mut state = AppState::new(
            catalog_fixture(),
            RunOptions::default(),
            PersistedState::default(),
        );
        state.selected_index = 1;
        state.toggle_selected_category_filter();
        assert_eq!(state.filter().category.as_deref(), Some("toolchain"));
        state.toggle_selected_tag_filter().expect("tag filter");
        assert_eq!(state.filter().tag.as_deref(), Some("sdk"));
        state.clear_filters();
        assert!(state.filter().category.is_none());
        assert!(state.filter().tag.is_none());
    }

    #[test]
    fn groups_visible_tasks_by_category() {
        let state = AppState::new(
            catalog_fixture(),
            RunOptions::default(),
            PersistedState::default(),
        );
        let entries = state.task_list_entries();
        assert!(
            matches!(entries[0], TaskListEntry::Header(ref category) if category == "package-manager")
        );
        assert!(entries.iter().any(
            |entry| matches!(entry, TaskListEntry::Header(category) if category == "toolchain")
        ));
    }

    #[test]
    fn unavailable_filter_shows_only_unavailable_tasks() {
        let mut state = AppState::new(
            catalog_fixture(),
            RunOptions::default(),
            PersistedState::default(),
        );
        state.cycle_scope_filter();
        state.cycle_scope_filter();
        state.cycle_scope_filter();
        let visible_ids = state
            .task_list_entries()
            .into_iter()
            .filter_map(|entry| match entry {
                TaskListEntry::Task(index) => Some(state.tasks()[index].id.as_str()),
                TaskListEntry::Header(_) => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(visible_ids, vec!["sdkman"]);
    }

    #[test]
    fn selected_task_tracks_visible_filter_space() {
        let mut state = AppState::new(
            catalog_fixture(),
            RunOptions::default(),
            PersistedState::default(),
        );
        state.selected_index = 1;
        state.toggle_selected_tag_filter().expect("tag filter");
        assert_eq!(state.selected_task().id, "flutter");
        state.toggle_current();
        assert_eq!(state.active_profile_id(), CUSTOM_PROFILE_ID);
    }

    #[test]
    fn handles_empty_filtered_views_without_a_selected_task() {
        let mut state = AppState::new(
            catalog_fixture(),
            RunOptions::default(),
            PersistedState::default(),
        );
        state.selected_index = 0;
        state.toggle_selected_tag_filter().expect("tag filter");
        state.filter.category = Some("toolchain".to_string());
        state.sync_selected_index();
        assert!(state.task_list_entries().is_empty());
        assert!(state.selected_visible_task().is_none());
        assert!(state.toggle_selected_tag_filter().is_err());
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
        let messages = state.pending_danger_messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].0, "Flutter");
        assert!(messages[0].1.contains("destructive"));
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
            task(
                "brew",
                "Homebrew",
                "package-manager",
                vec!["packages"],
                true,
                false,
                Vec::new(),
            ),
            task(
                "flutter",
                "Flutter",
                "toolchain",
                vec!["sdk", "destructive"],
                true,
                true,
                Vec::new(),
            ),
            task(
                "node",
                "Node",
                "toolchain",
                vec!["runtime"],
                true,
                false,
                Vec::new(),
            ),
            task(
                "npm-tools",
                "npm tools",
                "package-manager",
                vec!["cli"],
                true,
                false,
                vec!["node".to_string()],
            ),
            task(
                "rust",
                "Rust",
                "toolchain",
                vec!["runtime"],
                false,
                false,
                Vec::new(),
            ),
            task(
                "julia",
                "Julia",
                "toolchain",
                vec!["runtime"],
                false,
                false,
                Vec::new(),
            ),
            unavailable_task("sdkman", "SDKMAN", "toolchain"),
        ])
        .expect("catalog")
    }

    fn task(
        id: &str,
        label: &str,
        category: &str,
        tags: Vec<&str>,
        default_selected: bool,
        dangerous: bool,
        dependencies: Vec<String>,
    ) -> TaskDefinition {
        TaskDefinition {
            id: id.to_string(),
            label: label.to_string(),
            description: format!("{label} description"),
            category: category.to_string(),
            tags: tags.into_iter().map(ToString::to_string).collect(),
            notes: Vec::new(),
            default_selected,
            dangerous,
            danger_message: if dangerous {
                Some(format!("{label} has destructive side effects."))
            } else {
                None
            },
            dependencies,
            env: BTreeMap::new(),
            preflight: TaskPreflight {
                requires_commands: Vec::new(),
                requires_paths: Vec::new(),
                on_missing: MissingRequirementPolicy::Fail,
            },
            runner: TaskRunner::Command {
                program: "echo".to_string(),
                args: vec![id.to_string()],
            },
        }
    }

    fn unavailable_task(id: &str, label: &str, category: &str) -> TaskDefinition {
        TaskDefinition {
            id: id.to_string(),
            label: label.to_string(),
            description: format!("{label} description"),
            category: category.to_string(),
            tags: vec!["manager".to_string()],
            notes: Vec::new(),
            default_selected: false,
            dangerous: false,
            danger_message: None,
            dependencies: Vec::new(),
            env: BTreeMap::new(),
            preflight: TaskPreflight {
                requires_commands: vec!["definitely-not-installed-upgrade-cockpit".to_string()],
                requires_paths: Vec::new(),
                on_missing: MissingRequirementPolicy::Warn,
            },
            runner: TaskRunner::Command {
                program: "echo".to_string(),
                args: vec![id.to_string()],
            },
        }
    }

    #[test]
    fn marks_unavailable_tasks_from_preflight() {
        let state = AppState::new(
            catalog_fixture(),
            RunOptions::default(),
            PersistedState::default(),
        );
        let sdkman = state
            .tasks()
            .iter()
            .find(|task| task.id == "sdkman")
            .expect("sdkman");
        assert_eq!(sdkman.availability, AvailabilityState::WarnUnavailable);
        assert!(!sdkman.preflight_messages.is_empty());
    }
}
