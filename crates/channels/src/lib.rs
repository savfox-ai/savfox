//! Multi-platform chat channel adapters.
//!
//! Each top-level module under `channels` implements the inbound + outbound
//! glue for one chat platform — Slack, Discord, Telegram, Matrix, Feishu,
//! DingTalk, Mattermost, MS Teams, LINE, IRC, WhatsApp, and several
//! bridge-only formats (QQ, WeChat, Zalo).
//!
//! Every adapter splits responsibilities the same way:
//!
//! * `client.rs` — stateless API client functions (send / edit / delete + webhook signature
//!   verification). No per-channel state; takes a shared `&reqwest::Client` and a token.
//! * `parse.rs` — converts an inbound webhook / polling payload into a
//!   [`savfox_core::channel::ChannelAction`] for the rest of the system.
//! * `config.rs` — typed wrapper around the platform's saved `ChannelConfig` JSON.
//! * `mod.rs` — re-exports + the `Channel` trait impl glue.
//!
//! Common helpers live in [`base`] (config-store accessors, token resolution
//! template) and [`http`] (response checking + constant-time HMAC verify).

#![allow(clippy::if_same_then_else, clippy::manual_let_else)]

pub mod base;
pub mod dingtalk;
pub mod discord;
pub mod feishu;
pub mod googlechat;
pub mod http;
pub mod irc;
pub mod line;
pub mod matrix;
pub mod mattermost;
pub mod msteams;
pub mod qq;
pub mod slack;
pub mod telegram;
pub mod wechat;
pub mod whatsapp;
pub mod zalo;
