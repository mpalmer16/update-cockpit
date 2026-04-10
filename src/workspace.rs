use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

pub fn discover_root(explicit_root: Option<&Path>) -> Result<PathBuf> {
    if let Some(root) = explicit_root {
        return validate_root(root);
    }

    let current_dir = env::current_dir()?;
    if let Some(root) = find_root(&current_dir) {
        return Ok(root);
    }

    let executable = env::current_exe()?;
    if let Some(root) = executable.parent().and_then(find_root) {
        return Ok(root);
    }

    bail!("could not locate a project root containing a tasks directory")
}

fn validate_root(root: &Path) -> Result<PathBuf> {
    if root.join("tasks").is_dir() {
        Ok(root.to_path_buf())
    } else {
        bail!("{} does not contain a tasks directory", root.display())
    }
}

fn find_root(start: &Path) -> Option<PathBuf> {
    for ancestor in start.ancestors() {
        if ancestor.join("tasks").is_dir() {
            return Some(ancestor.to_path_buf());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::discover_root;

    #[test]
    fn accepts_explicit_root() {
        let root = TempDir::new().expect("tempdir");
        fs::create_dir(root.path().join("tasks")).expect("create tasks dir");
        let discovered = discover_root(Some(root.path())).expect("discover root");
        assert_eq!(discovered, root.path());
    }

    #[test]
    fn rejects_root_without_tasks_dir() {
        let root = TempDir::new().expect("tempdir");
        let error = discover_root(Some(root.path())).expect_err("should fail");
        assert!(error.to_string().contains("tasks directory"));
    }
}
