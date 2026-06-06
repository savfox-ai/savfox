//! Thin wrapper around [`cokret_http_client::Client`] that pre-attaches a
//! bearer `ck.session.grant` access token for one Cokret account.
//!
//! All HTTP / retry / canonical-bytes / NDJSON line splitting logic lives in
//! the upstream SDK; this type only exists so the gateway runtime never has
//! to think about constructing the underlying client.
//!
//! ### Phase 7 update
//!
//! Phase 6 had a local `CokretFrameStream` that did NDJSON line splitting
//! with `tokio::io::AsyncBufReadExt::lines()` and `serde_json::from_str`.
//! That code is now obsolete — SDK commit `bf29056` ships
//! `Client::account_subscribe_frames(&SyncRequestBody) -> Stream<AccountSubscribeFrame>`
//! (S-6) plus `AccountSubscribeFrame::from_ndjson_line` (S-3). We delegate
//! directly. `CokretFrameStream` is retained as a thin newtype alias for
//! API stability of downstream callers.

use std::pin::Pin;

use anyhow::Context;
use cokret::Ed25519MoveSigner;
use cokret_core::{
    AccountSubscribeFrame, Event, EventsSubmitOutcome, ServerDescription, SyncRequestBody,
};
use cokret_http_client::{Auth, Client, ClientBuilder};
use cokret_identifiers::{DeviceId, Did};
use futures_util::Stream;
use url::Url;

use super::session::{CokretSession, login_with_signer};

#[derive(Clone)]
#[allow(missing_debug_implementations)]
pub struct CokretHttpClient {
    inner: Client,
}

/// Stream of [`AccountSubscribeFrame`] yielded by
/// [`CokretHttpClient::account_subscribe_stream`].
///
/// Each item is a fully-parsed frame (transient line decode errors come
/// through as `Err` but the stream continues). Use
/// [`futures_util::StreamExt::next`] to pull frames.
pub type CokretFrameStream =
    Pin<Box<dyn Stream<Item = Result<AccountSubscribeFrame, anyhow::Error>> + Send>>;

impl CokretHttpClient {
    /// Build a new HTTP client bound to `base_url`, authenticated via the
    /// given bearer access token (typically a `ck.session.grant`).
    pub fn new(base_url: &str, access_token: &str) -> anyhow::Result<Self> {
        let url =
            Url::parse(base_url).with_context(|| format!("invalid Cokret base_url: {base_url}"))?;
        let inner = ClientBuilder::new(url)
            .auth(Auth::Bearer(access_token.to_owned()))
            .build()
            .map_err(|err| anyhow::anyhow!("failed to build Cokret HTTP client: {err}"))?;
        Ok(Self { inner })
    }

    #[must_use]
    pub fn inner(&self) -> &Client {
        &self.inner
    }

    /// Phase 8 (T8.B): construct a client by running DID-proof login.
    ///
    /// Builds an unauthenticated underlying `Client`, runs
    /// `AuthManager::login_did_proof` (S-2) to obtain a session grant,
    /// then rebuilds the authenticated `Client` carrying the
    /// `Authorization: Bearer <grant>` header. Returns both the wrapped
    /// client and the [`CokretSession`] (so callers can track expiry).
    pub async fn login(
        base_url: &str,
        signer: &Ed25519MoveSigner,
        principal_did: Did,
        device_id: DeviceId,
        verification_method: &str,
        audience: &str,
    ) -> anyhow::Result<(Self, CokretSession)> {
        let url =
            Url::parse(base_url).with_context(|| format!("invalid Cokret base_url: {base_url}"))?;
        let bootstrap = ClientBuilder::new(url.clone())
            .build()
            .map_err(|err| anyhow::anyhow!("bootstrap HTTP client: {err}"))?;
        let session = login_with_signer(
            &bootstrap,
            signer,
            principal_did,
            device_id,
            verification_method,
            audience,
        )
        .await?;
        let inner = ClientBuilder::new(url)
            .auth(Auth::Bearer(session.access_token.clone()))
            .build()
            .map_err(|err| anyhow::anyhow!("authenticated HTTP client: {err}"))?;
        Ok((Self { inner }, session))
    }

    /// Re-bind the bearer token in-place (after a re-login).
    pub fn refresh_bearer(&mut self, base_url: &str, access_token: &str) -> anyhow::Result<()> {
        let url =
            Url::parse(base_url).with_context(|| format!("invalid Cokret base_url: {base_url}"))?;
        self.inner = ClientBuilder::new(url)
            .auth(Auth::Bearer(access_token.to_owned()))
            .build()
            .map_err(|err| anyhow::anyhow!("re-authenticated HTTP client: {err}"))?;
        Ok(())
    }

    /// `GET /api/v1/server/describe` — used at startup to verify the target
    /// server and pin the service DID.
    pub async fn server_describe(&self) -> anyhow::Result<ServerDescription> {
        self.inner
            .describe()
            .await
            .map_err(|err| anyhow::anyhow!(err.to_string()))
    }

    /// `GET /api/v1/account/subscribe` — returns a [`CokretFrameStream`]
    /// that yields fully-parsed `AccountSubscribeFrame` items. Delegates
    /// to SDK `Client::account_subscribe_frames` (S-6).
    pub async fn account_subscribe_stream(
        &self,
        after: Option<&str>,
        catchup: bool,
    ) -> anyhow::Result<CokretFrameStream> {
        use futures_util::StreamExt;

        let req = SyncRequestBody {
            after: after.map(str::to_owned),
            catchup: Some(catchup),
            filter: None,
            set_presence: None,
            subscriptions: None,
            wait_for: None,
        };
        let stream = self
            .inner
            .account_subscribe_frames(&req)
            .await
            .map_err(|err| anyhow::anyhow!("cokret account_subscribe_frames: {err}"))?;
        let mapped = stream.map(|item| item.map_err(|err| anyhow::anyhow!("frame: {err}")));
        Ok(Box::pin(mapped))
    }

    /// `POST /api/v1/events` — submit one signed Event Envelope.
    pub async fn submit_event(&self, event: &Event) -> anyhow::Result<EventsSubmitOutcome> {
        self.inner
            .events_submit(event)
            .await
            .map_err(|err| anyhow::anyhow!(err.to_string()))
    }
}
