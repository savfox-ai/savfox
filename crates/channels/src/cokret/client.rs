//! Thin wrapper around [`cokret_http_client::Client`] for Cokret agent traffic.
//!
//! All HTTP / retry / canonical-bytes / NDJSON line splitting logic lives in
//! the upstream SDK; this type only exists so the gateway runtime never has
//! to think about constructing the underlying client.
//!
//! Personal-agent mode uses `agent_key_proof` to mint a short-lived
//! `ck.session.grant`, then presents that grant with a fresh DPoP proof on
//! every protected self-surface call.

use std::pin::Pin;
use std::sync::Arc;

use anyhow::Context;
use chrono::Utc;
use cokret::Ed25519MoveSigner;
use cokret_core::{
    Event, EventsSubmitOutcome, EventsSubscribeFrame, ServerDescription,
    SessionGrantDpopBindingProof,
};
use cokret_http_client::{Auth, Client, ClientBuilder, DpopAuth};
use cokret_identifiers::{DeviceId, Did};
use ed25519_dalek::{Signer as _, SigningKey};
use futures_util::{Stream, StreamExt};
use serde_json::json;
use url::Url;
use uuid::Uuid;

use super::session::{CokretSession, login_with_signer};
use super::signer::{CokretKeyRef, load_ed25519_signing_key};

const SESSION_GRANT_PATH: &str = "/_cokret/gate/account/session-grants";

#[derive(Clone)]
#[allow(missing_debug_implementations)]
pub struct CokretHttpClient {
    inner: Client,
}

/// Stream of [`EventsSubscribeFrame`] yielded by
/// [`CokretHttpClient::events_subscribe_stream`].
///
/// Each item is a fully-parsed frame (transient line decode errors come
/// through as `Err` but the stream continues). Use
/// [`futures_util::StreamExt::next`] to pull frames.
pub type CokretFrameStream =
    Pin<Box<dyn Stream<Item = Result<EventsSubscribeFrame, anyhow::Error>> + Send>>;

impl CokretHttpClient {
    #[must_use]
    pub fn inner(&self) -> &Client {
        &self.inner
    }

    /// Build an applet HTTP client bound to `base_url`, authenticated via the
    /// applet bearer token. Personal-agent account runtimes use
    /// [`Self::login_agent`] instead.
    pub fn new(base_url: &str, access_token: &str) -> anyhow::Result<Self> {
        let url =
            Url::parse(base_url).with_context(|| format!("invalid Cokret base_url: {base_url}"))?;
        let inner = ClientBuilder::new(url)
            .auth(Auth::Bearer(access_token.to_owned()))
            .build()
            .map_err(|err| anyhow::anyhow!("failed to build Cokret HTTP client: {err}"))?;
        Ok(Self { inner })
    }

