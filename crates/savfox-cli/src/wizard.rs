use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::Parser;
use owo_colors::OwoColorize;
use savfox_core::config::CONFIG_TOML_FILE;
use savfox_http_client::custom_ca::build_reqwest_client_with_custom_ca;
use serde::{Deserialize, Serialize};

/// Interactive setup wizard.
#[derive(Debug, Parser)]
pub struct WizardCommand {
    /// Skip interactive prompts and require env-driven defaults.
    #[clap(long)]
    pub non_interactive: bool,

    /// Resume from the last saved wizard progress without prompting.
    #[clap(long)]
    pub resume: bool,
}

const SUPPORTED_CHANNELS: [&str; 4] = ["discord", "telegram", "slack", "webhook"];
const PROVIDER_LIST_DISPLAY_LIMIT: usize = 12;
const WIZARD_PROVIDER_PRIORITY: [&str; 17] = [
    "openai",
    "anthropic",
    "gemini",
    "openrouter",
    "groq",
    "xai",
    "deepseek",
    "mistral",
    "together",
    "qwen",
    "minimax",
    "kimi-for-coding",
    "moonshotai",
    "bedrock",
    "ollama",
    "ollama-chat",
    "lmstudio",
];

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Default)]
#[serde(rename_all = "snake_case")]
enum WizardStep {
    #[default]
    Provider,
    ApiKey,
    Model,
    Channel,
    ConnectionTest,
    Complete,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WizardState {
    provider: String,
    api_key: Option<String>,
    model: String,
    channel_type: Option<String>,
    api_key_valid: Option<bool>,
}

impl Default for WizardState {
    fn default() -> Self {
        Self {
            provider: "openai".to_owned(),
            api_key: None,
            model: "gpt-4.1".to_owned(),
            channel_type: Some("webhook".to_owned()),
            api_key_valid: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WizardProgress {
    next_step: WizardStep,
    state: WizardState,
}

impl Default for WizardProgress {
    fn default() -> Self {
        Self {
            next_step: WizardStep::Provider,
            state: WizardState::default(),
        }
    }
}

fn resolve_savfox_home() -> anyhow::Result<PathBuf> {
    if let Ok(raw) = std::env::var("SAVFOX_HOME") {
        let path = PathBuf::from(raw);
        std::fs::create_dir_all(&path)
            .map_err(|e| anyhow::anyhow!("failed to create SAVFOX_HOME {}: {e}", path.display()))?;
        return Ok(path);
    }

    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not determine home directory"))?;
    let savfox_home = home.join(".savfox");
    std::fs::create_dir_all(&savfox_home).map_err(|e| {
        anyhow::anyhow!(
            "failed to create savfox home {}: {e}",
            savfox_home.display()
        )
    })?;
    Ok(savfox_home)
}

fn wizard_progress_path(savfox_home: &Path) -> PathBuf {
    savfox_home.join("wizard-progress.json")
}

fn load_progress(path: &Path) -> Option<WizardProgress> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<WizardProgress>(&content).ok()
}

fn save_progress(path: &Path, progress: &WizardProgress) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            anyhow::anyhow!(
                "failed to create wizard progress directory {}: {e}",
                parent.display()
            )
        })?;
    }
    let payload = serde_json::to_string_pretty(progress)?;
    std::fs::write(path, payload)
        .map_err(|e| anyhow::anyhow!("failed to persist wizard progress {}: {e}", path.display()))
}

fn clear_progress(path: &Path) {
    let _ = std::fs::remove_file(path);
}

fn step_label(step: WizardStep) -> &'static str {
    match step {
        WizardStep::Provider => "provider selection",
        WizardStep::ApiKey => "API key setup",
        WizardStep::Model => "model selection",
        WizardStep::Channel => "channel setup",
        WizardStep::ConnectionTest => "connection test",
        WizardStep::Complete => "complete",
    }
}

fn print_banner(first_run: bool) {
    println!();
    println!("{}", "===========================================".bold());
    println!("{}", "       Savfox Setup Wizard".bold());
    println!("{}", "===========================================".bold());
    println!();
    if first_run {
        println!("No config file detected. Running first-time setup.");
    } else {
        println!("This wizard will help you reconfigure Savfox.");
    }
    println!("You can re-run it at any time with `savfox wizard`.");
    println!();
}

