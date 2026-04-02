// Forbid accidental stdout/stderr writes in the *library* portion of the TUI.
// The standalone `savfox-tui` binary prints a short help message before the
// alternate‑screen mode starts; that file opts‑out locally via `allow`.
#![deny(clippy::print_stdout, clippy::print_stderr)]
#![deny(clippy::disallowed_methods)]
#![allow(unreachable_pub, unsafe_code)]
#![allow(missing_debug_implementations)]
use std::collections::HashSet;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

use additional_dirs::add_dir_warning_message;
use app::App;
pub use app::{AppExitInfo, ExitReason};
use cwd_prompt::{CwdPromptAction, CwdSelection};
use savfox_common::oss::{
    ensure_oss_provider_ready, get_default_model_for_oss_provider, ollama_chat_deprecation_notice,
};
use savfox_core::auth::enforce_login_restrictions;
use savfox_core::config::edit::ConfigEditsBuilder;
use savfox_core::config::provider_store::{ProviderStoreFile, has_provider_store_configuration};
use savfox_core::config::{
    Config, ConfigBuilder, ConfigOverrides, ConfigToml, find_savfox_home, resolve_oss_provider,
};
use savfox_core::config_loader::{
    CloudRequirementsLoader, ConfigLoadError, format_config_error_with_source,
};
use savfox_core::default_client::set_default_client_residency_requirement;
use savfox_core::protocol::AskForApproval;
use savfox_core::terminal::Multiplexer;
use savfox_core::windows_sandbox::WindowsSandboxLevelExt;
use savfox_core::{
    AuthManager, INTERACTIVE_SESSION_SOURCES, RolloutRecorder, SessionSortKey,
    find_session_path_by_id_str, find_session_path_by_name_str, parse_provider_prefixed_model,
    path_utils, read_session_meta_line,
};
use savfox_protocol::config_types::{AltScreenMode, SandboxMode, WindowsSandboxLevel};
use savfox_protocol::openai_models::ReasoningEffort;
use savfox_protocol::protocol::{RolloutItem, RolloutLine};
use savfox_state::log_db;
use serde_json::Value;
use tracing::error;
use tracing_appender::non_blocking;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;
use uuid::Uuid;

mod additional_dirs;
mod app;
mod app_backtrack;
mod app_event;
mod app_event_sender;
mod ascii_logo;
mod bottom_pane;
mod branch_view;
mod chat_screen;
mod cli;
mod clipboard_paste;
mod collab;
mod collaboration_modes;
mod color;
pub mod custom_terminal;
mod cwd_prompt;
mod diff_render;
mod exec_cell;
mod exec_command;
mod external_editor;
mod file_search;
mod get_git_diff;
mod history_cell;
pub mod insert_history;
mod key_hint;
mod keymap;
pub mod live_wrap;
mod logo_image;
mod markdown;
mod markdown_render;
mod markdown_stream;
mod model_migration;
mod notifications;
pub mod onboarding;
mod oss_selection;
mod pager_overlay;
mod provider_connect;
pub mod public_widgets;
mod render;
mod resume_picker;
mod selection_list;
mod session_log;
mod shimmer;
mod skills_helpers;
mod slash_command;
mod startup_hero;
mod status;
mod status_indicator_widget;
mod streaming;
mod style;
mod terminal_palette;
mod text_formatting;
mod tooltips;
mod tui;
mod ui_consts;
pub mod update_action;
mod update_prompt;
mod updates;
mod version;

mod wrapping;

#[cfg(test)]
pub mod test_backend;

pub use cli::Cli;
pub use markdown_render::render_markdown_text;
pub use public_widgets::composer_input::{ComposerAction, ComposerInput};

use crate::onboarding::onboarding_screen::{OnboardingScreenArgs, run_onboarding_app};
use crate::tui::Tui;
// (tests access modules directly within the crate)

const PROVIDER_MODELS_DIR: &str = "models";

#[derive(Debug, Clone)]
struct ProviderModelCatalog {
    provider_id: String,
    first_model: String,
    model_ids_lower: Vec<String>,
}

#[derive(Debug, Clone)]
struct EffectiveModelSelection {
    configured_model: String,
    configured_provider: Option<String>,
    effective_effort: Option<ReasoningEffort>,
}

fn trim_nonempty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn model_id_from_store_item(item: &Value, provider_id: &str) -> Option<String> {
    let provider_id = provider_id.trim();
    if provider_id.is_empty() {
        return None;
    }

    let raw = item
        .get("id")
        .and_then(Value::as_str)
        .and_then(trim_nonempty)
        .or_else(|| {
            item.get("model")
                .and_then(Value::as_str)
                .and_then(trim_nonempty)
        })
        .or_else(|| {
            item.get("model_slug")
                .and_then(Value::as_str)
                .and_then(trim_nonempty)
        })
        .or_else(|| item.as_str().and_then(trim_nonempty))?;

    // Extract the bare model slug (strip any existing provider prefix) and
    // re-prefix with the correct provider_id so that the catalog always uses
    // consistent identifiers that match config.toml.
    let slug = raw.rsplit_once('/').map_or(raw.as_ref(), |(_, s)| s);
    let slug = slug.trim();
    if slug.is_empty() {
        return None;
    }
    Some(format!("{provider_id}/{slug}"))
}

