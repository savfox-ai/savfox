use anyhow::Context;
use matrix_bot_sdk::client::{MatrixAuth, MatrixClient};
use reqwest::Method;
use savfox_channels::matrix::{
    MatrixAutoJoin, MatrixChannelConfig as MatrixPlatformConfig, MatrixInviteEvent,
    resolve_matrix_outbound_config as resolve_saved_matrix_config,
};
use savfox_core::channel::Channel;
use serde_json::Value;
use tracing::{debug, warn};
use url::Url;

use super::{GatewayChannel, ResolvedMatrixClient, non_empty_trimmed};

fn matrix_invite_matches_target(invite: &MatrixInviteEvent, target: &str) -> bool {
    let target = target.trim();
    if target.is_empty() {
        return false;
    }
    if target == "*" {
        return true;
    }
    invite.room_id.eq_ignore_ascii_case(target)
        || invite
            .canonical_alias
            .as_deref()
            .is_some_and(|alias| alias.eq_ignore_ascii_case(target))
}

fn matrix_invite_auto_join_allowed(
    config: &MatrixPlatformConfig,
    invite: &MatrixInviteEvent,
) -> bool {
    match config.auto_join {
        MatrixAutoJoin::Off => false,
        MatrixAutoJoin::Always => true,
        MatrixAutoJoin::Allowlist => {
            let targets = if config.auto_join_allowlist.is_empty() {
                &config.rooms
            } else {
                &config.auto_join_allowlist
            };
            !targets.is_empty()
                && targets
                    .iter()
                    .any(|target| matrix_invite_matches_target(invite, target))
        }
    }
}

impl GatewayChannel {
    // === Platform API calls ===

    /// Send a message to a Discord channel via the Bot API.
    pub(crate) async fn send_discord_message(
        &self,
        bot_token: &str,
        channel_id: &str,
        content: &str,
        reply_to_message_id: Option<&str>,
    ) -> anyhow::Result<()> {
        savfox_channels::discord::client::send_message(
            &self.http_client,
            bot_token,
            channel_id,
            content,
            reply_to_message_id,
        )
        .await
    }

    /// Send a Discord message and return the resulting message ID.
    pub(crate) async fn send_discord_message_with_id(
        &self,
        bot_token: &str,
        channel_id: &str,
        content: &str,
        reply_to_message_id: Option<&str>,
    ) -> anyhow::Result<Option<String>> {
        savfox_channels::discord::client::send_message_returning_id(
            &self.http_client,
            bot_token,
            channel_id,
            content,
            reply_to_message_id,
        )
        .await
    }

    /// Edit a Discord message.
    pub(crate) async fn edit_discord_message(
        &self,
        bot_token: &str,
        channel_id: &str,
        message_id: &str,
        content: &str,
    ) -> anyhow::Result<()> {
        savfox_channels::discord::client::edit_message(
            &self.http_client,
            bot_token,
            channel_id,
            message_id,
            content,
        )
        .await
    }

    /// Delete a Discord message.
    pub(crate) async fn delete_discord_message(
        &self,
        bot_token: &str,
        channel_id: &str,
        message_id: &str,
    ) -> anyhow::Result<()> {
        savfox_channels::discord::client::delete_message(
            &self.http_client,
            bot_token,
            channel_id,
            message_id,
        )
        .await
    }

    /// Send a rich embed message to a Discord channel.
    pub(crate) async fn send_discord_embed(
        &self,
        bot_token: &str,
        channel_id: &str,
        title: &str,
        description: &str,
        color: u32,
    ) -> anyhow::Result<()> {
        savfox_channels::discord::client::send_embed(
            &self.http_client,
            bot_token,
            channel_id,
            title,
            description,
            color,
        )
        .await
    }

    /// Send a message via the Telegram Bot API.
    pub(crate) async fn send_telegram_message(
        &self,
        bot_token: &str,
        chat_id: &str,
        text: &str,
        parse_mode: Option<&str>,
        reply_to_message_id: Option<&str>,
    ) -> anyhow::Result<()> {
        savfox_channels::telegram::client::send_message(
            &self.http_client,
            bot_token,
            chat_id,
            text,
            parse_mode,
            reply_to_message_id,
        )
        .await
        .map(|_| ())
    }

    /// Send a Telegram message and return the resulting `message_id`.
    pub(crate) async fn send_telegram_message_with_id(
        &self,
        bot_token: &str,
        chat_id: &str,
        text: &str,
        parse_mode: Option<&str>,
        reply_to_message_id: Option<&str>,
    ) -> anyhow::Result<Option<i64>> {
        savfox_channels::telegram::client::send_message(
            &self.http_client,
            bot_token,
            chat_id,
            text,
            parse_mode,
            reply_to_message_id,
        )
        .await
    }

    /// Edit an existing Telegram message.
    pub(crate) async fn edit_telegram_message(
        &self,
        bot_token: &str,
        chat_id: &str,
        message_id: i64,
        text: &str,
        parse_mode: Option<&str>,
    ) -> anyhow::Result<()> {
        savfox_channels::telegram::client::edit_message(
            &self.http_client,
            bot_token,
            chat_id,
            message_id,
            text,
            parse_mode,
        )
        .await
    }

    /// Resolve the Telegram bot token from saved configs, runtime secrets, or env.
    pub(crate) async fn resolve_telegram_bot_token(&self) -> Option<String> {
        let savfox_home = &self.config.savfox_home;
        if let Ok(Some(t)) =
            savfox_channels::telegram::resolve_telegram_outbound_token(savfox_home).await
        {
            return Some(t);
        }
        let runtime = self.runtime_channel_secrets.read().await;
        if let Some(t) = runtime.telegram_bot_token.clone() {
            return Some(t);
        }
        std::env::var("TELEGRAM_BOT_TOKEN").ok()
    }

