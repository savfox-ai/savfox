pub mod client;
pub mod config;
mod inbound;
mod meta;
mod parse;

pub use client::{
    delete_message, edit_message, resolve_bot_token, send_embed, send_message,
    send_message_returning_id, verify_signature,
};
pub use config::{DiscordChannelConfig, DiscordInboundMode, resolve_discord_outbound_token};
pub use inbound::{
    DiscordEventSink, DiscordInboundMessage, DiscordStreamRuntimeState, discord_stream_state_for,
    discord_stream_state_snapshot, is_discord_stream_running, start_discord_stream,
    stop_discord_stream,
};
pub use meta::{DiscordStartMeta, parse_start_meta};
pub use parse::{
    DiscordMessageParseResult, append_discord_option_parts, build_command_prompt,
    parse_display_name, parse_interaction, parse_interaction_with_resolver, parse_message,
    parse_message_with_resolver, parse_savfox_prompt, quote_discord_arg,
};
