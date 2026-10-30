use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use rama_core::error::extra::OpaqueError;
use rama_core::error::{BoxError, ErrorExt as _};
use rama_core::extensions::ExtensionsRef;
use rama_core::io::BridgeIo;
use rama_core::layer::AddInputExtensionLayer;
use rama_core::rt::Executor;
use rama_core::service::service_fn;
use rama_core::{Layer, Service};
use rama_http::layer::remove_header::{RemoveRequestHeaderLayer, RemoveResponseHeaderLayer};
use rama_http::layer::upgrade::{UpgradeLayer, UpgradeResponse, Upgraded};
use rama_http::matcher::MethodMatcher;
use rama_http::{Body, HeaderValue, Request, Response, StatusCode};
use rama_http_backend::client::proxy::layer::HttpProxyConnector;
use rama_http_backend::server::HttpServer;
use rama_net::address::ProxyAddress;
use rama_net::client::{
    ConnectRequest, ConnectorService, ConnectorTarget, EstablishedClientConnection, ProxyRoute,
};
use rama_net::proxy::IoForwardService;
use rama_net::stream::SocketInfo;
use rama_net::{ConnectorTargetInputExt, Protocol};
use rama_tcp::server::TcpListener;
use rama_tls::client::TlsClientConfig;
use rama_tls_boring::client::TlsConnectorLayer;
use serde::Serialize;
use tracing::{error, info, warn};

use crate::config::NetworkMode;
use crate::connector::PolicyConnector;
use crate::network_policy::{
    NetworkDecision, NetworkPolicyDecider, NetworkPolicyRequest, NetworkPolicyRequestArgs,
    NetworkProtocol, evaluate_host_policy_with_target,
};
use crate::policy::normalize_host;
use crate::reasons::{REASON_METHOD_NOT_ALLOWED, REASON_NOT_ALLOWED, REASON_PROXY_DISABLED};
use crate::responses::{blocked_header_value, json_response};
use crate::runtime::unix_socket_permissions_supported;
use crate::state::{BlockedRequest, BlockedRequestArgs, NetworkProxyState};
use crate::upstream::{UpstreamClient, proxy_for_connect};

pub async fn run_http_proxy(
    state: Arc<NetworkProxyState>,
    addr: SocketAddr,
    policy_decider: Option<Arc<dyn NetworkPolicyDecider>>,
) -> Result<()> {
    let listener = TcpListener::build(Executor::new())
        .bind_address(addr)
        .await
        // Rama's `BoxError` is a `Box<dyn Error + Send + Sync>` without an explicit `'static`
        // lifetime bound, which means it doesn't satisfy `anyhow::Context`'s `StdError` constraint.
        // Wrap it in Rama's `OpaqueError` so we can preserve the original error as a source and
        // still use `anyhow` for chaining.
        .map_err(|err| err.into_opaque_error())
        .map_err(anyhow::Error::from)
        .with_context(|| format!("bind HTTP proxy: {addr}"))?;

    let http_service = HttpServer::auto(Executor::new()).service(
        (
            UpgradeLayer::new_with_services(
                Executor::new(),
                MethodMatcher::CONNECT,
                service_fn({
                    let policy_decider = policy_decider.clone();
                    move |req| http_connect_accept(policy_decider.clone(), req)
                }),
                service_fn(http_connect_proxy),
            ),
            RemoveResponseHeaderLayer::hop_by_hop(),
            RemoveRequestHeaderLayer::hop_by_hop(),
        )
            .into_layer(service_fn({
                let policy_decider = policy_decider.clone();
                move |req| http_plain_proxy(policy_decider.clone(), req)
            })),
    );

    info!("HTTP proxy listening on {addr}");

    listener
        .serve(AddInputExtensionLayer::new_arc(state).into_layer(http_service))
        .await;
    Ok(())
}