fn prompt_input(prompt: &str) -> std::io::Result<String> {
    print!("{prompt}");
    std::io::stdout().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    Ok(input.trim().to_owned())
}

fn prompt_input_default(prompt: &str, default: &str) -> std::io::Result<String> {
    let full_prompt = format!("{prompt} [{default}]: ");
    let input = prompt_input(&full_prompt)?;
    if input.is_empty() {
        Ok(default.to_owned())
    } else {
        Ok(input)
    }
}

fn prompt_yes_no(prompt: &str, default_yes: bool) -> std::io::Result<bool> {
    let hint = if default_yes { "[Y/n]" } else { "[y/N]" };
    let full_prompt = format!("{prompt} {hint}: ");
    let input = prompt_input(&full_prompt)?;
    if input.is_empty() {
        Ok(default_yes)
    } else {
        Ok(input.eq_ignore_ascii_case("y") || input.eq_ignore_ascii_case("yes"))
    }
}

#[derive(Debug, Clone)]
struct ProviderOption {
    id: String,
    name: String,
    requires_api_key: bool,
}

fn provider_options() -> Vec<ProviderOption> {
    let model_providers = savfox_core::built_in_model_providers();

    let mut ordered_ids: Vec<String> = Vec::new();
    for provider_id in WIZARD_PROVIDER_PRIORITY {
        if model_providers.contains_key(provider_id) {
            ordered_ids.push(provider_id.to_owned());
        }
    }

    let mut extras: Vec<String> = model_providers
        .keys()
        .filter(|provider_id| !ordered_ids.contains(provider_id))
        .cloned()
        .collect();
    extras.sort_unstable();
    ordered_ids.extend(extras);

    ordered_ids
        .into_iter()
        .filter_map(|provider_id| {
            let info = model_providers.get(provider_id.as_str())?;
            Some(ProviderOption {
                id: provider_id.clone(),
                name: info.name.clone(),
                requires_api_key: provider_requires_api_key(provider_id.as_str()),
            })
        })
        .collect()
}

fn filter_provider_options<'a>(
    providers: &'a [ProviderOption],
    query: &str,
) -> Vec<&'a ProviderOption> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return providers.iter().collect();
    }
    let needle = trimmed.to_ascii_lowercase();
    providers
        .iter()
        .filter(|provider| {
            let haystack = format!("{} {}", provider.id, provider.name).to_ascii_lowercase();
            haystack.contains(&needle)
        })
        .collect()
}

fn resolve_provider_input(input: &str, providers: &[ProviderOption]) -> Option<String> {
    let raw = input.trim();
    if raw.is_empty() {
        return None;
    }

    if let Some(found) = providers
        .iter()
        .find(|provider| provider.id.eq_ignore_ascii_case(raw))
    {
        return Some(found.id.clone());
    }

    if let Some(found) = providers
        .iter()
        .find(|provider| provider.name.eq_ignore_ascii_case(raw))
    {
        return Some(found.id.clone());
    }

    let canonical = savfox_core::canonical_provider_id(raw);
    providers
        .iter()
        .find(|provider| provider.id.eq_ignore_ascii_case(canonical.as_str()))
        .map(|provider| provider.id.clone())
}

fn provider_requires_api_key(provider: &str) -> bool {
    !matches!(provider, "ollama" | "ollama-chat" | "lmstudio")
}

fn env_var_looks_like_secret(name: &str) -> bool {
    let lowered = name.to_ascii_lowercase();
    lowered.contains("api_key")
        || lowered.contains("apikey")
        || lowered.contains("token")
        || lowered.contains("secret")
}

fn fallback_env_var_for_provider(provider: &str) -> String {
    format!(
        "{}_API_KEY",
        provider.replace(['-', '/'], "_").to_ascii_uppercase()
    )
}

