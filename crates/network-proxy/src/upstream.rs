use rama_core::error::ErrorContext as _;
use rama_core::error::extra::OpaqueError;
use rama_core::extensions::ExtensionsRef;
use rama_core::rt::Executor;
use rama_core::service::BoxService;
use rama_core::{Layer, Service};
use rama_http::layer::version_adapter::RequestVersionAdapter;
use rama_http::{Body, Request, Response};
use rama_http_backend::client::proxy::layer::HttpProxyConnectorLayer;
use rama_http_backend::client::{HttpClientService, HttpConnector};
use rama_net::ProtocolInputExt;
use rama_net::address::ProxyAddress;
use rama_net::client::{ConnectionError, EstablishedClientConnection, ProxyRoute};
use rama_tls::client::TlsClientConfig;
use rama_tls_boring::client::TlsConnectorLayer;
#[cfg(target_os = "macos")]
use rama_unix::client::UnixConnector;
use tracing::warn;

use crate::connector::PolicyConnector;

#[derive(Clone, Default)]
struct ProxyConfig {
    http: Option<ProxyAddress>,
    https: Option<ProxyAddress>,
    all: Option<ProxyAddress>,
}

impl ProxyConfig {
    fn from_env() -> Self {
        let http = read_proxy_env(&["HTTP_PROXY", "http_proxy"]);
        let https = read_proxy_env(&["HTTPS_PROXY", "https_proxy"]);
        let all = read_proxy_env(&["ALL_PROXY", "all_proxy"]);
        Self { http, https, all }
    }

    fn proxy_for_request(&self, req: &Request) -> Option<ProxyAddress> {
        let is_secure = req
            .protocol()
            .map(|protocol| protocol.is_secure())
            .unwrap_or(false);
        self.proxy_for_protocol(is_secure)
    }

    fn proxy_for_protocol(&self, is_secure: bool) -> Option<ProxyAddress> {
        if is_secure {
            self.https
                .clone()
                .or_else(|| self.http.clone())
                .or_else(|| self.all.clone())
        } else {
            self.http.clone().or_else(|| self.all.clone())
        }
    }
}

fn read_proxy_env(keys: &[&str]) -> Option<ProxyAddress> {
    for key in keys {
        let Ok(value) = std::env::var(key) else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        match ProxyAddress::try_from(value) {
            Ok(proxy) => {
                if proxy
                    .protocol
                    .as_ref()
                    .map(rama_net::Protocol::is_http)
                    .unwrap_or(true)
                {
                    return Some(proxy);
                }
                warn!("ignoring {key}: non-http proxy protocol");
            }
            Err(err) => {
                warn!("ignoring {key}: invalid proxy address ({err})");
            }
        }
    }
    None
}

pub(crate) fn proxy_for_connect() -> Option<ProxyAddress> {
    ProxyConfig::from_env().proxy_for_protocol(true)
}

#[derive(Clone)]
pub(crate) struct UpstreamClient {
    connector: BoxService<
        Request<Body>,
        EstablishedClientConnection<HttpClientService<Body>, Request<Body>>,
        ConnectionError,
    >,
    proxy_config: ProxyConfig,
}

impl UpstreamClient {
    pub(crate) fn direct() -> Self {
        Self::new(ProxyConfig::default())
    }

    pub(crate) fn from_env_proxy() -> Self {
        Self::new(ProxyConfig::from_env())
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn unix_socket(path: &str) -> Self {
        let connector = build_unix_connector(path);
        Self {
            connector,
            proxy_config: ProxyConfig::default(),
        }
    }

    fn new(proxy_config: ProxyConfig) -> Self {
        let connector = build_http_connector();
        Self {
            connector,
            proxy_config,
        }
    }
}

impl Service<Request<Body>> for UpstreamClient {
    type Output = Response;
    type Error = OpaqueError;