fn load_provider_model_catalog(savfox_home: &Path) -> Vec<ProviderModelCatalog> {
    let models_dir = savfox_home.join(PROVIDER_MODELS_DIR);
    let Ok(entries) = std::fs::read_dir(models_dir) else {
        return Vec::new();
    };

    let mut catalogs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }

        let provider_from_filename = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(trim_nonempty);
        let Some(provider_from_filename) = provider_from_filename else {
            continue;
        };

        let Ok(data) = std::fs::read_to_string(&path) else {
            continue;
        };

        let (provider_id, models) = if let Ok(file) =
            serde_json::from_str::<ProviderStoreFile>(&data)
        {
            let provider_id = trim_nonempty(file.account_id()).unwrap_or(provider_from_filename);
            let models: Vec<Value> = if !file.models.is_empty() {
                let disabled: HashSet<&str> =
                    file.disabled_models.iter().map(String::as_str).collect();
                file.models
                    .into_iter()
                    .filter(|m| {
                        let slug = m
                            .get("model_slug")
                            .and_then(|v| v.as_str())
                            .or_else(|| m.get("id").and_then(|v| v.as_str()));
                        match slug {
                            Some(s) => !disabled.contains(s),
                            None => true,
                        }
                    })
                    .collect()
            } else {
                Vec::new()
            };
            (provider_id, models)
        } else if let Ok(models) = serde_json::from_str::<Vec<Value>>(&data) {
            (provider_from_filename, models)
        } else {
            continue;
        };

        if provider_id.trim().is_empty() {
            continue;
        }

        let mut model_ids: Vec<String> = Vec::new();
        let mut model_ids_lower: Vec<String> = Vec::new();
        let mut seen_lower = HashSet::new();
        for item in models.iter() {
            let Some(model_id) = model_id_from_store_item(item, provider_id.as_str()) else {
                continue;
            };
            let lower = model_id.to_ascii_lowercase();
            if !seen_lower.insert(lower.clone()) {
                continue;
            }
            model_ids.push(model_id);
            model_ids_lower.push(lower);
        }
        let Some(first_model) = model_ids.first().cloned() else {
            continue;
        };

        if catalogs.iter().any(|existing: &ProviderModelCatalog| {
            existing.provider_id.eq_ignore_ascii_case(&provider_id)
        }) {
            continue;
        }

        catalogs.push(ProviderModelCatalog {
            provider_id,
            first_model,
            model_ids_lower,
        });
    }

    catalogs.sort_by(|left, right| {
        left.provider_id
            .to_ascii_lowercase()
            .cmp(&right.provider_id.to_ascii_lowercase())
    });
    catalogs
}

fn model_exists_in_catalog(model: &str, catalog: &ProviderModelCatalog) -> bool {
    let normalized = model.trim().to_ascii_lowercase();
    catalog.model_ids_lower.iter().any(|id| id == &normalized)
}

fn effective_model_selection(
    config_toml: &ConfigToml,
) -> std::io::Result<Option<EffectiveModelSelection>> {
    let configured_model = config_toml
        .model
        .as_ref()
        .and_then(|model| model.to_model_id());
    let Some(configured_model) = configured_model else {
        return Ok(None);
    };

    let provider_from_model = parse_provider_prefixed_model(&configured_model)
        .map(|(provider_id, _)| provider_id.to_owned());
    let configured_provider = provider_from_model.or_else(|| {
        config_toml
            .model
            .as_ref()
            .and_then(|model| model.normalized_provider())
            .or_else(|| {
                config_toml
                    .model_provider
                    .as_deref()
                    .and_then(trim_nonempty)
            })
    });

    let effective_effort = config_toml
        .model
        .as_ref()
        .and_then(|model| model.reasoning_effort)
        .or(config_toml.model_reasoning_effort);

    Ok(Some(EffectiveModelSelection {
        configured_model,
        configured_provider,
        effective_effort,
    }))
}

fn find_catalog_for_provider<'a>(
    catalogs: &'a [ProviderModelCatalog],
    provider_id: &str,
) -> Option<&'a ProviderModelCatalog> {
    catalogs
        .iter()
        .find(|entry| entry.provider_id.eq_ignore_ascii_case(provider_id))
}

fn configured_model_for_provider(model: &str, provider_id: Option<&str>) -> Option<String> {
    if let Some((provider, code)) = parse_provider_prefixed_model(model) {
        return Some(format!("{}/{}", provider.trim(), code.trim()));
    }
    provider_id
        .and_then(trim_nonempty)
        .map(|provider| format!("{}/{}", provider, model.trim()))
}

async fn maybe_repair_model_from_provider_store(
    savfox_home: &Path,
    config_toml: &ConfigToml,
) -> std::io::Result<Option<String>> {
    let catalogs = load_provider_model_catalog(savfox_home);
    if catalogs.is_empty() {
        return Ok(None);
    }

    let Some(selection) = effective_model_selection(config_toml)? else {
        return Ok(None);
    };

    let configured_full_model = configured_model_for_provider(
        selection.configured_model.as_str(),
        selection.configured_provider.as_deref(),
    );

    let mut target_catalog: Option<&ProviderModelCatalog> = None;
    let mut warning = None;

    if let Some(provider_id) = selection.configured_provider.as_deref() {
        if let Some(catalog) = find_catalog_for_provider(&catalogs, provider_id) {
            let model_exists = configured_full_model
                .as_deref()
                .is_some_and(|model| model_exists_in_catalog(model, catalog));
            if !model_exists {
                target_catalog = Some(catalog);
                warning = Some(format!(
                    "Configured model `{}` is not available for provider `{}` in `~/.savfox/models`; falling back to `{}`.",
                    selection.configured_model, provider_id, catalog.first_model
                ));
            }
        } else if let Some(fallback) = catalogs.first() {
            target_catalog = Some(fallback);
            warning = Some(format!(
                "Configured provider `{}` (from model `{}`) was not found in `~/.savfox/models`; falling back to `{}`.",
                provider_id, selection.configured_model, fallback.first_model
            ));
        }
    } else if let Some(fallback) = catalogs.first() {
        target_catalog = Some(fallback);
        warning = Some(format!(
            "Could not resolve provider for configured model `{}`; falling back to `{}` from `~/.savfox/models`.",
            selection.configured_model, fallback.first_model
        ));
    }

    let Some(target) = target_catalog else {
        return Ok(None);
    };
    let Some(warning) = warning else {
        return Ok(None);
    };

    if selection
        .configured_model
        .trim()
        .eq_ignore_ascii_case(target.first_model.as_str())
    {
        return Ok(None);
    }

    ConfigEditsBuilder::new(savfox_home)
        .set_model(
            Some(target.first_model.as_str()),
            selection.effective_effort,
        )
        .apply()
        .await
        .map_err(|err| {
            std::io::Error::other(format!("failed to update config.toml model: {err}"))
        })?;

    Ok(Some(warning))
}