fn provider_env_var(provider: &str) -> Option<String> {
    let providers = savfox_core::built_in_model_providers();
    let info = providers.get(provider)?;

    if let Some(env_key) = info
        .env_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(env_key.to_owned());
    }

    if let Some(headers) = &info.env_http_headers {
        let mut candidates: Vec<String> = headers
            .values()
            .map(String::as_str)
            .filter(|name| env_var_looks_like_secret(name))
            .map(str::to_owned)
            .collect();
        candidates.sort_unstable();
        candidates.dedup();
        if let Some(first) = candidates.first() {
            return Some(first.clone());
        }
    }

    None
}

fn provider_models(provider: &str) -> &'static [(&'static str, &'static str)] {
    match provider {
        "openai" => &[
            ("gpt-4.1", "Best for most tasks"),
            ("gpt-4.1-mini", "Faster and cheaper"),
            ("o4-mini", "Reasoning-focused model"),
        ],
        "anthropic" => &[
            ("claude-sonnet-4-20250514", "Balanced Anthropic model"),
            ("claude-opus-4-20250514", "Higher quality Anthropic model"),
        ],
        "groq" => &[
            ("llama-3.3-70b-versatile", "High quality Groq-hosted model"),
            ("llama-3.1-8b-instant", "Fast Groq-hosted model"),
        ],
        "ollama" => &[
            ("ollama/llama3.1", "Local model via Ollama"),
            ("ollama/qwen2.5-coder", "Local coding-focused model"),
        ],
        "lmstudio" => &[("lmstudio/local-model", "Local model via LM Studio")],
        _ => &[],
    }
}

fn normalize_channel(value: &str) -> Option<String> {
    let channel = value.trim().to_ascii_lowercase();
    if SUPPORTED_CHANNELS.contains(&channel.as_str()) {
        Some(channel)
    } else {
        None
    }
}

fn api_key_looks_valid(provider: &str, key: &str) -> bool {
    let key = key.trim();
    if key.len() < 12 {
        return false;
    }
    match provider {
        "openai" => key.starts_with("sk-"),
        "anthropic" => key.starts_with("sk-ant-"),
        "groq" => key.starts_with("gsk_"),
        _ => true,
    }
}

fn step_provider_selection(state: &mut WizardState, non_interactive: bool) -> anyhow::Result<()> {
    println!("{}", "Step 1: Provider Selection".bold());
    println!("{}", "-------------------------".bold());
    let providers = provider_options();
    if providers.is_empty() {
        anyhow::bail!("no built-in model providers are available");
    }

    if non_interactive {
        let requested = std::env::var("SAVFOX_MODEL_PROVIDER")
            .ok()
            .unwrap_or_else(|| state.provider.clone());
        let provider = resolve_provider_input(requested.as_str(), &providers)
            .ok_or_else(|| anyhow::anyhow!("invalid SAVFOX_MODEL_PROVIDER value: {requested}"))?;
        state.provider = provider;
        println!("Using provider: {}", state.provider);
        println!();
        return Ok(());
    }

    println!("Type to search providers, then select by number or provider id.");
    println!(
        "If the list is long, refine the search until your provider appears (use /all to reset)."
    );
    println!();

    let mut query = String::new();
    loop {
        let filtered = filter_provider_options(&providers, query.as_str());
        if filtered.is_empty() {
            println!("No providers match '{query}'. Enter a new search.");
            println!();
            query.clear();
            continue;
        }

        let visible = filtered.len().min(PROVIDER_LIST_DISPLAY_LIMIT);
        if query.trim().is_empty() {
            println!("Providers (showing {visible} of {}):", filtered.len());
        } else {
            println!(
                "Providers matching '{}' (showing {visible} of {}):",
                query,
                filtered.len()
            );
        }
        for (index, provider) in filtered.iter().take(visible).enumerate() {
            let auth_note = if provider.requires_api_key {
                "API key"
            } else {
                "local/no API key"
            };
            println!(
                "  {}. {} ({}) - {}",
                index + 1,
                provider.name,
                provider.id,
                auth_note
            );
        }
        if filtered.len() > visible {
            println!(
                "  ... {} more provider(s). Refine your search to narrow the list.",
                filtered.len() - visible
            );
        }
        println!();

        let selected = prompt_input("Pick number/provider id, or type search text: ")?;
        let trimmed = selected.trim();
        if trimmed.eq_ignore_ascii_case("/all") {
            query.clear();
            println!();
            continue;
        }
        if let Ok(index) = trimmed.parse::<usize>() {
            if index >= 1 && index <= visible {
                let provider = filtered[index - 1].id.clone();
                state.provider = provider;
                println!("Selected provider: {}", state.provider);
                println!();
                return Ok(());
            }
            println!("Selection out of range. Choose 1-{visible}.");
            println!();
            continue;
        }

        if let Some(provider) = resolve_provider_input(trimmed, &providers) {
            state.provider = provider;
            println!("Selected provider: {}", state.provider);
            println!();
            return Ok(());
        }

        if !trimmed.is_empty() {
            query = trimmed.to_owned();
            println!();
            continue;
        }

        if visible == 1 {
            state.provider = filtered[0].id.clone();
            println!("Selected provider: {}", state.provider);
            println!();
            return Ok(());
        }

        println!("Enter a number, provider id, or search text.");
        println!();
    }
}

