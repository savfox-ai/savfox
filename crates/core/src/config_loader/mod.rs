mod cloud_requirements;
mod config_requirements;
mod diagnostics;
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod macos;
mod requirements_exec_policy;
mod state;

#[cfg(test)]
mod tests;

use std::io;
use std::path::Path;

pub use cloud_requirements::CloudRequirementsLoader;
pub use config_requirements::{
    ConfigRequirements, ConfigRequirementsToml, McpServerIdentity, McpServerRequirement,
    RequirementSource, ResidencyRequirement, SandboxModeRequirement, Sourced,
};
pub use diagnostics::{
    ConfigError, ConfigLoadError, TextPosition, TextRange, format_config_error,
    format_config_error_with_source,
};
pub(crate) use diagnostics::{
    config_error_from_toml, first_layer_config_error, first_layer_config_error_from_entries,
    io_error_from_config_error,
};
use dunce::canonicalize as normalize_path;
use savfox_app_server_protocol::ConfigLayerSource;
pub(crate) use savfox_config::build_cli_overrides_layer;
pub use savfox_config::merge_toml_values;
use savfox_protocol::config_types::TrustLevel;
use savfox_utils::absolute_path::{AbsolutePathBuf, AbsolutePathBufGuard};
pub use state::{ConfigLayerEntry, ConfigLayerStack, ConfigLayerStackOrdering, LoaderOverrides};
use toml::Value as TomlValue;

use crate::config::{
    CONFIG_JSON_FILE, CONFIG_TOML_FILE, CONFIG_YAML_FILE, CONFIG_YML_FILE, ConfigToml,
    deserialize_config_toml_with_base,
};
use crate::config_loader::config_requirements::ConfigRequirementsWithSources;
use crate::git_info::resolve_root_git_project_for_trust;

/// On Unix systems, load requirements from this file path, if present.
const DEFAULT_REQUIREMENTS_TOML_FILE_UNIX: &str = "/etc/savfox/requirements.toml";

/// On Unix systems, load default settings from this file path, if present.
/// Note that /etc/savfox/ is treated as a "config folder," so subfolders such
/// as skills/ and rules/ will also be honored.
pub const SYSTEM_CONFIG_TOML_FILE_UNIX: &str = "/etc/savfox/config.toml";
const DEFAULT_PROJECT_ROOT_MARKERS: &[&str] = &[".git"];

async fn resolve_preferred_config_file(
    default_toml_file: &AbsolutePathBuf,
) -> io::Result<AbsolutePathBuf> {
    let default_parent = default_toml_file.as_path().parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "config path {} has no parent",
                default_toml_file.as_path().display()
            ),
        )
    })?;

    let candidates = [
        default_toml_file.as_path().to_path_buf(),
        default_parent.join(CONFIG_JSON_FILE),
        default_parent.join(CONFIG_YAML_FILE),
        default_parent.join(CONFIG_YML_FILE),
    ];

    for candidate in candidates {
        match tokio::fs::metadata(&candidate).await {
            Ok(meta) if meta.is_file() => return AbsolutePathBuf::from_absolute_path(candidate),
            Ok(_) => continue,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => {
                return Err(io::Error::new(
                    err.kind(),
                    format!(
                        "Failed to read config metadata {}: {err}",
                        candidate.display()
                    ),
                ));
            }
        }
    }

    Ok(default_toml_file.clone())
}

