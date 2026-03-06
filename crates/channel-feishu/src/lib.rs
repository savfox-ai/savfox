mod channel;
mod config;
mod inbound;
mod parse;

pub use channel::FeishuChannel;
pub use config::{
    FeishuChannelConfig, FeishuInboundMode, FeishuOutboundConfig, fetch_feishu_tenant_access_token,
    load_feishu_channel_config, resolve_feishu_outbound_config,
};
pub use inbound::{FeishuActionSink, FeishuMessageMeta, build_feishu_event_dispatcher, start_feishu_stream};
pub use parse::{extract_channel_action, parse_text_command};
