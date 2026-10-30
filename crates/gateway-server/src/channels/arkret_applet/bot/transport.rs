//! One durable provider handoff, with cancellation checked on retained IO.
use arkret_bridge_runtime::managed_device::{NativeReplyClaim, NativeReplyTicket};
use savfox_http_client::{
    HttpTransport, Request, RequestCompression, Response, StreamResponse, TransportError,
};

use super::*;

pub(super) struct BotTransport {
    http: HttpClient,
    defaults: reqwest::header::HeaderMap,
    runtime: Arc<ManagedDeviceRuntime>,
    ticket: NativeReplyTicket,
    recipient: NativeContentRecipient,
    active: Arc<AtomicBool>,
    open: Arc<AtomicBool>,
    used: AtomicBool,
    claim: tokio::sync::Mutex<Option<NativeReplyClaim>>,
}

impl BotTransport {
    pub(super) fn new(
        http: HttpClient,
        defaults: reqwest::header::HeaderMap,
        runtime: Arc<ManagedDeviceRuntime>,
        ticket: NativeReplyTicket,
        recipient: NativeContentRecipient,
        active: Arc<AtomicBool>,
    ) -> Self {
        Self {
            http,
            defaults,
            runtime,
            ticket,
            recipient,
            active,
            open: Arc::new(AtomicBool::new(true)),
            used: AtomicBool::new(false),
            claim: tokio::sync::Mutex::new(None),
        }
    }
    pub(super) fn close(&self) {
        self.open.store(false, Ordering::Release);
    }
    fn check(&self) -> Result<(), ()> {
        check(&self.recipient, &self.active, &self.open)
    }
    pub(super) async fn retain_output(&self, text: &str) -> anyhow::Result<()> {
        self.check().map_err(|()| denied())?;
        self.recipient.validate_current().await?;
        self.check().map_err(|()| denied())?;
        let claim = self.claim.lock().await;
        self.runtime
            .retain_native_reply_output(claim.as_ref().context("Bot provider claim missing")?, text)
            .await?;
        Ok(())
    }
}
impl Drop for BotTransport {
    fn drop(&mut self) {
        self.close();
    }
}

fn denied() -> TransportError {
    TransportError::Build("native Bot provider request denied".into())
}
fn network() -> TransportError {
    TransportError::Network("native Bot provider transport failed".into())
}
fn check(
    recipient: &NativeContentRecipient,
    active: &AtomicBool,
    open: &AtomicBool,
) -> Result<(), ()> {
    if !active.load(Ordering::Acquire) || !open.load(Ordering::Acquire) {
        return Err(());
    }
    recipient.check_incarnation().map_err(|_| ())
}
struct CloseOnDrop(Arc<AtomicBool>, bool);
impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        if self.1 {
            self.0.store(false, Ordering::Release);
        }
    }
}

