use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TaskDefinition {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_true")]
    pub default_selected: bool,
    #[serde(default)]
    pub dangerous: bool,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub runner: TaskRunner,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskRunner {
    Script {
        path: PathBuf,
        #[serde(default = "default_shell")]
        shell: String,
        #[serde(default)]
        args: Vec<String>,
    },
    Command {
        program: String,
        #[serde(default)]
        args: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionPlan {
    pub tasks: Vec<TaskDefinition>,
}

#[derive(Debug, Clone)]
pub struct Catalog {
    tasks: BTreeMap<String, TaskDefinition>,
}

impl Catalog {
    pub fn load_from_root(root: &Path) -> Result<Self> {
        Self::load_from_tasks_dir(&root.join("tasks"))
    }

    pub fn load_from_tasks_dir(tasks_dir: &Path) -> Result<Self> {
        let mut tasks = BTreeMap::new();

        for entry in fs::read_dir(tasks_dir)
            .with_context(|| format!("failed to read task directory {}", tasks_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("toml") {
                continue;
            }

            let contents = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let task: TaskDefinition = toml::from_str(&contents)
                .with_context(|| format!("failed to parse {}", path.display()))?;

            if tasks.insert(task.id.clone(), task.clone()).is_some() {
                bail!("duplicate task id {}", task.id);
            }
        }

        if tasks.is_empty() {
            bail!("no task manifests found in {}", tasks_dir.display());
        }

        for task in tasks.values() {
            for dependency in &task.dependencies {
                if !tasks.contains_key(dependency) {
                    bail!(
                        "task {} references unknown dependency {}",
                        task.id,
                        dependency
                    );
                }
            }
        }

        Ok(Self { tasks })
    }

    pub fn tasks(&self) -> impl Iterator<Item = &TaskDefinition> {
        self.tasks.values()
    }

    pub fn plan(&self, all: bool, requested: &[String]) -> Result<ExecutionPlan> {
        let mut selected = BTreeSet::new();

        if all {
            selected.extend(self.tasks.keys().cloned());
        } else if requested.is_empty() {
            selected.extend(
                self.tasks
                    .values()
                    .filter(|task| task.default_selected)
                    .map(|task| task.id.clone()),
            );
        } else {
            for task_id in requested {
                if !self.tasks.contains_key(task_id) {
                    bail!("unknown task {task_id}");
                }
                selected.insert(task_id.clone());
            }
        }

        let mut ordered = Vec::new();
        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();

        for task_id in &selected {
            self.visit(task_id, &mut visiting, &mut visited, &mut ordered)?;
        }

        Ok(ExecutionPlan { tasks: ordered })
    }

    fn visit(
        &self,
        task_id: &str,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
        ordered: &mut Vec<TaskDefinition>,
    ) -> Result<()> {
        if visited.contains(task_id) {
            return Ok(());
        }

        if !visiting.insert(task_id.to_string()) {
            bail!("dependency cycle detected at task {task_id}");
        }

        let task = self
            .tasks
            .get(task_id)
            .with_context(|| format!("task {task_id} disappeared during planning"))?;

        for dependency in &task.dependencies {
            self.visit(dependency, visiting, visited, ordered)?;
        }

        visiting.remove(task_id);
        visited.insert(task_id.to_string());
        ordered.push(task.clone());
        Ok(())
    }
}

fn default_true() -> bool {
    true
}

fn default_shell() -> String {
    "zsh".to_string()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::Catalog;

    #[test]
    fn loads_task_manifests() {
        let root = TempDir::new().expect("tempdir");
        let tasks_dir = root.path().join("tasks");
        fs::create_dir(&tasks_dir).expect("create tasks dir");
        fs::write(
            tasks_dir.join("rust.toml"),
            r#"
                id = "rust"
                label = "Rust"

                [runner]
                kind = "command"
                program = "rustup"
                args = ["update"]
            "#,
        )
        .expect("write manifest");

        let catalog = Catalog::load_from_tasks_dir(&tasks_dir).expect("load catalog");
        let tasks: Vec<_> = catalog.tasks().collect();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "rust");
    }

    #[test]
    fn plans_default_tasks_with_dependencies() {
        let root = TempDir::new().expect("tempdir");
        let tasks_dir = root.path().join("tasks");
        fs::create_dir(&tasks_dir).expect("create tasks dir");
        fs::write(
            tasks_dir.join("base.toml"),
            r#"
                id = "base"
                label = "Base"
                default_selected = false

                [runner]
                kind = "command"
                program = "echo"
                args = ["base"]
            "#,
        )
        .expect("write base manifest");
        fs::write(
            tasks_dir.join("app.toml"),
            r#"
                id = "app"
                label = "App"
                dependencies = ["base"]

                [runner]
                kind = "command"
                program = "echo"
                args = ["app"]
            "#,
        )
        .expect("write app manifest");

        let catalog = Catalog::load_from_tasks_dir(&tasks_dir).expect("load catalog");
        let plan = catalog.plan(false, &[]).expect("plan");
        let ids: Vec<_> = plan.tasks.into_iter().map(|task| task.id).collect();
        assert_eq!(ids, vec!["base".to_string(), "app".to_string()]);
    }

    #[test]
    fn rejects_unknown_requested_task() {
        let root = TempDir::new().expect("tempdir");
        let tasks_dir = root.path().join("tasks");
        fs::create_dir(&tasks_dir).expect("create tasks dir");
        fs::write(
            tasks_dir.join("rust.toml"),
            r#"
                id = "rust"
                label = "Rust"

                [runner]
                kind = "command"
                program = "rustup"
                args = ["update"]
            "#,
        )
        .expect("write manifest");

        let catalog = Catalog::load_from_tasks_dir(&tasks_dir).expect("load catalog");
        let error = catalog
            .plan(false, &[String::from("missing")])
            .expect_err("should fail");
        assert!(error.to_string().contains("unknown task"));
    }

    #[test]
    fn detects_dependency_cycle() {
        let root = TempDir::new().expect("tempdir");
        let tasks_dir = root.path().join("tasks");
        fs::create_dir(&tasks_dir).expect("create tasks dir");
        fs::write(
            tasks_dir.join("a.toml"),
            r#"
                id = "a"
                label = "Task A"
                dependencies = ["b"]

                [runner]
                kind = "command"
                program = "echo"
                args = ["a"]
            "#,
        )
        .expect("write task a");
        fs::write(
            tasks_dir.join("b.toml"),
            r#"
                id = "b"
                label = "Task B"
                default_selected = false
                dependencies = ["a"]

                [runner]
                kind = "command"
                program = "echo"
                args = ["b"]
            "#,
        )
        .expect("write task b");

        let catalog = Catalog::load_from_tasks_dir(&tasks_dir).expect("load catalog");
        let error = catalog
            .plan(false, &[String::from("a")])
            .expect_err("should fail");
        assert!(error.to_string().contains("dependency cycle"));
    }

    #[test]
    fn rejects_duplicate_ids() {
        let root = TempDir::new().expect("tempdir");
        let tasks_dir = root.path().join("tasks");
        fs::create_dir(&tasks_dir).expect("create tasks dir");
        fs::write(
            tasks_dir.join("rust-a.toml"),
            r#"
                id = "rust"
                label = "Rust A"

                [runner]
                kind = "command"
                program = "echo"
                args = ["a"]
            "#,
        )
        .expect("write first manifest");
        fs::write(
            tasks_dir.join("rust-b.toml"),
            r#"
                id = "rust"
                label = "Rust B"

                [runner]
                kind = "command"
                program = "echo"
                args = ["b"]
            "#,
        )
        .expect("write second manifest");

        let error = Catalog::load_from_tasks_dir(&tasks_dir).expect_err("should fail");
        assert!(error.to_string().contains("duplicate task id"));
    }

    #[test]
    fn rejects_unknown_dependencies() {
        let root = TempDir::new().expect("tempdir");
        let tasks_dir = root.path().join("tasks");
        fs::create_dir(&tasks_dir).expect("create tasks dir");
        fs::write(
            tasks_dir.join("rust.toml"),
            r#"
                id = "rust"
                label = "Rust"
                dependencies = ["missing"]

                [runner]
                kind = "command"
                program = "echo"
                args = ["rust"]
            "#,
        )
        .expect("write manifest");

        let error = Catalog::load_from_tasks_dir(&tasks_dir).expect_err("should fail");
        assert!(error.to_string().contains("unknown dependency"));
    }
}