fn step_api_key(state: &mut WizardState, non_interactive: bool) -> anyhow::Result<()> {
    println!("{}", "Step 2: API Key Setup".bold());
    println!("{}", "---------------------".bold());

    if !provider_requires_api_key(&state.provider) {
        state.api_key = None;
        state.api_key_valid = Some(true);
        println!(
            "Provider '{}' does not require a cloud API key.",
            state.provider
        );
        println!();
        return Ok(());
    }

    let env_var = provider_env_var(&state.provider)
        .unwrap_or_else(|| fallback_env_var_for_provider(&state.provider));
    if let Ok(existing) = std::env::var(env_var.as_str())
        && api_key_looks_valid(&state.provider, &existing)
    {
        let masked = if existing.len() > 8 {
            format!("{}...{}", &existing[..4], &existing[existing.len() - 4..])
        } else {
            "****".to_owned()
        };
        println!("Found existing key in {env_var}: {masked}");
        if non_interactive || prompt_yes_no("Use this key?", true)? {
            state.api_key = Some(existing);
            println!();
            return Ok(());
        }
    }

    if non_interactive {
        anyhow::bail!(
            "missing valid API key for provider '{}'; set {} in the environment",
            state.provider,
            env_var
        );
    }

    loop {
        let key = prompt_input(&format!("Enter {env_var}: "))?;
        if key.is_empty() {
            println!("API key is required for provider '{}'.", state.provider);
            continue;
        }
        if !api_key_looks_valid(&state.provider, &key) {
            println!(
                "Key format does not look valid for provider '{}'.",
                state.provider
            );
            continue;
        }
        state.api_key = Some(key);
        println!("API key recorded for this wizard run.");
        println!();
        return Ok(());
    }
}

fn step_model_selection(state: &mut WizardState, non_interactive: bool) -> anyhow::Result<()> {
    println!("{}", "Step 3: Model Selection".bold());
    println!("{}", "-----------------------".bold());

    let models = provider_models(&state.provider);
    println!("Provider: {}", state.provider);
    if models.is_empty() {
        println!(
            "No curated model suggestions for '{}'. Enter a model id manually.",
            state.provider
        );
        println!();

        if non_interactive {
            let model = std::env::var("SAVFOX_MODEL")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .or_else(|| (!state.model.trim().is_empty()).then_some(state.model.clone()))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "missing SAVFOX_MODEL for provider '{}' (no default model list available)",
                        state.provider
                    )
                })?;
            state.model = model;
            println!("Using model: {}", state.model);
            println!();
            return Ok(());
        }

        loop {
            let model = if state.model.trim().is_empty() {
                prompt_input("Enter model id: ")?
            } else {
                prompt_input_default("Enter model id", state.model.as_str())?
            };
            if model.trim().is_empty() {
                println!("Model cannot be empty.");
                continue;
            }
            state.model = model.trim().to_owned();
            println!("Selected model: {}", state.model);
            println!();
            return Ok(());
        }
    }

    println!("Suggested models:");
    for (index, (name, description)) in models.iter().enumerate() {
        println!("  {}. {} - {}", index + 1, name, description);
    }
    println!();

    if non_interactive {
        let model = std::env::var("SAVFOX_MODEL")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| (!state.model.trim().is_empty()).then_some(state.model.clone()))
            .unwrap_or_else(|| models[0].0.to_owned());
        state.model = model;
        println!("Using model: {}", state.model);
        println!();
        return Ok(());
    }

    loop {
        let choice = prompt_input_default("Select model (number or name)", "1")?;
        if let Ok(index) = choice.parse::<usize>()
            && index >= 1
            && index <= models.len()
        {
            state.model = models[index - 1].0.to_owned();
            break;
        }
        if !choice.trim().is_empty() {
            state.model = choice.trim().to_owned();
            break;
        }
        println!("Model cannot be empty.");
    }

    if state.model.trim().is_empty() {
        anyhow::bail!("model selection cannot be empty");
    }
    println!("Selected model: {}", state.model);
    println!();
    Ok(())
}

