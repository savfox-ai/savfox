use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::Parser;
use savfox_core::config::CONFIG_TOML_FILE;
use owo_colors::OwoColorize;
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
            provider: "openai".to_string(),
            api_key: None,
            model: "gpt-4.1".to_string(),
            channel_type: Some("webhook".to_string()),
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
    Ok(input.trim().to_string())
}

fn prompt_input_default(prompt: &str, default: &str) -> std::io::Result<String> {
    let full_prompt = format!("{prompt} [{default}]: ");
    let input = prompt_input(&full_prompt)?;
    if input.is_empty() {
        Ok(default.to_string())
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

fn normalize_provider(value: &str) -> Option<String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "openai" => Some("openai".to_string()),
        "anthropic" => Some("anthropic".to_string()),
        "groq" => Some("groq".to_string()),
        "ollama" => Some("ollama".to_string()),
        _ => None,
    }
}

fn provider_requires_api_key(provider: &str) -> bool {
    !matches!(provider, "ollama")
}

fn provider_env_var(provider: &str) -> Option<&'static str> {
    match provider {
        "openai" => Some("OPENAI_API_KEY"),
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "groq" => Some("GROQ_API_KEY"),
        _ => None,
    }
}

fn provider_models(provider: &str) -> &'static [(&'static str, &'static str)] {
    match provider {
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
        _ => &[
            ("gpt-4.1", "Best for most tasks"),
            ("gpt-4.1-mini", "Faster and cheaper"),
            ("o4-mini", "Reasoning-focused model"),
        ],
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

    if non_interactive {
        let provider = std::env::var("SAVFOX_MODEL_PROVIDER")
            .ok()
            .and_then(|v| normalize_provider(&v))
            .unwrap_or_else(|| state.provider.clone());
        if normalize_provider(&provider).is_none() {
            anyhow::bail!("invalid SAVFOX_MODEL_PROVIDER value: {provider}");
        }
        state.provider = provider;
        println!("Using provider: {}", state.provider);
        println!();
        return Ok(());
    }

    println!("Available providers:");
    println!("  1. openai");
    println!("  2. anthropic");
    println!("  3. groq");
    println!("  4. ollama (local)");
    println!();

    loop {
        let selected = prompt_input_default("Choose provider (name or number)", "1")?;
        let provider = match selected.trim() {
            "1" => Some("openai".to_string()),
            "2" => Some("anthropic".to_string()),
            "3" => Some("groq".to_string()),
            "4" => Some("ollama".to_string()),
            raw => normalize_provider(raw),
        };
        if let Some(provider) = provider {
            state.provider = provider;
            println!("Selected provider: {}", state.provider);
            println!();
            return Ok(());
        }
        println!("Invalid provider. Try one of: openai, anthropic, groq, ollama.");
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

    let env_var = provider_env_var(&state.provider).unwrap_or("OPENAI_API_KEY");
    if let Ok(existing) = std::env::var(env_var) {
        if api_key_looks_valid(&state.provider, &existing) {
            let masked = if existing.len() > 8 {
                format!("{}...{}", &existing[..4], &existing[existing.len() - 4..])
            } else {
                "****".to_string()
            };
            println!("Found existing key in {env_var}: {masked}");
            if non_interactive || prompt_yes_no("Use this key?", true)? {
                state.api_key = Some(existing);
                println!();
                return Ok(());
            }
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
        let key = prompt_input(&format!("Enter {}: ", env_var))?;
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
    println!("Suggested models:");
    for (index, (name, description)) in models.iter().enumerate() {
        println!("  {}. {} - {}", index + 1, name, description);
    }
    println!();

    if non_interactive {
        let model = std::env::var("SAVFOX_MODEL")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| models[0].0.to_string());
        state.model = model;
        println!("Using model: {}", state.model);
        println!();
        return Ok(());
    }

    loop {
        let choice = prompt_input_default("Select model (number or name)", "1")?;
        if let Ok(index) = choice.parse::<usize>() {
            if index >= 1 && index <= models.len() {
                state.model = models[index - 1].0.to_string();
                break;
            }
        }
        if !choice.trim().is_empty() {
            state.model = choice.trim().to_string();
            break;
        }
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
            .unwrap_or_else(|| "webhook".to_string());
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
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .build()?;

    let provider = state.provider.as_str();
    let key = state
        .api_key
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("missing API key for provider '{}'", provider))?;

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
            Some(true) => "configured and verified".to_string(),
            Some(false) => "configured but not verified".to_string(),
            None => {
                if provider_requires_api_key(&state.provider) {
                    "required but missing".to_string()
                } else {
                    "not required".to_string()
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
                true,
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
