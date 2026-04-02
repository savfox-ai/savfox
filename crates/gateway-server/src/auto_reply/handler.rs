use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::info;

use super::commands::CommandRegistry;
use super::types::{AutoReplyConfig, AutoReplyResult, CommandContext, GroupActivation};

pub struct AutoReplyHandler {
    config: Arc<RwLock<AutoReplyConfig>>,
    registry: CommandRegistry,
    greeted_senders: Arc<RwLock<Vec<String>>>,
}

impl AutoReplyHandler {
    #[must_use] 
    pub fn new(config: AutoReplyConfig) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            registry: CommandRegistry::new(),
            greeted_senders: Arc::new(RwLock::new(Vec::new())),
        }
    }

    #[must_use] 
    pub fn with_registry(mut self, registry: CommandRegistry) -> Self {
        self.registry = registry;
        self
    }

    pub async fn handle_message(&self, text: &str, ctx: CommandContext) -> AutoReplyResult {
        let config = self.config.read().await;

        if !config.enabled {
            return AutoReplyResult {
                reply: None,
                is_command: false,
                action: None,
            };
        }

        let is_command = config.allow_commands && self.registry.has_command(text);
        if !should_activate_group(&config, text, &ctx, is_command) {
            return AutoReplyResult {
                reply: None,
                is_command: false,
                action: None,
            };
        }

        if is_command {
            if let Some(command_name) = self.registry.resolve_command_name(text)
                && !is_command_allowed(&config, &command_name) {
                    return AutoReplyResult {
                        reply: Some(format!("Command '/{command_name}' is not allowed.")),
                        is_command: true,
                        action: None,
                    };
                }
            return self.handle_command_internal(text, &ctx);
        }

        if ctx.is_mentioned && !ctx.is_authorized {
            return AutoReplyResult {
                reply: Some("You are not authorized to interact with me.".to_owned()),
                is_command: false,
                action: None,
            };
        }

        if let Some(status) = config.status_format.as_ref() {
            return AutoReplyResult {
                reply: Some(render_status(status, &ctx)),
                is_command: false,
                action: None,
            };
        }

        AutoReplyResult {
            reply: None,
            is_command: false,
            action: None,
        }
    }

    fn handle_command_internal(&self, text: &str, ctx: &CommandContext) -> AutoReplyResult {
        match self.registry.handle_command(text, ctx) {
            Some(result) => {
                if let Some(error) = &result.error {
                    info!("Command error: {}", error);
                }
                AutoReplyResult {
                    reply: result.reply,
                    is_command: true,
                    action: result.action,
                }
            }
            None => AutoReplyResult {
                reply: Some("Unknown command. Type /help for available commands.".to_owned()),
                is_command: true,
                action: None,
            },
        }
    }

    pub async fn handle_greeting(&self, sender_id: &str, ctx: &CommandContext) -> Option<String> {
        let config = self.config.read().await;

        if !config.enabled {
            return None;
        }

        let mut greeted = self.greeted_senders.write().await;
        if greeted.contains(&sender_id.to_owned()) {
            return None;
        }

        greeted.push(sender_id.to_owned());

        if let Some(ref greeting) = config.greeting_message {
            let mut result = greeting.clone();
            result = result.replace("{sender}", sender_id);
            result = result.replace("{channel}", &ctx.channel_id);
            Some(result)
        } else {
            None
        }
    }

    pub async fn reset_greeted(&self) {
        let mut greeted = self.greeted_senders.write().await;
        greeted.clear();
    }

    pub async fn update_config(&self, config: AutoReplyConfig) {
        let mut current = self.config.write().await;
        *current = config;
    }

    #[must_use] 
    pub fn registry(&self) -> &CommandRegistry {
        &self.registry
    }
}

fn is_command_allowed(config: &AutoReplyConfig, command_name: &str) -> bool {
    let command = command_name.trim_start_matches('/').to_lowercase();
    if !config.command_allowlist.is_empty() {
        let allow = config
            .command_allowlist
            .iter()
            .any(|c| c.trim_start_matches('/').eq_ignore_ascii_case(&command));
        if !allow {
            return false;
        }
    }
    if config
        .command_denylist
        .iter()
        .any(|c| c.trim_start_matches('/').eq_ignore_ascii_case(&command))
    {
        return false;
    }
    true
}

