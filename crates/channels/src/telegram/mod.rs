mod channel;
pub mod client;
pub mod config;
mod inbound;
mod meta;
mod parse;

pub use channel::TelegramChannel;
pub use client::{
    delete_message, edit_message, escape_html, resolve_bot_token, send_message,
    truncate_log_preview, verify_webhook_secret,
};
pub use config::{TelegramChannelConfig, resolve_telegram_outbound_token};
pub use inbound::{
    TelegramUpdateSink, is_telegram_polling_running, start_telegram_polling, stop_telegram_polling,
};
pub use meta::{TelegramStartMeta, parse_start_meta};
pub use parse::{parse_display_name, parse_update, parse_update_with_resolver};
