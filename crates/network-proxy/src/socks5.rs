use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use rama_core::error::{BoxError, ErrorExt as _};
use rama_core::extensions::ExtensionsRef;
use rama_core::layer::AddInputExtensionLayer;
use rama_core::rt::Executor;
use rama_core::service::service_fn;
use rama_core::{Layer, Service};
use rama_net::client::{ConnectRequest, EstablishedClientConnection};
use rama_net::stream::SocketInfo;
use rama_socks5::Socks5Acceptor;
use rama_socks5::server::udp::{RelayRequest, RelayResponse};
use rama_socks5::server::{DefaultConnector, DefaultUdpRelay};
use rama_tcp::TcpStream;
use rama_tcp::server::TcpListener;
use tracing::{error, info, warn};

use crate::config::NetworkMode;
use crate::connector::PolicyConnector;
use crate::network_policy::{
    NetworkDecision, NetworkPolicyDecider, NetworkPolicyRequest, NetworkPolicyRequestArgs,
    NetworkProtocol, evaluate_host_policy, evaluate_host_policy_with_target,
};
use crate::policy::normalize_host;
use crate::reasons::{REASON_METHOD_NOT_ALLOWED, REASON_PROXY_DISABLED};
use crate::state::{BlockedRequest, BlockedRequestArgs, NetworkProxyState};

pub async fn run_socks5(
    state: Arc<NetworkProxyState>,
    addr: SocketAddr,
    policy_decider: Option<Arc<dyn NetworkPolicyDecider>>,
    enable_socks5_udp: bool,
) -> Result<()> {
    let listener = TcpListener::build(Executor::new())
        .bind_address(addr)
        .await
        // See `http_proxy.rs` for details on why we wrap `BoxError` before converting to anyhow.
        .map_err(|err| err.into_opaque_error())
        .map_err(anyhow::Error::from)
        .with_context(|| format!("bind SOCKS5 proxy: {addr}"))?;

    info!("SOCKS5 proxy listening on {addr}");

    match state.network_mode().await {
        Ok(NetworkMode::Limited) => {
            info!("SOCKS5 is blocked in limited mode; set mode=\"full\" to allow SOCKS5");
        }
        Ok(NetworkMode::Full) => {}
        Err(err) => {
            warn!("failed to read network mode: {err}");
        }
    }

    let tcp_connector = PolicyConnector;
    let policy_tcp_connector = service_fn({
        let policy_decider = policy_decider.clone();
        move |req: ConnectRequest| {
            let tcp_connector = tcp_connector.clone();
            let policy_decider = policy_decider.clone();
            async move { handle_socks5_tcp(req, tcp_connector, policy_decider).await }
        }
    });

    let socks_connector = DefaultConnector::default().with_connector(policy_tcp_connector);
    let base = Socks5Acceptor::new(Executor::new()).with_connector(socks_connector);

    if enable_socks5_udp {
        let udp_state = state.clone();
        let udp_decider = policy_decider.clone();
        let udp_relay = DefaultUdpRelay::default().with_async_inspector(service_fn({
            move |request: RelayRequest| {
                let udp_state = udp_state.clone();
                let udp_decider = udp_decider.clone();
                async move { inspect_socks5_udp(request, udp_state, udp_decider).await }
            }
        }));
        let socks_acceptor = base.with_udp_associator(udp_relay);
        listener
            .serve(AddInputExtensionLayer::new_arc(state).into_layer(socks_acceptor))
            .await;
    } else {
        listener
            .serve(AddInputExtensionLayer::new_arc(state).into_layer(base))
            .await;
    }
    Ok(())
}