    /// Resolve a Discord bot token from saved configs, runtime secrets, or env.
    pub(crate) async fn resolve_discord_bot_token(&self) -> Option<String> {
        let savfox_home = &self.config.savfox_home;
        if let Ok(Some(t)) =
            savfox_channels::discord::resolve_discord_outbound_token(savfox_home).await
        {
            return Some(t);
        }
        std::env::var("DISCORD_BOT_TOKEN").ok()
    }

    /// Resolve a Slack bot token from saved configs, runtime secrets, or env.
    pub(crate) async fn resolve_slack_bot_token(&self) -> Option<String> {
        let savfox_home = &self.config.savfox_home;
        if let Ok(Some(t)) = savfox_channels::slack::resolve_slack_outbound_token(savfox_home).await
        {
            return Some(t);
        }
        std::env::var("SLACK_BOT_TOKEN").ok()
    }

    /// Resolve Mattermost server URL and token from saved configs or env.
    pub(crate) async fn resolve_mattermost_config(&self) -> Option<(String, String)> {
        let savfox_home = &self.config.savfox_home;
        if let Ok(Some(pair)) =
            savfox_channels::mattermost::resolve_mattermost_outbound_config(savfox_home).await
        {
            return Some(pair);
        }
        let server =
            std::env::var("MATTERMOST_URL").unwrap_or_else(|_| "http://localhost:8065".to_owned());
        let token = std::env::var("MATTERMOST_TOKEN").ok()?;
        Some((server, token))
    }

    /// Send a message via the Slack API.
    pub(crate) async fn send_slack_message(
        &self,
        bot_token: &str,
        channel: &str,
        text: &str,
        blocks: Option<Value>,
        thread_ts: Option<&str>,
    ) -> anyhow::Result<()> {
        savfox_channels::slack::client::send_message(
            &self.http_client,
            bot_token,
            channel,
            text,
            blocks,
            thread_ts,
        )
        .await
    }

    /// Send a Slack message and return the resulting message timestamp.
    pub(crate) async fn send_slack_message_with_id(
        &self,
        bot_token: &str,
        channel: &str,
        text: &str,
        thread_ts: Option<&str>,
    ) -> anyhow::Result<Option<String>> {
        savfox_channels::slack::client::send_message_returning_id(
            &self.http_client,
            bot_token,
            channel,
            text,
            thread_ts,
        )
        .await
    }

    /// Edit a Slack message by its timestamp.
    pub(crate) async fn edit_slack_message(
        &self,
        bot_token: &str,
        channel: &str,
        ts: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        savfox_channels::slack::client::edit_message(
            &self.http_client,
            bot_token,
            channel,
            ts,
            text,
        )
        .await
    }

    /// Delete a Slack message by its timestamp.
    pub(crate) async fn delete_slack_message(
        &self,
        bot_token: &str,
        channel: &str,
        ts: &str,
    ) -> anyhow::Result<()> {
        savfox_channels::slack::client::delete_message(&self.http_client, bot_token, channel, ts)
            .await
    }

    pub(crate) async fn resolve_matrix_client(
        homeserver: &str,
        access_token: Option<&str>,
        user_id: Option<&str>,
        password: Option<&str>,
        device_name: Option<&str>,
    ) -> anyhow::Result<ResolvedMatrixClient> {
        let homeserver_url = Url::parse(homeserver)
            .with_context(|| format!("invalid Matrix homeserver URL: {homeserver}"))?;

        if let Some(access_token) = non_empty_trimmed(access_token) {
            let auth = if let Some(user_id) = non_empty_trimmed(user_id) {
                MatrixAuth::new(access_token.to_owned()).with_user_id(user_id.to_owned())
            } else {
                MatrixAuth::new(access_token.to_owned())
            };
            let client = MatrixClient::new(homeserver_url, auth);
            let whoami = client
                .whoami()
                .await
                .context("failed to resolve Matrix user via whoami")?;

            let mut resolved_auth =
                MatrixAuth::new(access_token.to_owned()).with_user_id(whoami.user_id.clone());
            if let Some(device_id) = whoami.device_id {
                resolved_auth = resolved_auth.with_device_id(device_id);
            }
            client.login_with_token(resolved_auth).await;

            return Ok(ResolvedMatrixClient {
                client,
                access_token: access_token.to_owned(),
                user_id: whoami.user_id,
            });
        }

        let user_id = non_empty_trimmed(user_id)
            .context("Matrix userId is required when no access token is configured")?;
        let password = non_empty_trimmed(password)
            .context("Matrix password is required when no access token is configured")?;
        let client = MatrixClient::new(homeserver_url, MatrixAuth::default());
        let auth = client
            .password_login(user_id, password, non_empty_trimmed(device_name))
            .await
            .context("Matrix password login failed")?;
        let resolved_user_id = auth.user_id.clone().unwrap_or_else(|| user_id.to_owned());
        let access_token = auth.access_token.clone();

        let mut resolved_auth =
            MatrixAuth::new(access_token.clone()).with_user_id(resolved_user_id.clone());
        if let Some(device_id) = auth.device_id {
            resolved_auth = resolved_auth.with_device_id(device_id);
        }
        client.login_with_token(resolved_auth).await;

        Ok(ResolvedMatrixClient {
            client,
            access_token,
            user_id: resolved_user_id,
        })
    }

