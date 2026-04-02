use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};

/// Default gateway listen port.
pub const DEFAULT_PORT: u16 = 18881;

/// Gateway-specific configuration that can be set via config.toml `[gateway]` or CLI flags.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayConfig {
    /// Host address to bind to.
    #[serde(default = "default_host")]
    pub host: IpAddr,

    /// Port to listen on.
    #[serde(default = "default_port")]
    pub port: u16,

    /// Static bearer token for authentication. If `None`, one is auto-generated at startup.
    #[serde(default)]
    pub token: Option<String>,

    /// TLS certificate path (PEM). If set alongside `tls_key`, enables HTTPS.
    #[serde(default)]
    pub tls_cert: Option<String>,

    /// TLS private key path (PEM).
    #[serde(default)]
    pub tls_key: Option<String>,

    /// Channel configuration for chat platforms.
    #[serde(default, alias = "bridges")]
    pub channels: ChannelsConfig,

    /// Response footer rendering configuration.
    #[serde(default)]
    pub response_footer: ResponseFooterConfig,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            token: None,
            tls_cert: None,
            tls_key: None,
            channels: ChannelsConfig::default(),
            response_footer: ResponseFooterConfig::default(),
        }
    }
}

fn default_host() -> IpAddr {
    IpAddr::V4(Ipv4Addr::LOCALHOST)
}

fn default_port() -> u16 {
    DEFAULT_PORT
}

/// Chat-platform bridge configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelsConfig {
    #[serde(default)]
    pub discord: Option<DiscordChannelConfig>,

    #[serde(default)]
    pub dingtalk: Option<DingtalkChannelConfig>,

    #[serde(default)]
    pub telegram: Option<TelegramChannelConfig>,

    #[serde(default)]
    pub slack: Option<SlackChannelConfig>,

    #[serde(default)]
    pub msteams: Option<MsTeamsChannelConfig>,

    #[serde(default)]
    pub webhook: Option<WebhookChannelConfig>,

    #[serde(default)]
    pub whatsapp: Option<WhatsAppChannelConfig>,

    #[serde(default)]
    pub signal: Option<SignalChannelConfig>,

    #[serde(default)]
    pub imessage: Option<IMessageChannelConfig>,

    #[serde(default)]
    pub zalo: Option<ZaloChannelConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseFooterConfig {
    /// Global switch for appending response footers.
    #[serde(default = "default_response_footer_enabled")]
    pub enabled: bool,
    /// Template used when no channel-specific template is defined.
    /// Placeholders: `{model}`, `{provider}`, `{profile}`, `{tokens}`, `{cost}`,
    /// `{profile_segment}`, `{tokens_segment}`, `{cost_segment}`.
    #[serde(default = "default_response_footer_template")]
    pub template: String,
    /// Channel-specific template overrides, keyed by platform
    /// (e.g. `discord`, `telegram`, `slack`).
    #[serde(default)]
    pub channel_templates: HashMap<String, String>,
    /// Per-channel footer max length; values above this will be truncated with `...`.
    #[serde(default)]
    pub channel_max_length: HashMap<String, usize>,
    /// Global fallback max length for channels not in `channel_max_length`.
    #[serde(default = "default_response_footer_max_length")]
    pub max_length: usize,
}

impl Default for ResponseFooterConfig {
    fn default() -> Self {
        Self {
            enabled: default_response_footer_enabled(),
            template: default_response_footer_template(),
            channel_templates: default_response_footer_channel_templates(),
            channel_max_length: default_response_footer_channel_max_length(),
            max_length: default_response_footer_max_length(),
        }
    }
}

fn default_response_footer_enabled() -> bool {
    true
}

fn default_response_footer_template() -> String {
    "model: {model} | provider: {provider}{profile_segment}{tokens_segment}{cost_segment}".to_owned()
}

fn default_response_footer_max_length() -> usize {
    240
}

fn default_response_footer_channel_templates() -> HashMap<String, String> {
    let mut map = HashMap::new();
    map.insert(
        "discord".to_owned(),
        "m:{model} | p:{provider}{tokens_segment}{cost_segment}".to_owned(),
    );
    map.insert(
        "telegram".to_owned(),
        "m:{model} | p:{provider}{tokens_segment}{cost_segment}".to_owned(),
    );
    map.insert(
        "slack".to_owned(),
        "m:{model} | p:{provider}{tokens_segment}{cost_segment}".to_owned(),
    );
    map
}