#[async_trait::async_trait]
impl HttpTransport for BotTransport {
    async fn execute(&self, _: Request) -> Result<Response, TransportError> {
        Err(denied())
    }
    async fn stream(&self, request: Request) -> Result<StreamResponse, TransportError> {
        self.check().map_err(|()| denied())?;
        if self.used.swap(true, Ordering::AcqRel)
            || request.compression != RequestCompression::None
            || request.method != reqwest::Method::POST
            || request
                .headers
                .contains_key(reqwest::header::CONTENT_ENCODING)
        {
            return Err(denied());
        }
        let body = serde_json::to_vec(&request.body.ok_or_else(denied)?).map_err(|_| denied())?;
        if body.is_empty() || body.len() > 65_536 {
            return Err(denied());
        }
        let mut headers = self.defaults.clone();
        for name in request.headers.keys() {
            headers.remove(name);
            for value in request.headers.get_all(name) {
                headers.append(name, value.clone());
            }
        }
        let url = reqwest::Url::parse(&request.url).map_err(|_| denied())?;
        if !headers.contains_key(reqwest::header::HOST) {
            headers.insert(
                reqwest::header::HOST,
                reqwest::header::HeaderValue::from_str(
                    &url[url::Position::BeforeHost..url::Position::AfterPort],
                )
                .map_err(|_| denied())?,
            );
        }
        headers
            .entry(reqwest::header::ACCEPT_ENCODING)
            .or_insert(reqwest::header::HeaderValue::from_static("identity"));
        let mut builder = self
            .http
            .request(request.method, &request.url)
            .headers(headers)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::CONTENT_LENGTH, body.len())
            .body(body);
        if let Some(timeout) = request.timeout {
            builder = builder.timeout(timeout);
        }
        let mut built = builder.build_native().map_err(|_| denied())?;
        let bytes = built
            .body()
            .and_then(reqwest::Body::as_bytes)
            .ok_or_else(denied)?
            .to_vec();
        let mut headers: Vec<_> = built
            .headers()
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_bytes()))
            .collect();
        headers.sort_by(|a, b| a.0.cmp(b.0));
        let frozen = serde_json::json!({"method": built.method().as_str(), "url": built.url().as_str(), "headers":headers, "body": bytes, "timeout_ms":built.timeout().map(|t|t.as_millis()), "version":format!("{:?}",built.version())});
        self.recipient
            .validate_current()
            .await
            .map_err(|_| denied())?;
        self.check().map_err(|()| denied())?;
        let claim = self
            .runtime
            .claim_native_reply(&self.ticket, &frozen)
            .await
            .map_err(|_| denied())?
            .ok_or_else(denied)?;
        *self.claim.lock().await = Some(claim);
        self.check().map_err(|()| denied())?;
        let recipient = self.recipient.clone();
        let active = self.active.clone();
        let open = self.open.clone();
        let upload =
            futures_util::stream::unfold((bytes, 0usize, false), move |(bytes, at, done)| {
                let recipient = recipient.clone();
                let active = active.clone();
                let open = open.clone();
                async move {
                    if done || at == bytes.len() {
                        return None;
                    }
                    if check(&recipient, &active, &open).is_err() {
                        return Some((
                            Err(std::io::Error::other("Bot request retired")),
                            (bytes, at, true),
                        ));
                    }
                    let next = (at + 8192).min(bytes.len());
                    Some((Ok(bytes[at..next].to_vec()), (bytes, next, false)))
                }
            });
        *built.body_mut() = Some(reqwest::Body::wrap_stream(upload));
        let mut close = CloseOnDrop(self.open.clone(), true);
        let response = self
            .http
            .execute_native_request(built)
            .await
            .map_err(|_| network())?;
        self.check().map_err(|()| denied())?;
        let status = response.status();
        if !status.is_success() {
            return Err(TransportError::Http {
                status,
                url: None,
                headers: None,
                body: None,
            });
        }
        let headers = response.headers().clone();
        let recipient = self.recipient.clone();
        let active = self.active.clone();
        let open = self.open.clone();
        let bytes = futures_util::stream::unfold(
            (response.bytes_stream(), 0usize, false),
            move |(mut source, count, done)| {
                let recipient = recipient.clone();
                let active = active.clone();
                let open = open.clone();
                async move {
                    if done {
                        return None;
                    }
                    if check(&recipient, &active, &open).is_err() {
                        return Some((Err(denied()), (source, count, true)));
                    }
                    match source.next().await {
                        Some(Ok(chunk)) if count + chunk.len() <= 1_048_576 => {
                            if check(&recipient, &active, &open).is_err() {
                                return Some((Err(denied()), (source, count, true)));
                            }
                            let count = count + chunk.len();
                            Some((Ok(chunk), (source, count, false)))
                        }
                        Some(_) => Some((Err(network()), (source, count, true))),
                        None => None,
                    }
                }
            },
        );
        close.1 = false;
        Ok(StreamResponse {
            status,
            headers,
            bytes: Box::pin(bytes),
        })
    }
}

#[derive(Clone)]
pub(super) struct BotTransportHandle(pub Arc<BotTransport>);
#[async_trait::async_trait]
impl HttpTransport for BotTransportHandle {
    async fn execute(&self, request: Request) -> Result<Response, TransportError> {
        self.0.execute(request).await
    }
    async fn stream(&self, request: Request) -> Result<StreamResponse, TransportError> {
        self.0.stream(request).await
    }
}
