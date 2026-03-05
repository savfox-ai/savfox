use async_trait::async_trait;
use serde_json::{Value, json};
use tracing::{info, warn};

use super::{ChatBridge, RichMessage};
use crate::protocol::BridgeAction;

/// Nostr decentralized protocol bridge.
///
/// Uses the NIP-04 encrypted direct messages and NIP-01 events.
/// The bridge connects to one or more relays and listens for mentions/DMs
/// directed at the bot's npub.
pub(crate) struct NostrBridge {
    config: NostrBridgeConfig,
    http_client: reqwest::Client,
}

#[derive(Debug, Clone)]
pub(crate) struct NostrBridgeConfig {
    pub(crate) enabled: bool,
    /// Bot's nsec (private key) in hex or bech32.
    pub(crate) private_key: String,
    /// Relay URLs to connect to.
    pub(crate) relays: Vec<String>,
    /// Whether to respond to public mentions (kind 1 events).
    pub(crate) respond_to_mentions: bool,
    /// Whether to respond to encrypted DMs (kind 4 events).
    pub(crate) respond_to_dms: bool,
}

impl NostrBridge {
    #[must_use]
    pub(crate) fn new(config: NostrBridgeConfig, http_client: reqwest::Client) -> Self {
        Self {
            config,
            http_client,
        }
    }

    /// Parse a Nostr event payload.
    fn parse_event(payload: &Value) -> anyhow::Result<BridgeAction> {
        let kind = payload["kind"].as_u64().unwrap_or(0);

        match kind {
            // Kind 1: Text note (public mention).
            1 => {
                let content = payload["content"].as_str().unwrap_or("").to_string();
                let pubkey = payload["pubkey"].as_str().unwrap_or("unknown").to_string();

                if content.trim().is_empty() {
                    return Ok(BridgeAction::Ignore);
                }

                Ok(BridgeAction::StartThread {
                    channel: pubkey,
                    prompt: content,
                })
            }
            // Kind 4: Encrypted direct message.
            4 => {
                let content = payload["content"].as_str().unwrap_or("").to_string();
                let pubkey = payload["pubkey"].as_str().unwrap_or("unknown").to_string();

                // Content is encrypted (NIP-04), needs decryption.
                // For now, pass the raw content - decryption would require
                // the secp256k1 shared secret computation.
                if content.trim().is_empty() {
                    return Ok(BridgeAction::Ignore);
                }

                Ok(BridgeAction::StartThread {
                    channel: format!("dm:{pubkey}"),
                    prompt: content,
                })
            }
            _ => Ok(BridgeAction::Ignore),
        }
    }
}

impl std::fmt::Debug for NostrBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NostrBridge")
            .field("relays", &self.config.relays)
            .field("enabled", &self.config.enabled)
            .finish()
    }
}

#[async_trait]
impl ChatBridge for NostrBridge {
    async fn start(&mut self) -> anyhow::Result<()> {
        info!(
            relays = ?self.config.relays,
            "Nostr bridge initialized"
        );
        Ok(())
    }

    async fn send_message(&self, channel: &str, message: &str) -> anyhow::Result<()> {
        // In Nostr, sending a reply means publishing a kind-1 event
        // tagged with the original event id (NIP-10).
        warn!(channel = %channel, "Nostr send_message: relay publishing not yet implemented");
        Ok(())
    }

    async fn send_rich_message(&self, channel: &str, msg: RichMessage) -> anyhow::Result<()> {
        // Nostr doesn't support rich formatting natively.
        // Send as plain text with code blocks formatted in markdown.
        let mut text = msg.text.clone();
        for block in &msg.code_blocks {
            text.push_str(&format!("\n```{}\n{}\n```", block.language, block.content));
        }
        self.send_message(channel, &text).await
    }

    async fn handle_webhook(&self, payload: Value) -> anyhow::Result<BridgeAction> {
        Self::parse_event(&payload)
    }
}
