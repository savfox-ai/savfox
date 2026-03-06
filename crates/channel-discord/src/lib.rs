mod channel;
mod meta;
mod parse;

pub use channel::DiscordChannel;
pub use meta::{DiscordStartMeta, parse_start_meta};
pub use parse::{
    append_discord_option_parts, build_command_prompt, parse_interaction,
    parse_interaction_with_resolver, parse_savfox_prompt, quote_discord_arg,
};