    pub(crate) async fn resolve_matrix_outbound_config(
        &self,
        room_id: &str,
    ) -> anyhow::Result<Option<MatrixPlatformConfig>> {
        resolve_saved_matrix_config(&self.config.savfox_home, room_id).await
    }

    pub(crate) async fn resolve_matrix_user_id_for_room(
        &self,
        room_id: Option<&str>,
    ) -> anyhow::Result<Option<String>> {
        let config = self
            .resolve_matrix_outbound_config(room_id.unwrap_or_default())
            .await?;
        let Some(config) = config else {
            return Ok(None);
        };

        if let Some(runtime_user_id) = crate::channels::matrix::matrix_runtime_state_for(&config.id)
            .and_then(|state| state.user_id)
            .and_then(|user_id| {
                let user_id = user_id.trim().to_owned();
                if user_id.is_empty() {
                    None
                } else {
                    Some(user_id)
                }
            })
        {
            return Ok(Some(runtime_user_id));
        }

        if let Some(config_user_id) = non_empty_trimmed(config.user_id.as_deref()) {
            return Ok(Some(config_user_id.to_owned()));
        }
        if let Some(bot_user_id) = config.bot_user_id() {
            return Ok(Some(bot_user_id));
        }

        let resolved = Self::resolve_matrix_client(
            &config.homeserver,
            config.access_token.as_deref(),
            config.user_id.as_deref(),
            config.password.as_deref(),
            config.device_name.as_deref(),
        )
        .await?;
        Ok(Some(resolved.user_id))
    }

    /// Send a message via Matrix using matrix-bot-sdk.
    pub(crate) async fn send_matrix_message(
        &self,
        homeserver: &str,
        access_token: &str,
        user_id: Option<&str>,
        room_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        let homeserver_url = Url::parse(homeserver)
            .with_context(|| format!("invalid Matrix homeserver URL: {homeserver}"))?;
        let auth = if let Some(uid) = non_empty_trimmed(user_id) {
            MatrixAuth::new(access_token).with_user_id(uid.to_owned())
        } else {
            MatrixAuth::new(access_token)
        };
        let client = MatrixClient::new(homeserver_url, auth);
        client
            .send_text(room_id, text)
            .await
            .with_context(|| format!("failed to send Matrix message to room {room_id}"))?;
        Ok(())
    }

    async fn join_matrix_room_for_config(
        &self,
        room_id: &str,
        config: &MatrixPlatformConfig,
        configured_user_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let runtime_state = crate::channels::matrix::matrix_runtime_state_for(&config.id);
        let joined_room_id = if let Some(appservice_channel) =
            crate::channels::matrix::matrix_appservice_channel_for(&config.id)
        {
            appservice_channel.join_room(room_id).await?;
            room_id.to_owned()
        } else if let Some(runtime_state) = runtime_state.as_ref()
            && let Some(access_token) = runtime_state.access_token.as_deref()
        {
            let homeserver = runtime_state
                .homeserver
                .as_deref()
                .unwrap_or(&config.homeserver);
            let homeserver_url = Url::parse(homeserver).with_context(|| {
                format!(
                    "invalid Matrix homeserver URL for config {}: {}",
                    config.id, homeserver
                )
            })?;
            let auth = if let Some(uid) = configured_user_id {
                MatrixAuth::new(access_token.to_owned()).with_user_id(uid.to_owned())
            } else {
                MatrixAuth::new(access_token.to_owned())
            };
            let client = MatrixClient::new(homeserver_url, auth);
            client
                .join_room(room_id)
                .await
                .with_context(|| format!("failed to auto-join Matrix room {room_id}"))?
        } else {
            let resolved = Self::resolve_matrix_client(
                &config.homeserver,
                config.access_token.as_deref(),
                config.user_id.as_deref(),
                config.password.as_deref(),
                config.device_name.as_deref(),
            )
            .await
            .with_context(|| {
                format!(
                    "failed to authenticate Matrix client for config {}",
                    config.id
                )
            })?;
            resolved
                .client
                .join_room(room_id)
                .await
                .with_context(|| format!("failed to auto-join Matrix room {room_id}"))?
        };
        tracing::info!(
            room_id,
            joined_room_id,
            config_id = %config.id,
            "Auto-joined Matrix room after invite"
        );
        Ok(())
    }

    /// Join a Matrix room when this gateway user receives an invite event.
    pub(crate) async fn auto_join_matrix_invited_room(
        &self,
        invite: &MatrixInviteEvent,
    ) -> anyhow::Result<()> {
        let room_id = invite.room_id.trim();
        if room_id.is_empty() {
            return Ok(());
        }

        let Some(config) = self.resolve_matrix_outbound_config(room_id).await? else {
            warn!(
                room_id,
                "Matrix invite received but no Matrix channel config is available"
            );
            return Ok(());
        };

        let runtime_state = crate::channels::matrix::matrix_runtime_state_for(&config.id);
        let configured_user_id = runtime_state
            .as_ref()
            .and_then(|state| non_empty_trimmed(state.user_id.as_deref()))
            .or_else(|| non_empty_trimmed(config.user_id.as_deref()));
        let bot_user_id = config.bot_user_id();
        let configured_user_id = configured_user_id
            .map(str::to_owned)
            .or(bot_user_id.clone());
        if let (Some(invited), Some(configured)) = (
            non_empty_trimmed(invite.invited_user_id.as_deref()),
            configured_user_id.as_deref(),
        ) && !invited.eq_ignore_ascii_case(configured)
        {
            return Ok(());
        }

        if !matrix_invite_auto_join_allowed(&config, invite) {
            let store = crate::channels::matrix::MatrixInviteStore::for_savfox_home(
                &self.config.savfox_home,
            );
            store
                .upsert(crate::channels::matrix::MatrixPendingInvite::from_event(
                    &config.id, invite,
                ))
                .await?;
            return Ok(());
        }

        self.join_matrix_room_for_config(room_id, &config, configured_user_id.as_deref())
            .await?;
        let store =
            crate::channels::matrix::MatrixInviteStore::for_savfox_home(&self.config.savfox_home);
        let _ = store.remove(&config.id, room_id).await;
        Ok(())
    }