fn step_channel_config(state: &mut WizardState, non_interactive: bool) -> anyhow::Result<()> {
    println!("{}", "Step 4: Channel Setup".bold());
    println!("{}", "---------------------".bold());

    if non_interactive {
        let channel = std::env::var("SAVFOX_WIZARD_CHANNEL")
            .ok()
            .and_then(|v| normalize_channel(&v))
            .unwrap_or_else(|| "webhook".to_owned());
        state.channel_type = Some(channel.clone());
        println!("Using channel: {channel}");
        println!();
        return Ok(());
    }

    println!("Choose your first channel integration:");
    println!("  - discord");
    println!("  - telegram");
    println!("  - slack");
    println!("  - webhook");
    println!();

    loop {
        let raw = prompt_input_default("Channel", "webhook")?;
        if let Some(channel) = normalize_channel(&raw) {
            state.channel_type = Some(channel.clone());
            println!("Selected channel: {channel}");
            println!();
            return Ok(());
        }
        println!(
            "Invalid channel. Choose one of: {}",
            SUPPORTED_CHANNELS.join(", ")
        );
    }
}

async fn run_connection_probe(state: &WizardState) -> anyhow::Result<bool> {
    let client = build_reqwest_client_with_custom_ca(
        reqwest::Client::builder().timeout(Duration::from_secs(12)),
    )?;

    let provider = state.provider.as_str();
    let key = state
        .api_key
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("missing API key for provider '{provider}'"))?;

    let response = match provider {
        "openai" => {
            client
                .get("https://api.openai.com/v1/models")
                .header("Authorization", format!("Bearer {key}"))
                .send()
                .await
        }
        "anthropic" => {
            client
                .get("https://api.anthropic.com/v1/models")
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01")
                .send()
                .await
        }
        "groq" => {
            client
                .get("https://api.groq.com/openai/v1/models")
                .header("Authorization", format!("Bearer {key}"))
                .send()
                .await
        }
        _ => return Ok(true),
    };

    match response {
        Ok(resp) => Ok(resp.status().is_success()),
        Err(_) => Ok(false),
    }
}

async fn step_test_connection(
    state: &mut WizardState,
    non_interactive: bool,
) -> anyhow::Result<()> {
    println!("{}", "Step 5: Test Connection".bold());
    println!("{}", "----------------------".bold());

    if !provider_requires_api_key(&state.provider) {
        state.api_key_valid = Some(true);
        println!(
            "Skipping remote API test for local provider '{}'.",
            state.provider
        );
        println!();
        return Ok(());
    }

    loop {
        println!("Testing {} connection...", state.provider);
        let success = run_connection_probe(state).await?;
        state.api_key_valid = Some(success);
        if success {
            println!("  {} connection successful", "[OK]".green());
            println!();
            return Ok(());
        }

        println!("  {} connection failed", "[FAIL]".red());
        if non_interactive {
            anyhow::bail!(
                "connection validation failed for provider '{}'",
                state.provider
            );
        }
        if !prompt_yes_no("Retry connection test?", true)? {
            anyhow::bail!("wizard stopped because connection test failed");
        }
    }
}

