//! Fixed reply transport, composed with the original ModelClient native entry.
//! Private candidate: not applied, compiled or run.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::StreamExt;
use savfox_http_client::{
    HttpClient, HttpTransport, Request, RequestCompression, Response, StreamResponse,
    TransportError,
};

use super::{
    FrozenNativeProviderRequest, NativePublicReplyGuard, NativePublicReplyHost, NativeReplySource,
};
use crate::arkret_delivery::{ArkretExecutionBindingStore, NativeReplyHandoff, NativeReplySlot};

const MAX_REQUEST: usize = 64 * 1024;
const MAX_RESPONSE: usize = 1024 * 1024;
struct Held {
    guard: Option<Box<dyn NativePublicReplyGuard>>,
    upload_complete: bool,
    upload_failed: bool,
}
impl Held {
    fn permit(&mut self, digest: &arkret::Hash, response: bool) -> Result<(), std::io::Error> {
        if self.upload_failed {
            return Err(std::io::Error::other("native reply request cancelled"));
        }
        let guard = self
            .guard
            .as_mut()
            .ok_or_else(|| std::io::Error::other("native reply cut released"))?;
        let allowed = if response {
            guard.permits_response_chunk(digest)
        } else {
            guard.permits_model_handoff(digest)
        };
        allowed.map_err(|_| std::io::Error::other("native reply current authority unavailable"))
    }
}

// A reqwest/hyper request body can outlive its caller. Closing the SAME held
// authority makes every retained backend body/response poll refuse; dropping
// the upload future is not treated as a promise that the backend disappeared.
fn close_held(held: &Arc<Mutex<Held>>) {
    let mut lease = held
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    lease.upload_failed = true;
    lease.guard.take();
}
struct CancelHeldOnDrop {
    held: Arc<Mutex<Held>>,
    armed: bool,
}
impl Drop for CancelHeldOnDrop {
    fn drop(&mut self) {
        if self.armed {
            close_held(&self.held);
        }
    }
}

pub(crate) struct NativeGuardedTransport {
    http: HttpClient,
    default_headers: reqwest::header::HeaderMap,
    source: Arc<NativeReplySource>,
    host: Arc<dyn NativePublicReplyHost>,
    ledger: Arc<ArkretExecutionBindingStore>,
    slot: NativeReplySlot,
    input_digest: arkret::Hash,
    used: AtomicBool,
    handoff: tokio::sync::Mutex<Option<NativeReplyHandoff>>,
    held: tokio::sync::Mutex<Option<(Arc<Mutex<Held>>, arkret::Hash)>>,
}
impl NativeGuardedTransport {
    pub(crate) fn new(
        http: HttpClient,
        default_headers: reqwest::header::HeaderMap,
        source: Arc<NativeReplySource>,
        host: Arc<dyn NativePublicReplyHost>,
        ledger: Arc<ArkretExecutionBindingStore>,
        slot: NativeReplySlot,
        input_digest: arkret::Hash,
    ) -> Self {
        Self {
            http,
            default_headers,
            source,
            host,
            ledger,
            slot,
            input_digest,
            used: AtomicBool::new(false),
            handoff: tokio::sync::Mutex::new(None),
            held: tokio::sync::Mutex::new(None),
        }
    }
    pub(crate) async fn validate_completed_output(&self) -> anyhow::Result<()> {
        let held_slot = self.held.lock().await;
        let (held, digest) = held_slot
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("native cut missing"))?;
        let mut lease = held
            .lock()
            .map_err(|_| anyhow::anyhow!("native cut poisoned"))?;
        anyhow::ensure!(
            lease.upload_complete && !lease.upload_failed,
            "native request upload incomplete"
        );
        lease.permit(digest, true)?;
        Ok(())
    }
    pub(crate) async fn finish_after_complete(&self) -> anyhow::Result<()> {
        let (held, digest) = self
            .held
            .lock()
            .await
            .take()
            .ok_or_else(|| anyhow::anyhow!("native cut missing"))?;
        let guard = {
            let mut lease = held
                .lock()
                .map_err(|_| anyhow::anyhow!("native cut poisoned"))?;
            anyhow::ensure!(
                lease.upload_complete && !lease.upload_failed,
                "native request upload incomplete"
            );
            lease.permit(&digest, true)?;
            lease
                .guard
                .take()
                .ok_or_else(|| anyhow::anyhow!("native cut released"))?
        };
        guard.finish().await
    }
    pub(crate) async fn take_handoff(&self) -> anyhow::Result<NativeReplyHandoff> {
        self.handoff
            .lock()
            .await
            .take()
            .ok_or_else(|| anyhow::anyhow!("native provider handoff missing"))
    }
}
impl Drop for NativeGuardedTransport {
    fn drop(&mut self) {
        if let Some((held, _)) = self.held.get_mut().take() {
            close_held(&held);
        }
        // Durable HandoffStarted remains Unknown, never Ready. The original
        // ledger keeps the immutable intent even if no response was received.
    }
}