pub async fn run_main(
    cli: Cli,
    savfox_linux_sandbox_exe: Option<PathBuf>,
) -> std::io::Result<AppExitInfo> {
    let (sandbox_mode, approval_policy) = if cli.full_auto {
        (
            Some(SandboxMode::WorkspaceWrite),
            Some(AskForApproval::OnRequest),
        )
    } else if cli.dangerously_bypass_approvals_and_sandbox {
        (
            Some(SandboxMode::DangerFullAccess),
            Some(AskForApproval::Never),
        )
    } else {
        (
            cli.sandbox_mode.map(Into::<SandboxMode>::into),
            cli.approval_policy.map(Into::into),
        )
    };

    // Map the legacy --search flag to the canonical web_search mode.
    // When using `--oss`, let the bootstrapper pick the model (defaulting to
    // gpt-oss:20b) and ensure it is present locally. Also, force the built‑in
    // `oss` model provider.
    let _cli_kv_overrides: Vec<(String, toml::Value)> = Vec::new();

    // we load config.toml here to determine project state.
    #[allow(clippy::print_stderr)]
    let savfox_home = match find_savfox_home() {
        Ok(savfox_home) => savfox_home.clone(),
        Err(err) => {
            eprintln!("Error finding savfox home: {err}");
            std::process::exit(1);
        }
    };

    let cwd = cli.cwd.clone();

    #[allow(clippy::print_stderr)]
    let mut config_toml: ConfigToml = match Config::load_with_cli_overrides(Vec::new()).await {
        Ok(cfg) => cfg
            .config_layer_stack
            .effective_config()
            .try_into()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?,
        Err(err) => {
            let config_error = err
                .get_ref()
                .and_then(|err| err.downcast_ref::<ConfigLoadError>())
                .map(ConfigLoadError::config_error);
            if let Some(config_error) = config_error {
                eprintln!(
                    "Error loading config.toml:\n{}",
                    format_config_error_with_source(config_error)
                );
            } else {
                eprintln!("Error loading config.toml: {err}");
            }
            std::process::exit(1);
        }
    };

    if let Err(err) =
        savfox_core::personality_migration::maybe_migrate_personality(&savfox_home, &config_toml)
            .await
    {
        tracing::warn!(error = %err, "failed to run personality migration");
    }

    let has_model_cli_override = false;
    if !cli.oss && cli.model.is_none() && !has_model_cli_override {
        match maybe_repair_model_from_provider_store(&savfox_home, &config_toml).await {
            Ok(Some(warning)) => {
                #[allow(clippy::print_stderr)]
                {
                    eprintln!("Warning: {warning}");
                }
                let cfg = Config::load_with_cli_overrides(Vec::new()).await?;
                config_toml = cfg
                    .config_layer_stack
                    .effective_config()
                    .try_into()
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            }
            Ok(None) => {}
            Err(err) => {
                #[allow(clippy::print_stderr)]
                {
                    eprintln!("Warning: failed to auto-repair model config: {err}");
                }
            }
        }
    }

    let cloud_requirements = CloudRequirementsLoader::default();

    let model_provider_override = if cli.oss {
        let resolved = resolve_oss_provider(cli.oss_provider.as_deref(), &config_toml);

        if let Some(provider) = resolved {
            Some(provider)
        } else {
            // No provider configured, prompt the user
            let provider = oss_selection::select_oss_provider(&savfox_home).await?;
            if provider == "__CANCELLED__" {
                return Err(std::io::Error::other(
                    "OSS provider selection was cancelled by user",
                ));
            }
            Some(provider)
        }
    } else {
        None
    };

    // When using `--oss`, let the bootstrapper pick the model based on selected provider
    let model = if let Some(model) = &cli.model {
        Some(model.clone())
    } else if cli.oss {
        // Use the provider from model_provider_override
        model_provider_override
            .as_ref()
            .and_then(|provider_id| get_default_model_for_oss_provider(provider_id))
            .map(std::borrow::ToOwned::to_owned)
    } else {
        None // No model specified, will use the default.
    };

    let additional_dirs = cli.add_dir.clone();

    let overrides = ConfigOverrides {
        model,
        approval_policy,
        sandbox_mode,
        cwd,
        model_provider: model_provider_override.clone(),
        savfox_linux_sandbox_exe,
        show_raw_agent_reasoning: cli.oss.then_some(true),
        additional_writable_roots: additional_dirs,
        ..Default::default()
    };

    let config = load_config_or_exit(overrides.clone(), cloud_requirements.clone()).await;
    set_default_client_residency_requirement(config.enforce_residency.value());

    if let Some(warning) = add_dir_warning_message(&cli.add_dir, config.sandbox_policy.get()) {
        #[allow(clippy::print_stderr)]
        {
            eprintln!("Error adding directories: {warning}");
            std::process::exit(1);
        }
    }

    #[allow(clippy::print_stderr)]
    if let Err(err) = enforce_login_restrictions(&config) {
        eprintln!("{err}");
        std::process::exit(1);
    }

    let log_dir = savfox_core::config::log_dir(&config)?;
    std::fs::create_dir_all(&log_dir)?;
    // Open (or create) your log file, appending to it.
    let mut log_file_opts = OpenOptions::new();
    log_file_opts.create(true).append(true);

    // Ensure the file is only readable and writable by the current user.
    // Doing the equivalent to `chmod 600` on Windows is quite a bit more code
    // and requires the Windows API crates, so we can reconsider that when
    // Savfox CLI is officially supported on Windows.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        log_file_opts.mode(0o600);
    }

    let log_file = log_file_opts.open(log_dir.join("savfox-tui.log"))?;

    // Wrap file in non‑blocking writer.
    let (non_blocking, _guard) = non_blocking(log_file);

    // use RUST_LOG env var, default to info for savfox crates.
    let env_filter = || {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new("savfox_core=info,savfox_tui=info,savfox_rmcp_client=info")
        })
    };

    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        // `with_target(true)` is the default, but we previously disabled it for file output.
        // Keep it enabled so we can selectively enable targets via `RUST_LOG=...` and then
        // grep for a specific module/target while troubleshooting.
        .with_target(true)
        .with_ansi(false)
        .with_span_events(
            tracing_subscriber::fmt::format::FmtSpan::NEW
                | tracing_subscriber::fmt::format::FmtSpan::CLOSE,
        )
        .with_filter(env_filter());

    let feedback = savfox_feedback::SavfoxFeedback::new();
    let feedback_layer = feedback.logger_layer();
    let feedback_metadata_layer = feedback.metadata_layer();

    if cli.oss && model_provider_override.is_some() {
        // We're in the oss section, so provider_id should be Some
        // Let's handle None case gracefully though just in case
        let provider_id = if let Some(id) = model_provider_override.as_ref() {
            id
        } else {
            error!("OSS provider unexpectedly not set when oss flag is used");
            return Err(std::io::Error::other(
                "OSS provider not set but oss flag was used",
            ));
        };
        ensure_oss_provider_ready(provider_id, &config).await?;
    }

    let otel = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        savfox_core::otel_init::build_provider(&config, env!("CARGO_PKG_VERSION"), None, true)
    })) {
        Ok(Ok(otel)) => otel,
        Ok(Err(e)) => {
            #[allow(clippy::print_stderr)]
            {
                eprintln!("Could not create otel exporter: {e}");
            }
            None
        }
        Err(_) => {
            #[allow(clippy::print_stderr)]
            {
                eprintln!("Could not create otel exporter: panicked during initialization");
            }
            None
        }
    };

    let otel_logger_layer = otel.as_ref().and_then(|o| o.logger_layer());

    let otel_tracing_layer = otel.as_ref().and_then(|o| o.tracing_layer());

    let log_db_layer = savfox_core::state_db::get_state_db(&config, None)
        .await
        .map(|db| log_db::start(db).with_filter(env_filter()));

    let _ = tracing_subscriber::registry()
        .with(file_layer)
        .with(feedback_layer)
        .with(feedback_metadata_layer)
        .with(log_db_layer)
        .with(otel_logger_layer)
        .with(otel_tracing_layer)
        .try_init();

    run_ratatui_app(cli, config, overrides, cloud_requirements, feedback)
        .await
        .map_err(|err| std::io::Error::other(err.to_string()))
}

