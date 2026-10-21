//! Native Realm Bot replies use the Bot's own Device and retained input.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Context as _;
use arkret_bridge_runtime::managed_device::{
    ManagedDevice, ManagedDeviceReader, ManagedDeviceRuntime, NativeContent,
    NativeContentRecipient, NativeContentSink, NativeReplyPreparation, NativeReplyTicket,
};
use arkret_models_collaboration::events_payloads::{
    ContentBlock, MessageCreatePayload, collect_mention_subject_actor_ids,
};
use futures_util::StreamExt as _;
use savfox_core::models_manager::manager::ModelsManager;
use savfox_core::{AuthManager, ModelClient, ResponseEvent, TransportManager};
use savfox_http_client::{HttpClient, ReqwestTransport};

mod transport;

pub struct NativeRealmBotReplies {
    pub(super) runtime: Arc<ManagedDeviceRuntime>,
    pub(super) actor: arkret::ActorId,
    pub(super) channel_id: String,
    pub(super) config: Arc<savfox_core::config::Config>,
    pub(super) active: Arc<AtomicBool>,
}

impl NativeRealmBotReplies {
    /// Mount the same native consumer used by the Applet HTTP host.
    /// Configuration controls the model only; Station authority is checked per input.
    pub async fn from_home(
        runtime: Arc<ManagedDeviceRuntime>,
        actor: arkret::ActorId,
        channel_id: String,
        home: std::path::PathBuf,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            actor.as_realm_actor_id().is_some(),
            "Bot requires a complete Realm identity"
        );
        let config = Arc::new(
            savfox_core::config::ConfigBuilder::default()
                .savfox_home(home)
                .build()
                .await?,
        );
        Ok(Self {
            runtime,
            actor,
            channel_id,
            config,
            active: Arc::new(AtomicBool::new(true)),
        })
    }
    /// Immediately invalidate late provider requests and future deliveries.
    pub fn retire(&self) {
        self.active.store(false, Ordering::Release);
    }

    fn check_active(&self) -> anyhow::Result<()> {
        anyhow::ensure!(self.active.load(Ordering::Acquire), "Applet host retired");
        Ok(())
    }

    async fn receive(
        &self,
        recipient: &NativeContentRecipient,
        content: &NativeContent,
    ) -> anyhow::Result<()> {
        self.check_active()?;
        if recipient.actor_id() != &self.actor || content.original().event.actor_id == self.actor {
            return Ok(());
        }
        let Some(message) = content.message() else {
            return Ok(());
        };
        let Some(block) = &message.content else {
            return Ok(());
        };
        if !collect_mention_subject_actor_ids(&block.to_value()?)?.contains(&self.actor) {
            return Ok(());
        }
        let input = message.plain_body()?;
        // Leave room for the fixed request envelope/instructions. An input
        // outside this adapter's text limit must not starve newer stream items.
        if input.trim().is_empty() || serde_json::to_vec(&input)?.len() > 32_768 {
            return Ok(());
        }
        let binding = serde_json::json!({
            "channel_id": self.channel_id,
            "instructions": ModelClient::native_reply_instructions(),
            "model": self.config.model,
            "provider_id": self.config.model_provider_id,
            "provider_base": self.config.model_provider.base_url,
            "provider_api": self.config.model_provider.wire_api,
            "provider_query": self.config.model_provider.query_params,
            "provider_headers": self.config.model_provider.http_headers,
        });
        let (ticket, state) = self
            .runtime
            .prepare_native_reply(recipient, content, &binding)
            .await?;
        let text = match state {
            NativeReplyPreparation::Published(_) | NativeReplyPreparation::UnknownSettled => {
                return Ok(());
            }
            NativeReplyPreparation::InFlight => anyhow::bail!("native Bot request still in flight"),
            NativeReplyPreparation::OutputRetained(text) => text,
            NativeReplyPreparation::Ready => {
                recipient.validate_current().await?;
                self.check_active()?;
                let defaults = savfox_core::default_client::native_reply_default_headers()?;
                let http = savfox_core::default_client::try_build_native_reply_reqwest_client_with_headers(defaults.clone())?;
                let transport = Arc::new(transport::BotTransport::new(
                    HttpClient::new(http.clone()),
                    defaults,
                    self.runtime.clone(),
                    ticket.clone(),
                    recipient.clone(),
                    self.active.clone(),
                ));
                let model = self
                    .config
                    .model
                    .clone()
                    .context("Realm Bot has no configured model")?;
                let auth = AuthManager::shared(
                    self.config.savfox_home.clone(),
                    true,
                    self.config.cli_auth_credentials_store_mode,
                );
                let models = ModelsManager::new(self.config.savfox_home.clone(), auth.clone());
                let info = models.get_model_info(&model, &self.config).await;
                let session = savfox_protocol::SessionId::new();
                let source = savfox_protocol::protocol::SessionSource::Unknown;
                let otel = savfox_otel::OtelManager::new(
                    session,
                    &model,
                    &info.slug,
                    None,
                    None,
                    None,
                    false,
                    "realm_bot_reply".into(),
                    source.clone(),
                );
                let client = ModelClient::new_with_http_transport(
                    self.config.clone(),
                    Some(auth),
                    info,
                    otel,
                    self.config.model_provider.clone(),
                    None,
                    self.config.model_reasoning_summary,
                    session,
                    source,
                    TransportManager::new(),
                    ReqwestTransport::new(http),
                );
                let mut session = client.new_session(None);
                let result = tokio::time::timeout(std::time::Duration::from_secs(240), async {
                    let mut events = session
                        .stream_native_single_text(
                            &input,
                            transport::BotTransportHandle(transport.clone()),
                        )
                        .await?;
                    let mut text = String::new();
                    let mut completed = false;
                    while let Some(event) = events.next().await {
                        match event? {
                            ResponseEvent::OutputTextDelta(delta) => {
                                anyhow::ensure!(
                                    text.len() + delta.len() <= 65_536,
                                    "Bot output exceeds limit"
                                );
                                text.push_str(&delta);
                            }
                            ResponseEvent::Completed { .. } => {
                                completed = true;
                                break;
                            }
                            _ => {}
                        }
                    }
                    anyhow::ensure!(
                        completed && !text.trim().is_empty(),
                        "Bot reply did not complete"
                    );
                    transport.retain_output(&text).await?;
                    Ok::<_, anyhow::Error>(text)
                })
                .await;
                // Also closes request bodies retained by the network backend.
                transport.close();
                result??
            }
        };
        // Output is already durable. A pending MLS transition or unavailable
        // Station must not hold the input ACK and thereby block that transition.
        if let Err(error) = self.publish_output(&ticket, recipient, content, text).await {
            tracing::warn!(%error, "native Bot output retained for publication retry");
        }
        Ok(())
    }

    async fn publish_output(
        &self,
        ticket: &NativeReplyTicket,
        recipient: &NativeContentRecipient,
        content: &NativeContent,
        text: String,
    ) -> anyhow::Result<()> {
        recipient.validate_current().await?;
        self.check_active()?;
        let message = content
            .message()
            .context("retained Bot source is not a message")?;
        let mut reply = MessageCreatePayload::with_content(
            message.strand_id.clone(),
            message.track_name.clone(),
            ContentBlock::text(text),
        );
        reply.reply_to_id =
            Some(arkret::MessageId::from_event_id(&content.original().event.event_id).to_string());
        let id = self.runtime.submit_message(&self.actor, recipient.scope(), reply, serde_json::json!({
            "channel_id": self.channel_id, "source_event_id": content.original().event.event_id,
            "source_commit_id": content.original().commit.commit_id,
        })).await?;
        self.runtime
            .finish_native_reply(ticket, &id.parse()?)
            .await?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl NativeContentSink for NativeRealmBotReplies {
    async fn flush(
        &self,
        device: &ManagedDevice,
        reader: &ManagedDeviceReader,
    ) -> arkret_bridge_runtime::Result<()> {
        if device.actor_id != self.actor {
            return Ok(());
        }
        for pending in self
            .runtime
            .pending_native_reply_outputs(device, reader)
            .await?
        {
            if pending
                .processing_binding
                .get("channel_id")
                .and_then(serde_json::Value::as_str)
                != Some(self.channel_id.as_str())
            {
                continue;
            }
            if let Err(error) = self
                .publish_output(
                    &pending.ticket,
                    &pending.recipient,
                    &pending.content,
                    pending.text,
                )
                .await
            {
                tracing::warn!(%error, "native Bot output remains in the durable publication queue");
            }
        }
        Ok(())
    }
    async fn deliver(
        &self,
        recipient: &NativeContentRecipient,
        content: &NativeContent,
    ) -> arkret_bridge_runtime::Result<()> {
        self.receive(recipient, content).await.map_err(|error| {
            arkret_bridge_runtime::BridgeError::Runtime(format!(
                "native Bot processing failed: {error:#}"
            ))
        })
    }
}

impl Drop for NativeRealmBotReplies {
    fn drop(&mut self) {
        self.retire();
    }
}
