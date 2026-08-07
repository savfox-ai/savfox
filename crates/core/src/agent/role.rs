//! Applies built-in agent role files as configuration layers.
//!
//! Role files use the same keys as `config.toml`. Their config portion is
//! inserted at session-flag precedence and then resolved through the normal
//! config loader. Runtime choices inherited from the caller remain sticky
//! unless the role file explicitly overrides them.

use std::sync::LazyLock;

use savfox_app_server_protocol::ConfigLayerSource;
use serde::{Deserialize, Serialize};
use toml::Value as TomlValue;

use crate::config::{Config, ConfigOverrides, ConfigToml};
use crate::config_loader::ConfigLayerEntry;

const ALL_ROLES: [AgentRole; 3] = [
    AgentRole::Default,
    AgentRole::Explorer,
    AgentRole::Worker,
    // TODO(jif) expose when the orchestrator role is stable.
    // AgentRole::Orchestrator,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    Default,
    Orchestrator,
    Worker,
    Explorer,
}

#[derive(Debug)]
struct RoleDefinition {
    description: String,
    config: TomlValue,
}

impl AgentRole {
    /// Returns role declarations used in the spawn tool schema.
    pub fn enum_values() -> Vec<String> {
        ALL_ROLES
            .iter()
            .filter_map(|role| {
                let role_name = serde_json::to_string(role).ok()?;
                let description = serde_json::to_string(&role.definition().description).ok()?;
                Some(format!(
                    r#"{{ "name": {role_name}, "description": {description} }}"#
                ))
            })
            .collect()
    }

    /// Applies this role's embedded TOML file through the regular config layer
    /// machinery.
    pub fn apply_to_config(self, config: &mut Config) -> Result<(), String> {
        let role_config = self.definition().config.clone();
        if role_config.as_table().is_none_or(toml::map::Map::is_empty) {
            return Ok(());
        }

        let next_stack = config
            .config_layer_stack
            .with_layer(ConfigLayerEntry::new(
                ConfigLayerSource::SessionFlags,
                role_config.clone(),
            ))
            .map_err(|err| format!("agent role config layer is invalid: {err}"))?;
        let effective_config: ConfigToml = next_stack
            .effective_config()
            .try_into()
            .map_err(|err| format!("agent role config is invalid: {err}"))?;

        let current = config.clone();
        let has = |key: &str| role_config.get(key).is_some();
        let overrides = ConfigOverrides {
            model: (!has("model")).then(|| current.model.clone()).flatten(),
            review_model: (!has("review_model"))
                .then(|| current.review_model.clone())
                .flatten(),
            cwd: Some(current.cwd.clone()),
            approval_policy: None,
            sandbox_mode: None,
            model_provider: (!has("model_provider")).then(|| current.model_provider_id.clone()),
            savfox_linux_sandbox_exe: current.savfox_linux_sandbox_exe.clone(),
            base_instructions: (!has("instructions") && !has("model_instructions_file"))
                .then(|| current.base_instructions.clone())
                .flatten(),
            developer_instructions: (!has("developer_instructions"))
                .then(|| current.developer_instructions.clone())
                .flatten(),
            personality: (!has("personality"))
                .then_some(current.personality)
                .flatten(),
            compact_prompt: (!has("compact_prompt") && !has("experimental_compact_prompt_file"))
                .then(|| current.compact_prompt.clone())
                .flatten(),
            include_apply_patch_tool: Some(current.include_apply_patch_tool),
            show_raw_agent_reasoning: (!has("show_raw_agent_reasoning"))
                .then_some(current.show_raw_agent_reasoning),
            tools_web_search_request: None,
            ephemeral: Some(current.ephemeral),
            additional_writable_roots: Vec::new(),
        };

        let mut next = Config::load_config_with_layer_stack(
            effective_config,
            overrides,
            current.savfox_home.clone(),
            next_stack,
        )
        .map_err(|err| format!("failed to apply agent role config: {err}"))?;

        if !has("model_reasoning_effort") {
            next.model_reasoning_effort = current.model_reasoning_effort;
        }
        if !has("approval_policy") {
            next.approval_policy = current.approval_policy;
            next.did_user_set_custom_approval_policy_or_sandbox_mode =
                current.did_user_set_custom_approval_policy_or_sandbox_mode;
        }
        if !has("sandbox_mode") && !has("sandbox_workspace_write") {
            next.sandbox_policy = current.sandbox_policy;
            next.forced_auto_mode_downgraded_on_windows =
                current.forced_auto_mode_downgraded_on_windows;
        }
        if !has("model_reasoning_summary") {
            next.model_reasoning_summary = current.model_reasoning_summary;
        }
        if !has("model_supports_reasoning_summaries") {
            next.model_supports_reasoning_summaries = current.model_supports_reasoning_summaries;
        }
        if !has("model_verbosity") {
            next.model_verbosity = current.model_verbosity;
        }
        if !has("shell_environment_policy") {
            next.shell_environment_policy = current.shell_environment_policy;
        }
        if !has("features") {
            next.features = current.features;
            next.use_experimental_unified_exec_tool = current.use_experimental_unified_exec_tool;
        }
        if !has("web_search") {
            next.web_search_mode = current.web_search_mode;
        }
        next.tool_access_policy = current.tool_access_policy;
        next.user_instructions = current.user_instructions;

        *config = next;
        Ok(())
    }

