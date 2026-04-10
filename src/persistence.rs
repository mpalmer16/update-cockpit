use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::profiles::CUSTOM_PROFILE_ID;
use crate::runner::{OutcomeStatus, RunOptions, RunSummary};

pub const MAX_HISTORY_ENTRIES: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedState {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default = "default_active_profile")]
    pub active_profile_id: String,
    #[serde(default)]
    pub custom_profile: PersistedProfile,
    #[serde(default)]
    pub history: Vec<HistoryEntry>,
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            version: default_version(),
            active_profile_id: default_active_profile(),
            custom_profile: PersistedProfile::default(),
            history: Vec::new(),
        }
    }
}

impl PersistedState {
    pub fn trim_history(&mut self) {
        if self.history.len() > MAX_HISTORY_ENTRIES {
            let keep_from = self.history.len() - MAX_HISTORY_ENTRIES;
            self.history.drain(0..keep_from);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PersistedProfile {
    #[serde(default)]
    pub selected_tasks: Vec<String>,
    #[serde(default)]
    pub options: RunOptions,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub started_at_unix_secs: u64,
    pub duration_ms: u64,
    pub profile_id: String,
    pub selected_tasks: Vec<String>,
    pub summary: HistorySummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistorySummary {
    pub ok_count: usize,
    pub warn_count: usize,
    pub fail_count: usize,
    pub outcome_labels: Vec<String>,
}

impl HistoryEntry {
    pub fn from_run_summary(
        started_at_unix_secs: u64,
        duration_ms: u64,
        profile_id: String,
        selected_tasks: Vec<String>,
        summary: &RunSummary,
    ) -> Self {
        Self {
            started_at_unix_secs,
            duration_ms,
            profile_id,
            selected_tasks,
            summary: HistorySummary {
                ok_count: summary.ok_count,
                warn_count: summary.warn_count,
                fail_count: summary.fail_count,
                outcome_labels: summary
                    .outcomes
                    .iter()
                    .map(|outcome| format!("{}:{}", outcome.label, outcome.status.label()))
                    .collect(),
            },
        }
    }
}

impl HistorySummary {
    pub fn overall_status(&self) -> OutcomeStatus {
        if self.fail_count != 0 {
            OutcomeStatus::Fail
        } else if self.warn_count != 0 {
            OutcomeStatus::Warn
        } else {
            OutcomeStatus::Ok
        }
    }
}

#[derive(Debug, Clone)]
pub struct PersistenceStore {
    path: PathBuf,
}

impl PersistenceStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn default_path() -> Result<PathBuf> {
        let Some(project_dirs) = ProjectDirs::from("com", "mpalmer", "upgrade-cockpit") else {
            bail!("could not determine a config directory for upgrade-cockpit");
        };
        Ok(project_dirs.config_dir().join("state.toml"))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<PersistedState> {
        if !self.path.exists() {
            return Ok(PersistedState::default());
        }

        let contents = fs::read_to_string(&self.path)
            .with_context(|| format!("failed to read {}", self.path.display()))?;
        let mut state: PersistedState = toml::from_str(&contents)
            .with_context(|| format!("failed to parse {}", self.path.display()))?;
        state.trim_history();
        Ok(state)
    }

    pub fn save(&self, state: &PersistedState) -> Result<()> {
        let parent = self
            .path
            .parent()
            .with_context(|| format!("{} has no parent directory", self.path.display()))?;
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;

        let mut state = state.clone();
        state.trim_history();
        let contents = toml::to_string_pretty(&state).context("failed to serialize state")?;
        fs::write(&self.path, contents)
            .with_context(|| format!("failed to write {}", self.path.display()))?;
        Ok(())
    }
}

fn default_version() -> u32 {
    1
}

fn default_active_profile() -> String {
    CUSTOM_PROFILE_ID.to_string()
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::{
        HistoryEntry, MAX_HISTORY_ENTRIES, PersistedProfile, PersistedState, PersistenceStore,
    };
    use crate::runner::{OutcomeStatus, RunOptions, RunSummary, TaskOutcome};

    #[test]
    fn saves_and_loads_state() {
        let temp = TempDir::new().expect("tempdir");
        let store = PersistenceStore::new(temp.path().join("state.toml"));
        let state = PersistedState {
            active_profile_id: "safe".to_string(),
            custom_profile: PersistedProfile {
                selected_tasks: vec!["brew".to_string(), "rust".to_string()],
                options: RunOptions {
                    dry_run: true,
                    ..RunOptions::default()
                },
            },
            history: vec![history_entry(1)],
            ..PersistedState::default()
        };

        store.save(&state).expect("save state");
        let loaded = store.load().expect("load state");

        assert_eq!(loaded.active_profile_id, "safe");
        assert_eq!(
            loaded.custom_profile.selected_tasks,
            vec!["brew".to_string(), "rust".to_string()]
        );
        assert!(loaded.custom_profile.options.dry_run);
        assert_eq!(loaded.history.len(), 1);
    }

    #[test]
    fn trims_history_on_save() {
        let temp = TempDir::new().expect("tempdir");
        let store = PersistenceStore::new(temp.path().join("state.toml"));
        let mut state = PersistedState::default();
        state.history = (0..(MAX_HISTORY_ENTRIES + 3))
            .map(|index| history_entry(index as u64))
            .collect();

        store.save(&state).expect("save state");
        let loaded = store.load().expect("load state");

        assert_eq!(loaded.history.len(), MAX_HISTORY_ENTRIES);
        assert_eq!(
            loaded.history.first().expect("first").started_at_unix_secs,
            3
        );
    }

    #[test]
    fn builds_history_from_run_summary() {
        let summary = RunSummary {
            outcomes: vec![
                TaskOutcome {
                    id: "brew".to_string(),
                    label: "Homebrew".to_string(),
                    status: OutcomeStatus::Ok,
                },
                TaskOutcome {
                    id: "flutter".to_string(),
                    label: "Flutter".to_string(),
                    status: OutcomeStatus::Warn,
                },
            ],
            ok_count: 1,
            warn_count: 1,
            fail_count: 0,
        };

        let entry = HistoryEntry::from_run_summary(
            123,
            456,
            "safe".to_string(),
            vec!["brew".to_string()],
            &summary,
        );

        assert_eq!(entry.summary.warn_count, 1);
        assert_eq!(entry.summary.overall_status(), OutcomeStatus::Warn);
        assert_eq!(
            entry.summary.outcome_labels,
            vec!["Homebrew:OK".to_string(), "Flutter:WARN".to_string()]
        );
    }

    fn history_entry(started_at_unix_secs: u64) -> HistoryEntry {
        HistoryEntry {
            started_at_unix_secs,
            duration_ms: 42,
            profile_id: "custom".to_string(),
            selected_tasks: vec!["brew".to_string()],
            summary: super::HistorySummary {
                ok_count: 1,
                warn_count: 0,
                fail_count: 0,
                outcome_labels: vec!["Homebrew:OK".to_string()],
            },
        }
    }
}