/// To build up the set of admin-enforced constraints, we build up from multiple
/// configuration layers in the following order, but a constraint defined in an
/// earlier layer cannot be overridden by a later layer:
///
/// - admin:    managed preferences (*)
/// - cloud:    managed cloud requirements
/// - system    `/etc/savfox/requirements.toml`
///
/// For backwards compatibility, we also load from
/// `/etc/savfox/managed_config.toml` and map it to
/// `/etc/savfox/requirements.toml`.
///
/// Configuration is built up from multiple layers in the following order:
///
/// - admin:    managed preferences (*)
/// - system    `/etc/savfox/config.toml`
/// - user      `${SAVFOX_HOME}/config.toml`
/// - cwd       `${PWD}/config.toml` (loaded but disabled when the directory is untrusted)
/// - tree      parent directories up to root looking for `./.savfox/config.toml` (loaded but
///   disabled when untrusted)
/// - repo      `$(git rev-parse --show-toplevel)/.savfox/config.toml` (loaded but disabled when
///   untrusted)
/// - runtime   e.g., --config flags, model selector in UI
///
/// (*) Only available on macOS via managed device profiles.
///
/// See https://developers.openai.com/savfox/security for details.
///
/// When loading the config stack for a session, there should be a `cwd`
/// associated with it such that `cwd` should be `Some(...)`. Only for
/// session-agnostic config loading (e.g., for the app server's `/config`
/// endpoint) should `cwd` be `None`.
pub async fn load_config_layers_state(
    savfox_home: &Path,
    cwd: Option<AbsolutePathBuf>,
    cli_overrides: &[(String, TomlValue)],
    overrides: LoaderOverrides,
    cloud_requirements: CloudRequirementsLoader,
) -> io::Result<ConfigLayerStack> {
    let mut config_requirements_toml = ConfigRequirementsWithSources::default();

    #[cfg(target_os = "macos")]
    macos::load_managed_admin_requirements_toml(
        &mut config_requirements_toml,
        overrides
            .macos_managed_config_requirements_base64
            .as_deref(),
    )
    .await?;

    if let Some(requirements) = cloud_requirements.get().await {
        config_requirements_toml
            .merge_unset_fields(RequirementSource::CloudRequirements, requirements);
    }

    // Honor /etc/savfox/requirements.toml.
    if cfg!(unix) {
        load_requirements_toml(
            &mut config_requirements_toml,
            DEFAULT_REQUIREMENTS_TOML_FILE_UNIX,
        )
        .await?;
    }

    let _ = &overrides;

    let mut layers = Vec::<ConfigLayerEntry>::new();

    let cli_overrides_layer = if cli_overrides.is_empty() {
        None
    } else {
        Some(build_cli_overrides_layer(cli_overrides))
    };

    // Include an entry for the "system" config folder, loading its config.toml,
    // if it exists.
    let system_config_toml_file = if cfg!(unix) {
        Some(AbsolutePathBuf::from_absolute_path(
            SYSTEM_CONFIG_TOML_FILE_UNIX,
        )?)
    } else {
        // TODO(gt): Determine the path to load on Windows.
        None
    };
    if let Some(system_config_toml_file) = system_config_toml_file {
        let system_config_file = resolve_preferred_config_file(&system_config_toml_file).await?;
        let system_layer =
            load_config_toml_for_required_layer(&system_config_file, |config_toml| {
                ConfigLayerEntry::new(
                    ConfigLayerSource::System {
                        file: system_config_file.clone(),
                    },
                    config_toml,
                )
            })
            .await?;
        layers.push(system_layer);
    }

    // Add a layer for $SAVFOX_HOME/config.toml if it exists. Note if the file
    // exists, but is malformed, then this error should be propagated to the
    // user.
    let user_toml_file = AbsolutePathBuf::resolve_path_against_base(CONFIG_TOML_FILE, savfox_home)?;
    let user_file = resolve_preferred_config_file(&user_toml_file).await?;
    let user_layer = load_config_toml_for_required_layer(&user_file, |config_toml| {
        ConfigLayerEntry::new(
            ConfigLayerSource::User {
                file: user_file.clone(),
            },
            config_toml,
        )
    })
    .await?;
    layers.push(user_layer);

    if let Some(cwd) = cwd {
        let mut merged_so_far = TomlValue::Table(toml::map::Map::new());
        for layer in &layers {
            merge_toml_values(&mut merged_so_far, &layer.config);
        }
        if let Some(cli_overrides_layer) = cli_overrides_layer.as_ref() {
            merge_toml_values(&mut merged_so_far, cli_overrides_layer);
        }

        let project_root_markers = match project_root_markers_from_config(&merged_so_far) {
            Ok(markers) => markers.unwrap_or_else(default_project_root_markers),
            Err(err) => {
                if let Some(config_error) = first_layer_config_error_from_entries(&layers).await {
                    return Err(io_error_from_config_error(
                        io::ErrorKind::InvalidData,
                        config_error,
                        None,
                    ));
                }
                return Err(err);
            }
        };
        let project_trust_context = match project_trust_context(
            &merged_so_far,
            &cwd,
            &project_root_markers,
            savfox_home,
            &user_file,
        )
        .await
        {
            Ok(context) => context,
            Err(err) => {
                let source = err
                    .get_ref()
                    .and_then(|err| err.downcast_ref::<toml::de::Error>())
                    .cloned();
                if let Some(config_error) = first_layer_config_error_from_entries(&layers).await {
                    return Err(io_error_from_config_error(
                        io::ErrorKind::InvalidData,
                        config_error,
                        source,
                    ));
                }
                return Err(err);
            }
        };
        let project_layers = load_project_layers(
            &cwd,
            &project_trust_context.project_root,
            &project_trust_context,
            savfox_home,
        )
        .await?;
        layers.extend(project_layers);
    }

    // Add a layer for runtime overrides from the CLI or UI, if any exist.
    if let Some(cli_overrides_layer) = cli_overrides_layer {
        layers.push(ConfigLayerEntry::new(
            ConfigLayerSource::SessionFlags,
            cli_overrides_layer,
        ));
    }

    ConfigLayerStack::new(
        layers,
        config_requirements_toml.clone().try_into()?,
        config_requirements_toml.into_toml(),
    )
}

