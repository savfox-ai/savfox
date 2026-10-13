//! Process-local fixed MessageCreate reply port. No serde, wire, or authority claims.
use arkret::{AccountId, CommittedEventFullView, Hash, ScopeRef, StrandId};
use async_trait::async_trait;
/// Original standard resource request signed by the live Agent session DPoP key.
/// Main verifies these exact credentials with its original grant_dpop(forcefresh).
/// This carrier is not a verified session and never accepts parsed JWT authority.
pub use savfox_channels::arkret::ArkretNativeAgentCredentials as NativeAgentCredentials;

/// Retained only after original Garth verified Commit delivery and original MLS
/// authenticated decryption. Main independently binds it to accepted source.
pub struct NativeReplySource {
    pub channel_id: String,
    pub configured_account_id: String,
    pub agent: AccountId,
    pub controller: AccountId,
    pub caller: AccountId,
    pub accepted: CommittedEventFullView,
    pub scope: ScopeRef,
    pub strand: StrandId,
    pub text: String,
    pub credentials: NativeAgentCredentials,
}

/// Complete built HTTP request, including fixed instructions, actual body and
/// authorization/trace headers. Secret material is never Debug or Serialize.
/// Only the trusted transport constructs this after RequestBuilder::build_native.
pub struct FrozenNativeProviderRequest {
    request: reqwest::Request,
    digest: Hash,
}
impl FrozenNativeProviderRequest {
    pub(crate) fn new(request: reqwest::Request, digest: Hash) -> Self {
        Self { request, digest }
    }
    pub fn request(&self) -> &reqwest::Request {
        &self.request
    }
    pub fn digest(&self) -> &Hash {
        &self.digest
    }
    pub(crate) fn into_request(self) -> reqwest::Request {
        self.request
    }
}

// async-trait adds must_use to futures already covered by that lint.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait NativePublicReplyHost: Send + Sync {
    /// Authentication/network refresh is completed BEFORE acquiring local cut.
    /// Native source and all fields are checked against exact accepted material.
    async fn acquire_reply_cut(
        &self,
        source: &NativeReplySource,
        request: &FrozenNativeProviderRequest,
    ) -> anyhow::Result<Box<dyn NativePublicReplyGuard>>;
}

/// The same original local delivery guard spans the actual HTTP execution and
/// response body. Methods must not perform network callbacks while holding it.
/// Remote issuer current is an authenticated fresh observation before this cut,
/// never advertised as a remote revocation lock or time lease.
// async-trait adds must_use to futures already covered by that lint.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait NativePublicReplyGuard: Send {
    fn permits_model_handoff(&mut self, exact_digest: &Hash) -> anyhow::Result<()>;
    fn permits_response_chunk(&mut self, exact_digest: &Hash) -> anyhow::Result<()>;
    async fn finish(self: Box<Self>) -> anyhow::Result<()>;
}

/// Drop cancels all native account tasks immediately and requests the original
/// private SessionManager's normal shutdown. `shutdown` also awaits that cleanup.
/// No generic session/backend is advertised as synchronously closed by Drop.
pub struct NativePublicReplyHandle {
    pub(crate) task: tokio::task::JoinHandle<()>,
    pub(crate) stop: Option<tokio::sync::oneshot::Sender<()>>,
    pub(crate) children: std::sync::Arc<std::sync::Mutex<(bool, Vec<tokio::task::AbortHandle>)>>,
}
impl NativePublicReplyHandle {
    fn cancel_native_children(&mut self) {
        {
            let mut children = self.children.lock().expect("native child ownership");
            children.0 = true;
            for task in &children.1 {
                task.abort();
            }
        }
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
    /// Await the original private-session cleanup as well as native cancellation.
    pub async fn shutdown(mut self) -> anyhow::Result<()> {
        self.cancel_native_children();
        (&mut self.task)
            .await
            .map_err(|_| anyhow::anyhow!("native reply shutdown failed"))
    }
}
impl Drop for NativePublicReplyHandle {
    fn drop(&mut self) {
        self.cancel_native_children();
    }
}

// Implemented inside original channels::arkret so private normal crypto,
// provider, accepted-stream and outbound helpers remain single-owner code.
// pub async fn start_native_arkret_public_replies(
//     home: PathBuf, host: Arc<dyn NativePublicReplyHost>
// ) -> anyhow::Result<NativePublicReplyHandle>;

#[path = "native_reply/factory.rs"]
pub(super) mod factory;
#[path = "native_reply/guarded_transport.rs"]
mod guarded_transport;
pub(crate) use factory::native_save_onboarding;
pub use factory::start_native_arkret_public_replies;