    async fn serve(&self, req: Request<Body>) -> Result<Self::Output, Self::Error> {
        if let Some(proxy) = self.proxy_config.proxy_for_request(&req) {
            req.extensions().insert(ProxyRoute::Proxy(proxy));
        }

        let uri = req.uri().clone();
        let EstablishedClientConnection {
            input: req,
            conn: http_connection,
        } = self
            .connector
            .serve(req)
            .await
            .map_err(|err| err.into_opaque_error())?;

        req.extensions().extend(http_connection.extensions());

        http_connection
            .serve(req)
            .await
            .with_context(|| format!("http request failure for uri: {uri}"))
            .into_opaque_error()
    }
}

fn build_http_connector() -> BoxService<
    Request<Body>,
    EstablishedClientConnection<HttpClientService<Body>, Request<Body>>,
    ConnectionError,
> {
    let transport = PolicyConnector;
    let proxy_transport = TlsConnectorLayer::tunnel(None)
        .with_base_config(TlsClientConfig::default_http().with_alpn_http_1())
        .into_layer(transport);
    let proxy = HttpProxyConnectorLayer::optional().into_layer(proxy_transport);
    let tls_config = TlsClientConfig::default_http();
    let tls = TlsConnectorLayer::auto()
        .with_base_config(tls_config)
        .into_layer(proxy);
    let tls = RequestVersionAdapter::new(tls);
    let connector = HttpConnector::new(tls, Executor::new());
    connector.boxed()
}

#[cfg(target_os = "macos")]
fn build_unix_connector(
    path: &str,
) -> BoxService<
    Request<Body>,
    EstablishedClientConnection<HttpClientService<Body>, Request<Body>>,
    ConnectionError,
> {
    let transport = UnixConnector::fixed(path);
    let connector = HttpConnector::new(transport, Executor::new());
    connector.boxed()
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use rama_http::BodyExtractExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    async fn response_server() -> (u16, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            loop {
                let count = socket.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                request.extend_from_slice(&buffer[..count]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await
                .unwrap();
            String::from_utf8(request).unwrap()
        });
        (port, task)
    }

    #[tokio::test]
    async fn direct_http_client_resolves_a_domain_and_forwards_origin_form() {
        let (port, server) = response_server().await;
        let request = Request::builder()
            .uri(format!("http://localhost:{port}/health?full=1"))
            .body(Body::empty())
            .unwrap();
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            UpstreamClient::direct().serve(request),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(response.status(), rama_http::StatusCode::OK);
        assert_eq!(response.try_into_string().await.unwrap(), "ok");
        assert!(
            server
                .await
                .unwrap()
                .starts_with("GET /health?full=1 HTTP/1.1\r\n")
        );
    }

    #[tokio::test]
    async fn http_upstream_proxy_uses_the_selected_route_without_resolving_the_origin() {
        let (port, server) = response_server().await;
        let client = UpstreamClient::new(ProxyConfig {
            http: Some(format!("http://127.0.0.1:{port}").parse().unwrap()),
            ..Default::default()
        });
        let request = Request::builder()
            .uri("http://unresolvable.invalid/through-proxy")
            .body(Body::empty())
            .unwrap();
        request
            .extensions()
            .insert(crate::connector::ValidatedTarget {
                host: "unresolvable.invalid".to_owned(),
                port: 80,
                addresses: vec!["8.8.8.8:80".parse().unwrap()],
            });
        let response =
            tokio::time::timeout(std::time::Duration::from_secs(5), client.serve(request))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(response.try_into_string().await.unwrap(), "ok");
        assert!(
            server
                .await
                .unwrap()
                .starts_with("GET http://unresolvable.invalid/through-proxy HTTP/1.1\r\n")
        );
    }
    #[tokio::test]
    async fn http_client_preserves_the_authority_while_dialing_checked_addresses() {
        let (port, server) = response_server().await;
        let request = Request::builder()
            .uri(format!("http://checked.invalid:{port}/pinned"))
            .body(Body::empty())
            .unwrap();
        request
            .extensions()
            .insert(crate::connector::ValidatedTarget {
                host: "checked.invalid".to_owned(),
                port,
                addresses: vec![format!("127.0.0.1:{port}").parse().unwrap()],
            });
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            UpstreamClient::direct().serve(request),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(response.try_into_string().await.unwrap(), "ok");
        let forwarded = server.await.unwrap();
        assert!(forwarded.starts_with("GET /pinned HTTP/1.1\r\n"));
        assert!(
            forwarded
                .to_ascii_lowercase()
                .contains(&format!("host: checked.invalid:{port}\r\n"))
        );
    }
}