// Rama's UpgradeLayer requires the rejection response directly in this Result.
#[allow(clippy::result_large_err)]
async fn http_connect_accept(
    policy_decider: Option<Arc<dyn NetworkPolicyDecider>>,
    req: Request,
) -> Result<UpgradeResponse<Request, Response>, Response> {
    let app_state = req
        .extensions()
        .get_arc::<NetworkProxyState>()
        .ok_or_else(|| text_response(StatusCode::INTERNAL_SERVER_ERROR, "missing state"))?;

    let authority = if let Some(authority) = req.connector_target() {
        authority
    } else {
        warn!("CONNECT missing authority");
        return Err(text_response(StatusCode::BAD_REQUEST, "missing authority"));
    };

    let host = normalize_host(&authority.host.to_string());
    if host.is_empty() {
        return Err(text_response(StatusCode::BAD_REQUEST, "invalid host"));
    }

    let client = client_addr(&req);

    let enabled = app_state
        .enabled()
        .await
        .map_err(|err| internal_error("failed to read enabled state", err))?;
    if !enabled {
        let client = client.as_deref().unwrap_or_default();
        warn!("CONNECT blocked; proxy disabled (client={client}, host={host})");
        return Err(proxy_disabled_response(
            &app_state,
            host,
            client_addr(&req),
            Some("CONNECT".to_owned()),
            "http-connect",
        )
        .await);
    }

    let request = NetworkPolicyRequest::new(NetworkPolicyRequestArgs {
        protocol: NetworkProtocol::HttpsConnect,
        host: host.clone(),
        port: authority.port,
        client_addr: client.clone(),
        method: Some("CONNECT".to_owned()),
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
                    method: Some("CONNECT".to_owned()),
                    mode: None,
                    protocol: "http-connect".to_owned(),
                }))
                .await;
            let client = client.as_deref().unwrap_or_default();
            warn!("CONNECT blocked (client={client}, host={host}, reason={reason})");
            return Err(blocked_text(&reason));
        }
        Ok((NetworkDecision::Allow, target)) => {
            if let Some(target) = target {
                req.extensions().insert(target);
            }
            let client = client.as_deref().unwrap_or_default();
            info!("CONNECT allowed (client={client}, host={host})");
        }
        Err(err) => {
            error!("failed to evaluate host for CONNECT {host}: {err}");
            return Err(text_response(StatusCode::INTERNAL_SERVER_ERROR, "error"));
        }
    }

    let mode = app_state
        .network_mode()
        .await
        .map_err(|err| internal_error("failed to read network mode", err))?;

    if mode == NetworkMode::Limited {
        let _ = app_state
            .record_blocked(BlockedRequest::new(BlockedRequestArgs {
                host: host.clone(),
                reason: REASON_METHOD_NOT_ALLOWED.to_owned(),
                client: client.clone(),
                method: Some("CONNECT".to_owned()),
                mode: Some(NetworkMode::Limited),
                protocol: "http-connect".to_owned(),
            }))
            .await;
        let client = client.as_deref().unwrap_or_default();
        warn!("CONNECT blocked by method policy (client={client}, host={host}, mode=limited)");
        return Err(blocked_text(REASON_METHOD_NOT_ALLOWED));
    }

    let response = UpgradeResponse::new(
        req,
        Response::builder()
            .status(StatusCode::OK)
            .body(Body::empty())
            .unwrap_or_else(|_| Response::new(Body::empty())),
    )
    .with_extension(ConnectorTarget(authority));
    // Rama 0.4 keeps request extensions separate from upgraded transport
    // extensions. Transfer only the state and checked target needed here.
    response.extensions.insert_arc(app_state);
    if let Some(target) = response
        .request
        .extensions()
        .get_ref::<crate::connector::ValidatedTarget>()
    {
        response.extensions.insert(target.clone());
    }
    Ok(response)
}

async fn http_connect_proxy(upgraded: Upgraded) -> Result<(), Infallible> {
    if upgraded.extensions().get_ref::<ConnectorTarget>().is_none() {
        warn!("CONNECT missing proxy target");
        return Ok(());
    }

    let allow_upstream_proxy =
        if let Some(state) = upgraded.extensions().get_arc::<NetworkProxyState>() {
            match state.allow_upstream_proxy().await {
                Ok(allowed) => allowed,
                Err(err) => {
                    error!("failed to read upstream proxy setting: {err}");
                    false
                }
            }
        } else {
            error!("missing app state");
            false
        };

    let proxy = if allow_upstream_proxy {
        proxy_for_connect()
    } else {
        None
    };

    if let Err(err) = forward_connect_tunnel(upgraded, proxy).await {
        warn!("tunnel error: {err}");
    }
    Ok(())
}