    async fn matrix_invite_action_config(
        &self,
        config_id: &str,
        room_id: &str,
    ) -> anyhow::Result<MatrixPlatformConfig> {
        if let Some(config_id) = non_empty_trimmed(Some(config_id)) {
            let configs =
                savfox_core::config::channel_store::list_channel_configs(&self.config.savfox_home)
                    .await
                    .context("failed to load Matrix channel configs")?;
            for config in configs {
                if config.id != config_id || !config.kind.eq_ignore_ascii_case("matrix") {
                    continue;
                }
                return MatrixPlatformConfig::from_channel_config(&config)
                    .context("Matrix channel config must be an object");
            }
            anyhow::bail!("Matrix channel config '{config_id}' was not found");
        }

        self.resolve_matrix_outbound_config(room_id)
            .await?
            .context("Matrix invite received but no Matrix channel config is available")
    }

    pub(crate) async fn accept_matrix_invite(
        &self,
        config_id: &str,
        room_id: &str,
    ) -> anyhow::Result<()> {
        let room_id = room_id.trim();
        if room_id.is_empty() {
            anyhow::bail!("missing Matrix room_id");
        }
        let config = self.matrix_invite_action_config(config_id, room_id).await?;
        let runtime_state = crate::channels::matrix::matrix_runtime_state_for(&config.id);
        let configured_user_id = runtime_state
            .as_ref()
            .and_then(|state| non_empty_trimmed(state.user_id.as_deref()))
            .or_else(|| non_empty_trimmed(config.user_id.as_deref()))
            .map(str::to_owned)
            .or_else(|| config.bot_user_id());

        self.join_matrix_room_for_config(room_id, &config, configured_user_id.as_deref())
            .await?;

        let store =
            crate::channels::matrix::MatrixInviteStore::for_savfox_home(&self.config.savfox_home);
        let _ = store.remove(&config.id, room_id).await;
        Ok(())
    }

    pub(crate) async fn reject_matrix_invite(
        &self,
        config_id: &str,
        room_id: &str,
    ) -> anyhow::Result<()> {
        let room_id = room_id.trim();
        if room_id.is_empty() {
            anyhow::bail!("missing Matrix room_id");
        }
        let config = self.matrix_invite_action_config(config_id, room_id).await?;
        let resolved = Self::resolve_matrix_client(
            &config.homeserver,
            config.access_token.as_deref(),
            config.user_id.as_deref(),
            config.password.as_deref(),
            config.device_name.as_deref(),
        )
        .await
        .with_context(|| {
            format!(
                "failed to authenticate Matrix client for config {}",
                config.id
            )
        })?;
        let encoded_room_id: String =
            url::form_urlencoded::byte_serialize(room_id.as_bytes()).collect();
        let endpoint = format!("/_matrix/client/v3/rooms/{encoded_room_id}/leave");
        let body = serde_json::json!({});
        resolved
            .client
            .raw_json(Method::POST, &endpoint, Some(body))
            .await
            .with_context(|| format!("failed to reject Matrix invite for room {room_id}"))?;

        let store =
            crate::channels::matrix::MatrixInviteStore::for_savfox_home(&self.config.savfox_home);
        let _ = store.remove(&config.id, room_id).await;
        Ok(())
    }

    /// Send a message via the Mattermost REST API.
    pub(crate) async fn send_mattermost_message(
        &self,
        server_url: &str,
        access_token: &str,
        channel_id: &str,
        text: &str,
        root_id: Option<&str>,
    ) -> anyhow::Result<()> {
        savfox_channels::mattermost::client::send_message(
            &self.http_client,
            server_url,
            access_token,
            channel_id,
            text,
            root_id,
        )
        .await
    }

    /// Send a Mattermost message and return the resulting post ID.
    pub(crate) async fn send_mattermost_message_with_id(
        &self,
        server_url: &str,
        access_token: &str,
        channel_id: &str,
        text: &str,
        root_id: Option<&str>,
    ) -> anyhow::Result<Option<String>> {
        savfox_channels::mattermost::client::send_message_returning_id(
            &self.http_client,
            server_url,
            access_token,
            channel_id,
            text,
            root_id,
        )
        .await
    }

    /// Edit a Mattermost post.
    pub(crate) async fn edit_mattermost_message(
        &self,
        server_url: &str,
        access_token: &str,
        post_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        savfox_channels::mattermost::client::edit_message(
            &self.http_client,
            server_url,
            access_token,
            post_id,
            text,
        )
        .await
    }

    /// Delete a Mattermost post.
    pub(crate) async fn delete_mattermost_message(
        &self,
        server_url: &str,
        access_token: &str,
        post_id: &str,
    ) -> anyhow::Result<()> {
        savfox_channels::mattermost::client::delete_message(
            &self.http_client,
            server_url,
            access_token,
            post_id,
        )
        .await
    }

