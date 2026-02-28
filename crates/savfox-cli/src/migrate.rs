use std::collections::HashMap;
use std::path::{Path, PathBuf};

use clap::{Parser, ValueEnum};
use owo_colors::OwoColorize;
use tracing::{info, warn};

/// Migrate configuration from an OpenClaw (TypeScript) installation to Savfox.
#[derive(Debug, Parser)]
pub struct MigrateCommand {
    /// Path to the OpenClaw installation directory containing config.yaml or config.json.
    #[arg(value_name = "SOURCE_DIR")]
    pub source_dir: PathBuf,

    /// Output directory for the converted Savfox config. Defaults to the current directory.
    #[arg(short = 'o', long = "output", value_name = "OUTPUT_DIR")]
    pub output_dir: Option<PathBuf>,

    /// Perform a dry run without writing any files.
    #[arg(long, default_value_t = false)]
    pub dry_run: bool,

    /// Output format for the converted config.
    #[arg(long, value_enum, default_value_t = OutputFormat::Toml)]
    pub format: OutputFormat,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum OutputFormat {
    Toml,
    Json,
}

// ---------------------------------------------------------------------------
// OpenClaw source types
// ---------------------------------------------------------------------------

/// Top-level OpenClaw configuration (config.yaml / config.json).
#[derive(Debug, serde::Deserialize)]
struct OpenClawConfig {
    #[serde(default)]
    server: Option<OpenClawServerConfig>,

    #[serde(default)]
    model: Option<OpenClawModelConfig>,

    #[serde(default)]
    channels: Option<Vec<OpenClawChannelConfig>>,

    #[serde(default)]
    skills: Option<Vec<OpenClawSkillConfig>>,

    #[serde(default)]
    memory: Option<OpenClawMemoryConfig>,
}

#[derive(Debug, serde::Deserialize)]
struct OpenClawServerConfig {
    #[serde(default)]
    port: Option<u16>,

    #[serde(default)]
    token: Option<String>,

    #[serde(default)]
    host: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct OpenClawModelConfig {
    #[serde(default)]
    default: Option<String>,

    #[serde(default)]
    providers: Option<Vec<OpenClawProviderConfig>>,
}

#[derive(Debug, serde::Deserialize)]
struct OpenClawProviderConfig {
    #[serde(default)]
    name: Option<String>,

    #[serde(default)]
    base_url: Option<String>,

    #[serde(default)]
    api_key: Option<String>,

    #[serde(default)]
    env_key: Option<String>,

    #[serde(default)]
    models: Option<Vec<String>>,
}

#[derive(Debug, serde::Deserialize)]
struct OpenClawChannelConfig {
    #[serde(default, alias = "type")]
    channel_type: Option<String>,

    #[serde(default)]
    enabled: Option<bool>,

    /// Catch-all for channel-specific configuration fields.
    #[serde(flatten)]
    config: HashMap<String, serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
struct OpenClawSkillConfig {
    #[serde(default)]
    name: Option<String>,

    #[serde(default)]
    path: Option<String>,

    #[serde(default)]
    enabled: Option<bool>,
}

#[derive(Debug, serde::Deserialize)]
struct OpenClawMemoryConfig {
    #[serde(default)]
    enabled: Option<bool>,

    #[serde(default)]
    max_entries: Option<u64>,

    #[serde(default)]
    budget_kb: Option<u64>,

    #[serde(default)]
    layers: Option<Vec<String>>,
}

// ---------------------------------------------------------------------------
// Savfox target types (TOML serialization)
// ---------------------------------------------------------------------------

#[derive(Debug, Default, serde::Serialize)]
struct SavfoxConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,

    #[serde(skip_serializing_if = "HashMap::is_empty")]
    model_providers: HashMap<String, SavfoxProviderConfig>,

    #[serde(skip_serializing_if = "Option::is_none")]
    gateway: Option<SavfoxGatewayConfig>,

    #[serde(skip_serializing_if = "Option::is_none")]
    skills: Option<SavfoxSkillsConfig>,

    #[serde(skip_serializing_if = "Option::is_none")]
    memory: Option<SavfoxMemoryConfig>,
}

#[derive(Debug, serde::Serialize)]
struct SavfoxProviderConfig {
    name: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    base_url: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    env_key: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    wire_api: Option<String>,
}

#[derive(Debug, serde::Serialize)]
struct SavfoxGatewayConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    host: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    port: Option<u16>,

    #[serde(skip_serializing_if = "Option::is_none")]
    token: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    bridges: Option<SavfoxBridgesConfig>,
}

#[derive(Debug, Default, serde::Serialize)]
struct SavfoxBridgesConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    discord: Option<SavfoxDiscordBridge>,