fn denied() -> TransportError {
    TransportError::Build("native fixed reply denied".into())
}
fn network() -> TransportError {
    TransportError::Network("native fixed reply transport failed".into())
}

#[async_trait]
impl HttpTransport for NativeGuardedTransport {
    async fn execute(&self, _request: Request) -> Result<Response, TransportError> {
        Err(denied())
    }
    async fn stream(&self, request: Request) -> Result<StreamResponse, TransportError> {
        if self.used.swap(true, Ordering::AcqRel)
            || request.compression != RequestCompression::None
            || request.method != reqwest::Method::POST
        {
            return Err(denied());
        }
        let body = serde_json::to_vec(&request.body.ok_or_else(denied)?).map_err(|_| denied())?;
        if body.is_empty()
            || body.len() > MAX_REQUEST
            || request
                .headers
                .contains_key(reqwest::header::CONTENT_ENCODING)
        {
            return Err(denied());
        }
        let mut headers = self.default_headers.clone();
        // Same snapshot as the original strict client. Explicit application
        // headers take precedence; execute cannot add an unfrozen default.
        for name in request.headers.keys() {
            headers.remove(name);
            for value in request.headers.get_all(name).iter() {
                headers.append(name, value.clone());
            }
        }
        let url = reqwest::Url::parse(&request.url).map_err(|_| denied())?;
        if !headers.contains_key(reqwest::header::HOST) {
            let authority = &url[url::Position::BeforeHost..url::Position::AfterPort];
            headers.insert(
                reqwest::header::HOST,
                reqwest::header::HeaderValue::from_str(authority).map_err(|_| denied())?,
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
        // Original trace/header propagation completes here, before digest/gate.
        let built = builder.build_native().map_err(|_| denied())?;
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
        // Stable key sort retains original duplicate value ordering.
        headers.sort_by(|a, b| a.0.cmp(b.0));
        let digest = arkret::Hash::new(arkret::canonical::canonical_sha256(&serde_json::json!({
            "method": built.method().as_str(), "url": built.url().as_str(),
            "headers": headers, "body": bytes, "timeout_ms": built.timeout().map(|t| t.as_millis()), "version": format!("{:?}",built.version())
        })).map_err(|_| denied())?).map_err(|_| denied())?;
        let frozen = FrozenNativeProviderRequest::new(built, digest.clone());
        let guard = self
            .host
            .acquire_reply_cut(&self.source, &frozen)
            .await
            .map_err(|_| denied())?;
        let token = self
            .ledger
            .native_claim_handoff(
                self.slot.clone(),
                self.input_digest.clone(),
                frozen.request().url().as_str().to_owned(),
                digest.clone(),
            )
            .await
            .map_err(|_| denied())?;
        *self.handoff.lock().await = Some(token);
        let held = Arc::new(Mutex::new(Held {
            guard: Some(guard),
            upload_complete: false,
            upload_failed: false,
        }));
        // fsync/CAS awaited: fresh local clock/current check before any bytes.
        held.lock()
            .map_err(|_| denied())?
            .permit(&digest, false)
            .map_err(|_| denied())?;
        *self.held.lock().await = Some((Arc::clone(&held), digest.clone()));
        let upload_held = Arc::clone(&held);
        let upload_digest = digest.clone();
        let upload = futures_util::stream::unfold((bytes, 0usize), move |(bytes, at)| {
            let held = Arc::clone(&upload_held);
            let digest = upload_digest.clone();
            async move {
                let mut lease = match held.lock() {
                    Ok(value) => value,
                    Err(_) => {
                        return Some((
                            Err(std::io::Error::other("native reply cut poisoned")),
                            (bytes.clone(), bytes.len()),
                        ));
                    }
                };
                if lease.upload_failed || at == bytes.len() {
                    return None;
                }
                if let Err(error) = lease.permit(&digest, false) {
                    lease.upload_failed = true;
                    return Some((Err(error), (bytes.clone(), bytes.len())));
                }
                let next = (at + 8192).min(bytes.len());
                // Content-Length may finish without another EOF poll. This
                // marks ALL exact bytes yielded, not remote acceptance.
                if next == bytes.len() {
                    lease.upload_complete = true;
                }
                Some((Ok(bytes[at..next].to_vec()), (bytes, next)))
            }
        });
        let mut built = frozen.into_request();
        *built.body_mut() = Some(reqwest::Body::wrap_stream(upload));
        // Only wrapping occurs after freeze: exact bytes, headers and URL stay fixed.
        // No retries/redirects in the strictly built client. Drop/cancel leaves durable
        // StartedUnknown.
        let mut call_cancel = CancelHeldOnDrop {
            held: Arc::clone(&held),
            armed: true,
        };
        let response = self
            .http
            .execute_native_request(built)
            .await
            .map_err(|_| network())?;
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
        let bytes = futures_util::stream::unfold(
            (response.bytes_stream(), held, digest, 0usize, false),
            |(mut bytes, held, digest, count, done)| async move {
                if done {
                    return None;
                }
                if held
                    .lock()
                    .map_err(|_| ())
                    .and_then(|mut guard| guard.permit(&digest, true).map_err(|_| ()))
                    .is_err()
                {
                    return Some((Err(denied()), (bytes, held, digest, count, true)));
                }
                match bytes.next().await {
                    Some(Ok(chunk)) if count + chunk.len() <= MAX_RESPONSE => {
                        // Current/clock again after network await, immediately before exposing
                        // chunk to parser.
                        if held
                            .lock()
                            .map_err(|_| ())
                            .and_then(|mut guard| guard.permit(&digest, true).map_err(|_| ()))
                            .is_err()
                        {
                            return Some((Err(denied()), (bytes, held, digest, count, true)));
                        }
                        let count = count + chunk.len();
                        Some((Ok(chunk), (bytes, held, digest, count, false)))
                    }
                    Some(_) => Some((Err(network()), (bytes, held, digest, count, true))),
                    // EOF alone does not release authority or prove model completion.
                    // Consumer calls finish_after_complete only after strict Completed.
                    None => None,
                }
            },
        );
        // Transfer success to the original transport lifetime/response stream.
        // Every earlier return or cancellation closes authority immediately.
        call_cancel.armed = false;
        Ok(StreamResponse {
            status,
            headers,
            bytes: Box::pin(bytes),
        })
    }
}

#[derive(Clone)]
pub(crate) struct NativeTransportHandle(pub Arc<NativeGuardedTransport>);
#[async_trait]
impl HttpTransport for NativeTransportHandle {
    async fn execute(&self, request: Request) -> Result<Response, TransportError> {
        self.0.execute(request).await
    }
    async fn stream(&self, request: Request) -> Result<StreamResponse, TransportError> {
        self.0.stream(request).await
    }
}

#[cfg(test)]
mod native_cancel_tests {
    use super::*;
    struct Guard(std::sync::Arc<AtomicBool>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    #[async_trait]
    impl NativePublicReplyGuard for Guard {
        fn permits_model_handoff(&mut self, _: &arkret::Hash) -> anyhow::Result<()> {
            Ok(())
        }
        fn permits_response_chunk(&mut self, _: &arkret::Hash) -> anyhow::Result<()> {
            Ok(())
        }
        async fn finish(self: Box<Self>) -> anyhow::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn native_upload_cancel_releases_cut_while_backend_keeps_body_arc() {
        let released = Arc::new(AtomicBool::new(false));
        let held = Arc::new(Mutex::new(Held {
            guard: Some(Box::new(Guard(Arc::clone(&released)))),
            upload_complete: false,
            upload_failed: false,
        }));
        let digest =
            arkret::Hash::new(arkret::canonical::sha256_digest(b"exact native request")).unwrap();
        let backend_body = Arc::clone(&held);
        let backend_response = Arc::clone(&held);
        held.lock().unwrap().permit(&digest, false).unwrap();
        let call = CancelHeldOnDrop { held, armed: true };
        drop(call);
        assert!(released.load(Ordering::Acquire));
        assert!(backend_body.lock().unwrap().upload_failed);
        assert!(backend_body.lock().unwrap().permit(&digest, false).is_err());
        assert!(
            backend_response
                .lock()
                .unwrap()
                .permit(&digest, true)
                .is_err()
        );
        assert!(backend_body.lock().unwrap().guard.is_none());
    }
}