async fn handle_socks5_tcp(
    req: ConnectRequest,
    tcp_connector: PolicyConnector,
    policy_decider: Option<Arc<dyn NetworkPolicyDecider>>,
) -> Result<EstablishedClientConnection<TcpStream, ConnectRequest>, BoxError> {
    let app_state = req
        .extensions()
        .get_arc::<NetworkProxyState>()
        .ok_or_else(|| io::Error::other("missing state"))?;

    let host = normalize_host(&req.authority.host.to_string());
    let port = req.authority.port;
    if host.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid host").into());
    }

    let client = req
        .extensions()
        .get_ref::<SocketInfo>()
        .map(|info| info.peer_addr().to_string());

    match app_state.enabled().await {
        Ok(true) => {}
        Ok(false) => {
            let _ = app_state
                .record_blocked(BlockedRequest::new(BlockedRequestArgs {
                    host: host.clone(),
                    reason: REASON_PROXY_DISABLED.to_owned(),
                    client: client.clone(),
                    method: None,
                    mode: None,
                    protocol: "socks5".to_owned(),
                }))
                .await;
            let client = client.as_deref().unwrap_or_default();
            warn!("SOCKS blocked; proxy disabled (client={client}, host={host})");
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "proxy disabled").into());
        }
        Err(err) => {
            error!("failed to read enabled state: {err}");
            return Err(io::Error::other("proxy error").into());
        }
    }

    match app_state.network_mode().await {
        Ok(NetworkMode::Limited) => {
            let _ = app_state
                .record_blocked(BlockedRequest::new(BlockedRequestArgs {
                    host: host.clone(),
                    reason: REASON_METHOD_NOT_ALLOWED.to_owned(),
                    client: client.clone(),
                    method: None,
                    mode: Some(NetworkMode::Limited),
                    protocol: "socks5".to_owned(),
                }))
                .await;
            let client = client.as_deref().unwrap_or_default();
            warn!(
                "SOCKS blocked by method policy (client={client}, host={host}, mode=limited, allowed_methods=GET, HEAD, OPTIONS)"
            );
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "blocked").into());
        }
        Ok(NetworkMode::Full) => {}
        Err(err) => {
            error!("failed to evaluate method policy: {err}");
            return Err(io::Error::other("proxy error").into());
        }
    }

    let request = NetworkPolicyRequest::new(NetworkPolicyRequestArgs {
        protocol: NetworkProtocol::Socks5Tcp,
        host: host.clone(),
        port,
        client_addr: client.clone(),
        method: None,
        command: None,
        exec_policy_hint: None,
    });

    match evaluate_host_policy_with_target(&app_state, policy_decider.as_ref(), &request).await {
        Ok((NetworkDecision::Deny { reason }, _)) => {
            let _ = app_state
                .record_blocked(BlockedRequest::new(BlockedRequestArgs {
                    host: host.clone(),
                    reason: reason.clone(),
                    client: client.clone(),
                    method: None,
                    mode: None,
                    protocol: "socks5".to_owned(),
                }))
                .await;
            let client = client.as_deref().unwrap_or_default();
            warn!("SOCKS blocked (client={client}, host={host}, reason={reason})");
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "blocked").into());
        }
        Ok((NetworkDecision::Allow, target)) => {
            if let Some(target) = target {
                req.extensions().insert(target);
            }
            let client = client.as_deref().unwrap_or_default();
            info!("SOCKS allowed (client={client}, host={host}, port={port})");
        }
        Err(err) => {
            error!("failed to evaluate host: {err}");
            return Err(io::Error::other("proxy error").into());
        }
    }

    tcp_connector.serve(req).await.map_err(Into::into)
}

