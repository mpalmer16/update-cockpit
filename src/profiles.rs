use crate::runner::RunOptions;

pub const CUSTOM_PROFILE_ID: &str = "custom";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileDefinition {
    pub id: String,
    pub label: String,
    pub description: String,
    pub selected_tasks: Vec<String>,
    pub options: RunOptions,
}

impl ProfileDefinition {
    pub fn custom(selected_tasks: Vec<String>, options: RunOptions) -> Self {
        Self {
            id: CUSTOM_PROFILE_ID.to_string(),
            label: "Custom".to_string(),
            description: "Your current saved task selection and runtime flags.".to_string(),
            selected_tasks,
            options,
        }
    }
}

pub fn built_in_profiles() -> Vec<ProfileDefinition> {
    vec![
        ProfileDefinition {
            id: "full".to_string(),
            label: "Full".to_string(),
            description: "Run the full maintenance sweep across every registered task."
                .to_string(),
            selected_tasks: vec![
                "rust".to_string(),
                "julia".to_string(),
                "brew".to_string(),
                "flutter".to_string(),
                "node".to_string(),
                "sdkman".to_string(),
                "npm-tools".to_string(),
            ],
            options: RunOptions::default(),
        },
        ProfileDefinition {
            id: "safe".to_string(),
            label: "Safe".to_string(),
            description:
                "A conservative pass that avoids tasks marked dangerous while still refreshing core tooling."
                    .to_string(),
            selected_tasks: vec![
                "rust".to_string(),
                "julia".to_string(),
                "brew".to_string(),
                "node".to_string(),
                "sdkman".to_string(),
                "npm-tools".to_string(),
            ],
            options: RunOptions::default(),
        },
        ProfileDefinition {
            id: "toolchains".to_string(),
            label: "Toolchains".to_string(),
            description: "Focus on language runtimes and SDK managers.".to_string(),
            selected_tasks: vec![
                "rust".to_string(),
                "julia".to_string(),
                "flutter".to_string(),
                "node".to_string(),
                "sdkman".to_string(),
            ],
            options: RunOptions::default(),
        },
        ProfileDefinition {
            id: "package-managers".to_string(),
            label: "Package Managers".to_string(),
            description: "Refresh package managers and globally installed CLI tooling."
                .to_string(),
            selected_tasks: vec![
                "brew".to_string(),
                "node".to_string(),
                "npm-tools".to_string(),
            ],
            options: RunOptions::default(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::{CUSTOM_PROFILE_ID, ProfileDefinition, built_in_profiles};
    use crate::runner::RunOptions;

    #[test]
    fn includes_expected_built_in_profiles() {
        let ids: Vec<_> = built_in_profiles()
            .into_iter()
            .map(|profile| profile.id)
            .collect();
        assert_eq!(
            ids,
            vec![
                "full".to_string(),
                "safe".to_string(),
                "toolchains".to_string(),
                "package-managers".to_string()
            ]
        );
    }

    #[test]
    fn builds_custom_profile() {
        let profile = ProfileDefinition::custom(vec!["brew".to_string()], RunOptions::default());
        assert_eq!(profile.id, CUSTOM_PROFILE_ID);
        assert_eq!(profile.selected_tasks, vec!["brew".to_string()]);
    }
}