/// Attempts to load a config file from `config_file`.
/// Supports JSON (`.json`), TOML (`.toml`), and YAML (`.yaml` / `.yml`).
/// - If the file exists and is valid, passes the parsed `toml::Value` to `create_entry` and returns
///   the resulting layer entry.
/// - If the file does not exist, uses an empty `Table` with `create_entry` and returns the
///   resulting layer entry.
/// - If there is an error reading the file or parsing the content, returns an error.
fn parse_config_file_content(config_file: &Path, contents: &str) -> io::Result<TomlValue> {
    match config_file.extension().and_then(|ext| ext.to_str()) {
        Some("json") => {
            let json_value: serde_json::Value = serde_json::from_str(contents).map_err(|err| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "Error parsing JSON config file {}: {err}",
                        config_file.display()
                    ),
                )
            })?;
            Ok(json_to_toml_value(&json_value))
        }
        Some("yaml" | "yml") => {
            let yaml_value: serde_yaml::Value = serde_yaml::from_str(contents).map_err(|err| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "Error parsing YAML config file {}: {err}",
                        config_file.display()
                    ),
                )
            })?;
            let json_value = serde_json::to_value(yaml_value).map_err(|err| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "Error converting YAML config file {} to JSON: {err}",
                        config_file.display()
                    ),
                )
            })?;
            Ok(json_to_toml_value(&json_value))
        }
        _ => {
            let config: TomlValue = toml::from_str(contents).map_err(|err| {
                let config_error = config_error_from_toml(config_file, contents, err.clone());
                io_error_from_config_error(io::ErrorKind::InvalidData, config_error, Some(err))
            })?;
            Ok(config)
        }
    }
}

fn json_to_toml_value(value: &serde_json::Value) -> TomlValue {
    match value {
        serde_json::Value::Null => TomlValue::String(String::new()),
        serde_json::Value::Bool(value) => TomlValue::Boolean(*value),
        serde_json::Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                TomlValue::Integer(value)
            } else if let Some(value) = number.as_u64() {
                if value > i64::MAX as u64 {
                    TomlValue::String(value.to_string())
                } else {
                    TomlValue::Integer(value as i64)
                }
            } else if let Some(value) = number.as_f64() {
                TomlValue::Float(value)
            } else {
                TomlValue::String(number.to_string())
            }
        }
        serde_json::Value::String(value) => TomlValue::String(value.clone()),
        serde_json::Value::Array(values) => {
            TomlValue::Array(values.iter().map(json_to_toml_value).collect())
        }
        serde_json::Value::Object(map) => {
            let mut table = toml::map::Map::new();
            for (key, value) in map {
                table.insert(key.clone(), json_to_toml_value(value));
            }
            TomlValue::Table(table)
        }
    }
}

async fn load_config_toml_for_required_layer(
    config_file: impl AsRef<Path>,
    create_entry: impl FnOnce(TomlValue) -> ConfigLayerEntry,
) -> io::Result<ConfigLayerEntry> {
    let config_file = config_file.as_ref();
    let config_value = match tokio::fs::read_to_string(config_file).await {
        Ok(contents) => {
            let config = parse_config_file_content(config_file, &contents)?;
            let config_parent = config_file.parent().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "Config file {} has no parent directory",
                        config_file.display()
                    ),
                )
            })?;
            resolve_relative_paths_in_config_toml(config, config_parent)
        }
        Err(e) => {
            if e.kind() == io::ErrorKind::NotFound {
                Ok(TomlValue::Table(toml::map::Map::new()))
            } else {
                Err(io::Error::new(
                    e.kind(),
                    format!("Failed to read config file {}: {e}", config_file.display()),
                ))
            }
        }
    }?;

    Ok(create_entry(config_value))
}