async fn inspect_socks5_udp(
    request: RelayRequest,
    state: Arc<NetworkProxyState>,
    policy_decider: Option<Arc<dyn NetworkPolicyDecider>>,
) -> io::Result<RelayResponse> {
    let RelayRequest {
        server_address,
        payload,
        extensions,
        ..
    } = request;

    let host = normalize_host(&server_address.ip_addr.to_string());
    let port = server_address.port;
    if host.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid host"));
    }

    let client = extensions
        .get_ref::<SocketInfo>()
        .map(|info| info.peer_addr().to_string());

    match state.enabled().await {
        Ok(true) => {}
        Ok(false) => {
            let _ = state
                .record_blocked(BlockedRequest::new(BlockedRequestArgs {
                    host: host.clone(),
                    reason: REASON_PROXY_DISABLED.to_owned(),
                    client: client.clone(),
                    method: None,
                    mode: None,
                    protocol: "socks5-udp".to_owned(),
                }))
                .await;
            let client = client.as_deref().unwrap_or_default();
            warn!("SOCKS UDP blocked; proxy disabled (client={client}, host={host})");
            return Ok(RelayResponse {
                maybe_payload: None,
                extensions,
            });
        }
        Err(err) => {
            error!("failed to read enabled state: {err}");
            return Err(io::Error::other("proxy error"));
        }
    }

    match state.network_mode().await {
        Ok(NetworkMode::Limited) => {
            let _ = state
                .record_blocked(BlockedRequest::new(BlockedRequestArgs {
                    host: host.clone(),
                    reason: REASON_METHOD_NOT_ALLOWED.to_owned(),
                    client: client.clone(),
                    method: None,
                    mode: Some(NetworkMode::Limited),
                    protocol: "socks5-udp".to_owned(),
                }))
                .await;
            return Ok(RelayResponse {
                maybe_payload: None,
                extensions,
            });
        }
        Ok(NetworkMode::Full) => {}
        Err(err) => {
            error!("failed to evaluate method policy: {err}");
            return Err(io::Error::other("proxy error"));
        }
    }

    let request = NetworkPolicyRequest::new(NetworkPolicyRequestArgs {
        protocol: NetworkProtocol::Socks5Udp,
        host: host.clone(),
        port,
        client_addr: client.clone(),
        method: None,
        command: None,
        exec_policy_hint: None,
    });

    match evaluate_host_policy(&state, policy_decider.as_ref(), &request).await {
        Ok(NetworkDecision::Deny { reason }) => {
            let _ = state
                .record_blocked(BlockedRequest::new(BlockedRequestArgs {
                    host: host.clone(),
                    reason: reason.clone(),
                    client: client.clone(),
                    method: None,
                    mode: None,
                    protocol: "socks5-udp".to_owned(),
                }))
                .await;
            let client = client.as_deref().unwrap_or_default();
            warn!("SOCKS UDP blocked (client={client}, host={host}, reason={reason})");
            Ok(RelayResponse {
                maybe_payload: None,
                extensions,
            })
        }
        Ok(NetworkDecision::Allow) => Ok(RelayResponse {
            maybe_payload: Some(payload),
            extensions,
        }),
        Err(err) => {
            error!("failed to evaluate UDP host: {err}");
            Err(io::Error::other("proxy error"))
        }
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::config::NetworkPolicy;
    use crate::connector::ValidatedTarget;
    use crate::runtime::network_proxy_state_for_policy;

    #[tokio::test]
    async fn socks5_tcp_keeps_approved_candidates_in_the_connection_input() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            socket.write_all(b"pinned socks").await.unwrap();
        });
        let state = Arc::new(network_proxy_state_for_policy(NetworkPolicy {
            allowed_domains: vec!["checked.invalid".to_owned()],
            allow_local_binding: true,
            ..Default::default()
        }));
        state.set_network_mode(NetworkMode::Full).await.unwrap();
        let request = ConnectRequest::new(
            format!("checked.invalid:{}", address.port())
                .parse()
                .unwrap(),
        );
        request.extensions.insert_arc(state);
        request.extensions.insert(ValidatedTarget {
            host: "checked.invalid".to_owned(),
            port: address.port(),
            addresses: vec![address],
        });
        let mut connected = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            handle_socks5_tcp(request, PolicyConnector, None),
        )
        .await
        .unwrap()
        .unwrap()
        .conn;
        let mut bytes = [0; 12];
        connected.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"pinned socks");
        server.await.unwrap();
    }
}