    #[serde(skip_serializing_if = "Option::is_none")]
    telegram: Option<SavfoxTelegramBridge>,

    #[serde(skip_serializing_if = "Option::is_none")]
    slack: Option<SavfoxSlackBridge>,

    #[serde(skip_serializing_if = "Option::is_none")]
    webhook: Option<SavfoxWebhookBridge>,

    #[serde(skip_serializing_if = "Option::is_none")]
    msteams: Option<SavfoxMsTeamsBridge>,

    #[serde(skip_serializing_if = "Option::is_none")]
    whatsapp: Option<SavfoxWhatsAppBridge>,
}

#[derive(Debug, serde::Serialize)]
struct SavfoxDiscordBridge {
    enabled: bool,
    bot_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    application_id: Option<String>,
}

#[derive(Debug, serde::Serialize)]
struct SavfoxTelegramBridge {
    enabled: bool,
    bot_token: String,
}

#[derive(Debug, serde::Serialize)]
struct SavfoxSlackBridge {
    enabled: bool,
    bot_token: String,
    signing_secret: String,
}

#[derive(Debug, serde::Serialize)]
struct SavfoxWebhookBridge {
    enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    callback_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    secret: Option<String>,
}

#[derive(Debug, serde::Serialize)]
struct SavfoxMsTeamsBridge {
    enabled: bool,
    app_id: String,
    app_password: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tenant_id: Option<String>,
}

#[derive(Debug, serde::Serialize)]
struct SavfoxWhatsAppBridge {
    enabled: bool,
    phone_number_id: String,
    access_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    verify_token: Option<String>,
}

#[derive(Debug, Default, serde::Serialize)]
struct SavfoxSkillsConfig {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    config: Vec<SavfoxSkillEntry>,
}

#[derive(Debug, serde::Serialize)]
struct SavfoxSkillEntry {
    path: String,
    enabled: bool,
}

#[derive(Debug, serde::Serialize)]
struct SavfoxMemoryConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    budget_kb: Option<u64>,
}

// ---------------------------------------------------------------------------
// Migration report
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct MigrationReport {
    converted: Vec<String>,
    warnings: Vec<String>,
    manual_steps: Vec<String>,
}

impl MigrationReport {
    fn add_converted(&mut self, item: &str) {
        self.converted.push(item.to_string());
    }

    fn add_warning(&mut self, item: &str) {
        self.warnings.push(item.to_string());
    }

    fn add_manual_step(&mut self, item: &str) {
        self.manual_steps.push(item.to_string());
    }
}

// ---------------------------------------------------------------------------
// Known channel types
// ---------------------------------------------------------------------------

const KNOWN_CHANNEL_TYPES: &[&str] = &[
    "discord", "telegram", "slack", "webhook", "msteams", "whatsapp",
];

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Attempts to locate and parse the OpenClaw config from the source directory.
fn find_and_parse_config(source_dir: &Path) -> anyhow::Result<OpenClawConfig> {
    let yaml_path = source_dir.join("config.yaml");
    let yml_path = source_dir.join("config.yml");
    let json_path = source_dir.join("config.json");

    if yaml_path.is_file() {
        info!("Found OpenClaw config at {}", yaml_path.display());
        let contents = std::fs::read_to_string(&yaml_path)?;
        let config: OpenClawConfig = serde_yaml::from_str(&contents)
            .map_err(|e| anyhow::anyhow!("Failed to parse {}: {e}", yaml_path.display()))?;
        Ok(config)
    } else if yml_path.is_file() {
        info!("Found OpenClaw config at {}", yml_path.display());
        let contents = std::fs::read_to_string(&yml_path)?;
        let config: OpenClawConfig = serde_yaml::from_str(&contents)
            .map_err(|e| anyhow::anyhow!("Failed to parse {}: {e}", yml_path.display()))?;
        Ok(config)
    } else if json_path.is_file() {
        info!("Found OpenClaw config at {}", json_path.display());
        let contents = std::fs::read_to_string(&json_path)?;
        let config: OpenClawConfig = serde_json::from_str(&contents)
            .map_err(|e| anyhow::anyhow!("Failed to parse {}: {e}", json_path.display()))?;
        Ok(config)
    } else {
        anyhow::bail!(
            "No OpenClaw config found in {}. Expected config.yaml, config.yml, or config.json.",
            source_dir.display()
        );
    }
}

// ---------------------------------------------------------------------------
// Conversion
// ---------------------------------------------------------------------------

