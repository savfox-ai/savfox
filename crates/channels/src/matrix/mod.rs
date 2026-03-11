mod channel;
mod config;
mod parse;
mod startup;

pub use channel::MatrixChannel;
pub use config::{
    MatrixChannelConfig, MatrixMode, load_matrix_channel_configs, resolve_matrix_outbound_config,
};
pub use parse::{
    MatrixCommandEvent, MatrixInboundParseResult, MatrixWebhookParseResult, parse_command_event,
    parse_command_event_for_user, parse_inbound_payload, parse_inbound_payload_for_user,
    parse_invite_event, parse_webhook_payload, parse_webhook_payload_for_user,
};
pub use startup::log_configured_matrix_startup;