async fn forward_connect_tunnel(
    upgraded: Upgraded,
    proxy: Option<ProxyAddress>,
) -> Result<(), BoxError> {
    let authority = upgraded
        .extensions()
        .get_ref::<ConnectorTarget>()
        .map(|target| target.0.clone())
        .ok_or_else(|| {
            OpaqueError::from_static_str("missing forward authority").into_box_error()
        })?;

    let extensions = upgraded.extensions().clone();
    if let Some(proxy) = proxy {
        extensions.insert(ProxyRoute::Proxy(proxy));
    }

    let req = ConnectRequest::new_with_extensions(authority.clone(), extensions)
        .with_application_protocol(Protocol::HTTPS);
    let transport = PolicyConnector;
    // TLS to an HTTPS upstream proxy precedes its HTTP CONNECT handshake.
    // The destination's TLS remains end-to-end inside the returned tunnel.
    let proxy_transport = TlsConnectorLayer::tunnel(None)
        .with_base_config(TlsClientConfig::default_http().with_alpn_http_1())
        .into_layer(transport);
    let connector = HttpProxyConnector::optional(proxy_transport);
    let EstablishedClientConnection { conn: target, .. } = connector
        .connect(req)
        .await
        .map_err(|err| err.with_context(|| format!("establish CONNECT tunnel to {authority}")))?;

    IoForwardService::default()
        .serve(BridgeIo(upgraded, target))
        .await
        .map(|_| ())
        .map_err(|err| err.with_context(|| format!("forward CONNECT tunnel to {authority}")))
}

async fn http_plain_proxy(
    policy_decider: Option<Arc<dyn NetworkPolicyDecider>>,
    req: Request,
) -> Result<Response, Infallible> {
    let app_state = if let Some(state) = req.extensions().get_arc::<NetworkProxyState>() {
        state
    } else {
        error!("missing app state");
        return Ok(text_response(StatusCode::INTERNAL_SERVER_ERROR, "error"));
    };
    let client = client_addr(&req);

    let method_allowed = match app_state
        .method_allowed(req.method().as_str())
        .await
        .map_err(|err| internal_error("failed to evaluate method policy", err))
    {
        Ok(allowed) => allowed,
        Err(resp) => return Ok(resp),
    };

    // `x-unix-socket` is an escape hatch for talking to local daemons. We keep it tightly scoped:
    // macOS-only + explicit allowlist, to avoid turning the proxy into a general local capability
    // escalation mechanism.
    if let Some(unix_socket_header) = req.headers().get("x-unix-socket") {
        let socket_path = if let Ok(value) = unix_socket_header.to_str() {
            value.to_owned()
        } else {
            warn!("invalid x-unix-socket header value (non-UTF8)");
            return Ok(text_response(
                StatusCode::BAD_REQUEST,
                "invalid x-unix-socket header",
            ));
        };
        let enabled = match app_state
            .enabled()
            .await
            .map_err(|err| internal_error("failed to read enabled state", err))
        {
            Ok(enabled) => enabled,
            Err(resp) => return Ok(resp),
        };
        if !enabled {
            let client = client.as_deref().unwrap_or_default();
            warn!("unix socket blocked; proxy disabled (client={client}, path={socket_path})");
            return Ok(proxy_disabled_response(
                &app_state,
                socket_path,
                client_addr(&req),
                Some(req.method().as_str().to_owned()),
                "unix-socket",
            )
            .await);
        }
        if !method_allowed {
            let client = client.as_deref().unwrap_or_default();
            let method = req.method();
            warn!(
                "unix socket blocked by method policy (client={client}, method={method}, mode=limited, allowed_methods=GET, HEAD, OPTIONS)"
            );
            return Ok(json_blocked("unix-socket", REASON_METHOD_NOT_ALLOWED));
        }

        if !unix_socket_permissions_supported() {
            warn!("unix socket proxy unsupported on this platform (path={socket_path})");
            return Ok(text_response(
                StatusCode::NOT_IMPLEMENTED,
                "unix sockets unsupported",
            ));
        }

        return match app_state.is_unix_socket_allowed(&socket_path).await {
            Ok(true) => {
                let client = client.as_deref().unwrap_or_default();
                info!("unix socket allowed (client={client}, path={socket_path})");
                match proxy_via_unix_socket(req, &socket_path).await {
                    Ok(resp) => Ok(resp),
                    Err(err) => {
                        warn!("unix socket proxy failed: {err}");
                        Ok(text_response(
                            StatusCode::BAD_GATEWAY,
                            "unix socket proxy failed",
                        ))
                    }
                }
            }
            Ok(false) => {
                let client = client.as_deref().unwrap_or_default();
                warn!("unix socket blocked (client={client}, path={socket_path})");
                Ok(json_blocked("unix-socket", REASON_NOT_ALLOWED))
            }
            Err(err) => {
                warn!("unix socket check failed: {err}");
                Ok(text_response(StatusCode::INTERNAL_SERVER_ERROR, "error"))
            }
        };
    }

    let authority = if let Some(authority) = req.connector_target() {
        authority
    } else {
        warn!("missing host");
        return Ok(text_response(StatusCode::BAD_REQUEST, "missing host"));
    };
    let host = normalize_host(&authority.host.to_string());
    let port = authority.port;
    let enabled = match app_state
        .enabled()
        .await
        .map_err(|err| internal_error("failed to read enabled state", err))
    {
        Ok(enabled) => enabled,
        Err(resp) => return Ok(resp),
    };
    if !enabled {
        let client = client.as_deref().unwrap_or_default();
        let method = req.method();
        warn!("request blocked; proxy disabled (client={client}, host={host}, method={method})");
        return Ok(proxy_disabled_response(
            &app_state,
            host,
            client_addr(&req),
            Some(req.method().as_str().to_owned()),
            "http",
        )
        .await);
    }

    let request = NetworkPolicyRequest::new(NetworkPolicyRequestArgs {
        protocol: NetworkProtocol::Http,
        host: host.clone(),
        port,
        client_addr: client.clone(),
        method: Some(req.method().as_str().to_owned()),
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
                    method: Some(req.method().as_str().to_owned()),
                    mode: None,
                    protocol: "http".to_owned(),
                }))
                .await;
            let client = client.as_deref().unwrap_or_default();
            warn!("request blocked (client={client}, host={host}, reason={reason})");
            return Ok(json_blocked(&host, &reason));
        }
        Ok((NetworkDecision::Allow, target)) => {
            if let Some(target) = target {
                req.extensions().insert(target);
            }
        }
        Err(err) => {
            error!("failed to evaluate host for {host}: {err}");
            return Ok(text_response(StatusCode::INTERNAL_SERVER_ERROR, "error"));
        }
    }

    if !method_allowed {
        let _ = app_state
            .record_blocked(BlockedRequest::new(BlockedRequestArgs {
                host: host.clone(),
                reason: REASON_METHOD_NOT_ALLOWED.to_owned(),
                client: client.clone(),
                method: Some(req.method().as_str().to_owned()),
                mode: Some(NetworkMode::Limited),
                protocol: "http".to_owned(),
            }))
            .await;
        let client = client.as_deref().unwrap_or_default();
        let method = req.method();
        warn!(
            "request blocked by method policy (client={client}, host={host}, method={method}, mode=limited, allowed_methods=GET, HEAD, OPTIONS)"
        );
        return Ok(json_blocked(&host, REASON_METHOD_NOT_ALLOWED));
    }

    let client = client.as_deref().unwrap_or_default();
    let method = req.method();
    info!("request allowed (client={client}, host={host}, method={method})");

    let allow_upstream_proxy = match app_state
        .allow_upstream_proxy()
        .await
        .map_err(|err| internal_error("failed to read upstream proxy config", err))
    {
        Ok(allow) => allow,
        Err(resp) => return Ok(resp),
    };
    let client = if allow_upstream_proxy {
        UpstreamClient::from_env_proxy()
    } else {
        UpstreamClient::direct()
    };

    match client.serve(req).await {
        Ok(resp) => Ok(resp),
        Err(err) => {
            warn!("upstream request failed: {err}");
            Ok(text_response(StatusCode::BAD_GATEWAY, "upstream failure"))
        }
    }
}