fn convert_config(source: OpenClawConfig, report: &mut MigrationReport) -> SavfoxConfig {
    let mut savfox = SavfoxConfig::default();

    // -- Model --
    if let Some(ref model_cfg) = source.model {
        if let Some(ref default_model) = model_cfg.default {
            savfox.model = Some(default_model.clone());
            report.add_converted(&format!("model.default -> model = \"{default_model}\""));
        }

        if let Some(ref providers) = model_cfg.providers {
            for provider in providers {
                convert_provider(provider, &mut savfox.model_providers, report);
            }
        }
    }

    // -- Server -> Gateway --
    if let Some(ref server_cfg) = source.server {
        let gateway = convert_server_to_gateway(server_cfg, report);
        savfox.gateway = Some(gateway);
    }

    // -- Channels -> Gateway bridges --
    if let Some(ref channels) = source.channels {
        let bridges = convert_channels(channels, report);
        if let Some(ref mut gw) = savfox.gateway {
            gw.bridges = Some(bridges);
        } else {
            savfox.gateway = Some(SavfoxGatewayConfig {
                host: None,
                port: None,
                token: None,
                bridges: Some(bridges),
            });
        }
    }

    // -- Skills --
    if let Some(ref skills) = source.skills {
        let savfox_skills = convert_skills(skills, report);
        if !savfox_skills.config.is_empty() {
            savfox.skills = Some(savfox_skills);
        }
    }

    // -- Memory --
    if let Some(ref memory) = source.memory {
        savfox.memory = Some(convert_memory(memory, report));
    }

    savfox
}

fn convert_provider(
    provider: &OpenClawProviderConfig,
    providers: &mut HashMap<String, SavfoxProviderConfig>,
    report: &mut MigrationReport,
) {
    let name = match &provider.name {
        Some(n) => n.clone(),
        None => {
            report.add_warning("Skipping provider with no name");
            return;
        }
    };

    let env_key = provider.env_key.clone().or_else(|| {
        provider.api_key.as_ref().map(|_| {
            report.add_warning(&format!(
                "Provider \"{name}\": api_key was specified inline. \
                 For security, set an environment variable instead and use env_key."
            ));
            report.add_manual_step(&format!(
                "Provider \"{name}\": Move the inline API key to an environment variable \
                 and set env_key in the config."
            ));
            format!("{}_API_KEY", name.to_uppercase().replace('-', "_"))
        })
    });

    let wire_api = infer_wire_api(&name);

    let savfox_provider = SavfoxProviderConfig {
        name: name.clone(),
        base_url: provider.base_url.clone(),
        env_key,
        wire_api,
    };

    report.add_converted(&format!(
        "model.providers[{name}] -> [model_providers.{name}]"
    ));

    if let Some(ref models) = provider.models {
        if !models.is_empty() {
            report.add_manual_step(&format!(
                "Provider \"{name}\": OpenClaw specified models {:?}. \
                 In Savfox, use the provider prefix syntax: \"{name}/<model_name>\".",
                models
            ));
        }
    }

    providers.insert(name, savfox_provider);
}

/// Infer the wire API type based on the provider name.
fn infer_wire_api(name: &str) -> Option<String> {
    let lower = name.to_lowercase();
    if lower.contains("anthropic") || lower.contains("claude") {
        Some("anthropic".to_string())
    } else if lower.contains("ollama") && lower.contains("chat") {
        Some("chat".to_string())
    } else if lower.contains("ollama") {
        Some("chat".to_string())
    } else {
        // Default to OpenAI-compatible "responses" or "chat" — leave unset for default.
        None
    }
}

fn convert_server_to_gateway(
    server: &OpenClawServerConfig,
    report: &mut MigrationReport,
) -> SavfoxGatewayConfig {
    let mut gateway = SavfoxGatewayConfig {
        host: None,
        port: None,
        token: None,
        bridges: None,
    };

    if let Some(port) = server.port {
        gateway.port = Some(port);
        report.add_converted(&format!("server.port -> gateway.port = {port}"));
    }

    if let Some(ref token) = server.token {
        gateway.token = Some(token.clone());
        report.add_converted("server.token -> gateway.token");
    }

    if let Some(ref host) = server.host {
        gateway.host = Some(host.clone());
        report.add_converted(&format!("server.host -> gateway.host = \"{host}\""));
    }

    gateway
}