async fn run_ratatui_app(
    cli: Cli,
    initial_config: Config,
    overrides: ConfigOverrides,
    cloud_requirements: CloudRequirementsLoader,
    feedback: savfox_feedback::SavfoxFeedback,
) -> color_eyre::Result<AppExitInfo> {
    color_eyre::install()?;

    tooltips::announcement::prewarm();

    // Forward panic reports through tracing so they appear in the UI status
    // line, but do not swallow the default/color-eyre panic handler.
    // Chain to the previous hook so users still get a rich panic report
    // (including backtraces) after we restore the terminal.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("panic: {info}");
        prev_hook(info);
    }));
    let mut terminal = tui::init()?;
    terminal.clear()?;

    let mut tui = Tui::new(terminal);

    #[cfg(not(debug_assertions))]
    {
        use crate::update_prompt::UpdatePromptOutcome;

        let skip_update_prompt = cli.prompt.as_ref().is_some_and(|prompt| !prompt.is_empty());
        if !skip_update_prompt {
            match update_prompt::run_update_prompt_if_needed(&mut tui, &initial_config).await? {
                UpdatePromptOutcome::Continue => {}
                UpdatePromptOutcome::RunUpdate(action) => {
                    crate::tui::restore()?;
                    return Ok(AppExitInfo {
                        token_usage: savfox_core::protocol::TokenUsage::default(),
                        session_id: None,
                        session_name: None,
                        model_display: None,
                        directory: None,
                        update_action: Some(action),
                        exit_reason: ExitReason::UserRequested,
                    });
                }
            }
        }
    }

    // Initialize high-fidelity session event logging if enabled.
    session_log::maybe_init(&initial_config);

    let auth_manager = AuthManager::shared(
        initial_config.savfox_home.clone(),
        false,
        initial_config.cli_auth_credentials_store_mode,
    );
    let has_connected_provider = has_connected_provider(&initial_config);
    let should_show_trust_screen_flag = should_show_trust_screen(&initial_config);
    let should_show_onboarding = should_show_onboarding(
        &initial_config,
        should_show_trust_screen_flag,
        has_connected_provider,
    );

    let config = if should_show_onboarding {
        let onboarding_result = run_onboarding_app(
            OnboardingScreenArgs {
                show_provider_setup_screen: should_show_provider_setup_screen(
                    &initial_config,
                    has_connected_provider,
                ),
                has_connected_provider,
                show_trust_screen: should_show_trust_screen_flag,
                auth_manager: auth_manager.clone(),
                config: initial_config.clone(),
            },
            &mut tui,
        )
        .await?;
        if onboarding_result.should_exit {
            restore();
            session_log::log_session_end();
            let _ = tui.terminal.clear();
            return Ok(AppExitInfo {
                token_usage: savfox_core::protocol::TokenUsage::default(),
                session_id: None,
                session_name: None,
                model_display: None,
                directory: None,
                update_action: None,
                exit_reason: ExitReason::UserRequested,
            });
        }
        // If the user made an explicit trust decision, reload config so current
        // process state reflects what was persisted to config.toml.
        if onboarding_result.directory_trust_decision.is_some() {
            load_config_or_exit(overrides.clone(), cloud_requirements.clone()).await
        } else {
            initial_config
        }
    } else {
        initial_config
    };

    let ollama_chat_support_notice = match ollama_chat_deprecation_notice(&config).await {
        Ok(notice) => notice,
        Err(err) => {
            tracing::warn!(?err, "Failed to detect Ollama wire API");
            None
        }
    };
    let mut missing_session_exit = |id_str: &str, action: &str| {
        error!("Error finding conversation path: {id_str}");
        restore();
        session_log::log_session_end();
        let _ = tui.terminal.clear();
        Ok(AppExitInfo {
            token_usage: savfox_core::protocol::TokenUsage::default(),
            session_id: None,
            session_name: None,
            model_display: None,
            directory: None,
            update_action: None,
            exit_reason: ExitReason::Fatal(format!(
                "No saved session found with ID {id_str}. Run `savfox {action}` without an ID to choose from existing sessions."
            )),
        })
    };

    let use_fork = cli.fork_picker || cli.fork_last || cli.fork_session_id.is_some();
    let session_selection = if use_fork {
        if let Some(id_str) = cli.fork_session_id.as_deref() {
            let is_uuid = Uuid::parse_str(id_str).is_ok();
            let path = if is_uuid {
                find_session_path_by_id_str(&config.savfox_home, id_str).await?
            } else {
                find_session_path_by_name_str(&config.savfox_home, id_str).await?
            };
            match path {
                Some(path) => resume_picker::SessionSelection::Fork(path),
                None => return missing_session_exit(id_str, "fork"),
            }
        } else if cli.fork_last {
            let provider_filter = vec![config.model_provider_id.clone()];
            match RolloutRecorder::list_sessions(
                &config.savfox_home,
                1,
                None,
                SessionSortKey::UpdatedAt,
                INTERACTIVE_SESSION_SOURCES,
                Some(provider_filter.as_slice()),
                &config.model_provider_id,
            )
            .await
            {
                Ok(page) => page
                    .items
                    .first()
                    .map(|it| resume_picker::SessionSelection::Fork(it.path.clone()))
                    .unwrap_or(resume_picker::SessionSelection::StartFresh),
                Err(_) => resume_picker::SessionSelection::StartFresh,
            }
        } else if cli.fork_picker {
            match resume_picker::run_fork_picker(
                &mut tui,
                &config.savfox_home,
                &config.model_provider_id,
                cli.fork_show_all,
            )
            .await?
            {
                resume_picker::SessionSelection::Exit => {
                    restore();
                    session_log::log_session_end();
                    return Ok(AppExitInfo {
                        token_usage: savfox_core::protocol::TokenUsage::default(),
                        session_id: None,
                        session_name: None,
                        model_display: None,
                        directory: None,
                        update_action: None,
                        exit_reason: ExitReason::UserRequested,
                    });
                }
                other => other,
            }
        } else {
            resume_picker::SessionSelection::StartFresh
        }
    } else if let Some(id_str) = cli.resume_session_id.as_deref() {
        let is_uuid = Uuid::parse_str(id_str).is_ok();
        let path = if is_uuid {
            find_session_path_by_id_str(&config.savfox_home, id_str).await?
        } else {
            find_session_path_by_name_str(&config.savfox_home, id_str).await?
        };
        match path {
            Some(path) => resume_picker::SessionSelection::Resume(path),
            None => return missing_session_exit(id_str, "resume"),
        }
    } else if cli.resume_last {
        let provider_filter = vec![config.model_provider_id.clone()];
        let filter_cwd = if cli.resume_show_all {
            None
        } else {
            Some(config.cwd.as_path())
        };
        match RolloutRecorder::find_latest_session_path(
            &config.savfox_home,
            1,
            None,
            SessionSortKey::UpdatedAt,
            INTERACTIVE_SESSION_SOURCES,
            Some(provider_filter.as_slice()),
            &config.model_provider_id,
            filter_cwd,
        )
        .await
        {
            Ok(Some(path)) => resume_picker::SessionSelection::Resume(path),
            _ => resume_picker::SessionSelection::StartFresh,
        }
    } else if cli.resume_picker {
        match resume_picker::run_resume_picker(
            &mut tui,
            &config.savfox_home,
            &config.model_provider_id,
            cli.resume_show_all,
        )
        .await?
        {
            resume_picker::SessionSelection::Exit => {
                restore();
                session_log::log_session_end();
                return Ok(AppExitInfo {
                    token_usage: savfox_core::protocol::TokenUsage::default(),
                    session_id: None,
                    session_name: None,
                    model_display: None,
                    directory: None,
                    update_action: None,
                    exit_reason: ExitReason::UserRequested,
                });
            }
            other => other,
        }
    } else {
        resume_picker::SessionSelection::StartFresh
    };

    let current_cwd = config.cwd.clone();
    let allow_prompt = cli.cwd.is_none();
    let action_and_path_if_resume_or_fork = match &session_selection {
        resume_picker::SessionSelection::Resume(path) => Some((CwdPromptAction::Resume, path)),
        resume_picker::SessionSelection::Fork(path) => Some((CwdPromptAction::Fork, path)),
        _ => None,
    };
    let fallback_cwd = match action_and_path_if_resume_or_fork {
        Some((action, path)) => {
            resolve_cwd_for_resume_or_fork(&mut tui, &current_cwd, path, action, allow_prompt)
                .await?
        }
        None => None,
    };

    let config = match &session_selection {
        resume_picker::SessionSelection::Resume(_) | resume_picker::SessionSelection::Fork(_) => {
            load_config_or_exit_with_fallback_cwd(
                overrides.clone(),
                cloud_requirements.clone(),
                fallback_cwd,
            )
            .await
        }
        _ => config,
    };
    let should_show_trust_screen = should_show_trust_screen(&config);

    let Cli {
        prompt,
        images,
        alt_screen,
        ..
    } = cli;

    let use_alt_screen = determine_alt_screen_mode(alt_screen, config.tui_alternate_screen);
    tui.set_alt_screen_enabled(use_alt_screen);

    let app_result = App::run(
        &mut tui,
        auth_manager,
        config,
        overrides.clone(),
        prompt,
        images,
        session_selection,
        feedback,
        should_show_trust_screen, // Proxy to: is it a first run in this directory?
        ollama_chat_support_notice,
    )
    .await;

    restore();
    // Mark the end of the recorded session.
    session_log::log_session_end();
    // ignore error when collecting usage – report underlying error instead
    app_result
}