/// If available, apply requirements from `/etc/savfox/requirements.toml` to
/// `config_requirements_toml` by filling in any unset fields.
async fn load_requirements_toml(
    config_requirements_toml: &mut ConfigRequirementsWithSources,
    requirements_toml_file: impl AsRef<Path>,
) -> io::Result<()> {
    let requirements_toml_file =
        AbsolutePathBuf::from_absolute_path(requirements_toml_file.as_ref())?;
    match tokio::fs::read_to_string(&requirements_toml_file).await {
        Ok(contents) => {
            let requirements_config: ConfigRequirementsToml =
                toml::from_str(&contents).map_err(|e| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "Error parsing requirements file {}: {e}",
                            requirements_toml_file.as_ref().display(),
                        ),
                    )
                })?;
            config_requirements_toml.merge_unset_fields(
                RequirementSource::SystemRequirementsToml {
                    file: requirements_toml_file.clone(),
                },
                requirements_config,
            );
        }
        Err(e) => {
            if e.kind() != io::ErrorKind::NotFound {
                return Err(io::Error::new(
                    e.kind(),
                    format!(
                        "Failed to read requirements file {}: {e}",
                        requirements_toml_file.as_ref().display(),
                    ),
                ));
            }
        }
    }

    Ok(())
}

/// Reads `project_root_markers` from the [toml::Value] produced by merging
/// `config.toml` from the config layers in the stack preceding
/// [ConfigLayerSource::Project].
///
/// Invariants:
/// - If `project_root_markers` is not specified, returns `Ok(None)`.
/// - If `project_root_markers` is specified, returns `Ok(Some(markers))` where `markers` is a
///   `Vec<String>` (including `Ok(Some(Vec::new()))` for an empty array, which indicates that root
///   detection should be disabled).
/// - Returns an error if `project_root_markers` is specified but is not an array of strings.
pub(crate) fn project_root_markers_from_config(
    config: &TomlValue,
) -> io::Result<Option<Vec<String>>> {
    let Some(table) = config.as_table() else {
        return Ok(None);
    };
    let Some(markers_value) = table.get("project_root_markers") else {
        return Ok(None);
    };
    let TomlValue::Array(entries) = markers_value else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "project_root_markers must be an array of strings",
        ));
    };
    if entries.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let mut markers = Vec::new();
    for entry in entries {
        let Some(marker) = entry.as_str() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "project_root_markers must be an array of strings",
            ));
        };
        markers.push(marker.to_owned());
    }
    Ok(Some(markers))
}

pub(crate) fn default_project_root_markers() -> Vec<String> {
    DEFAULT_PROJECT_ROOT_MARKERS
        .iter()
        .map(ToString::to_string)
        .collect()
}

struct ProjectTrustContext {
    project_root: AbsolutePathBuf,
    project_root_key: String,
    repo_root_key: Option<String>,
    projects_trust: std::collections::HashMap<String, TrustLevel>,
    user_config_file: AbsolutePathBuf,
}

struct ProjectTrustDecision {
    trust_level: Option<TrustLevel>,
    trust_key: String,
}

impl ProjectTrustDecision {
    fn is_trusted(&self) -> bool {
        matches!(self.trust_level, Some(TrustLevel::Trusted))
    }
}

impl ProjectTrustContext {
    fn decision_for_dir(&self, dir: &AbsolutePathBuf) -> ProjectTrustDecision {
        let dir_key = dir.as_path().to_string_lossy().to_string();
        if let Some(trust_level) = self.projects_trust.get(&dir_key).copied() {
            return ProjectTrustDecision {
                trust_level: Some(trust_level),
                trust_key: dir_key,
            };
        }

        if let Some(trust_level) = self.projects_trust.get(&self.project_root_key).copied() {
            return ProjectTrustDecision {
                trust_level: Some(trust_level),
                trust_key: self.project_root_key.clone(),
            };
        }

        if let Some(repo_root_key) = self.repo_root_key.as_ref()
            && let Some(trust_level) = self.projects_trust.get(repo_root_key).copied()
        {
            return ProjectTrustDecision {
                trust_level: Some(trust_level),
                trust_key: repo_root_key.clone(),
            };
        }

        ProjectTrustDecision {
            trust_level: None,
            trust_key: self
                .repo_root_key
                .clone()
                .unwrap_or_else(|| self.project_root_key.clone()),
        }
    }

