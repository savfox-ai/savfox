use std::collections::BTreeMap;

use super::JsonSchema;
use super::declarations::{FunctionToolDecl, function_tool};
use crate::client_common::tools::ToolSpec;

pub(super) fn create_message_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "channel".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Channel identifier in the format `platform:id` (e.g., `discord:12345`, `telegram:67890`, `slack:C01ABC`).".to_owned(),
                ),
            },
        ),
        (
            "text".to_owned(),
            JsonSchema::String {
                description: Some("The message text to send.".to_owned()),
            },
        ),
        (
            "format".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Optional format hint: \"plain\" (default), \"markdown\", or \"html\".".to_owned(),
                ),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "message",
        description: "Send a message to a chat platform channel via the gateway server. The channel is specified as `platform:id`.",
        properties,
        required: &["channel", "text"],
    })
}

pub(super) fn create_discord_actions_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "action".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Discord action: \"send_message\", \"edit_message\", \"delete_message\", \"add_reaction\", \"remove_reaction\", \"pin_message\", \"unpin_message\", \"create_session\", \"list_members\", \"kick_member\", \"ban_member\", \"get_channel_info\", \"get_message_history\", \"set_channel_topic\".".to_owned(),
                ),
            },
        ),
        (
            "channel_id".to_owned(),
            JsonSchema::String {
                description: Some("Discord channel ID.".to_owned()),
            },
        ),
        (
            "message_id".to_owned(),
            JsonSchema::String {
                description: Some("Discord message ID (for message-specific actions).".to_owned()),
            },
        ),
        (
            "guild_id".to_owned(),
            JsonSchema::String {
                description: Some("Discord guild/server ID (for guild-specific actions).".to_owned()),
            },
        ),
        (
            "content".to_owned(),
            JsonSchema::String {
                description: Some("Message content or text payload.".to_owned()),
            },
        ),
        (
            "emoji".to_owned(),
            JsonSchema::String {
                description: Some("Emoji for reaction actions; the tool URL-encodes it.".to_owned()),
            },
        ),
        (
            "topic".to_owned(),
            JsonSchema::String {
                description: Some("Channel topic text.".to_owned()),
            },
        ),
        (
            "user_id".to_owned(),
            JsonSchema::String {
                description: Some("Target user ID (for moderation actions).".to_owned()),
            },
        ),
        (
            "reason".to_owned(),
            JsonSchema::String {
                description: Some("Reason for moderation action.".to_owned()),
            },
        ),
        (
            "limit".to_owned(),
            JsonSchema::Number {
                description: Some("Maximum number of items to return.".to_owned()),
            },
        ),
        (
            "name".to_owned(),
            JsonSchema::String {
                description: Some("Name for created resources (e.g. session name).".to_owned()),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "discord_actions",
        description: "Perform Discord platform actions: send/edit/delete messages, reactions, pins, sessions, member management, and channel operations. Requires DISCORD_BOT_TOKEN environment variable.",
        properties,
        required: &["action"],
    })
}

pub(super) fn create_slack_actions_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "action".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Slack action: \"send_message\", \"update_message\", \"delete_message\", \"add_reaction\", \"remove_reaction\", \"pin_item\", \"unpin_item\", \"get_channel_history\", \"set_channel_topic\".".to_owned(),
                ),
            },
        ),
        (
            "channel_id".to_owned(),
            JsonSchema::String {
                description: Some("Slack channel ID.".to_owned()),
            },
        ),
        (
            "message_id".to_owned(),
            JsonSchema::String {
                description: Some("Slack message timestamp (ts) for message-specific actions.".to_owned()),
            },
        ),
        (
            "content".to_owned(),
            JsonSchema::String {
                description: Some("Message content or text payload.".to_owned()),
            },
        ),
        (
            "emoji".to_owned(),
            JsonSchema::String {
                description: Some("Emoji name for reaction actions (without colons).".to_owned()),
            },
        ),
        (
            "topic".to_owned(),
            JsonSchema::String {
                description: Some("Channel topic text.".to_owned()),
            },
        ),
        (
            "limit".to_owned(),
            JsonSchema::Number {
                description: Some("Maximum number of items to return.".to_owned()),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "slack_actions",
        description: "Perform Slack platform actions: send/update/delete messages, reactions, pins, channel history, and topic management. Requires SLACK_BOT_TOKEN environment variable.",
        properties,
        required: &["action"],
    })
}