pub(crate) async fn read_session_cwd(path: &Path) -> Option<PathBuf> {
    // Prefer the latest TurnContext cwd so resume/fork reflects the most recent
    // session directory (for the changed-cwd prompt). The alternative would be
    // mutating the SessionMeta line when the session cwd changes, but the rollout
    // is an append-only JSONL log and rewriting the head would be error-prone.
    // When rollouts move to SQLite, we can drop this scan.
    if let Some(cwd) = parse_latest_turn_context_cwd(path).await {
        return Some(cwd);
    }
    match read_session_meta_line(path).await {
        Ok(meta_line) => Some(meta_line.meta.cwd),
        Err(err) => {
            let rollout_path = path.display().to_string();
            tracing::warn!(
                %rollout_path,
                %err,
                "Failed to read session metadata from rollout"
            );
            None
        }
    }
}

async fn parse_latest_turn_context_cwd(path: &Path) -> Option<PathBuf> {
    let text = tokio::fs::read_to_string(path).await.ok()?;
    for line in text.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(rollout_line) = serde_json::from_str::<RolloutLine>(trimmed) else {
            continue;
        };
        if let RolloutItem::TurnContext(item) = rollout_line.item {
            return Some(item.cwd);
        }
    }
    None
}

pub(crate) fn cwds_differ(current_cwd: &Path, session_cwd: &Path) -> bool {
    match (
        path_utils::normalize_for_path_comparison(current_cwd),
        path_utils::normalize_for_path_comparison(session_cwd),
    ) {
        (Ok(current), Ok(session)) => current != session,
        _ => current_cwd != session_cwd,
    }
}