    fn disabled_reason_for_dir(&self, dir: &AbsolutePathBuf) -> Option<String> {
        let decision = self.decision_for_dir(dir);
        if decision.is_trusted() {
            return None;
        }

        let trust_key = decision.trust_key.as_str();
        let user_config_file = self.user_config_file.as_path().display();
        match decision.trust_level {
            Some(TrustLevel::Untrusted) => Some(format!(
                "{trust_key} is marked as untrusted in {user_config_file}. To load config.toml, mark it trusted."
            )),
            _ => Some(format!(
                "To load config.toml, add {trust_key} as a trusted project in {user_config_file}."
            )),
        }
    }
}

fn project_layer_entry(
    trust_context: &ProjectTrustContext,
    dot_savfox_folder: &AbsolutePathBuf,
    layer_dir: &AbsolutePathBuf,
    config: TomlValue,
    config_toml_exists: bool,
) -> ConfigLayerEntry {
    let source = ConfigLayerSource::Project {
        dot_savfox_folder: dot_savfox_folder.clone(),
    };

    if config_toml_exists && let Some(reason) = trust_context.disabled_reason_for_dir(layer_dir) {
        ConfigLayerEntry::new_disabled(source, config, reason)
    } else {
        ConfigLayerEntry::new(source, config)
    }
}

async fn project_trust_context(
    merged_config: &TomlValue,
    cwd: &AbsolutePathBuf,
    project_root_markers: &[String],
    config_base_dir: &Path,
    user_config_file: &AbsolutePathBuf,
) -> io::Result<ProjectTrustContext> {
    let config_toml = deserialize_config_toml_with_base(merged_config.clone(), config_base_dir)?;

    let project_root = find_project_root(cwd, project_root_markers).await?;
    let projects = config_toml.projects.unwrap_or_default();

    let project_root_key = project_root.as_path().to_string_lossy().to_string();
    let repo_root = resolve_root_git_project_for_trust(cwd.as_path());
    let repo_root_key = repo_root
        .as_ref()
        .map(|root| root.to_string_lossy().to_string());

    let projects_trust = projects
        .into_iter()
        .filter_map(|(key, project)| project.trust_level.map(|trust_level| (key, trust_level)))
        .collect();

    Ok(ProjectTrustContext {
        project_root,
        project_root_key,
        repo_root_key,
        projects_trust,
        user_config_file: user_config_file.clone(),
    })
}

/// Takes a `toml::Value` parsed from a config.toml file and walks through it,
/// resolving any `AbsolutePathBuf` fields against `base_dir`, returning a new
/// `toml::Value` with the same shape but with paths resolved.
///
/// This ensures that multiple config layers can be merged together correctly
/// even if they were loaded from different directories.
fn resolve_relative_paths_in_config_toml(
    mut value_from_config_toml: TomlValue,
    base_dir: &Path,
) -> io::Result<TomlValue> {
    substitute_env_vars_in_toml_value(&mut value_from_config_toml)?;

    // Use the serialize/deserialize round-trip to convert the
    // `toml::Value` into a `ConfigToml` with `AbsolutePath
    let _guard = AbsolutePathBufGuard::new(base_dir);
    let Ok(resolved) = value_from_config_toml.clone().try_into::<ConfigToml>() else {
        return Ok(value_from_config_toml);
    };
    drop(_guard);

    let resolved_value = TomlValue::try_from(resolved).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Failed to serialize resolved config: {e}"),
        )
    })?;

    Ok(copy_shape_from_original(
        &value_from_config_toml,
        &resolved_value,
    ))
}

fn substitute_env_vars_in_toml_value(value: &mut TomlValue) -> io::Result<()> {
    match value {
        TomlValue::String(text) => {
            *text = expand_env_placeholders(text)?;
        }
        TomlValue::Array(items) => {
            for item in items {
                substitute_env_vars_in_toml_value(item)?;
            }
        }
        TomlValue::Table(table) => {
            for (_, item) in table.iter_mut() {
                substitute_env_vars_in_toml_value(item)?;
            }
        }
        _ => {}
    }

    Ok(())
}