fn default_response_footer_channel_max_length() -> HashMap<String, usize> {
    let mut map = HashMap::new();
    map.insert("discord".to_owned(), 180);
    map.insert("telegram".to_owned(), 220);
    map.insert("slack".to_owned(), 220);
    map
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordChannelConfig {
    pub enabled: bool,
    pub bot_token: String,
    #[serde(default)]
    pub application_id: Option<String>,
    #[serde(default)]
    pub application_public_key: Option<String>,
}

pub type DiscordBridgeConfig = DiscordChannelConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DingtalkChannelConfig {
    pub enabled: bool,
    /// DingTalk custom robot webhook URL.
    #[serde(default)]
    pub webhook_url: Option<String>,
    /// DingTalk custom robot access token (used when webhook_url is not provided).
    #[serde(default)]
    pub access_token: Option<String>,
    /// DingTalk custom robot signing secret.
    #[serde(default)]
    pub secret: Option<String>,
}

pub type DingtalkBridgeConfig = DingtalkChannelConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramChannelConfig {
    pub enabled: bool,
    pub bot_token: String,
    #[serde(default)]
    pub webhook_secret_token: Option<String>,
}

pub type TelegramBridgeConfig = TelegramChannelConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlackChannelConfig {
    pub enabled: bool,
    pub bot_token: String,
    pub signing_secret: String,
}

pub type SlackBridgeConfig = SlackChannelConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MsTeamsChannelConfig {
    pub enabled: bool,
    /// Microsoft App ID (Bot registration).
    pub app_id: String,
    /// Microsoft App Password (client secret).
    pub app_password: String,
    /// Optional tenant ID for single-tenant bots.
    #[serde(default)]
    pub tenant_id: Option<String>,
}

pub type MsTeamsBridgeConfig = MsTeamsChannelConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookChannelConfig {
    pub enabled: bool,
    /// URL to send outbound events to.
    #[serde(default)]
    pub callback_url: Option<String>,
    /// Shared secret for HMAC-SHA256 signature verification of inbound requests.
    #[serde(default)]
    pub secret: Option<String>,
}

pub type WebhookBridgeConfig = WebhookChannelConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhatsAppChannelConfig {
    pub enabled: bool,
    /// WhatsApp Business Phone Number ID.
    pub phone_number_id: String,
    /// WhatsApp Business Access Token.
    pub access_token: String,
    /// Webhook verify token for initial setup.
    #[serde(default)]
    pub verify_token: Option<String>,
    /// App secret for webhook signature verification.
    #[serde(default)]
    pub app_secret: Option<String>,
}

pub type WhatsAppBridgeConfig = WhatsAppChannelConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalChannelConfig {
    pub enabled: bool,
    /// Signal phone number (account identifier).
    #[serde(default)]
    pub phone_number: Option<String>,
    /// signal-cli JSON-RPC URL.
    #[serde(default)]
    pub rpc_url: Option<String>,
}

pub type SignalBridgeConfig = SignalChannelConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IMessageChannelConfig {
    pub enabled: bool,
    /// BlueBubbles server URL (e.g. "http://localhost:1234").
    pub api_url: String,
    /// BlueBubbles server password for API authentication.
    pub password: String,
    /// Polling interval in seconds. Defaults to 5 if not set.
    #[serde(default)]
    pub poll_interval_secs: Option<u64>,
}

pub type IMessageBridgeConfig = IMessageChannelConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZaloChannelConfig {
    pub enabled: bool,
    /// Zalo OA App ID.
    pub app_id: String,
    /// Zalo OA App Secret (used for webhook signature verification).
    pub app_secret: String,
    /// Zalo OA Access Token for sending messages.
    pub access_token: String,
    /// Optional webhook verify token for initial setup handshake.
    #[serde(default)]
    pub webhook_verify_token: Option<String>,
}

pub type ZaloBridgeConfig = ZaloChannelConfig;

/// CLI arguments for `savfox gateway`.
#[derive(Debug, Parser)]
pub struct GatewayCommand {
    /// Host address to bind to (default: 127.0.0.1). Use 0.0.0.0 for all interfaces.
    #[arg(long, default_value = "127.0.0.1")]
    pub host: IpAddr,

    /// Port to listen on.
    #[arg(long, default_value_t = DEFAULT_PORT)]
    pub port: u16,

    /// Bearer token for authentication. Auto-generated if omitted.
    #[arg(long)]
    pub token: Option<String>,

    /// Path to TLS certificate (PEM).
    #[arg(long)]
    pub tls_cert: Option<String>,

    /// Path to TLS private key (PEM).
    #[arg(long)]
    pub tls_key: Option<String>,

    /// Gateway subcommand. If omitted, starts the gateway server.
    #[command(subcommand)]
    pub subcommand: Option<GatewaySubcommand>,
}

/// Gateway management subcommands.
#[derive(Debug, Subcommand)]
pub enum GatewaySubcommand {
    /// Check the status of a running gateway.
    Status {
        /// Gateway URL to query.
        #[arg(long, default_value = "http://127.0.0.1:18881")]
        url: String,
    },

    /// View gateway logs (connects to a running gateway).
    Logs {
        /// Gateway URL to connect to.
        #[arg(long, default_value = "http://127.0.0.1:18881")]
        url: String,
        /// Number of recent log lines to show.
        #[arg(long, default_value_t = 50)]
        lines: usize,
        /// Follow logs in real-time.
        #[arg(long)]
        follow: bool,
    },

    /// List available models from the gateway.
    Models {
        /// Gateway URL to query.
        #[arg(long, default_value = "http://127.0.0.1:18881")]
        url: String,
    },

    /// Manage exec approval requests.
    Approvals {
        /// Gateway URL to query.
        #[arg(long, default_value = "http://127.0.0.1:18881")]
        url: String,
        /// Subcommand for approvals.
        #[command(subcommand)]
        action: Option<ApprovalsAction>,
    },

    /// Manage device pairing tokens.
    Devices {
        /// Gateway URL to query.
        #[arg(long, default_value = "http://127.0.0.1:18881")]
        url: String,
        /// Subcommand for devices.
        #[command(subcommand)]
        action: Option<DevicesAction>,
    },

    /// Manage chat channel integrations.
    Channels {
        /// Gateway URL to query.
        #[arg(long, default_value = "http://127.0.0.1:18881")]
        url: String,
    },

    /// Manage connected nodes.
    Nodes {
        /// Gateway URL to query.
        #[arg(long, default_value = "http://127.0.0.1:18881")]
        url: String,
    },

    /// Start the gateway as a background daemon.
    Start {
        /// Port to listen on.
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
        /// Host address to bind to.
        #[arg(long, default_value = "127.0.0.1")]
        host: IpAddr,
        /// PID file location. Defaults to `{savfox_home}/gateway.pid`.
        #[arg(long)]
        pid_file: Option<PathBuf>,
    },

    /// Stop a running gateway daemon.
    Stop {
        /// PID file to read. Defaults to `{savfox_home}/gateway.pid`.
        #[arg(long)]
        pid_file: Option<PathBuf>,
    },

    /// Restart the gateway daemon (stop then start).
    Restart {
        /// Port to listen on.
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
        /// Host address to bind to.
        #[arg(long, default_value = "127.0.0.1")]
        host: IpAddr,
        /// PID file location. Defaults to `{savfox_home}/gateway.pid`.
        #[arg(long)]
        pid_file: Option<PathBuf>,
    },

    /// Install gateway as a system service (systemd on Linux, launchd on macOS).
    Install {
        /// Service name.
        #[arg(long, default_value = "savfox-gateway")]
        name: String,
        /// Runtime/service manager override (`auto`, `systemd`, `launchd`, `windows-task`).
        #[arg(long, default_value = "auto")]
        runtime: String,
    },

    /// Uninstall gateway system service.
    Uninstall {
        /// Service name.
        #[arg(long, default_value = "savfox-gateway")]
        name: String,
        /// Runtime/service manager override (`auto`, `systemd`, `launchd`, `windows-task`).
        #[arg(long, default_value = "auto")]
        runtime: String,
    },
}

/// Approval management actions.
#[derive(Debug, Subcommand)]
pub enum ApprovalsAction {
    /// List pending approval requests.
    List,
    /// Approve a pending request by ID.
    Approve {
        /// Approval request ID.
        id: String,
    },
    /// Deny a pending request by ID.
    Deny {
        /// Approval request ID.
        id: String,
        /// Reason for denial.
        #[arg(long)]
        reason: Option<String>,
    },
}

/// Device management actions.
#[derive(Debug, Subcommand)]
pub enum DevicesAction {
    /// List paired devices.
    List,
    /// Generate a new pairing token.
    Pair {
        /// Display name for the device.
        #[arg(long)]
        name: Option<String>,
    },
    /// Revoke a device by ID.
    Revoke {
        /// Device ID to revoke.
        id: String,
    },
}

impl GatewayCommand {
    /// Merge CLI flags into a `GatewayConfig`, with CLI taking precedence.
    #[must_use]
    pub fn into_config(self) -> GatewayConfig {
        GatewayConfig {
            host: self.host,
            port: self.port,
            token: self.token,
            tls_cert: self.tls_cert,
            tls_key: self.tls_key,
            channels: ChannelsConfig::default(),
            response_footer: ResponseFooterConfig::default(),
        }
    }
}