    /// Construct a personal-agent HTTP client by exchanging a runtime-key
    /// `agent_key_proof` for a short-lived DPoP-bound session grant.
    #[allow(clippy::too_many_arguments)]
    pub async fn login_agent(
        base_url: &str,
        key_ref: &CokretKeyRef,
        principal_did: Did,
        verification_method: &str,
        agent_key_authorization_ref: &str,
        requested_scope: Vec<String>,
        audience: &str,
        realm_id: Option<&str>,
    ) -> anyhow::Result<(Self, CokretSession)> {
        let url =
            Url::parse(base_url).with_context(|| format!("invalid Cokret base_url: {base_url}"))?;
        let signing_key = Arc::new(load_ed25519_signing_key(key_ref)?);
        let grant_htu = joined_htu(&url, SESSION_GRANT_PATH)?;
        let binding_proof = build_dpop_header(&signing_key, "POST", grant_htu.clone(), None)?;
        let bootstrap = ClientBuilder::new(url.clone())
            .auth(Auth::Dpop(DpopAuth::proof_only({
                let expected_htu = grant_htu.clone();
                let proof_jwt = binding_proof.clone();
                move |request| {
                    if request.method != "POST"
                        || request.htu != expected_htu
                        || request.access_token.is_some()
                    {
                        return Err(cokret_core::Error::Protocol(
                            "unexpected DPoP kickoff request shape".to_owned(),
                        ));
                    }
                    Ok(proof_jwt.clone())
                }
            })))
            .build()
            .map_err(|err| anyhow::anyhow!("agent session bootstrap HTTP client: {err}"))?;

        let expires_at = Utc::now() + chrono::Duration::minutes(5);
        let challenge = format!("savfox-agent-session-{}", Uuid::now_v7());
        let nonce = Uuid::now_v7().to_string();
        let agent_scope_request = if let Some(realm_id) = realm_id {
            json!({ "realm_ids": [realm_id] })
        } else {
            json!({})
        };
        let dpop_binding_proof = SessionGrantDpopBindingProof {
            proof_jwt: binding_proof,
        };
        let signing_input = cokret::agent::agent_key_proof_signing_input_for_session_grant(
            &principal_did,
            &requested_scope,
            agent_key_authorization_ref,
            &agent_scope_request,
            &dpop_binding_proof,
            verification_method.to_owned(),
            challenge.clone(),
            nonce.clone(),
            audience.to_owned(),
            expires_at,
        )
        .map_err(|err| anyhow::anyhow!("agent_key_proof signing input: {err}"))?;
        let signature = signing_key.sign(
            &signing_input
                .canonical_bytes()
                .map_err(|err| anyhow::anyhow!("agent_key_proof canonical bytes: {err}"))?,
        );
        let signature = cokret_core::base64url_encode(signature.to_bytes());
        let request = cokret::agent::agent_key_proof_session_grant_request(
            principal_did.clone(),
            requested_scope,
            agent_key_authorization_ref,
            agent_scope_request,
            dpop_binding_proof,
            verification_method.to_owned(),
            challenge,
            nonce,
            audience.to_owned(),
            expires_at,
            signature,
        )
        .map_err(|err| anyhow::anyhow!("agent_key_proof session request: {err}"))?;
        let outcome = bootstrap
            .auth_issue_session_grant(&request)
            .await
            .map_err(|err| anyhow::anyhow!("agent_key_proof session grant exchange: {err}"))?;

        let session_grant = outcome.session_grant.clone();
        let inner = ClientBuilder::new(url)
            .auth(Auth::Dpop(DpopAuth::with_access_token(
                session_grant.clone(),
                {
                    let signing_key = Arc::clone(&signing_key);
                    move |request| {
                        let cokret_http_client::DpopProofRequest {
                            method,
                            htu,
                            access_token,
                        } = request;
                        build_dpop_header(&signing_key, method, htu, access_token.as_deref())
                    }
                },
            )))
            .build()
            .map_err(|err| anyhow::anyhow!("DPoP-bound Cokret HTTP client: {err}"))?;
        Ok((
            Self { inner },
            CokretSession {
                session_grant,
                expires_at: outcome.expires_at,
                principal_did: outcome.principal_id,
                device_id: outcome.device_id,
            },
        ))
    }

    /// Construct an applet HTTP client by running DID-proof login.
    ///
    /// Builds an unauthenticated underlying `Client`, runs the applet
    /// DID-proof grant exchange, then rebuilds the authenticated `Client`
    /// carrying the `Authorization: Bearer <grant>` header. This is not the
    /// personal-agent runtime path.
    pub async fn login(
        base_url: &str,
        signer: &Ed25519MoveSigner,
        principal_did: Did,
        device_id: DeviceId,
        challenge: &str,
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
            challenge,
            audience,
        )
        .await?;
        let inner = ClientBuilder::new(url)
            .auth(Auth::Bearer(session.session_grant.clone()))
            .build()
            .map_err(|err| anyhow::anyhow!("authenticated HTTP client: {err}"))?;
        Ok((Self { inner }, session))
    }

    /// `GET /api/v1/server/describe` — used at startup to verify the target
    /// server and pin the service DID.
    pub async fn server_describe(&self) -> anyhow::Result<ServerDescription> {
        self.inner
            .describe()
            .await
            .map_err(|err| anyhow::anyhow!(err.to_string()))
    }

    /// `GET /_cokret/self/events/subscribe` — returns a
    /// [`CokretFrameStream`] that yields fully-parsed `EventsSubscribeFrame`
    /// items.
    pub async fn events_subscribe_stream(
        &self,
        realm_id: &str,
        after: Option<&str>,
    ) -> anyhow::Result<CokretFrameStream> {
        let response = self
            .inner
            .events_subscribe_stream(realm_id, after)
            .await
            .map_err(|err| anyhow::anyhow!("cokret events_subscribe_stream: {err}"))?;
        Ok(Box::pin(ndjson_event_frame_stream(response)))
    }

    /// `POST /api/v1/events` — submit one signed Event Envelope.
    pub async fn submit_event(&self, event: &Event) -> anyhow::Result<EventsSubmitOutcome> {
        self.inner
            .events_submit(event)
            .await
            .map_err(|err| anyhow::anyhow!(err.to_string()))
    }
}