fn convert_channels(
    channels: &[OpenClawChannelConfig],
    report: &mut MigrationReport,
) -> SavfoxBridgesConfig {
    let mut bridges = SavfoxBridgesConfig::default();

    for channel in channels {
        let channel_type = match &channel.channel_type {
            Some(ct) => ct.to_lowercase(),
            None => {
                report.add_warning("Skipping channel entry with no channel_type field");
                continue;
            }
        };

        let enabled = channel.enabled.unwrap_or(true);

        if !KNOWN_CHANNEL_TYPES.contains(&channel_type.as_str()) {
            report.add_warning(&format!(
                "Unknown channel type \"{channel_type}\"; skipping. \
                 Known types: {}",
                KNOWN_CHANNEL_TYPES.join(", ")
            ));
            continue;
        }

        match channel_type.as_str() {
            "discord" => {
                let bot_token = extract_string(&channel.config, "bot_token").unwrap_or_default();
                let application_id = extract_string(&channel.config, "application_id");

                if bot_token.is_empty() {
                    report.add_warning(
                        "Discord channel: missing bot_token; bridge will need manual configuration",
                    );
                    report.add_manual_step(
                        "Discord channel: Set the bot_token field in [gateway.bridges.discord]",
                    );
                }

                bridges.discord = Some(SavfoxDiscordBridge {
                    enabled,
                    bot_token,
                    application_id,
                });
                report.add_converted("channels[discord] -> [gateway.bridges.discord]");
            }
            "telegram" => {
                let bot_token = extract_string(&channel.config, "bot_token").unwrap_or_default();

                if bot_token.is_empty() {
                    report.add_warning(
                        "Telegram channel: missing bot_token; bridge will need manual configuration",
                    );
                    report.add_manual_step(
                        "Telegram channel: Set the bot_token field in [gateway.bridges.telegram]",
                    );
                }

                bridges.telegram = Some(SavfoxTelegramBridge { enabled, bot_token });
                report.add_converted("channels[telegram] -> [gateway.bridges.telegram]");
            }
            "slack" => {
                let bot_token = extract_string(&channel.config, "bot_token").unwrap_or_default();
                let signing_secret = extract_string(&channel.config, "signing_secret")
                    .or_else(|| extract_string(&channel.config, "app_token"))
                    .unwrap_or_default();

                if bot_token.is_empty() {
                    report.add_warning(
                        "Slack channel: missing bot_token; bridge will need manual configuration",
                    );
                    report.add_manual_step(
                        "Slack channel: Set the bot_token field in [gateway.bridges.slack]",
                    );
                }
                if signing_secret.is_empty() {
                    report.add_warning(
                        "Slack channel: missing signing_secret; bridge will need manual configuration",
                    );
                    report.add_manual_step(
                        "Slack channel: Set the signing_secret field in [gateway.bridges.slack]",
                    );
                }

                bridges.slack = Some(SavfoxSlackBridge {
                    enabled,
                    bot_token,
                    signing_secret,
                });
                report.add_converted("channels[slack] -> [gateway.bridges.slack]");
            }
            "webhook" => {
                let callback_url = extract_string(&channel.config, "callback_url")
                    .or_else(|| extract_string(&channel.config, "url"));
                let secret = extract_string(&channel.config, "secret");

                bridges.webhook = Some(SavfoxWebhookBridge {
                    enabled,
                    callback_url,
                    secret,
                });
                report.add_converted("channels[webhook] -> [gateway.bridges.webhook]");
            }
            "msteams" => {
                let app_id = extract_string(&channel.config, "app_id").unwrap_or_default();
                let app_password =
                    extract_string(&channel.config, "app_password").unwrap_or_default();
                let tenant_id = extract_string(&channel.config, "tenant_id");

                if app_id.is_empty() {
                    report.add_warning(
                        "MS Teams channel: missing app_id; bridge will need manual configuration",
                    );
                    report.add_manual_step(
                        "MS Teams channel: Set the app_id field in [gateway.bridges.msteams]",
                    );
                }
                if app_password.is_empty() {
                    report.add_warning(
                        "MS Teams channel: missing app_password; bridge will need manual configuration",
                    );
                    report.add_manual_step(
                        "MS Teams channel: Set the app_password field in [gateway.bridges.msteams]",
                    );
                }

                bridges.msteams = Some(SavfoxMsTeamsBridge {
                    enabled,
                    app_id,
                    app_password,
                    tenant_id,
                });
                report.add_converted("channels[msteams] -> [gateway.bridges.msteams]");
            }
            "whatsapp" => {
                let phone_number_id =
                    extract_string(&channel.config, "phone_number_id").unwrap_or_default();
                let access_token =
                    extract_string(&channel.config, "access_token").unwrap_or_default();
                let verify_token = extract_string(&channel.config, "verify_token");

                if phone_number_id.is_empty() {
                    report.add_warning(
                        "WhatsApp channel: missing phone_number_id; bridge will need manual configuration",
                    );
                }
                if access_token.is_empty() {
                    report.add_warning(
                        "WhatsApp channel: missing access_token; bridge will need manual configuration",
                    );
                }

                bridges.whatsapp = Some(SavfoxWhatsAppBridge {
                    enabled,
                    phone_number_id,
                    access_token,
                    verify_token,
                });
                report.add_converted("channels[whatsapp] -> [gateway.bridges.whatsapp]");
            }
            _ => {
                // Already handled by the KNOWN_CHANNEL_TYPES check above.
            }
        }
    }

    bridges
}

