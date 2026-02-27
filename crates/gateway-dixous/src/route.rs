use dioxus::prelude::*;

use crate::components::layout::Layout;
use crate::pages::agents::Agents;
use crate::pages::approvals::Approvals;
use crate::pages::channels::discord::DiscordChannel;
use crate::pages::channels::feishu::FeishuChannel;
use crate::pages::channels::google_chat::GoogleChatChannel;
use crate::pages::channels::imessage::IMessageChannel;
use crate::pages::channels::irc::IrcChannel;
use crate::pages::channels::line::LineChannel;
use crate::pages::channels::matrix::MatrixChannel;
use crate::pages::channels::mattermost::MattermostChannel;
use crate::pages::channels::msteams::MsTeamsChannel;
use crate::pages::channels::nostr::NostrChannel;
use crate::pages::channels::signal::SignalChannel;
use crate::pages::channels::slack::SlackChannel;
use crate::pages::channels::telegram::TelegramChannel;
use crate::pages::channels::whatsapp::WhatsAppChannel;
use crate::pages::channels::Channels;
use crate::pages::config::{Config, ConfigSection};
use crate::pages::cron::Cron;
use crate::pages::debug::Debug;
use crate::pages::device_pair::DevicePair;
use crate::pages::instances::Instances;
use crate::pages::logs::Logs;
use crate::pages::memory::Memory;
use crate::pages::models::connect_provider::ConnectProvider;
use crate::pages::models::Models;
use crate::pages::nodes::Nodes;
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
        #[route("/memory")]
        Memory {},
        #[route("/agents")]
        Agents {},
        #[route("/channels")]
        Channels {},
        #[route("/channels/discord")]
        DiscordChannel {},
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
        #[route("/models")]
        Models {},
        #[route("/config")]
        Config {},
        #[route("/config/:section")]
        ConfigSection { section: String },
        #[route("/cron")]
        Cron {},
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