    /// Send a message via the Google Chat API (webhook URL or Spaces API).
    pub(crate) async fn send_googlechat_message(
        &self,
        webhook_url: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        let ssrf_cfg = crate::ssrf::SsrfConfig::from_env();
        let body = serde_json::json!({ "text": text });
        // `build_pinned_client` re-validates the URL and pins the resolved IP
        // into the client's resolver, closing the DNS-rebinding window
        // between validation and connection.
        let client = crate::ssrf::build_pinned_client(webhook_url, &ssrf_cfg)
            .await
            .map_err(|err| anyhow::anyhow!("blocked googlechat webhook url: {err}"))?;

        let response = client
            .post(webhook_url)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.bytes().await.unwrap_or_default();
            warn!(
                "Google Chat API error: HTTP {status}: {}",
                String::from_utf8_lossy(&body)
            );
        }
        Ok(())
    }

    /// Send a message via the QQ bridge webhook.
    pub(crate) async fn send_qq_message(
        &self,
        webhook_url: &str,
        channel: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        savfox_channels::qq::send_webhook_message(&self.http_client, webhook_url, channel, text)
            .await
    }

    /// Send a message via the WeChat bridge webhook.
    pub(crate) async fn send_wechat_message(
        &self,
        webhook_url: &str,
        channel: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        savfox_channels::wechat::send_webhook_message(&self.http_client, webhook_url, channel, text)
            .await
    }

    /// Send a message via the Microsoft Teams Bot Framework / webhook.
    pub(crate) async fn send_teams_message(
        &self,
        webhook_url: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        let ssrf_cfg = crate::ssrf::SsrfConfig::from_env();
        let body = serde_json::json!({
            "type": "message",
            "text": text,
        });
        let client = crate::ssrf::build_pinned_client(webhook_url, &ssrf_cfg)
            .await
            .map_err(|err| anyhow::anyhow!("blocked teams webhook url: {err}"))?;

        let response = client
            .post(webhook_url)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.bytes().await.unwrap_or_default();
            warn!(
                "Teams API error: HTTP {status}: {}",
                String::from_utf8_lossy(&body)
            );
        }
        Ok(())
    }

    /// Send a reply message via the LINE Messaging API.
    pub(crate) async fn send_line_message(
        &self,
        channel_token: &str,
        reply_token: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        savfox_channels::line::client::reply_message(
            &self.http_client,
            channel_token,
            reply_token,
            text,
        )
        .await
    }

    /// Push a message via the LINE Messaging API (no reply token needed).
    pub(crate) async fn push_line_message(
        &self,
        channel_token: &str,
        user_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        savfox_channels::line::client::push_message(&self.http_client, channel_token, user_id, text)
            .await
    }

    /// Send a message via the Feishu/Lark Bot API.
    pub(crate) async fn send_feishu_message(
        &self,
        base_url: &str,
        tenant_access_token: &str,
        receive_id: &str,
        receive_id_type: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        savfox_channels::feishu::client::send_message(
            &self.http_client,
            base_url,
            tenant_access_token,
            receive_id,
            receive_id_type,
            text,
        )
        .await
    }

    /// Send a Feishu message and return the resulting message_id.
    pub(crate) async fn send_feishu_message_with_id(
        &self,
        base_url: &str,
        tenant_access_token: &str,
        receive_id: &str,
        receive_id_type: &str,
        text: &str,
    ) -> anyhow::Result<Option<String>> {
        savfox_channels::feishu::client::send_message_returning_id(
            &self.http_client,
            base_url,
            tenant_access_token,
            receive_id,
            receive_id_type,
            text,
        )
        .await
    }

    /// Edit a Feishu message by its message_id.
    pub(crate) async fn edit_feishu_message(
        &self,
        base_url: &str,
        tenant_access_token: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        savfox_channels::feishu::client::edit_message(
            &self.http_client,
            base_url,
            tenant_access_token,
            message_id,
            text,
        )
        .await
    }

    /// Delete a Feishu message by its message_id.
    pub(crate) async fn delete_feishu_message(
        &self,
        base_url: &str,
        tenant_access_token: &str,
        message_id: &str,
    ) -> anyhow::Result<()> {
        savfox_channels::feishu::client::delete_message(
            &self.http_client,
            base_url,
            tenant_access_token,
            message_id,
        )
        .await
    }

    /// Send a message via DingTalk custom robot webhook.
    ///
    /// `webhook_or_token` accepts either a full webhook URL or an access token.
    pub(crate) async fn send_dingtalk_message(
        &self,
        webhook_or_token: &str,
        secret: Option<&str>,
        text: &str,
    ) -> anyhow::Result<()> {
        savfox_channels::dingtalk::send_dingtalk_text_message(
            &self.http_client,
            webhook_or_token,
            secret,
            None,
            text,
        )
        .await
    }

    /// Send a message via the WhatsApp Cloud API.
    pub(crate) async fn send_whatsapp_message(
        &self,
        phone_number_id: &str,
        access_token: &str,
        to: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        savfox_channels::whatsapp::client::send_message(
            &self.http_client,
            phone_number_id,
            access_token,
            to,
            text,
        )
        .await
    }

    /// Send a message via the Zalo OA Customer Service API.
    pub(crate) async fn send_zalo_message(
        &self,
        access_token: &str,
        user_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        savfox_channels::zalo::send_message(&self.http_client, access_token, user_id, text).await
    }

    /// Send a message via an IRC channel HTTP API.
    pub(crate) async fn send_irc_message(
        &self,
        channel_url: &str,
        channel_name: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        savfox_channels::irc::send_message(&self.http_client, channel_url, channel_name, text).await
    }

    /// Send a message to the appropriate platform via the gateway.
    /// The `channel` format is `platform:id` (e.g., `discord:12345`, `telegram:67890`).
    /// Platform credentials are resolved from saved channel configs, then runtime
    /// secrets, then environment variables.
    pub(crate) async fn send_platform_message(
        &self,
        channel: &str,
        text: &str,
        discord_token: Option<&str>,
        telegram_token: Option<&str>,
        slack_token: Option<&str>,
    ) -> anyhow::Result<()> {
        self.send_platform_message_with_context(
            channel,
            text,
            discord_token,
            telegram_token,
            slack_token,
            None,
            None,
            None,
        )
        .await
    }

    /// Send a message with optional thread, reply, and saved-instance context.
    pub(crate) async fn send_platform_message_with_context(
        &self,
        channel: &str,
        text: &str,
        discord_token: Option<&str>,
        telegram_token: Option<&str>,
        slack_token: Option<&str>,
        session_id: Option<&str>,
        reply_target: Option<&str>,
        saved_channel_config_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let runtime = self.runtime_channel_secrets.read().await.clone();
        let (platform, channel_id) = channel.split_once(':').unwrap_or(("webhook", channel));
        let savfox_home = &self.config.savfox_home;

        match platform {
            "discord" => {
                let saved_token =
                    savfox_channels::discord::resolve_discord_outbound_token(savfox_home)
                        .await
                        .ok()
                        .flatten();
                let token = discord_token
                    .map(|s| s.to_owned())
                    .or(saved_token)
                    .or(runtime.discord_bot_token.clone())
                    .or_else(|| std::env::var("DISCORD_BOT_TOKEN").ok());
                if let Some(token) = token {
                    self.send_discord_message(&token, channel_id, text, reply_target)
                        .await?;
                } else {
                    warn!("Discord token not configured for channel: {channel}");
                }
            }
            "telegram" => {
                let saved_token =
                    savfox_channels::telegram::resolve_telegram_outbound_token(savfox_home)
                        .await
                        .ok()
                        .flatten();
                let token_source;
                let token = if let Some(t) = telegram_token.map(|s| s.to_owned()) {
                    token_source = "explicit";
                    Some(t)
                } else if let Some(t) = saved_token {
                    token_source = "saved_channel_config";
                    Some(t)
                } else if let Some(t) = runtime.telegram_bot_token.clone() {
                    token_source = "runtime_secrets";
                    Some(t)
                } else if let Ok(t) = std::env::var("TELEGRAM_BOT_TOKEN") {
                    token_source = "env_var";
                    Some(t)
                } else {
                    token_source = "none";
                    None
                };
                debug!(source = token_source, "Token resolved");
                if let Some(token) = token {
                    self.send_telegram_message(
                        &token,
                        channel_id,
                        text,
                        Some("HTML"),
                        reply_target,
                    )
                    .await?;
                } else {
                    warn!(channel = %channel, "No token configured for channel");
                }
            }
            "slack" => {
                let saved_token = savfox_channels::slack::resolve_slack_outbound_token(savfox_home)
                    .await
                    .ok()
                    .flatten();
                let token = slack_token
                    .map(|s| s.to_owned())
                    .or(saved_token)
                    .or(runtime.slack_bot_token.clone())
                    .or_else(|| std::env::var("SLACK_BOT_TOKEN").ok());
                if let Some(token) = token {
                    self.send_slack_message(&token, channel_id, text, None, session_id)
                        .await?;
                } else {
                    warn!("Slack token not configured for channel: {channel}");
                }
            }
            "matrix" => {
                if let Some(config) = self.resolve_matrix_outbound_config(channel_id).await? {
                    if let Some(appservice_channel) =
                        crate::channels::matrix::matrix_appservice_channel_for(&config.id)
                    {
                        appservice_channel.send_message(channel_id, text).await?;
                    } else if let Some(runtime_state) =
                        crate::channels::matrix::matrix_runtime_state_for(&config.id)
                        && let Some(access_token) = runtime_state.access_token.as_deref()
                    {
                        let homeserver = runtime_state
                            .homeserver
                            .as_deref()
                            .unwrap_or(&config.homeserver);
                        let user_id = runtime_state
                            .user_id
                            .as_deref()
                            .or(config.user_id.as_deref());
                        self.send_matrix_message(
                            homeserver,
                            access_token,
                            user_id,
                            channel_id,
                            text,
                        )
                        .await?;
                    } else {
                        let resolved = Self::resolve_matrix_client(
                            &config.homeserver,
                            config.access_token.as_deref(),
                            config.user_id.as_deref(),
                            config.password.as_deref(),
                            config.device_name.as_deref(),
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "failed to authenticate Matrix client for channel '{}'",
                                config.id
                            )
                        })?;
                        resolved
                            .client
                            .send_text(channel_id, text)
                            .await
                            .with_context(|| {
                                format!("failed to send Matrix message to room {channel_id}")
                            })?;
                    }
                } else {
                    warn!("Matrix channel not configured in channels/*.json");
                }
            }
            "mattermost" => {
                let saved =
                    savfox_channels::mattermost::resolve_mattermost_outbound_config(savfox_home)
                        .await
                        .ok()
                        .flatten();
                let (server, token) = if let Some((s, t)) = saved {
                    (s, Some(t))
                } else {
                    let s = std::env::var("MATTERMOST_URL")
                        .unwrap_or_else(|_| "http://localhost:8065".to_owned());
                    let t = std::env::var("MATTERMOST_TOKEN").ok();
                    (s, t)
                };
                if let Some(token) = token {
                    self.send_mattermost_message(&server, &token, channel_id, text, reply_target)
                        .await?;
                } else {
                    warn!("Mattermost token not configured for channel: {channel}");
                }
            }
            "googlechat" | "gchat" => {
                let saved =
                    savfox_channels::googlechat::resolve_googlechat_webhook_url(savfox_home)
                        .await
                        .ok()
                        .flatten();
                let webhook_url = saved.or_else(|| std::env::var("GOOGLECHAT_WEBHOOK_URL").ok());
                if let Some(webhook_url) = webhook_url {
                    self.send_googlechat_message(&webhook_url, text).await?;
                } else {
                    warn!("Google Chat webhook URL not configured");
                }
            }
            "teams" | "msteams" => {
                if let Ok(webhook_url) = std::env::var("TEAMS_WEBHOOK_URL") {
                    self.send_teams_message(&webhook_url, text).await?;
                } else {
                    warn!("Teams webhook URL not configured");
                }
            }
            "line" => {
                let saved = savfox_channels::line::resolve_line_outbound_token(savfox_home)
                    .await
                    .ok()
                    .flatten();
                let token = saved.or_else(|| std::env::var("LINE_CHANNEL_TOKEN").ok());
                if let Some(token) = token {
                    self.push_line_message(&token, channel_id, text).await?;
                } else {
                    warn!("LINE channel token not configured");
                }
            }
            "feishu" | "lark" => {
                let config = crate::channels::feishu::resolve_feishu_outbound_config(
                    &self.config.savfox_home,
                )
                .await?;
                let configured_receive_id_type = config
                    .as_ref()
                    .map(|cfg| cfg.receive_id_type.as_str())
                    .unwrap_or("chat_id");
                let receive_id_type =
                    Self::infer_feishu_receive_id_type(channel_id, configured_receive_id_type);
                let token = if let Some(token) = config
                    .as_ref()
                    .and_then(|cfg| cfg.app_access_token.as_deref())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    Some(token.to_owned())
                } else if let (Some(app_id), Some(app_secret)) = (
                    config
                        .as_ref()
                        .and_then(|cfg| cfg.app_id.as_deref())
                        .map(str::trim)
                        .filter(|value| !value.is_empty()),
                    config
                        .as_ref()
                        .and_then(|cfg| cfg.app_secret.as_deref())
                        .map(str::trim)
                        .filter(|value| !value.is_empty()),
                ) {
                    match crate::channels::feishu::fetch_feishu_tenant_access_token(
                        &self.http_client,
                        config
                            .as_ref()
                            .map(|cfg| cfg.base_url.as_str())
                            .unwrap_or("https://open.feishu.cn"),
                        app_id,
                        app_secret,
                    )
                    .await
                    {
                        Ok(token) => Some(token),
                        Err(err) => {
                            warn!("failed to fetch Feishu tenant token from channel config: {err}");
                            None
                        }
                    }
                } else {
                    std::env::var("FEISHU_TENANT_ACCESS_TOKEN")
                        .ok()
                        .or_else(|| std::env::var("FEISHU_APP_ACCESS_TOKEN").ok())
                };

                if let Some(token) = token {
                    let base_url = config
                        .as_ref()
                        .map(|cfg| cfg.base_url.as_str())
                        .unwrap_or("https://open.feishu.cn");
                    self.send_feishu_message(base_url, &token, channel_id, &receive_id_type, text)
                        .await?;
                } else {
                    warn!(
                        "Feishu credentials are not configured (need tenant token or app_id/app_secret)"
                    );
                }
            }
            "dingtalk" => {
                if channel_id.starts_with("https://") || channel_id.starts_with("http://") {
                    self.send_dingtalk_message(
                        channel_id,
                        std::env::var("DINGTALK_SECRET").ok().as_deref(),
                        text,
                    )
                    .await?;
                } else {
                    let config = crate::channels::dingtalk::resolve_dingtalk_outbound_config(
                        &self.config.savfox_home,
                    )
                    .await?;
                    let target = config
                        .as_ref()
                        .and_then(|cfg| {
                            cfg.webhook_url.clone().or_else(|| cfg.access_token.clone())
                        })
                        .or_else(|| {
                            std::env::var("DINGTALK_WEBHOOK_URL")
                                .ok()
                                .or_else(|| std::env::var("DINGTALK_ACCESS_TOKEN").ok())
                        });
                    let secret = config
                        .as_ref()
                        .and_then(|cfg| cfg.secret.clone())
                        .or_else(|| std::env::var("DINGTALK_SECRET").ok());

                    if let Some(target) = target {
                        self.send_dingtalk_message(&target, secret.as_deref(), text)
                            .await?;
                    } else {
                        warn!(
                            "Dingtalk credentials are not configured (need webhook URL or access token)"
                        );
                    }
                }
            }
            "irc" => {
                let channel_url = std::env::var("IRC_BRIDGE_URL")
                    .unwrap_or_else(|_| "http://127.0.0.1:6667".to_owned());
                self.send_irc_message(&channel_url, channel_id, text)
                    .await?;
            }
            "whatsapp" => {
                let saved =
                    savfox_channels::whatsapp::resolve_whatsapp_outbound_config(savfox_home)
                        .await
                        .ok()
                        .flatten();
                let (phone_id, token) = if let Some((p, t)) = saved {
                    (Some(p), Some(t))
                } else {
                    (
                        std::env::var("WHATSAPP_PHONE_NUMBER_ID").ok(),
                        std::env::var("WHATSAPP_ACCESS_TOKEN").ok(),
                    )
                };
                if let (Some(phone_id), Some(token)) = (phone_id, token) {
                    self.send_whatsapp_message(&phone_id, &token, channel_id, text)
                        .await?;
                } else {
                    warn!(
                        "WhatsApp credentials not configured (need phone_number_id and access_token)"
                    );
                }
            }
            "qq" => {
                let webhook_url = savfox_channels::qq::resolve_qq_webhook_url(savfox_home)
                    .await
                    .ok()
                    .flatten()
                    .or_else(|| std::env::var("QQ_WEBHOOK_URL").ok());
                if let Some(webhook_url) = webhook_url {
                    self.send_qq_message(&webhook_url, channel_id, text).await?;
                } else {
                    warn!("QQ webhook URL not configured");
                }
            }
            "wechat" => {
                let webhook_url = savfox_channels::wechat::resolve_wechat_webhook_url(savfox_home)
                    .await
                    .ok()
                    .flatten()
                    .or_else(|| std::env::var("WECHAT_WEBHOOK_URL").ok());
                if let Some(webhook_url) = webhook_url {
                    self.send_wechat_message(&webhook_url, channel_id, text)
                        .await?;
                } else {
                    warn!("WeChat webhook URL not configured");
                }
            }
            "zalo" => {
                if let Ok(token) = std::env::var("ZALO_OA_ACCESS_TOKEN") {
                    self.send_zalo_message(&token, channel_id, text).await?;
                } else {
                    warn!("Zalo OA access token not configured");
                }
            }
            #[cfg(feature = "arkret")]
            "arkret" => {
                // `reply_target` is either a plain strand id or a strand id
                // with an encoded Sidecar exchange context appended by the
                // inbound dispatch (see `encode_sidecar_reply_target`).
                let (strand_id, sidecar_exchange) = match non_empty_trimmed(reply_target)
                    .filter(|value| value.starts_with("ak:strand:"))
                {
                    Some(value) => {
                        let (strand, context) =
                            savfox_channels::arkret::split_sidecar_reply_target(value);
                        (Some(strand), context)
                    }
                    None => (None, None),
                };
                let strand_id = strand_id.as_deref();
                // Sidecar exchange replies must carry the encrypted
                // `user_facing_response` binding, which only the account path
                // can attach; the applet path would silently drop it.
                if sidecar_exchange.is_none()
                    && crate::channels::arkret_applet::send_to_arkret_applet_for_realm(
                        channel_id, strand_id, text,
                    )
                    .await?
                {
                    return Ok(());
                }
                crate::channels::arkret::send_to_arkret_account(
                    savfox_home,
                    channel_id,
                    strand_id,
                    text,
                    sidecar_exchange.as_ref(),
                    saved_channel_config_id,
                )
                .await?;
            }
            #[cfg(not(feature = "arkret"))]
            "arkret" => {
                warn!("Arkret support is not enabled for channel: {channel}");
            }
            _ => {
                warn!("unknown platform for channel: {channel}");
            }
        }

        Ok(())
    }

    pub(crate) fn infer_feishu_receive_id_type(channel_id: &str, configured: &str) -> String {
        savfox_channels::feishu::client::infer_receive_id_type(channel_id, configured)
    }
}

