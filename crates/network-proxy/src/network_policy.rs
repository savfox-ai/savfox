use std::future::Future;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::reasons::REASON_POLICY_DENIED;
use crate::runtime::{HostBlockDecision, HostBlockReason};
use crate::state::NetworkProxyState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkProtocol {
    Http,
    HttpsConnect,
    Socks5Tcp,
    Socks5Udp,
}

#[derive(Clone, Debug)]
pub struct NetworkPolicyRequest {
    pub protocol: NetworkProtocol,
    pub host: String,
    pub port: u16,
    pub client_addr: Option<String>,
    pub method: Option<String>,
    pub command: Option<String>,
    pub exec_policy_hint: Option<String>,
}

pub struct NetworkPolicyRequestArgs {
    pub protocol: NetworkProtocol,
    pub host: String,
    pub port: u16,
    pub client_addr: Option<String>,
    pub method: Option<String>,
    pub command: Option<String>,
    pub exec_policy_hint: Option<String>,
}

impl NetworkPolicyRequest {
    #[must_use]
    pub fn new(args: NetworkPolicyRequestArgs) -> Self {
        let NetworkPolicyRequestArgs {
            protocol,
            host,
            port,
            client_addr,
            method,
            command,
            exec_policy_hint,
        } = args;
        Self {
            protocol,
            host,
            port,
            client_addr,
            method,
            command,
            exec_policy_hint,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetworkDecision {
    Allow,
    Deny { reason: String },
}

impl NetworkDecision {
    pub fn deny(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        let reason = if reason.is_empty() {
            REASON_POLICY_DENIED.to_owned()
        } else {
            reason
        };
        Self::Deny { reason }
    }
}

/// Decide whether a network request should be allowed.
///
/// If `command` or `exec_policy_hint` is provided, callers can map exec-policy
/// approvals to network access (e.g., allow all requests for commands matching
/// approved prefixes like `curl *`).
#[async_trait]
pub trait NetworkPolicyDecider: Send + Sync + 'static {
    async fn decide(&self, req: NetworkPolicyRequest) -> NetworkDecision;
}

#[async_trait]
impl<D: NetworkPolicyDecider + ?Sized> NetworkPolicyDecider for Arc<D> {
    async fn decide(&self, req: NetworkPolicyRequest) -> NetworkDecision {
        (**self).decide(req).await
    }
}

#[async_trait]
impl<F, Fut> NetworkPolicyDecider for F
where
    F: Fn(NetworkPolicyRequest) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = NetworkDecision> + Send,
{
    async fn decide(&self, req: NetworkPolicyRequest) -> NetworkDecision {
        (self)(req).await
    }
}

pub(crate) async fn evaluate_host_policy(
    state: &NetworkProxyState,
    decider: Option<&Arc<dyn NetworkPolicyDecider>>,
    request: &NetworkPolicyRequest,
) -> Result<NetworkDecision> {
    match state.host_blocked(&request.host, request.port).await? {
        HostBlockDecision::Allowed => Ok(NetworkDecision::Allow),
        HostBlockDecision::Blocked(HostBlockReason::NotAllowed) => {
            if let Some(decider) = decider {
                Ok(decider.decide(request.clone()).await)
            } else {
                Ok(NetworkDecision::deny(HostBlockReason::NotAllowed.as_str()))
            }
        }
        HostBlockDecision::Blocked(reason) => Ok(NetworkDecision::deny(reason.as_str())),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::config::NetworkPolicy;
    use crate::reasons::{REASON_DENIED, REASON_NOT_ALLOWED_LOCAL};
    use crate::state::network_proxy_state_for_policy;

    #[tokio::test]
    async fn evaluate_host_policy_invokes_decider_for_not_allowed() {
        let state = network_proxy_state_for_policy(NetworkPolicy::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let decider: Arc<dyn NetworkPolicyDecider> = Arc::new({
            let calls = calls.clone();
            move |_req| {
                calls.fetch_add(1, Ordering::SeqCst);
                // The default policy denies all; the decider is consulted for not_allowed
                // requests and can override that decision.
                async { NetworkDecision::Allow }
            }
        });

        let request = NetworkPolicyRequest::new(NetworkPolicyRequestArgs {
            protocol: NetworkProtocol::Http,
            host: "example.com".to_owned(),
            port: 80,
            client_addr: None,
            method: Some("GET".to_owned()),
            command: None,
            exec_policy_hint: None,
        });

        let decision = evaluate_host_policy(&state, Some(&decider), &request)
            .await
            .unwrap();
        assert_eq!(decision, NetworkDecision::Allow);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn evaluate_host_policy_skips_decider_for_denied() {
        let state = network_proxy_state_for_policy(NetworkPolicy {
            allowed_domains: vec!["example.com".to_owned()],
            denied_domains: vec!["blocked.com".to_owned()],
            ..NetworkPolicy::default()
        });
        let calls = Arc::new(AtomicUsize::new(0));
        let decider: Arc<dyn NetworkPolicyDecider> = Arc::new({
            let calls = calls.clone();
            move |_req| {
                calls.fetch_add(1, Ordering::SeqCst);
                async { NetworkDecision::Allow }
            }
        });

        let request = NetworkPolicyRequest::new(NetworkPolicyRequestArgs {
            protocol: NetworkProtocol::Http,
            host: "blocked.com".to_owned(),
            port: 80,
            client_addr: None,
            method: Some("GET".to_owned()),
            command: None,
            exec_policy_hint: None,
        });

        let decision = evaluate_host_policy(&state, Some(&decider), &request)
            .await
            .unwrap();
        assert_eq!(
            decision,
            NetworkDecision::Deny {
                reason: REASON_DENIED.to_owned()
            }
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn evaluate_host_policy_skips_decider_for_not_allowed_local() {
        let state = network_proxy_state_for_policy(NetworkPolicy {
            allowed_domains: vec!["example.com".to_owned()],
            allow_local_binding: false,
            ..NetworkPolicy::default()
        });
        let calls = Arc::new(AtomicUsize::new(0));
        let decider: Arc<dyn NetworkPolicyDecider> = Arc::new({
            let calls = calls.clone();
            move |_req| {
                calls.fetch_add(1, Ordering::SeqCst);
                async { NetworkDecision::Allow }
            }
        });

        let request = NetworkPolicyRequest::new(NetworkPolicyRequestArgs {
            protocol: NetworkProtocol::Http,
            host: "127.0.0.1".to_owned(),
            port: 80,
            client_addr: None,
            method: Some("GET".to_owned()),
            command: None,
            exec_policy_hint: None,
        });

        let decision = evaluate_host_policy(&state, Some(&decider), &request)
            .await
            .unwrap();
        assert_eq!(
            decision,
            NetworkDecision::Deny {
                reason: REASON_NOT_ALLOWED_LOCAL.to_owned()
            }
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    // ── T2 follow-up: extended decider matrix coverage ───────────────

    #[tokio::test]
    async fn allowed_domain_skips_decider() {
        // Hosts in the allow list flow through with Allow without
        // consulting the decider — fast path for known-good destinations.
        let state = network_proxy_state_for_policy(NetworkPolicy {
            allowed_domains: vec!["example.com".to_owned()],
            ..NetworkPolicy::default()
        });
        let calls = Arc::new(AtomicUsize::new(0));
        let decider: Arc<dyn NetworkPolicyDecider> = Arc::new({
            let calls = calls.clone();
            move |_req| {
                calls.fetch_add(1, Ordering::SeqCst);
                async { NetworkDecision::Allow }
            }
        });

        let request = NetworkPolicyRequest::new(NetworkPolicyRequestArgs {
            protocol: NetworkProtocol::HttpsConnect,
            host: "example.com".to_owned(),
            port: 443,
            client_addr: None,
            method: None,
            command: None,
            exec_policy_hint: None,
        });

        let decision = evaluate_host_policy(&state, Some(&decider), &request)
            .await
            .unwrap();
        assert_eq!(decision, NetworkDecision::Allow);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "decider must NOT be called when host is on allow list"
        );
    }

    #[tokio::test]
    async fn no_decider_means_deny_for_not_allowed_hosts() {
        // When no decider is registered, hosts that are not on the allow
        // list must default-deny rather than fall through.
        let state = network_proxy_state_for_policy(NetworkPolicy {
            allowed_domains: vec!["example.com".to_owned()],
            ..NetworkPolicy::default()
        });

        let request = NetworkPolicyRequest::new(NetworkPolicyRequestArgs {
            protocol: NetworkProtocol::Http,
            host: "unknown.test".to_owned(),
            port: 80,
            client_addr: None,
            method: Some("GET".to_owned()),
            command: None,
            exec_policy_hint: None,
        });

        let decision = evaluate_host_policy(&state, None, &request).await.unwrap();
        assert!(matches!(decision, NetworkDecision::Deny { .. }));
    }

    #[tokio::test]
    async fn decider_decision_propagates_for_socks5() {
        // The same allow/deny path applies to SOCKS5 protocols, not just
        // HTTP. Verify the protocol field is forwarded into the request
        // the decider sees so deciders can branch on it.
        let state = network_proxy_state_for_policy(NetworkPolicy::default());
        let observed_protocol = Arc::new(std::sync::Mutex::new(None));
        let observed = observed_protocol.clone();
        let decider: Arc<dyn NetworkPolicyDecider> = Arc::new(move |req: NetworkPolicyRequest| {
            let observed = observed.clone();
            async move {
                *observed.lock().unwrap() = Some(req.protocol);
                NetworkDecision::deny("policy says no")
            }
        });

        let request = NetworkPolicyRequest::new(NetworkPolicyRequestArgs {
            protocol: NetworkProtocol::Socks5Tcp,
            host: "tcp.example.com".to_owned(),
            port: 22,
            client_addr: None,
            method: None,
            command: None,
            exec_policy_hint: None,
        });

        let decision = evaluate_host_policy(&state, Some(&decider), &request)
            .await
            .unwrap();
        assert_eq!(
            decision,
            NetworkDecision::Deny {
                reason: "policy says no".to_owned()
            }
        );
        assert_eq!(
            *observed_protocol.lock().unwrap(),
            Some(NetworkProtocol::Socks5Tcp)
        );
    }

    #[test]
    fn deny_with_empty_reason_uses_default_reason() {
        // Edge case: callers that pass an empty reason should not produce
        // an unhelpful "Deny { reason: \"\" }" response. The constructor
        // rewrites empty to REASON_POLICY_DENIED.
        let d = NetworkDecision::deny("");
        match d {
            NetworkDecision::Deny { reason } => {
                assert_eq!(reason, crate::reasons::REASON_POLICY_DENIED);
            }
            _ => panic!("expected Deny"),
        }
    }
}