    fn definition(self) -> &'static RoleDefinition {
        static DEFAULT: LazyLock<RoleDefinition> =
            LazyLock::new(|| parse_role_file("default", include_str!("roles/default.toml")));
        static ORCHESTRATOR: LazyLock<RoleDefinition> = LazyLock::new(|| {
            parse_role_file("orchestrator", include_str!("roles/orchestrator.toml"))
        });
        static WORKER: LazyLock<RoleDefinition> =
            LazyLock::new(|| parse_role_file("worker", include_str!("roles/worker.toml")));
        static EXPLORER: LazyLock<RoleDefinition> =
            LazyLock::new(|| parse_role_file("explorer", include_str!("roles/explorer.toml")));

        match self {
            Self::Default => &DEFAULT,
            Self::Orchestrator => &ORCHESTRATOR,
            Self::Worker => &WORKER,
            Self::Explorer => &EXPLORER,
        }
    }
}

fn parse_role_file(expected_name: &str, contents: &str) -> RoleDefinition {
    let mut value: TomlValue = toml::from_str(contents)
        .unwrap_or_else(|err| panic!("built-in agent role {expected_name} is invalid TOML: {err}"));
    let table = value
        .as_table_mut()
        .unwrap_or_else(|| panic!("built-in agent role {expected_name} must be a TOML table"));
    let name = table
        .remove("name")
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("built-in agent role {expected_name} is missing name"));
    assert_eq!(name, expected_name, "built-in agent role name mismatch");
    let description = table
        .remove("description")
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("built-in agent role {expected_name} is missing description"));

    let _: ConfigToml = value.clone().try_into().unwrap_or_else(|err| {
        panic!("built-in agent role {expected_name} has invalid config: {err}")
    });
    RoleDefinition {
        description,
        config: value,
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use savfox_protocol::openai_models::ReasoningEffort;

    use super::*;
    use crate::config::test_config;

    #[test]
    fn role_files_are_valid_and_descriptions_drive_the_schema() {
        for role in [
            AgentRole::Default,
            AgentRole::Orchestrator,
            AgentRole::Worker,
            AgentRole::Explorer,
        ] {
            assert!(!role.definition().description.trim().is_empty());
        }
        let values = AgentRole::enum_values().join("\n");
        assert!(values.contains("explorer"));
        assert!(values.contains("Use for execution and production work"));
    }

    #[test]
    fn explorer_role_uses_a_config_layer_and_preserves_runtime_model() {
        let mut config = test_config();
        config.model = Some("provider/runtime-model".to_owned());
        config.base_instructions = Some("runtime instructions".to_owned());
        config.model_reasoning_effort = Some(ReasoningEffort::High);

        AgentRole::Explorer
            .apply_to_config(&mut config)
            .expect("apply explorer role");

        assert_eq!(config.model.as_deref(), Some("provider/runtime-model"));
        assert_eq!(
            config.base_instructions.as_deref(),
            Some("runtime instructions")
        );
        assert_eq!(config.model_reasoning_effort, Some(ReasoningEffort::Medium));
        assert_eq!(
            config
                .config_layer_stack
                .effective_config()
                .get("model_reasoning_effort")
                .and_then(TomlValue::as_str),
            Some("medium")
        );
    }

    #[test]
    fn orchestrator_instructions_come_from_its_role_file() {
        let mut config = test_config();
        config.base_instructions = Some("parent instructions".to_owned());

        AgentRole::Orchestrator
            .apply_to_config(&mut config)
            .expect("apply orchestrator role");

        let instructions = config
            .base_instructions
            .as_deref()
            .expect("orchestrator instructions");
        assert!(instructions.starts_with("You are Savfox, a coding agent based on GPT-5."));
        assert_ne!(instructions, "parent instructions");
    }
}