fn build_dpop_header(
    signing_key: &SigningKey,
    method: impl Into<String>,
    htu: impl Into<String>,
    access_token: Option<&str>,
) -> cokret_core::Result<String> {
    let mut request = cokret::dpop::DpopProofRequest::new(method, htu);
    if let Some(access_token) = access_token {
        request = request.access_token(access_token.to_owned());
    }
    cokret::dpop::build_dpop_proof(&request, signing_key).map(|proof| proof.header_value)
}

fn joined_htu(base_url: &Url, path: &str) -> anyhow::Result<String> {
    let mut url = base_url
        .join(path.trim_start_matches('/'))
        .with_context(|| format!("invalid Cokret endpoint path: {path}"))?;
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string())
}

fn ndjson_event_frame_stream(
    response: reqwest::Response,
) -> impl Stream<Item = Result<EventsSubscribeFrame, anyhow::Error>> + Send {
    let byte_stream = response
        .bytes_stream()
        .map(|item| item.map_err(|err| anyhow::anyhow!("stream bytes: {err}")))
        .boxed();
    futures_util::stream::unfold(
        (byte_stream, Vec::<u8>::new()),
        |(mut byte_stream, mut buffer)| async move {
            loop {
                if let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') {
                    let line_bytes: Vec<u8> = buffer.drain(..=newline).collect();
                    let line = String::from_utf8_lossy(&line_bytes);
                    match EventsSubscribeFrame::from_ndjson_line(&line) {
                        Ok(Some(frame)) => return Some((Ok(frame), (byte_stream, buffer))),
                        Ok(None) => continue,
                        Err(err) => {
                            return Some((
                                Err(anyhow::anyhow!("events subscribe frame: {err}")),
                                (byte_stream, buffer),
                            ));
                        }
                    }
                }
                match byte_stream.next().await {
                    Some(Ok(bytes)) => buffer.extend_from_slice(&bytes),
                    Some(Err(err)) => return Some((Err(err), (byte_stream, buffer))),
                    None if buffer.is_empty() => return None,
                    None => {
                        let line = String::from_utf8_lossy(&buffer);
                        let result = EventsSubscribeFrame::from_ndjson_line(&line)
                            .map_err(|err| anyhow::anyhow!("events subscribe frame: {err}"))
                            .and_then(|frame| {
                                frame.ok_or_else(|| {
                                    anyhow::anyhow!(
                                        "events subscribe stream ended with empty frame"
                                    )
                                })
                            });
                        buffer.clear();
                        return Some((result, (byte_stream, buffer)));
                    }
                }
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    fn signing_key() -> SigningKey {
        SigningKey::from_bytes(&[7_u8; 32])
    }

    #[test]
    fn dpop_header_binds_access_token_ath() {
        let header = build_dpop_header(
            &signing_key(),
            "GET",
            "https://cokret.example/_cokret/self/events",
            Some("session-grant-token"),
        )
        .expect("dpop header");
        let parts: Vec<&str> = header.split('.').collect();
        assert_eq!(parts.len(), 3);

        let protected: Value =
            serde_json::from_slice(&cokret_core::base64url_decode(parts[0]).unwrap()).unwrap();
        let payload: Value =
            serde_json::from_slice(&cokret_core::base64url_decode(parts[1]).unwrap()).unwrap();

        assert_eq!(protected["typ"], "dpop+jwt");
        assert_eq!(protected["alg"], "EdDSA");
        assert_eq!(payload["htm"], "GET");
        assert_eq!(payload["htu"], "https://cokret.example/_cokret/self/events");
        assert_eq!(
            payload["ath"],
            cokret::dpop::dpop_access_token_hash("session-grant-token")
        );
        assert_ne!(
            payload["ath"],
            cokret::dpop::dpop_access_token_hash("other-token")
        );
    }
}