fn step_summary(state: &WizardState, savfox_home: &Path, first_run: bool) {
    println!("{}", "Summary".bold());
    println!("{}", "-------".bold());
    println!();
    println!("  Savfox home: {}", savfox_home.display());
    println!("  Provider:    {}", state.provider);
    println!("  Model:       {}", state.model);
    println!(
        "  Channel:     {}",
        state.channel_type.as_deref().unwrap_or("not configured")
    );
    println!(
        "  API key:     {}",
        match state.api_key_valid {
            Some(true) => "configured and verified".to_owned(),
            Some(false) => "configured but not verified".to_owned(),
            None => {
                if provider_requires_api_key(&state.provider) {
                    "required but missing".to_owned()
                } else {
                    "not required".to_owned()
                }
            }
        }
    );
    println!();

    if first_run {
        println!(
            "Config file target: {}",
            savfox_home.join(CONFIG_TOML_FILE).display()
        );
        println!("Create or edit config as needed, then run `savfox doctor`.");
    }

    println!("{}", "Next steps:".bold());
    println!("  - Run diagnostics: savfox doctor");
    println!("  - Start gateway: savfox gateway --port 18881");
    println!("  - Verify channel login in dashboard or RPC");
    println!();
    println!("{}", "Setup complete!".bold());
}

pub async fn run_wizard(cmd: WizardCommand) -> anyhow::Result<()> {
    let savfox_home = resolve_savfox_home()?;
    let first_run = !savfox_home.join(CONFIG_TOML_FILE).exists();
    let progress_path = wizard_progress_path(&savfox_home);

    print_banner(first_run);

    let mut progress = WizardProgress::default();
    if let Some(saved) = load_progress(&progress_path)
        && saved.next_step != WizardStep::Complete
    {
        let should_resume = if cmd.resume || cmd.non_interactive {
            true
        } else {
            prompt_yes_no(
                &format!(
                    "Found interrupted setup at '{}'. Resume?",
                    step_label(saved.next_step)
                ),
                false,
            )?
        };
        if should_resume {
            println!("Resuming from {}.", step_label(saved.next_step));
            println!();
            progress = saved;
        } else {
            clear_progress(&progress_path);
        }
    }

    if progress.next_step <= WizardStep::Provider {
        if let Err(err) = step_provider_selection(&mut progress.state, cmd.non_interactive) {
            let _ = save_progress(&progress_path, &progress);
            return Err(err);
        }
        progress.next_step = WizardStep::ApiKey;
        save_progress(&progress_path, &progress)?;
    }

    if progress.next_step <= WizardStep::ApiKey {
        if let Err(err) = step_api_key(&mut progress.state, cmd.non_interactive) {
            let _ = save_progress(&progress_path, &progress);
            return Err(err);
        }
        progress.next_step = WizardStep::Model;
        save_progress(&progress_path, &progress)?;
    }

    if progress.next_step <= WizardStep::Model {
        if let Err(err) = step_model_selection(&mut progress.state, cmd.non_interactive) {
            let _ = save_progress(&progress_path, &progress);
            return Err(err);
        }
        progress.next_step = WizardStep::Channel;
        save_progress(&progress_path, &progress)?;
    }

    if progress.next_step <= WizardStep::Channel {
        if let Err(err) = step_channel_config(&mut progress.state, cmd.non_interactive) {
            let _ = save_progress(&progress_path, &progress);
            return Err(err);
        }
        progress.next_step = WizardStep::ConnectionTest;
        save_progress(&progress_path, &progress)?;
    }

    if progress.next_step <= WizardStep::ConnectionTest {
        if let Err(err) = step_test_connection(&mut progress.state, cmd.non_interactive).await {
            let _ = save_progress(&progress_path, &progress);
            return Err(err);
        }
        progress.next_step = WizardStep::Complete;
        save_progress(&progress_path, &progress)?;
    }

    step_summary(&progress.state, &savfox_home, first_run);
    clear_progress(&progress_path);
    Ok(())
}