fn convert_skills(
    skills: &[OpenClawSkillConfig],
    report: &mut MigrationReport,
) -> SavfoxSkillsConfig {
    let mut entries = Vec::new();

    for skill in skills {
        let path = match &skill.path {
            Some(p) => p.clone(),
            None => {
                if let Some(ref name) = skill.name {
                    report.add_warning(&format!("Skill \"{name}\": no path specified; skipping"));
                } else {
                    report.add_warning("Skill entry with no name or path; skipping");
                }
                continue;
            }
        };

        let enabled = skill.enabled.unwrap_or(true);
        let label = skill.name.as_deref().unwrap_or(&path);

        entries.push(SavfoxSkillEntry {
            path: path.clone(),
            enabled,
        });

        report.add_converted(&format!(
            "skills[\"{label}\"] -> skills.config entry (path=\"{path}\", enabled={enabled})"
        ));
    }

    SavfoxSkillsConfig { config: entries }
}

fn convert_memory(
    memory: &OpenClawMemoryConfig,
    report: &mut MigrationReport,
) -> SavfoxMemoryConfig {
    let mut out = SavfoxMemoryConfig {
        enabled: None,
        budget_kb: None,
    };

    if let Some(enabled) = memory.enabled {
        out.enabled = Some(enabled);
        report.add_converted(&format!("memory.enabled -> memory.enabled = {enabled}"));
    }

    if let Some(budget) = memory.budget_kb {
        out.budget_kb = Some(budget);
        report.add_converted(&format!("memory.budget_kb -> memory.budget_kb = {budget}"));
    }

    if let Some(max_entries) = memory.max_entries {
        report.add_warning(&format!(
            "memory.max_entries = {max_entries} has no direct Savfox equivalent; \
             Savfox uses a KB-based budget instead."
        ));
        report.add_manual_step(
            "memory: Savfox uses memory.budget_kb (default 16) instead of max_entries. \
             Adjust the budget_kb value if needed.",
        );
    }

    if let Some(ref layers) = memory.layers {
        report.add_converted(&format!(
            "memory.layers {:?} noted; Savfox uses a fixed 4-layer system \
             (global, project, agent, session)",
            layers
        ));
        report.add_manual_step(
            "memory: Review your memory layer configuration. Savfox uses a fixed \
             4-layer system: global (~/.savfox/memory/global/), project (<git>/.savfox/memory/), \
             agent (~/.savfox/memory/agents/<name>/), and session (in-memory).",
        );
    }

    out
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract a string value from a generic JSON map.
fn extract_string(map: &HashMap<String, serde_json::Value>, key: &str) -> Option<String> {
    map.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
}

// ---------------------------------------------------------------------------
// Output serialization
// ---------------------------------------------------------------------------

fn serialize_config(config: &SavfoxConfig, format: OutputFormat) -> anyhow::Result<String> {
    match format {
        OutputFormat::Toml => {
            let output = toml::to_string_pretty(config)
                .map_err(|e| anyhow::anyhow!("Failed to serialize to TOML: {e}"))?;
            Ok(add_toml_header(&output))
        }
        OutputFormat::Json => {
            let output = serde_json::to_string_pretty(config)
                .map_err(|e| anyhow::anyhow!("Failed to serialize to JSON: {e}"))?;
            Ok(output)
        }
    }
}

fn add_toml_header(body: &str) -> String {
    let mut output = String::new();
    output.push_str("# Savfox configuration\n");
    output.push_str("# Migrated from OpenClaw by `savfox migrate`\n");
    output.push_str("#\n");
    output.push_str("# Documentation: https://developers.openai.com/savfox/config\n");
    output.push('\n');
    output.push_str(body);
    output
}

fn output_filename(format: OutputFormat) -> &'static str {
    match format {
        OutputFormat::Toml => "config.toml",
        OutputFormat::Json => "config.json",
    }
}

// ---------------------------------------------------------------------------
// Report printing
// ---------------------------------------------------------------------------