async fn proxy_via_unix_socket(req: Request, socket_path: &str) -> Result<Response> {
    #[cfg(target_os = "macos")]
    {
        let client = UpstreamClient::unix_socket(socket_path);

        let (mut parts, body) = req.into_parts();
        let path = parts.uri.request_target().into_owned();
        parts.uri = path
            .parse()
            .with_context(|| format!("invalid unix socket request path: {path}"))?;
        parts.headers.remove("x-unix-socket");

        let req = Request::from_parts(parts, body);
        client.serve(req).await.map_err(anyhow::Error::from)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = req;
        let _ = socket_path;
        Err(anyhow::anyhow!("unix sockets not supported"))
    }
}

fn client_addr<T: ExtensionsRef>(input: &T) -> Option<String> {
    input
        .extensions()
        .get_ref::<SocketInfo>()
        .map(|info| info.peer_addr().to_string())
}

fn json_blocked(host: &str, reason: &str) -> Response {
    let response = BlockedResponse {
        status: "blocked",
        host,
        reason,
    };
    let mut resp = json_response(&response);
    *resp.status_mut() = StatusCode::FORBIDDEN;
    resp.headers_mut().insert(
        "x-proxy-error",
        HeaderValue::from_static(blocked_header_value(reason)),
    );
    resp
}

fn blocked_text(reason: &str) -> Response {
    crate::responses::blocked_text_response(reason)
}