pub(crate) async fn resolve_cwd_for_resume_or_fork(
    tui: &mut Tui,
    current_cwd: &Path,
    path: &Path,
    action: CwdPromptAction,
    allow_prompt: bool,
) -> color_eyre::Result<Option<PathBuf>> {
    let Some(history_cwd) = read_session_cwd(path).await else {
        return Ok(None);
    };
    if allow_prompt && cwds_differ(current_cwd, &history_cwd) {
        let selection =
            cwd_prompt::run_cwd_selection_prompt(tui, action, current_cwd, &history_cwd).await?;
        return Ok(Some(match selection {
            CwdSelection::Current => current_cwd.to_path_buf(),
            CwdSelection::Session => history_cwd,
        }));
    }
    Ok(Some(history_cwd))
}

#[expect(
    clippy::print_stderr,
    reason = "TUI should no longer be displayed, so we can write to stderr."
)]
fn restore() {
    if let Err(err) = tui::restore() {
        eprintln!(
            "failed to restore terminal. Run `reset` or restart your terminal to recover: {err}"
        );
    }
}

/// Determine whether to use the terminal's alternate screen buffer.
///
/// The alternate screen buffer provides a cleaner fullscreen experience without polluting
/// the terminal's scrollback history. However, it conflicts with terminal multiplexers like
/// Zellij that strictly follow the xterm spec, which disallows scrollback in alternate screen
/// buffers. Zellij intentionally disables scrollback in alternate screen mode (see
/// https://github.com/zellij-org/zellij/pull/1032) and offers no configuration option to
/// change this behavior.
///
/// This function implements a pragmatic workaround:
/// - If `--alt-screen` is explicitly passed, use its value and override config
/// - Otherwise, respect the `tui.alternate_screen` config setting:
///   - `always`: Use alternate screen everywhere (original behavior)
///   - `never`: Inline mode only, preserves scrollback
///   - `auto` (default): Auto-detect the terminal multiplexer and disable alternate screen only in
///     Zellij, enabling it everywhere else
fn determine_alt_screen_mode(
    cli_alt_screen: Option<AltScreenMode>,
    tui_alternate_screen: AltScreenMode,
) -> bool {
    let effective_mode = cli_alt_screen.unwrap_or(tui_alternate_screen);
    match effective_mode {
        AltScreenMode::Always => true,
        AltScreenMode::Never => false,
        AltScreenMode::Auto => {
            let terminal_info = savfox_core::terminal::terminal_info();
            !matches!(terminal_info.multiplexer, Some(Multiplexer::Zellij { .. }))
        }
    }
}

async fn load_config_or_exit(
    overrides: ConfigOverrides,
    cloud_requirements: CloudRequirementsLoader,
) -> Config {
    load_config_or_exit_with_fallback_cwd(overrides, cloud_requirements, None).await
}

async fn load_config_or_exit_with_fallback_cwd(
    overrides: ConfigOverrides,
    cloud_requirements: CloudRequirementsLoader,
    fallback_cwd: Option<PathBuf>,
) -> Config {
    #[allow(clippy::print_stderr)]
    match ConfigBuilder::default()
        .harness_overrides(overrides)
        .cloud_requirements(cloud_requirements)
        .fallback_cwd(fallback_cwd)
        .build()
        .await
    {
        Ok(config) => config,
        Err(err) => {
            eprintln!("Error loading configuration: {err}");
            std::process::exit(1);
        }
    }
}

/// Determine if user has configured a sandbox / approval policy,
/// or if the current cwd project is already trusted. If not, we need to
/// show the trust screen.
fn should_show_trust_screen(config: &Config) -> bool {
    if cfg!(target_os = "windows")
        && WindowsSandboxLevel::from_config(config) == WindowsSandboxLevel::Disabled
    {
        // If the experimental sandbox is not enabled, Native Windows cannot enforce sandboxed write
        // access; skip the trust prompt entirely.
        return false;
    }
    if config.did_user_set_custom_approval_policy_or_sandbox_mode {
        // Respect explicit approval/sandbox overrides made by the user.
        return false;
    }
    // otherwise, show only if no trust decision has been made
    config.active_project.trust_level.is_none()
}

fn should_show_onboarding(
    config: &Config,
    show_trust_screen: bool,
    has_connected_provider: bool,
) -> bool {
    if show_trust_screen {
        return true;
    }

    should_show_provider_setup_screen(config, has_connected_provider)
}

fn has_connected_provider(config: &Config) -> bool {
    has_provider_store_configuration(config.savfox_home.as_path())
}

fn should_show_provider_setup_screen(config: &Config, has_connected_provider: bool) -> bool {
    if has_connected_provider {
        return false;
    }

    let has_configured_model = config
        .model
        .as_ref()
        .is_some_and(|model| !model.trim().is_empty());
    !has_configured_model
}

#[cfg(test)]
mod tests {
    use savfox_core::config::{
        ConfigBuilder, ConfigOverrides, ConfigToml, ProjectConfig, SelectedModel,
    };
    use savfox_core::protocol::AskForApproval;
    use savfox_protocol::protocol::{
        RolloutItem, RolloutLine, SessionMeta, SessionMetaLine, TurnContextItem,
    };
    use serial_test::serial;
    use tempfile::TempDir;

