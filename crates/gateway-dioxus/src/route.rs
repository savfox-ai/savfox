use dioxus::prelude::*;

use crate::components::layout::Layout;
use crate::pages::agents::{Agents, AgentsDetail, AgentsDetailTab, AgentsNew};
use crate::pages::approvals::Approvals;
use crate::pages::channels::cokret::CokretChannel;
use crate::pages::channels::dingtalk::DingtalkChannel;
use crate::pages::channels::discord::DiscordChannel;
use crate::pages::channels::feishu::FeishuChannel;
use crate::pages::channels::google_chat::GoogleChatChannel;
use crate::pages::channels::imessage::IMessageChannel;
use crate::pages::channels::irc::IrcChannel;
use crate::pages::channels::line::LineChannel;
use crate::pages::channels::matrix::MatrixChannel;
use crate::pages::channels::mattermost::MattermostChannel;
use crate::pages::channels::msteams::MsTeamsChannel;
use crate::pages::channels::nextcloud::NextCloudChannel;
use crate::pages::channels::nostr::NostrChannel;
use crate::pages::channels::signal::SignalChannel;
use crate::pages::channels::slack::SlackChannel;
use crate::pages::channels::telegram::TelegramChannel;
use crate::pages::channels::tlon::TlonChannel;
use crate::pages::channels::twitch::TwitchChannel;
use crate::pages::channels::webhook::WebhookChannel;
use crate::pages::channels::whatsapp::WhatsAppChannel;
use crate::pages::channels::zalo::ZaloChannel;
use crate::pages::channels::{Channels, ChannelsAdd, ChannelsEdit, ChannelsHealth};
use crate::pages::config::{Config, ConfigSection};
use crate::pages::cron::{Cron, CronDetail, CronNew};
use crate::pages::debug::Debug;
use crate::pages::device_pair::DevicePair;
use crate::pages::instances::Instances;
use crate::pages::logs::Logs;
use crate::pages::models::Models;
use crate::pages::models::connect_provider::ConnectProvider;
use crate::pages::nodes::{Nodes, NodesDetail};
use crate::pages::overview::Overview;
use crate::pages::sessions::Sessions;
use crate::pages::settings::Settings;
use crate::pages::skills::Skills;
use crate::pages::tts::Tts;
use crate::pages::usage::Usage;
use crate::pages::voice::Voice;

#[derive(Routable, Clone, PartialEq, Debug)]
#[rustfmt::skip]
pub enum Route {
    #[layout(Layout)]
        #[route("/")]
        Overview {},
        #[route("/sessions")]
        Sessions {},
        #[route("/agents")]
        Agents {},
        #[route("/agents/new")]
        AgentsNew {},
        #[route("/agents/:agent_id")]
        AgentsDetail { agent_id: String },
        #[route("/agents/:agent_id/:tab")]
        AgentsDetailTab { agent_id: String, tab: String },
        #[route("/channels")]
        Channels {},
        #[route("/channels/add")]
        ChannelsAdd {},
        #[route("/channels/edit/:channel_id")]
        ChannelsEdit { channel_id: String },
        #[route("/channels/health/:channel_id")]
        ChannelsHealth { channel_id: String },
        #[route("/channels/discord")]
        DiscordChannel {},
        #[route("/channels/dingtalk")]
        DingtalkChannel {},
        #[route("/channels/telegram")]
        TelegramChannel {},
        #[route("/channels/whatsapp")]
        WhatsAppChannel {},
        #[route("/channels/slack")]
        SlackChannel {},
        #[route("/channels/nostr")]
        NostrChannel {},
        #[route("/channels/signal")]
        SignalChannel {},
        #[route("/channels/matrix")]
        MatrixChannel {},
        #[route("/channels/cokret")]
        CokretChannel {},
        #[route("/channels/mattermost")]
        MattermostChannel {},
        #[route("/channels/googlechat")]
        GoogleChatChannel {},
        #[route("/channels/line")]
        LineChannel {},
        #[route("/channels/feishu")]
        FeishuChannel {},
        #[route("/channels/irc")]
        IrcChannel {},
        #[route("/channels/msteams")]
        MsTeamsChannel {},
        #[route("/channels/imessage")]
        IMessageChannel {},
        #[route("/channels/webhook")]
        WebhookChannel {},
        #[route("/channels/zalo")]
        ZaloChannel {},
        #[route("/channels/nextcloud")]
        NextCloudChannel {},
        #[route("/channels/twitch")]
        TwitchChannel {},
        #[route("/channels/tlon")]
        TlonChannel {},
        #[route("/models")]
        Models {},
        #[route("/config")]
        Config {},
        #[route("/config/:section")]
        ConfigSection { section: String },
        #[route("/cron")]
        Cron {},
        #[route("/cron/new")]
        CronNew {},
        #[route("/cron/:job_id")]
        CronDetail { job_id: String },
        #[route("/debug")]
        Debug {},
        #[route("/instances")]
        Instances {},
        #[route("/logs")]
        Logs {},
        #[route("/usage")]
        Usage {},
        #[route("/approvals")]
        Approvals {},
        #[route("/nodes")]
        Nodes {},
        #[route("/nodes/:node_id")]
        NodesDetail { node_id: String },
        #[route("/skills")]
        Skills {},
        #[route("/tts")]
        Tts {},
        #[route("/voice")]
        Voice {},
        #[route("/device-pair")]
        DevicePair {},
        #[route("/models/connect-provider")]
        ConnectProvider {},
        #[route("/settings")]
        Settings {},
}