pub(super) fn create_telegram_actions_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "action".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Telegram action: \"send_message\", \"edit_message\", \"delete_message\", \"send_sticker\", \"get_chat_info\".".to_owned(),
                ),
            },
        ),
        (
            "channel_id".to_owned(),
            JsonSchema::String {
                description: Some("Telegram chat ID.".to_owned()),
            },
        ),
        (
            "message_id".to_owned(),
            JsonSchema::String {
                description: Some("Telegram message ID for message-specific actions.".to_owned()),
            },
        ),
        (
            "content".to_owned(),
            JsonSchema::String {
                description: Some("Message content or text payload.".to_owned()),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "telegram_actions",
        description: "Perform Telegram platform actions: send/edit/delete messages, send stickers, and get chat information. Requires TELEGRAM_BOT_TOKEN environment variable.",
        properties,
        required: &["action"],
    })
}

pub(super) fn create_whatsapp_actions_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "action".to_owned(),
            JsonSchema::String {
                description: Some("WhatsApp action: \"react\".".to_owned()),
            },
        ),
        (
            "chat_jid".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Chat JID identifier (e.g., \"1234567890@s.whatsapp.net\").".to_owned(),
                ),
            },
        ),
        (
            "message_id".to_owned(),
            JsonSchema::String {
                description: Some("Message ID to react to.".to_owned()),
            },
        ),
        (
            "emoji".to_owned(),
            JsonSchema::String {
                description: Some("Emoji for reaction.".to_owned()),
            },
        ),
        (
            "remove".to_owned(),
            JsonSchema::Boolean {
                description: Some("Whether to remove an existing reaction.".to_owned()),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "whatsapp_actions",
        description: "WhatsApp message actions: send emoji reactions. Requires WHATSAPP_API_URL environment variable.",
        properties,
        required: &["action"],
    })
}

pub(super) fn create_channel_tools_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "action".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Channel action: \"send\", \"react\", \"edit\", \"delete\", \"history\", \
                     \"list_channels\".".to_owned(),
                ),
            },
        ),
        (
            "platform".to_owned(),
            JsonSchema::String {
                description: Some(
                    "Target platform: \"discord\", \"slack\", \"telegram\", \"whatsapp\", \"webhook\".".to_owned(),
                ),
            },
        ),
        (
            "channel_id".to_owned(),
            JsonSchema::String {
                description: Some("Channel/chat identifier.".to_owned()),
            },
        ),
        (
            "content".to_owned(),
            JsonSchema::String {
                description: Some("Message content (for send/edit).".to_owned()),
            },
        ),
        (
            "message_id".to_owned(),
            JsonSchema::String {
                description: Some("Message ID (for edit/delete/react).".to_owned()),
            },
        ),
        (
            "emoji".to_owned(),
            JsonSchema::String {
                description: Some("Emoji for reactions.".to_owned()),
            },
        ),
        (
            "session_id".to_owned(),
            JsonSchema::String {
                description: Some("Session/reply-to ID.".to_owned()),
            },
        ),
        (
            "limit".to_owned(),
            JsonSchema::Number {
                description: Some("Maximum messages to fetch for history (default: 50).".to_owned()),
            },
        ),
    ]);

    function_tool(FunctionToolDecl {
        name: "channel_tools",
        description: "Unified multi-platform channel tool: send messages, reactions, edit/delete messages, fetch history, and list channels across Discord, Slack, Telegram, WhatsApp, and webhooks. Routes through the gateway server.",
        properties,
        required: &["action", "platform"],
    })
}