    use super::*;

    async fn build_config(temp_dir: &TempDir) -> std::io::Result<Config> {
        ConfigBuilder::default()
            .savfox_home(temp_dir.path().to_path_buf())
            .build()
            .await
    }

    #[test]
    fn determine_alt_screen_mode_respects_cli_override() {
        assert!(determine_alt_screen_mode(
            Some(AltScreenMode::Always),
            AltScreenMode::Never
        ));
        assert!(!determine_alt_screen_mode(
            Some(AltScreenMode::Never),
            AltScreenMode::Always
        ));
    }

    #[test]
    fn determine_alt_screen_mode_uses_config_when_no_cli_override() {
        assert!(determine_alt_screen_mode(None, AltScreenMode::Always));
        assert!(!determine_alt_screen_mode(None, AltScreenMode::Never));
    }

    #[tokio::test]
    #[serial]
    async fn windows_skips_trust_prompt_without_sandbox() -> std::io::Result<()> {
        let temp_dir = TempDir::new()?;
        let mut config = build_config(&temp_dir).await?;
        config.did_user_set_custom_approval_policy_or_sandbox_mode = false;
        config.active_project = ProjectConfig { trust_level: None };
        config.set_windows_sandbox_enabled(false);

        let should_show = should_show_trust_screen(&config);
        if cfg!(target_os = "windows") {
            assert!(
                !should_show,
                "Windows trust prompt should always be skipped on native Windows"
            );
        } else {
            assert!(
                should_show,
                "Non-Windows should still show trust prompt when project is untrusted"
            );
        }
        Ok(())
    }
    #[tokio::test]
    #[serial]
    async fn windows_shows_trust_prompt_with_sandbox() -> std::io::Result<()> {
        let temp_dir = TempDir::new()?;
        let mut config = build_config(&temp_dir).await?;
        config.did_user_set_custom_approval_policy_or_sandbox_mode = false;
        config.active_project = ProjectConfig { trust_level: None };
        config.set_windows_sandbox_enabled(true);

        let should_show = should_show_trust_screen(&config);
        if cfg!(target_os = "windows") {
            assert!(
                should_show,
                "Windows trust prompt should be shown on native Windows with sandbox enabled"
            );
        } else {
            assert!(
                should_show,
                "Non-Windows should still show trust prompt when project is untrusted"
            );
        }
        Ok(())
    }
    #[tokio::test]
    async fn untrusted_project_skips_trust_prompt() -> std::io::Result<()> {
        use savfox_protocol::config_types::TrustLevel;
        let temp_dir = TempDir::new()?;
        let mut config = build_config(&temp_dir).await?;
        config.did_user_set_custom_approval_policy_or_sandbox_mode = false;
        config.active_project = ProjectConfig {
            trust_level: Some(TrustLevel::Untrusted),
        };

        let should_show = should_show_trust_screen(&config);
        assert!(
            !should_show,
            "Trust prompt should not be shown for projects explicitly marked as untrusted"
        );
        Ok(())
    }

    fn build_turn_context(config: &Config, cwd: PathBuf) -> TurnContextItem {
        let model = config
            .model
            .clone()
            .unwrap_or_else(|| "gpt-5.1".to_string());
        TurnContextItem {
            cwd,
            approval_policy: config.approval_policy.value(),
            sandbox_policy: config.sandbox_policy.get().clone(),
            model,
            personality: None,
            collaboration_mode: None,
            effort: config.model_reasoning_effort,
            summary: config.model_reasoning_summary,
            user_instructions: None,
            developer_instructions: None,
            final_output_json_schema: None,
            truncation_policy: None,
        }
    }

    #[tokio::test]
    async fn read_session_cwd_prefers_latest_turn_context() -> std::io::Result<()> {
        let temp_dir = TempDir::new()?;
        let config = build_config(&temp_dir).await?;
        let first = temp_dir.path().join("first");
        let second = temp_dir.path().join("second");
        std::fs::create_dir_all(&first)?;
        std::fs::create_dir_all(&second)?;

        let rollout_path = temp_dir.path().join("rollout.jsonl");
        let lines = vec![
            RolloutLine {
                timestamp: "t0".to_string(),
                item: RolloutItem::TurnContext(build_turn_context(&config, first)),
            },
            RolloutLine {
                timestamp: "t1".to_string(),
                item: RolloutItem::TurnContext(build_turn_context(&config, second.clone())),
            },
        ];
        let mut text = String::new();
        for line in lines {
            text.push_str(&serde_json::to_string(&line).expect("serialize rollout"));
            text.push('\n');
        }
        std::fs::write(&rollout_path, text)?;

        let cwd = read_session_cwd(&rollout_path).await.expect("expected cwd");
        assert_eq!(cwd, second);
        Ok(())
    }

    #[tokio::test]
    async fn should_prompt_when_meta_matches_current_but_latest_turn_differs() -> std::io::Result<()>
    {
        let temp_dir = TempDir::new()?;
        let config = build_config(&temp_dir).await?;
        let current = temp_dir.path().join("current");
        let latest = temp_dir.path().join("latest");
        std::fs::create_dir_all(&current)?;
        std::fs::create_dir_all(&latest)?;

        let rollout_path = temp_dir.path().join("rollout.jsonl");
        let session_meta = SessionMeta {
            cwd: current.clone(),
            ..SessionMeta::default()
        };
        let lines = vec![
            RolloutLine {
                timestamp: "t0".to_string(),
                item: RolloutItem::SessionMeta(SessionMetaLine {
                    meta: session_meta,
                    git: None,
                }),
            },
            RolloutLine {
                timestamp: "t1".to_string(),
                item: RolloutItem::TurnContext(build_turn_context(&config, latest.clone())),
            },
        ];
        let mut text = String::new();
        for line in lines {
            text.push_str(&serde_json::to_string(&line).expect("serialize rollout"));
            text.push('\n');
        }
        std::fs::write(&rollout_path, text)?;

        let session_cwd = read_session_cwd(&rollout_path).await.expect("expected cwd");
        assert_eq!(session_cwd, latest);
        assert!(cwds_differ(&current, &session_cwd));
        Ok(())
    }