async fn proxy_disabled_response(
    app_state: &NetworkProxyState,
    host: String,
    client: Option<String>,
    method: Option<String>,
    protocol: &str,
) -> Response {
    let _ = app_state
        .record_blocked(BlockedRequest::new(BlockedRequestArgs {
            host,
            reason: REASON_PROXY_DISABLED.to_owned(),
            client,
            method,
            mode: None,
            protocol: protocol.to_owned(),
        }))
        .await;
    text_response(StatusCode::SERVICE_UNAVAILABLE, "proxy disabled")
}

fn internal_error(context: &str, err: impl std::fmt::Display) -> Response {
    error!("{context}: {err}");
    text_response(StatusCode::INTERNAL_SERVER_ERROR, "error")
}

fn text_response(status: StatusCode, body: &str) -> Response {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain")
        .body(Body::from(body.to_owned()))
        .unwrap_or_else(|_| Response::new(Body::from(body.to_owned())))
}

#[derive(Serialize)]
struct BlockedResponse<'a> {
    status: &'static str,
    host: &'a str,
    reason: &'a str,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use pretty_assertions::assert_eq;
    use rama_http::{Method, Request};

    use super::*;
    use crate::config::{NetworkMode, NetworkPolicy};
    use crate::runtime::network_proxy_state_for_policy;

    #[tokio::test]
    async fn http_connect_accept_blocks_in_limited_mode() {
        let policy = NetworkPolicy {
            allowed_domains: vec!["example.com".to_owned()],
            allow_local_binding: true,
            ..Default::default()
        };
        let state = Arc::new(network_proxy_state_for_policy(policy));
        state.set_network_mode(NetworkMode::Limited).await.unwrap();

        let req = Request::builder()
            .method(Method::CONNECT)
            .uri("https://example.com:443")
            .header("host", "example.com:443")
            .body(Body::empty())
            .unwrap();
        req.extensions().insert_arc(state);

        let response = http_connect_accept(None, req).await.unwrap_err();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            response.headers().get("x-proxy-error").unwrap(),
            "blocked-by-method-policy"
        );
    }
    #[tokio::test]
    async fn http_connect_upgrade_carries_only_checked_target_and_state() {
        let policy = NetworkPolicy {
            allowed_domains: vec!["8.8.8.8".to_owned()],
            ..Default::default()
        };
        let state = Arc::new(network_proxy_state_for_policy(policy));
        state.set_network_mode(NetworkMode::Full).await.unwrap();
        let req = Request::builder()
            .method(Method::CONNECT)
            .uri(rama_net::uri::Uri::parse_authority_form("8.8.8.8:443").unwrap())
            .body(Body::empty())
            .unwrap();
        req.extensions().insert_arc(state.clone());
        let accepted = http_connect_accept(None, req).await.unwrap();
        assert_eq!(accepted.response.status(), StatusCode::OK);
        assert_eq!(
            accepted
                .extensions
                .get_ref::<ConnectorTarget>()
                .unwrap()
                .0
                .to_string(),
            "8.8.8.8:443"
        );
        assert!(Arc::ptr_eq(
            &accepted.extensions.get_arc::<NetworkProxyState>().unwrap(),
            &state
        ));
        let checked = accepted
            .extensions
            .get_ref::<crate::connector::ValidatedTarget>()
            .unwrap();
        assert_eq!(checked.host, "8.8.8.8");
        assert_eq!(
            checked.addresses,
            vec!["8.8.8.8:443".parse::<std::net::SocketAddr>().unwrap()]
        );
    }

    #[tokio::test]
    async fn connect_tunnel_uses_pinned_addresses_and_preserves_end_to_end_bytes() {
        use rama_core::ServiceInput;
        use rama_core::bytes::Bytes;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let echo = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut data = [0; 12];
            socket.read_exact(&mut data).await.unwrap();
            assert_eq!(&data, b"opaque bytes");
            socket.write_all(&data).await.unwrap();
            socket.shutdown().await.unwrap();
        });
        let (mut client, input) = tokio::io::duplex(128);
        let input = ServiceInput::new(input);
        input.extensions.insert(ConnectorTarget(
            format!("checked.invalid:{port}").parse().unwrap(),
        ));
        input.extensions.insert(crate::connector::ValidatedTarget {
            host: "checked.invalid".to_owned(),
            port,
            addresses: vec![format!("127.0.0.1:{port}").parse().unwrap()],
        });
        let upgraded = Upgraded::new(input, Bytes::new());
        let forwarding = tokio::spawn(forward_connect_tunnel(upgraded, None));
        client.write_all(b"opaque bytes").await.unwrap();
        let mut response = [0; 12];
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.read_exact(&mut response),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(&response, b"opaque bytes");
        client.shutdown().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), forwarding)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        echo.await.unwrap();
    }
}
