mod channel;
pub mod client;
mod config;
mod inbound;
mod parse;

pub use channel::{DingtalkChannel, send_dingtalk_text_message};
pub use config::{
    DingtalkChannelConfig, DingtalkInboundMode, DingtalkOutboundConfig,
    load_dingtalk_channel_config, resolve_dingtalk_outbound_config,
};
pub use inbound::{DingtalkActionSink, start_dingtalk_stream};
pub use parse::{
    DingtalkMessageMeta, DingtalkParseResult, extract_dingtalk_channel, extract_dingtalk_text,
    parse_inbound_payload, parse_text_command,
};