    #[tokio::test]
    async fn config_rebuild_changes_trust_defaults_with_cwd() -> std::io::Result<()> {
        let temp_dir = TempDir::new()?;
        let savfox_home = temp_dir.path().to_path_buf();
        let trusted = temp_dir.path().join("trusted");
        let untrusted = temp_dir.path().join("untrusted");
        std::fs::create_dir_all(&trusted)?;
        std::fs::create_dir_all(&untrusted)?;

        // TOML keys need escaped backslashes on Windows paths.
        let trusted_display = trusted.display().to_string().replace('\\', "\\\\");
        let untrusted_display = untrusted.display().to_string().replace('\\', "\\\\");
        let config_toml = format!(
            r#"[projects."{trusted_display}"]
trust_level = "trusted"

[projects."{untrusted_display}"]
trust_level = "untrusted"
"#
        );
        std::fs::write(temp_dir.path().join("config.toml"), config_toml)?;

        let trusted_overrides = ConfigOverrides {
            cwd: Some(trusted.clone()),
            ..Default::default()
        };
        let trusted_config = ConfigBuilder::default()
            .savfox_home(savfox_home.clone())
            .harness_overrides(trusted_overrides.clone())
            .build()
            .await?;
        assert_eq!(
            trusted_config.approval_policy.value(),
            AskForApproval::OnRequest
        );

        let untrusted_overrides = ConfigOverrides {
            cwd: Some(untrusted),
            ..trusted_overrides
        };
        let untrusted_config = ConfigBuilder::default()
            .savfox_home(savfox_home)
            .harness_overrides(untrusted_overrides)
            .build()
            .await?;
        assert_eq!(
            untrusted_config.approval_policy.value(),
            AskForApproval::UnlessTrusted
        );
        Ok(())
    }

    #[tokio::test]
    async fn read_session_cwd_falls_back_to_session_meta() -> std::io::Result<()> {
        let temp_dir = TempDir::new()?;
        let _config = build_config(&temp_dir).await?;
        let session_cwd = temp_dir.path().join("session");
        std::fs::create_dir_all(&session_cwd)?;

        let rollout_path = temp_dir.path().join("rollout.jsonl");
        let session_meta = SessionMeta {
            cwd: session_cwd.clone(),
            ..SessionMeta::default()
        };
        let meta_line = RolloutLine {
            timestamp: "t0".to_string(),
            item: RolloutItem::SessionMeta(SessionMetaLine {
                meta: session_meta,
                git: None,
            }),
        };
        let text = format!(
            "{}\n",
            serde_json::to_string(&meta_line).expect("serialize meta")
        );
        std::fs::write(&rollout_path, text)?;

        let cwd = read_session_cwd(&rollout_path).await.expect("expected cwd");
        assert_eq!(cwd, session_cwd);
        Ok(())
    }

    #[tokio::test]
    async fn skips_provider_setup_when_provider_store_is_configured() -> std::io::Result<()> {
        let temp_dir = TempDir::new()?;
        let config = build_config(&temp_dir).await?;
        let models_dir = temp_dir.path().join("models");
        std::fs::create_dir_all(&models_dir)?;
        std::fs::write(
            models_dir.join("zhipuai-coding-plan.json"),
            r#"{
  "version": 2,
  "provider_id": "zhipuai-coding-plan",
  "name": "ZhipuAI Coding Plan",
  "auth": {
    "type": "api_key",
    "env_key": "ZHIPUAI_API_KEY",
    "api_key": "sk-test-zhipu"
  },
  "disabled_models": []
}"#,
        )?;

        let connected = has_connected_provider(&config);
        assert!(connected);
        assert!(!should_show_provider_setup_screen(&config, connected));
        Ok(())
    }

    #[tokio::test]
    async fn auto_repairs_model_when_provider_missing_from_store() -> std::io::Result<()> {
        let temp_dir = TempDir::new()?;
        let models_dir = temp_dir.path().join("models");
        std::fs::create_dir_all(&models_dir)?;
        std::fs::write(
            models_dir.join("zhipuai-coding-plan.json"),
            r#"{
  "version": 2,
  "provider_id": "zhipuai-coding-plan",
  "disabled_models": []
}"#,
        )?;

        let config_toml = ConfigToml {
            model: Some(SelectedModel {
                slug: "missing-model".to_string(),
                provider: "missing-provider".to_string(),
                reasoning_effort: None,
            }),
            ..ConfigToml::default()
        };

        let warning = maybe_repair_model_from_provider_store(temp_dir.path(), &config_toml).await?;
        assert!(warning.is_some());

        let updated: ConfigToml = toml::from_str(&std::fs::read_to_string(
            temp_dir.path().join("config.toml"),
        )?)
        .expect("parse updated config");
        assert_eq!(
            updated.model.as_ref().and_then(SelectedModel::to_model_id),
            Some("zhipuai-coding-plan/glm-5".to_string())
        );
        Ok(())
    }

    #[tokio::test]
    async fn auto_repairs_model_when_provider_exists_but_model_missing() -> std::io::Result<()> {
        let temp_dir = TempDir::new()?;
        let models_dir = temp_dir.path().join("models");
        std::fs::create_dir_all(&models_dir)?;
        std::fs::write(
            models_dir.join("zhipuai-coding-plan.json"),
            r#"{
  "version": 2,
  "provider_id": "zhipuai-coding-plan",
  "disabled_models": []
}"#,
        )?;

        let config_toml = ConfigToml {
            model: Some(SelectedModel {
                slug: "does-not-exist".to_string(),
                provider: "zhipuai-coding-plan".to_string(),
                reasoning_effort: None,
            }),
            ..ConfigToml::default()
        };

        let warning = maybe_repair_model_from_provider_store(temp_dir.path(), &config_toml).await?;
        assert!(warning.is_some());

        let updated: ConfigToml = toml::from_str(&std::fs::read_to_string(
            temp_dir.path().join("config.toml"),
        )?)
        .expect("parse updated config");
        assert_eq!(
            updated.model.as_ref().and_then(SelectedModel::to_model_id),
            Some("zhipuai-coding-plan/glm-4.5".to_string())
        );
        Ok(())
    }
}