fn expand_env_placeholders(input: &str) -> io::Result<String> {
    let mut output = String::new();
    let mut cursor = 0usize;

    while let Some(relative_start) = input[cursor..].find("${") {
        let start = cursor + relative_start;
        output.push_str(&input[cursor..start]);

        let expr_start = start + 2;
        let Some(relative_end) = input[expr_start..].find('}') else {
            output.push_str(&input[start..]);
            return Ok(output);
        };
        let end = expr_start + relative_end;
        let expr = &input[expr_start..end];
        output.push_str(&resolve_env_expr(expr)?);
        cursor = end + 1;
    }

    output.push_str(&input[cursor..]);
    Ok(output)
}

fn resolve_env_expr(expr: &str) -> io::Result<String> {
    if let Some((name, message)) = expr.split_once(":?") {
        let name = name.trim();
        if name.is_empty() {
            return Ok(String::new());
        }
        match std::env::var(name) {
            Ok(value) if !value.is_empty() => Ok(value),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                if message.trim().is_empty() {
                    format!("required environment variable '{name}' is missing")
                } else {
                    message.trim().to_owned()
                },
            )),
        }
    } else if let Some((name, default)) = expr.split_once(":-") {
        let name = name.trim();
        if name.is_empty() {
            return Ok(default.to_owned());
        }
        match std::env::var(name) {
            Ok(value) if !value.is_empty() => Ok(value),
            _ => Ok(default.to_owned()),
        }
    } else {
        let name = expr.trim();
        if name.is_empty() {
            return Ok(String::new());
        }
        Ok(std::env::var(name).unwrap_or_default())
    }
}

/// Ensure that every field in `original` is present in the returned
/// `toml::Value`, taking the value from `resolved` where possible. This ensures
/// the fields that we "removed" during the serialize/deserialize round-trip in
/// `resolve_config_paths` are preserved, out of an abundance of caution.
fn copy_shape_from_original(original: &TomlValue, resolved: &TomlValue) -> TomlValue {
    match (original, resolved) {
        (TomlValue::Table(original_table), TomlValue::Table(resolved_table)) => {
            let mut table = toml::map::Map::new();
            for (key, original_value) in original_table {
                let resolved_value = resolved_table.get(key).unwrap_or(original_value);
                table.insert(
                    key.clone(),
                    copy_shape_from_original(original_value, resolved_value),
                );
            }
            TomlValue::Table(table)
        }
        (TomlValue::Array(original_array), TomlValue::Array(resolved_array)) => {
            let mut items = Vec::new();
            for (index, original_value) in original_array.iter().enumerate() {
                let resolved_value = resolved_array.get(index).unwrap_or(original_value);
                items.push(copy_shape_from_original(original_value, resolved_value));
            }
            TomlValue::Array(items)
        }
        (_, resolved_value) => resolved_value.clone(),
    }
}

async fn find_project_root(
    cwd: &AbsolutePathBuf,
    project_root_markers: &[String],
) -> io::Result<AbsolutePathBuf> {
    if project_root_markers.is_empty() {
        return Ok(cwd.clone());
    }

    for ancestor in cwd.as_path().ancestors() {
        for marker in project_root_markers {
            let marker_path = ancestor.join(marker);
            if tokio::fs::metadata(&marker_path).await.is_ok() {
                return AbsolutePathBuf::from_absolute_path(ancestor);
            }
        }
    }
    Ok(cwd.clone())
}