fn resolve_group_activation(config: &AutoReplyConfig, channel_id: &str) -> GroupActivation {
    if let Some(mode) = config
        .group_activation_overrides
        .get(&channel_id.to_lowercase())
    {
        return *mode;
    }
    if let Some((platform, _)) = channel_id.split_once(':')
        && let Some(mode) = config
            .group_activation_overrides
            .get(&platform.to_lowercase())
    {
        return *mode;
    }
    config.group_activation
}

fn has_group_keyword(text: &str, keywords: &[String]) -> bool {
    if keywords.is_empty() {
        return false;
    }
    let lower = text.to_lowercase();
    keywords.iter().any(|k| {
        let needle = k.trim().to_lowercase();
        !needle.is_empty() && lower.contains(&needle)
    })
}

fn should_activate_group(
    config: &AutoReplyConfig,
    text: &str,
    ctx: &CommandContext,
    is_command: bool,
) -> bool {
    if !ctx.is_group {
        return true;
    }

    match resolve_group_activation(config, &ctx.channel_id) {
        GroupActivation::Always => true,
        GroupActivation::Off => false,
        GroupActivation::Command => is_command,
        GroupActivation::Mention => ctx.is_mentioned,
        GroupActivation::Keyword => {
            ctx.is_mentioned || has_group_keyword(text, &config.group_keywords)
        }
    }
}

fn render_status(template: &str, ctx: &CommandContext) -> String {
    let session = ctx.session_id.clone().unwrap_or_else(|| "none".to_owned());
    template
        .replace("{sender}", &ctx.sender_id)
        .replace("{channel}", &ctx.channel_id)
        .replace("{session}", &session)
        .replace(
            "{authorized}",
            if ctx.is_authorized { "true" } else { "false" },
        )
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::AutoReplyHandler;
    use crate::auto_reply::{AutoReplyConfig, CommandContext, GroupActivation};

    fn group_ctx() -> CommandContext {
        CommandContext {
            sender_id: "u1".to_string(),
            channel_id: "discord:chan".to_string(),
            session_id: Some("0194f7b3-1d7b-7c40-ae3d-95b6ef93e161".to_string()),
            is_authorized: true,
            is_mentioned: false,
            is_group: true,
            metadata: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn mention_mode_requires_mention() {
        let mut cfg = AutoReplyConfig::default();
        cfg.group_activation = GroupActivation::Mention;
        cfg.status_format = Some("active".to_string());
        let handler = AutoReplyHandler::new(cfg);

        let result = handler.handle_message("hello", group_ctx()).await;
        assert!(result.reply.is_none());

        let mut mentioned = group_ctx();
        mentioned.is_mentioned = true;
        let result = handler.handle_message("hello", mentioned).await;
        assert_eq!(result.reply.as_deref(), Some("active"));
    }

    #[tokio::test]
    async fn keyword_mode_uses_keywords() {
        let mut cfg = AutoReplyConfig::default();
        cfg.group_activation = GroupActivation::Keyword;
        cfg.group_keywords = vec!["savfox".to_string()];
        cfg.status_format = Some("matched".to_string());
        let handler = AutoReplyHandler::new(cfg);

        let result = handler.handle_message("hi savfox bot", group_ctx()).await;
        assert_eq!(result.reply.as_deref(), Some("matched"));
    }

    #[tokio::test]
    async fn always_mode_responds_without_mention() {
        let mut cfg = AutoReplyConfig::default();
        cfg.group_activation = GroupActivation::Always;
        cfg.status_format = Some("always".to_string());
        let handler = AutoReplyHandler::new(cfg);

        let result = handler.handle_message("hello", group_ctx()).await;
        assert_eq!(result.reply.as_deref(), Some("always"));
    }

    #[tokio::test]
    async fn command_mode_only_responds_to_commands() {
        let mut cfg = AutoReplyConfig::default();
        cfg.group_activation = GroupActivation::Command;
        cfg.status_format = Some("status".to_string());
        let handler = AutoReplyHandler::new(cfg);

        let result = handler.handle_message("hello", group_ctx()).await;
        assert!(result.reply.is_none());

        let result = handler.handle_message("/help", group_ctx()).await;
        assert!(result.is_command);
        assert!(result.reply.is_some());
    }

    #[tokio::test]
    async fn per_channel_override_applies() {
        let mut cfg = AutoReplyConfig::default();
        cfg.group_activation = GroupActivation::Always;
        cfg.group_activation_overrides
            .insert("discord:chan".to_string(), GroupActivation::Command);
        cfg.status_format = Some("always".to_string());
        let handler = AutoReplyHandler::new(cfg);

        let result = handler.handle_message("hello", group_ctx()).await;
        assert!(result.reply.is_none());
    }
}