fn print_report(report: &MigrationReport) {
    println!();
    println!("{}", "Migration Report".bold().underline());
    println!();

    // Converted items
    if report.converted.is_empty() {
        println!("{}", "No fields were converted.".yellow());
    } else {
        println!(
            "{} ({} item{}):",
            "Converted".green().bold(),
            report.converted.len(),
            if report.converted.len() == 1 { "" } else { "s" }
        );
        for item in &report.converted {
            println!("  {} {item}", "+".green());
        }
    }

    // Warnings
    if !report.warnings.is_empty() {
        println!();
        println!(
            "{} ({} item{}):",
            "Warnings".yellow().bold(),
            report.warnings.len(),
            if report.warnings.len() == 1 { "" } else { "s" }
        );
        for item in &report.warnings {
            println!("  {} {item}", "!".yellow());
        }
    }

    // Manual steps
    if !report.manual_steps.is_empty() {
        println!();
        println!(
            "{} ({} item{}):",
            "Manual Steps Required".red().bold(),
            report.manual_steps.len(),
            if report.manual_steps.len() == 1 {
                ""
            } else {
                "s"
            }
        );
        for (i, item) in report.manual_steps.iter().enumerate() {
            println!("  {}. {item}", i + 1);
        }
    }

    println!();
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub async fn run_migrate(cmd: MigrateCommand) -> anyhow::Result<()> {
    println!("{}", "Savfox Migration Tool".bold().underline());
    println!("Migrating from OpenClaw configuration...");
    println!();

    // Validate source directory.
    if !cmd.source_dir.is_dir() {
        anyhow::bail!(
            "Source directory does not exist or is not a directory: {}",
            cmd.source_dir.display()
        );
    }

    // Parse source config.
    let source_config = find_and_parse_config(&cmd.source_dir)?;

    println!(
        "  {} Parsed OpenClaw config from {}",
        "[OK]".green(),
        cmd.source_dir.display()
    );

    // Convert.
    let mut report = MigrationReport::default();
    let savfox_config = convert_config(source_config, &mut report);

    // Serialize.
    let serialized = serialize_config(&savfox_config, cmd.format)?;

    // Determine output path.
    let output_dir = cmd
        .output_dir
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let output_path = output_dir.join(output_filename(cmd.format));

    if cmd.dry_run {
        println!();
        println!(
            "{} Dry run mode — no files will be written.",
            "[DRY RUN]".cyan()
        );
        println!();
        println!("{}", "--- Generated config ---".bold());
        println!("{serialized}");
        println!("{}", "--- End of config ---".bold());
        println!();
        println!(
            "Would write to: {}",
            output_path.display().to_string().bold()
        );
    } else {
        // Ensure the output directory exists.
        if !output_dir.exists() {
            std::fs::create_dir_all(&output_dir).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to create output directory {}: {e}",
                    output_dir.display()
                )
            })?;
        }

        // Warn if the output file already exists.
        if output_path.exists() {
            warn!(
                "Output file {} already exists; it will be overwritten.",
                output_path.display()
            );
            println!(
                "  {} Overwriting existing file: {}",
                "[WARN]".yellow(),
                output_path.display()
            );
        }

        std::fs::write(&output_path, &serialized).map_err(|e| {
            anyhow::anyhow!("Failed to write config to {}: {e}", output_path.display())
        })?;

        println!(
            "  {} Wrote Savfox config to {}",
            "[OK]".green(),
            output_path.display()
        );
    }

    // Print migration report.
    print_report(&report);

    if report.manual_steps.is_empty() && report.warnings.is_empty() {
        println!(
            "{}",
            "Migration completed successfully with no issues.".green()
        );
    } else {
        println!("Migration completed. Please review the warnings and manual steps above.");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_yaml_config() {
        let yaml = r#"
model:
  default: gpt-4.1
server:
  port: 9090
"#;
        let config: OpenClawConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(
            config.model.as_ref().unwrap().default.as_deref(),
            Some("gpt-4.1")
        );
        assert_eq!(config.server.as_ref().unwrap().port, Some(9090));
    }

    #[test]
    fn parse_minimal_json_config() {
        let json = r#"{
            "model": { "default": "gpt-4.1" },
            "server": { "port": 8080, "token": "secret" }
        }"#;
        let config: OpenClawConfig = serde_json::from_str(json).unwrap();
        assert_eq!(
            config.model.as_ref().unwrap().default.as_deref(),
            Some("gpt-4.1")
        );
        assert_eq!(config.server.as_ref().unwrap().port, Some(8080));
        assert_eq!(
            config.server.as_ref().unwrap().token.as_deref(),
            Some("secret")
        );
    }

    #[test]
    fn convert_server_maps_to_gateway() {
        let source = OpenClawConfig {
            server: Some(OpenClawServerConfig {
                port: Some(3000),
                token: Some("tok123".to_string()),
                host: Some("0.0.0.0".to_string()),
            }),
            model: None,
            channels: None,
            skills: None,
            memory: None,
        };

        let mut report = MigrationReport::default();
        let result = convert_config(source, &mut report);

        let gw = result.gateway.unwrap();
        assert_eq!(gw.port, Some(3000));
        assert_eq!(gw.token.as_deref(), Some("tok123"));
        assert_eq!(gw.host.as_deref(), Some("0.0.0.0"));
        assert!(!report.converted.is_empty());
    }

    #[test]
    fn convert_channels_discord() {
        let mut config_map = HashMap::new();
        config_map.insert(
            "bot_token".to_string(),
            serde_json::Value::String("discord-tok".to_string()),
        );
        config_map.insert(
            "application_id".to_string(),
            serde_json::Value::String("app-123".to_string()),
        );

        let source = OpenClawConfig {
            server: None,
            model: None,
            channels: Some(vec![OpenClawChannelConfig {
                channel_type: Some("discord".to_string()),
                enabled: Some(true),
                config: config_map,
            }]),
            skills: None,
            memory: None,
        };

        let mut report = MigrationReport::default();
        let result = convert_config(source, &mut report);

        let bridges = result.gateway.unwrap().bridges.unwrap();
        let discord = bridges.discord.unwrap();
        assert!(discord.enabled);
        assert_eq!(discord.bot_token, "discord-tok");
        assert_eq!(discord.application_id.as_deref(), Some("app-123"));
    }

    #[test]
    fn unknown_channel_type_warns() {
        let source = OpenClawConfig {
            server: None,
            model: None,
            channels: Some(vec![OpenClawChannelConfig {
                channel_type: Some("foobar".to_string()),
                enabled: Some(true),
                config: HashMap::new(),
            }]),
            skills: None,
            memory: None,
        };

        let mut report = MigrationReport::default();
        let _result = convert_config(source, &mut report);

        assert!(report.warnings.iter().any(|w| w.contains("foobar")));
    }

    #[test]
    fn convert_providers_with_env_key() {
        let source = OpenClawConfig {
            server: None,
            model: Some(OpenClawModelConfig {
                default: Some("my-model".to_string()),
                providers: Some(vec![OpenClawProviderConfig {
                    name: Some("my-provider".to_string()),
                    base_url: Some("https://api.example.com".to_string()),
                    api_key: None,
                    env_key: Some("MY_PROVIDER_KEY".to_string()),
                    models: None,
                }]),
            }),
            channels: None,
            skills: None,
            memory: None,
        };

        let mut report = MigrationReport::default();
        let result = convert_config(source, &mut report);

        assert_eq!(result.model.as_deref(), Some("my-model"));
        let provider = result.model_providers.get("my-provider").unwrap();
        assert_eq!(provider.env_key.as_deref(), Some("MY_PROVIDER_KEY"));
        assert_eq!(
            provider.base_url.as_deref(),
            Some("https://api.example.com")
        );
    }

    #[test]
    fn convert_skills_with_path() {
        let source = OpenClawConfig {
            server: None,
            model: None,
            channels: None,
            skills: Some(vec![
                OpenClawSkillConfig {
                    name: Some("my-skill".to_string()),
                    path: Some("/path/to/skill".to_string()),
                    enabled: Some(true),
                },
                OpenClawSkillConfig {
                    name: Some("disabled-skill".to_string()),
                    path: Some("/path/to/other".to_string()),
                    enabled: Some(false),
                },
            ]),
            memory: None,
        };

        let mut report = MigrationReport::default();
        let result = convert_config(source, &mut report);

        let skills = result.skills.unwrap();
        assert_eq!(skills.config.len(), 2);
        assert_eq!(skills.config[0].path, "/path/to/skill");
        assert!(skills.config[0].enabled);
        assert!(!skills.config[1].enabled);
    }

    #[test]
    fn convert_memory_config() {
        let source = OpenClawConfig {
            server: None,
            model: None,
            channels: None,
            skills: None,
            memory: Some(OpenClawMemoryConfig {
                enabled: Some(true),
                max_entries: Some(100),
                budget_kb: Some(32),
                layers: Some(vec!["global".to_string(), "project".to_string()]),
            }),
        };

        let mut report = MigrationReport::default();
        let result = convert_config(source, &mut report);

        let mem = result.memory.unwrap();
        assert_eq!(mem.enabled, Some(true));
        assert_eq!(mem.budget_kb, Some(32));

        // max_entries should produce a warning
        assert!(report.warnings.iter().any(|w| w.contains("max_entries")));
        // layers should produce a manual step
        assert!(report.manual_steps.iter().any(|s| s.contains("4-layer")));
    }

    #[test]
    fn dry_run_flag_parses() {
        use clap::Parser;
        let cmd = MigrateCommand::try_parse_from([
            "migrate",
            "/some/path",
            "--dry-run",
            "--format",
            "json",
        ])
        .unwrap();
        assert!(cmd.dry_run);
        assert!(matches!(cmd.format, OutputFormat::Json));
    }

    #[test]
    fn serialize_to_toml_produces_valid_toml() {
        let config = SavfoxConfig {
            model: Some("gpt-4.1".to_string()),
            gateway: Some(SavfoxGatewayConfig {
                host: None,
                port: Some(9090),
                token: Some("secret".to_string()),
                bridges: None,
            }),
            ..Default::default()
        };

        let output = serialize_config(&config, OutputFormat::Toml).unwrap();
        assert!(output.contains("model = \"gpt-4.1\""));
        assert!(output.contains("[gateway]"));
        assert!(output.contains("port = 9090"));
    }

    #[test]
    fn serialize_to_json_produces_valid_json() {
        let config = SavfoxConfig {
            model: Some("gpt-4.1".to_string()),
            ..Default::default()
        };

        let output = serialize_config(&config, OutputFormat::Json).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["model"], "gpt-4.1");
    }

    #[test]
    fn infer_wire_api_anthropic() {
        assert_eq!(infer_wire_api("anthropic").as_deref(), Some("anthropic"));
        assert_eq!(
            infer_wire_api("my-claude-provider").as_deref(),
            Some("anthropic")
        );
    }

    #[test]
    fn infer_wire_api_ollama() {
        assert_eq!(infer_wire_api("ollama").as_deref(), Some("chat"));
        assert_eq!(infer_wire_api("ollama-chat").as_deref(), Some("chat"));
    }

    #[test]
    fn infer_wire_api_default() {
        assert_eq!(infer_wire_api("openai"), None);
        assert_eq!(infer_wire_api("custom-provider"), None);
    }

    #[test]
    fn empty_config_produces_no_errors() {
        let source = OpenClawConfig {
            server: None,
            model: None,
            channels: None,
            skills: None,
            memory: None,
        };

        let mut report = MigrationReport::default();
        let result = convert_config(source, &mut report);

        assert!(result.model.is_none());
        assert!(result.gateway.is_none());
        assert!(result.skills.is_none());
        assert!(result.memory.is_none());
        assert!(report.converted.is_empty());
        assert!(report.warnings.is_empty());
        assert!(report.manual_steps.is_empty());
    }

    #[test]
    fn provider_with_inline_api_key_generates_warning() {
        let source = OpenClawConfig {
            server: None,
            model: Some(OpenClawModelConfig {
                default: None,
                providers: Some(vec![OpenClawProviderConfig {
                    name: Some("test-provider".to_string()),
                    base_url: None,
                    api_key: Some("sk-secret-key".to_string()),
                    env_key: None,
                    models: None,
                }]),
            }),
            channels: None,
            skills: None,
            memory: None,
        };

        let mut report = MigrationReport::default();
        let result = convert_config(source, &mut report);

        let provider = result.model_providers.get("test-provider").unwrap();
        assert_eq!(provider.env_key.as_deref(), Some("TEST_PROVIDER_API_KEY"));
        assert!(report.warnings.iter().any(|w| w.contains("api_key")));
        assert!(
            report
                .manual_steps
                .iter()
                .any(|s| s.contains("environment variable"))
        );
    }

    #[test]
    fn provider_with_no_name_warns() {
        let source = OpenClawConfig {
            server: None,
            model: Some(OpenClawModelConfig {
                default: None,
                providers: Some(vec![OpenClawProviderConfig {
                    name: None,
                    base_url: None,
                    api_key: None,
                    env_key: None,
                    models: None,
                }]),
            }),
            channels: None,
            skills: None,
            memory: None,
        };

        let mut report = MigrationReport::default();
        let _result = convert_config(source, &mut report);

        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("Skipping provider with no name"))
        );
    }

    #[test]
    fn channel_with_no_type_warns() {
        let source = OpenClawConfig {
            server: None,
            model: None,
            channels: Some(vec![OpenClawChannelConfig {
                channel_type: None,
                enabled: Some(true),
                config: HashMap::new(),
            }]),
            skills: None,
            memory: None,
        };

        let mut report = MigrationReport::default();
        let _result = convert_config(source, &mut report);

        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("no channel_type"))
        );
    }

    #[test]
    fn skill_with_no_path_warns() {
        let source = OpenClawConfig {
            server: None,
            model: None,
            channels: None,
            skills: Some(vec![OpenClawSkillConfig {
                name: Some("my-skill".to_string()),
                path: None,
                enabled: Some(true),
            }]),
            memory: None,
        };

        let mut report = MigrationReport::default();
        let result = convert_config(source, &mut report);

        assert!(result.skills.is_none());
        assert!(report.warnings.iter().any(|w| w.contains("no path")));
    }
}