/// Return the appropriate list of layers (each with
/// [ConfigLayerSource::Project] as the source) between `cwd` and
/// `project_root`, inclusive. The list is ordered in _increasing_ precdence,
/// starting from folders closest to `project_root` (which is the lowest
/// precedence) to those closest to `cwd` (which is the highest precedence).
async fn load_project_layers(
    cwd: &AbsolutePathBuf,
    project_root: &AbsolutePathBuf,
    trust_context: &ProjectTrustContext,
    savfox_home: &Path,
) -> io::Result<Vec<ConfigLayerEntry>> {
    let savfox_home_abs = AbsolutePathBuf::from_absolute_path(savfox_home)?;
    let savfox_home_normalized =
        normalize_path(savfox_home_abs.as_path()).unwrap_or_else(|_| savfox_home_abs.to_path_buf());
    let mut dirs = cwd
        .as_path()
        .ancestors()
        .scan(false, |done, a| {
            if *done {
                None
            } else {
                if a == project_root.as_path() {
                    *done = true;
                }
                Some(a)
            }
        })
        .collect::<Vec<_>>();
    dirs.reverse();

    let mut layers = Vec::new();
    for dir in dirs {
        let dot_savfox = dir.join(".savfox");
        if !tokio::fs::metadata(&dot_savfox)
            .await
            .map(|meta| meta.is_dir())
            .unwrap_or(false)
        {
            continue;
        }

        let layer_dir = AbsolutePathBuf::from_absolute_path(dir)?;
        let decision = trust_context.decision_for_dir(&layer_dir);
        let dot_savfox_abs = AbsolutePathBuf::from_absolute_path(&dot_savfox)?;
        let dot_savfox_normalized = normalize_path(dot_savfox_abs.as_path())
            .unwrap_or_else(|_| dot_savfox_abs.to_path_buf());
        if dot_savfox_abs == savfox_home_abs || dot_savfox_normalized == savfox_home_normalized {
            continue;
        }
        let config_toml_file = dot_savfox_abs.join(CONFIG_TOML_FILE)?;
        let config_file = resolve_preferred_config_file(&config_toml_file).await?;
        match tokio::fs::read_to_string(&config_file).await {
            Ok(contents) => {
                let config: TomlValue = match parse_config_file_content(
                    config_file.as_path(),
                    &contents,
                ) {
                    Ok(config) => config,
                    Err(e) => {
                        if decision.is_trusted() {
                            let config_file_display = config_file.as_path().display();
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!(
                                    "Error parsing project config file {config_file_display}: {e}"
                                ),
                            ));
                        }
                        layers.push(project_layer_entry(
                            trust_context,
                            &dot_savfox_abs,
                            &layer_dir,
                            TomlValue::Table(toml::map::Map::new()),
                            true,
                        ));
                        continue;
                    }
                };
                let config =
                    resolve_relative_paths_in_config_toml(config, dot_savfox_abs.as_path())?;
                let entry =
                    project_layer_entry(trust_context, &dot_savfox_abs, &layer_dir, config, true);
                layers.push(entry);
            }
            Err(err) => {
                if err.kind() == io::ErrorKind::NotFound {
                    // If there is no config file, record an empty entry
                    // for this project layer, as this may still have subfolders
                    // that are significant in the overall ConfigLayerStack.
                    layers.push(project_layer_entry(
                        trust_context,
                        &dot_savfox_abs,
                        &layer_dir,
                        TomlValue::Table(toml::map::Map::new()),
                        false,
                    ));
                } else {
                    let config_file_display = config_file.as_path().display();
                    return Err(io::Error::new(
                        err.kind(),
                        format!("Failed to read project config file {config_file_display}: {err}"),
                    ));
                }
            }
        }
    }

    Ok(layers)
}

// Cannot name this `mod tests` because of tests.rs in this folder.
#[cfg(test)]
mod unit_tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn ensure_resolve_relative_paths_in_config_toml_preserves_all_fields() -> anyhow::Result<()> {
        let tmp = tempdir()?;
        let base_dir = tmp.path();
        let contents = r#"
# This is a field recognized by config.toml that is an AbsolutePathBuf in
# the ConfigToml struct.
model_instructions_file = "./some_file.md"

# This is a field recognized by config.toml.
model = { provider = "openai", slug = "gpt-1000" }

# This is a field not recognized by config.toml.
foo = "xyzzy"
"#;
        let user_config: TomlValue = toml::from_str(contents)?;

        let normalized_toml_value = resolve_relative_paths_in_config_toml(user_config, base_dir)?;
        let mut expected_toml_value = toml::map::Map::new();
        expected_toml_value.insert(
            "model_instructions_file".to_owned(),
            TomlValue::String(
                AbsolutePathBuf::resolve_path_against_base("./some_file.md", base_dir)?
                    .as_path()
                    .to_string_lossy()
                    .to_string(),
            ),
        );
        let mut model = toml::map::Map::new();
        model.insert(
            "provider".to_owned(),
            TomlValue::String("openai".to_owned()),
        );
        model.insert(
            "slug".to_owned(),
            TomlValue::String("gpt-1000".to_owned()),
        );
        expected_toml_value.insert("model".to_owned(), TomlValue::Table(model));
        expected_toml_value.insert("foo".to_owned(), TomlValue::String("xyzzy".to_owned()));
        assert_eq!(normalized_toml_value, TomlValue::Table(expected_toml_value));
        Ok(())
    }
}