// === Signature verification utilities ===

/// Verify a Slack request signature using HMAC-SHA256.
pub(crate) fn verify_slack_signature(
    signing_secret: &str,
    timestamp: &str,
    signature: &str,
    body: &[u8],
) -> bool {
    savfox_channels::slack::client::verify_signature(signing_secret, timestamp, body, signature)
}

/// Check if Slack timestamp is within an allowed replay window.
pub(crate) fn is_slack_timestamp_fresh(
    timestamp: &str,
    max_age_secs: u64,
    now_epoch_secs: u64,
) -> bool {
    let Ok(ts) = timestamp.parse::<u64>() else {
        return false;
    };
    if ts > now_epoch_secs {
        return false;
    }
    now_epoch_secs.saturating_sub(ts) <= max_age_secs
}

/// Verify a Discord interaction signature using Ed25519.
pub(crate) fn verify_discord_signature(
    public_key: &str,
    signature: &str,
    timestamp: &str,
    body: &[u8],
) -> bool {
    savfox_channels::discord::client::verify_signature(public_key, timestamp, body, signature)
}

/// Verify a generic webhook HMAC-SHA256 signature.
pub(crate) fn verify_webhook_hmac(secret: &str, signature: &str, body: &[u8]) -> bool {
    use hmac::{Hmac, KeyInit, Mac};
    use sha2::Sha256;

    let mut mac = match Hmac::<Sha256>::new_from_slice(secret.as_bytes()) {
        Ok(mac) => mac,
        Err(_) => return false,
    };
    mac.update(body);
    let result = mac.finalize();
    let computed = hex::encode(result.into_bytes());

    // Support both raw hex and "sha256=" prefix
    let expected = signature.strip_prefix("sha256=").unwrap_or(signature);
    computed == expected
}

/// Verify Telegram webhook secret token equality.
pub(crate) fn verify_telegram_webhook_secret(expected_secret: &str, received_secret: &str) -> bool {
    savfox_channels::telegram::verify_webhook_secret(expected_secret, received_secret)
}

#[cfg(test)]
pub(crate) fn escape_telegram_html_text(text: &str) -> String {
    savfox_channels::telegram::escape_html(text)
}
