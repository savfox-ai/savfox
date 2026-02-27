use std::sync::{Arc, OnceLock};
use std::time::Duration;

use http::{HeaderMap, Method};
use savfox_client::{HttpTransport, RequestCompression, RequestTelemetry, StreamResponse};
use serde_json::Value;

use crate::auth::{AuthProvider, add_auth_headers};
use crate::common::ResponseStream;
use crate::error::ApiError;
use crate::provider::Provider;
use crate::telemetry::{SseTelemetry, run_with_request_telemetry};

pub(crate) struct StreamingClient<T: HttpTransport, A: AuthProvider> {
    transport: T,
    provider: Provider,
    auth: A,
    request_telemetry: Option<Arc<dyn RequestTelemetry>>,
    sse_telemetry: Option<Arc<dyn SseTelemetry>>,
}

type StreamSpawner = fn(
    StreamResponse,
    Duration,
    Option<Arc<dyn SseTelemetry>>,
    Option<Arc<OnceLock<String>>>,
) -> ResponseStream;

impl<T: HttpTransport, A: AuthProvider> StreamingClient<T, A> {
    pub(crate) fn new(transport: T, provider: Provider, auth: A) -> Self {
        Self {
            transport,
            provider,
            auth,
            request_telemetry: None,
            sse_telemetry: None,
        }
    }

    pub(crate) fn with_telemetry(
        mut self,
        request: Option<Arc<dyn RequestTelemetry>>,
        sse: Option<Arc<dyn SseTelemetry>>,
    ) -> Self {
        self.request_telemetry = request;
        self.sse_telemetry = sse;
        self
    }

    pub(crate) fn provider(&self) -> &Provider {
        &self.provider
    }

    pub(crate) fn has_bearer_auth(&self) -> bool {
        self.auth
            .bearer_token()
            .is_some_and(|token| !token.trim().is_empty())
    }

    pub(crate) fn has_account_id(&self) -> bool {
        self.auth
            .account_id()
            .is_some_and(|account_id| !account_id.trim().is_empty())
    }

    pub(crate) async fn stream(
        &self,
        path: &str,
        body: Value,
        extra_headers: HeaderMap,
        compression: RequestCompression,
        spawner: StreamSpawner,
        turn_state: Option<Arc<OnceLock<String>>>,
    ) -> Result<ResponseStream, ApiError> {
        let url = self.provider.url_for_path(path);
        let body_pretty = serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string());
        // eprintln!("\n========== STREAMING REQUEST DEBUG ==========");
        // eprintln!("Provider: {}", self.provider.name);
        // eprintln!("URL: {url}");
        // eprintln!("Has bearer token: {}", self.has_bearer_auth());
        // eprintln!("Request body:\n{body_pretty}");
        // eprintln!("==============================================\n");

        let builder = || {
            let mut req = self.provider.build_request(Method::POST, path);
            req.headers.extend(extra_headers.clone());
            req.headers.insert(
                http::header::ACCEPT,
                http::HeaderValue::from_static("text/event-stream"),
            );
            req.body = Some(body.clone());
            req.compression = compression;
            add_auth_headers(&self.auth, req)
        };

        let stream_response = run_with_request_telemetry(
            self.provider.retry.to_policy(),
            self.request_telemetry.clone(),
            builder,
            |req| self.transport.stream(req),
        )
        .await?;

        Ok(spawner(
            stream_response,
            self.provider.stream_idle_timeout,
            self.sse_telemetry.clone(),
            turn_state,
        ))
    }
}
